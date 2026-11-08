use std::time::Instant;

use scrabble_bot::game::dict::Dict;
use scrabble_bot::game::scrabbler::{Params, Scrabbler};
use scrabble_bot::sim::agents::HoardFlushAgent;
use scrabble_bot::sim::engine::{EngineConfig, Seat};
use scrabble_bot::sim::harness::{Scenario, format_stats, paired_diff, run, seat_stats};

fn main() -> anyhow::Result<()> {
    let dict: &'static Dict = Dict::load_default()?.leak();
    let cfg = EngineConfig::default();
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(400);
    let hoard = Scenario {
        name: "hoard".into(),
        seats: vec![Box::new(move || Seat::agent("hoard-flush", Box::new(HoardFlushAgent::new(dict))))],
        pad_sims: 5,
        cfg: cfg.clone(),
    };
    let t = Instant::now();
    let hb = run(dict, &hoard, n, 1);
    println!("{}  ({:?})", format_stats("hoard-flush + 5 sims", &seat_stats(&hb)), t.elapsed());
    let mut params = Params::default();
    if std::env::args().any(|a| a == "--play-long") { params.always_play_long = true; }
    if let Some(f) = std::env::args().find_map(|a| a.strip_prefix("--floor=").map(|x| x.parse::<i64>().unwrap())) { params.play_floor = f; }
    if let Some(f) = std::env::args().find_map(|a| a.strip_prefix("--cap=").map(|x| x.parse::<usize>().unwrap())) { params.proj_cap = f; }
    if let Some(f) = std::env::args().find_map(|a| a.strip_prefix("--k=").map(|x| x.parse::<usize>().unwrap())) { params.samples = f; }
    if let Some(f) = std::env::args().find_map(|a| a.strip_prefix("--limit=").map(|x| x.parse::<u32>().unwrap())) { params.rack_limit = f; }
    println!("params: {params:?}");
    let scr = Scenario {
        name: "scrabbler".into(),
        seats: vec![Box::new(move || Seat::agent("scrabbler", Box::new(Scrabbler::new(dict, params.clone(), 7))))],
        pad_sims: 5,
        cfg: cfg.clone(),
    };
    let t = Instant::now();
    let sb = run(dict, &scr, n, 1);
    println!("{}  ({:?})", format_stats("scrabbler + 5 sims", &seat_stats(&sb)), t.elapsed());
    let (ds, dm) = paired_diff(&sb, "scrabbler", &hb, "hoard-flush");
    println!("paired score diff {:.1} ± {:.1}, margin diff {:.1} ± {:.1}", ds.mean, ds.ci, dm.mean, dm.ci);
    Ok(())
}
