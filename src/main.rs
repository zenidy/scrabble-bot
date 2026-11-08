use std::time::Instant;

use scrabble_bot::game::dict::Dict;
use scrabble_bot::game::rules::SimStrategy;
use scrabble_bot::sim::agents::{HoardFlushAgent, StarterAgent};
use scrabble_bot::sim::engine::{EngineConfig, Seat};
use scrabble_bot::sim::harness::{Scenario, format_stats, run, seat_stats};

fn main() -> anyhow::Result<()> {
    let dict: &'static Dict = Dict::load_default()?.leak();
    let cfg = EngineConfig::default();
    let sims_only = Scenario {
        name: "sims only".into(),
        seats: vec![],
        pad_sims: 6,
        cfg: cfg.clone(),
    };
    let t = Instant::now();
    let out = run(dict, &sims_only, 2000, 1);
    println!("{}  ({:?})", format_stats("6 sims", &seat_stats(&out)), t.elapsed());
    let starter = Scenario {
        name: "starter".into(),
        seats: vec![Box::new(move || Seat::agent("starter", Box::new(StarterAgent::new(dict))))],
        pad_sims: 5,
        cfg: cfg.clone(),
    };
    let t = Instant::now();
    let out = run(dict, &starter, 2000, 1);
    println!("{}  ({:?})", format_stats("starter + 5 sims", &seat_stats(&out)), t.elapsed());
    let hoard = Scenario {
        name: "hoard".into(),
        seats: vec![Box::new(move || Seat::agent("hoard-flush", Box::new(HoardFlushAgent::new(dict))))],
        pad_sims: 5,
        cfg: cfg.clone(),
    };
    let t = Instant::now();
    let out = run(dict, &hoard, 2000, 1);
    println!("{}  ({:?})", format_stats("hoard-flush + 5 sims", &seat_stats(&out)), t.elapsed());
    let _ = SimStrategy::Aggressive;
    Ok(())
}
