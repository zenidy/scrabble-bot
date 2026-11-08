//! The live loop: connection lifecycle, subscriptions, lobby and tournament
//! handling, and feeding snapshots to the strategy. Runs on the main thread;
//! the SDK thread only produces snapshots.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use serde_json::json;
use spacetimedb_sdk::SubscriptionHandle as _;
use tracing::{debug, error, info, warn};

use super::client::{Client, ClientConfig};
use super::events::*;
use super::module_bindings::SubscriptionHandle;
use super::subs;
use crate::game::dict::Dict;
use crate::game::rules::letter_char;
use crate::game::scrabbler::{Params, Scrabbler};
use crate::game::view::{
    Action, AuctionRecord, MatchSettings, MatchView, OpenAuction, Phase, PlayRecord, PlayerInfo,
    Strategy,
};
use crate::persist::{self, MatchLog};

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub host: String,
    pub db: String,
    /// From `BOT_TOKEN`; otherwise read from `token_file`.
    pub token: Option<String>,
    pub nonce: Option<String>,
    pub token_file: PathBuf,
    pub data_dir: PathBuf,
    pub tournament: Option<u64>,
    pub auto_register: bool,
    pub join_lobby: bool,
    pub expected_name: Option<String>,
    pub max_matches: Option<u64>,
    pub params: Params,
    /// Skip a bid we expect to land within this many ms of the close.
    pub safety_ms: u64,
}

#[derive(Default)]
struct RunStats {
    matches: u64,
    /// Results already written to each match's log (survives reconnects).
    logged_results: HashMap<u64, usize>,
    late_bids: u64,
    skipped_bids: u64,
    failed_words: u64,
    /// Final-phase games: match id -> (my score, best opponent score).
    final_games: HashMap<u64, (i64, i64)>,
}

enum Exit {
    Done,
    Fatal(anyhow::Error),
    Disconnected(String),
}

pub fn run(cfg: RunConfig, dict: &'static Dict) -> Result<()> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut params = cfg.params.clone();
    // Live: bound the time per decision (the simulator runs unbounded).
    if params.think_ms == 0 {
        params.think_ms = 150;
    }
    if params.partition_ms == 0 {
        params.partition_ms = 60;
    }
    let mut strategy = Scrabbler::new(dict, params, seed);
    strategy.prices = persist::load_prices(&cfg.data_dir.join("opponents.json"));
    let mut stats = RunStats::default();
    let mut backoff = Duration::from_millis(250);
    loop {
        let token = cfg
            .token
            .clone()
            .or_else(|| persist::load_token(&cfg.token_file));
        if token.is_none() && cfg.nonce.is_none() {
            bail!(
                "no token: set BOT_TOKEN (Team page) or BOT_NONCE, or put a token in {}",
                cfg.token_file.display()
            );
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        info!(host = %cfg.host, db = %cfg.db, "connecting");
        match Client::connect(
            &ClientConfig {
                host: cfg.host.clone(),
                db: cfg.db.clone(),
                token,
            },
            tx,
        ) {
            Ok(client) => {
                let mut session = Session::new(&cfg, client, &mut strategy, &mut stats);
                let exit = session.run(&rx);
                let connected = session.connected;
                drop(session);
                match exit {
                    Exit::Done => return Ok(()),
                    Exit::Fatal(e) => return Err(e),
                    Exit::Disconnected(why) => {
                        warn!(%why, "connection lost");
                        if connected {
                            backoff = Duration::from_millis(250);
                        }
                    }
                }
            }
            Err(e) => warn!("connect failed: {e:#}"),
        }
        info!(?backoff, "reconnecting");
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

struct Session<'a> {
    cfg: &'a RunConfig,
    client: Client,
    strategy: &'a mut Scrabbler,
    stats: &'a mut RunStats,
    connected: bool,
    me: Option<u64>,
    claimed: bool,
    bootstrap: Option<SubscriptionHandle>,
    always: Option<(SubscriptionHandle, Arc<AtomicBool>)>,
    match_subs: HashMap<u64, Vec<SubscriptionHandle>>,
    archive_subs: HashSet<u64>,
    known_matches: Option<HashSet<u64>>,
    finalized: HashSet<u64>,
    seen_running: HashSet<u64>,
    ended_seen: HashMap<u64, Instant>,
    first_seen: HashMap<u64, Instant>,
    logs: HashMap<u64, MatchLog>,
    last_join: Option<Instant>,
    last_register: HashMap<u64, Instant>,
    rtt_ms: f64,
    last: Option<Box<Snapshot>>,
}

impl<'a> Session<'a> {
    fn new(
        cfg: &'a RunConfig,
        client: Client,
        strategy: &'a mut Scrabbler,
        stats: &'a mut RunStats,
    ) -> Self {
        Session {
            cfg,
            client,
            strategy,
            stats,
            connected: false,
            me: None,
            claimed: false,
            bootstrap: None,
            always: None,
            match_subs: HashMap::new(),
            archive_subs: HashSet::new(),
            known_matches: None,
            finalized: HashSet::new(),
            seen_running: HashSet::new(),
            ended_seen: HashMap::new(),
            first_seen: HashMap::new(),
            logs: HashMap::new(),
            last_join: None,
            last_register: HashMap::new(),
            rtt_ms: 60.0,
            last: None,
        }
    }

    fn run(&mut self, rx: &Receiver<NetEvent>) -> Exit {
        loop {
            let ev = match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(ev) => ev,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(s) = self.last.take() {
                        self.housekeeping(&s);
                        self.last = Some(s);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Exit::Disconnected("event channel closed".into());
                }
            };
            let exit = match ev {
                NetEvent::Snapshot(mut s) => {
                    // Only the newest snapshot matters; keep other events in order after it.
                    let mut rest = Vec::new();
                    while let Ok(next) = rx.try_recv() {
                        match next {
                            NetEvent::Snapshot(n) => s = n,
                            other => rest.push(other),
                        }
                    }
                    let mut exit = self.on_snapshot(s);
                    for other in rest {
                        if exit.is_none() {
                            exit = self.on_event(other);
                        }
                    }
                    exit
                }
                other => self.on_event(other),
            };
            if let Some(e) = exit {
                return e;
            }
            if let Some(max) = self.cfg.max_matches
                && self.stats.matches >= max
            {
                info!(matches = self.stats.matches, "reached --max-matches");
                return Exit::Done;
            }
        }
    }

    fn on_event(&mut self, ev: NetEvent) -> Option<Exit> {
        match ev {
            NetEvent::Connected { identity, token } => {
                self.connected = true;
                if (self.cfg.token.is_none() || self.cfg.nonce.is_some())
                    && let Err(e) = persist::save_token(&self.cfg.token_file, &token)
                {
                    warn!("couldn't save token: {e:#}");
                }
                info!(identity = &identity[..16.min(identity.len())], "connected");
                self.bootstrap = Some(self.client.subscribe(subs::bootstrap()).0);
                None
            }
            NetEvent::Snapshot(s) => self.on_snapshot(s),
            NetEvent::CallResult {
                call,
                result,
                sent_at,
            } => {
                self.on_call_result(call, result, sent_at);
                None
            }
            NetEvent::SubscriptionError(e) => {
                error!(%e, "subscription error");
                None
            }
            NetEvent::Disconnected(why) => Some(Exit::Disconnected(why)),
        }
    }

    fn on_snapshot(&mut self, s: Box<Snapshot>) -> Option<Exit> {
        let Some(me) = s.my_bot_id else {
            if s.bots.is_empty() {
                return None; // bootstrap rows not here yet
            }
            if let Some(nonce) = &self.cfg.nonce {
                if !self.claimed {
                    info!("redeeming nonce for a bot credential");
                    self.client.claim(nonce);
                    self.claimed = true;
                }
                return None;
            }
            return Some(Exit::Fatal(anyhow!(
                "this token is not a credential for any bot: generate a token on the Team page (BOT_TOKEN) or mint a nonce (BOT_NONCE)"
            )));
        };
        if self.me.is_none() {
            self.me = Some(me);
            let name = s.my_bot_name.clone().unwrap_or_default();
            info!(bot_id = me, %name, "playing as");
            if let Some(expected) = &self.cfg.expected_name
                && &name != expected
            {
                warn!(%name, %expected, "bot name differs from the expected name");
            }
            self.always = Some(self.client.subscribe(subs::always(me)));
        }
        let always_applied = self
            .always
            .as_ref()
            .map(|a| a.1.load(Ordering::Acquire))
            .unwrap_or(false);
        if always_applied {
            if self.known_matches.is_none() {
                let known: HashSet<u64> = s.my_match_ids.iter().copied().collect();
                // Resume the newest match if we were mid-match.
                if let Some(&latest) = s.my_match_ids.last() {
                    self.subscribe_match(latest);
                }
                self.known_matches = Some(known);
            } else {
                let known = self.known_matches.as_ref().expect("set above");
                let new: Vec<u64> = s
                    .my_match_ids
                    .iter()
                    .copied()
                    .filter(|id| !known.contains(id))
                    .collect();
                for id in new {
                    if let Some(k) = self.known_matches.as_mut() {
                        k.insert(id);
                    }
                    info!(match_id = id, "new match");
                    self.subscribe_match(id);
                }
            }
        }
        for m in &s.matches {
            if m.row.running {
                self.play(m, &s);
            } else if m.row.ended && !self.finalized.contains(&m.row.id) {
                self.finish(m, &s);
            }
        }
        self.housekeeping(&s);
        self.last = Some(s);
        None
    }

    fn subscribe_match(&mut self, id: u64) {
        if self.match_subs.contains_key(&id) || self.finalized.contains(&id) {
            return;
        }
        let (h, _) = self.client.subscribe(subs::per_match(id));
        self.match_subs.insert(id, vec![h]);
    }

    fn phase_for(&self, match_id: u64, s: &Snapshot) -> Phase {
        let Some(tm) = s.tournament_matches.iter().find(|t| t.match_id == match_id) else {
            return Phase::Lobby;
        };
        match tm.phase.as_str() {
            "Swiss" => Phase::Swiss,
            "Bracket" => Phase::Bracket,
            "Final" => {
                let earlier: Vec<u64> = s
                    .tournament_matches
                    .iter()
                    .filter(|t| {
                        t.tournament_id == tm.tournament_id
                            && t.phase == "Final"
                            && t.round < tm.round
                    })
                    .map(|t| t.match_id)
                    .collect();
                let margin = earlier
                    .iter()
                    .filter_map(|id| self.stats.final_games.get(id))
                    .map(|(a, b)| a - b)
                    .sum();
                Phase::Final {
                    game: tm.round,
                    margin,
                }
            }
            _ => Phase::Lobby,
        }
    }

    fn view_parts(
        &self,
        m: &MatchSnap,
        s: &Snapshot,
    ) -> (Vec<PlayerInfo>, Vec<AuctionRecord>, Vec<PlayRecord>) {
        let players = m
            .participants
            .iter()
            .map(|p| PlayerInfo {
                bot_id: p.bot_id,
                balance: p.balance,
                score: p.score,
                sim: s.bots.get(&p.bot_id).and_then(|b| b.1),
            })
            .collect();
        let results: Vec<AuctionRecord> = m
            .results
            .iter()
            .map(|r| AuctionRecord {
                auction_id: r.auction_id,
                letter: r.letter,
                winner: r.winner,
                paid: r.paid,
            })
            .collect();
        let plays = m
            .plays
            .iter()
            .map(|p| PlayRecord {
                bot_id: p.bot_id,
                word: p.word.clone(),
                reward: p.reward,
                after_results: m
                    .results
                    .iter()
                    .filter(|r| r.closed_at_us <= p.played_at_us)
                    .count() as u32,
            })
            .collect();
        (players, results, plays)
    }

    fn log(&mut self, match_id: u64, record: serde_json::Value) {
        let dir = self.cfg.data_dir.join("matches");
        self.logs
            .entry(match_id)
            .or_insert_with(|| MatchLog::open(&dir, match_id))
            .write(&record);
    }

    fn play(&mut self, m: &MatchSnap, s: &Snapshot) {
        let me = self.me.unwrap_or(0);
        let mid = m.row.id;
        self.seen_running.insert(mid);
        // Log newly closed auctions.
        let done = *self.stats.logged_results.get(&mid).unwrap_or(&0);
        if m.results.len() > done {
            for r in &m.results[done..] {
                let rec = json!({"t": "result", "auction": r.auction_id, "letter": letter_char(r.letter).to_string(), "winner": r.winner, "paid": r.paid, "mine": r.winner == Some(me)});
                self.log(mid, rec);
            }
            self.stats.logged_results.insert(mid, m.results.len());
        }
        let open = m
            .auctions
            .iter()
            .find(|a| a.open && Some(a.id) == m.row.current_auction_id)
            .or_else(|| m.auctions.iter().find(|a| a.open));
        let (players, results, plays) = self.view_parts(m, s);
        if !players.iter().any(|p| p.bot_id == me) {
            return; // participants not delivered yet
        }
        let open_auction = open.map(|a| {
            self.first_seen.entry(a.id).or_insert(s.received);
            OpenAuction {
                auction_id: a.id,
                letter: a.letter,
                index: m.row.current_round.saturating_sub(1),
                closes_at_ms: a.closes_at_us / 1000,
            }
        });
        let view = MatchView {
            match_id: mid,
            my_bot_id: me,
            settings: MatchSettings {
                auction_type: m.row.auction_type,
                starting_rack_size: m.row.starting_rack_size,
                auction_duration_ms: m.row.auction_duration_ms,
            },
            phase: self.phase_for(mid, s),
            players: &players,
            my_rack: m.rack,
            bag_total: m.row.bag_total,
            open: open_auction,
            results: &results,
            plays: &plays,
            ended: false,
            now_ms: 0,
        };
        let t0 = Instant::now();
        let actions = self.strategy.on_update(&view);
        let think_ms = t0.elapsed().as_secs_f64() * 1e3;
        let decision = self.strategy.last_decision.take();
        for a in actions {
            match a {
                Action::PlayWord(w) => {
                    info!(match_id = mid, word = %w, "play");
                    self.client.word(mid, &w);
                    self.log(mid, json!({"t": "word", "word": w}));
                }
                Action::Bid { auction_id, amount } => {
                    let age = self
                        .first_seen
                        .get(&auction_id)
                        .map(|t| t.elapsed().as_secs_f64() * 1e3)
                        .unwrap_or(0.0);
                    let budget = m.row.auction_duration_ms as f64
                        - self.cfg.safety_ms as f64
                        - self.rtt_ms / 2.0;
                    if age > budget {
                        self.stats.skipped_bids += 1;
                        warn!(auction_id, age_ms = age as u64, "too late to bid; skipped");
                        self.log(
                            mid,
                            json!({"t": "skip", "auction": auction_id, "age_ms": age}),
                        );
                        continue;
                    }
                    self.client.bid(mid, auction_id, amount);
                    let d = decision.clone().unwrap_or_default();
                    debug!(
                        auction_id,
                        amount,
                        value = d.value,
                        lambda = d.lambda,
                        "bid"
                    );
                    self.log(
                        mid,
                        json!({"t": "bid", "auction": auction_id, "letter": d.letter.to_string(), "value": d.value, "lambda": d.lambda,
                               "bid": amount, "balance": d.balance, "age_ms": age, "think_ms": think_ms, "rtt_ms": self.rtt_ms}),
                    );
                }
            }
        }
    }

    fn finish(&mut self, m: &MatchSnap, s: &Snapshot) {
        let mid = m.row.id;
        if !self.seen_running.contains(&mid) {
            // Already over before this process saw it (e.g. the last match of
            // a previous run): nothing to learn twice or to count.
            if let Some(handles) = self.match_subs.remove(&mid) {
                for h in handles {
                    let _ = h.unsubscribe();
                }
            }
            self.finalized.insert(mid);
            return;
        }
        let seen = *self.ended_seen.entry(mid).or_insert_with(Instant::now);
        if m.archive.is_empty() {
            if self.archive_subs.insert(mid) {
                let (h, _) = self.client.subscribe(subs::archive(mid));
                self.match_subs.entry(mid).or_default().push(h);
            }
            if seen.elapsed() < Duration::from_secs(5) {
                return;
            }
        }
        let me = self.me.unwrap_or(0);
        let (players, results, plays) = self.view_parts(m, s);
        let view = MatchView {
            match_id: mid,
            my_bot_id: me,
            settings: MatchSettings {
                auction_type: m.row.auction_type,
                starting_rack_size: m.row.starting_rack_size,
                auction_duration_ms: m.row.auction_duration_ms,
            },
            phase: self.phase_for(mid, s),
            players: &players,
            my_rack: m.rack,
            bag_total: 0,
            open: None,
            results: &results,
            plays: &plays,
            ended: true,
            now_ms: 0,
        };
        if !m.results.is_empty() {
            self.strategy.on_match_end(&view, &m.archive);
            if let Err(e) = persist::save_prices(
                &self.cfg.data_dir.join("opponents.json"),
                &self.strategy.prices,
            ) {
                warn!("couldn't save opponent models: {e:#}");
            }
        }
        let mine = players
            .iter()
            .find(|p| p.bot_id == me)
            .map(|p| p.score)
            .unwrap_or(0);
        let best_other = players
            .iter()
            .filter(|p| p.bot_id != me)
            .map(|p| p.score)
            .max()
            .unwrap_or(0);
        let place = 1 + players.iter().filter(|p| p.score > mine).count();
        if matches!(view.phase, Phase::Final { .. }) {
            self.stats.final_games.insert(mid, (mine, best_other));
        }
        let scores: Vec<serde_json::Value> = players
            .iter()
            .map(|p| json!({"bot": p.bot_id, "name": s.bots.get(&p.bot_id).map(|b| b.0.clone()), "score": p.score, "sim": p.sim.is_some()}))
            .collect();
        self.log(mid, json!({"t": "end", "score": mine, "place": place, "players": players.len(), "margin": mine - best_other, "scores": scores, "leftover": m.rack.to_string()}));
        self.logs.remove(&mid);
        if !m.results.is_empty() {
            self.stats.matches += 1;
            info!(
                match_id = mid,
                score = mine,
                place,
                of = players.len(),
                margin = mine - best_other,
                matches = self.stats.matches,
                late = self.stats.late_bids,
                skipped = self.stats.skipped_bids,
                failed_words = self.stats.failed_words,
                "match over"
            );
        }
        if let Some(handles) = self.match_subs.remove(&mid) {
            for h in handles {
                let _ = h.unsubscribe();
            }
        }
        self.finalized.insert(mid);
    }

    fn held_for_tournament(&self, s: &Snapshot) -> bool {
        s.tournaments.iter().any(|t| {
            t.registered
                && !t.eliminated
                && matches!(t.status.as_str(), "Swiss" | "Bracket" | "Final")
        })
    }

    fn housekeeping(&mut self, s: &Snapshot) {
        let Some(me) = self.me else { return };
        // Tournament registration.
        for t in &s.tournaments {
            if t.status == "Registration"
                && !t.registered
                && (self.cfg.tournament == Some(t.id) || self.cfg.auto_register)
            {
                let due = self
                    .last_register
                    .get(&t.id)
                    .is_none_or(|at| at.elapsed() > Duration::from_secs(10));
                if due {
                    info!(tournament = t.id, "registering");
                    self.client.register(t.id);
                    self.last_register.insert(t.id, Instant::now());
                }
            }
        }
        // Lobby.
        if !self.cfg.join_lobby || self.known_matches.is_none() {
            return;
        }
        let busy = s.matches.iter().any(|m| m.row.running)
            || self.known_matches.as_ref().is_some_and(|k| {
                k.iter().any(|id| {
                    self.match_subs.contains_key(id) && !s.matches.iter().any(|m| m.row.id == *id)
                })
            });
        if busy || self.held_for_tournament(s) {
            return;
        }
        // About to stop at --max-matches: don't queue for a match we won't play.
        if let Some(max) = self.cfg.max_matches {
            let pending = s
                .matches
                .iter()
                .filter(|m| {
                    m.row.ended
                        && !self.finalized.contains(&m.row.id)
                        && self.seen_running.contains(&m.row.id)
                })
                .count() as u64;
            if self.stats.matches + pending >= max {
                return;
            }
        }
        if s.lobby.as_ref().is_some_and(|l| l.members.contains(&me)) {
            return;
        }
        if self
            .last_join
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        info!("joining lobby");
        self.client.join_lobby();
        self.last_join = Some(Instant::now());
    }

    fn on_call_result(&mut self, call: Call, result: Result<(), String>, sent_at: Instant) {
        let rtt = sent_at.elapsed().as_secs_f64() * 1e3;
        self.rtt_ms = 0.8 * self.rtt_ms + 0.2 * rtt;
        match (call, result) {
            (
                Call::Bid {
                    match_id,
                    auction_id,
                    amount,
                },
                Err(msg),
            ) => {
                if msg.contains("window expired") || msg.contains("Auction closed") {
                    self.stats.late_bids += 1;
                }
                warn!(auction_id, amount, %msg, rtt_ms = rtt as u64, "bid refused");
                let retry = msg.contains("Insufficient balance");
                self.strategy.on_bid_failed(auction_id, retry);
                self.log(
                    match_id,
                    json!({"t": "bid_error", "auction": auction_id, "amount": amount, "msg": msg}),
                );
            }
            (Call::Word { match_id, word }, Err(msg)) => {
                self.stats.failed_words += 1;
                warn!(match_id, %word, %msg, "word refused");
                self.strategy.on_word_failed(match_id, &word);
                self.log(
                    match_id,
                    json!({"t": "word_error", "word": word, "msg": msg}),
                );
            }
            (Call::JoinLobby, Err(msg)) => debug!(%msg, "join_lobby refused"),
            (Call::Register(t), Ok(())) => info!(tournament = t, "registered"),
            (Call::Register(t), Err(msg)) => warn!(tournament = t, %msg, "registration refused"),
            (Call::Claim, Ok(())) => info!("credential claimed"),
            (Call::Claim, Err(msg)) => error!(%msg, "nonce claim refused"),
            _ => {}
        }
    }
}
