//! A short run of the lobby scenario: the Scrabbler beats the sims and never
//! sends an invalid action.

use scrabble_bot::game::dict::Dict;
use scrabble_bot::game::scrabbler::{Params, Scrabbler};
use scrabble_bot::sim::engine::{EngineConfig, Seat};
use scrabble_bot::sim::harness::{Scenario, run, seat_stats};

#[test]
fn scrabbler_beats_sims() {
    let dict: &'static Dict = Dict::load_default().expect("wordlist").leak();
    let n: usize = std::env::var("SMOKE_MATCHES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let scen = Scenario {
        name: "smoke".into(),
        seats: vec![Box::new(move || {
            Seat::agent(
                "scrabbler",
                Box::new(Scrabbler::new(dict, Params::default(), 1)),
            )
        })],
        pad_sims: 5,
        cfg: EngineConfig::default(),
    };
    let out = run(dict, &scen, n, 500);
    let st = seat_stats(&out);
    let me = st.iter().find(|s| s.label == "scrabbler").unwrap();
    assert!(me.win.mean >= 0.95, "win rate {}", me.win.mean);
    assert!(me.score.mean > 300.0, "mean score {}", me.score.mean);
    assert_eq!(me.failed, 0, "invalid actions");
    for o in &out {
        assert_eq!(o.seats.iter().map(|s| s.failed_actions).sum::<u32>(), 0);
    }
}
