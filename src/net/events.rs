//! Messages from the SDK thread to the strategy thread.

use std::collections::HashMap;
use std::time::Instant;

use crate::game::rules::{AuctionType, Rack, SimStrategy};

#[derive(Clone, Debug)]
pub struct MatchRow {
    pub id: u64,
    pub running: bool,
    pub ended: bool,
    pub current_round: u32,
    pub current_auction_id: Option<u64>,
    pub bag_total: u32,
    pub auction_type: AuctionType,
    pub auction_duration_ms: u64,
    pub starting_rack_size: u32,
    pub created_at_us: i64,
}

#[derive(Clone, Debug)]
pub struct ParticipantRow {
    pub bot_id: u64,
    pub balance: i64,
    pub score: i64,
}

#[derive(Clone, Debug)]
pub struct AuctionRow {
    pub id: u64,
    pub letter: u8,
    pub opens_at_us: i64,
    pub closes_at_us: i64,
    pub open: bool,
}

#[derive(Clone, Debug)]
pub struct ResultRow {
    pub auction_id: u64,
    pub letter: u8,
    pub winner: Option<u64>,
    pub paid: i64,
    pub closed_at_us: i64,
}

#[derive(Clone, Debug)]
pub struct PlayRow {
    pub id: u64,
    pub bot_id: u64,
    pub word: String,
    pub reward: i64,
    pub played_at_us: i64,
}

/// Everything about one of my matches, read from the client cache in one go.
#[derive(Clone, Debug)]
pub struct MatchSnap {
    pub row: MatchRow,
    pub participants: Vec<ParticipantRow>,
    pub auctions: Vec<AuctionRow>,
    pub results: Vec<ResultRow>,
    pub plays: Vec<PlayRow>,
    pub rack: Rack,
    pub archive: Vec<(u64, i64)>,
}

#[derive(Clone, Debug)]
pub struct LobbySnap {
    pub lobby_id: u64,
    pub closes_at_us: i64,
    pub max_size: u32,
    pub members: Vec<u64>,
}

#[derive(Clone, Debug)]
pub struct TournamentSnap {
    pub id: u64,
    pub status: String,
    pub registered: bool,
    pub eliminated: bool,
}

#[derive(Clone, Debug)]
pub struct TournamentMatchSnap {
    pub tournament_id: u64,
    pub match_id: u64,
    pub round: u32,
    pub phase: String,
}

/// A consistent view of the client cache after one server message.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub received: Instant,
    pub my_bot_id: Option<u64>,
    pub my_bot_name: Option<String>,
    /// Matches I have a participant row in (any status).
    pub my_match_ids: Vec<u64>,
    /// bot id -> (name, sim strategy if simulated)
    pub bots: HashMap<u64, (String, Option<SimStrategy>)>,
    pub matches: Vec<MatchSnap>,
    pub lobby: Option<LobbySnap>,
    pub tournaments: Vec<TournamentSnap>,
    pub tournament_matches: Vec<TournamentMatchSnap>,
}

#[derive(Clone, Debug)]
pub enum Call {
    Bid {
        match_id: u64,
        auction_id: u64,
        amount: i64,
    },
    Word {
        match_id: u64,
        word: String,
    },
    JoinLobby,
    Register(u64),
    Claim,
}

#[derive(Debug)]
pub enum NetEvent {
    Connected {
        identity: String,
        token: String,
    },
    Snapshot(Box<Snapshot>),
    /// `Ok(())` or the module's error message.
    CallResult {
        call: Call,
        result: Result<(), String>,
        sent_at: Instant,
    },
    SubscriptionError(String),
    Disconnected(String),
}
