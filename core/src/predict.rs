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
//! 6. Learned-history serialization as text with a dirty flag; the host
//!    (osk-backend) resolves the path and does the file I/O.
//! 7. Spelling correction: misspelled partials get close words from
//!    [`crate::spell`], ranked against the completions.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use parking_lot::RwLock;

use crate::ngm::{model, EMPTY};
use crate::spell;
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
/// Score lost per tenth of an edit between the typed letters and a
/// spelling correction.
const CORRECTION_PENALTY: i32 = 100;
/// Bonus for a correction that spans the whole word, mirroring the
/// exact-match bonus of a completion.
const WHOLE_CORRECTION_BONUS: i32 = 300;
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
        return w;
    }
    drop(set);
    let mut set = INTERNED.write();
    if let Some(w) = set.iter().find(|w| **w == s) {
        return w;
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

/// The word being typed, kept twice on purpose: `lower` (all
/// lowercase, for matching, completion and context) and `typed`
/// (exactly as typed, for retyping a spelling correction). Every
/// operation updates both, so the pair cannot drift apart.
#[derive(Clone, Debug, Default)]
struct Partial {
    lower: String,
    typed: String,
}

impl Partial {
    fn is_empty(&self) -> bool {
        self.lower.is_empty()
    }

    /// Length of the lowercased form (the typed form never differs
    /// except in case, so this bounds both).
    fn len(&self) -> usize {
        self.lower.len()
    }

    fn last_char(&self) -> Option<char> {
        self.lower.chars().next_back()
    }

    fn clear(&mut self) {
        self.lower.clear();
        self.typed.clear();
    }

    /// Type one character: `lower` gets the lowercase form, `typed`
    /// keeps the case it was typed in.
    fn push(&mut self, c: char) {
        self.lower.push(c.to_ascii_lowercase());
        self.typed.push(c);
    }

    fn pop(&mut self) {
        self.lower.pop();
        self.typed.pop();
    }

    /// Take the lowercased word to hand to the context; both forms end
    /// up empty, as [`Partial::clear`] leaves them.
    fn take_lower(&mut self) -> String {
        let lower = std::mem::take(&mut self.lower);
        self.typed.clear();
        lower
    }
}

/// The last two committed words: `prev` was typed before `last`, and
/// together they are the trigram/bigram context. Several paths (space
/// on a bare word, `.`/`!`/`?`, Enter, forget) clear both at once.
#[derive(Clone, Debug, Default)]
struct WordContext {
    prev: String,
    last: String,
}

impl WordContext {
    /// `last` falls back to `prev` and `word` becomes the new last.
    fn shift(&mut self, word: &str) {
        self.prev = std::mem::take(&mut self.last);
        self.last = word.to_string();
    }

    fn clear(&mut self) {
        self.prev.clear();
        self.last.clear();
    }
}

#[derive(Clone, Debug, Default)]
struct UserHistory {
    /// (prev_word, next_word, count)
    bigrams: Vec<(&'static str, &'static str, u16)>,
    /// (word, count)
    unigrams: Vec<(&'static str, u16)>,
}

impl UserHistory {
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
                    word: w,
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

    /// The history as TSV lines:
    /// `u \t word \t <empty> \t count` and `b \t prev \t next \t count`.
    fn tsv(&self) -> String {
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
        out
    }

    /// Merge the history serialized by [`UserHistory::tsv`] into this
    /// one. Malformed lines are skipped.
    fn load_tsv(&mut self, data: &str) {
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
    partial: Partial,
    words: WordContext,
    /// True right after `.`/`!`/`?`; the next word gets the sentence-start blend.
    at_sentence_start: bool,
    user_history: UserHistory,
    /// True when a mutation changed the learned history and the host
    /// has not saved it yet (`take_history_dirty`).
    history_dirty: bool,
    /// Bumped by every mutation so the spell cache can tell that the
    /// corrections it holds were computed for a stale state.
    revision: u64,
    /// Running sentence text sent to the neural engine on each commit.
    sentence_context: String,
    /// Spelling corrections for the last revision seen, since
    /// `suggestions()` runs several times per keystroke.
    spell_cache: RefCell<Option<(u64, Vec<spell::Correction>)>>,
}

/// Completion score boost for the word at `rank` in the neural
/// ranking's prefix matches.
fn neural_boost(rank: usize) -> i32 {
    (4000 - 600 * rank as i32).max(800)
}

/// A sentence context as a neural prompt; None when it is empty.
fn prompt_of(ctx: &str) -> Option<String> {
    let prompt = ctx.trim_end();
    (!prompt.is_empty()).then(|| prompt.to_string())
}

/// Rank of each lexicon word, most frequent first.
static WORD_RANK: LazyLock<HashMap<&'static str, usize>> =
    LazyLock::new(|| WORDS.iter().enumerate().map(|(i, w)| (*w, i)).collect());

/// Global frequency prior: the lexicon rank, or the model rank on a
/// log scale for words only the model knows; the better of the two.
fn frequency_prior(w: &str) -> i32 {
    let lexicon = WORD_RANK
        .get(w)
        .map_or(0, |&rank| (WORDS.len() as i32 - rank as i32) / 4);
    let m = model();
    let vocab = m.len().max(1) as f32;
    let ngram = m.id_of(w).map_or(0, |id| {
        (1000.0 * (1.0 - (id as f32 + 1.0).ln() / (vocab + 1.0).ln())) as i32
    });
    lexicon.max(ngram)
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
    /// A fresh predictor with no learned history. The host loads the
    /// history ([`Predictor::load_history_tsv`], e.g. via
    /// `osk_backend::load_history`) and saves it when it goes dirty.
    pub fn new() -> Self {
        Self::default()
    }

    /// A predictor never meant to be persisted (tests, one-shot jobs).
    pub fn ephemeral() -> Self {
        Self::default()
    }

    /// True when a mutation changed the learned history since the last
    /// take; taking leaves the flag clear.
    pub fn take_history_dirty(&mut self) -> bool {
        std::mem::take(&mut self.history_dirty)
    }

    /// Flag the learned history unsaved again, after a host's write
    /// failed, so the next save retries it.
    pub fn mark_history_dirty(&mut self) {
        self.history_dirty = true;
    }

    /// The learned history serialized as TSV text, for the host to
    /// write wherever it sees fit.
    pub fn history_tsv(&self) -> String {
        self.user_history.tsv()
    }

    /// Merge host-loaded TSV history into this predictor. Loading is
    /// not a user mutation: it does not flag the history dirty.
    pub fn load_history_tsv(&mut self, data: &str) {
        self.revision += 1;
        self.user_history.load_tsv(data);
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
    /// it and the history is flagged for saving.
    pub fn forget_word(&mut self, word: &str) -> bool {
        let clean = clean_word(word);
        if clean.is_empty() {
            return false;
        }
        let removed = self.user_history.forget(&clean);
        if removed {
            self.revision += 1;
            self.history_dirty = true;
            if self.words.last == clean {
                self.words.last.clear();
            }
            if self.words.prev == clean {
                self.words.prev.clear();
            }
        }
        removed
    }

    /// Teach `word` with `count` commits (at least one); false when
    /// the word fails validation. The history is flagged for saving.
    pub fn learn_word(&mut self, word: &str, count: u16) -> bool {
        let clean = clean_word(word);
        if !is_valid_word(&clean) {
            return false;
        }
        self.revision += 1;
        let w_static = find_or_intern(&clean);
        self.user_history.learn(w_static, count.max(1));
        self.history_dirty = true;
        true
    }

    /// Drop all learned words and bigrams and the running word context.
    pub fn clear_learned(&mut self) {
        self.user_history.clear();
        self.words.clear();
        self.revision += 1;
        self.history_dirty = true;
    }

    /// Reset to a clean state (no partial, no context).
    pub fn reset(&mut self) {
        self.clear_partial();
        self.words.clear();
        self.at_sentence_start = false;
        self.sentence_context.clear();
        self.revision += 1;
    }

    /// Enter or send: the sentence being typed is over. Clears the
    /// partial, the word context, and the sentence context, and marks
    /// the next word as a sentence start so the pills offer fresh
    /// openers.
    pub fn end_message(&mut self) {
        self.clear_partial();
        self.words.clear();
        self.sentence_context.clear();
        self.at_sentence_start = true;
        self.revision += 1;
    }

    /// A key was typed. Letters extend the partial word; `'` extends it
    /// when the previous character is a letter (contractions); any other
    /// non-letter ends the word, and `.`/`!`/`?` mark a sentence start.
    pub fn char(&mut self, c: char) {
        self.revision += 1;
        if c == '\'' {
            let last_alpha = self
                .partial
                .last_char()
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
        self.partial.push(c);
    }

    /// Backspace: drop the last letter of the partial.
    pub fn backspace(&mut self) {
        self.partial.pop();
        self.revision += 1;
    }

    fn clear_partial(&mut self) {
        self.partial.clear();
    }

    /// Space: the partial becomes the context; a bare space drops it.
    pub fn space(&mut self) {
        self.revision += 1;
        if !self.partial.is_empty() {
            let cur = self.partial.take_lower();
            let prev = (!self.words.last.is_empty()).then(|| self.words.last.clone());
            self.words.shift(&cur);
            let w_static = find_or_intern(&cur);
            let p_static = prev.as_deref().map(find_or_intern);
            self.user_history.record(p_static, w_static);
            self.history_dirty = true;
            self.at_sentence_start = false;
            self.append_word(&cur);
        } else {
            self.words.clear();
        }
    }

    /// Cursor move or punctuation: drop partial and context.
    pub fn break_word(&mut self) {
        self.clear_partial();
        self.words.clear();
        self.revision += 1;
    }

    /// A suggestion was accepted: its leading letter/apostrophe run
    /// (lowercased) becomes the context. Empty words clear the context.
    pub fn commit(&mut self, word: &str) {
        self.revision += 1;
        self.clear_partial();
        let clean = clean_word(word);
        if clean.is_empty() {
            self.words.clear();
        } else {
            let prev = (!self.words.last.is_empty()).then(|| self.words.last.clone());
            self.words.shift(&clean);
            let w_static = find_or_intern(&clean);
            let p_static = prev.as_deref().map(find_or_intern);
            self.user_history.record(p_static, w_static);
            self.history_dirty = true;
            self.at_sentence_start = false;
            self.append_word(&clean);
        }
    }

    /// Append a committed word to the running sentence context.
    fn append_word(&mut self, word: &str) {
        self.sentence_context = self.context_after(word);
    }

    /// The running sentence context with `word` committed, keeping only
    /// the tail beyond [`MAX_CONTEXT`].
    fn context_after(&self, word: &str) -> String {
        let mut ctx = self.sentence_context.clone();
        if word.is_empty() {
            return ctx;
        }
        ctx.push_str(word);
        ctx.push(' ');
        if ctx.len() > MAX_CONTEXT {
            let cut = ctx.floor_char_boundary(ctx.len() - MAX_CONTEXT);
            ctx.drain(..cut);
        }
        ctx
    }

    /// Bumped by every change that can change the suggestions.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Nothing of the current sentence is typed: no partial word and
    /// no committed context (at launch and after Enter).
    pub fn sentence_is_empty(&self) -> bool {
        self.partial.lower.is_empty() && self.sentence_context.is_empty()
    }

    /// The text the neural engine predicts the next word after: the
    /// sentence so far, with the partial word committed the way a space
    /// would commit it, so a request made mid-word is the one the
    /// space needs. None at a sentence start.
    pub fn neural_prompt(&self) -> Option<String> {
        prompt_of(&self.context_after(&self.partial.lower))
    }

    /// A lowercase word from the vocabulary, the lexicon or the user's
    /// learned words.
    pub fn is_known(&self, w: &str) -> bool {
        model().id_of(w).is_some()
            || WORD_RANK.contains_key(w)
            || self.user_history.user_unigram_count(w) > 0
    }

    /// The prompt for the word being typed: the sentence before it.
    pub fn context_prompt(&self) -> Option<String> {
        prompt_of(&self.sentence_context)
    }

    /// The prompt after a click on the pill `word`, the way
    /// [`Predictor::commit`] commits it.
    pub fn neural_prompt_after(&self, word: &str) -> Option<String> {
        prompt_of(&self.context_after(&clean_word(word)))
    }

    /// The current partial word (lowercased).
    #[inline]
    pub fn partial(&self) -> &str {
        &self.partial.lower
    }

    /// The current partial word with the letter case it was typed in.
    #[inline]
    pub fn typed(&self) -> &str {
        &self.partial.typed
    }

    /// (id of the word before last, id of the last word) when known.
    fn context(&self) -> (Option<u16>, Option<u16>) {
        let m = model();
        let pa = (!self.words.prev.is_empty())
            .then(|| m.id_of(&self.words.prev))
            .flatten();
        let la = (!self.words.last.is_empty())
            .then(|| m.id_of(&self.words.last))
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
        for (w, _) in self.user_history.user_bigram_followers(&self.words.last) {
            if !out.contains(&w) {
                out.push(w);
            }
        }
        out
    }

    /// Up to [`MAX_SUGG`] suggestions for the current state using Google Keyboard
    /// (Gboard) interpolated scoring. `neural` is the neural ranking of
    /// the next word after the sentence so far, as its owner adopted
    /// it (empty for none): its top words lead the next-word pills,
    /// and while a word is typed its matches boost the completions.
    // One coherent scoring pipeline (context tiers, correction merge,
    // cache refill); splitting it would scatter a single contract.
    #[allow(clippy::too_many_lines)]
    pub fn suggestions(&self, neural: &[&'static str]) -> Vec<&'static str> {
        let m = model();

        if self.partial.is_empty() {
            // Neural predictions take priority in the next-word pills;
            // the n-gram row fills the remaining slots.
            let mut row: Vec<&'static str> = Vec::new();
            for &w in neural {
                push_sugg(&mut row, w);
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
                        .find(|(a, b, _)| *a == self.words.last && *b == w)
                        .map(|(_, _, c)| *c)
                        .unwrap_or(0);
                    score += (u_bg as i32 * 600).min(3000);

                    let u_uni = self.user_history.user_unigram_count(w);
                    score += (u_uni as i32 * 100).min(1000);

                    (w, score)
                })
                .collect();

            scored.sort_by_key(|&(_, s)| std::cmp::Reverse(s));
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
        let p = self.partial.lower.as_str();
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

        let mut candidates: Vec<&'static str> = Vec::new();
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
        // Model vocabulary words starting with prefix, for completions
        // the lexicon lacks
        for w in (0..u16::MAX).map_while(|id| m.word(id)) {
            if candidates.len() >= 250 {
                break;
            }
            if w.starts_with(p) && !candidates.contains(&w) {
                candidates.push(w);
            }
        }
        // Learned words starting with prefix, including out-of-vocabulary ones
        for (w, _) in &self.user_history.unigrams {
            if w.starts_with(p) && !candidates.contains(w) {
                candidates.push(w);
            }
        }
        // The neural ranking's known words starting with prefix, best
        // first. The model also ranks word pieces ("groc" for
        // "grocery"), which must not become pills.
        let neural: Vec<&'static str> = neural
            .iter()
            .copied()
            .filter(|w| w.starts_with(p) && self.is_known(w))
            .collect();
        for &w in &neural {
            if !candidates.contains(&w) {
                candidates.push(w);
            }
        }

        let mut scored: Vec<(&'static str, i32)> = candidates
            .into_iter()
            .map(|w| {
                let mut score = self.completion_score(w, p, trigram_slice, bigram_slice);
                // Exact match completion bonus
                if w == p {
                    score += 800;
                }
                if let Some(rank) = neural.iter().position(|&n| n == w) {
                    score += neural_boost(rank);
                }
                (w, score)
            })
            .collect();

        // Spelling corrections compete with the completions, discounted
        // by how far the typed letters are from the word. The partial is
        // only trusted to match the text field while it is uncapped.
        if p.len() < WORD_LEN {
            let known = self.is_known(p);
            let mut cache = self.spell_cache.borrow_mut();
            if cache.as_ref().map(|(k, _)| *k) != Some(self.revision) {
                let learned = self.user_history.unigrams.iter().map(|(w, _)| w);
                *cache = Some((self.revision, spell::corrections(p, known, learned)));
            }
            let corrections = cache.as_ref().map(|(_, c)| c.as_slice()).unwrap_or(&[]);
            for &c in corrections {
                let mut score = self.completion_score(c.word, p, trigram_slice, bigram_slice);
                score -= i32::from(c.cost) * CORRECTION_PENALTY;
                if c.whole {
                    score += WHOLE_CORRECTION_BONUS;
                }
                scored.push((c.word, score));
            }
        }

        scored.sort_by_key(|&(_, s)| std::cmp::Reverse(s));
        take_top(scored)
    }

    /// Context, history, frequency and brevity score for a candidate
    /// completing (or correcting) the partial `p`.
    fn completion_score(
        &self,
        w: &'static str,
        p: &str,
        trigram_slice: &[u16],
        bigram_slice: &[u16],
    ) -> i32 {
        let m = model();
        let mut score = 0;
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
            .find(|(a, b, _)| *a == self.words.last && *b == w)
            .map(|(_, _, c)| *c)
            .unwrap_or(0);
        score += (u_bg as i32 * 500).min(2500);

        let u_uni = self.user_history.user_unigram_count(w);
        score += (u_uni as i32 * 100).min(800);

        score += frequency_prior(w);

        // Length brevity bonus (prefer immediate completions)
        let delta = w.len().saturating_sub(p.len());
        if delta <= 2 {
            score += 150;
        } else if delta <= 4 {
            score += 75;
        }
        score
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

// ---- tests: the pure predictor's history contract ----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_sets_the_history_dirty_flag() {
        let mut p = Predictor::ephemeral();
        assert!(!p.take_history_dirty());
        p.commit("hello");
        assert!(p.take_history_dirty());
        assert!(!p.take_history_dirty(), "take consumes the flag");
    }

    #[test]
    fn history_tsv_round_trips_learned_words_and_bigrams() {
        let mut p = Predictor::ephemeral();
        p.commit("hello");
        p.commit("world");
        let tsv = p.history_tsv();
        assert!(
            tsv.lines().any(|l| l == "u\thello\t\t1")
                && tsv.lines().any(|l| l == "b\thello\tworld\t1"),
            "{tsv:?}"
        );

        let mut q = Predictor::ephemeral();
        q.load_history_tsv(&tsv);
        assert_eq!(q.history_tsv(), tsv, "round trip preserves the entries");
        assert!(q.is_learned("hello") && q.is_learned("world"));
    }

    /// "i want to e", typed through the predictor.
    fn typing_e_after_i_want_to() -> Predictor {
        let mut p = Predictor::ephemeral();
        for w in ["i", "want", "to"] {
            p.commit(w);
        }
        p.char('e');
        p
    }

    #[test]
    fn neural_ranking_lifts_a_known_completion() {
        let p = typing_e_after_i_want_to();
        assert!(!p.suggestions(&[]).contains(&"explore"));
        assert!(p.suggestions(&["explore"]).contains(&"explore"));
    }

    #[test]
    fn neural_word_pieces_never_become_completions() {
        let p = typing_e_after_i_want_to();
        assert!(!p.is_known("exq"));
        assert!(!p.suggestions(&["exq"]).contains(&"exq"));
    }
}
