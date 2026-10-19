//! The shared keyboard controller: everything a click does.
//!
//! Both UIs (the cairo `osk` UI and the Slint UI) route key clicks and
//! suggestion clicks through this module, which owns the modifier
//! engine, the predictor and the optional neural engine (as a value;
//! there is no process global). The UIs keep only rendering, key
//! repeat, and the Wayland/Slint event loop, so a behavior fix lands
//! once and every surface gets it.
//!
//! The key -> predictor transition is a match with no wildcard arm
//! ([`Transition::apply`]), so a new transition case cannot skip a
//! caller: adding a variant breaks the build until the arm is written.

use std::cell::RefCell;

use crate::events::{replace_word, tap_events, OskEvent, TAP_MAX_EVENTS};
use crate::key::{key, Click, Key, KeyKind};
use crate::mods::{mod_index, Casing, ModKeyState, ModifierEngine, MomentaryClick, MOD_SHIFT};
use crate::neural::NeuralEngine;
use crate::predict::Predictor;
use crate::typekey::{type_string, CharMap, KeySink, NoKey};

/// Policy that differs between hosts, expressed as data.
#[derive(Clone, Copy, Debug)]
pub struct ControllerConfig {
    /// Feed the predictor on ordinary key taps (`osk` can turn this
    /// off from its settings menu).
    pub prediction: bool,
    /// Tab accepts the top suggestion when the predictor has one
    /// (plain Tab otherwise). Shift+Tab always passes through.
    pub tab_accepts_prediction: bool,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        ControllerConfig {
            prediction: true,
            tab_accepts_prediction: false,
        }
    }
}

/// The predictor transition a tapped key produces. The switch in
/// [`Transition::apply`] has no wildcard arm, so every variant is
/// handled at every call site by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    /// KEY_BACKSPACE: drop the last letter of the partial.
    Backspace,
    /// KEY_SPACE: the partial becomes the word context.
    Space,
    /// KEY_ENTER: the message is over; the next word starts a sentence.
    EndMessage,
    /// A letter key; `typed` is the case the letter was typed in, so a
    /// spelling correction can be retyped to match.
    Letter { typed: char },
    /// A single-character label (symbols, arrows) feeds the partial.
    Symbol { c: char },
    /// A multi-character label (com, pgdn, esc): ends the current word.
    Break,
    /// A tap key that never touches the predictor (no label).
    None,
}

impl Transition {
    /// Apply the transition to the predictor. No wildcard arm: a new
    /// variant is a compile error here, and this is the only place a
    /// tap reaches the predictor, so no caller can be skipped.
    pub fn apply(self, p: &mut Predictor) {
        match self {
            Transition::Backspace => p.backspace(),
            Transition::Space => p.space(),
            Transition::EndMessage => p.end_message(),
            Transition::Letter { typed } => p.char(typed),
            Transition::Symbol { c } => p.char(c),
            Transition::Break => p.break_word(),
            Transition::None => {}
        }
    }
}

/// Classify a tapped key (the transition had been derived per UI in
/// two diverging matches; this is now the single copy). Called only
/// for [`Click::Tap`] keys, so no modifier handling is needed here.
fn transition(k: &Key, casing: Casing) -> Transition {
    match k.code_u16() {
        key::KEY_BACKSPACE => Transition::Backspace,
        key::KEY_SPACE => Transition::Space,
        key::KEY_ENTER => Transition::EndMessage,
        _ => {
            let single = k.label.and_then(|l| {
                let mut it = l.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => Some(c),
                    _ => None,
                }
            });
            if k.letter {
                match single {
                    Some(c) => {
                        // The case the letter was typed in, so a spelling
                        // correction can be retyped to match.
                        let typed = if casing.letter_upper() {
                            c.to_ascii_uppercase()
                        } else {
                            c.to_ascii_lowercase()
                        };
                        Transition::Letter { typed }
                    }
                    None => Transition::None,
                }
            } else {
                match single {
                    Some(c) => Transition::Symbol { c },
                    None => Transition::Break,
                }
            }
        }
    }
}

/// What [`Controller::click_key`] decided to do, for UI feedback
/// (flash, pressed look, key repeat).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clicked {
    /// A gap key: nothing happened.
    Inert,
    /// A momentary modifier: one press+release injected.
    Momentary,
    /// A latching modifier ran the 3-state machine: slot index and
    /// new state.
    Latch(usize, ModKeyState),
    /// Caps Lock toggled; `true` when now on.
    Caps(bool),
    /// An ordinary key was tapped: `accepted` when a Tab tap accepted
    /// the top suggestion instead of typing a Tab.
    Tapped { accepted: bool },
}

/// A suggestion commit could not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitError {
    /// Nothing is suggested at that index.
    NoWord,
    /// No default keymap, so the word cannot be typed.
    NoKeymap,
    /// The word needs more than the fixed 64-char edit buffer.
    TooLarge,
    /// The word holds a char with no key in the default keymap.
    NoKey(NoKey),
}

/// The controller state shared by every UI. The neural engine is
/// owned here as a value (`Option` plus an on/off flag), not as a
/// process global.
pub struct Controller {
    cfg: ControllerConfig,
    mods: ModifierEngine,
    predictor: Predictor,
    charmap: Option<CharMap>,
    neural: Option<NeuralEngine>,
    /// On/off switch for the engine; true while the engine is new.
    neural_enabled: bool,
    /// The neural words the row shows, read once per keystroke so the
    /// row never changes between keystrokes.
    neural_words: Vec<&'static str>,
    /// The row for the predictor revision it was computed at; a frame
    /// reads it several times, and only a change recomputes it.
    row: RefCell<Option<(u64, Vec<&'static str>)>>,
}

impl Controller {
    /// The construction path every UI shares: an existing predictor
    /// plus the engine the host started for it (`None` runs the plain
    /// n-gram predictor, like the offline render path). The engine
    /// starts enabled.
    pub fn with_neural(
        cfg: ControllerConfig,
        predictor: Predictor,
        neural: Option<NeuralEngine>,
    ) -> Self {
        let neural_enabled = neural.is_some();
        Controller {
            cfg,
            mods: ModifierEngine::new(),
            predictor,
            charmap: CharMap::new(),
            neural,
            neural_enabled,
            neural_words: Vec::new(),
            row: RefCell::new(None),
        }
    }

    /// A controller over an existing predictor without the neural
    /// engine (offline paths, tests); a host starts the engine with
    /// [`Controller::with_neural`]. `ephemeral()` in tests where no
    /// history may be written to disk.
    pub fn with_predictor(cfg: ControllerConfig, predictor: Predictor) -> Self {
        Self::with_neural(cfg, predictor, None)
    }

    // ---- read accessors for rendering ----

    pub fn mods(&self) -> &ModifierEngine {
        &self.mods
    }

    pub fn predictor(&self) -> &Predictor {
        &self.predictor
    }

    pub fn charmap(&self) -> Option<&CharMap> {
        self.charmap.as_ref()
    }

    pub fn prediction(&self) -> bool {
        self.cfg.prediction
    }

    /// Host-side state changes that bypass a click (learned words,
    /// history load/save) reach the predictor here.
    pub fn predictor_mut(&mut self) -> &mut Predictor {
        &mut self.predictor
    }

    pub fn tab_accepts_prediction(&self) -> bool {
        self.cfg.tab_accepts_prediction
    }

    // ---- the neural engine (owned here, not a global) ----

    /// Whether the engine is installed and currently enabled.
    pub fn is_neural_enabled(&self) -> bool {
        self.neural_enabled && self.neural.is_some()
    }

    /// Toggle the engine on or off and return the new state; `false`
    /// when no engine is installed.
    pub fn toggle_neural(&mut self) -> bool {
        if self.neural.is_some() {
            self.neural_enabled = !self.neural_enabled;
        }
        self.settle_row();
        self.is_neural_enabled()
    }

    /// Drain the failures the engine's worker reported since the last
    /// call (see [`NeuralEngine::take_errors`]). Unlike the engine's
    /// prediction work, this ignores the toggle: the disabled worker
    /// still fails and its host must still hear about it.
    pub fn take_errors(&self) -> Vec<String> {
        self.neural
            .as_ref()
            .map(NeuralEngine::take_errors)
            .unwrap_or_default()
    }

    /// The suggestion row for the current state: empty while nothing
    /// of the sentence is typed (at launch, after Enter), else the
    /// n-gram words led by the neural words read at the last keystroke.
    pub fn suggestions(&self) -> Vec<&'static str> {
        let rev = self.predictor.revision();
        let mut row = self.row.borrow_mut();
        match row.as_ref() {
            Some((at, words)) if *at == rev => words.clone(),
            _ => {
                let words = if self.predictor.sentence_is_empty() {
                    Vec::new()
                } else {
                    self.predictor.suggestions(&self.neural_words)
                };
                *row = Some((rev, words.clone()));
                words
            }
        }
    }

    /// After a keystroke: take the neural ranking for the sentence
    /// before the word being typed when it is already in, else leave
    /// the n-gram row alone until a later keystroke. Then request what
    /// the next keystrokes may need: that ranking, the sentence as a
    /// space would end the word, and as each pill on the row would.
    /// Requested ahead, an answer is usually in by the time it is
    /// needed.
    fn settle_row(&mut self) {
        self.neural_words.clear();
        *self.row.get_mut() = None;
        // Field borrows: the engine is disjoint from `neural_words`.
        let Some(engine) = self.neural.as_ref().filter(|_| self.neural_enabled) else {
            return;
        };
        let context = self.predictor.context_prompt();
        if let Some(c) = &context {
            self.neural_words = engine.answer(c).unwrap_or_default();
        }
        let mut prompts: Vec<String> = Vec::new();
        let ahead = self.suggestions().into_iter().map(|w| self.predictor.neural_prompt_after(w));
        for p in [context, self.predictor.neural_prompt()].into_iter().chain(ahead).flatten() {
            if !prompts.contains(&p) {
                prompts.push(p);
            }
        }
        engine.request(&prompts);
    }

    // ---- host policy ----

    pub fn set_prediction(&mut self, on: bool) {
        self.cfg.prediction = on;
    }

    pub fn set_tab_accepts_prediction(&mut self, on: bool) {
        self.cfg.tab_accepts_prediction = on;
    }

    /// Forget `word`: host-side (right-click) learned-word removal.
    pub fn forget_word(&mut self, word: &str) -> bool {
        self.predictor.forget_word(word)
    }

    // ---- the click path ----

    /// Send a deferred standalone tap (Win waiting out the
    /// double-click window) once it is due. Hosts call this when
    /// [`Controller::tick_due`] passes. Returns whether a tap was sent.
    pub fn tick(&mut self, now: u64, sink: &mut dyn KeySink) -> bool {
        match self.mods.take_due_tap(now, false) {
            Some(code) => {
                sink.press(code);
                sink.release(code);
                true
            }
            None => false,
        }
    }

    /// When [`Controller::tick`] has work, in ms; None = nothing
    /// pending, so the host needs no timer.
    pub fn tick_due(&self) -> Option<u64> {
        self.mods.deferred_due()
    }

    /// Send a pending deferred tap right away (before another key).
    fn flush_deferred(&mut self, sink: &mut dyn KeySink) {
        if let Some(code) = self.mods.take_due_tap(0, true) {
            sink.press(code);
            sink.release(code);
        }
    }

    /// Route one key click by [`Click`](crate::key::Click): momentary
    /// press+release, the 3-state latch machine, Caps Lock, or an
    /// ordinary tap. `now` is the click time in ms.
    pub fn click_key(&mut self, k: &Key, now: u64, sink: &mut dyn KeySink) -> Clicked {
        let (code, click) = match k.kind {
            KeyKind::Inject { code, click } => (code.get(), click),
            KeyKind::Gap => return Clicked::Inert,
        };
        // A deferred Win tap goes out before any other key, so the order
        // stays what was clicked. A click on that same key decides the
        // double-click instead.
        let same_slot = click == Click::Momentary && self.mods.deferred_slot() == mod_index(code);
        if !same_slot {
            self.flush_deferred(sink);
        }
        match click {
            Click::Momentary => {
                // A single click is one standalone press+release, sent at
                // once: Alt must be a real tap so Unity-style mouse-mode
                // toggles fire, and a lone Super opens the shell search.
                // A double-click locks the slot like Ctrl (the modifier
                // is then held around every tap); a click unlocks it.
                match self.mods.momentary_click(code, now) {
                    MomentaryClick::Tap => {
                        sink.press(code);
                        sink.release(code);
                        Clicked::Momentary
                    }
                    // Win waits out the double-click window; the host
                    // sends it from tick() when due.
                    MomentaryClick::Deferred => Clicked::Momentary,
                    MomentaryClick::Locked | MomentaryClick::Unlocked => {
                        let slot = mod_index(code).unwrap_or(MOD_SHIFT);
                        Clicked::Latch(slot, self.mods.state(slot))
                    }
                }
            }
            Click::Latch => {
                // 3-state machine: OFF -> LATCHED -> LOCKED -> OFF
                // (double-click locks). A latch key is always a
                // modifier; the engine no-ops on any other code.
                self.mods.click(code, now);
                let slot = mod_index(code).unwrap_or(MOD_SHIFT);
                Clicked::Latch(slot, self.mods.state(slot))
            }
            Click::Caps => {
                self.mods.toggle_caps();
                Clicked::Caps(self.mods.caps_lock())
            }
            Click::Tap => {
                let accepted = self.tap_key(k, sink);
                Clicked::Tapped { accepted }
            }
        }
    }

    /// Tap an injectable key: build the chord, inject it, consume
    /// latched modifiers, feed the predictor. Returns whether a Tab tap
    /// accepted the top suggestion instead of typing a Tab; non-tap
    /// keys return `false` unchanged.
    pub fn tap_key(&mut self, k: &Key, sink: &mut dyn KeySink) -> bool {
        let Some(tap) = k.tap() else {
            return false;
        };
        let casing = self.mods.casing();
        // When enabled, Tab accepts the top suggestion (like clicking
        // the first pill); Shift+Tab passes through as a plain TAB.
        if self.cfg.prediction
            && self.cfg.tab_accepts_prediction
            && tap.code_u16() == key::KEY_TAB
            && !casing.shift
            && !self.suggestions().is_empty()
        {
            let _ = self.commit_suggestion(0, sink);
            return true;
        }
        let sticky = self.mods.sticky();
        let mut ev = [OskEvent {
            code: 0,
            press: false,
        }; TAP_MAX_EVENTS];
        let Ok(n) = tap_events(tap, casing, &sticky, &mut ev) else {
            return false;
        };
        for e in &ev[..n] {
            if e.press {
                sink.press(e.code);
            } else {
                sink.release(e.code);
            }
        }
        self.mods.consume_latched();
        if self.cfg.prediction {
            transition(k, casing).apply(&mut self.predictor);
            self.settle_row();
        }
        false
    }

    /// Commit suggestion `idx`: erase back to the shared prefix, type
    /// the completion or spelling correction, consume latched
    /// modifiers, and commit the word to the predictor. Only
    /// `NoWord`/`NoKeymap` return before anything is injected (the
    /// whole word is validated before typing; a failed type leaves the
    /// text untouched, as before).
    pub fn commit_suggestion(
        &mut self,
        idx: usize,
        sink: &mut dyn KeySink,
    ) -> Result<(), CommitError> {
        self.flush_deferred(sink);
        // The row the user saw, so a pill commits the word it showed.
        let Some(word) = self.suggestions().get(idx).copied() else {
            return Err(CommitError::NoWord);
        };
        let Some(map) = &self.charmap else {
            return Err(CommitError::NoKeymap);
        };
        let partial = self.predictor.partial().to_string();
        let casing = self.mods.casing();
        let mut out = [0 as char; 64];
        let Some((erase, n)) =
            replace_word(word, &partial, self.predictor.typed(), casing, &mut out)
        else {
            return Err(CommitError::TooLarge);
        };
        let text: String = out[..n].iter().collect();
        for _ in 0..erase {
            sink.press(key::KEY_BACKSPACE);
            sink.release(key::KEY_BACKSPACE);
        }
        let _ = type_string(sink, map, &text, || {});
        self.mods.consume_latched();
        self.predictor.commit(word);
        self.settle_row();
        Ok(())
    }
}

// ---- tests: the behavior contract through the controller ----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::MOD_ALT;

    /// Every injected event sequence, press/release in order.
    #[derive(Default)]
    struct Rec {
        ev: Vec<OskEvent>,
    }

    impl Rec {
        fn codes(&self, press: Option<bool>) -> Vec<u16> {
            self.ev
                .iter()
                .filter(|e| press.is_none_or(|p| e.press == p))
                .map(|e| e.code)
                .collect()
        }
    }

    impl KeySink for Rec {
        fn press(&mut self, code: u16) {
            self.ev.push(OskEvent { code, press: true });
        }
        fn release(&mut self, code: u16) {
            self.ev.push(OskEvent { code, press: false });
        }
    }

    fn ctl() -> Controller {
        Controller::with_predictor(
            ControllerConfig {
                tab_accepts_prediction: true,
                ..Default::default()
            },
            Predictor::ephemeral(),
        )
    }

    const K_A_KEY: Key = Key::letter("a", key::KEY_A);

    #[test]
    fn plain_tap_injects_and_feeds_partial() {
        let mut c = ctl();
        let mut s = Rec::default();
        let accepted = c.tap_key(&K_A_KEY, &mut s);
        assert!(!accepted);
        assert_eq!(s.codes(None), &[key::KEY_A, key::KEY_A]);
        assert_eq!(s.codes(Some(true)), &[key::KEY_A]);
        assert_eq!(s.codes(Some(false)), &[key::KEY_A]);
        assert_eq!(c.predictor().partial(), "a");
        assert_eq!(c.predictor().typed(), "a");
    }

    #[test]
    fn latched_shift_wraps_the_tap_and_is_consumed() {
        let mut c = ctl();
        let mut s = Rec::default();
        let shift = Key::latch("\u{21e7}", key::KEY_LEFTSHIFT, 45);
        assert_eq!(
            c.click_key(&shift, 100, &mut s),
            Clicked::Latch(crate::mods::MOD_SHIFT, ModKeyState::Latched)
        );
        let accepted = c.tap_key(&K_A_KEY, &mut s);
        assert!(!accepted);
        // shift press, a press/release, shift release
        assert_eq!(s.codes(None), &[
            key::KEY_LEFTSHIFT,
            key::KEY_A,
            key::KEY_A,
            key::KEY_LEFTSHIFT,
        ]);
        // consumed after exactly one tap
        assert_eq!(c.mods().state(crate::mods::MOD_SHIFT), ModKeyState::Off);
        assert_eq!(c.predictor().partial(), "a");
        // the predictor saw the case the letter was typed in
        assert_eq!(c.predictor().typed(), "A");
    }

    #[test]
    fn enter_ends_the_message_for_every_ui() {
        let mut c = ctl();
        let mut s = Rec::default();
        for ch in "th".chars() {
            c.predictor_mut().char(ch);
        }
        assert!(!c.predictor().partial().is_empty());
        let enter = Key::wide("enter", key::KEY_ENTER, 40);
        assert!(matches!(
            c.click_key(&enter, 0, &mut s),
            Clicked::Tapped { accepted: false }
        ));
        // The tap injects Enter and the transition ends the message:
        // partial, typed case and word context all reset.
        assert_eq!(s.codes(Some(true)), &[key::KEY_ENTER]);
        assert_eq!(s.codes(Some(false)), &[key::KEY_ENTER]);
        assert_eq!(c.predictor().partial(), "");
        assert_eq!(c.predictor().typed(), "");
        // With no context left the pills fall back to the sentence
        // starters for the next message.
        let starters = ["i", "the", "we", "how", "what", "yes", "thanks"];
        let suggs = c.predictor().suggestions(&[]);
        assert!(!suggs.is_empty());
        assert!(suggs.iter().all(|w| starters.contains(w)), "{suggs:?}");
    }

    #[test]
    fn space_hands_the_partial_to_the_context() {
        let mut c = ctl();
        let mut s = Rec::default();
        c.predictor_mut().char('x');
        let space = Key::space(60);
        assert!(matches!(
            c.click_key(&space, 0, &mut s),
            Clicked::Tapped { accepted: false }
        ));
        assert_eq!(s.codes(Some(true)), &[key::KEY_SPACE]);
        assert_eq!(c.predictor().partial(), "");
        assert_eq!(c.predictor().typed(), "");
    }

    #[test]
    fn tab_accepts_the_top_suggestion_like_the_first_pill() {
        let mut c = ctl();
        c.predictor_mut().char('t');
        c.predictor_mut().char('h');
        // The expected edit is derived with the same replace_word the
        // controller uses: this test checks the plumbing (accept, erase,
        // type, commit), not the corpus.
        let word = c.predictor().suggestions(&[])[0];
        let partial = c.predictor().partial().to_string();
        let typed = c.predictor().typed().to_string();
        let mut out = [0 as char; 64];
        let (erase, n) =
            replace_word(word, &partial, &typed, Casing::default(), &mut out)
                .expect("fits the buffer");
        let mut s = Rec::default();
        let tab = Key::wide("tab", key::KEY_TAB, 60);
        let accepted = c.tap_key(&tab, &mut s);
        assert!(accepted);
        // The completion/correction went out; never a TAB key.
        assert!(!s.codes(None).contains(&key::KEY_TAB));
        // Presses: `erase` backspaces, then the typed chars over the
        // default keymap (a trailing space types as KEY_SPACE).
        let map = CharMap::new().expect("default keymap");
        let mut want: Vec<u16> = Vec::new();
        want.extend(std::iter::repeat_n(key::KEY_BACKSPACE, erase));
        for ch in out[..n].iter() {
            if *ch == ' ' {
                want.push(key::KEY_SPACE);
            } else {
                let (code, shift) = map.char_to_key(*ch).expect("mapped");
                if shift {
                    want.push(key::KEY_LEFTSHIFT);
                }
                want.push(code);
            }
        }
        assert_eq!(s.codes(Some(true)), want);
        // The commit consumed the partial.
        assert_eq!(c.predictor().partial(), "");
        assert_eq!(c.predictor().typed(), "");
    }

    #[test]
    fn shift_tab_is_a_plain_tab() {
        let mut c = ctl();
        c.predictor_mut().char('t');
        c.predictor_mut().char('h');
        c.predictor_mut().space();
        // No partial: Tab would accept the next-word prediction.
        let shift = Key::latch("\u{21e7}", key::KEY_LEFTSHIFT, 45);
        let mut s = Rec::default();
        c.click_key(&shift, 100, &mut s);
        let tab = Key::wide("tab", key::KEY_TAB, 60);
        let accepted = c.tap_key(&tab, &mut s);
        assert!(!accepted);
        assert_eq!(s.codes(Some(true)), &[key::KEY_LEFTSHIFT, key::KEY_TAB]);
        assert_eq!(s.codes(Some(false)), &[key::KEY_TAB, key::KEY_LEFTSHIFT]);
    }

    #[test]
    fn momentary_key_injects_one_press_and_release() {
        let mut c = ctl();
        let mut s = Rec::default();
        let alt = Key::momentary("alt", key::KEY_LEFTALT, 25);
        assert_eq!(c.click_key(&alt, 100, &mut s), Clicked::Momentary);
        assert_eq!(s.codes(None), &[key::KEY_LEFTALT, key::KEY_LEFTALT]);
        assert_eq!(c.mods().state(MOD_ALT), ModKeyState::Off);
    }

    #[test]
    fn slow_momentary_clicks_each_tap() {
        let mut c = ctl();
        let mut s = Rec::default();
        let alt = Key::momentary("alt", key::KEY_LEFTALT, 25);
        assert_eq!(c.click_key(&alt, 100, &mut s), Clicked::Momentary);
        assert_eq!(c.click_key(&alt, 900, &mut s), Clicked::Momentary);
        assert_eq!(s.codes(Some(true)), &[key::KEY_LEFTALT, key::KEY_LEFTALT]);
        assert_eq!(c.mods().state(MOD_ALT), ModKeyState::Off);
    }

    #[test]
    fn double_clicked_momentary_locks_holds_and_unlocks() {
        let mut c = ctl();
        let mut s = Rec::default();
        let alt = Key::momentary("alt", key::KEY_LEFTALT, 25);
        let k = Key::letter("C", key::KEY_C);
        // The first click taps at once; the quick second click locks
        // without sending a second tap.
        assert_eq!(c.click_key(&alt, 100, &mut s), Clicked::Momentary);
        assert_eq!(
            c.click_key(&alt, 300, &mut s),
            Clicked::Latch(MOD_ALT, ModKeyState::Locked)
        );
        assert_eq!(s.codes(Some(true)), &[key::KEY_LEFTALT]);
        // Locked: Alt is held around every tap, and stays locked.
        s.ev.clear();
        c.click_key(&k, 400, &mut s);
        c.click_key(&k, 500, &mut s);
        assert_eq!(
            s.codes(Some(true)),
            &[key::KEY_LEFTALT, key::KEY_C, key::KEY_LEFTALT, key::KEY_C]
        );
        assert_eq!(c.mods().state(MOD_ALT), ModKeyState::Locked);
        // A click unlocks without a tap; the next key is plain.
        s.ev.clear();
        assert_eq!(c.click_key(&alt, 2000, &mut s), Clicked::Latch(MOD_ALT, ModKeyState::Off));
        c.click_key(&k, 2100, &mut s);
        assert_eq!(s.codes(Some(true)), &[key::KEY_C]);
    }

    #[test]
    fn win_tap_waits_out_the_double_click_window() {
        let mut c = ctl();
        let mut s = Rec::default();
        let win = Key::momentary("win", key::KEY_LEFTMETA, 25);
        assert_eq!(c.click_key(&win, 100, &mut s), Clicked::Momentary);
        assert!(s.ev.is_empty(), "nothing sent before the window passes");
        let due = c.tick_due().expect("a deferred tap is pending");
        assert!(!c.tick(due - 1, &mut s), "not due yet");
        assert!(c.tick(due, &mut s), "due: the tap goes out");
        assert_eq!(s.codes(None), &[key::KEY_LEFTMETA, key::KEY_LEFTMETA]);
        assert_eq!(c.tick_due(), None, "nothing left pending");
    }

    #[test]
    fn double_clicked_win_locks_without_any_tap() {
        let mut c = ctl();
        let mut s = Rec::default();
        let win = Key::momentary("win", key::KEY_LEFTMETA, 25);
        c.click_key(&win, 100, &mut s);
        assert_eq!(
            c.click_key(&win, 300, &mut s),
            Clicked::Latch(crate::mods::MOD_META, ModKeyState::Locked)
        );
        assert!(!c.tick(10_000, &mut s), "the deferred tap was dropped");
        assert!(s.ev.is_empty(), "a double-click must not tap Super: {:?}", s.ev);
        let k = Key::letter("E", key::KEY_E);
        c.click_key(&k, 400, &mut s);
        assert_eq!(s.codes(Some(true)), &[key::KEY_LEFTMETA, key::KEY_E]);
    }

    #[test]
    fn a_key_after_a_win_click_keeps_the_order() {
        let mut c = ctl();
        let mut s = Rec::default();
        let win = Key::momentary("win", key::KEY_LEFTMETA, 25);
        let k = Key::letter("E", key::KEY_E);
        c.click_key(&win, 100, &mut s);
        c.click_key(&k, 150, &mut s);
        assert_eq!(s.codes(Some(true)), &[key::KEY_LEFTMETA, key::KEY_E]);
        assert_eq!(c.tick_due(), None, "flushed, not sent twice");
    }

    #[test]
    fn latch_key_toggles_state() {
        let mut c = ctl();
        let mut s = Rec::default();
        let shift = Key::latch("\u{21e7}", key::KEY_LEFTSHIFT, 45);
        // Outside the double-click window: OFF -> LATCHED -> OFF -> ...
        assert_eq!(
            c.click_key(&shift, 100, &mut s),
            Clicked::Latch(0, ModKeyState::Latched)
        );
        assert_eq!(c.click_key(&shift, 600, &mut s), Clicked::Latch(0, ModKeyState::Off));
        assert_eq!(
            c.click_key(&shift, 700, &mut s),
            Clicked::Latch(crate::mods::MOD_SHIFT, ModKeyState::Latched)
        );
        // Two quick clicks lock the slot.
        assert_eq!(
            c.click_key(&shift, 700 + crate::mods::DOUBLE_CLICK_MS - 1, &mut s),
            Clicked::Latch(0, ModKeyState::Locked)
        );
    }

    #[test]
    fn caps_toggles_and_gaps_are_inert() {
        let mut c = ctl();
        let mut s = Rec::default();
        let caps = Key::caps("caps", key::KEY_CAPSLOCK, 45);
        assert_eq!(c.click_key(&caps, 0, &mut s), Clicked::Caps(true));
        assert_eq!(c.click_key(&caps, 10, &mut s), Clicked::Caps(false));
        let gap = Key::inert(None, 10);
        assert_eq!(c.click_key(&gap, 0, &mut s), Clicked::Inert);
        assert!(s.codes(None).is_empty());
    }

    #[test]
    fn commit_suggestion_erases_and_retypes_a_correction() {
        let mut c = ctl();
        // A misspelled partial whose top suggestion is a correction
        // (it does not extend "teh").
        for ch in "teh".chars() {
            c.predictor_mut().char(ch);
        }
        let suggs = c.predictor().suggestions(&[]);
        let Some((idx, word)) = suggs
            .iter()
            .enumerate()
            .find(|(_, w)| !w.starts_with("teh"))
            .map(|(i, w)| (i, *w))
        else {
            panic!("expected a spelling correction among the suggestions");
        };
        let partial = c.predictor().partial().to_string();
        let typed = c.predictor().typed().to_string();
        let mut out = [0 as char; 64];
        let (erase, n) =
            replace_word(word, &partial, &typed, Casing::default(), &mut out)
                .expect("fits the buffer");
        assert!(erase > 0, "a correction erases the mistyped tail");
        let mut s = Rec::default();
        c.commit_suggestion(idx, &mut s).expect("commits");
        // erase backspaces first, then the corrected word on the keymap
        let map = CharMap::new().expect("default keymap");
        let mut want: Vec<u16> = Vec::new();
        want.extend(std::iter::repeat_n(key::KEY_BACKSPACE, erase));
        for ch in out[..n].iter() {
            if *ch == ' ' {
                want.push(key::KEY_SPACE);
            } else {
                let (code, shift) = map.char_to_key(*ch).expect("mapped");
                if shift {
                    want.push(key::KEY_LEFTSHIFT);
                }
                want.push(code);
            }
        }
        assert_eq!(s.codes(Some(true)), want);
        // The commit consumed the partial.
        assert_eq!(c.predictor().partial(), "");
        assert_eq!(c.predictor().typed(), "");
    }

    #[test]
    fn no_suggestions_until_the_sentence_starts() {
        let mut c = ctl();
        let mut s = Rec::default();
        // Learned history gives the predictor sentence openers.
        for w in ["hello", "hello", "hello"] {
            c.predictor_mut().commit(w);
            c.predictor_mut().end_message();
        }
        assert!(!c.predictor().suggestions(&[]).is_empty(), "the predictor has openers");
        assert_eq!(c.suggestions(), Vec::<&str>::new(), "launch: empty row");
        c.tap_key(&Key::letter("i", key::KEY_I), &mut s);
        assert!(!c.suggestions().is_empty());
        c.tap_key(&Key::wide("enter", key::KEY_ENTER, 40), &mut s);
        assert_eq!(c.suggestions(), Vec::<&str>::new(), "after Enter: empty row");
    }

    #[test]
    fn a_host_change_to_the_predictor_reaches_the_row() {
        let mut c = ctl();
        c.tap_key(&Key::letter("t", key::KEY_T), &mut Rec::default());
        let before = c.suggestions();
        c.predictor_mut().char('h');
        let after = c.suggestions();
        assert_ne!(after, before);
        assert_eq!(after, c.predictor().suggestions(&[]));
    }

    #[cfg(feature = "neural")]
    fn neural_ctl() -> Controller {
        Controller::with_neural(
            ControllerConfig::default(),
            Predictor::ephemeral(),
            NeuralEngine::new(),
        )
    }

    /// Wait until the engine has answered `prompt`.
    #[cfg(feature = "neural")]
    fn wait_for_answer(c: &Controller, prompt: &str) {
        let start = std::time::Instant::now();
        while c.neural.as_ref().unwrap().answer(prompt).is_none() {
            assert!(start.elapsed().as_secs() < 120, "no answer for {prompt:?}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[cfg(feature = "neural")]
    #[test]
    fn neural_words_requested_mid_word_lead_the_row_at_the_space() {
        let mut c = neural_ctl();
        let mut s = Rec::default();
        c.tap_key(&Key::letter("i", key::KEY_I), &mut s);
        // Typing pace: the answer for "i" lands before the space.
        wait_for_answer(&c, "i");
        c.tap_key(&Key::space(100), &mut s);
        let row = c.suggestions();
        assert!(!row.is_empty());
        assert_ne!(row, c.predictor().suggestions(&[]), "the neural words lead the row");
    }

    #[cfg(feature = "neural")]
    #[test]
    fn neural_words_lead_the_row_after_a_pill_click() {
        let mut c = neural_ctl();
        let mut s = Rec::default();
        c.tap_key(&Key::letter("w", key::KEY_W), &mut s);
        let last = c.suggestions().len() - 1;
        let after = c.predictor().neural_prompt_after(c.suggestions()[last]).unwrap();
        // The click comes once that pill's answer has landed.
        wait_for_answer(&c, &after);
        c.commit_suggestion(last, &mut s).expect("commits");
        let row = c.suggestions();
        assert!(!row.is_empty());
        assert_ne!(row, c.predictor().suggestions(&[]), "the neural words lead the row");
    }

    #[cfg(feature = "neural")]
    #[test]
    fn row_never_changes_between_keystrokes() {
        let mut c = neural_ctl();
        let mut s = Rec::default();
        c.tap_key(&Key::letter("i", key::KEY_I), &mut s);
        // The space beats the answer (the first one pays the model load).
        c.tap_key(&Key::space(100), &mut s);
        let row = c.suggestions();
        assert_eq!(row, c.predictor().suggestions(&[]), "the n-gram row shows at once");
        wait_for_answer(&c, "i");
        assert_eq!(c.suggestions(), row, "the late answer never replaces it");
    }
}
