//! Pure logic for the osk-win Windows on-screen keyboard.
//!
//! Vendored from the Linux `osk` project's `osk-core`. The xkb-based char
//! mapping is replaced by a US-QWERTY table in [`typekey`], and the neural
//! LLM predictor is compiled out (its stub keeps the n-gram predictor intact).
//! Everything here is platform-neutral and testable headless.

#![forbid(unsafe_code)]

pub mod events;
pub mod key;
pub mod layout;
pub mod mods;
pub mod neural;
pub mod ngm;
pub mod predict;
pub mod selftest;
pub mod typekey;
pub mod words;

pub use events::{OskEvent, StickyMods, TAP_MAX_EVENTS};
pub use key::{Key, Layout, Row};
pub use layout::LAYOUT;
pub use mods::{ModKeyState, ModifierEngine};
pub use predict::{LearnedWord, Predictor};
