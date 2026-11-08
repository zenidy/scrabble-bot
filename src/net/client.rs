//! SpacetimeDB connection. The SDK thread here runs the message loop; after
//! each server message that touched a subscribed row it reads one
//! consistent [`Snapshot`] of the client cache and sends it to the strategy
//! thread. Row callbacks only flip a flag; nothing blocks the SDK thread.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use spacetimedb_sdk::{DbContext, Identity, Table, TableWithPrimaryKey};

use super::events::*;
use super::module_bindings::*;
use crate::game::rules::{AuctionType as GameAuctionType, Rack, SimStrategy, letter_index};

pub struct ClientConfig {
    pub host: String,
    pub db: String,
    pub token: Option<String>,
}

pub struct Client {
    pub conn: Arc<DbConnection>,
    tx: Sender<NetEvent>,
    dirty: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn flatten(
    res: Result<Result<(), String>, spacetimedb_sdk::__codegen::InternalError>,
) -> Result<(), String> {
    match res {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) => Err(msg),
        Err(e) => Err(format!("internal: {e}")),
    }
}

fn letter_of(s: &str) -> u8 {
    s.chars().next().and_then(letter_index).unwrap_or(0)
}

fn us(t: spacetimedb_sdk::Timestamp) -> i64 {
    t.to_micros_since_unix_epoch()
}

impl Client {
    pub fn connect(cfg: &ClientConfig, tx: Sender<NetEvent>) -> Result<Client> {
        let dirty = Arc::new(AtomicBool::new(true));
        let (t1, t2, t3) = (tx.clone(), tx.clone(), tx.clone());
        let conn = DbConnection::builder()
            .with_uri(cfg.host.as_str())
            .with_database_name(cfg.db.clone())
            .with_token(cfg.token.clone())
            .with_confirmed_reads(false)
            .on_connect(move |_c, identity: Identity, token: &str| {
                let _ = t1.send(NetEvent::Connected {
                    identity: identity.to_hex().to_string(),
                    token: token.to_string(),
                });
            })
            .on_connect_error(move |_ctx, err| {
                let _ = t2.send(NetEvent::Disconnected(format!("connect error: {err}")));
            })
            .on_disconnect(move |_ctx, err| {
                let _ = t3.send(NetEvent::Disconnected(format!("disconnected: {err:?}")));
            })
            .build()
            .context("building SpacetimeDB connection")?;
        let conn = Arc::new(conn);
        Self::watch(&conn, &dirty);

        let c = Arc::clone(&conn);
        let d = Arc::clone(&dirty);
        let t = tx.clone();
        let thread = std::thread::Builder::new()
            .name("stdb-sdk".into())
            .spawn(move || {
                loop {
                    if let Err(e) = c.advance_one_message_blocking() {
                        let _ = t.send(NetEvent::Disconnected(format!("message loop ended: {e}")));
                        return;
                    }
                    if d.swap(false, Ordering::AcqRel) {
                        let snap = build_snapshot(&c);
                        if t.send(NetEvent::Snapshot(Box::new(snap))).is_err() {
                            return;
                        }
                    }
                }
            })
            .context("spawning SDK thread")?;
        Ok(Client {
            conn,
            tx,
            dirty,
            thread: Some(thread),
        })
    }

    /// Row callbacks: mark the cache dirty, nothing else.
    fn watch(conn: &DbConnection, dirty: &Arc<AtomicBool>) {
        macro_rules! mark {
            ($t:expr) => {{
                let d = Arc::clone(dirty);
                $t.on_insert(move |_, _| d.store(true, Ordering::Release));
                let d = Arc::clone(dirty);
                $t.on_delete(move |_, _| d.store(true, Ordering::Release));
            }};
        }
        macro_rules! mark_upd {
            ($t:expr) => {{
                mark!($t);
                let d = Arc::clone(dirty);
                $t.on_update(move |_, _, _| d.store(true, Ordering::Release));
            }};
        }
        mark_upd!(conn.db.match_state());
        mark_upd!(conn.db.auction());
        mark_upd!(conn.db.auction_result());
        mark_upd!(conn.db.word_play());
        mark_upd!(conn.db.match_participant());
        mark_upd!(conn.db.my_rack());
        mark!(conn.db.open_lobbies());
        mark!(conn.db.open_lobby_members());
        mark_upd!(conn.db.tournament());
        mark_upd!(conn.db.tournament_entry());
        mark_upd!(conn.db.tournament_match());
        mark_upd!(conn.db.auction_top_bid_archive());
        mark!(conn.db.bot_credential());
        mark!(conn.db.bot());
    }

    pub fn identity(&self) -> Option<Identity> {
        self.conn.try_identity()
    }

    /// Subscribes; the flag turns true once the initial rows are applied.
    pub fn subscribe(&self, queries: Vec<String>) -> (SubscriptionHandle, Arc<AtomicBool>) {
        let d = Arc::clone(&self.dirty);
        let t = self.tx.clone();
        let applied = Arc::new(AtomicBool::new(false));
        let a = Arc::clone(&applied);
        let h = self
            .conn
            .subscription_builder()
            .on_applied(move |_| {
                a.store(true, Ordering::Release);
                d.store(true, Ordering::Release);
            })
            .on_error(move |_, e| {
                let _ = t.send(NetEvent::SubscriptionError(e.to_string()));
            })
            .subscribe(queries);
        (h, applied)
    }

    fn reply(
        &self,
        call: Call,
    ) -> impl FnOnce(
        &ReducerEventContext,
        Result<Result<(), String>, spacetimedb_sdk::__codegen::InternalError>,
    ) + Send
    + 'static {
        let t = self.tx.clone();
        let sent_at = Instant::now();
        move |_ctx, res| {
            let _ = t.send(NetEvent::CallResult {
                call,
                result: flatten(res),
                sent_at,
            });
        }
    }

    pub fn bid(&self, match_id: u64, auction_id: u64, amount: i64) {
        let cb = self.reply(Call::Bid {
            match_id,
            auction_id,
            amount,
        });
        let _ = self.conn.reducers.submit_bid_then(auction_id, amount, cb);
    }

    pub fn word(&self, match_id: u64, word: &str) {
        let cb = self.reply(Call::Word {
            match_id,
            word: word.to_string(),
        });
        let _ = self
            .conn
            .reducers
            .submit_word_then(match_id, word.to_string(), cb);
    }

    pub fn join_lobby(&self) {
        let cb = self.reply(Call::JoinLobby);
        let _ = self.conn.reducers.join_lobby_then(cb);
    }

    pub fn register(&self, tournament_id: u64) {
        let cb = self.reply(Call::Register(tournament_id));
        let _ = self
            .conn
            .reducers
            .register_for_tournament_then(tournament_id, cb);
    }

    pub fn claim(&self, code: &str) {
        let cb = self.reply(Call::Claim);
        let _ = self
            .conn
            .reducers
            .claim_credential_then(code.to_string(), cb);
    }

    pub fn disconnect(&mut self) {
        let _ = self.conn.disconnect();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.conn.disconnect();
    }
}

fn sim_strategy(s: &BotStrategy) -> Option<SimStrategy> {
    match s {
        BotStrategy::Cheapskate => Some(SimStrategy::Cheapskate),
        BotStrategy::ValueBidder => Some(SimStrategy::ValueBidder),
        BotStrategy::Aggressive => Some(SimStrategy::Aggressive),
        BotStrategy::Human => None,
    }
}

/// Reads the cache. Called on the SDK thread between messages, so every
/// table reflects the same transaction.
pub fn build_snapshot(conn: &DbConnection) -> Snapshot {
    let received = Instant::now();
    let me = conn
        .try_identity()
        .and_then(|id| conn.db.bot_credential().identity().find(&id))
        .map(|c| c.bot_id);
    let mut bots = HashMap::new();
    for b in conn.db.bot().iter() {
        let sim = if b.is_simulated {
            sim_strategy(&b.strategy)
        } else {
            None
        };
        bots.insert(b.id, (b.name.clone(), sim));
    }
    let my_bot_name = me.and_then(|id| bots.get(&id).map(|b| b.0.clone()));

    let mut matches = Vec::new();
    let mut my_match_ids: Vec<u64> = Vec::new();
    if let Some(me) = me {
        my_match_ids = conn
            .db
            .match_participant()
            .iter()
            .filter(|p| p.bot_id == me)
            .map(|p| p.match_id)
            .collect();
        let all_parts: Vec<MatchParticipant> = conn.db.match_participant().iter().collect();
        let all_auctions: Vec<Auction> = conn.db.auction().iter().collect();
        let all_results: Vec<AuctionResult> = conn.db.auction_result().iter().collect();
        let all_plays: Vec<WordPlay> = conn.db.word_play().iter().collect();
        let all_rack: Vec<Holding> = conn.db.my_rack().iter().collect();
        let all_archive: Vec<AuctionTopBidArchive> =
            conn.db.auction_top_bid_archive().iter().collect();
        for m in conn.db.match_state().iter() {
            if !my_match_ids.contains(&m.id) {
                continue;
            }
            let running = matches!(m.status, MatchStatus::Running);
            let ended = matches!(m.status, MatchStatus::Ended);
            let row = MatchRow {
                id: m.id,
                running,
                ended,
                current_round: m.current_round,
                current_auction_id: m.current_auction_id,
                bag_total: m.bag_total,
                auction_type: match m.auction_type {
                    AuctionType::FirstPrice => GameAuctionType::FirstPrice,
                    AuctionType::Vickrey => GameAuctionType::Vickrey,
                },
                auction_duration_ms: m.auction_duration_ms,
                starting_rack_size: m.starting_rack_size,
                created_at_us: us(m.created_at),
            };
            let mut rack = Rack::EMPTY;
            for h in all_rack.iter().filter(|h| h.match_id == m.id) {
                let l = letter_of(&h.letter);
                rack.0[l as usize] = rack.0[l as usize].saturating_add(h.count.min(255) as u8);
            }
            let mut results: Vec<ResultRow> = all_results
                .iter()
                .filter(|r| r.match_id == m.id)
                .map(|r| ResultRow {
                    auction_id: r.auction_id,
                    letter: letter_of(&r.letter),
                    winner: r.winner_bot_id,
                    paid: r.paid,
                    closed_at_us: us(r.closed_at),
                })
                .collect();
            results.sort_by_key(|r| (r.closed_at_us, r.auction_id));
            let mut plays: Vec<PlayRow> = all_plays
                .iter()
                .filter(|p| p.match_id == m.id)
                .map(|p| PlayRow {
                    id: p.id,
                    bot_id: p.bot_id,
                    word: p.word.clone(),
                    reward: p.total_reward,
                    played_at_us: us(p.played_at),
                })
                .collect();
            plays.sort_by_key(|p| (p.played_at_us, p.id));
            let mut auctions: Vec<AuctionRow> = all_auctions
                .iter()
                .filter(|a| a.match_id == m.id)
                .map(|a| AuctionRow {
                    id: a.id,
                    letter: letter_of(&a.letter),
                    opens_at_us: us(a.opens_at),
                    closes_at_us: us(a.closes_at),
                    open: matches!(a.status, AuctionStatus::Open),
                })
                .collect();
            auctions.sort_by_key(|a| (a.opens_at_us, a.id));
            matches.push(MatchSnap {
                row,
                participants: all_parts
                    .iter()
                    .filter(|p| p.match_id == m.id)
                    .map(|p| ParticipantRow {
                        bot_id: p.bot_id,
                        balance: p.balance,
                        score: p.score,
                    })
                    .collect(),
                auctions,
                results,
                plays,
                rack,
                archive: all_archive
                    .iter()
                    .filter(|a| a.match_id == m.id)
                    .map(|a| (a.auction_id, a.top_bid))
                    .collect(),
            });
        }
    }

    let lobby = conn.db.open_lobbies().iter().next().map(|l| LobbySnap {
        lobby_id: l.id,
        closes_at_us: us(l.closes_at),
        max_size: l.max_size,
        members: conn
            .db
            .open_lobby_members()
            .iter()
            .filter(|m| m.lobby_id == l.id)
            .map(|m| m.bot_id)
            .collect(),
    });

    let mut tournaments = Vec::new();
    let mut tournament_matches = Vec::new();
    if let Some(me) = me {
        let entries: Vec<TournamentEntry> = conn
            .db
            .tournament_entry()
            .iter()
            .filter(|e| e.bot_id == me)
            .collect();
        for t in conn.db.tournament().iter() {
            let e = entries.iter().find(|e| e.tournament_id == t.id);
            tournaments.push(TournamentSnap {
                id: t.id,
                status: format!("{:?}", t.status),
                registered: e.is_some(),
                eliminated: e.map(|e| e.eliminated).unwrap_or(false),
            });
        }
        for tm in conn.db.tournament_match().iter() {
            tournament_matches.push(TournamentMatchSnap {
                tournament_id: tm.tournament_id,
                match_id: tm.match_id,
                round: tm.round,
                phase: format!("{:?}", tm.phase),
            });
        }
    }

    my_match_ids.sort_unstable();
    my_match_ids.dedup();
    Snapshot {
        received,
        my_bot_id: me,
        my_bot_name,
        my_match_ids,
        bots,
        matches,
        lobby,
        tournaments,
        tournament_matches,
    }
}
