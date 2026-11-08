//! Rack solver: the set of disjoint words with the highest total reward.
//!
//! No word scores above 3x its base, so maximizing reward is minimizing
//! total *loss*: `3*base - reward` per word plus `3*value` per unused tile.
//!
//! Search (exact cover style): at each sub-rack, keep the candidate words
//! that still fit and whose loss is under the budget, pick the letter with
//! the fewest such words (the pivot), and branch: spend the pivot in one of
//! those words, or leave every copy of it unused. Candidates are kept in loss
//! order, the loss budget shrinks as better partitions turn up, and the memo
//! keeps exact losses and proven lower bounds per sub-rack. The root deepens
//! the budget iteratively, so cheap partitions are proven first.
//!
//! A node budget / deadline makes it anytime: once spent, unsolved sub-racks
//! are filled greedily and the result is flagged inexact.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::time::{Duration, Instant};

use super::dict::Dict;
use super::rules::{NUM_LETTERS, Rack, value};

#[derive(Default)]
pub struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }
    #[inline]
    fn write_u128(&mut self, k: u128) {
        let lo = k as u64;
        let hi = (k >> 64) as u64;
        let mut x = lo ^ hi.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        self.0 = x ^ (x >> 31);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<KeyHasher>>;

/// Leave every copy of the pivot unused.
const LEAVE: u32 = u32::MAX;
/// Filled greedily after the budget ran out.
const GREEDY: u32 = u32::MAX - 1;
const INF: u32 = u32::MAX / 4;
const MEMO_CAP: usize = 3_000_000;

#[derive(Clone, Copy)]
struct Entry {
    /// Exact minimum loss, or a proven lower bound on it.
    loss: u32,
    exact: bool,
    choice: u32,
    /// The pivot letter `LEAVE` refers to.
    pivot: u8,
}

/// A partition of a rack into words.
#[derive(Clone, Debug, Default)]
pub struct Solution {
    pub value: u32,
    /// Class ids of the chosen words.
    pub classes: Vec<u32>,
    /// False when the work budget ran out and part of the answer is greedy.
    pub exact: bool,
}

impl Solution {
    pub fn words<'a>(&self, dict: &'a Dict) -> Vec<&'a str> {
        self.classes.iter().map(|&c| dict.class_word(c)).collect()
    }

    /// Tiles the partition uses.
    pub fn used(&self, dict: &Dict) -> Rack {
        let mut r = Rack::EMPTY;
        for &c in &self.classes {
            r = r.plus(&dict.classes[c as usize].counts);
        }
        r
    }
}

pub struct Solver<'d> {
    dict: &'d Dict,
    memo: FastMap<u128, Entry>,
    /// Candidate lists of the active search path, each in loss order.
    stack: Vec<u32>,
    /// Root candidates by reward, for greedy fill.
    all: Vec<u32>,
    nodes: u64,
    budget: u64,
    max_time: Option<Duration>,
    deadline: Option<Instant>,
    exhausted: bool,
    memo_min_len: usize,
    pub stats_nodes: u64,
}

impl<'d> Solver<'d> {
    pub fn new(dict: &'d Dict) -> Self {
        Solver {
            dict,
            memo: FastMap::default(),
            stack: Vec::new(),
            all: Vec::new(),
            nodes: 0,
            budget: u64::MAX,
            max_time: None,
            deadline: None,
            exhausted: false,
            memo_min_len: 2,
            stats_nodes: 0,
        }
    }

    pub fn dict(&self) -> &'d Dict {
        self.dict
    }

    /// Caps the work of each later solve (`None` = unlimited).
    pub fn set_limits(&mut self, max_nodes: Option<u64>, max_time: Option<Duration>) {
        self.budget = max_nodes.unwrap_or(u64::MAX);
        self.max_time = max_time;
    }

    fn prepare(&mut self, rack: &Rack, min_len: usize, keep_memo: bool) {
        // Memo entries depend only on the sub-rack (never on the root), so
        // exact ones survive across solves with the same minimum length.
        if !keep_memo || self.exhausted || self.memo.len() > MEMO_CAP || min_len != self.memo_min_len {
            self.memo.clear();
        }
        self.memo_min_len = min_len;
        self.stack.clear();
        self.all.clear();
        self.nodes = 0;
        self.exhausted = false;
        self.deadline = self.max_time.map(|d| Instant::now() + d);
        let dict = self.dict;
        let all = &mut self.all;
        dict.for_each_fit(rack, min_len.max(2), |c| all.push(c));
        self.stack.extend_from_slice(&self.all);
        self.stack.sort_unstable_by_key(|&c| {
            let cl = &dict.classes[c as usize];
            (cl.loss, std::cmp::Reverse(cl.len), c)
        });
        self.all.sort_unstable_by_key(|&c| {
            let cl = &dict.classes[c as usize];
            (std::cmp::Reverse(cl.reward), cl.loss, c)
        });
    }

    /// Best total reward for `rack`.
    pub fn value(&mut self, rack: &Rack) -> u32 {
        self.prepare(rack, 2, false);
        let loss = self.root(*rack);
        3 * rack.base() - loss
    }

    /// Like [`Self::value`] but reuses sub-rack results from earlier calls;
    /// pays off when solving many overlapping racks.
    pub fn value_cached(&mut self, rack: &Rack) -> u32 {
        self.prepare(rack, 2, true);
        let loss = self.root(*rack);
        if self.exhausted {
            self.memo.clear();
        }
        3 * rack.base() - loss
    }

    /// Best partition of `rack` using words of at least `min_len` letters.
    pub fn solve_min_len(&mut self, rack: &Rack, min_len: usize) -> Solution {
        self.prepare(rack, min_len, false);
        let loss = self.root(*rack);
        let value = 3 * rack.base() - loss;
        let mut classes = Vec::new();
        let mut r = *rack;
        while !r.is_empty() {
            let Some(e) = self.memo.get(&r.key()).copied() else {
                break;
            };
            match e.choice {
                GREEDY => {
                    classes.extend(self.greedy_words(r));
                    break;
                }
                LEAVE => r.0[e.pivot as usize] = 0,
                c => {
                    classes.push(c);
                    r = r.minus(&self.dict.classes[c as usize].counts);
                }
            }
        }
        debug_assert_eq!(
            classes.iter().map(|&c| self.dict.classes[c as usize].reward as u32).sum::<u32>(),
            value
        );
        Solution { value, classes, exact: !self.exhausted }
    }

    pub fn solve(&mut self, rack: &Rack) -> Solution {
        self.solve_min_len(rack, 2)
    }

    /// Iterative deepening on the loss budget: failed rounds leave lower
    /// bounds in the memo for the next.
    fn root(&mut self, rack: Rack) -> u32 {
        let n = self.stack.len();
        // Round 1 proves a cheap partition if one exists; round 2 is bounded
        // by a greedy partition, so it always succeeds.
        let first = 8u32;
        let loss = self.search(rack, first, 0, n);
        if loss < first || self.exhausted {
            return loss;
        }
        let greedy = self.greedy_loss(rack);
        let loss = self.search(rack, greedy + 1, 0, n);
        loss.min(greedy)
    }

    fn over_budget(&mut self) -> bool {
        if self.exhausted {
            return true;
        }
        self.nodes += 1;
        self.stats_nodes += 1;
        if self.nodes > self.budget {
            self.exhausted = true;
        } else if self.nodes & 255 == 0 {
            if let Some(d) = self.deadline {
                if Instant::now() >= d {
                    self.exhausted = true;
                }
            }
        }
        self.exhausted
    }

    fn greedy_loss(&self, r: Rack) -> u32 {
        let used: u32 = self.greedy_words(r).iter().map(|&c| self.dict.classes[c as usize].reward as u32).sum();
        3 * r.base() - used
    }

    /// Minimum loss of `r` if it is below `budget`; otherwise some value
    /// `>= budget` that is a lower bound on it. `stack[lo..hi]` holds (in
    /// loss order) every candidate that might fit `r`.
    fn search(&mut self, r: Rack, budget: u32, lo: usize, hi: usize) -> u32 {
        if r.is_empty() {
            return 0;
        }
        let key = r.key();
        let prior_lb = match self.memo.get(&key) {
            Some(e) if e.exact || e.loss >= budget => return e.loss,
            Some(e) => e.loss,
            None => 0,
        };
        if self.over_budget() {
            let loss = self.greedy_loss(r);
            self.memo.insert(key, Entry { loss, exact: true, choice: GREEDY, pivot: 0 });
            return loss;
        }

        // A rack of n < 7 tiles can't reach 3x: its loss is at least what
        // one word using every tile would lose.
        let n_tiles = r.total();
        if n_tiles < 7 {
            let b = r.base();
            let lb = match n_tiles {
                0..=3 => 2 * b,
                4 => 3 * b - b * 3 / 2,
                5 => b,
                _ => 3 * b - b * 5 / 2,
            };
            if lb >= budget {
                return lb;
            }
        }

        // Candidates that fit and are under budget, still in loss order.
        let dict = self.dict;
        let start = self.stack.len();
        let mut opts = [0u16; NUM_LETTERS];
        let mut rmask = 0u32;
        for l in 0..NUM_LETTERS {
            if r.0[l] > 0 {
                rmask |= 1 << l;
            }
        }
        for i in lo..hi {
            let c = self.stack[i];
            let cl = &dict.classes[c as usize];
            if cl.loss as u32 >= budget {
                break;
            }
            if cl.mask & !rmask == 0 && r.contains(&cl.counts) {
                self.stack.push(c);
                let mut m = cl.mask;
                while m != 0 {
                    opts[m.trailing_zeros() as usize] += 1;
                    m &= m - 1;
                }
            }
        }
        let end = self.stack.len();

        // Pivot: the letter on the rack with the fewest candidates; ties go
        // to the rarer letter.
        let mut pivot = 0u8;
        let mut best_key = (u16::MAX, u8::MAX);
        for l in 0..NUM_LETTERS {
            if r.0[l] > 0 {
                let k = (opts[l], dict.rank[l]);
                if k < best_key {
                    best_key = k;
                    pivot = l as u8;
                }
            }
        }
        let pbit = 1u32 << pivot;
        let leave_loss = 3 * value(pivot) * r.0[pivot as usize] as u32;

        let mut best = INF;
        let mut choice = LEAVE;
        let mut leave_done = false;
        let mut i = start;
        loop {
            // Next candidate containing the pivot.
            let mut next = None;
            while i < end {
                let c = self.stack[i];
                i += 1;
                if dict.classes[c as usize].mask & pbit != 0 {
                    next = Some(c);
                    break;
                }
            }
            let next_loss = next.map(|c| dict.classes[c as usize].loss as u32).unwrap_or(INF);
            if !leave_done && leave_loss <= next_loss {
                leave_done = true;
                let cap = budget.min(best);
                if leave_loss < cap {
                    let mut rr = r;
                    rr.0[pivot as usize] = 0;
                    let sub = self.search(rr, cap - leave_loss, start, end);
                    // Only exact child results (below the child budget) count.
                    if leave_loss + sub < cap {
                        best = leave_loss + sub;
                        choice = LEAVE;
                    }
                }
                if self.exhausted {
                    break;
                }
            }
            let Some(c) = next else { break };
            let cap = budget.min(best);
            if next_loss >= cap {
                break;
            }
            let cl = &dict.classes[c as usize];
            let sub = self.search(r.minus(&cl.counts), cap - next_loss, start, end);
            if next_loss + sub < cap {
                best = next_loss + sub;
                choice = c;
            }
            if self.exhausted {
                break;
            }
        }
        self.stack.truncate(start);

        if self.exhausted {
            // Out of budget: keep the best complete option, else go greedy.
            if best >= INF {
                best = self.greedy_loss(r);
                choice = GREEDY;
            }
            self.memo.insert(key, Entry { loss: best, exact: true, choice, pivot });
            return best;
        }
        if best < budget {
            self.memo.insert(key, Entry { loss: best, exact: true, choice, pivot });
            best
        } else {
            let lb = budget.max(prior_lb);
            self.memo.insert(key, Entry { loss: lb, exact: false, choice: LEAVE, pivot });
            lb
        }
    }

    /// Greedy fill: repeatedly take the highest-reward fitting word.
    fn greedy_words(&self, mut r: Rack) -> Vec<u32> {
        let mut out = Vec::new();
        'outer: loop {
            for &c in &self.all {
                let cl = &self.dict.classes[c as usize];
                if r.contains(&cl.counts) {
                    out.push(c);
                    r = r.minus(&cl.counts);
                    continue 'outer;
                }
            }
            return out;
        }
    }
}

/// Exhaustive reference solver for property tests (small racks only).
pub fn brute_force(dict: &Dict, rack: &Rack) -> u32 {
    let mut cands = Vec::new();
    dict.for_each_fit(rack, 2, |c| cands.push(c));
    fn go(dict: &Dict, cands: &[u32], start: usize, r: Rack) -> u32 {
        let mut best = 0;
        for i in start..cands.len() {
            let cl = &dict.classes[cands[i] as usize];
            if r.contains(&cl.counts) {
                // Index i again: a class may be used more than once.
                let v = cl.reward as u32 + go(dict, cands, i, r.minus(&cl.counts));
                best = best.max(v);
            }
        }
        best
    }
    go(dict, &cands, 0, *rack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_dictionary_partition() {
        let d = Dict::from_text("CAT\nDOG\nCATDOG\nAT\nGOD\nTA\n").unwrap();
        let mut s = Solver::new(&d);
        let sol = s.solve(&Rack::from_letters("CATDOGX"));
        // CATDOG is only in this test list: 6 letters, base 10 -> 25.
        assert_eq!(sol.value, 25);
        assert!(sol.exact);
        let sol = s.solve(&Rack::from_letters("CATDOGCAT"));
        assert_eq!(sol.value, 30);
        assert_eq!(sol.classes.len(), 2);
        assert_eq!(brute_force(&d, &Rack::from_letters("CATDOGCAT")), 30);
        assert_eq!(s.value_cached(&Rack::from_letters("CATDOGCAT")), 30);
        assert_eq!(s.value_cached(&Rack::from_letters("CATCAT")), 10);
    }
}
