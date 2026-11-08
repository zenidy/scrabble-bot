//! Solver properties against brute force on small racks.

use std::sync::OnceLock;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use scrabble_bot::game::dict::Dict;
use scrabble_bot::game::rules::Rack;
use scrabble_bot::game::solver::{Solver, brute_force};

fn dict() -> &'static Dict {
    static D: OnceLock<&'static Dict> = OnceLock::new();
    D.get_or_init(|| Dict::load_default().expect("wordlist").leak())
}

fn random_rack(rng: &mut ChaCha8Rng, size: usize) -> Rack {
    let mut bag = Rack::full_bag().letters();
    let mut r = Rack::EMPTY;
    for _ in 0..size {
        let i = rng.random_range(0..bag.len());
        r.add(bag.swap_remove(i));
    }
    r
}

#[test]
fn matches_brute_force_on_small_racks() {
    let d = dict();
    let mut s = Solver::new(d);
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    for i in 0..400 {
        let size = 2 + i % 8; // 2..=9 tiles
        let rack = random_rack(&mut rng, size);
        let sol = s.solve(&rack);
        assert_eq!(sol.value, brute_force(d, &rack), "rack {rack}");
        assert!(sol.exact);
        // The words are real, fit together in the rack and add up.
        assert!(
            rack.contains(&sol.used(d)),
            "rack {rack} words {:?}",
            sol.words(d)
        );
        let total: u32 = sol
            .words(d)
            .iter()
            .map(|w| scrabble_bot::game::rules::word_reward(w))
            .sum();
        assert_eq!(total, sol.value);
        for w in sol.words(d) {
            assert!(d.is_valid(w), "{w}");
        }
    }
}

#[test]
fn cached_and_paired_values_agree_with_fresh_solves() {
    let d = dict();
    let mut fresh = Solver::new(d);
    let mut cached = Solver::new(d);
    let mut rng = ChaCha8Rng::seed_from_u64(7);
    for _ in 0..150 {
        let size = rng.random_range(5..22);
        let rack = random_rack(&mut rng, size);
        let l = rng.random_range(0..26u8);
        let mut plus = rack;
        plus.add(l);
        let (a, b) = cached.pair_values(&rack, l);
        assert_eq!(a, fresh.value(&rack), "rack {rack}");
        assert_eq!(b, fresh.value(&plus), "rack {plus}");
        assert_eq!(cached.value_cached(&rack), a);
    }
}

#[test]
fn node_cap_still_returns_a_valid_partition() {
    let d = dict();
    let mut s = Solver::new(d);
    s.set_limits(Some(200), None);
    let mut rng = ChaCha8Rng::seed_from_u64(9);
    for _ in 0..30 {
        let rack = random_rack(&mut rng, 30);
        let sol = s.solve(&rack);
        assert!(rack.contains(&sol.used(d)), "rack {rack}");
        let total: u32 = sol
            .words(d)
            .iter()
            .map(|w| scrabble_bot::game::rules::word_reward(w))
            .sum();
        assert_eq!(total, sol.value);
    }
}
