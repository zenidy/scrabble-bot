use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use tracing_subscriber::EnvFilter;

use scrabble_bot::config::{BenchArgs, Cli, Cmd, RunArgs, SimArgs, TuneArgs};
use scrabble_bot::game::dict::Dict;
use scrabble_bot::game::rules::Rack;
use scrabble_bot::game::scrabbler::Params;
use scrabble_bot::game::solver::Solver;
use scrabble_bot::game::view::MatchSettings;
use scrabble_bot::net::runner::{self, RunConfig};
use scrabble_bot::sim::engine::EngineConfig;
use scrabble_bot::sim::harness::{format_stats, paired_diff, run, seat_stats, summarize};
use scrabble_bot::sim::scenarios;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let dict: &'static Dict = Dict::load_default()?.leak();
    match cli.cmd {
        Cmd::Run(a) => run_live(a, dict),
        Cmd::Sim(a) => sim(a, dict),
        Cmd::Bench(a) => bench(a, dict),
        Cmd::Tune(a) => tune(a, dict),
    }
}

fn load_params(path: &Option<std::path::PathBuf>) -> Result<Params> {
    match path {
        Some(p) => Ok(serde_json::from_str(
            &std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
        )?),
        None => Ok(Params::default()),
    }
}

fn run_live(a: RunArgs, dict: &'static Dict) -> Result<()> {
    let params_path = std::env::var("PARAMS_FILE")
        .ok()
        .map(std::path::PathBuf::from);
    let cfg = RunConfig {
        host: a.host.clone(),
        db: a.db.clone(),
        token: a.token.clone(),
        nonce: a.nonce.clone(),
        token_file: a.token_path(),
        data_dir: a.data_dir.clone(),
        tournament: a.tournament,
        auto_register: a.auto_register,
        join_lobby: !a.no_lobby,
        expected_name: Some(a.name.clone()),
        max_matches: a.max_matches,
        params: load_params(&params_path)?,
        safety_ms: a.safety_ms,
    };
    runner::run(cfg, dict)
}

fn engine_cfg(a: &SimArgs) -> EngineConfig {
    EngineConfig {
        settings: MatchSettings {
            auction_type: a.auction.into(),
            starting_rack_size: a.rack,
            ..MatchSettings::default()
        },
        late_bid_prob: a.late,
        ..EngineConfig::default()
    }
}

fn sim(a: SimArgs, dict: &'static Dict) -> Result<()> {
    let params = load_params(&a.params)?;
    let cfg = engine_cfg(&a);
    let scen = scenarios::build(a.scenario, dict, &params, &cfg);
    let mut outs = Vec::new();
    for s in &scen {
        let t = Instant::now();
        let out = run(dict, s, a.matches, a.seed);
        println!(
            "{}({:.1}s, seeds {}..{})\n",
            format_stats(&s.name, &seat_stats(&out)),
            t.elapsed().as_secs_f64(),
            a.seed,
            a.seed + a.matches as u64
        );
        outs.push(out);
    }
    if matches!(a.scenario, scrabble_bot::config::Scenario::Lobby) && outs.len() >= 2 {
        let (ds, dm) = paired_diff(&outs[0], "scrabbler", &outs[1], "hoard-flush");
        let base = summarize(
            &outs[1]
                .iter()
                .map(|o| o.seats[0].score as f64)
                .collect::<Vec<_>>(),
        );
        println!(
            "paired vs hoard-flush (same seeds): score {:+.1} ± {:.1} ({:+.1}%), margin {:+.1} ± {:.1}",
            ds.mean,
            ds.ci,
            100.0 * ds.mean / base.mean.max(1.0),
            dm.mean,
            dm.ci
        );
    }
    Ok(())
}

fn bench(a: BenchArgs, dict: &'static Dict) -> Result<()> {
    let mut solver = Solver::new(dict);
    let mut rng = ChaCha8Rng::seed_from_u64(1);
    println!(
        "solver: exact best partition, {} random racks per size drawn from the full bag",
        a.racks
    );
    for size in [7usize, 10, 15, 20, 25, 30, 35, 40] {
        let mut times = Vec::new();
        let mut exact = 0;
        for _ in 0..a.racks {
            let mut bag: Vec<u8> = Rack::full_bag().letters();
            let mut rack = Rack::EMPTY;
            for _ in 0..size {
                let i = rng.random_range(0..bag.len());
                rack.add(bag.swap_remove(i));
            }
            let t = Instant::now();
            let s = solver.solve(&rack);
            times.push(t.elapsed().as_secs_f64() * 1e3);
            exact += s.exact as usize;
        }
        times.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let q = |f: f64| times[((times.len() as f64 - 1.0) * f) as usize];
        println!(
            "  {size:2} tiles: p50 {:7.3} ms  p99 {:8.3} ms  max {:8.3} ms  exact {exact}/{}",
            q(0.5),
            q(0.99),
            q(1.0),
            a.racks
        );
    }
    Ok(())
}

fn tune(a: TuneArgs, dict: &'static Dict) -> Result<()> {
    let (name, values) = a.sweep.split_once('=').context("--sweep name=v1,v2,...")?;
    let cfg = EngineConfig::default();
    println!(
        "tuning {name} on {:?}, {} matches, seeds {}..",
        a.scenario, a.matches, a.seed
    );
    for v in values.split(',') {
        let mut json = serde_json::to_value(Params::default())?;
        let num: serde_json::Value =
            serde_json::from_str(v.trim()).with_context(|| format!("value {v}"))?;
        *json
            .get_mut(name)
            .with_context(|| format!("unknown param {name}"))? = num;
        let p: Params = serde_json::from_value(json)?;
        let scen = scenarios::build(a.scenario, dict, &p, &cfg);
        let t = Instant::now();
        let out = run(dict, &scen[0], a.matches, a.seed);
        let stats = seat_stats(&out);
        let mine: Vec<_> = stats
            .iter()
            .filter(|s| s.label.starts_with("scrabbler") && !s.label.contains("v1"))
            .collect();
        for s in mine {
            println!(
                "{name}={v:<8} {:<12} score {:6.1}±{:4.1} win {:5.1}% place {:.2} margin {:+6.1}±{:4.1} ({:.0}s)",
                s.label,
                s.score.mean,
                s.score.ci,
                100.0 * s.win.mean,
                s.place.mean,
                s.margin.mean,
                s.margin.ci,
                t.elapsed().as_secs_f64()
            );
        }
    }
    Ok(())
}
