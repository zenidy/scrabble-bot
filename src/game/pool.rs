//! What's left to see: the unseen-tile pool and each opponent's likely rack,
//! derived only from public tables plus our own rack.
//!
//! - My starting tiles = my rack + letters I played - letters I won.
//! - An opponent's revealed starting tiles, per letter, are the running max
//!   of (played - won) over the match's events in order.
//! - Unseen = full bag - my start - every auctioned tile (open one included)
//!   - opponents' revealed starting tiles. It holds the bag plus the
//!   opponents' unrevealed starting tiles; future tiles are a uniform sample
//!   of it.

use super::rules::{NUM_LETTERS, Rack};
use super::view::MatchView;

#[derive(Clone, Debug)]
pub struct Opponent {
    pub bot_id: u64,
    /// Tiles we know they hold: won - played + revealed start.
    pub known: Rack,
    /// Starting tiles not yet revealed (drawn from the unseen pool).
    pub hidden: u32,
    pub balance: i64,
    pub score: i64,
    pub is_sim: bool,
}

#[derive(Clone, Debug)]
pub struct PoolInfo {
    pub unseen: Rack,
    pub my_start: Rack,
    pub opponents: Vec<Opponent>,
}

fn word_rack(word: &str) -> Rack {
    Rack::from_letters(word)
}

pub fn analyze(view: &MatchView) -> PoolInfo {
    let ids: Vec<u64> = view.players.iter().map(|p| p.bot_id).collect();
    let idx = |id: u64| ids.iter().position(|&x| x == id);
    let n = ids.len();
    let mut won = vec![[0i32; NUM_LETTERS]; n];
    let mut played = vec![[0i32; NUM_LETTERS]; n];
    let mut revealed = vec![[0i32; NUM_LETTERS]; n];

    let mut plays = view.plays.iter().peekable();
    let mut apply_plays_through = |k: u32, won: &Vec<[i32; NUM_LETTERS]>, played: &mut Vec<[i32; NUM_LETTERS]>, revealed: &mut Vec<[i32; NUM_LETTERS]>| {
        while let Some(p) = plays.peek() {
            if p.after_results > k {
                break;
            }
            if let Some(i) = idx(p.bot_id) {
                let r = word_rack(&p.word);
                for l in 0..NUM_LETTERS {
                    played[i][l] += r.0[l] as i32;
                    revealed[i][l] = revealed[i][l].max(played[i][l] - won[i][l]);
                }
            }
            plays.next();
        }
    };
    for (k, res) in view.results.iter().enumerate() {
        apply_plays_through(k as u32, &won, &mut played, &mut revealed);
        if let Some(w) = res.winner.and_then(idx) {
            won[w][res.letter as usize] += 1;
        }
    }
    apply_plays_through(u32::MAX, &won, &mut played, &mut revealed);

    let me = idx(view.my_bot_id);
    let mut my_start = Rack::EMPTY;
    if let Some(m) = me {
        for l in 0..NUM_LETTERS {
            let s = view.my_rack.0[l] as i32 + played[m][l] - won[m][l];
            my_start.0[l] = s.max(0) as u8;
        }
    }

    let mut unseen = Rack::full_bag().minus_sat(&my_start);
    let mut auctioned = Rack::EMPTY;
    for r in view.results {
        auctioned.add(r.letter);
    }
    if let Some(o) = view.open {
        auctioned.add(o.letter);
    }
    unseen = unseen.minus_sat(&auctioned);

    let mut opponents = Vec::new();
    for (i, p) in view.players.iter().enumerate() {
        if Some(i) == me {
            continue;
        }
        let mut rev = Rack::EMPTY;
        let mut known = Rack::EMPTY;
        for l in 0..NUM_LETTERS {
            rev.0[l] = revealed[i][l].max(0) as u8;
            known.0[l] = (won[i][l] - played[i][l] + revealed[i][l]).max(0) as u8;
        }
        unseen = unseen.minus_sat(&rev);
        let hidden = view.settings.starting_rack_size.saturating_sub(rev.total());
        opponents.push(Opponent {
            bot_id: p.bot_id,
            known,
            hidden,
            balance: p.balance,
            score: p.score,
            is_sim: p.sim.is_some(),
        });
    }
    PoolInfo { unseen, my_start, opponents }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::rules::letter_index;
    use crate::game::view::{AuctionRecord, MatchSettings, OpenAuction, Phase, PlayRecord, PlayerInfo};

    #[test]
    fn revealed_start_uses_running_max() {
        let e = letter_index('E').unwrap();
        let players = vec![
            PlayerInfo { bot_id: 1, balance: 100, score: 0, sim: None },
            PlayerInfo { bot_id: 2, balance: 100, score: 0, sim: None },
        ];
        // Bot 2 plays "EE" (wait: needs a real word; pool math ignores validity)
        // after zero wins, then wins an E: it must have started with both E's.
        let plays = vec![PlayRecord { bot_id: 2, word: "EE".into(), reward: 2, after_results: 0 }];
        let results = vec![AuctionRecord { auction_id: 1, letter: e, winner: Some(2), paid: 1 }];
        let view = MatchView {
            match_id: 1,
            my_bot_id: 1,
            settings: MatchSettings::default(),
            phase: Phase::Lobby,
            players: &players,
            my_rack: Rack::from_letters("ABCDF"),
            bag_total: 10,
            open: Some(OpenAuction { auction_id: 2, letter: e, index: 1, closes_at_ms: 0 }),
            results: &results,
            plays: &plays,
            ended: false,
            now_ms: 0,
        };
        let info = analyze(&view);
        let opp = &info.opponents[0];
        assert_eq!(opp.hidden, 3);
        assert_eq!(opp.known.count(e), 1);
        // 12 E's - 2 revealed - 1 auctioned - 1 open = 8 unseen.
        assert_eq!(info.unseen.count(e), 8);
        assert_eq!(info.my_start, Rack::from_letters("ABCDF"));
    }
}
