use std::time::Instant;
use scrabble_bot::game::dict::Dict;
use scrabble_bot::game::scrabbler::{Params, Scrabbler};
use scrabble_bot::sim::agents::{HoardFlushAgent, StarterAgent};
use scrabble_bot::sim::engine::{EngineConfig, Seat};
use scrabble_bot::sim::harness::{Scenario, format_stats, run, seat_stats};

fn main() -> anyhow::Result<()> {
    let dict: &'static Dict = Dict::load_default()?.leak();
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(200);
    let p = Params::default();
    let p2 = p.clone();
    let league = Scenario {
        name: "league".into(),
        seats: vec![
            Box::new(move || Seat::agent("scrabbler-a", Box::new(Scrabbler::new(dict, p.clone(), 1)))),
            Box::new(move || Seat::agent("scrabbler-b", Box::new(Scrabbler::new(dict, p2.clone(), 2)))),
            Box::new(move || Seat::agent("hoard-1", Box::new(HoardFlushAgent::new(dict)))),
            Box::new(move || Seat::agent("hoard-2", Box::new(HoardFlushAgent::new(dict)))),
            Box::new(move || Seat::agent("starter-1", Box::new(StarterAgent::new(dict)))),
            Box::new(move || Seat::agent("starter-2", Box::new(StarterAgent::new(dict)))),
        ],
        pad_sims: 0,
        cfg: EngineConfig::default(),
    };
    let t = Instant::now();
    let out = run(dict, &league, n, 11);
    println!("{}  ({:?})", format_stats("6-bot league", &seat_stats(&out)), t.elapsed());
    Ok(())
}
