//! Linux input event codes (KEY_*) and the key/row/layout types.
//!
//! The KEY_* value is the single currency of osk: layouts produce them and
//! both injection backends (Wayland virtual-keyboard and /dev/uinput)
//! consume them directly. Values come from `<linux/input-event-codes.h>`.
//!
//! Key behavior is data: [`KeyKind`] says whether a key injects (and how
//! its click behaves) or is an inert [`KeyKind::Gap`]. There is no
//! code-0 sentinel; `tap_events` takes a [`TapKey`] that can only be
//! built from a [`Click::Tap`] key.

use std::num::NonZeroU16;

/// Linux KEY_* event codes used by the osk layouts.
#[allow(clippy::module_inception)]
pub mod key {
    pub const KEY_ESC: u16 = 1;
    pub const KEY_1: u16 = 2;
    pub const KEY_2: u16 = 3;
    pub const KEY_3: u16 = 4;
    pub const KEY_4: u16 = 5;
    pub const KEY_5: u16 = 6;
    pub const KEY_6: u16 = 7;
    pub const KEY_7: u16 = 8;
    pub const KEY_8: u16 = 9;
    pub const KEY_9: u16 = 10;
    pub const KEY_0: u16 = 11;
    pub const KEY_MINUS: u16 = 12;
    pub const KEY_EQUAL: u16 = 13;
    pub const KEY_BACKSPACE: u16 = 14;
    pub const KEY_TAB: u16 = 15;
    pub const KEY_Q: u16 = 16;
    pub const KEY_W: u16 = 17;
    pub const KEY_E: u16 = 18;
    pub const KEY_R: u16 = 19;
    pub const KEY_T: u16 = 20;
    pub const KEY_Y: u16 = 21;
    pub const KEY_U: u16 = 22;
    pub const KEY_I: u16 = 23;
    pub const KEY_O: u16 = 24;
    pub const KEY_P: u16 = 25;
    pub const KEY_LEFTBRACE: u16 = 26;
    pub const KEY_RIGHTBRACE: u16 = 27;
    pub const KEY_ENTER: u16 = 28;
    pub const KEY_LEFTCTRL: u16 = 29;
    pub const KEY_A: u16 = 30;
    pub const KEY_S: u16 = 31;
    pub const KEY_D: u16 = 32;
    pub const KEY_F: u16 = 33;
    pub const KEY_G: u16 = 34;
    pub const KEY_H: u16 = 35;
    pub const KEY_J: u16 = 36;
    pub const KEY_K: u16 = 37;
    pub const KEY_L: u16 = 38;
    pub const KEY_SEMICOLON: u16 = 39;
    pub const KEY_APOSTROPHE: u16 = 40;
    pub const KEY_GRAVE: u16 = 41;
    pub const KEY_LEFTSHIFT: u16 = 42;
    pub const KEY_BACKSLASH: u16 = 43;
    pub const KEY_Z: u16 = 44;
    pub const KEY_X: u16 = 45;
    pub const KEY_C: u16 = 46;
    pub const KEY_V: u16 = 47;
    pub const KEY_B: u16 = 48;
    pub const KEY_N: u16 = 49;
    pub const KEY_M: u16 = 50;
    pub const KEY_COMMA: u16 = 51;
    pub const KEY_DOT: u16 = 52;
    pub const KEY_SLASH: u16 = 53;
    pub const KEY_RIGHTSHIFT: u16 = 54;
    pub const KEY_LEFTALT: u16 = 56;
    pub const KEY_SPACE: u16 = 57;
    pub const KEY_CAPSLOCK: u16 = 58;
    pub const KEY_F1: u16 = 59;
    pub const KEY_F2: u16 = 60;
    pub const KEY_F3: u16 = 61;
    pub const KEY_F4: u16 = 62;
    pub const KEY_F5: u16 = 63;
    pub const KEY_F6: u16 = 64;
    pub const KEY_F7: u16 = 65;
    pub const KEY_F8: u16 = 66;
    pub const KEY_F9: u16 = 67;
    pub const KEY_F10: u16 = 68;
    pub const KEY_SCROLLLOCK: u16 = 70;
    pub const KEY_F11: u16 = 87;
    pub const KEY_F12: u16 = 88;
    pub const KEY_RIGHTCTRL: u16 = 97;
    pub const KEY_SYSRQ: u16 = 99;
    pub const KEY_RIGHTALT: u16 = 100;
    pub const KEY_HOME: u16 = 102;
    pub const KEY_UP: u16 = 103;
    pub const KEY_PAGEUP: u16 = 104;
    pub const KEY_LEFT: u16 = 105;
    pub const KEY_RIGHT: u16 = 106;
    pub const KEY_END: u16 = 107;
    pub const KEY_DOWN: u16 = 108;
    pub const KEY_PAGEDOWN: u16 = 109;
    pub const KEY_INSERT: u16 = 110;
    pub const KEY_DELETE: u16 = 111;
    pub const KEY_PAUSE: u16 = 119;
    pub const KEY_LEFTMETA: u16 = 125;
    pub const KEY_RIGHTMETA: u16 = 126;
    pub const KEY_KPASTERISK: u16 = 55;
    pub const KEY_KPMINUS: u16 = 74;
    pub const KEY_KPPLUS: u16 = 78;
    pub const KEY_KPENTER: u16 = 96;
    pub const KEY_KPSLASH: u16 = 98;
    pub const KEY_KPEQUAL: u16 = 117;
}

/// Build a [`NonZeroU16`] in a `const` context: `0` is a [`KeyKind::Gap`],
/// never an injectable code.
pub const fn nz(code: u16) -> NonZeroU16 {
    match NonZeroU16::new(code) {
        Some(v) => v,
        None => panic!("key code 0 is a Gap, use Key::inert"),
    }
}

/// How a click on an injectable key behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Normal key: injects via `tap_events`.
    Tap,
    /// Win/Alt style: one press injects one press+release, no latch.
    Momentary,
    /// Shift/Ctrl style: runs the 3-state latch machine.
    Latch,
    /// Caps Lock: flips the caps toggle.
    Caps,
}

/// Whether a key injects and how, or is an inert spacing gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    /// An injectable key.
    Inject {
        /// KEY_* code, never 0.
        code: NonZeroU16,
        /// Click behavior.
        click: Click,
    },
    /// Drawn (or skipped) but never injected. `Key::inert(None, _)` is
    /// the 0.5u spacing gap.
    Gap,
}

/// A key that can be tapped: the proof that [`KeyKind`] was `Tap`.
/// [`tap_events`](crate::events::tap_events) takes this, so a gap or a
/// modifier key cannot reach injection at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapKey {
    code: NonZeroU16,
    letter: bool,
}

impl TapKey {
    /// The KEY_* code to inject.
    #[inline]
    pub const fn code(self) -> NonZeroU16 {
        self.code
    }

    /// The KEY_* code as a plain `u16`.
    #[inline]
    pub const fn code_u16(self) -> u16 {
        self.code.get()
    }

    /// Whether case follows the modifier state.
    #[inline]
    pub const fn is_letter(self) -> bool {
        self.letter
    }
}

/// One key. Injectable keys carry their KEY_* code in [`KeyKind`];
/// gaps carry none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    /// Base (unshifted) display text; `None` = blank (space bar).
    pub label: Option<&'static str>,
    /// Inject behavior or gap.
    pub kind: KeyKind,
    /// `true` = alphabetic; case follows modifier state.
    pub letter: bool,
    /// Width in 1/20 key units (20 = normal).
    pub width: u16,
    /// `true` = repeats while held.
    pub repeat: bool,
    /// Shifted display text (numbers/symbols); `None` = none.
    pub shift_label: Option<&'static str>,
}

impl Key {
    /// Whether this key is an inert gap.
    #[inline]
    pub const fn is_gap(&self) -> bool {
        matches!(self.kind, KeyKind::Gap)
    }

    /// The KEY_* code, or `None` for a gap.
    #[inline]
    pub fn code(&self) -> Option<NonZeroU16> {
        match self.kind {
            KeyKind::Inject { code, .. } => Some(code),
            KeyKind::Gap => None,
        }
    }

    /// The KEY_* code as `u16`, or `0` for a gap (for table checks and
    /// predictor dispatch; injection uses [`TapKey`]).
    #[inline]
    pub fn code_u16(&self) -> u16 {
        match self.kind {
            KeyKind::Inject { code, .. } => code.get(),
            KeyKind::Gap => 0,
        }
    }

    /// Click behavior, or `None` for a gap.
    #[inline]
    pub fn click(&self) -> Option<Click> {
        match self.kind {
            KeyKind::Inject { click, .. } => Some(click),
            KeyKind::Gap => None,
        }
    }

    /// The tappable projection: `Some` only for [`Click::Tap`] keys.
    #[inline]
    pub fn tap(&self) -> Option<TapKey> {
        match self.kind {
            KeyKind::Inject {
                code,
                click: Click::Tap,
            } => Some(TapKey {
                code,
                letter: self.letter,
            }),
            _ => None,
        }
    }
    /// A key with no shifted label and no special flags.
    pub const fn plain(label: &'static str, code: u16, width: u16) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Tap,
            },
            letter: false,
            width,
            repeat: false,
            shift_label: None,
        }
    }

    /// An alphabetic key: repeats, case follows the modifier state.
    pub const fn letter(label: &'static str, code: u16) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Tap,
            },
            letter: true,
            width: 20,
            repeat: true,
            shift_label: None,
        }
    }

    /// A number/symbol key: repeats, has a shifted label.
    pub const fn sym(label: &'static str, code: u16, shift_label: &'static str) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Tap,
            },
            letter: false,
            width: 20,
            repeat: true,
            shift_label: Some(shift_label),
        }
    }

    /// A number/symbol key with a custom width and a shifted label.
    pub const fn sym_wide(
        label: &'static str,
        code: u16,
        shift_label: &'static str,
        width: u16,
    ) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Tap,
            },
            letter: false,
            width,
            repeat: true,
            shift_label: Some(shift_label),
        }
    }
    /// A wide special key (Tab, Enter, Backspace, arrows, ...).
    pub const fn wide(label: &'static str, code: u16, width: u16) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Tap,
            },
            letter: false,
            width,
            repeat: false,
            shift_label: None,
        }
    }

    /// A latching modifier (Shift/Ctrl): runs the 3-state machine
    /// (OFF -> LATCHED -> LOCKED -> OFF). Does not repeat.
    pub const fn latch(label: &'static str, code: u16, width: u16) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Latch,
            },
            letter: false,
            width,
            repeat: false,
            shift_label: None,
        }
    }

    /// Caps Lock: flips the caps toggle. Does not repeat.
    pub const fn caps(label: &'static str, code: u16, width: u16) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Caps,
            },
            letter: false,
            width,
            repeat: false,
            shift_label: None,
        }
    }

    /// A momentary modifier (Win/Alt): one press injects one
    /// press+release, never latches. Does not repeat.
    pub const fn momentary(label: &'static str, code: u16, width: u16) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Momentary,
            },
            letter: false,
            width,
            repeat: false,
            shift_label: None,
        }
    }

    /// A wide key that repeats (Backspace, arrow keys).
    pub const fn wide_repeat(label: &'static str, code: u16, width: u16) -> Self {
        Key {
            label: Some(label),
            kind: KeyKind::Inject {
                code: nz(code),
                click: Click::Tap,
            },
            letter: false,
            width,
            repeat: true,
            shift_label: None,
        }
    }

    /// A space bar: blank, wide, repeats.
    pub const fn space(width: u16) -> Self {
        Key {
            label: None,
            kind: KeyKind::Inject {
                code: nz(key::KEY_SPACE),
                click: Click::Tap,
            },
            letter: false,
            width,
            repeat: true,
            shift_label: None,
        }
    }

    /// An inert gap: drawn (when labeled) but never injected. Used for
    /// the 0.5u gap before the arrow cluster.
    pub const fn inert(label: Option<&'static str>, width: u16) -> Self {
        Key {
            label,
            kind: KeyKind::Gap,
            letter: false,
            width,
            repeat: false,
            shift_label: None,
        }
    }
}

/// A horizontal run of keys.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub keys: &'static [Key],
}

impl Row {
    /// Number of keys in the row.
    #[inline]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the row has no keys.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Sum of the key widths, in 1/20 key units.
    pub fn width(&self) -> u16 {
        self.keys.iter().map(|k| k.width).sum()
    }
}

/// A keyboard profile: a named set of rows.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// Profile name, e.g. "65%".
    pub name: &'static str,
    pub rows: &'static [Row],
}

impl Layout {
    /// Number of rows.
    #[inline]
    pub fn nrows(&self) -> usize {
        self.rows.len()
    }
}
