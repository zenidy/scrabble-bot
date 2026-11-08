//! Baseline strategies to measure against.

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::game::dict::Dict;
use crate::game::rules::{Rack, is_vowel, value};
use crate::game::solver::Solver;
use crate::game::view::{Action, MatchView, Strategy};

/// (a) The TypeScript starter, ported: bid value+2 for letters it lacks,
/// else value; play the longest word it can spell right away.
pub struct StarterAgent {
    dict: &'static Dict,
    bid_on: Option<u64>,
    words_this_auction: (u64, u32),
}

impl StarterAgent {
    pub fn new(dict: &'static Dict) -> Self {
        StarterAgent {
            dict,
            bid_on: None,
            words_this_auction: (0, 0),
        }
    }
}

impl Strategy for StarterAgent {
    fn name(&self) -> String {
        "starter".into()
    }

    fn on_update(&mut self, view: &MatchView) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(open) = view.open else { return out };
        // The starter retries words at most every 500 ms: ~2 per 1 s auction.
        if self.words_this_auction.0 != open.auction_id {
            self.words_this_auction = (open.auction_id, 0);
        }
        let mut rack = view.my_rack;
        if self.words_this_auction.1 < 2
            && let Some(c) = self.dict.best_playable(&rack, 2)
        {
            let w = self.dict.class_word(c).to_string();
            rack = rack.minus(&self.dict.classes[c as usize].counts);
            self.words_this_auction.1 += 1;
            out.push(Action::PlayWord(w));
        }
        if self.bid_on != Some(open.auction_id) {
            self.bid_on = Some(open.auction_id);
            let v = value(open.letter) as i64;
            let want = if rack.count(open.letter) == 0 {
                v + 2
            } else {
                v.max(1)
            };
            let amount = want.min(view.my_balance().max(0));
            if amount > 0 {
                out.push(Action::Bid {
                    auction_id: open.auction_id,
                    amount,
                });
            }
        }
        out
    }
}

/// (b) Hoard and flush: bid value+4 (x2.5 in the last 20 auctions), play
/// the best 7+ letter word when the balance drops below 8, and play the
/// best partition of the whole rack when the last auction opens.
pub struct HoardFlushAgent {
    dict: &'static Dict,
    solver: Solver<'static>,
    bid_on: Option<u64>,
    flushed: bool,
}

impl HoardFlushAgent {
    pub fn new(dict: &'static Dict) -> Self {
        let mut solver = Solver::new(dict);
        solver.set_limits(Some(200_000), Some(std::time::Duration::from_millis(200)));
        HoardFlushAgent {
            dict,
            solver,
            bid_on: None,
            flushed: false,
        }
    }

    fn best_long_word(&self, rack: &Rack) -> Option<u32> {
        let mut best: Option<(u16, u32)> = None;
        self.dict.for_each_fit(rack, 7, |c| {
            let r = self.dict.classes[c as usize].reward;
            if best.is_none_or(|(br, bc)| r > br || (r == br && c < bc)) {
                best = Some((r, c));
            }
        });
        best.map(|(_, c)| c)
    }
}

impl Strategy for HoardFlushAgent {
    fn name(&self) -> String {
        "hoard-flush".into()
    }

    fn on_update(&mut self, view: &MatchView) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(open) = view.open else { return out };
        if view.is_last_auction() {
            if !self.flushed {
                self.flushed = true;
                let sol = self.solver.solve(&view.my_rack);
                for w in sol.words(self.dict) {
                    out.push(Action::PlayWord(w.to_string()));
                }
            }
        } else if view.my_balance() < 8
            && let Some(c) = self.best_long_word(&view.my_rack)
        {
            // Bid on the next round, once the reward is in the balance.
            out.push(Action::PlayWord(self.dict.class_word(c).to_string()));
            return out;
        }
        if self.bid_on != Some(open.auction_id) {
            self.bid_on = Some(open.auction_id);
            let mut b = value(open.letter) as i64 + 4;
            if view.auctions_left() <= 20 {
                b = b * 5 / 2;
            }
            let amount = b.min(view.my_balance().max(0));
            if amount > 0 {
                out.push(Action::Bid {
                    auction_id: open.auction_id,
                    amount,
                });
            }
        }
        out
    }
}

/// (c) A stand-in for the live lobby's real bots, fitted to their prices
/// (Nov 2026, 5 matches: the winning bid is ~3.5 x (value + 3), the
/// runner-up ~2.75 x). Bids `aggr * need * noise * (value + 3)` with a
/// per-match `aggr`, a rack-balance `need` and per-auction noise; banks any
/// 7+ letter word at once (recycling the reward) and flushes at the end.
pub struct MarketAgent {
    dict: &'static Dict,
    solver: Solver<'static>,
    rng: Option<ChaCha8Rng>,
    aggr: f64,
    bid_on: Option<u64>,
    flushed: bool,
}

/// Per-match aggressiveness range of [`MarketAgent`].
pub const MARKET_AGGR: (f64, f64) = (2.3, 3.4);

impl MarketAgent {
    pub fn new(dict: &'static Dict) -> Self {
        let mut solver = Solver::new(dict);
        solver.set_limits(Some(200_000), Some(std::time::Duration::from_millis(200)));
        MarketAgent {
            dict,
            solver,
            rng: None,
            aggr: 0.0,
            bid_on: None,
            flushed: false,
        }
    }

    /// More for what balances the rack's vowels, less for a third copy.
    fn need(rack: &Rack, letter: u8) -> f64 {
        let total = rack.total();
        let mut need = 1.0;
        if total > 0 {
            let vowels = (0..26u8)
                .filter(|&l| is_vowel(l))
                .map(|l| rack.count(l) as u32)
                .sum::<u32>();
            let frac = vowels as f64 / total as f64;
            let (short, long) = if is_vowel(letter) {
                (frac < 0.3, frac > 0.5)
            } else {
                (frac > 0.5, frac < 0.3)
            };
            need = if short {
                1.3
            } else if long {
                0.6
            } else {
                1.0
            };
        }
        if rack.count(letter) >= 2 {
            need *= 0.5;
        }
        need
    }
}

impl Strategy for MarketAgent {
    fn name(&self) -> String {
        "market".into()
    }

    fn on_update(&mut self, view: &MatchView) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(open) = view.open else { return out };
        let rng = self.rng.get_or_insert_with(|| {
            ChaCha8Rng::seed_from_u64(view.match_id.wrapping_mul(0x9E37_79B9) ^ view.my_bot_id)
        });
        if self.aggr == 0.0 {
            self.aggr = rng.random_range(MARKET_AGGR.0..MARKET_AGGR.1);
        }
        if view.is_last_auction() {
            if !self.flushed {
                self.flushed = true;
                let sol = self.solver.solve(&view.my_rack);
                for w in sol.words(self.dict) {
                    out.push(Action::PlayWord(w.to_string()));
                }
            }
            return out;
        }
        let mut best: Option<(u16, u32)> = None;
        self.dict.for_each_fit(&view.my_rack, 7, |c| {
            let r = self.dict.classes[c as usize].reward;
            if best.is_none_or(|(br, bc)| r > br || (r == br && c < bc)) {
                best = Some((r, c));
            }
        });
        if let Some((_, c)) = best {
            out.push(Action::PlayWord(self.dict.class_word(c).to_string()));
            return out;
        }
        if self.bid_on != Some(open.auction_id) {
            self.bid_on = Some(open.auction_id);
            let noise = rng.random_range(0.7..1.3);
            let v = value(open.letter) as f64 + 3.0;
            let b = (self.aggr * Self::need(&view.my_rack, open.letter) * noise * v).round() as i64;
            let amount = b.min(view.my_balance().max(0));
            if amount > 0 {
                out.push(Action::Bid {
                    auction_id: open.auction_id,
                    amount,
                });
            }
        }
        out
    }
}
