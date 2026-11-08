//! Tile value in points, by Monte Carlo with common random numbers:
//! `V(L) = mean_k [ F(R + X_k + L) - F(R + X_k) ]`, where `F` is the best
//! partition value, `R` our rack and `X_k` a sampled set of tiles we expect
//! to win later (drawn from the unseen pool, weighted by how likely we are
//! to buy each letter). The same `X_k` serve every letter evaluated.

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use super::dict::Dict;
use super::rules::{NUM_LETTERS, Rack};
use super::solver::Solver;

#[derive(Clone, Debug)]
pub struct Projection {
    /// Tiles that may still come up (the unseen pool).
    pub pool: Rack,
    /// Relative chance we end up buying a tile of each letter.
    pub weights: [f64; NUM_LETTERS],
    /// How many future tiles we expect to win.
    pub m: usize,
}

pub struct Valuer {
    pub solver: Solver<'static>,
    rng: ChaCha8Rng,
    tiles: Vec<(u8, f64)>,
}

impl Valuer {
    pub fn new(dict: &'static Dict, seed: u64, node_cap: u64) -> Self {
        let mut solver = Solver::new(dict);
        solver.set_limits(Some(node_cap), None);
        Valuer { solver, rng: ChaCha8Rng::seed_from_u64(seed), tiles: Vec::new() }
    }

    pub fn reseed(&mut self, seed: u64) {
        self.rng = ChaCha8Rng::seed_from_u64(seed);
    }

    /// Draws `m` tiles from `pool` without replacement, tile weight by letter.
    pub fn sample(&mut self, pool: &Rack, weights: &[f64; NUM_LETTERS], m: usize) -> Rack {
        self.tiles.clear();
        for l in 0..NUM_LETTERS {
            let w = weights[l].max(1e-6);
            for _ in 0..pool.0[l] {
                self.tiles.push((l as u8, w));
            }
        }
        let mut out = Rack::EMPTY;
        let mut total: f64 = self.tiles.iter().map(|t| t.1).sum();
        for _ in 0..m.min(self.tiles.len()) {
            let mut x = self.rng.random::<f64>() * total;
            let mut pick = self.tiles.len() - 1;
            for (i, t) in self.tiles.iter().enumerate() {
                if x < t.1 {
                    pick = i;
                    break;
                }
                x -= t.1;
            }
            let (l, w) = self.tiles.swap_remove(pick);
            total -= w;
            out.add(l);
        }
        out
    }

    /// Marginal value of each of `letters` (same futures for all).
    pub fn marginal(&mut self, rack: &Rack, letters: &[u8], proj: &Projection, samples: usize, cap: usize) -> Vec<f64> {
        let mut sum = vec![0.0; letters.len()];
        let room = cap.saturating_sub(rack.total() as usize);
        let m = proj.m.min(room);
        let k = if m == 0 { 1 } else { samples.max(1) };
        for _ in 0..k {
            let x = if m == 0 { Rack::EMPTY } else { self.sample(&proj.pool, &proj.weights, m) };
            let base_rack = rack.plus(&x);
            for (i, &l) in letters.iter().enumerate() {
                let (v0, v1) = self.solver.pair_values(&base_rack, l);
                sum[i] += v1 as f64 - v0 as f64;
            }
        }
        sum.iter().map(|s| s / k as f64).collect()
    }
}
