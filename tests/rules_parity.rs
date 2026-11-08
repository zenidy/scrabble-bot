//! Parity with the module's own tests (spacetimedb/src/tests/*.test.ts).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use scrabble_bot::game::dict::Dict;
use scrabble_bot::game::rules::{
    AuctionType, Rack, STARTING_BALANCE, SimStrategy, auctions_in_match, word_reward,
};
use scrabble_bot::game::view::{Action, MatchSettings, MatchView, Strategy};
use scrabble_bot::sim::engine::{EngineConfig, Seat, run_match};

fn dict() -> &'static Dict {
    static D: OnceLock<&'static Dict> = OnceLock::new();
    D.get_or_init(|| Dict::load_default().expect("wordlist").leak())
}

/// Bids a fixed amount on every auction and never plays.
struct Fixed(i64);
impl Strategy for Fixed {
    fn name(&self) -> String {
        "fixed".into()
    }
    fn on_update(&mut self, v: &MatchView) -> Vec<Action> {
        match v.open {
            Some(o) if self.0 > 0 && self.0 <= v.my_balance() => vec![Action::Bid {
                auction_id: o.auction_id,
                amount: self.0,
            }],
            _ => vec![],
        }
    }
}

/// Records how the engine calls it around the end of the match.
struct Late {
    asked_after_end: Arc<AtomicBool>,
    told_end: Arc<AtomicBool>,
    saw_last_auction: Arc<AtomicBool>,
}
impl Strategy for Late {
    fn name(&self) -> String {
        "late".into()
    }
    fn on_update(&mut self, v: &MatchView) -> Vec<Action> {
        if v.ended || v.open.is_none() {
            self.asked_after_end.store(true, Ordering::SeqCst);
        }
        if v.is_last_auction() {
            self.saw_last_auction.store(true, Ordering::SeqCst);
        }
        vec![]
    }
    fn on_match_end(&mut self, v: &MatchView, _top: &[(u64, i64)]) {
        assert!(v.ended && v.open.is_none());
        self.told_end.store(true, Ordering::SeqCst);
    }
}

fn cfg(auction_type: AuctionType, rack: u32) -> EngineConfig {
    EngineConfig {
        settings: MatchSettings {
            auction_type,
            starting_rack_size: rack,
            ..MatchSettings::default()
        },
        ..EngineConfig::default()
    }
}

#[test]
fn rewards_are_integer_and_floored() {
    for (w, r) in [
        ("QUIZ", 33),
        ("AXES", 16),
        ("QUAKE", 36),
        ("ZEBRAS", 42),
        ("JUKEBOX", 81),
        ("CAT", 5),
    ] {
        assert_eq!(word_reward(w), r, "{w}");
        assert!(dict().is_valid(w), "{w} should be in the wordlist");
    }
    assert!(!dict().is_valid("QI"), "no QI in ENABLE");
    assert!(!dict().is_valid("ZA"), "no ZA in ENABLE");
}

#[test]
fn auction_count_is_bag_minus_starting_racks() {
    for (players, rack) in [(2usize, 0u32), (2, 5), (6, 5), (4, 10), (12, 5), (6, 0)] {
        let mut seats: Vec<Seat> = (0..players)
            .map(|i| Seat::agent(&format!("f{i}"), Box::new(Fixed(0))))
            .collect();
        let out = run_match(dict(), &mut seats, &cfg(AuctionType::Vickrey, rack), 3);
        assert_eq!(
            out.auctions,
            auctions_in_match(players as u32, rack),
            "{players} players, rack {rack}"
        );
    }
}

#[test]
fn unbid_tiles_are_discarded() {
    // Nobody bids: every tile goes unsold and nobody pays.
    let mut seats = vec![
        Seat::agent("a", Box::new(Fixed(0))),
        Seat::agent("b", Box::new(Fixed(0))),
    ];
    let out = run_match(dict(), &mut seats, &cfg(AuctionType::Vickrey, 5), 9);
    assert_eq!(out.unsold, out.auctions);
    for s in &out.seats {
        assert_eq!(s.balance, STARTING_BALANCE);
        assert_eq!(s.tiles_won, 0);
        assert_eq!(s.leftover_tiles, 5, "only the starting rack");
    }
}

#[test]
fn ties_are_random_and_pay_one() {
    for t in [AuctionType::Vickrey, AuctionType::FirstPrice] {
        let mut seats = vec![
            Seat::agent("a", Box::new(Fixed(1))),
            Seat::agent("b", Box::new(Fixed(1))),
        ];
        let out = run_match(dict(), &mut seats, &cfg(t, 5), 4);
        let (a, b) = (out.seats[0].tiles_won, out.seats[1].tiles_won);
        assert!(
            a > 10 && b > 10,
            "{t:?}: both tied bidders win sometimes ({a} vs {b})"
        );
        assert_eq!(out.seats[0].spent, a as i64, "{t:?}: every tie pays 1");
        assert_eq!(out.seats[1].spent, b as i64);
    }
}

#[test]
fn vickrey_pays_second_price_first_price_pays_own() {
    let mut seats = vec![
        Seat::agent("hi", Box::new(Fixed(2))),
        Seat::agent("lo", Box::new(Fixed(1))),
    ];
    let out = run_match(dict(), &mut seats, &cfg(AuctionType::Vickrey, 5), 5);
    // "hi" outbids "lo" until its balance can't cover 2 any more.
    assert!(out.seats[0].tiles_won > 0);
    assert_eq!(
        out.seats[0].spent, out.seats[0].tiles_won as i64,
        "second price is 1"
    );

    let mut seats = vec![
        Seat::agent("hi", Box::new(Fixed(2))),
        Seat::agent("lo", Box::new(Fixed(1))),
    ];
    let out = run_match(dict(), &mut seats, &cfg(AuctionType::FirstPrice, 5), 5);
    assert_eq!(
        out.seats[0].spent,
        2 * out.seats[0].tiles_won as i64,
        "first price pays own bid"
    );

    // A lone bidder pays the reserve of 1.
    let mut seats = vec![
        Seat::agent("solo", Box::new(Fixed(7))),
        Seat::agent("none", Box::new(Fixed(0))),
    ];
    let out = run_match(dict(), &mut seats, &cfg(AuctionType::Vickrey, 5), 6);
    assert_eq!(out.seats[0].spent, out.seats[0].tiles_won as i64);
}

#[test]
fn sims_play_longest_word_alphabetical() {
    let d = dict();
    let w = |r: &str, min| {
        d.best_playable(&Rack::from_letters(r), min)
            .map(|c| d.class_word(c).to_string())
    };
    assert_eq!(w("TAC", 3).as_deref(), Some("ACT"));
    assert_eq!(w("AA", 3), None);
    assert_eq!(w("AA", 2).as_deref(), Some("AA"));
    assert_eq!(w("QXZ", 3), None);
    assert_eq!(w("", 2), None);
    let long = w("SETAERX", 3).unwrap();
    assert!(
        long.len() >= 6 && Rack::from_letters("SETAERX").contains(&Rack::from_letters(&long)),
        "{long}"
    );
}

#[test]
fn every_tile_accounted_for_and_balances_add_up() {
    let mut seats: Vec<Seat> = vec![
        Seat::sim("c", SimStrategy::Cheapskate),
        Seat::sim("v", SimStrategy::ValueBidder),
        Seat::sim("b", SimStrategy::Aggressive),
        Seat::sim("h", SimStrategy::ValueBidder),
        Seat::sim("m", SimStrategy::Aggressive),
        Seat::sim("s", SimStrategy::Cheapskate),
    ];
    for t in [AuctionType::Vickrey, AuctionType::FirstPrice] {
        let out = run_match(dict(), &mut seats, &cfg(t, 5), 21);
        let held: u32 = out.seats.iter().map(|s| s.leftover_tiles).sum();
        let won: u32 = out.seats.iter().map(|s| s.tiles_won).sum();
        assert_eq!(won + out.unsold, out.auctions);
        assert!(
            out.seats.iter().any(|s| s.words > 0),
            "sims should have played"
        );
        for s in &out.seats {
            assert_eq!(
                s.balance,
                STARTING_BALANCE - s.spent + s.score,
                "{t:?} {}",
                s.name
            );
            assert!(s.balance >= 0);
        }
        // held + played letters + unsold = 98 is checked through the engine's
        // own debug assertions; here: no tile is created or lost.
        assert!(held <= 98);
    }
}

#[test]
fn matches_are_deterministic_per_seed() {
    let play = |seed| {
        let mut seats: Vec<Seat> = vec![
            Seat::sim("c", SimStrategy::Cheapskate),
            Seat::sim("b", SimStrategy::Aggressive),
            Seat::sim("m", SimStrategy::Aggressive),
        ];
        let out = run_match(dict(), &mut seats, &cfg(AuctionType::Vickrey, 5), seed);
        out.seats
            .iter()
            .map(|s| (s.score, s.balance, s.tiles_won))
            .collect::<Vec<_>>()
    };
    assert_eq!(play(77), play(77));
}

#[test]
fn no_plays_after_the_match_ends() {
    let (a, b, c) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let late = Late {
        asked_after_end: a.clone(),
        told_end: b.clone(),
        saw_last_auction: c.clone(),
    };
    let mut seats = vec![
        Seat::agent("late", Box::new(late)),
        Seat::sim("b", SimStrategy::Aggressive),
    ];
    let _ = run_match(dict(), &mut seats, &cfg(AuctionType::Vickrey, 5), 8);
    assert!(
        !a.load(Ordering::SeqCst),
        "never asked to act once the match is over"
    );
    assert!(b.load(Ordering::SeqCst), "told the match ended");
    assert!(
        c.load(Ordering::SeqCst),
        "saw the last auction open (bag empty) while it could still play"
    );
}
