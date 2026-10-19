//! 3-state modifier engine. Each modifier (shift/ctrl/alt/meta) is OFF,
//! LATCHED (armed for the next single key), or LOCKED (stays active until
//! clicked again). Caps Lock is a plain on/off toggle.
//!
//! The engine is the single source of truth for modifier state. The UI
//! calls [`ModifierEngine::click`] on modifier-key presses and
//! [`ModifierEngine::consume_latched`] after every tap; tap-event
//! generation reads the state through [`ModifierEngine::shift_active`]
//! and [`ModifierEngine::sticky`].

use crate::events::StickyMods;
use crate::key::{key, Click, Key, KeyKind};

/// State of one modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModKeyState {
    #[default]
    Off = 0,
    Latched = 1,
    Locked = 2,
}

impl ModKeyState {
    /// Latched and locked modifiers both apply to the next key.
    #[inline]
    pub fn is_active(self) -> bool {
        self != ModKeyState::Off
    }
}

/// Modifier slot indices, in the C `OSK_MOD_*` order.
pub const MOD_SHIFT: usize = 0;
pub const MOD_CTRL: usize = 1;
pub const MOD_ALT: usize = 2;
pub const MOD_META: usize = 3;
pub const NMODS: usize = 4;

/// Double-click window in ms. A click on a LATCHED modifier inside the
/// window locks it; outside, it turns off.
pub const DOUBLE_CLICK_MS: u64 = 400;

/// Modifier slot for a KEY_* code: `Some(0)`=shift, `1`=ctrl, `2`=alt,
/// `3`=meta; `None` for non-modifier keys (including Caps Lock).
pub fn mod_index(code: u16) -> Option<usize> {
    match code {
        key::KEY_LEFTSHIFT | key::KEY_RIGHTSHIFT => Some(MOD_SHIFT),
        key::KEY_LEFTCTRL | key::KEY_RIGHTCTRL => Some(MOD_CTRL),
        key::KEY_LEFTALT | key::KEY_RIGHTALT => Some(MOD_ALT),
        key::KEY_LEFTMETA | key::KEY_RIGHTMETA => Some(MOD_META),
        _ => None,
    }
}

/// Next state after a click at `now` (ms), given the previous state and
/// its last-click time.
pub fn next_state(cur: ModKeyState, now: u64, last_click: u64) -> ModKeyState {
    match cur {
        ModKeyState::Off => ModKeyState::Latched,
        ModKeyState::Latched => match now.checked_sub(last_click) {
            Some(d) if d < DOUBLE_CLICK_MS => ModKeyState::Locked,
            _ => ModKeyState::Off,
        },
        ModKeyState::Locked => ModKeyState::Off,
    }
}

/// The Shift and Caps Lock state that decides what case a key types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Casing {
    pub shift: bool,
    pub caps: bool,
}

impl Casing {
    /// Whether a letter types uppercase: Caps Lock inverts Shift.
    #[inline]
    pub const fn letter_upper(self) -> bool {
        self.shift ^ self.caps
    }

    /// Whether a digit or symbol types its shifted form: Caps Lock
    /// does not apply.
    #[inline]
    pub const fn symbol_shift(self) -> bool {
        self.shift
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ModifierEngine {
    state: [ModKeyState; NMODS],
    /// Actual KEY_* (L vs R) of the key that armed each slot.
    code: [u16; NMODS],
    last_click_ms: [u64; NMODS],
    /// Time of the last standalone tap on a momentary key per slot,
    /// for double-click locking; None until the first tap.
    tap_ms: [Option<u64>; NMODS],
    /// A deferred standalone tap (code, due time) waiting out the
    /// double-click window; see [`defers_tap`].
    deferred: Option<(u16, u64)>,
    caps_lock: bool,
}

/// Whether a momentary slot waits out the double-click window before
/// sending its standalone tap. Win does: a lone Super tap makes the
/// compositor or shell act at once (e.g. open a search overlay over
/// the keyboard), which would swallow the second click of a
/// double-click. Alt taps at once, so its toggles stay instant.
fn defers_tap(slot: usize) -> bool {
    slot == MOD_META
}

/// What a click on a momentary modifier (Win, Alt) did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MomentaryClick {
    /// Send one standalone press+release.
    Tap,
    /// A second click within [`DOUBLE_CLICK_MS`] locked the slot: it is
    /// held around every tap until clicked again. No second tap is sent.
    Locked,
    /// The slot was locked; this click released it. Nothing is sent.
    Unlocked,
    /// The standalone tap is deferred until the double-click window
    /// passes ([`ModifierEngine::take_due_tap`]); nothing is sent yet.
    Deferred,
}

impl ModifierEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Click a modifier key at `now` (ms). Applies the state transition
    /// and records which physical key armed the slot, so only that key
    /// lights up. Non-modifier codes are ignored.
    pub fn click(&mut self, code: u16, now: u64) {
        let Some(slot) = mod_index(code) else {
            return;
        };
        self.state[slot] = next_state(self.state[slot], now, self.last_click_ms[slot]);
        self.last_click_ms[slot] = now;
        self.code[slot] = code;
    }

    /// Click a momentary modifier (Win, Alt) at `now` (ms). A single
    /// click is a standalone tap; a second click within
    /// [`DOUBLE_CLICK_MS`] locks the slot like a double-clicked Ctrl;
    /// a click on a locked slot unlocks it. Non-modifier codes tap.
    pub fn momentary_click(&mut self, code: u16, now: u64) -> MomentaryClick {
        let Some(slot) = mod_index(code) else {
            return MomentaryClick::Tap;
        };
        if self.state[slot] == ModKeyState::Locked {
            self.state[slot] = ModKeyState::Off;
            self.tap_ms[slot] = None;
            return MomentaryClick::Unlocked;
        }
        if self.tap_ms[slot].is_some_and(|t| now.saturating_sub(t) < DOUBLE_CLICK_MS) {
            self.state[slot] = ModKeyState::Locked;
            self.code[slot] = code;
            self.tap_ms[slot] = None;
            // A deferred first tap is dropped: the double-click locks
            // instead of tapping.
            if self.deferred.is_some_and(|(c, _)| mod_index(c) == Some(slot)) {
                self.deferred = None;
            }
            return MomentaryClick::Locked;
        }
        self.tap_ms[slot] = Some(now);
        if defers_tap(slot) {
            self.deferred = Some((code, now + DOUBLE_CLICK_MS));
            return MomentaryClick::Deferred;
        }
        MomentaryClick::Tap
    }

    /// The deferred standalone tap's code once it is due at `now` (or
    /// at once with `force`, before another key so the order holds),
    /// clearing it; None when nothing is pending or it is not due.
    pub fn take_due_tap(&mut self, now: u64, force: bool) -> Option<u16> {
        let (code, due) = self.deferred?;
        if force || now >= due {
            self.deferred = None;
            Some(code)
        } else {
            None
        }
    }

    /// The modifier slot of the pending deferred tap, if any.
    pub fn deferred_slot(&self) -> Option<usize> {
        self.deferred.and_then(|(code, _)| mod_index(code))
    }

    /// When the deferred standalone tap is due, if one is pending.
    pub fn deferred_due(&self) -> Option<u64> {
        self.deferred.map(|(_, due)| due)
    }

    pub fn toggle_caps(&mut self) {
        self.caps_lock = !self.caps_lock;
    }

    #[inline]
    pub fn caps_lock(&self) -> bool {
        self.caps_lock
    }

    #[inline]
    pub fn state(&self, slot: usize) -> ModKeyState {
        self.state[slot]
    }

    #[inline]
    pub fn code(&self, slot: usize) -> u16 {
        self.code[slot]
    }

    /// Consume latched modifiers (latched -> off); locked modifiers stay.
    /// Called after every tap and suggestion commit.
    pub fn consume_latched(&mut self) {
        for s in &mut self.state {
            if *s == ModKeyState::Latched {
                *s = ModKeyState::Off;
            }
        }
    }

    /// Shift applies when its slot is latched or locked.
    #[inline]
    pub fn shift_active(&self) -> bool {
        self.state[MOD_SHIFT].is_active()
    }

    /// The current [`Casing`] for tap events and word completion.
    #[inline]
    pub fn casing(&self) -> Casing {
        Casing {
            shift: self.shift_active(),
            caps: self.caps_lock,
        }
    }

    /// Sticky ctrl/alt/meta for tap-event generation, taken from the
    /// engine slots.
    pub fn sticky(&self) -> StickyMods {
        StickyMods {
            ctrl: self.state[MOD_CTRL]
                .is_active()
                .then_some(self.code[MOD_CTRL]),
            alt: self.state[MOD_ALT]
                .is_active()
                .then_some(self.code[MOD_ALT]),
            meta: self.state[MOD_META]
                .is_active()
                .then_some(self.code[MOD_META]),
        }
    }

    /// Visual state of a key for rendering: latching modifier keys return
    /// their slot state (only the physical key that was latched/locked
    /// lights up), caps returns LOCKED when caps_lock is on, everything
    /// else (tap keys, momentary keys, gaps) returns OFF.
    pub fn key_visual(&self, k: &Key) -> ModKeyState {
        match k.kind {
            KeyKind::Inject {
                code,
                click: Click::Caps,
            } if code.get() == key::KEY_CAPSLOCK => {
                return if self.caps_lock {
                    ModKeyState::Locked
                } else {
                    ModKeyState::Off
                };
            }
            KeyKind::Inject {
                code,
                click: Click::Latch | Click::Momentary,
            } => {
                if let Some(idx) = mod_index(code.get()) {
                    if self.state[idx].is_active() && self.code[idx] == code.get() {
                        return self.state[idx];
                    }
                }
            }
            _ => {}
        }
        ModKeyState::Off
    }
}
