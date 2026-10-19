//! Spelling correction for the partial word.
//!
//! [`corrections`] scans the n-gram vocabulary, the static lexicon and
//! the learned words for entries whose spelling (or whose leading part,
//! so a misspelled word can still be completed) lies within a small
//! weighted edit distance of what was typed. Costs follow how typing
//! errors actually happen on a QWERTY grid:
//!
//! - hitting a neighboring key costs half a plain substitution;
//! - a doubled or dropped repeated letter (`untill`, `ocur`) is cheap;
//! - swapping two adjacent letters (`teh`) counts as one edit;
//! - a wrong first letter costs extra, since it is rarely mistyped.
//!
//! Costs are in tenths of an edit.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::ngm::model;
use crate::words::WORDS;

/// Shortest partial that gets corrections; shorter ones are too
/// ambiguous to guess.
pub const MIN_LEN: usize = 3;
/// Candidates kept per scan, cheapest first.
const MAX_CANDIDATES: usize = 40;

const SUB: u16 = 10;
const SUB_NEAR: u16 = 5;
const INDEL: u16 = 10;
const INDEL_DOUBLE: u16 = 4;
const SWAP: u16 = 5;
const FIRST_LETTER: u16 = 5;

/// Largest correction cost accepted for a partial of `len` chars.
pub fn max_cost(len: usize) -> u16 {
    match len {
        0..=2 => 0,
        3..=4 => 10,
        5..=7 => 18,
        _ => 25,
    }
}

/// Grid position of a letter on QWERTY, rows offset like the keycaps.
fn key_pos(c: char) -> Option<(f32, f32)> {
    const ROWS: [(&str, f32); 3] = [("qwertyuiop", 0.0), ("asdfghjkl", 0.25), ("zxcvbnm", 0.75)];
    ROWS.iter().enumerate().find_map(|(y, (row, off))| {
        row.find(c).map(|x| (x as f32 + off, y as f32))
    })
}

/// Which letter pairs are neighboring keys, indexed by `a - 'a'`.
static NEAR: LazyLock<[[bool; 26]; 26]> = LazyLock::new(|| {
    let mut t = [[false; 26]; 26];
    for (i, a) in ('a'..='z').enumerate() {
        for (j, b) in ('a'..='z').enumerate() {
            if let (Some((ax, ay)), Some((bx, by))) = (key_pos(a), key_pos(b)) {
                let (dx, dy) = (ax - bx, ay - by);
                t[i][j] = dx * dx + dy * dy <= 1.6;
            }
        }
    }
    t
});

fn near(a: u8, b: u8) -> bool {
    let idx = |c: u8| c.is_ascii_lowercase().then(|| (c - b'a') as usize);
    match (idx(a), idx(b)) {
        (Some(i), Some(j)) => NEAR[i][j],
        _ => false,
    }
}

/// Longest partial the scan handles; the predictor caps words at 31.
const MAX_TYPED: usize = 31;

/// Cost of turning `typed` into a leading part of `word`, or into all
/// of it, whichever is cheaper. `None` once the cost exceeds `limit`.
/// The flag is true when the cheapest match spans the whole word.
pub fn cost(typed: &[u8], word: &[u8], limit: u16) -> Option<(u16, bool)> {
    let n = typed.len();
    if n == 0
        || n > MAX_TYPED
        || word.is_empty()
        || word.len() + usize::from(limit / INDEL_DOUBLE) < n
    {
        return None;
    }
    // Three rolling columns of the edit table: costs of typed[..i]
    // against word[..j], word[..j - 1] and word[..j - 2].
    let mut col = [0u16; MAX_TYPED + 1];
    let mut prev = [0u16; MAX_TYPED + 1];
    let mut prev2 = [0u16; MAX_TYPED + 1];
    for i in 1..=n {
        let first = if i == 1 { FIRST_LETTER } else { 0 };
        prev[i] = prev[i - 1] + del_cost(typed, i - 1, word, 0) + first;
    }
    let mut best: Option<(u16, bool)> = None;
    let mut over = 0;
    for j in 1..=word.len() {
        let b = word[j - 1];
        let first = if j == 1 { FIRST_LETTER } else { 0 };
        col[0] = prev[0] + ins_cost(word, j - 1) + first;
        let mut min = col[0];
        for i in 1..=n {
            let a = typed[i - 1];
            let sub = if a == b {
                0
            } else {
                let base = if near(a, b) { SUB_NEAR } else { SUB };
                if i == 1 && j == 1 {
                    base + FIRST_LETTER
                } else {
                    base
                }
            };
            let mut v = prev[i - 1] + sub;
            v = v.min(col[i - 1] + del_cost(typed, i - 1, word, j));
            v = v.min(prev[i] + ins_cost(word, j - 1));
            if i >= 2 && j >= 2 && a == word[j - 2] && typed[i - 2] == b {
                v = v.min(prev2[i - 2] + SWAP);
            }
            col[i] = v;
            min = min.min(v);
        }
        let end = col[n];
        if end <= limit {
            let whole = j == word.len();
            match best {
                Some((c, _)) if c < end => {}
                Some((c, _)) if c == end && !whole => {}
                _ => best = Some((end, whole)),
            }
        }
        // A swap reaches back two columns, so the scan ends once two
        // columns in a row are entirely past the limit.
        if min > limit {
            over += 1;
            if over >= 2 {
                break;
            }
        } else {
            over = 0;
        }
        prev2 = prev;
        prev = col;
    }
    best
}

/// Cost of the typed letter at `i` having no counterpart in the word:
/// cheap when it repeats its neighbor (a doubled key press).
fn del_cost(typed: &[u8], i: usize, word: &[u8], j: usize) -> u16 {
    let c = typed[i];
    let doubled = (i > 0 && typed[i - 1] == c) || (j > 0 && word[j - 1] == c);
    if doubled {
        INDEL_DOUBLE
    } else {
        INDEL
    }
}

/// Cost of the word letter at `j` not having been typed: cheap when
/// it is the second of a double letter.
fn ins_cost(word: &[u8], j: usize) -> u16 {
    if j > 0 && word[j - 1] == word[j] {
        INDEL_DOUBLE
    } else {
        INDEL
    }
}

/// Whether `word` can start the way `typed` does: same first letter, a
/// neighboring key, or the first two letters swapped. Anything else
/// pays the first-letter penalty on top of an edit and is skipped
/// before the full scan.
fn plausible_start(typed: &[u8], word: &[u8]) -> bool {
    let (Some(&t0), Some(&w0)) = (typed.first(), word.first()) else {
        return false;
    };
    t0 == w0 || near(t0, w0) || typed.get(1) == Some(&w0)
}

/// The correction dictionary: the model vocabulary, then the lexicon
/// words it lacks, each once; plus the same words as a set.
static DICT: LazyLock<(Vec<&'static str>, HashSet<&'static str>)> = LazyLock::new(|| {
    let m = model();
    let mut seen = HashSet::new();
    let words = (0..u16::MAX)
        .map_while(|id| m.word(id))
        .chain(WORDS.iter().copied())
        .filter(|w| seen.insert(*w))
        .collect();
    (words, seen)
});

/// A correction candidate for the partial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Correction {
    pub word: &'static str,
    /// Edit cost in tenths of an edit.
    pub cost: u16,
    /// The match spans the whole word (a correction rather than a
    /// corrected completion).
    pub whole: bool,
}

/// Words that `partial` looks like a misspelling of, or a misspelled
/// beginning of, cheapest first. Words that `partial` is already a
/// prefix of are left to the completion pass. `extra` adds learned
/// words to the scan. When `known` (the partial is itself a word),
/// only a single letter swap is offered, so a correctly spelled word
/// is not crowded out by look-alikes while `form` still suggests `from`.
pub fn corrections<'a>(
    partial: &str,
    known: bool,
    extra: impl IntoIterator<Item = &'a &'static str>,
) -> Vec<Correction> {
    let typed = partial.as_bytes();
    let mut limit = max_cost(typed.len());
    if known {
        limit = limit.min(SWAP);
    }
    if limit == 0 {
        return Vec::new();
    }
    let known: &HashSet<&'static str> = &DICT.1;
    let extra = extra.into_iter().copied().filter(|w| !known.contains(w));
    let mut out: Vec<Correction> = Vec::new();
    for w in DICT.0.iter().copied().chain(extra) {
        let wb = w.as_bytes();
        if w.starts_with(partial) || !plausible_start(typed, wb) {
            continue;
        }
        if let Some((cost, whole)) = cost(typed, wb, limit) {
            out.push(Correction { word: w, cost, whole });
        }
    }
    out.sort_by_key(|c| (c.cost, !c.whole));
    out.truncate(MAX_CANDIDATES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(typed: &str, word: &str) -> Option<(u16, bool)> {
        cost(typed.as_bytes(), word.as_bytes(), 30)
    }

    #[test]
    fn swap_is_one_edit() {
        assert_eq!(c("teh", "the"), Some((SWAP, true)));
    }

    #[test]
    fn neighbor_key_is_cheap() {
        assert_eq!(c("hellp", "hello"), Some((SUB_NEAR, true)));
        assert_eq!(c("hezlo", "hello"), Some((SUB, true)));
    }

    #[test]
    fn double_letters_are_cheap() {
        assert_eq!(c("untill", "until"), Some((INDEL_DOUBLE, true)));
        assert_eq!(c("ocur", "occur"), Some((INDEL_DOUBLE, true)));
    }

    #[test]
    fn misspelled_prefix_matches() {
        // "recie" is a swapped "recei", the start of "receive".
        assert_eq!(c("recie", "receive"), Some((SWAP, false)));
    }

    #[test]
    fn far_words_are_rejected() {
        assert_eq!(cost(b"zzzz", b"hello", max_cost(4)), None);
    }

    #[test]
    fn known_words_only_get_swaps() {
        let got = corrections("form", true, std::iter::empty());
        assert!(got.iter().any(|c| c.word == "from"), "{got:?}");
        assert!(got.iter().all(|c| c.cost <= SWAP), "{got:?}");
    }

    #[test]
    fn short_partials_get_nothing() {
        assert!(corrections("te", false, std::iter::empty()).is_empty());
    }

    #[test]
    fn finds_common_fixes() {
        for (typed, want) in [("teh", "the"), ("recieve", "receive"), ("becuase", "because"), ("wierd", "weird")] {
            let got = corrections(typed, false, std::iter::empty());
            assert!(got.iter().any(|c| c.word == want), "{typed}: {got:?}");
        }
    }
}
