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
use crate::key::{key, Key};

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

#[derive(Clone, Copy, Debug, Default)]
pub struct ModifierEngine {
    state: [ModKeyState; NMODS],
    /// Actual KEY_* (L vs R) of the key that armed each slot.
    code: [u16; NMODS],
    last_click_ms: [u64; NMODS],
    caps_lock: bool,
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

    /// Visual state of a key for rendering: modifier keys return their
    /// slot state (only the physical key that was latched/locked lights
    /// up), caps returns LOCKED when caps_lock is on, everything else
    /// returns OFF.
    pub fn key_visual(&self, k: &Key) -> ModKeyState {
        if k.toggle && k.code == key::KEY_CAPSLOCK {
            return if self.caps_lock {
                ModKeyState::Locked
            } else {
                ModKeyState::Off
            };
        }
        if let Some(idx) = mod_index(k.code) {
            if self.state[idx].is_active() && self.code[idx] == k.code {
                return self.state[idx];
            }
        }
        ModKeyState::Off
    }
}
