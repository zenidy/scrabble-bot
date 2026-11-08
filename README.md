# The Scrabbler

A Rust bot for [ScrabbleBot](https://scrabblebot.spacetimedb.com), the bot-vs-bot word auction game
on SpacetimeDB maincloud. Bots bid for letters in sealed-bid auctions, then spend them on dictionary
words; a word's reward is paid as both score and money, so money can be recycled into more letters.

Built against the event module at commit `b5ccb36` (`EVENT_REPO_COMMIT`), SpacetimeDB SDK and CLI
2.14.1, Rust 1.99 (edition 2024).

## Results so far

All offline numbers come from the simulator in `src/sim`, a seeded port of the module's match rules,
with 95% confidence intervals, for the configuration in this commit (`Params::default()`). "Same
seeds" means the same tile order, so comparisons are paired. "v1" is the first version that beat
the baselines (`Params::v1()`), kept as the past version to beat.

| Scenario (matches) | The Scrabbler | Others |
|---|---|---|
| Lobby: 1 bot + 5 random sims, Vickrey (400) | **399.2 ± 2.7**, wins 100% | hoard-flush 317.7 ± 4.3; paired on the same seeds **+81.5 ± 4.0 (+25.6%)**; starter 25.7 |
| 6-bot league: 2 Scrabblers, 2 hoard-flush, 2 starters, Vickrey (200) | **156.8 / 159.7**, every win | hoard-flush 54.6 / 55.6, starter 9.1 / 9.3 |
| All versions + baselines in one 6-bot match, Vickrey (200) | **128.7 / 131.4**, 97% of wins | v1 76.6 (4%), hoard-flush 48-50, starter 9.4 |
| All versions + baselines in one 6-bot match, first price (200) | **122.1 / 121.6**, 98.5% of wins | v1 79.7 (2%), hoard-flush 61-63, starter 9.4 |
| 4-bot Swiss table, Vickrey (200) | **254.2 ± 6.9**, wins 94.5%, place 1.07 | v1 117.2, hoard-flush 101.2, starter 9.4 |
| 4-bot Swiss table, first price (200) | **227.6 ± 4.5**, wins 94.5%, place 1.06 | v1 118.7, hoard-flush 118.6, starter 10.5 |
| 8-bot elimination match, Vickrey (200) | **147.3 ± 4.7**, wins 97%, P(last) 0% | v1 62.7 / 63.3, hoard-flush 39-40, starter 9-10 |
| 8-bot elimination match, first price (200) | **108.5 ± 3.1**, wins 75%, P(last) 0% | v1 76.3 / 76.4, hoard-flush 45-47, starter 9-10 |
| 2-bot final game vs hoard-flush, Vickrey (200) | **425.3 ± 5.7**, wins 100% | 92.8 |
| 2-bot final game vs v1, Vickrey (200) | **295.3 ± 10.1**, wins 76.5%, margin +82.6 ± 18.6 | 212.7 |
| 2-bot final game vs hoard-flush, first price (200) | **380.6 ± 4.3**, wins 100% | 130.6 |
| 2-bot final game vs v1, first price (200) | **330.8 ± 5.9**, wins 92%, margin +125.2 ± 12.0 | 205.7 |

Each final row is a single game; a real final sums three, which only widens these margins. In an
all-strong lobby (`--scenario strong6`: three Scrabblers with the lambda cap vs three without, 200
matches) the cap is neutral: 77.5-78.3 vs 77.4-78.5 points, same win and last-place rates. Where
two Scrabblers play in one match they split the wins between them.

How the heads-up rows got there (same seeds): before the denial term, first-price income model and
the lambda cap, the first-price final was lost to both hoard-flush (16% wins) and v1 (17%). Capping
lambda at 1.2 points per coin was the decisive change: above that the pacing settles on a
pessimistic fixed point ("I won't win much, so I won't earn much, so I can't afford to bid").

Simulator calibration against the brief's independent numbers (sims only: Aggressive 75.9 vs ~76,
ValueBidder 13.6 vs ~14, Cheapskate 4.2 vs ~4; TS starter 25.6 vs ~27; hoard-flush 318.5 and 100%
wins vs ~300).

Local end-to-end (local `spacetime start`, the event module published as `scrabblebot-local`, a team
and bot created with `create_team` / `mint_credential_nonce`, bot started with `BOT_NONCE`):

| Test | Result |
|---|---|
| 1 bot + 5 sims, 4 matches | 366, 413, 347, 401: 1st every time, margins +308 to +395 |
| Two copies on two teams + 4 sims | 164 and 149, places 1 and 2 (best sim 29) |
| `kill -9` mid-match, restart from the saved token | resumed the same match within 2 s, won it |
| 3 s network cut mid-match (TCP proxy killed) | reconnected with backoff in 4 s, won with 401 |
| Errors over all local matches | 0 late bids, 0 skipped bids, 0 failed words, 0 unexpected reducer errors |

Live on maincloud: **not started yet**. It needs the bot's token (see "Going live").

Solver (`cargo bench --bench solver`, exact answers, random racks from the full bag, Apple M-series):
p99 4.8 ms at 20 tiles (target 5), 36 ms at 30 tiles (target 50); rare outliers are cut off by the
anytime time cap.

## How it plays

- **Rack solver** (`game/solver.rs`). Maximizing reward is minimizing loss (`3*base - reward` per
  word plus `3*value` per unused tile, since no word pays more than 3x). Exact-cover style search:
  keep the words that still fit and are under the loss budget, branch on the letter with the fewest
  such words (spend it in one of them, or leave every copy unused), memoize per sub-rack, deepen the
  budget at the root. A node or time budget makes it anytime.
- **Unseen pool** (`game/pool.rs`). Full bag minus my starting tiles, every auctioned tile and
  opponents' starting tiles revealed by their plays (running max of played minus won). Future tiles
  are a uniform sample of it; everything is rebuilt from tables after a reconnect.
- **Price model** (`game/price.rs`). Sims are exact: each bids `clamp(base + j, 1, balance)`, so the
  top sim bid has a closed-form distribution from their live balances. Real bots get a bid histogram
  over `letter value + offset`, learned from the post-match archive of winning bids and kept in
  `data/opponents.json`, starting from a cautious prior.
- **Tile value** (`game/value.rs`). `V(L) = mean_k [F(R + X_k + L) - F(R + X_k)]` with `F` the best
  partition value, `R` my rack, `X_k` sampled future wins (weighted by how likely I am to buy each
  letter). Both solves share one word enumeration. The estimate is shrunk toward a prior
  (`0.8 * 3 * value`) to damp the winner's curse.
- **Value of money, lambda** (`game/scrabbler.rs`). Points per coin for the rest of the match, set so
  expected spend over the remaining auctions (from the price model) matches the money I'll have:
  balance, bankable 7+ letter words, and expected word income (a flat 50% of bought value in
  Vickrey; in first price 85%, fading over the last 12 auctions). It falls to its floor as the
  match runs out, because leftover coins are worth nothing, and is capped at 1.2.
- **Denial.** Head to head (two bots, or a tournament final) every tile I lose goes to my only
  rival and the margin is what counts, so the bid value adds the tile's prior value to the rival.
  In first price, real bots' bids are also learned during the match from `paid` (there it is the
  winner's own bid).
- **Bids.** Vickrey: `V / lambda`, truthful in coins. First price: the `b` that maximizes
  `P(win | b) * (V / lambda - b)`. The last auction's tile arrives in the transaction that ends the
  match, so no bot can play it; the Scrabbler flushes its words, then bids its whole balance on that
  tile (free: coins are worthless after the match, and it denies a sim a closing word).
- **Words.** When coins are scarce (lambda >= 0.3), or the balance is low, or the rack is huge, bank
  the best 7+ letter word of the current best partition (already at the 3x cap). If broke with
  auctions to go, bank the best 5+ letter word. When the last auction opens, play the best
  partition of the whole rack, all calls at once. Against sims money is plentiful and it hoards
  (that alone is worth ~27 points a match vs banking every long word at once).

## Commands

```sh
cargo build --release
./target/release/scrabbler sim --scenario lobby --matches 500        # vs 5 sims, paired vs hoard-flush
./target/release/scrabbler sim --scenario versions --auction first   # all versions + baselines
./target/release/scrabbler sim --scenario table4|elim8|final2|league --matches 300 --rack 5
./target/release/scrabbler tune --scenario league --sweep play_long_lambda=0.2,0.3,0.5
./target/release/scrabbler bench
./target/release/scrabbler run [--tournament ID | --auto-register] [--max-matches N]
cargo run --release --example trace -- --league                      # one match, every decision
```

`sim` options: `--auction vickrey|first`, `--rack N` (starting rack size), `--late P` (chance a bid
misses the window), `--params file.json` (Scrabbler params; see `Params` in `game/scrabbler.rs`).

## Going live

1. On the website, create a team with bot name **The Scrabbler** and click "Generate token" on the
   Team page (shown once). Or mint a nonce: `spacetime call scrabblebot mint_credential_nonce`, then
   `spacetime sql scrabblebot "SELECT code FROM my_nonces"`.
2. `cp .env.example .env` and set `BOT_TOKEN` (or `BOT_NONCE`).
3. `set -a; . ./.env; set +a; ./target/release/scrabbler run`. The token is then kept in
   `secrets/scrabblebot.token` (mode 600, gitignored), so later runs need no env.

The bot resolves its bot id from `bot_credential` and warns if the name isn't "The Scrabbler". It
joins the lobby whenever it isn't in a running match, re-joins after each match, and with
`--tournament ID` or `--auto-register` registers itself (only the bot's own credential can).

**Stopping it:** the module has no "leave lobby" reducer, and a queued bot gets seated even if it's
offline (and then loses every auction). Restarting is fine (it resumes within seconds), but to stop
for longer, use `--max-matches N` (it won't re-queue after its last match) or stop it right after a
match ends.

## Deploy

- Docker: `docker compose -f deploy/docker-compose.yml up -d --build` (restart always; state, token
  and logs in `./state`). CI builds the image.
- systemd: `deploy/scrabbler.service` runs the release binary with `Restart=always`.
- Env: `BOT_TOKEN` or `BOT_NONCE` (first run), `STDB_HOST` (default
  `https://maincloud.spacetimedb.com`), `STDB_DB` (default `scrabblebot`), `RUST_LOG`, `DATA_DIR`,
  `TOKEN_FILE`, `WORDLIST`.
- Pick an always-on VPS in the region with the lowest round trip to maincloud.spacetimedb.com. The
  bot logs the reducer round trip (`rtt_ms`) per bid. Decisions are capped at ~150 ms live.

## Logs

`tracing` to stdout plus `data/matches/match-<id>.jsonl`: every bid (letter, value, lambda, bid,
balance, time since the auction appeared, think time, rtt), every result, every word, refusals, and
the final scores. The summary line per match counts late bids, skipped bids and failed words
(target: zero).

## Layout

```
src/
  main.rs config.rs persist.rs lib.rs
  game/   rules.rs dict.rs solver.rs view.rs pool.rs price.rs value.rs scrabbler.rs
  net/    client.rs subs.rs events.rs runner.rs module_bindings/ (generated)
  sim/    engine.rs agents.rs harness.rs scenarios.rs
benches/solver.rs
tests/  rules_parity.rs solver_props.rs sim_smoke.rs
examples/trace.rs
scripts/gen-bindings.sh  deploy/  data/wordlist.txt  EVENT_REPO_COMMIT
```

## Decisions (and where this differs from the brief)

- Repo `zenidy/scrabble-bot` (private), crate `scrabble-bot`, binary `scrabbler`.
- Strategy, bid and play logic live in one module, `game/scrabbler.rs`, rather than separate
  `bid.rs` / `play.rs` / `phase.rs` files; there was no second user of those pieces.
- Per-match subscriptions instead of the join trick: a join on `match_participant` keeps every past
  match's auctions in the cache forever. The first auction costs one round trip; locally the first
  bid still went out within ~110 ms.
- The SDK thread reads one consistent snapshot of the cache after every server message (the SDK
  applies a whole transaction before callbacks run), so the strategy always sees a transaction as a
  whole: e.g. the second-to-last tile arrives in the same transaction that opens the last auction,
  and the flush includes it.
- The last auction: the brief says bid 0. Since nobody can play that tile but a sim, and coins are
  worthless afterwards, the bot bids its whole balance instead (free denial against sims).
- The simulator runs decisions without a time budget (deterministic); live decisions are capped at
  150 ms (`think_ms`), with mid-match partition solves capped at 60 ms.
- In the simulator each match builds fresh agents, so cross-match opponent learning is only
  exercised live.

## Fair play

Only the bot's own credential connection reads match data; it never subscribes to
`visible_auction_top_bids` or to whole match tables, and only calls reducers in response to game
events (one bid per auction plus at most a re-bid, words, `join_lobby` at most every 2 s).

## Regenerating bindings

`scripts/gen-bindings.sh` checks out the pinned event commit, installs the module's npm deps and runs
`spacetime generate --lang rust` (the CLI version must equal the `spacetimedb-sdk` version).

## Milestones

- [x] M2 Engine: dictionary, solver (benchmarks met), simulator with parity tests, baselines measured.
- [x] M3 Strategy: vs 5 sims 100% wins and +25.6% over hoard-flush on the same seeds; wins the
  6-bot league against both baselines by a wide margin.
- [x] M4 Competition (offline): the current version wins the all-versions league, 4-bot tables,
  8-bot elimination (never last) and 2-bot finals, in both auction types (table above). Not done:
  Swiss / bracket-specific play beyond the head-to-head denial, and cross-match opponent learning
  is exercised live only (the simulator builds fresh agents each match).
- [~] M1 Plumbing: every local test passes (0 late bids, 0 failed words); the 10 live lobby
  matches on maincloud need the bot's token.
- [~] M5 Hardening: kill/restart and network-cut recovery pass locally; the Docker image builds and
  runs in CI; the 24 h soak and the VPS deploy are pending (deploy target not chosen yet).

## License

MIT
