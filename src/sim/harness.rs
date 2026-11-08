//! Runs many seeded matches in parallel and reports per-seat statistics
//! with 95% confidence intervals.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;

use super::engine::{EngineConfig, MatchOutcome, Seat, run_match};
use crate::game::dict::Dict;
use crate::game::rules::SimStrategy;

pub type SeatFactory = Box<dyn Fn() -> Seat + Send + Sync>;

/// The six module sims (`init` in index.ts).
pub const SIM_POOL: [(&str, SimStrategy); 6] = [
    ("Cheapo-sim", SimStrategy::Cheapskate),
    ("Valor-sim", SimStrategy::ValueBidder),
    ("Brutus-sim", SimStrategy::Aggressive),
    ("Hagrid-sim", SimStrategy::ValueBidder),
    ("Maverick-sim", SimStrategy::Aggressive),
    ("Snippet-sim", SimStrategy::Cheapskate),
];

pub struct Scenario {
    pub name: String,
    /// Fixed seats (real bots), in roster order.
    pub seats: Vec<SeatFactory>,
    /// Pad with this many distinct random sims from [`SIM_POOL`] (lobby timeout).
    pub pad_sims: usize,
    pub cfg: EngineConfig,
}

impl Scenario {
    fn build(&self, seed: u64) -> Vec<Seat> {
        let mut seats: Vec<Seat> = self.seats.iter().map(|f| f()).collect();
        if self.pad_sims > 0 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed ^ 0x5157_5157);
            let mut pool: Vec<usize> = (0..SIM_POOL.len()).collect();
            // Same Fisher-Yates as the module's lobby padding.
            for i in (1..pool.len()).rev() {
                let j = rand::RngExt::random_range(&mut rng, 0..=i);
                pool.swap(i, j);
            }
            for &k in pool.iter().take(self.pad_sims) {
                let (name, s) = SIM_POOL[k];
                seats.push(Seat::sim(&format!("{s:?}"), s));
                let _ = name;
            }
        }
        seats
    }
}

/// Runs `matches` matches with seeds `seed..seed+matches`.
pub fn run(dict: &Dict, scenario: &Scenario, matches: usize, seed: u64) -> Vec<MatchOutcome> {
    (0..matches as u64)
        .into_par_iter()
        .map(|i| {
            let s = seed + i;
            let mut seats = scenario.build(s);
            run_match(dict, &mut seats, &scenario.cfg, s)
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub n: usize,
    pub mean: f64,
    pub ci: f64,
}

pub fn summarize(xs: &[f64]) -> Summary {
    let n = xs.len();
    if n == 0 {
        return Summary::default();
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = if n > 1 {
        xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64
    } else {
        0.0
    };
    Summary {
        n,
        mean,
        ci: 1.96 * (var / n as f64).sqrt(),
    }
}

pub fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = v.len() / 2;
    if v.len().is_multiple_of(2) {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    }
}

#[derive(Clone, Debug, Default)]
pub struct SeatStats {
    pub label: String,
    pub score: Summary,
    pub median: f64,
    pub win: Summary,
    pub place: Summary,
    pub last: Summary,
    pub margin: Summary,
    pub late_bids: u64,
    pub failed: u64,
    pub tiles: f64,
    pub leftover: f64,
}

/// Per-label stats; seats sharing a name are pooled.
pub fn seat_stats(outcomes: &[MatchOutcome]) -> Vec<SeatStats> {
    let mut by: BTreeMap<String, Vec<(f64, f64, f64, f64, f64, u32, u32, u32, u32)>> =
        BTreeMap::new();
    for o in outcomes {
        for (i, s) in o.seats.iter().enumerate() {
            by.entry(s.name.clone()).or_default().push((
                s.score as f64,
                o.won(i) as u8 as f64,
                o.place(i) as f64,
                o.last(i) as u8 as f64,
                o.margin(i) as f64,
                s.late_bids,
                s.failed_actions,
                s.tiles_won,
                s.leftover_tiles,
            ));
        }
    }
    by.into_iter()
        .map(|(label, rows)| {
            let col = |f: fn(&(f64, f64, f64, f64, f64, u32, u32, u32, u32)) -> f64| {
                rows.iter().map(f).collect::<Vec<f64>>()
            };
            let scores = col(|r| r.0);
            SeatStats {
                label,
                score: summarize(&scores),
                median: median(&scores),
                win: summarize(&col(|r| r.1)),
                place: summarize(&col(|r| r.2)),
                last: summarize(&col(|r| r.3)),
                margin: summarize(&col(|r| r.4)),
                late_bids: rows.iter().map(|r| r.5 as u64).sum(),
                failed: rows.iter().map(|r| r.6 as u64).sum(),
                tiles: rows.iter().map(|r| r.7 as f64).sum::<f64>() / rows.len() as f64,
                leftover: rows.iter().map(|r| r.8 as f64).sum::<f64>() / rows.len() as f64,
            }
        })
        .collect()
}

pub fn format_stats(title: &str, stats: &[SeatStats]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "== {title}");
    let _ = writeln!(
        s,
        "{:<22} {:>6} {:>15} {:>7} {:>13} {:>12} {:>12} {:>15} {:>6} {:>6} {:>5} {:>5}",
        "seat",
        "n",
        "score",
        "median",
        "win%",
        "place",
        "P(last)%",
        "margin",
        "tiles",
        "left",
        "late",
        "fail"
    );
    for st in stats {
        let _ = writeln!(
            s,
            "{:<22} {:>6} {:>8.1}±{:<6.1} {:>7.1} {:>6.1}±{:<5.1} {:>5.2}±{:<5.2} {:>5.1}±{:<5.1} {:>8.1}±{:<6.1} {:>6.1} {:>6.1} {:>5} {:>5}",
            st.label,
            st.score.n,
            st.score.mean,
            st.score.ci,
            st.median,
            100.0 * st.win.mean,
            100.0 * st.win.ci,
            st.place.mean,
            st.place.ci,
            100.0 * st.last.mean,
            100.0 * st.last.ci,
            st.margin.mean,
            st.margin.ci,
            st.tiles,
            st.leftover,
            st.late_bids,
            st.failed
        );
    }
    s
}

/// Paired difference of a seat's score and margin between two runs on the
/// same seeds (`a - b`).
pub fn paired_diff(
    a: &[MatchOutcome],
    a_seat: &str,
    b: &[MatchOutcome],
    b_seat: &str,
) -> (Summary, Summary) {
    let pick = |o: &MatchOutcome, name: &str| o.seats.iter().position(|s| s.name == name);
    let mut ds = Vec::new();
    let mut dm = Vec::new();
    for (oa, ob) in a.iter().zip(b) {
        if let (Some(i), Some(j)) = (pick(oa, a_seat), pick(ob, b_seat)) {
            ds.push((oa.seats[i].score - ob.seats[j].score) as f64);
            dm.push((oa.margin(i) - ob.margin(j)) as f64);
        }
    }
    (summarize(&ds), summarize(&dm))
}
