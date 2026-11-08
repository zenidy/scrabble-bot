//! The exact game rules, ported from the event module (`letters.ts`, `game.ts`).
//!
//! Letters are indices 0..26 (`A` = 0). A [`Rack`] is a 26-letter count vector
//! padded to 32 bytes so fit checks vectorize.

use std::fmt;

pub const NUM_LETTERS: usize = 26;
pub const BAG_SIZE: u32 = 98;
pub const STARTING_BALANCE: i64 = 100;
pub const AUCTION_RESERVE: i64 = 1;
pub const SIM_BID_JITTER: i64 = 2;
pub const DEFAULT_RACK_SIZE: u32 = 5;
pub const DEFAULT_AUCTION_MS: u64 = 1000;
pub const DEFAULT_LOBBY_SIZE: u32 = 6;

/// Standard Scrabble distribution without blanks (`DEFAULT_BAG`): 98 tiles.
pub const BAG: [u8; NUM_LETTERS] = [
    9, 2, 2, 4, 12, 2, 3, 2, 9, 1, 1, 4, 2, 6, 8, 2, 1, 6, 4, 6, 4, 2, 2, 1, 2, 1,
];

/// `letterValue`: AEIOULNSTR=1, DG=2, BCMP=3, FHVWY=4, K=5, JX=8, QZ=10.
pub const VALUES: [u8; NUM_LETTERS] = [
    1, 3, 3, 2, 1, 4, 2, 4, 1, 8, 5, 1, 3, 1, 1, 3, 10, 1, 1, 1, 1, 4, 4, 8, 4, 10,
];

pub const VOWELS: [bool; NUM_LETTERS] = {
    let mut v = [false; NUM_LETTERS];
    v[0] = true; // A
    v[4] = true; // E
    v[8] = true; // I
    v[14] = true; // O
    v[20] = true; // U
    v
};

#[inline]
pub fn value(letter: u8) -> u32 {
    VALUES[letter as usize] as u32
}

#[inline]
pub fn is_vowel(letter: u8) -> bool {
    VOWELS[letter as usize]
}

#[inline]
pub fn letter_index(c: char) -> Option<u8> {
    if c.is_ascii_uppercase() {
        Some(c as u8 - b'A')
    } else {
        None
    }
}

#[inline]
pub fn letter_char(letter: u8) -> char {
    (b'A' + letter) as char
}

/// `lengthMultiplier`: (num, den) with reward = floor(base * num / den).
#[inline]
pub fn length_multiplier(len: usize) -> (u32, u32) {
    match len {
        0..=3 => (1, 1),
        4 => (3, 2),
        5 => (2, 1),
        6 => (5, 2),
        _ => (3, 1),
    }
}

#[inline]
pub fn reward(base: u32, len: usize) -> u32 {
    let (num, den) = length_multiplier(len);
    base * num / den
}

/// Reward of an uppercase A-Z word (assumes it is valid).
pub fn word_reward(word: &str) -> u32 {
    let base: u32 = word.bytes().map(|b| value(b - b'A')).sum();
    reward(base, word.len())
}

/// Auctions in a match: every tile left after the starting racks.
pub fn auctions_in_match(players: u32, starting_rack_size: u32) -> u32 {
    BAG_SIZE.saturating_sub(players * starting_rack_size)
}

/// Sim bot strategies (`BotStrategy` minus `Human`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SimStrategy {
    Cheapskate,
    ValueBidder,
    Aggressive,
}

impl SimStrategy {
    /// The strategy's base bid before jitter.
    pub fn base_bid(self, letter: u8) -> i64 {
        let v = value(letter) as i64;
        match self {
            SimStrategy::Cheapskate => v - 1,
            SimStrategy::ValueBidder => v,
            SimStrategy::Aggressive => v + if is_vowel(letter) { 4 } else { 2 },
        }
    }

    /// `decideBid`: max(min(max(base + jitter, 1), balance), 0).
    pub fn bid(self, letter: u8, balance: i64, jitter: i64) -> i64 {
        (self.base_bid(letter) + jitter).max(1).min(balance).max(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AuctionType {
    FirstPrice,
    Vickrey,
}

/// Letter counts, padded to 32 lanes (lanes 26..32 stay zero).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rack(pub [u8; 32]);

impl Rack {
    pub const EMPTY: Rack = Rack([0; 32]);

    pub fn full_bag() -> Rack {
        let mut r = Rack::EMPTY;
        r.0[..NUM_LETTERS].copy_from_slice(&BAG);
        r
    }

    /// Parses A-Z letters (lowercase accepted); other characters are ignored.
    pub fn from_letters(s: &str) -> Rack {
        let mut r = Rack::EMPTY;
        for c in s.chars() {
            if let Some(l) = letter_index(c.to_ascii_uppercase()) {
                r.0[l as usize] += 1;
            }
        }
        r
    }

    #[inline]
    pub fn count(&self, letter: u8) -> u8 {
        self.0[letter as usize]
    }

    #[inline]
    pub fn add(&mut self, letter: u8) {
        self.0[letter as usize] += 1;
    }

    #[inline]
    pub fn remove(&mut self, letter: u8) {
        debug_assert!(self.0[letter as usize] > 0);
        self.0[letter as usize] -= 1;
    }

    #[inline]
    pub fn total(&self) -> u32 {
        self.0.iter().map(|&c| c as u32).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|&c| c == 0)
    }

    /// Sum of letter values.
    #[inline]
    pub fn base(&self) -> u32 {
        let mut s = 0u32;
        for i in 0..NUM_LETTERS {
            s += self.0[i] as u32 * VALUES[i] as u32;
        }
        s
    }

    /// Whether `other` fits inside `self` (multiset inclusion).
    #[inline]
    pub fn contains(&self, other: &Rack) -> bool {
        let mut ok = true;
        for i in 0..32 {
            ok &= other.0[i] <= self.0[i];
        }
        ok
    }

    /// `self - other`; caller guarantees `self.contains(other)`.
    #[inline]
    pub fn minus(&self, other: &Rack) -> Rack {
        let mut r = *self;
        for i in 0..32 {
            r.0[i] -= other.0[i];
        }
        r
    }

    /// Saturating difference.
    pub fn minus_sat(&self, other: &Rack) -> Rack {
        let mut r = *self;
        for i in 0..32 {
            r.0[i] = r.0[i].saturating_sub(other.0[i]);
        }
        r
    }

    #[inline]
    pub fn plus(&self, other: &Rack) -> Rack {
        let mut r = *self;
        for i in 0..32 {
            r.0[i] += other.0[i];
        }
        r
    }

    /// Nibble-packed key (every count is <= 15).
    #[inline]
    pub fn key(&self) -> u128 {
        let mut k = 0u128;
        for i in 0..NUM_LETTERS {
            k |= ((self.0[i] & 15) as u128) << (4 * i);
        }
        k
    }

    /// Letters in alphabetical order, one per tile.
    pub fn letters(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.total() as usize);
        for i in 0..NUM_LETTERS {
            for _ in 0..self.0[i] {
                v.push(i as u8);
            }
        }
        v
    }
}

impl fmt::Display for Rack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for l in self.letters() {
            write!(f, "{}", letter_char(l))?;
        }
        Ok(())
    }
}

impl fmt::Debug for Rack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rack({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bag_and_values() {
        assert_eq!(BAG.iter().map(|&c| c as u32).sum::<u32>(), 98);
        let face: u32 = (0..26).map(|i| BAG[i] as u32 * VALUES[i] as u32).sum();
        assert_eq!(face, 187);
        assert_eq!(value(letter_index('Q').unwrap()), 10);
        assert_eq!(value(letter_index('K').unwrap()), 5);
        assert_eq!(value(letter_index('D').unwrap()), 2);
    }

    #[test]
    fn multipliers_match_module() {
        assert_eq!(word_reward("QUIZ"), 33);
        assert_eq!(word_reward("AXES"), 16);
        assert_eq!(word_reward("QUAKE"), 36);
        assert_eq!(word_reward("ZEBRAS"), 42);
        assert_eq!(word_reward("JUKEBOX"), 81);
        assert_eq!(word_reward("CAT"), 5);
    }

    #[test]
    fn sim_bids_match_module() {
        let e = letter_index('E').unwrap();
        let q = letter_index('Q').unwrap();
        use SimStrategy::*;
        assert_eq!(Cheapskate.bid(e, 100, 0), 1);
        assert_eq!(Cheapskate.bid(q, 100, 0), 9);
        assert_eq!(ValueBidder.bid(q, 100, 0), 10);
        assert_eq!(Aggressive.bid(e, 100, 0), 5);
        assert_eq!(Aggressive.bid(q, 100, 0), 12);
        assert_eq!(ValueBidder.bid(q, 100, 2), 12);
        assert_eq!(ValueBidder.bid(q, 100, -2), 8);
        assert_eq!(Cheapskate.bid(e, 100, -2), 1);
        assert_eq!(Aggressive.bid(q, 11, 2), 11);
        assert_eq!(Aggressive.bid(q, 7, 0), 7);
        assert_eq!(ValueBidder.bid(q, 0, 0), 0);
        assert_eq!(ValueBidder.bid(q, -5, 0), 0);
    }

    #[test]
    fn rack_ops() {
        let r = Rack::from_letters("CATT");
        assert_eq!(r.total(), 4);
        assert!(r.contains(&Rack::from_letters("TAT")));
        assert!(!r.contains(&Rack::from_letters("CATS")));
        assert_eq!(r.minus(&Rack::from_letters("CAT")).to_string(), "T");
        assert_eq!(auctions_in_match(6, 5), 68);
    }
}
