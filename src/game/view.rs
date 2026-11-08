//! What a strategy sees and does. The simulator and the live client both
//! build a [`MatchView`] after every transaction and send back the
//! [`Action`]s, so strategy code never touches the network.

use super::rules::{AuctionType, Rack, SimStrategy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchSettings {
    pub auction_type: AuctionType,
    pub starting_rack_size: u32,
    pub auction_duration_ms: u64,
}

impl Default for MatchSettings {
    fn default() -> Self {
        MatchSettings {
            auction_type: AuctionType::Vickrey,
            starting_rack_size: super::rules::DEFAULT_RACK_SIZE,
            auction_duration_ms: super::rules::DEFAULT_AUCTION_MS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerInfo {
    pub bot_id: u64,
    pub balance: i64,
    pub score: i64,
    /// `Some` for module-run sim bots (public in the `bot` table).
    pub sim: Option<SimStrategy>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenAuction {
    pub auction_id: u64,
    pub letter: u8,
    /// 0-based index of this auction within the match.
    pub index: u32,
    /// Server time the auction closes (ms since epoch; sim: virtual time).
    pub closes_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuctionRecord {
    pub auction_id: u64,
    pub letter: u8,
    pub winner: Option<u64>,
    /// Vickrey: runner-up bid, or 1 for a lone bidder. First price: top bid.
    pub paid: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayRecord {
    pub bot_id: u64,
    pub word: String,
    pub reward: i64,
    /// How many auctions had closed when the word was played.
    pub after_results: u32,
}

/// Where the match sits in a tournament (or not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Lobby,
    Swiss,
    Bracket,
    /// Two-bot final: `margin` is my summed score minus the opponent's over
    /// the final's earlier games; `game` is 1-based.
    Final { game: u32, margin: i64 },
}

#[derive(Clone, Debug)]
pub struct MatchView<'a> {
    pub match_id: u64,
    pub my_bot_id: u64,
    pub settings: MatchSettings,
    pub phase: Phase,
    pub players: &'a [PlayerInfo],
    pub my_rack: Rack,
    /// Tiles left in the bag, not counting the open auction's tile.
    pub bag_total: u32,
    pub open: Option<OpenAuction>,
    /// Closed auctions, oldest first.
    pub results: &'a [AuctionRecord],
    /// Words played so far, oldest first.
    pub plays: &'a [PlayRecord],
    pub ended: bool,
    /// Local estimate of server time now (ms); sims use virtual time.
    pub now_ms: i64,
}

impl MatchView<'_> {
    pub fn me(&self) -> Option<&PlayerInfo> {
        self.players.iter().find(|p| p.bot_id == self.my_bot_id)
    }

    pub fn my_balance(&self) -> i64 {
        self.me().map(|p| p.balance).unwrap_or(0)
    }

    pub fn my_score(&self) -> i64 {
        self.me().map(|p| p.score).unwrap_or(0)
    }

    /// Auctions still to run, counting the open one.
    pub fn auctions_left(&self) -> u32 {
        self.bag_total + u32::from(self.open.is_some())
    }

    /// The open auction is the match's last: its tile arrives in the
    /// transaction that ends the match, too late for any bot to play.
    pub fn is_last_auction(&self) -> bool {
        self.open.is_some() && self.bag_total == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    PlayWord(String),
    Bid { auction_id: u64, amount: i64 },
}

/// A bidding and word-play policy.
pub trait Strategy: Send {
    fn name(&self) -> String;

    /// Called after every transaction touching the match (including the one
    /// that starts it), and again after this strategy's own words land.
    /// Returns what to send now, in order.
    fn on_update(&mut self, view: &MatchView) -> Vec<Action>;

    /// The match is over; final scores are in the view.
    fn on_match_end(&mut self, _view: &MatchView, _top_bids: &[(u64, i64)]) {}
}
