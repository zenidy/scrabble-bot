//! `cargo bench --bench solver`: solver latency by rack size.
//! Targets: p99 < 5 ms at 20 tiles, < 50 ms at 30.

fn main() {
    let dict = scrabble_bot::game::dict::Dict::load_default().expect("wordlist");
    let racks = std::env::var("BENCH_RACKS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    for r in scrabble_bot::game::solver::benchmark(&dict, racks, 1) {
        println!(
            "{:2} tiles: p50 {:7.3} ms  p99 {:8.3} ms  max {:8.3} ms  exact {}/{}",
            r.tiles, r.p50_ms, r.p99_ms, r.max_ms, r.exact, r.racks
        );
    }
}
