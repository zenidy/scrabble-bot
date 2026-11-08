use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use scrabble_bot::game::{dict::Dict, rules::Rack, solver::Solver};

fn main() -> anyhow::Result<()> {
    let dict: &'static Dict = Dict::load_default()?.leak();
    let mut rng = ChaCha8Rng::seed_from_u64(3);
    for cap in [30_000u64, 200_000] {
        for size in [15usize, 20, 25] {
            let mut capped = Solver::new(dict);
            capped.set_limits(Some(cap), None);
            let mut exact = Solver::new(dict);
            let (mut big, mut n, mut err) = (0, 0, 0.0);
            let t = std::time::Instant::now();
            for _ in 0..300 {
                let mut bag: Vec<u8> = Rack::full_bag().letters();
                let mut rack = Rack::EMPTY;
                for _ in 0..size { let i = rng.random_range(0..bag.len()); rack.add(bag.swap_remove(i)); }
                let l = bag[rng.random_range(0..bag.len())];
                let (a, b) = capped.pair_values(&rack, l);
                let mut r2 = rack; r2.add(l);
                let (ea, eb) = (exact.value(&rack), exact.value(&r2));
                let d = ((b as f64 - a as f64) - (eb as f64 - ea as f64)).abs();
                err += d; n += 1; if d >= 5.0 { big += 1; }
            }
            println!("cap {cap} size {size}: mean |err| {:.2}, errors>=5: {big}/{n}  ({:?})", err / n as f64, t.elapsed());
        }
    }
    Ok(())
}
