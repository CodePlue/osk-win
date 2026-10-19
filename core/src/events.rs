//! Tap event sequences: the ordered press/release events a key tap
//! produces, including sticky modifiers and the shift wrap.
//!
//! A tap of key `k` with `n` sticky modifiers and a shift wrap is
//! `n + 1 + 1` presses then the same releases in reverse, so at most
//! [`TAP_MAX_EVENTS`] events (3 mods + shift + key).

use crate::key::{key, TapKey};
use crate::mods::Casing;

/// Maximum events one tap can produce (3 sticky mods + shift + key).
pub const TAP_MAX_EVENTS: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OskEvent {
    pub code: u16,
    pub press: bool,
}

/// Sticky ctrl/alt/meta codes, in slot order. `None` = not active.
/// Replaces the C bitmask-plus-parallel-array pair, which could not
/// express "set but no key".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StickyMods {
    pub ctrl: Option<u16>,
    pub alt: Option<u16>,
    pub meta: Option<u16>,
}

impl StickyMods {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ctrl.is_none() && self.alt.is_none() && self.meta.is_none()
    }

    /// Number of active sticky modifiers.
    #[inline]
    pub fn count(&self) -> usize {
        usize::from(self.ctrl.is_some())
            + usize::from(self.alt.is_some())
            + usize::from(self.meta.is_some())
    }

    /// Active codes in ctrl, alt, meta order.
    #[inline]
    pub fn codes(&self) -> [Option<u16>; 3] {
        [self.ctrl, self.alt, self.meta]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapError {
    /// The event buffer is too small for this chord.
    TooSmall,
}

/// Build the press/release sequence for tapping `k` under the given
/// modifier state. Takes a [`TapKey`], so gaps and modifier keys cannot
/// reach injection: they have no `TapKey`. Returns the number of events
/// written.
pub fn tap_events(
    k: TapKey,
    casing: Casing,
    sticky: &StickyMods,
    out: &mut [OskEvent],
) -> Result<usize, TapError> {
    let mods = sticky.codes();
    let nd = mods.iter().filter(|m| m.is_some()).count();
    // Shift decision at the injection boundary: Caps Lock changes the
    // case of letters only; an active Shift still shifts digits and
    // symbols.
    let need_shift = if k.is_letter() {
        casing.letter_upper()
    } else {
        casing.symbol_shift()
    };
    let needed = (nd + usize::from(need_shift)) * 2 + 2;
    if out.len() < needed {
        return Err(TapError::TooSmall);
    }
    let mut idx = 0;
    for m in mods.iter().flatten() {
        out[idx] = OskEvent {
            code: *m,
            press: true,
        };
        idx += 1;
    }
    if need_shift {
        out[idx] = OskEvent {
            code: key::KEY_LEFTSHIFT,
            press: true,
        };
        idx += 1;
    }
    out[idx] = OskEvent {
        code: k.code_u16(),
        press: true,
    };
    idx += 1;
    out[idx] = OskEvent {
        code: k.code_u16(),
        press: false,
    };
    idx += 1;
    if need_shift {
        out[idx] = OskEvent {
            code: key::KEY_LEFTSHIFT,
            press: false,
        };
        idx += 1;
    }
    for m in mods.iter().flatten().rev() {
        out[idx] = OskEvent {
            code: *m,
            press: false,
        };
        idx += 1;
    }
    Ok(idx)
}

/// The sticky modifier codes to hold while typing a suggestion word,
/// in ctrl, alt, meta order. Unused slots are 0.
pub fn sticky_presses(sticky: &StickyMods) -> [u16; 3] {
    let mut codes = [0u16; 3];
    for (slot, m) in sticky.codes().into_iter().flatten().take(3).enumerate() {
        codes[slot] = m;
    }
    codes
}

/// The remainder of `word` after the typed `partial`, with casing
/// applied for the committed text and a trailing space.
///
/// Casing rule: the first letter of the remainder follows the shift
/// latch (inverted by caps), the rest follow caps.
///
/// Returns the number of characters written to `out`, or `None` when
/// the prefix does not match or the buffer is too small.
pub fn complete_word(
    word: &str,
    partial: &str,
    casing: Casing,
    out: &mut [char],
) -> Option<usize> {
    if !word.starts_with(partial) {
        return None;
    }
    let rem: String = word[partial.len()..]
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if c.is_ascii_lowercase() {
                let upper = if i == 0 { casing.letter_upper() } else { casing.caps };
                if upper {
                    return c.to_ascii_uppercase();
                }
            }
            c
        })
        .collect();
    let total = rem.chars().count() + 1;
    if out.len() < total {
        return None;
    }
    for (slot, c) in out.iter_mut().zip(rem.chars().chain(std::iter::once(' '))) {
        *slot = c;
    }
    Some(total)
}

/// The edit that turns the typed partial into `word`: how many
/// backspaces to send, then the characters to type (with a trailing
/// space) written to `out`.
///
/// A word that extends `partial` is a plain completion (no
/// backspaces, cased by [`complete_word`]). Anything else is a
/// spelling correction: the letters after the shared prefix are erased
/// and the rest of `word` is retyped in the case of `typed` (the
/// partial as typed): all caps when it was all caps, otherwise each
/// letter follows the typed letter at the same position.
///
/// Returns `None` when the buffer is too small.
pub fn replace_word(
    word: &str,
    partial: &str,
    typed: &str,
    casing: Casing,
    out: &mut [char],
) -> Option<(usize, usize)> {
    if word.starts_with(partial) {
        return complete_word(word, partial, casing, out).map(|n| (0, n));
    }
    let keep = word
        .chars()
        .zip(partial.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let erase = partial.chars().count() - keep;
    let letters: Vec<char> = typed.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    let all_caps = letters.len() >= 2 && letters.iter().all(|c| c.is_ascii_uppercase());
    let typed: Vec<char> = typed.chars().collect();
    let rest: Vec<char> = word
        .chars()
        .enumerate()
        .skip(keep)
        .map(|(i, c)| {
            let upper = all_caps || typed.get(i).is_some_and(|t| t.is_ascii_uppercase());
            if upper {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .chain(std::iter::once(' '))
        .collect();
    if out.len() < rest.len() {
        return None;
    }
    out[..rest.len()].copy_from_slice(&rest);
    Some((erase, rest.len()))
}
