//! Price model: the distribution of the highest competing bid in an auction.
//!
//! - Sims: exact. Each bids `clamp(base + j, 1, balance)` with `j` uniform
//!   in -2..=2, and nothing at balance 0; the max over sims is a CDF product.
//! - Real bots: a per-bot histogram of bids over `letter value + offset`,
//!   learned from the post-match archive (exact winning bids) and from
//!   `paid` when we win (the top competing bid), starting from a cautious
//!   prior. Persisted across runs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::rules::{SimStrategy, value};
use super::view::MatchView;

/// `pmf[x]` = P(bid == x); `pmf[0]` = no bid.
pub type Pmf = Vec<f64>;

pub fn sim_pmf(s: SimStrategy, letter: u8, balance: i64) -> Pmf {
    if balance <= 0 {
        return vec![1.0];
    }
    let mut pmf = vec![0.0; 1];
    for j in -2..=2 {
        let b = s.bid(letter, balance, j).max(0) as usize;
        if pmf.len() <= b {
            pmf.resize(b + 1, 0.0);
        }
        pmf[b] += 0.2;
    }
    pmf
}

/// Distribution of the max of independent bids.
pub fn max_pmf(pmfs: &[Pmf]) -> Pmf {
    let len = pmfs.iter().map(|p| p.len()).max().unwrap_or(1).max(1);
    let mut cdf = vec![1.0f64; len];
    for p in pmfs {
        let mut acc = 0.0;
        for (x, c) in cdf.iter_mut().enumerate() {
            acc += p.get(x).copied().unwrap_or(0.0);
            *c *= acc.min(1.0);
        }
    }
    let mut out = vec![0.0; len];
    let mut prev = 0.0;
    for x in 0..len {
        out[x] = (cdf[x] - prev).max(0.0);
        prev = cdf[x];
    }
    out
}

/// P(win) and E[payment, 0 if lost] for our bid `b` against max competing
/// bid `m ~ pmf`; ties are coin flips. Vickrey pays `m` (1 if nobody else
/// bid); first price pays `b`.
pub fn outcome(pmf: &Pmf, b: i64, vickrey: bool) -> (f64, f64) {
    if b <= 0 {
        return (0.0, 0.0);
    }
    let b = b as usize;
    let mut p_win = 0.0;
    let mut cost = 0.0;
    for (m, &p) in pmf.iter().enumerate() {
        if p == 0.0 {
            continue;
        }
        let w = if m < b {
            1.0
        } else if m == b {
            0.5
        } else {
            break;
        };
        p_win += p * w;
        let pay = if vickrey { (m.max(1)) as f64 } else { b as f64 };
        cost += p * w * pay;
    }
    (p_win, cost)
}

/// Bid histogram for one real bot over `value(letter) + offset`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BotModel {
    /// Pseudo-counts for offsets `OFFSET_MIN..=OFFSET_MAX`.
    pub hist: Vec<f64>,
    pub observations: u64,
}

pub const OFFSET_MIN: i64 = -10;
pub const OFFSET_MAX: i64 = 60;

impl BotModel {
    /// Cautious prior: mass from value-1 up to value+12, heavier near value+4.
    pub fn prior() -> Self {
        let n = (OFFSET_MAX - OFFSET_MIN + 1) as usize;
        let mut hist = vec![0.0; n];
        for off in -1..=12i64 {
            let w = 1.0 / (1.0 + ((off - 4) as f64 / 4.0).powi(2));
            hist[(off - OFFSET_MIN) as usize] = 0.6 * w;
        }
        BotModel {
            hist,
            observations: 0,
        }
    }

    pub fn observe(&mut self, letter: u8, bid: i64) {
        let off = (bid - value(letter) as i64).clamp(OFFSET_MIN, OFFSET_MAX);
        self.hist[(off - OFFSET_MIN) as usize] += 1.0;
        self.observations += 1;
    }

    pub fn pmf(&self, letter: u8, balance: i64) -> Pmf {
        if balance <= 0 {
            return vec![1.0];
        }
        let total: f64 = self.hist.iter().sum();
        let v = value(letter) as i64;
        let mut pmf = vec![0.0; 1];
        for (i, &w) in self.hist.iter().enumerate() {
            if w == 0.0 {
                continue;
            }
            let bid = (v + OFFSET_MIN + i as i64).clamp(0, balance) as usize;
            if pmf.len() <= bid {
                pmf.resize(bid + 1, 0.0);
            }
            pmf[bid] += w / total;
        }
        pmf
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PriceModel {
    pub bots: HashMap<u64, BotModel>,
}

impl PriceModel {
    /// Highest competing bid for `letter` among everyone but me.
    pub fn competitor_pmf(&self, view: &MatchView, letter: u8) -> Pmf {
        let mut parts = Vec::with_capacity(view.players.len());
        for p in view.players {
            if p.bot_id == view.my_bot_id {
                continue;
            }
            match p.sim {
                Some(s) => parts.push(sim_pmf(s, letter, p.balance)),
                None => parts.push(match self.bots.get(&p.bot_id) {
                    Some(m) => m.pmf(letter, p.balance),
                    None => BotModel::prior().pmf(letter, p.balance),
                }),
            }
        }
        max_pmf(&parts)
    }

    /// Learn from a finished match: archive top bids give each winner's bid.
    pub fn observe_match(&mut self, view: &MatchView, top_bids: &[(u64, i64)]) {
        let tops: HashMap<u64, i64> = top_bids.iter().copied().collect();
        for r in view.results {
            let Some(w) = r.winner else { continue };
            if w == view.my_bot_id {
                continue;
            }
            let is_sim = view
                .players
                .iter()
                .any(|p| p.bot_id == w && p.sim.is_some());
            if is_sim {
                continue;
            }
            if let Some(&top) = tops.get(&r.auction_id) {
                self.bots
                    .entry(w)
                    .or_insert_with(BotModel::prior)
                    .observe(r.letter, top);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_distributions() {
        // Aggressive on E (value 1, vowel): base 5, bids 3..=7.
        let p = sim_pmf(SimStrategy::Aggressive, 4, 100);
        assert_eq!(p.len(), 8);
        assert!((p[3..=7].iter().sum::<f64>() - 1.0).abs() < 1e-9);
        // Cheapskate on E: base 0 -> clamp to 1 for j <= 1, 2 for j = 2.
        let c = sim_pmf(SimStrategy::Cheapskate, 4, 100);
        assert!((c[1] - 0.8).abs() < 1e-9 && (c[2] - 0.2).abs() < 1e-9);
        let m = max_pmf(&[p.clone(), c]);
        assert!((m.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        // Balance 0: no bid.
        assert_eq!(sim_pmf(SimStrategy::Aggressive, 4, 0), vec![1.0]);
        let (w, cost) = outcome(&p, 8, true);
        assert!((w - 1.0).abs() < 1e-9);
        assert!((cost - 5.0).abs() < 1e-9);
    }
}
