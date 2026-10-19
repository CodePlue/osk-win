//! Char-to-key mapping via a US-QWERTY table, and string typing over a
//! [`KeySink`].
//!
//! The Linux build resolves a char to (linux KEY_*, shift) through the
//! system default xkb keymap. On Windows there is no xkb, so this maps the
//! same US-QWERTY layout directly. The result agrees with what a standard
//! US keyboard produces, which is what the SendInput backend injects.

use crate::key::key;

/// A char has no key at the base or shift level of the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoKey(pub char);

/// Injection target for [`type_string`].
pub trait KeySink {
    fn press(&mut self, code: u16);
    fn release(&mut self, code: u16);
}

/// The US-QWERTY keymap used to resolve chars to (KEY_*, shift).
#[derive(Clone, Copy, Default)]
pub struct CharMap;

impl CharMap {
    /// Always succeeds: the layout is a fixed table, no system keymap needed.
    pub fn new() -> Option<Self> {
        Some(Self)
    }

    /// Map a single char to (linux KEY_*, shift) for a US-QWERTY keyboard.
    /// `None` if the char has no key.
    pub fn char_to_key(&self, c: char) -> Option<(u16, bool)> {
        use key::*;
        match c {
            // Lowercase letters.
            'a' => Some((KEY_A, false)),
            'b' => Some((KEY_B, false)),
            'c' => Some((KEY_C, false)),
            'd' => Some((KEY_D, false)),
            'e' => Some((KEY_E, false)),
            'f' => Some((KEY_F, false)),
            'g' => Some((KEY_G, false)),
            'h' => Some((KEY_H, false)),
            'i' => Some((KEY_I, false)),
            'j' => Some((KEY_J, false)),
            'k' => Some((KEY_K, false)),
            'l' => Some((KEY_L, false)),
            'm' => Some((KEY_M, false)),
            'n' => Some((KEY_N, false)),
            'o' => Some((KEY_O, false)),
            'p' => Some((KEY_P, false)),
            'q' => Some((KEY_Q, false)),
            'r' => Some((KEY_R, false)),
            's' => Some((KEY_S, false)),
            't' => Some((KEY_T, false)),
            'u' => Some((KEY_U, false)),
            'v' => Some((KEY_V, false)),
            'w' => Some((KEY_W, false)),
            'x' => Some((KEY_X, false)),
            'y' => Some((KEY_Y, false)),
            'z' => Some((KEY_Z, false)),
            // Uppercase letters (shifted).
            'A' => Some((KEY_A, true)),
            'B' => Some((KEY_B, true)),
            'C' => Some((KEY_C, true)),
            'D' => Some((KEY_D, true)),
            'E' => Some((KEY_E, true)),
            'F' => Some((KEY_F, true)),
            'G' => Some((KEY_G, true)),
            'H' => Some((KEY_H, true)),
            'I' => Some((KEY_I, true)),
            'J' => Some((KEY_J, true)),
            'K' => Some((KEY_K, true)),
            'L' => Some((KEY_L, true)),
            'M' => Some((KEY_M, true)),
            'N' => Some((KEY_N, true)),
            'O' => Some((KEY_O, true)),
            'P' => Some((KEY_P, true)),
            'Q' => Some((KEY_Q, true)),
            'R' => Some((KEY_R, true)),
            'S' => Some((KEY_S, true)),
            'T' => Some((KEY_T, true)),
            'U' => Some((KEY_U, true)),
            'V' => Some((KEY_V, true)),
            'W' => Some((KEY_W, true)),
            'X' => Some((KEY_X, true)),
            'Y' => Some((KEY_Y, true)),
            'Z' => Some((KEY_Z, true)),
            // Digits (base level).
            '0' => Some((KEY_0, false)),
            '1' => Some((KEY_1, false)),
            '2' => Some((KEY_2, false)),
            '3' => Some((KEY_3, false)),
            '4' => Some((KEY_4, false)),
            '5' => Some((KEY_5, false)),
            '6' => Some((KEY_6, false)),
            '7' => Some((KEY_7, false)),
            '8' => Some((KEY_8, false)),
            '9' => Some((KEY_9, false)),
            // Shifted digits.
            '!' => Some((KEY_1, true)),
            '@' => Some((KEY_2, true)),
            '#' => Some((KEY_3, true)),
            '$' => Some((KEY_4, true)),
            '%' => Some((KEY_5, true)),
            '^' => Some((KEY_6, true)),
            '&' => Some((KEY_7, true)),
            '*' => Some((KEY_8, true)),
            '(' => Some((KEY_9, true)),
            ')' => Some((KEY_0, true)),
            // Base-level symbols.
            '-' => Some((KEY_MINUS, false)),
            '=' => Some((KEY_EQUAL, false)),
            '[' => Some((KEY_LEFTBRACE, false)),
            ']' => Some((KEY_RIGHTBRACE, false)),
            '\\' => Some((KEY_BACKSLASH, false)),
            ';' => Some((KEY_SEMICOLON, false)),
            '\'' => Some((KEY_APOSTROPHE, false)),
            '`' => Some((KEY_GRAVE, false)),
            ',' => Some((KEY_COMMA, false)),
            '.' => Some((KEY_DOT, false)),
            '/' => Some((KEY_SLASH, false)),
            // Shifted symbols.
            '_' => Some((KEY_MINUS, true)),
            '+' => Some((KEY_EQUAL, true)),
            '{' => Some((KEY_LEFTBRACE, true)),
            '}' => Some((KEY_RIGHTBRACE, true)),
            '|' => Some((KEY_BACKSLASH, true)),
            ':' => Some((KEY_SEMICOLON, true)),
            '"' => Some((KEY_APOSTROPHE, true)),
            '~' => Some((KEY_GRAVE, true)),
            '<' => Some((KEY_COMMA, true)),
            '>' => Some((KEY_DOT, true)),
            '?' => Some((KEY_SLASH, true)),
            _ => None,
        }
    }
}

/// Type a string by mapping each char onto the keymap.
///
/// Validates the whole string first: never inject a prefix and then
/// fail halfway through a password or email address. `pause` runs
/// after every character; the app passes a short sleep so the
/// target settles, tests pass a no-op.
pub fn type_string<S: KeySink + ?Sized>(
    sink: &mut S,
    map: &CharMap,
    s: &str,
    pause: impl Fn(),
) -> Result<(), NoKey> {
    for c in s.chars() {
        if c == ' ' {
            continue;
        }
        if map.char_to_key(c).is_none() {
            return Err(NoKey(c));
        }
    }
    for c in s.chars() {
        if c == ' ' {
            sink.press(key::KEY_SPACE);
            sink.release(key::KEY_SPACE);
        } else {
            let (code, shift) = map.char_to_key(c).expect("validated above");
            if shift {
                sink.press(key::KEY_LEFTSHIFT);
            }
            sink.press(code);
            sink.release(code);
            if shift {
                sink.release(key::KEY_LEFTSHIFT);
            }
        }
        pause();
    }
    Ok(())
}

/// Like [`type_string`], but a `\n` injects a KEY_ENTER press/release and a
/// `\t` a KEY_TAB press/release (multi-line / tabbed replacements). Same
/// whole-string validation: never type a prefix and fail halfway.
pub fn type_text<S: KeySink + ?Sized>(
    sink: &mut S,
    map: &CharMap,
    s: &str,
    pause: impl Fn(),
) -> Result<(), NoKey> {
    for c in s.chars() {
        if c == ' ' || c == '\n' || c == '\t' || c == '\r' {
            continue;
        }
        if map.char_to_key(c).is_none() {
            return Err(NoKey(c));
        }
    }
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' => {
                sink.press(key::KEY_SPACE);
                sink.release(key::KEY_SPACE);
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    continue;
                }
                sink.press(key::KEY_ENTER);
                sink.release(key::KEY_ENTER);
            }
            '\n' => {
                sink.press(key::KEY_ENTER);
                sink.release(key::KEY_ENTER);
            }
            '\t' => {
                sink.press(key::KEY_TAB);
                sink.release(key::KEY_TAB);
            }
            _ => {
                let (code, shift) = map.char_to_key(c).expect("validated above");
                if shift {
                    sink.press(key::KEY_LEFTSHIFT);
                }
                sink.press(code);
                sink.release(code);
                if shift {
                    sink.release(key::KEY_LEFTSHIFT);
                }
            }
        }
        pause();
    }
    Ok(())
}
