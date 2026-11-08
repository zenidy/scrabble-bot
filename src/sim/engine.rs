//! Offline match engine: a port of the module's match rules (`startMatchWith`,
//! `drawLetter`, `closeAuction`, `simulateBids`, `simulateWordPlay`,
//! `playWord`). Seeded and deterministic.
//!
//! Random streams are split (bag order, sim jitter, tie breaks, latency) so
//! two strategies run on the same seed see the same tiles in the same order:
//! common random numbers for paired comparisons. Pre-shuffling the bag and
//! drawing in order has the same distribution as the module's uniform draw
//! from what's left.

use rand::{Rng, RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::game::dict::Dict;
use crate::game::rules::{AUCTION_RESERVE, AuctionType, NUM_LETTERS, Rack, SIM_BID_JITTER, STARTING_BALANCE, SimStrategy, BAG};
use crate::game::view::{Action, AuctionRecord, MatchSettings, MatchView, OpenAuction, Phase, PlayRecord, PlayerInfo, Strategy};

pub enum SeatKind {
    Sim(SimStrategy),
    Agent(Box<dyn Strategy>),
}

pub struct Seat {
    pub name: String,
    pub kind: SeatKind,
}

impl Seat {
    pub fn sim(name: &str, s: SimStrategy) -> Seat {
        Seat { name: name.to_string(), kind: SeatKind::Sim(s) }
    }
    pub fn agent(name: &str, s: Box<dyn Strategy>) -> Seat {
        Seat { name: name.to_string(), kind: SeatKind::Agent(s) }
    }
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub settings: MatchSettings,
    pub phase: Phase,
    /// Chance that an agent's bid lands after the window closes.
    pub late_bid_prob: f64,
    /// How many times an agent is re-asked after its own words land.
    pub max_rounds: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig { settings: MatchSettings::default(), phase: Phase::Lobby, late_bid_prob: 0.0, max_rounds: 4 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SeatResult {
    pub name: String,
    pub score: i64,
    pub balance: i64,
    pub tiles_won: u32,
    pub spent: i64,
    pub words: u32,
    pub late_bids: u32,
    pub failed_actions: u32,
    pub leftover_tiles: u32,
}

#[derive(Clone, Debug, Default)]
pub struct MatchOutcome {
    pub seats: Vec<SeatResult>,
    pub auctions: u32,
    pub unsold: u32,
}

impl MatchOutcome {
    /// 1-based place: 1 + number of seats with a strictly higher score.
    pub fn place(&self, seat: usize) -> usize {
        let s = self.seats[seat].score;
        1 + self.seats.iter().filter(|r| r.score > s).count()
    }

    /// Everyone tied on the top score shares the win (as the module rates it).
    pub fn won(&self, seat: usize) -> bool {
        self.place(seat) == 1
    }

    /// Strictly or jointly lowest score.
    pub fn last(&self, seat: usize) -> bool {
        let s = self.seats[seat].score;
        self.seats.iter().all(|r| r.score >= s)
    }

    /// Score minus the best other score.
    pub fn margin(&self, seat: usize) -> i64 {
        let best_other = self
            .seats
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != seat)
            .map(|(_, r)| r.score)
            .max()
            .unwrap_or(0);
        self.seats[seat].score - best_other
    }
}

fn stream(seed: u64, id: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ id.wrapping_mul(0xD1B5_4A32_D192_ED03))
}

/// Fisher-Yates as `shuffle` in game.ts.
fn shuffle<T>(rng: &mut impl Rng, items: &mut [T]) {
    for i in (1..items.len()).rev() {
        let j = rng.random_range(0..=i);
        items.swap(i, j);
    }
}

struct State {
    players: Vec<PlayerInfo>,
    racks: Vec<Rack>,
    results: Vec<AuctionRecord>,
    plays: Vec<PlayRecord>,
    top_bids: Vec<(u64, i64)>,
    stats: Vec<SeatResult>,
}

pub fn run_match(dict: &Dict, seats: &mut [Seat], cfg: &EngineConfig, seed: u64) -> MatchOutcome {
    let n = seats.len();
    let mut bag_rng = stream(seed, 1);
    let mut jitter_rng = stream(seed, 2);
    let mut tie_rng = stream(seed, 3);
    let mut late_rng = stream(seed, 4);
    let mut order_rng = stream(seed, 5);

    let mut bag: Vec<u8> = Vec::with_capacity(98);
    for (l, &c) in BAG.iter().enumerate().take(NUM_LETTERS) {
        for _ in 0..c {
            bag.push(l as u8);
        }
    }
    shuffle(&mut bag_rng, &mut bag);
    let mut next_tile = 0usize;
    let draw = |next_tile: &mut usize| {
        if *next_tile < bag.len() {
            *next_tile += 1;
            Some(bag[*next_tile - 1])
        } else {
            None
        }
    };

    let match_id = seed;
    let bot_id = |i: usize| 100 + i as u64;
    let mut st = State {
        players: (0..n)
            .map(|i| PlayerInfo {
                bot_id: bot_id(i),
                balance: STARTING_BALANCE,
                score: 0,
                sim: match seats[i].kind {
                    SeatKind::Sim(s) => Some(s),
                    SeatKind::Agent(_) => None,
                },
            })
            .collect(),
        racks: vec![Rack::EMPTY; n],
        results: Vec::new(),
        plays: Vec::new(),
        top_bids: Vec::new(),
        stats: seats.iter().map(|s| SeatResult { name: s.name.clone(), ..Default::default() }).collect(),
    };

    // Starting racks, dealt in roster order.
    'deal: for i in 0..n {
        for _ in 0..cfg.settings.starting_rack_size {
            match draw(&mut next_tile) {
                Some(l) => st.racks[i].add(l),
                None => break 'deal,
            }
        }
    }
    let total_auctions = (98 - next_tile) as u32;

    let duration = cfg.settings.auction_duration_ms as i64;
    let mut letter = draw(&mut next_tile);
    let mut index = 0u32;
    let mut auction_id = match_id * 1000;
    let mut agent_order: Vec<usize> = (0..n).filter(|&i| matches!(seats[i].kind, SeatKind::Agent(_))).collect();

    while let Some(l) = letter {
        auction_id += 1;
        let bag_total = total_auctions - index - 1;
        let open = OpenAuction { auction_id, letter: l, index, closes_at_ms: (index as i64 + 1) * duration };
        let mut bids = vec![0i64; n];

        // simulateBids: every sim draws jitter; balance 0 means no bid.
        for i in 0..n {
            if let SeatKind::Sim(s) = seats[i].kind {
                let j = jitter_rng.random_range(-SIM_BID_JITTER..=SIM_BID_JITTER);
                let amount = s.bid(l, st.players[i].balance, j);
                if amount > 0 {
                    bids[i] = amount;
                }
            }
        }

        // Agents act, in a random order each auction.
        shuffle(&mut order_rng, &mut agent_order);
        for &i in &agent_order {
            for _round in 0..cfg.max_rounds.max(1) {
                let view = MatchView {
                    match_id,
                    my_bot_id: bot_id(i),
                    settings: cfg.settings,
                    phase: cfg.phase,
                    players: &st.players,
                    my_rack: st.racks[i],
                    bag_total,
                    open: Some(open),
                    results: &st.results,
                    plays: &st.plays,
                    ended: false,
                    now_ms: index as i64 * duration,
                };
                let SeatKind::Agent(agent) = &mut seats[i].kind else { unreachable!() };
                let actions = agent.on_update(&view);
                let mut played = false;
                for a in actions {
                    match a {
                        Action::PlayWord(w) => {
                            if play_word(dict, &mut st, i, &w) {
                                played = true;
                            } else {
                                st.stats[i].failed_actions += 1;
                            }
                        }
                        Action::Bid { auction_id: aid, amount } => {
                            if aid != auction_id || amount < 0 || amount > st.players[i].balance {
                                st.stats[i].failed_actions += 1;
                            } else if cfg.late_bid_prob > 0.0 && late_rng.random_bool(cfg.late_bid_prob) {
                                st.stats[i].late_bids += 1;
                            } else {
                                bids[i] = amount;
                            }
                        }
                    }
                }
                if !played {
                    break;
                }
            }
        }

        // closeAuction: positive bids, shuffled, stable-sorted high to low.
        let mut sorted: Vec<(usize, i64)> = (0..n).filter(|&i| bids[i] > 0).map(|i| (i, bids[i])).collect();
        shuffle(&mut tie_rng, &mut sorted);
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        let mut winner = None;
        let mut paid = 0;
        let mut top = 0;
        if let Some(&(w, amount)) = sorted.first() {
            top = amount;
            paid = match cfg.settings.auction_type {
                AuctionType::FirstPrice => amount,
                AuctionType::Vickrey => sorted.get(1).map(|s| s.1).unwrap_or(AUCTION_RESERVE.min(amount)),
            };
            winner = Some(w);
        }
        let mut sim_winner = None;
        if let Some(w) = winner {
            if st.players[w].balance >= paid {
                st.players[w].balance -= paid;
                st.racks[w].add(l);
                st.stats[w].tiles_won += 1;
                st.stats[w].spent += paid;
                if matches!(seats[w].kind, SeatKind::Sim(_)) {
                    sim_winner = Some(w);
                }
            }
        }
        st.results.push(AuctionRecord { auction_id, letter: l, winner: winner.map(bot_id), paid });
        st.top_bids.push((auction_id, top));
        if let Some(w) = sim_winner {
            // simulateWordPlay: longest 3+ word, alphabetical tie-break.
            if let Some(c) = dict.best_playable(&st.racks[w], 3) {
                let word = dict.class_word(c).to_string();
                let ok = play_word(dict, &mut st, w, &word);
                debug_assert!(ok);
            }
        }
        letter = draw(&mut next_tile);
        index += 1;
    }

    // Match over: no more plays. Tell agents.
    for i in 0..n {
        if let SeatKind::Agent(agent) = &mut seats[i].kind {
            let view = MatchView {
                match_id,
                my_bot_id: bot_id(i),
                settings: cfg.settings,
                phase: cfg.phase,
                players: &st.players,
                my_rack: st.racks[i],
                bag_total: 0,
                open: None,
                results: &st.results,
                plays: &st.plays,
                ended: true,
                now_ms: index as i64 * duration,
            };
            agent.on_match_end(&view, &st.top_bids);
        }
    }

    let unsold = st.results.iter().filter(|r| r.winner.is_none()).count() as u32;
    for i in 0..n {
        st.stats[i].score = st.players[i].score;
        st.stats[i].balance = st.players[i].balance;
        st.stats[i].leftover_tiles = st.racks[i].total();
    }
    MatchOutcome { seats: st.stats, auctions: total_auctions, unsold }
}

/// `submitWord` + `playWord`: validate, take the letters, pay the reward.
fn play_word(dict: &Dict, st: &mut State, seat: usize, word: &str) -> bool {
    let up = word.to_ascii_uppercase();
    let Some(counts) = dict.word_counts(&up) else { return false };
    if !st.racks[seat].contains(&counts) {
        return false;
    }
    st.racks[seat] = st.racks[seat].minus(&counts);
    let reward = crate::game::rules::word_reward(&up) as i64;
    st.players[seat].balance += reward;
    st.players[seat].score += reward;
    st.stats[seat].words += 1;
    let after_results = st.results.len() as u32;
    st.plays.push(PlayRecord { bot_id: st.players[seat].bot_id, word: up, reward, after_results });
    true
}
