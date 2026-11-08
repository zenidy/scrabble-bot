//! The Scrabbler: value-based bidding with budget pacing.
//!
//! Each auction:
//! 1. `V` = Monte Carlo marginal value of the tile in points (`value.rs`).
//! 2. `lambda` = points per coin for the rest of the match, set so expected
//!    spend over the remaining auctions matches the money we'll have
//!    (balance + bankable words + expected word income). It falls to its
//!    floor as the match runs out, since leftover coins are worth nothing.
//! 3. Vickrey: bid `V / lambda` (truthful in coins). First price: maximize
//!    `P(win | b) * (V / lambda - b)` under the price model.
//!
//! Words: when money runs short, play the best 7+ letter word of the
//! current best partition (already at the 3x cap). When the last auction
//! opens, play the best partition of the whole rack, then bid the whole
//! balance on that last tile: nobody can play it but a sim, and coins are
//! worthless after the match.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::dict::Dict;
use super::pool::{self, PoolInfo};
use super::price::{Pmf, PriceModel, outcome};
use super::rules::{AuctionType, NUM_LETTERS, Rack, value};
use super::solver::Solver;
use super::value::{Projection, Valuer};
use super::view::{Action, MatchView, Phase, Strategy};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Params {
    /// Monte Carlo futures per valuation.
    pub samples: usize,
    /// Cap on rack + projected tiles in a valuation (solver cost).
    pub proj_cap: usize,
    /// Solver node budget per valuation solve.
    pub node_cap: u64,
    /// Prior tile value for pacing: `v0_scale * 3 * value(letter)`.
    pub v0_scale: f64,
    pub lambda_min: f64,
    pub lambda_max: f64,
    /// Share of future bought value expected back as coins before the end.
    pub income_rate: f64,
    /// Share of the current rack's 7+ letter words counted as spendable.
    pub liquid_rate: f64,
    /// Play a 7+ letter word when the balance is below this.
    pub play_floor: i64,
    /// Play 7+ letter words as soon as they appear.
    pub always_play_long: bool,
    /// Bank the best 7+ letter word once the rack holds more tiles than this.
    pub rack_limit: u32,
    /// Bank 7+ letter words while coins are this scarce (lambda at or above).
    pub play_long_lambda: f64,
    /// Below this balance with auctions to spare, bank any decent word.
    pub starve_floor: i64,
    pub starve_min_left: u32,
    pub starve_min_len: u8,
    /// Pseudo-samples of the prior value mixed into the Monte Carlo value.
    pub shrink_k0: f64,
    /// Mix of uniform weights into projected future wins (0..1).
    pub proj_uniform: f64,
    /// Cap on a single future bid in the pacing model.
    pub future_bid_cap: f64,
    /// Bid the whole balance on the last (unplayable) tile.
    pub last_all_in: bool,
    /// Multiplier on `V / lambda`.
    pub bid_scale: f64,
    /// Weight on the strongest opponent's gain (denial).
    pub denial_weight: f64,
    /// Time cap for the final flush solve.
    pub flush_ms: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            samples: 6,
            proj_cap: 24,
            node_cap: 30_000,
            v0_scale: 0.8,
            lambda_min: 0.05,
            lambda_max: 20.0,
            income_rate: 0.5,
            liquid_rate: 0.8,
            play_floor: 30,
            always_play_long: false,
            rack_limit: 30,
            play_long_lambda: 0.3,
            starve_floor: 8,
            starve_min_left: 6,
            starve_min_len: 5,
            shrink_k0: 2.0,
            proj_uniform: 0.5,
            future_bid_cap: 150.0,
            last_all_in: true,
            bid_scale: 1.0,
            denial_weight: 0.0,
            flush_ms: 250,
        }
    }
}

/// One auction's decision, for logs.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Decision {
    pub auction_id: u64,
    pub letter: char,
    pub value: f64,
    pub lambda: f64,
    pub bid: i64,
    pub balance: i64,
    pub expected_buys: f64,
}

pub struct Scrabbler {
    pub params: Params,
    label: String,
    dict: &'static Dict,
    valuer: Valuer,
    flush_solver: Solver<'static>,
    pub prices: PriceModel,
    match_id: Option<u64>,
    long_cache: Option<(u128, Vec<(u32, u32, u8)>)>,
    bid_on: Option<(u64, i64)>,
    flushed: bool,
    pub last_decision: Option<Decision>,
}

impl Scrabbler {
    pub fn new(dict: &'static Dict, params: Params, seed: u64) -> Self {
        let mut flush_solver = Solver::new(dict);
        flush_solver.set_limits(None, Some(Duration::from_millis(params.flush_ms)));
        Scrabbler {
            valuer: Valuer::new(dict, seed, params.node_cap),
            params,
            label: "scrabbler".into(),
            dict,
            flush_solver,
            prices: PriceModel::default(),
            match_id: None,
            long_cache: None,
            bid_on: None,
            flushed: false,
            last_decision: None,
        }
    }

    pub fn with_label(mut self, label: &str) -> Self {
        self.label = label.to_string();
        self
    }

    fn reset_for(&mut self, match_id: u64) {
        if self.match_id != Some(match_id) {
            self.match_id = Some(match_id);
            self.bid_on = None;
            self.flushed = false;
            self.valuer.reseed(match_id ^ 0xA5A5_5A5A);
        }
    }

    /// The rack's best partition as `(class, reward, len)`, best first.
    fn partition(&mut self, rack: &Rack) -> Vec<(u32, u32, u8)> {
        if let Some((k, v)) = &self.long_cache {
            if *k == rack.key() {
                return v.clone();
            }
        }
        let sol = self.flush_solver.solve(rack);
        let mut out: Vec<(u32, u32, u8)> = sol
            .classes
            .iter()
            .map(|&c| {
                let cl = &self.dict.classes[c as usize];
                (c, cl.reward as u32, cl.len)
            })
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1));
        self.long_cache = Some((rack.key(), out.clone()));
        out
    }

    /// Value of the 7+ letter words in the rack's best partition.
    fn long_words(&mut self, rack: &Rack) -> Vec<(u32, u32)> {
        self.partition(rack).into_iter().filter(|w| w.2 >= 7).map(|w| (w.0, w.1)).collect()
    }

    fn v0(&self, letter: u8) -> f64 {
        self.params.v0_scale * 3.0 * value(letter) as f64
    }

    /// Points per coin for the rest of the match, plus expected future buys.
    fn pacing(&self, view: &MatchView, info: &PoolInfo, pmfs: &[Pmf], cur: u8, v_cur: f64, liquid: f64) -> (f64, f64, [f64; NUM_LETTERS]) {
        let p = &self.params;
        let vickrey = view.settings.auction_type == AuctionType::Vickrey;
        let n_future = view.bag_total as f64;
        let unseen = info.unseen.total().max(1) as f64;
        let q: Vec<f64> = (0..NUM_LETTERS).map(|l| info.unseen.0[l] as f64 / unseen).collect();
        let balance = view.my_balance() as f64;

        let eval = |lambda: f64| -> (f64, f64, f64, [f64; NUM_LETTERS]) {
            // Current auction at its own value, future ones at the prior.
            let b_cur = (v_cur / lambda).min(balance).max(0.0).round() as i64;
            let (_, mut spend) = outcome(&pmfs[cur as usize], b_cur, vickrey);
            let mut buys = 0.0;
            let mut gain = 0.0;
            let mut wins = [0.0; NUM_LETTERS];
            for l in 0..NUM_LETTERS {
                if q[l] == 0.0 {
                    continue;
                }
                let v = self.v0(l as u8);
                let b = (v / lambda).min(p.future_bid_cap).max(0.0).round() as i64;
                let (w, c) = outcome(&pmfs[l], b, vickrey);
                wins[l] = w;
                spend += n_future * q[l] * c;
                buys += n_future * q[l] * w;
                gain += n_future * q[l] * w * v;
            }
            (spend, buys, gain, wins)
        };
        let budget = |gain: f64| balance + p.liquid_rate * liquid + p.income_rate * gain;

        let (s_min, b_min, g_min, w_min) = eval(p.lambda_min);
        if s_min <= budget(g_min) {
            return (p.lambda_min, b_min, w_min);
        }
        let (mut lo, mut hi) = (p.lambda_min.ln(), p.lambda_max.ln());
        for _ in 0..24 {
            let mid = 0.5 * (lo + hi);
            let (s, _, g, _) = eval(mid.exp());
            if s <= budget(g) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let lambda = hi.exp();
        let (_, buys, _, wins) = eval(lambda);
        (lambda, buys, wins)
    }

    /// Competitor price model for every letter, plus bankable value now.
    fn context(&mut self, view: &MatchView) -> (Vec<Pmf>, f64) {
        let pmfs: Vec<Pmf> = (0..NUM_LETTERS as u8).map(|l| self.prices.competitor_pmf(view, l)).collect();
        let liquid: f64 = if view.my_rack.total() >= 7 {
            self.long_words(&view.my_rack).iter().map(|w| w.1 as f64).sum()
        } else {
            0.0
        };
        (pmfs, liquid)
    }

    /// Words to bank now, if any (best first).
    fn choose_plays(&mut self, view: &MatchView, lambda: f64) -> Vec<String> {
        let p = self.params.clone();
        let rack = view.my_rack;
        if rack.total() < 2 {
            return vec![];
        }
        let balance = view.my_balance();
        let words = self.partition(&rack);
        let long = words.iter().find(|w| w.2 >= 7);
        let want_long = p.always_play_long
            || lambda >= p.play_long_lambda
            || balance < p.play_floor
            || rack.total() > p.rack_limit;
        if let Some(w) = long {
            if want_long {
                return vec![self.dict.class_word(w.0).to_string()];
            }
        }
        // Starved: broke with auctions to go. Bank the best decent word.
        if balance < p.starve_floor && view.auctions_left() > p.starve_min_left {
            if let Some(w) = words.iter().find(|w| w.2 >= p.starve_min_len) {
                return vec![self.dict.class_word(w.0).to_string()];
            }
        }
        vec![]
    }

    fn decide_bid(&mut self, view: &MatchView, info: &PoolInfo, pmfs: &[Pmf], liquid: f64) -> i64 {
        let open = view.open.expect("open auction");
        let letter = open.letter;
        let balance = view.my_balance();
        if balance <= 0 {
            return 0;
        }
        let p = self.params.clone();

        // Size the projection with the prior value, value the tile by Monte
        // Carlo (shrunk toward the prior), then re-pace with that value.
        let (_, buys, wins) = self.pacing(view, info, pmfs, letter, self.v0(letter), liquid);
        let mut weights = wins;
        for w in weights.iter_mut() {
            *w = p.proj_uniform + (1.0 - p.proj_uniform) * *w;
        }
        let proj = Projection { pool: info.unseen, weights, m: buys.round().max(0.0) as usize };
        let v_mc = self.valuer.marginal(&view.my_rack, &[letter], &proj, p.samples, p.proj_cap)[0];
        let k = p.samples as f64;
        let v = ((k * v_mc + p.shrink_k0 * self.v0(letter)) / (k + p.shrink_k0)).max(0.0);
        let (lambda, buys, _) = self.pacing(view, info, pmfs, letter, v, liquid);

        let coins = (p.bid_scale * v / lambda).max(0.0);
        let bid = match view.settings.auction_type {
            AuctionType::Vickrey => coins.round() as i64,
            AuctionType::FirstPrice => {
                // argmax_b P(win | b) * (coins - b)
                let pmf = &pmfs[letter as usize];
                let mut best = (0.0, 0i64);
                let top = (coins.floor() as i64).min(balance);
                for b in 1..=top {
                    let (w, _) = outcome(pmf, b, false);
                    let u = w * (coins - b as f64);
                    if u > best.0 {
                        best = (u, b);
                    }
                }
                best.1
            }
        }
        .clamp(0, balance);

        self.last_decision = Some(Decision {
            auction_id: open.auction_id,
            letter: (b'A' + letter) as char,
            value: v,
            lambda,
            bid,
            balance,
            expected_buys: buys,
        });
        bid
    }
}

impl Strategy for Scrabbler {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn on_update(&mut self, view: &MatchView) -> Vec<Action> {
        let mut out = Vec::new();
        if view.ended {
            return out;
        }
        self.reset_for(view.match_id);
        let Some(open) = view.open else { return out };

        if view.is_last_auction() {
            if !self.flushed {
                self.flushed = true;
                let sol = self.flush_solver.solve(&view.my_rack);
                for w in sol.words(self.dict) {
                    out.push(Action::PlayWord(w.to_string()));
                }
            }
            if self.params.last_all_in && self.bid_on.map(|b| b.0) != Some(open.auction_id) {
                let amount = view.my_balance();
                self.bid_on = Some((open.auction_id, amount));
                if amount > 0 {
                    out.push(Action::Bid { auction_id: open.auction_id, amount });
                }
            }
            return out;
        }

        let info = pool::analyze(view);
        let (pmfs, liquid) = self.context(view);
        let (lambda, _, _) = self.pacing(view, &info, &pmfs, open.letter, self.v0(open.letter), liquid);
        let plays = self.choose_plays(view, lambda);
        if !plays.is_empty() {
            // Bid on the next round, once the reward is in the balance.
            return plays.into_iter().map(Action::PlayWord).collect();
        }

        if self.bid_on.map(|b| b.0) != Some(open.auction_id) {
            let amount = self.decide_bid(view, &info, &pmfs, liquid);
            self.bid_on = Some((open.auction_id, amount));
            if amount > 0 {
                out.push(Action::Bid { auction_id: open.auction_id, amount });
            }
        }
        let _ = Phase::Lobby;
        out
    }

    fn on_match_end(&mut self, view: &MatchView, top_bids: &[(u64, i64)]) {
        self.prices.observe_match(view, top_bids);
    }
}
