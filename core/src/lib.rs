//! Pure logic for the osk on-screen keyboard: layouts, the modifier
//! engine, tap events, word prediction, and char-to-key mapping.
//!
//! No Wayland, no `unsafe`; everything here is testable headless, and
//! [`selftest::run`] is the behavior contract, ported from the C
//! reference in `legacy/src/selftest.c`.
//!
//! One boundary crosses the purity rule, on purpose: `neural.rs` is
//! the only module that reads a file (an optional downloaded model),
//! reads the environment (its XDG model directory) and runs a worker
//! thread. All other file/environment access, including the
//! predictor's history persistence, lives outside this crate (see
//! `osk-backend`).
//!
//! Module map:
//! - [`controller`]: the click path both UIs share (taps, suggestion
//!   commit, modifier dispatch, the key -> predictor transition).
//! - [`key`]: KEY_* event codes and the Key/Row/Layout types.
//! - [`layout`]: the 65% keyboard profile.
//! - [`metrics`]: the one spacing scale both front ends draw with.
//! - [`mods`]: the 3-state modifier engine (OFF / LATCHED / LOCKED).
//! - [`events`]: tap event sequences and sticky-modifier handling.
//! - [`interaction`]: the pointer/touch interaction state machine.
//! - [`predict`]: next-word prediction (history serialization as text).
//! - [`spell`]: spelling correction for the partial word.
//! - [`typekey`]: char-to-key mapping and string typing.
//! - [`selftest`]: the headless self-test suite.

#![forbid(unsafe_code)]

pub mod controller;
pub mod events;
pub mod interaction;
pub mod key;
pub mod layout;
pub mod metrics;
pub mod mods;
pub mod neural;
pub mod ngm;
pub mod predict;
pub mod selftest;
pub mod spell;
pub mod typekey;
pub mod words;

pub use events::{OskEvent, StickyMods, TAP_MAX_EVENTS};
pub use key::{Click, Key, KeyKind, Layout, Row, TapKey, nz};
pub use layout::LAYOUT;
pub use mods::{Casing, ModKeyState, ModifierEngine};
pub use predict::{LearnedWord, Predictor};
