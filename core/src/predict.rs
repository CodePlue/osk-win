//! Next-word prediction using Google Keyboard (Gboard) style interpolated
//! n-gram language modeling with dynamic user learning.
//!
//! Candidates are scored using an interpolated Bayesian Katz-backoff formula:
//! Score(w) = α·User(w|ctx) + β·Trigram(w|prev,last) + γ·Bigram(w|last) + δ·Unigram(w) + Bonus
//!
//! Features:
//! 1. Dynamic user adaptation: tracks recently and frequently committed words & bigrams.
//! 2. Out-of-vocabulary learning: any committed word is interned to a canonical
//!    `&'static str` and learned, even if it is absent from the static lexicon.
//! 3. Multi-tier context interpolation: trigram -> bigram -> unigram backoff with rank decay.
//! 4. Context-aware prefix completion: contextual followers are prioritized over generic lexicon.
//! 5. Brevity weighting, exact-match bonus, and a stem penalty that diversifies the pills.
//! 6. Disk persistence of the user history (TSV under the XDG data home).
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use parking_lot::RwLock;

use crate::ngm::{model, EMPTY};
use crate::words::WORDS;

/// Maximum suggestions shown.
pub const MAX_SUGG: usize = 5;
/// Maximum tracked word length.
pub const WORD_LEN: usize = 31;
/// Maximum entries in dynamic user bigram cache.
const MAX_USER_BIGRAMS: usize = 2048;
/// Maximum entries in dynamic user unigram vocabulary.
const MAX_USER_WORDS: usize = 1024;
/// Common openers offered at a sentence start or with no context.
const SENTENCE_STARTERS: &[&str] = &["i", "the", "we", "how", "what", "yes", "thanks"];
/// Maximum length of the running sentence context sent to the neural
/// engine (the tail of the sentence is kept).
const MAX_CONTEXT: usize = 2048;

/// Process-wide cache of interned strings: words outside the static
/// vocabulary get one canonical `&'static str` each. The leak is
/// deliberate and bounded: one small allocation per distinct word the
/// user commits, capped by the history size.
static INTERNED: LazyLock<RwLock<HashSet<&'static str>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Return a canonical `'static` copy of `s`, reusing an existing entry
/// when the same word was already interned.
pub fn intern(s: &str) -> &'static str {
    let set = INTERNED.read();
    if let Some(w) = set.iter().find(|w| **w == s) {
        return *w;
    }
    drop(set);
    let mut set = INTERNED.write();
    if let Some(w) = set.iter().find(|w| **w == s) {
        return *w;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// A learned unigram as listed by [`Predictor::learned_words`]: the
/// word, its commit count, and how many learned bigrams it appears in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LearnedWord {
    pub word: &'static str,
    pub count: u16,
    pub links: usize,
}

#[derive(Clone, Debug, Default)]
struct UserHistory {
    /// (prev_word, next_word, count)
    bigrams: Vec<(&'static str, &'static str, u16)>,
    /// (word, count)
    unigrams: Vec<(&'static str, u16)>,
    /// Where this history persists; `None` = ephemeral.
    path: Option<PathBuf>,
}

impl UserHistory {
    /// `$XDG_DATA_HOME/osk/user_history.tsv`, else
    /// `~/.local/share/osk/user_history.tsv`.
    fn default_path() -> PathBuf {
        if let Some(appdata) = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA")) {
            let p = PathBuf::from(appdata);
            if p.is_absolute() {
                return p.join("osk").join("user_history.tsv");
            }
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            let p = PathBuf::from(profile);
            if p.is_absolute() {
                return p.join("AppData").join("Roaming").join("osk").join("user_history.tsv");
            }
        }
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            let p = PathBuf::from(xdg);
            if p.is_absolute() {
                return p.join("osk").join("user_history.tsv");
            }
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join(".local/share/osk/user_history.tsv")
    }

    fn record(&mut self, prev: Option<&'static str>, word: &'static str) {
        self.add_unigram(word, 1);
        if let Some(p) = prev {
            self.add_bigram(p, word, 1);
        }
    }

    fn add_unigram(&mut self, word: &'static str, count: u16) {
        if let Some(pos) = self.unigrams.iter().position(|&(w, _)| w == word) {
            self.unigrams[pos].1 = self.unigrams[pos].1.saturating_add(count);
        } else {
            if self.unigrams.len() >= MAX_USER_WORDS {
                if let Some((min_idx, _)) = self
                    .unigrams
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, &(_, c))| c)
                {
                    self.unigrams.remove(min_idx);
                }
            }
            self.unigrams.push((word, count));
        }
    }

    fn add_bigram(&mut self, a: &'static str, b: &'static str, count: u16) {
        if let Some(pos) = self
            .bigrams
            .iter()
            .position(|&(pa, pb, _)| pa == a && pb == b)
        {
            self.bigrams[pos].2 = self.bigrams[pos].2.saturating_add(count);
        } else {
            if self.bigrams.len() >= MAX_USER_BIGRAMS {
                if let Some((min_idx, _)) = self
                    .bigrams
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, &(_, _, c))| c)
                {
                    self.bigrams.remove(min_idx);
                }
            }
            self.bigrams.push((a, b, count));
        }
    }

    fn user_bigram_followers(&self, prev: &str) -> Vec<(&'static str, u16)> {
        self.bigrams
            .iter()
            .filter(|(a, _, _)| *a == prev)
            .map(|(_, b, c)| (*b, *c))
            .collect()
    }

    fn user_unigram_count(&self, word: &str) -> u16 {
        self.unigrams
            .iter()
            .find(|(w, _)| *w == word)
            .map(|(_, c)| *c)
            .unwrap_or(0)
    }

    /// The learned unigrams with their bigram link counts, most
    /// frequent first, ties broken alphabetically.
    fn learned(&self) -> Vec<LearnedWord> {
        let mut out: Vec<LearnedWord> = self
            .unigrams
            .iter()
            .map(|(w, c)| {
                let links = self
                    .bigrams
                    .iter()
                    .filter(|(a, b, _)| *a == *w || *b == *w)
                    .count();
                LearnedWord {
                    word: *w,
                    count: *c,
                    links,
                }
            })
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.word.cmp(b.word)));
        out
    }

    /// Remove `word` and every bigram that touches it; true if
    /// anything was removed.
    fn forget(&mut self, word: &str) -> bool {
        let had_unigram = self
            .unigrams
            .iter()
            .position(|(w, _)| *w == word)
            .is_some();
        self.unigrams.retain(|(w, _)| *w != word);
        let bigrams_before = self.bigrams.len();
        self.bigrams.retain(|(a, b, _)| *a != word && *b != word);
        had_unigram || self.bigrams.len() != bigrams_before
    }

    /// Add `count` to `word`'s unigram frequency.
    fn learn(&mut self, word: &'static str, count: u16) {
        self.add_unigram(word, count);
    }

    /// Drop all learned unigrams and bigrams.
    fn clear(&mut self) {
        self.unigrams.clear();
        self.bigrams.clear();
    }

    /// Write the history as TSV lines:
    /// `u \t word \t <empty> \t count` and `b \t prev \t next \t count`.
    fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut out = String::new();
        for (w, c) in &self.unigrams {
            out.push('u');
            out.push('\t');
            out.push_str(w);
            out.push('\t');
            out.push('\t');
            out.push_str(&c.to_string());
            out.push('\n');
        }
        for (a, b, c) in &self.bigrams {
            out.push('b');
            out.push('\t');
            out.push_str(a);
            out.push('\t');
            out.push_str(b);
            out.push('\t');
            out.push_str(&c.to_string());
            out.push('\n');
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &out).is_ok() {
            if std::fs::rename(&tmp, path).is_err() {
                let _ = std::fs::write(path, out);
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// Merge the history stored at `path` into this one.
    fn load_from(&mut self, path: &Path) {
        let Ok(data) = std::fs::read_to_string(path) else {
            return;
        };
        for line in data.lines() {
            let mut f = line.split('\t');
            let (kind, w1, w2, count) = match (f.next(), f.next(), f.next(), f.next()) {
                (Some(k), Some(a), Some(b), Some(c)) => (k, a, b, c),
                _ => continue,
            };
            let Ok(count) = count.parse::<u16>() else {
                continue;
            };
            match kind {
                "u" if is_valid_word(w1) => {
                    self.add_unigram(find_or_intern(w1), count);
                }
                "b" if is_valid_word(w1) && is_valid_word(w2) => {
                    self.add_bigram(find_or_intern(w1), find_or_intern(w2), count);
                }
                _ => continue,
            }
        }
    }

    fn save_to_disk(&self) {
        if let Some(p) = &self.path {
            self.save_to(p);
        }
    }

    fn load_from_disk(&mut self) {
        let Some(p) = self.path.clone() else {
            return;
        };
        self.load_from(&p);
    }
}

fn is_valid_word(w: &str) -> bool {
    !w.is_empty()
        && w.len() <= WORD_LEN
        && w
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '\'')
}

/// The canonical `&'static str` for `word`: a model or lexicon word is
/// reused as-is, any novel word is interned.
fn find_or_intern(word: &str) -> &'static str {
    if let Some(id) = model().id_of(word) {
        if let Some(w) = model().word(id) {
            return w;
        }
    }
    if let Some(w) = WORDS.iter().find(|&&s| s == word) {
        return w;
    }
    intern(word)
}

#[derive(Clone, Debug, Default)]
pub struct Predictor {
    partial: String,
    last_word: String,
    prev_word: String,
    /// True right after `.`/`!`/`?`; the next word gets the sentence-start blend.
    at_sentence_start: bool,
    user_history: UserHistory,
    /// Running sentence text sent to the neural engine on each commit.
    sentence_context: String,
    /// Optional explicitly attached neural engine. When absent, falls back to the process-wide engine.
    neural_engine: Option<std::sync::Arc<crate::neural::NeuralEngine>>,
}

fn push_sugg(row: &mut Vec<&'static str>, s: &'static str) {
    if row.len() < MAX_SUGG && !row.contains(&s) {
        row.push(s);
    }
}

/// Pick up to [`MAX_SUGG`] candidates, highest effective score first.
/// A candidate sharing a 4+ char prefix with an already picked one is
/// discounted, so the pills show varied words instead of one root's
/// inflections.
fn take_top(mut scored: Vec<(&'static str, i32)>) -> Vec<&'static str> {
    let mut row: Vec<&'static str> = Vec::new();
    while row.len() < MAX_SUGG && !scored.is_empty() {
        let best = scored
            .iter()
            .enumerate()
            .max_by(|(i1, (w1, s1)), (i2, (w2, s2))| {
                let e1: i32 = *s1 - stem_penalty(w1, &row);
                let e2: i32 = *s2 - stem_penalty(w2, &row);
                e1.cmp(&e2).then(i2.cmp(i1))
            })
            .map(|(i, _)| i)
            .unwrap();
        let (w, _) = scored.remove(best);
        row.push(w);
    }
    row
}

fn stem_penalty(w: &str, selected: &[&str]) -> i32 {
    if selected.iter().any(|s| common_prefix_len(w, s) >= 4) {
        500
    } else {
        0
    }
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// The lowercased leading letter/apostrophe run of `word`, capped at
/// [`WORD_LEN`]; empty when `word` has no such run.
fn clean_word(word: &str) -> String {
    word.chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '\'')
        .take(WORD_LEN)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl Predictor {
    /// A predictor whose history persists to the standard location
    /// (`$XDG_DATA_HOME/osk/user_history.tsv` or
    /// `~/.local/share/osk/user_history.tsv`), loaded on construction.
    pub fn new() -> Self {
        let mut p = Self::default();
        p.user_history.path = Some(UserHistory::default_path());
        p.user_history.load_from_disk();
        p
    }

    /// A predictor with no disk persistence.
    pub fn ephemeral() -> Self {
        Self::default()
    }

    /// Persist the learned history to the standard location.
    pub fn save_to_disk(&self) {
        self.user_history.save_to_disk();
    }

    /// Merge the history from the standard location into this predictor.
    pub fn load_from_disk(&mut self) {
        self.user_history.load_from_disk();
    }

    /// Persist the learned history to `path` (parent dirs created).
    pub fn save_history_to(&self, path: &Path) {
        self.user_history.save_to(path);
    }

    /// Merge the history stored at `path` into this predictor.
    pub fn load_history_from(&mut self, path: &Path) {
        self.user_history.load_from(path);
    }

    /// The learned words with their commit counts and bigram link
    /// counts, most frequent first.
    pub fn learned_words(&self) -> Vec<LearnedWord> {
        self.user_history.learned()
    }

    /// True when `word` is in the user's learned vocabulary.
    pub fn is_learned(&self, word: &str) -> bool {
        self.user_history.unigrams.iter().any(|(w, _)| *w == word)
    }

    /// Forget `word` and every bigram that touches it; true if
    /// anything was removed. The running word context is cleared of
    /// it and the history is persisted.
    pub fn forget_word(&mut self, word: &str) -> bool {
        let clean = clean_word(word);
        if clean.is_empty() {
            return false;
        }
        let removed = self.user_history.forget(&clean);
        if removed {
            if self.last_word == clean {
                self.last_word.clear();
            }
            if self.prev_word == clean {
                self.prev_word.clear();
            }
        }
        self.user_history.save_to_disk();
        removed
    }

    /// Teach `word` with `count` commits (at least one); false when
    /// the word fails validation. The history is persisted.
    pub fn learn_word(&mut self, word: &str, count: u16) -> bool {
        let clean = clean_word(word);
        if !is_valid_word(&clean) {
            return false;
        }
        let w_static = find_or_intern(&clean);
        self.user_history.learn(w_static, count.max(1));
        self.user_history.save_to_disk();
        true
    }

    /// Drop all learned words and bigrams and the running word context.
    pub fn clear_learned(&mut self) {
        self.user_history.clear();
        self.user_history.save_to_disk();
        self.last_word.clear();
        self.prev_word.clear();
    }

    /// Reset to a clean state (no partial, no context).
    pub fn reset(&mut self) {
        self.partial.clear();
        self.last_word.clear();
        self.prev_word.clear();
        self.at_sentence_start = false;
        self.sentence_context.clear();
    }

    /// A key was typed. Letters extend the partial word; `'` extends it
    /// when the previous character is a letter (contractions); any other
    /// non-letter ends the word, and `.`/`!`/`?` mark a sentence start.
    pub fn char(&mut self, c: char) {
        if c == '\'' {
            let last_alpha = self
                .partial
                .chars()
                .next_back()
                .is_some_and(|l| l.is_ascii_alphabetic());
            if self.partial.is_empty()
                || !last_alpha
                || self.partial.len() >= WORD_LEN
            {
                self.break_word();
                return;
            }
            self.partial.push('\'');
            return;
        }
        if !c.is_ascii_alphabetic() {
            self.break_word();
            if c == '.' || c == '!' || c == '?' {
                self.at_sentence_start = true;
                self.sentence_context.clear();
            }
            return;
        }
        if self.partial.len() >= WORD_LEN {
            return;
        }
        self.partial.push(c.to_ascii_lowercase());
    }

    /// Backspace: drop the last letter of the partial.
    pub fn backspace(&mut self) {
        self.partial.pop();
    }

    /// Space: the partial becomes the context; a bare space drops it.
    pub fn space(&mut self) {
        if !self.partial.is_empty() {
            let cur = std::mem::take(&mut self.partial);
            let prev = (!self.last_word.is_empty()).then(|| self.last_word.clone());
            self.prev_word = std::mem::take(&mut self.last_word);
            self.last_word = cur.clone();
            let w_static = find_or_intern(&cur);
            let p_static = prev.as_deref().map(find_or_intern);
            self.user_history.record(p_static, w_static);
            self.user_history.save_to_disk();
            self.at_sentence_start = false;
            self.append_word(&cur);
            self.request_neural();
        } else {
            self.last_word.clear();
            self.prev_word.clear();
        }
    }

    /// Cursor move or punctuation: drop partial and context.
    pub fn break_word(&mut self) {
        self.partial.clear();
        self.last_word.clear();
        self.prev_word.clear();
    }

    /// A suggestion was accepted: its leading letter/apostrophe run
    /// (lowercased) becomes the context. Empty words clear the context.
    pub fn commit(&mut self, word: &str) {
        self.partial.clear();
        let clean: String = word
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == '\'')
            .take(WORD_LEN)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if clean.is_empty() {
            self.last_word.clear();
            self.prev_word.clear();
        } else {
            let prev = (!self.last_word.is_empty()).then(|| self.last_word.clone());
            self.prev_word = std::mem::take(&mut self.last_word);
            self.last_word = clean.clone();
            let w_static = find_or_intern(&clean);
            let p_static = prev.as_deref().map(find_or_intern);
            self.user_history.record(p_static, w_static);
            self.user_history.save_to_disk();
            self.at_sentence_start = false;
            self.append_word(&clean);
            self.request_neural();
        }
    }

    /// Append a committed word to the running sentence context,
    /// keeping only the tail beyond [`MAX_CONTEXT`].
    fn append_word(&mut self, word: &str) {
        if word.is_empty() {
            return;
        }
        self.sentence_context.push_str(word);
        self.sentence_context.push(' ');
        if self.sentence_context.len() > MAX_CONTEXT {
            let cut = self.sentence_context.len() - MAX_CONTEXT;
            let cut = self.sentence_context.floor_char_boundary(cut);
            self.sentence_context.drain(..cut);
        }
    }

    /// Attach a neural engine directly to this predictor.
    pub fn with_neural_engine(mut self, engine: std::sync::Arc<crate::neural::NeuralEngine>) -> Self {
        self.neural_engine = Some(engine);
        self
    }

    /// Set or clear the attached neural engine.
    pub fn set_neural_engine(&mut self, engine: Option<std::sync::Arc<crate::neural::NeuralEngine>>) {
        self.neural_engine = engine;
    }

    fn poll_neural(&self) -> Option<Vec<&'static str>> {
        if let Some(ref e) = self.neural_engine {
            e.poll()
        } else {
            crate::neural::with_engine(|e| e.poll()).flatten()
        }
    }

    /// Kick off a background neural prediction for the current
    /// sentence, when an engine is active.
    fn request_neural(&self) {
        let prompt = self.sentence_context.trim_end();
        if prompt.is_empty() {
            return;
        }
        if let Some(ref e) = self.neural_engine {
            e.request(prompt);
        } else {
            crate::neural::with_engine(|e| e.request(prompt));
        }
    }

    /// The current partial word.
    #[inline]
    pub fn partial(&self) -> &str {
        &self.partial
    }

    /// (id of the word before last, id of the last word) when known.
    fn context(&self) -> (Option<u16>, Option<u16>) {
        let m = model();
        let pa = (!self.prev_word.is_empty())
            .then(|| m.id_of(&self.prev_word))
            .flatten();
        let la = (!self.last_word.is_empty())
            .then(|| m.id_of(&self.last_word))
            .flatten();
        (pa, la)
    }

    /// Context followers in priority order: trigram, then bigram.
    fn followers(&self) -> Vec<&'static str> {
        let m = model();
        let mut out = Vec::new();
        let (pa, la) = self.context();
        if let (Some(a), Some(b)) = (pa, la) {
            if let Some(t) = m.trigram(a, b) {
                for &s in t {
                    if s != EMPTY {
                        if let Some(w) = m.word(s) {
                            if !out.contains(&w) {
                                out.push(w);
                            }
                        }
                    }
                }
            }
        }
        if let Some(b) = la {
            if let Some(f) = m.bigram(b) {
                for &s in f {
                    if s != EMPTY {
                        if let Some(w) = m.word(s) {
                            if !out.contains(&w) {
                                out.push(w);
                            }
                        }
                    }
                }
            }
        }
        for (w, _) in self.user_history.user_bigram_followers(&self.last_word) {
            if !out.contains(&w) {
                out.push(w);
            }
        }
        out
    }

    /// Up to [`MAX_SUGG`] suggestions for the current state using Google Keyboard
    /// (Gboard) interpolated scoring.
    pub fn suggestions(&self) -> Vec<&'static str> {
        let m = model();

        if self.partial.is_empty() {
            // Ready neural predictions take priority in the next-word
            // pills; the n-gram row fills the remaining slots.
            let mut row: Vec<&'static str> = Vec::new();
            if let Some(words) = self.poll_neural() {
                for w in words {
                    push_sugg(&mut row, w);
                }
            }
            let followers = self.followers();
            if followers.is_empty() || self.at_sentence_start {
                let out = self.sentence_start();
                let mut merged = std::mem::take(&mut row);
                for w in out {
                    push_sugg(&mut merged, w);
                }
                return merged;
            }


            // Score contextual candidates using Bayesian Katz/Jelinek-Mercer interpolation
            let (pa, la) = self.context();
            let trigram_slice = match (pa, la) {
                (Some(a), Some(b)) => m.trigram(a, b).unwrap_or(&[]),
                _ => &[],
            };
            let bigram_slice = match la {
                Some(b) => m.bigram(b).unwrap_or(&[]),
                _ => &[],
            };

            let mut scored: Vec<(&'static str, i32)> = followers
                .into_iter()
                .map(|w| {
                    let mut score = 0;
                    if let Some(id) = m.id_of(w) {
                        if let Some(pos) = trigram_slice.iter().position(|&s| s == id) {
                            score += match pos {
                                0 => 5000,
                                1 => 4200,
                                2 => 3500,
                                _ => 2500,
                            };
                        }
                        if let Some(pos) = bigram_slice.iter().position(|&s| s == id) {
                            score += match pos {
                                0 => 2500,
                                1 => 2000,
                                2 => 1600,
                                3 => 1200,
                                4 => 800,
                                _ => 400,
                            };
                        }
                    }
                    let u_bg = self
                        .user_history
                        .bigrams
                        .iter()
                        .find(|(a, b, _)| *a == self.last_word && *b == w)
                        .map(|(_, _, c)| *c)
                        .unwrap_or(0);
                    score += (u_bg as i32 * 600).min(3000);

                    let u_uni = self.user_history.user_unigram_count(w);
                    score += (u_uni as i32 * 100).min(1000);

                    (w, score)
                })
                .collect();

            scored.sort_by(|a, b| b.1.cmp(&a.1));
            let out = take_top(scored);
            let mut merged = std::mem::take(&mut row);
            for w in &out {
                push_sugg(&mut merged, w);
            }
            for &w in m.top() {
                push_sugg(&mut merged, w);
            }
            return merged;
        }

        // Prefix completion with context-boosted interpolated ranking
        let p = self.partial.as_str();
        let followers = self.followers();
        let (pa, la) = self.context();
        let trigram_slice = match (pa, la) {
            (Some(a), Some(b)) => m.trigram(a, b).unwrap_or(&[]),
            _ => &[],
        };
        let bigram_slice = match la {
            Some(b) => m.bigram(b).unwrap_or(&[]),
            _ => &[],
        };

        // Mid-word typing: restrict neural predictions from the preceding context
        // to tokens consistent with the typed prefix instead of running a new pass per keystroke.
        let neural_words = self.poll_neural().unwrap_or_default();

        let mut candidates: Vec<&'static str> = Vec::new();
        // Neural predictions consistent with typed prefix
        for &w in &neural_words {
            if w.starts_with(p) && !candidates.contains(&w) {
                candidates.push(w);
            }
        }
        // Contextual followers starting with prefix
        for &w in &followers {
            if w.starts_with(p) && !candidates.contains(&w) {
                candidates.push(w);
            }
        }
        // Lexicon words starting with prefix (cap scan to top 250 matches)
        for &w in WORDS {
            if w.starts_with(p) && !candidates.contains(&w) {
                candidates.push(w);
                if candidates.len() >= 250 {
                    break;
                }
            }
        }
        // Learned words starting with prefix, including out-of-vocabulary ones
        for (w, _) in &self.user_history.unigrams {
            if w.starts_with(p) && !candidates.contains(&w) {
                candidates.push(w);
            }
        }

        let mut scored: Vec<(&'static str, i32)> = candidates
            .into_iter()
            .map(|w| {
                let mut score = 0;
                // Neural context boost for prefix-consistent candidates
                if let Some(pos) = neural_words.iter().position(|x| *x == w) {
                    score += match pos {
                        0 => 8000,
                        1 => 7000,
                        2 => 6000,
                        _ => 5000,
                    };
                }
                // Trigram context boost
                if let Some(id) = m.id_of(w) {
                    if let Some(pos) = trigram_slice.iter().position(|&s| s == id) {
                        score += match pos {
                            0 => 4000,
                            1 => 3200,
                            2 => 2500,
                            _ => 2000,
                        };
                    }
                    if let Some(pos) = bigram_slice.iter().position(|&s| s == id) {
                        score += match pos {
                            0 => 2000,
                            1 => 1600,
                            2 => 1200,
                            3 => 800,
                            4 => 400,
                            _ => 200,
                        };
                    }
                }

                // Dynamic user history boost
                let u_bg = self
                    .user_history
                    .bigrams
                    .iter()
                    .find(|(a, b, _)| *a == self.last_word && *b == w)
                    .map(|(_, _, c)| *c)
                    .unwrap_or(0);
                score += (u_bg as i32 * 500).min(2500);

                let u_uni = self.user_history.user_unigram_count(w);
                score += (u_uni as i32 * 100).min(800);

                // Global unigram frequency prior (position in WORDS)
                if let Some(rank) = WORDS.iter().position(|&s| s == w) {
                    score += ((WORDS.len() as i32) - rank as i32) / 4;
                }

                // Exact match completion bonus
                if w == p {
                    score += 800;
                }

                // Length brevity bonus (prefer immediate completions)
                let delta = w.len().saturating_sub(p.len());
                if delta <= 2 {
                    score += 150;
                } else if delta <= 4 {
                    score += 75;
                }

                (w, score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        take_top(scored)
    }

    /// Sentence-start fallback: common openers blended with the user's
    /// most frequent words; learned words outrank the generic starters.
    fn sentence_start(&self) -> Vec<&'static str> {
        let mut scored: Vec<(&'static str, i32)> =
            SENTENCE_STARTERS.iter().map(|w| (*w, 100)).collect();
        for (w, c) in &self.user_history.unigrams {
            let s = (*c as i32 * 200).min(5000);
            if let Some(pos) = scored.iter().position(|(x, _)| *x == *w) {
                scored[pos].1 = scored[pos].1.max(s);
            } else {
                scored.push((w, s));
            }
        }
        take_top(scored)
    }
}
