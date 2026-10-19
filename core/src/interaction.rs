//! Pointer / touch interaction state, as one value.
//!
//! The UI used to carry `pressed`, `rep_key`, `moving`, `resizing` and
//! their grab/repeat payload as sibling fields. A reset path could (and,
//! before `c0178d8`, did) forget one of them, and a *new* field would be
//! forgotten from every reset by construction. This module folds the
//! whole thing into one enum whose variants carry only the data that is
//! meaningful in that state, so an interaction is either idle, pressing
//! a key, moving the surface, or resizing it - never a mix.
//!
//! [`step`] is the pure transition function. It has no clock, no I/O and
//! no Wayland types, so the reset rules are unit-testable headless. The
//! UI supplies `now` in the event and applies the returned [`Step`].

/// Auto-repeat delay before the first repeat, in milliseconds.
pub const REPEAT_DELAY_MS: u64 = 500;
/// Hold time before a key is drawn "held", in milliseconds.
pub const HOLD_MS: u64 = 200;

/// What the pointer/touch interaction is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Interaction {
    /// Nothing held: no key down, no drag, no resize.
    #[default]
    Idle,
    /// A key is held down. `rep_next` is `Some` while it auto-repeats.
    Press {
        key: usize,
        press_time: u64,
        hold_fired: bool,
        rep_next: Option<u64>,
    },
    /// The surface is being dragged by the title bar.
    Move {
        grab_x: i32,
        grab_y: i32,
        fx: f64,
        fy: f64,
    },
    /// The surface is being resized by the corner handle.
    Resize {
        grab_x: i32,
        grab_y: i32,
        w: i32,
        h: i32,
    },
}

/// An input that can change the interaction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// A key went down at time `now`. `repeat` arms auto-repeat.
    Press { key: usize, repeat: bool, now: u64 },
    /// The drag handle (title bar) was grabbed at `(x, y)`.
    StartMove { x: i32, y: i32 },
    /// The resize handle was grabbed at `(x, y)`, base size `(w, h)`.
    StartResize { x: i32, y: i32, w: i32, h: i32 },
    /// Pointer motion. It never changes the interaction *kind*; the UI
    /// mutates the active variant's payload directly.
    Motion,
    /// The pointer or touch left the surface (or the touch was cancelled).
    Leave,
    /// The button or touch was released.
    Release,
}

/// The result of an [`Event`]: the next interaction plus the side effects
/// the UI must perform. `flash` shows a key's press look briefly;
/// `apply_geometry` re-commits the dragged geometry; `save_config`
/// persists it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Step {
    pub next: Interaction,
    pub flash: Option<usize>,
    pub apply_geometry: bool,
    pub save_config: bool,
}

impl Step {
    /// A step that only changes the interaction.
    fn to(next: Interaction) -> Self {
        Step {
            next,
            ..Default::default()
        }
    }
}

/// Advance the interaction. Pure: same inputs, same output, no clock.
///
/// The two reset paths differ only in whether they re-commit geometry:
/// a release re-commits it (the drag may have ended mid-frame), a leave
/// does not (motion already committed on every sample) but still saves.
pub fn step(cur: Interaction, ev: Event) -> Step {
    match ev {
        Event::Press { key, repeat, now } => Step::to(Interaction::Press {
            key,
            press_time: now,
            hold_fired: false,
            rep_next: repeat.then_some(now + REPEAT_DELAY_MS),
        }),
        Event::StartMove { x, y } => Step::to(Interaction::Move {
            grab_x: x,
            grab_y: y,
            fx: 0.0,
            fy: 0.0,
        }),
        Event::StartResize { x, y, w, h } => Step::to(Interaction::Resize {
            grab_x: x,
            grab_y: y,
            w,
            h,
        }),
        Event::Motion => Step::to(cur),
        Event::Leave => match cur {
            Interaction::Press { key, .. } => Step {
                next: Interaction::Idle,
                flash: Some(key),
                ..Default::default()
            },
            Interaction::Move { .. } | Interaction::Resize { .. } => Step {
                next: Interaction::Idle,
                save_config: true,
                ..Default::default()
            },
            Interaction::Idle => Step::to(Interaction::Idle),
        },
        Event::Release => match cur {
            Interaction::Press { key, .. } => Step {
                next: Interaction::Idle,
                flash: Some(key),
                ..Default::default()
            },
            Interaction::Move { .. } | Interaction::Resize { .. } => Step {
                next: Interaction::Idle,
                apply_geometry: true,
                save_config: true,
                ..Default::default()
            },
            Interaction::Idle => Step::to(Interaction::Idle),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_then_leave_then_release_is_idle() {
        // Press a repeating key.
        let s = step(
            Interaction::Idle,
            Event::Press {
                key: 7,
                repeat: true,
                now: 100,
            },
        );
        assert_eq!(
            s.next,
            Interaction::Press {
                key: 7,
                press_time: 100,
                hold_fired: false,
                rep_next: Some(600),
            }
        );

        // Pointer leaves the surface mid-press: back to Idle in one step.
        let s = step(s.next, Event::Leave);
        assert_eq!(s.next, Interaction::Idle);
        assert_eq!(s.flash, Some(7), "the released key should flash");
        assert!(!s.apply_geometry && !s.save_config);

        // The release that the compositor never delivers is a no-op.
        let s = step(s.next, Event::Release);
        assert_eq!(s.next, Interaction::Idle);
        assert_eq!(s.flash, None);
        assert!(!s.apply_geometry && !s.save_config);
    }

    #[test]
    fn drag_then_release_applies_geometry_and_saves() {
        // Grab the bar, drag, then release.
        let s = step(Interaction::Idle, Event::StartMove { x: 10, y: 20 });
        assert!(matches!(s.next, Interaction::Move { .. }));
        let s = step(s.next, Event::Motion);
        assert!(
            matches!(s.next, Interaction::Move { .. }),
            "motion keeps the drag"
        );
        let s = step(s.next, Event::Release);
        assert_eq!(s.next, Interaction::Idle);
        assert!(s.apply_geometry, "drag end must re-commit geometry");
        assert!(s.save_config, "drag end must persist geometry");
        assert_eq!(s.flash, None);
    }

    #[test]
    fn leave_during_drag_saves_without_reapplying() {
        let s = step(
            Interaction::Idle,
            Event::StartResize {
                x: 1,
                y: 2,
                w: 300,
                h: 200,
            },
        );
        assert!(matches!(s.next, Interaction::Resize { .. }));
        let s = step(s.next, Event::Leave);
        assert_eq!(s.next, Interaction::Idle);
        assert!(s.save_config);
        assert!(!s.apply_geometry, "a leave relies on per-motion commits");
        assert_eq!(s.flash, None);
    }
}
