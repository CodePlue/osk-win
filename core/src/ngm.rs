//! Embedded n-gram language model for next-word prediction.
//!
//! The model is generated offline from a text corpus (see
//! `tools/gen_ngm.py`) and embedded at compile time. It holds a
//! frequency-ordered vocabulary, a bigram table (context word to its
//! five most likely followers), and a trigram table (two-word context
//! to its three most likely followers). Lookup is a binary search
//! over the sorted context tables.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Followers kept per bigram context.
pub const TOPK_B: usize = 5;
/// Followers kept per trigram context.
pub const TOPK_T: usize = 3;
/// Empty follower slot.
pub const EMPTY: u16 = 0xFFFF;

#[derive(Clone, Copy)]
struct Bigram {
    ctx: u16,
    next: [u16; TOPK_B],
}

#[derive(Clone, Copy)]
struct Trigram {
    ab: u32,
    next: [u16; TOPK_T],
}

pub struct Model {
    words: Vec<&'static str>,
    id: HashMap<&'static str, u16>,
    bigrams: Vec<Bigram>,
    trigrams: Vec<Trigram>,
}

static MODEL: OnceLock<Model> = OnceLock::new();

/// The embedded model, parsed once.
pub fn model() -> &'static Model {
    MODEL.get_or_init(load)
}

fn u16(d: &[u8], p: &mut usize) -> u16 {
    let v = u16::from_le_bytes([d[*p], d[*p + 1]]);
    *p += 2;
    v
}

fn u32(d: &[u8], p: &mut usize) -> u32 {
    let v = u32::from_le_bytes([d[*p], d[*p + 1], d[*p + 2], d[*p + 3]]);
    *p += 4;
    v
}

fn load() -> Model {
    let d: &'static [u8] = include_bytes!("../data/ngm.bin");
    assert!(d.len() > 18 && &d[0..4] == b"OSNG", "ngm: bad magic");
    let mut p = 4;
    let version = u16(d, &mut p);
    assert!(version == 1, "ngm: unsupported version {version}");
    let vocab_size = u32(d, &mut p) as usize;
    let n_bg = u32(d, &mut p) as usize;
    let n_tg = u32(d, &mut p) as usize;

    let words: Vec<&'static str> = (0..vocab_size)
        .map(|_| {
            let len = d[p] as usize;
            p += 1;
            let s = std::str::from_utf8(&d[p..p + len]).expect("ngm: bad word");
            p += len;
            s
        })
        .collect();

    let bigrams = (0..n_bg)
        .map(|_| {
            let ctx = u16(d, &mut p);
            let mut next = [0u16; TOPK_B];
            for s in next.iter_mut() {
                *s = u16(d, &mut p);
            }
            Bigram { ctx, next }
        })
        .collect();

    let trigrams = (0..n_tg)
        .map(|_| {
            let a = u16(d, &mut p);
            let b = u16(d, &mut p);
            let mut next = [0u16; TOPK_T];
            for s in next.iter_mut() {
                *s = u16(d, &mut p);
            }
            Trigram {
                ab: (u32::from(a) << 16) | u32::from(b),
                next,
            }
        })
        .collect();

    let id = words
        .iter()
        .enumerate()
        .map(|(i, w)| (*w, i as u16))
        .collect();

    Model {
        words,
        id,
        bigrams,
        trigrams,
    }
}

impl Model {
    /// Word id for a lowercase word, if it is in the vocabulary.
    pub fn id_of(&self, w: &str) -> Option<u16> {
        self.id.get(w).copied()
    }

    /// Number of words in the vocabulary.
    #[allow(clippy::len_without_is_empty)] // the embedded vocab is never empty
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// The word for an id.
    pub fn word(&self, id: u16) -> Option<&'static str> {
        self.words.get(id as usize).copied()
    }

    /// The most frequent words, for the no-context fallback.
    pub fn top(&self) -> &[&'static str] {
        &self.words[..TOPK_B.min(self.words.len())]
    }

    /// Followers of a one-word context, best first.
    pub fn bigram(&self, ctx: u16) -> Option<&[u16]> {
        self.bigrams
            .binary_search_by_key(&ctx, |b| b.ctx)
            .ok()
            .map(|i| &self.bigrams[i].next[..])
    }

    /// Followers of a two-word context, best first.
    pub fn trigram(&self, a: u16, b: u16) -> Option<&[u16]> {
        let key = (u32::from(a) << 16) | u32::from(b);
        self.trigrams
            .binary_search_by_key(&key, |t| t.ab)
            .ok()
            .map(|i| &self.trigrams[i].next[..])
    }
}
