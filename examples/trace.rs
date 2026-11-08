use scrabble_bot::game::dict::Dict;
use scrabble_bot::game::scrabbler::{Params, Scrabbler};
use scrabble_bot::game::view::{Action, MatchView, Strategy};
use scrabble_bot::sim::agents::{HoardFlushAgent, StarterAgent};
use scrabble_bot::sim::engine::{EngineConfig, Seat, run_match};

struct Traced(Scrabbler);
impl Strategy for Traced {
    fn name(&self) -> String {
        self.0.name()
    }
    fn on_update(&mut self, v: &MatchView) -> Vec<Action> {
        let a = self.0.on_update(v);
        for x in &a {
            if let Action::PlayWord(w) = x {
                println!("    play {w}");
            }
        }
        if let Some(d) = self.0.last_decision.take() {
            let res = v
                .results
                .last()
                .map(|r| {
                    format!(
                        "prev {} won by {:?} paid {}",
                        (b'A' + r.letter) as char,
                        r.winner,
                        r.paid
                    )
                })
                .unwrap_or_default();
            println!(
                "#{:2} {} V={:5.1} lam={:5.2} bid={:3} bal={:3} rack={} buys={:4.1} | {res}",
                v.open.unwrap().index,
                d.letter,
                d.value,
                d.lambda,
                d.bid,
                d.balance,
                v.my_rack,
                d.expected_buys
            );
        }
        a
    }
}

fn main() -> anyhow::Result<()> {
    let dict: &'static Dict = Dict::load_default()?.leak();
    let league = std::env::args().any(|a| a == "--league");
    let mut seats = vec![Seat::agent(
        "scrabbler",
        Box::new(Traced(Scrabbler::new(dict, Params::default(), 1))),
    )];
    let first = std::env::args().any(|a| a == "--first");
    if std::env::args().any(|a| a == "--final-v1") {
        seats.push(Seat::agent(
            "scrabbler-v1",
            Box::new(Scrabbler::new(dict, Params::v1(), 2)),
        ));
    } else if std::env::args().any(|a| a == "--final") {
        seats.push(Seat::agent("hoard-1", Box::new(HoardFlushAgent::new(dict))));
    } else if league {
        seats.push(Seat::agent(
            "scrabbler-b",
            Box::new(Scrabbler::new(dict, Params::default(), 2)),
        ));
        seats.push(Seat::agent("hoard-1", Box::new(HoardFlushAgent::new(dict))));
        seats.push(Seat::agent("hoard-2", Box::new(HoardFlushAgent::new(dict))));
        seats.push(Seat::agent("starter-1", Box::new(StarterAgent::new(dict))));
        seats.push(Seat::agent("starter-2", Box::new(StarterAgent::new(dict))));
    } else {
        use scrabble_bot::game::rules::SimStrategy::*;
        for (n, s) in [
            ("Valor", ValueBidder),
            ("Brutus", Aggressive),
            ("Hagrid", ValueBidder),
            ("Maverick", Aggressive),
            ("Snippet", Cheapskate),
        ] {
            seats.push(Seat::sim(n, s));
        }
    }
    let mut cfg = EngineConfig::default();
    if first {
        cfg.settings.auction_type = scrabble_bot::game::rules::AuctionType::FirstPrice;
    }
    let out = run_match(dict, &mut seats, &cfg, 11);
    for s in &out.seats {
        println!(
            "{:12} score {:4} bal {:4} tiles {:3} spent {:4} words {:3} left {}",
            s.name, s.score, s.balance, s.tiles_won, s.spent, s.words, s.leftover_tiles
        );
    }
    Ok(())
}
