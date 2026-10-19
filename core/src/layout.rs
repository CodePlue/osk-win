//! The 65% keyboard profile.
//!
//! Key codes are linux input event codes (KEY_*), exactly what both
//! injection backends consume.
//!
//! Widths are in 1/20 key units (20 = 1.0u). A gap key is inert:
//! drawn when labeled, never injected. `Key::inert(None, _)` is a
//! spacing gap.

use crate::key::{key, Key, Layout, Row};

// ---- 65%: number row through modifier row, nav keys on the right
// ---- edge, arrows bottom-right. All rows sum to 320 (16.0u).
const P65_ROW0: [Key; 15] = [
     Key::wide("esc", key::KEY_ESC, 20),
    Key::sym("1", key::KEY_1, "!"),
    Key::sym("2", key::KEY_2, "@"),
    Key::sym("3", key::KEY_3, "#"),
    Key::sym("4", key::KEY_4, "$"),
    Key::sym("5", key::KEY_5, "%"),
    Key::sym("6", key::KEY_6, "^"),
    Key::sym("7", key::KEY_7, "&"),
    Key::sym("8", key::KEY_8, "*"),
    Key::sym("9", key::KEY_9, "("),
    Key::sym("0", key::KEY_0, ")"),
    Key::sym("-", key::KEY_MINUS, "_"),
    Key::sym("=", key::KEY_EQUAL, "+"),
     Key::wide_repeat("\u{232B}", key::KEY_BACKSPACE, 40), // ⌫
     Key::wide("home", key::KEY_HOME, 20),
];

const P65_ROW1: [Key; 15] = [
     Key::wide("\u{21E5}", key::KEY_TAB, 30), // ⇥
    Key::letter("Q", key::KEY_Q),
    Key::letter("W", key::KEY_W),
    Key::letter("E", key::KEY_E),
    Key::letter("R", key::KEY_R),
    Key::letter("T", key::KEY_T),
    Key::letter("Y", key::KEY_Y),
    Key::letter("U", key::KEY_U),
    Key::letter("I", key::KEY_I),
    Key::letter("O", key::KEY_O),
    Key::letter("P", key::KEY_P),
    Key::sym("[", key::KEY_LEFTBRACE, "{"),
    Key::sym("]", key::KEY_RIGHTBRACE, "}"),
    Key::sym_wide("\\", key::KEY_BACKSLASH, "|", 30),
     Key::wide("pgup", key::KEY_PAGEUP, 20),
];

const P65_ROW2: [Key; 14] = [
     Key::caps("caps", key::KEY_CAPSLOCK, 35),
    Key::letter("A", key::KEY_A),
    Key::letter("S", key::KEY_S),
    Key::letter("D", key::KEY_D),
    Key::letter("F", key::KEY_F),
    Key::letter("G", key::KEY_G),
    Key::letter("H", key::KEY_H),
    Key::letter("J", key::KEY_J),
    Key::letter("K", key::KEY_K),
    Key::letter("L", key::KEY_L),
    Key::sym(";", key::KEY_SEMICOLON, ":"),
    Key::sym("'", key::KEY_APOSTROPHE, "\""),
     Key::wide("\u{21B5}", key::KEY_ENTER, 45), // ↵
     Key::wide("pgdn", key::KEY_PAGEDOWN, 20),
];

const P65_ROW3: [Key; 14] = [
     Key::latch("\u{21E7}", key::KEY_LEFTSHIFT, 45), // ⇧
    Key::letter("Z", key::KEY_Z),
    Key::letter("X", key::KEY_X),
    Key::letter("C", key::KEY_C),
    Key::letter("V", key::KEY_V),
    Key::letter("B", key::KEY_B),
    Key::letter("N", key::KEY_N),
    Key::letter("M", key::KEY_M),
    Key::sym(",", key::KEY_COMMA, "<"),
    Key::sym(".", key::KEY_DOT, ">"),
    Key::sym("/", key::KEY_SLASH, "?"),
     Key::latch("\u{21E7}", key::KEY_RIGHTSHIFT, 35), // ⇧
    Key::wide_repeat("\u{2191}", key::KEY_UP, 20),             // ↑
    Key::wide("end", key::KEY_END, 20),
];

const P65_ROW4: [Key; 10] = [
     Key::latch("ctrl", key::KEY_LEFTCTRL, 25),
     Key::momentary("win", key::KEY_LEFTMETA, 25),
     Key::momentary("alt", key::KEY_LEFTALT, 25),
    Key::space(125),
     Key::momentary("alt", key::KEY_RIGHTALT, 25),
     Key::latch("ctrl", key::KEY_RIGHTCTRL, 25),
    // 0.5u gap before the arrow cluster
    Key::inert(None, 10),
    Key::wide_repeat("\u{2190}", key::KEY_LEFT, 20), // ←
    Key::wide_repeat("\u{2193}", key::KEY_DOWN, 20), // ↓
    Key::wide_repeat("\u{2192}", key::KEY_RIGHT, 20), // →
];

const ROWS_65: [Row; 5] = [
    Row { keys: &P65_ROW0 },
    Row { keys: &P65_ROW1 },
    Row { keys: &P65_ROW2 },
    Row { keys: &P65_ROW3 },
    Row { keys: &P65_ROW4 },
];

pub const LAYOUT_65: Layout = Layout {
    name: "65%",
    rows: &ROWS_65,
};

// ---- 65% + Numpad: 20.5u (410 units) wide, 65% layout with numeric keypad on the right ----
const NUMPAD_ROW0: [Key; 20] = [
    Key::wide("esc", key::KEY_ESC, 20),
    Key::sym("1", key::KEY_1, "!"),
    Key::sym("2", key::KEY_2, "@"),
    Key::sym("3", key::KEY_3, "#"),
    Key::sym("4", key::KEY_4, "$"),
    Key::sym("5", key::KEY_5, "%"),
    Key::sym("6", key::KEY_6, "^"),
    Key::sym("7", key::KEY_7, "&"),
    Key::sym("8", key::KEY_8, "*"),
    Key::sym("9", key::KEY_9, "("),
    Key::sym("0", key::KEY_0, ")"),
    Key::sym("-", key::KEY_MINUS, "_"),
    Key::sym("=", key::KEY_EQUAL, "+"),
    Key::wide_repeat("\u{232B}", key::KEY_BACKSPACE, 40),
    Key::wide("home", key::KEY_HOME, 20),
    // 0.5u gap before the numpad
    Key::inert(None, 10),
    Key::wide("del", key::KEY_DELETE, 20),
    Key::sym_wide("/", key::KEY_KPSLASH, "/", 20),
    Key::sym_wide("*", key::KEY_KPASTERISK, "*", 20),
    Key::wide_repeat("\u{232B}", key::KEY_BACKSPACE, 20),
];

const NUMPAD_ROW1: [Key; 20] = [
    Key::wide("\u{21E5}", key::KEY_TAB, 30),
    Key::letter("Q", key::KEY_Q),
    Key::letter("W", key::KEY_W),
    Key::letter("E", key::KEY_E),
    Key::letter("R", key::KEY_R),
    Key::letter("T", key::KEY_T),
    Key::letter("Y", key::KEY_Y),
    Key::letter("U", key::KEY_U),
    Key::letter("I", key::KEY_I),
    Key::letter("O", key::KEY_O),
    Key::letter("P", key::KEY_P),
    Key::sym("[", key::KEY_LEFTBRACE, "{"),
    Key::sym("]", key::KEY_RIGHTBRACE, "}"),
    Key::sym_wide("\\", key::KEY_BACKSLASH, "|", 30),
    Key::wide("pgup", key::KEY_PAGEUP, 20),
    // 0.5u gap before the numpad
    Key::inert(None, 10),
    Key::sym("7", key::KEY_7, "&"),
    Key::sym("8", key::KEY_8, "*"),
    Key::sym("9", key::KEY_9, "("),
    Key::sym_wide("-", key::KEY_KPMINUS, "-", 20),
];

const NUMPAD_ROW2: [Key; 19] = [
    Key::caps("caps", key::KEY_CAPSLOCK, 35),
    Key::letter("A", key::KEY_A),
    Key::letter("S", key::KEY_S),
    Key::letter("D", key::KEY_D),
    Key::letter("F", key::KEY_F),
    Key::letter("G", key::KEY_G),
    Key::letter("H", key::KEY_H),
    Key::letter("J", key::KEY_J),
    Key::letter("K", key::KEY_K),
    Key::letter("L", key::KEY_L),
    Key::sym(";", key::KEY_SEMICOLON, ":"),
    Key::sym("'", key::KEY_APOSTROPHE, "\""),
    Key::wide("\u{21B5}", key::KEY_ENTER, 45),
    Key::wide("pgdn", key::KEY_PAGEDOWN, 20),
    // 0.5u gap before the numpad
    Key::inert(None, 10),
    Key::sym("4", key::KEY_4, "$"),
    Key::sym("5", key::KEY_5, "%"),
    Key::sym("6", key::KEY_6, "^"),
    Key::sym_wide("+", key::KEY_KPPLUS, "+", 20),
];

const NUMPAD_ROW3: [Key; 19] = [
    Key::latch("\u{21E7}", key::KEY_LEFTSHIFT, 45),
    Key::letter("Z", key::KEY_Z),
    Key::letter("X", key::KEY_X),
    Key::letter("C", key::KEY_C),
    Key::letter("V", key::KEY_V),
    Key::letter("B", key::KEY_B),
    Key::letter("N", key::KEY_N),
    Key::letter("M", key::KEY_M),
    Key::sym(",", key::KEY_COMMA, "<"),
    Key::sym(".", key::KEY_DOT, ">"),
    Key::sym("/", key::KEY_SLASH, "?"),
    Key::latch("\u{21E7}", key::KEY_RIGHTSHIFT, 35),
    Key::wide_repeat("\u{2191}", key::KEY_UP, 20),
    Key::wide("end", key::KEY_END, 20),
    // 0.5u gap before the numpad
    Key::inert(None, 10),
    Key::sym("1", key::KEY_1, "!"),
    Key::sym("2", key::KEY_2, "@"),
    Key::sym("3", key::KEY_3, "#"),
    Key::sym_wide("=", key::KEY_KPEQUAL, "=", 20),
];

const NUMPAD_ROW4: [Key; 14] = [
    Key::latch("ctrl", key::KEY_LEFTCTRL, 25),
    Key::momentary("win", key::KEY_LEFTMETA, 25),
    Key::momentary("alt", key::KEY_LEFTALT, 25),
    Key::space(125),
    Key::momentary("alt", key::KEY_RIGHTALT, 25),
    Key::latch("ctrl", key::KEY_RIGHTCTRL, 25),
    // 0.5u gap before the arrow cluster
    Key::inert(None, 10),
    Key::wide_repeat("\u{2190}", key::KEY_LEFT, 20),
    Key::wide_repeat("\u{2193}", key::KEY_DOWN, 20),
    Key::wide_repeat("\u{2192}", key::KEY_RIGHT, 20),
    // 0.5u gap before the numpad
    Key::inert(None, 10),
    Key::sym_wide("0", key::KEY_0, ")", 40),
    Key::sym_wide(".", key::KEY_DOT, ">", 20),
    Key::wide("\u{21B5}", key::KEY_KPENTER, 20),
];

const ROWS_NUMPAD: [Row; 5] = [
    Row { keys: &NUMPAD_ROW0 },
    Row { keys: &NUMPAD_ROW1 },
    Row { keys: &NUMPAD_ROW2 },
    Row { keys: &NUMPAD_ROW3 },
    Row { keys: &NUMPAD_ROW4 },
];

pub const LAYOUT_NUMPAD: Layout = Layout {
    name: "65%+Num",
    rows: &ROWS_NUMPAD,
};

pub static ALL_LAYOUTS: &[&Layout] = &[&LAYOUT_65, &LAYOUT_NUMPAD];

/// Default profile: 65%.
pub const LAYOUT: Layout = LAYOUT_65;

