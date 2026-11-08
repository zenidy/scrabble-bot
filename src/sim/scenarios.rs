//! Named offline scenarios for `scrabbler sim` and `scrabbler tune`.

use crate::config::Scenario as Which;
use crate::game::dict::Dict;
use crate::game::scrabbler::{Params, Scrabbler};
use crate::sim::agents::{HoardFlushAgent, StarterAgent};
use crate::sim::engine::{EngineConfig, Seat};
use crate::sim::harness::{Scenario, SeatFactory};

fn scr(dict: &'static Dict, label: &'static str, p: Params, seed: u64) -> SeatFactory {
    Box::new(move || {
        Seat::agent(
            label,
            Box::new(Scrabbler::new(dict, p.clone(), seed).with_label(label)),
        )
    })
}
fn hoard(dict: &'static Dict, label: &'static str) -> SeatFactory {
    Box::new(move || Seat::agent(label, Box::new(HoardFlushAgent::new(dict))))
}
fn starter(dict: &'static Dict, label: &'static str) -> SeatFactory {
    Box::new(move || Seat::agent(label, Box::new(StarterAgent::new(dict))))
}

/// The scenarios to run for `which`. Paired comparisons share seeds.
pub fn build(which: Which, dict: &'static Dict, p: &Params, cfg: &EngineConfig) -> Vec<Scenario> {
    let mk = |name: &str, seats: Vec<SeatFactory>, pad: usize| Scenario {
        name: name.into(),
        seats,
        pad_sims: pad,
        cfg: cfg.clone(),
    };
    match which {
        Which::Lobby => vec![
            mk(
                "scrabbler + 5 sims",
                vec![scr(dict, "scrabbler", p.clone(), 7)],
                5,
            ),
            mk("hoard-flush + 5 sims", vec![hoard(dict, "hoard-flush")], 5),
            mk("starter + 5 sims", vec![starter(dict, "starter")], 5),
        ],
        Which::League => vec![mk(
            "6-bot league",
            vec![
                scr(dict, "scrabbler-a", p.clone(), 1),
                scr(dict, "scrabbler-b", p.clone(), 2),
                hoard(dict, "hoard-1"),
                hoard(dict, "hoard-2"),
                starter(dict, "starter-1"),
                starter(dict, "starter-2"),
            ],
            0,
        )],
        Which::Versions => vec![mk(
            "versions + baselines",
            vec![
                scr(dict, "scrabbler", p.clone(), 1),
                scr(dict, "scrabbler-v1", Params::v1(), 2),
                hoard(dict, "hoard-1"),
                hoard(dict, "hoard-2"),
                starter(dict, "starter"),
                scr(dict, "scrabbler-2", p.clone(), 3),
            ],
            0,
        )],
        Which::Table4 => vec![mk(
            "4-bot table",
            vec![
                scr(dict, "scrabbler", p.clone(), 1),
                scr(dict, "scrabbler-v1", Params::v1(), 2),
                hoard(dict, "hoard-flush"),
                starter(dict, "starter"),
            ],
            0,
        )],
        Which::Elim8 => vec![mk(
            "8-bot elimination",
            vec![
                scr(dict, "scrabbler", p.clone(), 1),
                scr(dict, "scrabbler-v1", Params::v1(), 2),
                scr(dict, "scrabbler-v1b", Params::v1(), 3),
                hoard(dict, "hoard-1"),
                hoard(dict, "hoard-2"),
                hoard(dict, "hoard-3"),
                starter(dict, "starter-1"),
                starter(dict, "starter-2"),
            ],
            0,
        )],
        Which::Strong6 => {
            let uncapped = Params {
                lambda_max: 20.0,
                ..p.clone()
            };
            vec![mk(
                "all-strong lobby",
                vec![
                    scr(dict, "scrabbler-cap-1", p.clone(), 1),
                    scr(dict, "scrabbler-cap-2", p.clone(), 2),
                    scr(dict, "scrabbler-cap-3", p.clone(), 3),
                    scr(dict, "uncapped-1", uncapped.clone(), 4),
                    scr(dict, "uncapped-2", uncapped.clone(), 5),
                    scr(dict, "uncapped-3", uncapped, 6),
                ],
                0,
            )]
        }
        Which::Final2 => vec![
            mk(
                "final vs hoard-flush",
                vec![
                    scr(dict, "scrabbler", p.clone(), 1),
                    hoard(dict, "hoard-flush"),
                ],
                0,
            ),
            mk(
                "final vs v1",
                vec![
                    scr(dict, "scrabbler", p.clone(), 1),
                    scr(dict, "scrabbler-v1", Params::v1(), 2),
                ],
                0,
            ),
        ],
    }
}
