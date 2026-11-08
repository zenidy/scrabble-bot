//! The server wordlist, indexed by anagram class.
//!
//! Every word with the same letter multiset earns the same reward, so the
//! solver works on classes; each class keeps its alphabetically first word.
//! Classes sit in a trie keyed by their letters sorted rare-first, so a DFS
//! over a rack visits only prefixes the rack can spell.

use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::rules::{NUM_LETTERS, Rack, letter_index, reward, value};

/// sha256 of `spacetimedb/wordlist.txt` at the pinned event commit.
pub const WORDLIST_SHA256: &str =
    "b819e7b67f8f2c80640ae76f0edbe59a4a1befb6de0f348463642ad49bdd2ad3";
pub const WORDLIST_WORDS: usize = 172_820;

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub struct Class {
    pub counts: Rack,
    pub len: u8,
    pub base: u16,
    pub reward: u16,
    /// `3 * base - reward`: how far below the 3x ceiling this class scores.
    pub loss: u16,
    /// Index into `Dict::words` of the class's alphabetically first word.
    pub word: u32,
    /// The class's first letter in decision order (its rarest letter).
    pub first: u8,
    /// Bit `l` set when the class contains letter `l`.
    pub mask: u32,
}

#[derive(Clone, Copy)]
struct Node {
    first_edge: u32,
    n_edges: u8,
    class: u32,
}

pub struct Dict {
    words: Vec<String>,
    pub classes: Vec<Class>,
    nodes: Vec<Node>,
    edges: Vec<(u8, u32)>,
    /// Letters rare-first (by frequency in the wordlist).
    pub order: [u8; NUM_LETTERS],
    /// `rank[letter]` = position of `letter` in `order`.
    pub rank: [u8; NUM_LETTERS],
}

impl Dict {
    /// Loads the wordlist and checks its sha256 against the pinned hash.
    pub fn load(path: &Path) -> Result<Dict> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading wordlist {}", path.display()))?;
        let digest = Sha256::digest(&bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        if hex != WORDLIST_SHA256 {
            bail!(
                "wordlist {} has sha256 {hex}, expected {WORDLIST_SHA256}; copy spacetimedb/wordlist.txt from the pinned event commit",
                path.display()
            );
        }
        let text = String::from_utf8(bytes).context("wordlist is not UTF-8")?;
        let dict = Dict::from_text(&text)?;
        if dict.words.len() != WORDLIST_WORDS {
            bail!("wordlist has {} words, expected {WORDLIST_WORDS}", dict.words.len());
        }
        Ok(dict)
    }

    /// Finds `data/wordlist.txt` from the working directory, the crate root or
    /// next to the executable, or `WORDLIST` if set.
    pub fn load_default() -> Result<Dict> {
        if let Ok(p) = std::env::var("WORDLIST") {
            return Dict::load(Path::new(&p));
        }
        let mut candidates = vec![
            std::path::PathBuf::from("data/wordlist.txt"),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("data/wordlist.txt"),
        ];
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("data/wordlist.txt"));
                candidates.push(dir.join("../data/wordlist.txt"));
            }
        }
        for c in &candidates {
            if c.exists() {
                return Dict::load(c);
            }
        }
        bail!("data/wordlist.txt not found (set WORDLIST=<path>)")
    }

    /// Builds the index from raw wordlist text without the hash check (tests).
    pub fn from_text(text: &str) -> Result<Dict> {
        let mut words: Vec<String> = Vec::new();
        for line in text.lines() {
            let w = line.trim();
            if w.is_empty() {
                continue;
            }
            if !w.bytes().all(|b| b.is_ascii_uppercase()) {
                bail!("wordlist entry {w:?} is not A-Z");
            }
            words.push(w.to_string());
        }
        words.sort();
        words.dedup();

        // Rare-first letter order by frequency in the wordlist.
        let mut freq = [0u64; NUM_LETTERS];
        for w in &words {
            for b in w.bytes() {
                freq[(b - b'A') as usize] += 1;
            }
        }
        let mut order: [u8; NUM_LETTERS] = std::array::from_fn(|i| i as u8);
        order.sort_by_key(|&l| (freq[l as usize], l));
        let mut rank = [0u8; NUM_LETTERS];
        for (i, &l) in order.iter().enumerate() {
            rank[l as usize] = i as u8;
        }

        // Group into anagram classes keyed by rank-sorted letters. Words are
        // in lex order, so the first word seen for a class is its smallest.
        let mut keyed: Vec<(Vec<u8>, u32)> = words
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let mut k: Vec<u8> = w.bytes().map(|b| rank[(b - b'A') as usize]).collect();
                k.sort_unstable();
                (k, i as u32)
            })
            .collect();
        keyed.sort();
        let mut classes: Vec<Class> = Vec::new();
        let mut class_keys: Vec<Vec<u8>> = Vec::new();
        for (k, wi) in keyed {
            if class_keys.last() == Some(&k) {
                continue; // same class, later (larger) word
            }
            let w = &words[wi as usize];
            let counts = Rack::from_letters(w);
            let base: u32 = w.bytes().map(|b| value(b - b'A')).sum();
            let r = reward(base, w.len());
            classes.push(Class {
                counts,
                len: w.len() as u8,
                base: base as u16,
                reward: r as u16,
                loss: (3 * base - r) as u16,
                word: wi,
                first: order[k[0] as usize],
                mask: w.bytes().fold(0u32, |m, b| m | 1 << (b - b'A')),
            });
            class_keys.push(k);
        }

        // Trie over the rank-sorted keys (already sorted).
        struct B {
            children: Vec<(u8, u32)>,
            class: u32,
        }
        let mut b: Vec<B> = vec![B { children: vec![], class: NONE }];
        for (ci, key) in class_keys.iter().enumerate() {
            let mut n = 0usize;
            for &r in key {
                let letter = order[r as usize];
                let next = match b[n].children.last() {
                    Some(&(l, c)) if l == letter => c as usize,
                    _ => {
                        let id = b.len();
                        b.push(B { children: vec![], class: NONE });
                        b[n].children.push((letter, id as u32));
                        id
                    }
                };
                n = next;
            }
            b[n].class = ci as u32;
        }
        let mut nodes = Vec::with_capacity(b.len());
        let mut edges = Vec::new();
        for node in &b {
            nodes.push(Node {
                first_edge: edges.len() as u32,
                n_edges: node.children.len() as u8,
                class: node.class,
            });
            edges.extend_from_slice(&node.children);
        }

        Ok(Dict { words, classes, nodes, edges, order, rank })
    }

    /// Leaks the dictionary for the life of the process (strategies hold it).
    pub fn leak(self) -> &'static Dict {
        Box::leak(Box::new(self))
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn word(&self, index: u32) -> &str {
        &self.words[index as usize]
    }

    /// The class's representative (alphabetically first) word.
    pub fn class_word(&self, class: u32) -> &str {
        self.word(self.classes[class as usize].word)
    }

    /// Server-side validity: 2+ letters, A-Z after uppercasing a-z, listed.
    pub fn is_valid(&self, word: &str) -> bool {
        let up = word.to_ascii_uppercase();
        if up.len() < 2 || !up.bytes().all(|b| b.is_ascii_uppercase()) {
            return false;
        }
        self.words.binary_search(&up).is_ok()
    }

    /// Calls `f` for every class of at least `min_len` letters that `rack` can spell.
    pub fn for_each_fit(&self, rack: &Rack, min_len: usize, mut f: impl FnMut(u32)) {
        let mut r = *rack;
        self.dfs(0, 0, &mut r, min_len, &mut f);
    }

    fn dfs(&self, node: u32, depth: usize, rack: &mut Rack, min_len: usize, f: &mut impl FnMut(u32)) {
        let n = self.nodes[node as usize];
        if n.class != NONE && depth >= min_len {
            f(n.class);
        }
        let start = n.first_edge as usize;
        for &(letter, child) in &self.edges[start..start + n.n_edges as usize] {
            let l = letter as usize;
            if rack.0[l] > 0 {
                rack.0[l] -= 1;
                self.dfs(child, depth + 1, rack, min_len, f);
                rack.0[l] += 1;
            }
        }
    }

    /// `findBestPlayable`: the longest word of at least `min_len` letters the
    /// rack can spell, ties going to the alphabetically first word.
    pub fn best_playable(&self, rack: &Rack, min_len: usize) -> Option<u32> {
        let mut best: Option<(u8, u32, u32)> = None; // (len, word index, class)
        self.for_each_fit(rack, min_len, |c| {
            let cl = &self.classes[c as usize];
            let better = match best {
                None => true,
                Some((len, wi, _)) => cl.len > len || (cl.len == len && cl.word < wi),
            };
            if better {
                best = Some((cl.len, cl.word, c));
            }
        });
        best.map(|(_, _, c)| c)
    }

    /// Validates `word` and returns its letter counts.
    pub fn word_counts(&self, word: &str) -> Option<Rack> {
        if !self.is_valid(word) {
            return None;
        }
        Some(Rack::from_letters(&word.to_ascii_uppercase()))
    }

    /// Letter index helper for callers holding chars.
    pub fn letter(c: char) -> Option<u8> {
        letter_index(c.to_ascii_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> Dict {
        Dict::from_text("ACT\nCAT\nAA\nTACT\nQUIZ\nZA\nAT\nTA\n").unwrap()
    }

    #[test]
    fn classes_group_anagrams() {
        let d = tiny();
        let act: Vec<_> = d.classes.iter().filter(|c| c.counts == Rack::from_letters("CAT")).collect();
        assert_eq!(act.len(), 1);
        assert_eq!(d.word(act[0].word), "ACT");
        assert_eq!(act[0].reward, 5);
    }

    #[test]
    fn fits_and_best_playable() {
        let d = tiny();
        let mut fits = vec![];
        d.for_each_fit(&Rack::from_letters("TACT"), 2, |c| fits.push(d.class_word(c).to_string()));
        fits.sort();
        assert_eq!(fits, vec!["ACT", "AT", "TACT"]);
        let best = d.best_playable(&Rack::from_letters("TAC"), 3).unwrap();
        assert_eq!(d.class_word(best), "ACT");
        assert!(d.best_playable(&Rack::from_letters("AA"), 3).is_none());
        assert_eq!(d.class_word(d.best_playable(&Rack::from_letters("AA"), 2).unwrap()), "AA");
    }

    #[test]
    fn validity_rules() {
        let d = tiny();
        assert!(d.is_valid("cat"));
        assert!(d.is_valid("QUIZ"));
        assert!(!d.is_valid("C"));
        assert!(!d.is_valid("CA T"));
        assert!(!d.is_valid("CAT1"));
        assert!(!d.is_valid("XQZ"));
    }
}
