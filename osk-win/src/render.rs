//! Drawing, shared by both front ends: the Wayland UI paints the
//! current [`View`] snapshot of its [`State`](crate::ui::State), and
//! `osk --render` builds a [`View`] headless and paints it into a PNG.
//! One copy of the geometry and painting code means the two paths
//! cannot drift. Nothing here touches Wayland.

use std::fs::File;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Instant;

use crate::cairo::{self, Context, FontSlant, FontWeight, Format, ImageSurface, Operator};
use osk_core::controller::{Controller, ControllerConfig};
use osk_core::interaction::{self, Interaction};
use osk_core::key::{key, Click, Key, KeyKind, Layout};
use osk_core::layout::ALL_LAYOUTS;
use osk_core::metrics::{self, Metrics};
use osk_core::mods::{
    mod_index, DOUBLE_CLICK_MS, ModKeyState, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT,
};
use osk_core::predict::Predictor;
use osk_core::typekey::KeySink;

// ---- geometry ----
// Every spacing metric comes from one scale per surface:
// `osk_core::metrics::Metrics::new`. The old two-scale mix (some
// numbers off the width's unit, some off `min(unit_x, row_h)`) and the
// hand-tuned pixel literals in the bar painters are gone; see that
// module for the scale's design and its fractions.
/// Default pixels per scale unit for the default window size.
const U0: f64 = 46.0;

// Sans-serif face for the labels; it carries every glyph we draw
// (letters, digits, and the arrow/symbol caps) in one font.
pub(crate) const FONT: &str = "Adwaita Sans";

/// Dictation lifecycle.
#[cfg(feature = "dictation")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DictState {
    Idle,
    Recording,
    Transcribing,
}

/// Number of waveform bars drawn in the bar's free area while
/// dictation records. The UI reads this many [`crate::dictate`]
/// levels per frame; `--render` fakes the same count.
#[cfg(feature = "dictation")]
pub(crate) const WAVE_BARS: usize = 20;

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub name: &'static str,
    pub case: u32,
    pub key: u32,
    pub key_hov: u32,
    #[allow(dead_code)]
    pub key_hi: u32,
    pub key_shdw: u32,
    pub key_mod: u32,
    pub keytxt: u32,
    pub modtxt: u32,
    pub accent: u32,
    pub ink: u32,
    pub on_bg: u32,
    pub on_txt: u32,
    pub key_press: u32,
    pub key_hold: u32,
    #[allow(dead_code)]
    pub hair: u32,
    pub edge: u32,
    #[allow(dead_code)]
    pub sugtxt: u32,
    #[allow(dead_code)]
    pub sugtxt_first: u32,
    #[allow(dead_code)]
    pub part: u32,
    pub close: u32,
    pub opacity: f64,
}

pub static THEMES: &[Theme] = &[
    Theme {
        name: "Default",
        case: 0x0f1420,
        key: 0x1b2132,
        key_hov: 0x313748,
        key_hi: 0x292e3e,
        key_shdw: 0x090d15,
        key_mod: 0x181e2e,
        keytxt: 0xd5dcec,
        modtxt: 0x7d88a3,
        accent: 0x5ad6c6,
        ink: 0x04201c,
        on_bg: 0x1e3a45,
        on_txt: 0x7de8da,
        key_press: 0x5ad6c6,
        key_hold: 0x38b2a3,
        hair: 0x222732,
        edge: 0x283145,
        sugtxt: 0x8a96b0,
        sugtxt_first: 0x7de8da,
        part: 0xe4e9f5,
        close: 0x7d88a3,
        opacity: 1.0,
    },
    Theme {
        name: "Tokyo",
        case: 0x1a1b26,
        key: 0x24283b,
        key_hov: 0x343b58,
        key_hi: 0x394b70,
        key_shdw: 0x13141c,
        key_mod: 0x212538,
        keytxt: 0xc0caf5,
        modtxt: 0x7aa2f7,
        accent: 0xbb9af7,
        ink: 0x1a1b26,
        on_bg: 0x3b335a,
        on_txt: 0xc0caf5,
        key_press: 0xbb9af7,
        key_hold: 0x9d7cd8,
        hair: 0x292e42,
        edge: 0x323852,
        sugtxt: 0x7aa2f7,
        sugtxt_first: 0xbb9af7,
        part: 0xff9e64,
        close: 0xf7768e,
        opacity: 1.0,
    },
    Theme {
        name: "Catppuccin",
        case: 0x11111b,
        key: 0x1e1e2e,
        key_hov: 0x313244,
        key_hi: 0x45475a,
        key_shdw: 0x09090d,
        key_mod: 0x1b1b2a,
        keytxt: 0xcdd6f4,
        modtxt: 0xa6adc8,
        accent: 0xcba6f7,
        ink: 0x11111b,
        on_bg: 0x45355c,
        on_txt: 0xcba6f7,
        key_press: 0xcba6f7,
        key_hold: 0xb4befe,
        hair: 0x313244,
        edge: 0x313244,
        sugtxt: 0x89b4fa,
        sugtxt_first: 0xcba6f7,
        part: 0xa6e3a1,
        close: 0xf38ba8,
        opacity: 1.0,
    },
    Theme {
        name: "Nord",
        case: 0x242933,
        key: 0x2e3440,
        key_hov: 0x434c5e,
        key_hi: 0x4c566a,
        key_shdw: 0x1b1f27,
        key_mod: 0x2b313d,
        keytxt: 0xeceff4,
        modtxt: 0xd8dee9,
        accent: 0x88c0d0,
        ink: 0x2e3440,
        on_bg: 0x3b4a58,
        on_txt: 0x88c0d0,
        key_press: 0x88c0d0,
        key_hold: 0x81a1c1,
        hair: 0x3b4252,
        edge: 0x3f4859,
        sugtxt: 0x81a1c1,
        sugtxt_first: 0x88c0d0,
        part: 0xa3be8c,
        close: 0xbf616a,
        opacity: 1.0,
    },
    Theme {
        name: "Gruvbox",
        case: 0x1d2021,
        key: 0x282828,
        key_hov: 0x3c3836,
        key_hi: 0x504945,
        key_shdw: 0x141617,
        key_mod: 0x252525,
        keytxt: 0xebdbb2,
        modtxt: 0xd5c4a1,
        accent: 0xfe8019,
        ink: 0x1d2021,
        on_bg: 0x503b28,
        on_txt: 0xfe8019,
        key_press: 0xfe8019,
        key_hold: 0xd65d0e,
        hair: 0x3c3836,
        edge: 0x3c3836,
        sugtxt: 0x83a598,
        sugtxt_first: 0xfe8019,
        part: 0xd3869b,
        close: 0xfb4934,
        opacity: 1.0,
    },
    Theme {
        name: "Rose Pine",
        case: 0x191724,
        key: 0x1f1d2e,
        key_hov: 0x26233a,
        key_hi: 0x312f44,
        key_shdw: 0x13111c,
        key_mod: 0x222033,
        keytxt: 0xe0def4,
        modtxt: 0x908caa,
        accent: 0xebbcba,
        ink: 0x191724,
        on_bg: 0x3b2f3d,
        on_txt: 0xebbcba,
        key_press: 0xebbcba,
        key_hold: 0xd49b99,
        hair: 0x26233a,
        edge: 0x332f48,
        sugtxt: 0x908caa,
        sugtxt_first: 0xebbcba,
        part: 0xeb6f92,
        close: 0xeb6f92,
        opacity: 1.0,
    },
    Theme {
        name: "Dracula",
        case: 0x21222c,
        key: 0x282a36,
        key_hov: 0x44475a,
        key_hi: 0x6272a4,
        key_shdw: 0x191a21,
        key_mod: 0x292b3a,
        keytxt: 0xf8f8f2,
        modtxt: 0x6272a4,
        accent: 0xbd93f9,
        ink: 0x282a36,
        on_bg: 0x493d63,
        on_txt: 0xbd93f9,
        key_press: 0xbd93f9,
        key_hold: 0x9a6bd8,
        hair: 0x44475a,
        edge: 0x3b3e52,
        sugtxt: 0x8be9fd,
        sugtxt_first: 0xbd93f9,
        part: 0xffb86c,
        close: 0xff5555,
        opacity: 1.0,
    },
    Theme {
        name: "Everforest",
        case: 0x232a2e,
        key: 0x2d353b,
        key_hov: 0x3d484d,
        key_hi: 0x475258,
        key_shdw: 0x1e2326,
        key_mod: 0x2a3137,
        keytxt: 0xd3c6aa,
        modtxt: 0x859289,
        accent: 0xa7c080,
        ink: 0x232a2e,
        on_bg: 0x374239,
        on_txt: 0xa7c080,
        key_press: 0xa7c080,
        key_hold: 0x8fa86b,
        hair: 0x343f44,
        edge: 0x3b464c,
        sugtxt: 0x83c092,
        sugtxt_first: 0xa7c080,
        part: 0xe69875,
        close: 0xe67e80,
        opacity: 1.0,
    },
    Theme {
        name: "Kanagawa",
        case: 0x181820,
        key: 0x1f1f28,
        key_hov: 0x2a2a37,
        key_hi: 0x363646,
        key_shdw: 0x16161d,
        key_mod: 0x22222d,
        keytxt: 0xdcd7ba,
        modtxt: 0x727169,
        accent: 0x957fb8,
        ink: 0x181820,
        on_bg: 0x363447,
        on_txt: 0x957fb8,
        key_press: 0x957fb8,
        key_hold: 0x7e699b,
        hair: 0x2a2a37,
        edge: 0x31313f,
        sugtxt: 0x7aa89f,
        sugtxt_first: 0x957fb8,
        part: 0xffa066,
        close: 0xc34043,
        opacity: 1.0,
    },
    Theme {
        name: "Solarized",
        case: 0x00212b,
        key: 0x073642,
        key_hov: 0x0d4857,
        key_hi: 0x586e75,
        key_shdw: 0x001b22,
        key_mod: 0x002b36,
        keytxt: 0x93a1a1,
        modtxt: 0x657b83,
        accent: 0x268bd2,
        ink: 0x002b36,
        on_bg: 0x074354,
        on_txt: 0x268bd2,
        key_press: 0x268bd2,
        key_hold: 0x1d70a8,
        hair: 0x073642,
        edge: 0x001b22,
        sugtxt: 0x2aa198,
        sugtxt_first: 0x268bd2,
        part: 0xcb4b16,
        close: 0xdc322f,
        opacity: 1.0,
    },
    Theme {
        name: "Cyberpunk",
        case: 0x120e24,
        key: 0x1a1638,
        key_hov: 0x282156,
        key_hi: 0x3d2b78,
        key_shdw: 0x0d091b,
        key_mod: 0x15102c,
        keytxt: 0x00ffc2,
        modtxt: 0x965fd4,
        accent: 0xff0055,
        ink: 0x120e24,
        on_bg: 0x47152b,
        on_txt: 0xff0055,
        key_press: 0xff0055,
        key_hold: 0xcc0044,
        hair: 0x261e47,
        edge: 0x0d091b,
        sugtxt: 0x965fd4,
        sugtxt_first: 0xff0055,
        part: 0xff5500,
        close: 0xff0055,
        opacity: 1.0,
    },
    Theme {
        // Deep sea navy-green with a coral accent.
        name: "Abyss",
        case: 0x0b1a1f,
        key: 0x10242b,
        key_hov: 0x1b3740,
        key_hi: 0x2f5560,
        key_shdw: 0x061014,
        key_mod: 0x0e2026,
        keytxt: 0xd8eef0,
        modtxt: 0x5f8a91,
        accent: 0xff8a80,
        ink: 0x0b1a1f,
        on_bg: 0x3d2a2e,
        on_txt: 0xff8a80,
        key_press: 0xff8a80,
        key_hold: 0xe06a62,
        hair: 0x1b3740,
        edge: 0x173039,
        sugtxt: 0x7fd1c7,
        sugtxt_first: 0xff8a80,
        part: 0xffd38a,
        close: 0xff6b6b,
        opacity: 1.0,
    },
];

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

pub(crate) fn now_ms() -> u64 {
    START.elapsed().as_millis() as u64
}

// ---- handle hover animation ----
// The chosen handle (the corner arc) eases in when the pointer enters
// its area and eases back out when it leaves, once per transition:
// the stroke brightens from the resting color to the accent while it
// grows slightly longer and thicker. One shot only — while the pointer
// rests the picture is static, so the UI stops requesting frames the
// moment the ease settles (the keyboard's idle-CPU budget).

/// Duration of the handle ease, in milliseconds.
pub(crate) const HANDLE_ANIM_MS: u64 = 160;

/// Ease-out cubic on 0..=1: fast start, gentle settle. Inputs outside
/// the range clamp, so the curve ends flat at 1.0.
pub(crate) fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

/// Animation progress at `now` of a one-shot ease that started at
/// `since`. Past the duration the value is exactly 1.0 and stays
/// there — the caller can compare against 1.0 to stop requesting
/// frames. `dur` of 0 means already finished.
pub(crate) fn anim_progress(now: u64, since: u64, dur: u64) -> f64 {
    match dur {
        0 => 1.0,
        _ => ease_out_cubic(now.saturating_sub(since) as f64 / dur as f64),
    }
}

/// Mix two ARGB colors: a=0 gives `from`, a=1 gives `to`, linear per
/// channel.
pub(crate) fn mix_color(from: u32, to: u32, a: f64) -> u32 {
    let a = a.clamp(0.0, 1.0);
    (0..3)
        .map(|sh| {
            let f = ((from >> (16 - sh * 8)) & 0xff) as f64;
            let t = ((to >> (16 - sh * 8)) & 0xff) as f64;
            (f + (t - f) * a).round() as u32
        })
        .fold(0u32, |acc, v| (acc << 8) | v)
}

fn set_src_rgba(cr: &Context, argb: u32, alpha: f64) {
    cr.set_source_rgba(
        ((argb >> 16) & 0xff) as f64 / 255.0,
        ((argb >> 8) & 0xff) as f64 / 255.0,
        (argb & 0xff) as f64 / 255.0,
        alpha,
    );
}

fn set_src(cr: &Context, argb: u32) {
    set_src_rgba(cr, argb, 1.0);
}

/// Rounded rectangle subpath.
fn rrect(cr: &Context, x: f64, y: f64, w: f64, h: f64, rad: f64) {
    let mut r = rad;
    if r > w / 2.0 {
        r = w / 2.0;
    }
    if r > h / 2.0 {
        r = h / 2.0;
    }
    let half = std::f64::consts::FRAC_PI_2;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -half, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, half);
    cr.arc(x + r, y + h - r, r, half, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, half * 3.0);
    cr.close_path();
}

/// A measured rectangle in logical coordinates.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
    /// Index into the suggestion list (0 for close/layout).
    pub(crate) idx: usize,
}

impl Rect {
    pub(crate) fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Bar element positions measured during render.
#[derive(Clone, Copy)]
pub(crate) struct BarLayout {
    pub(crate) sugg: [Rect; 5],
    pub(crate) n_sugg: usize,
    /// The bar's free area: the strip between the left margin and one
    /// gap before the first chip, where the suggestion pills are
    /// centered and where the VIA status line may run. Measured once
    /// here so both painters share it.
    pub(crate) text_left: f64,
    pub(crate) text_right: f64,
    pub(crate) layout_rect: Rect,
    pub(crate) via_rect: Rect,
    pub(crate) theme_rect: Rect,
    pub(crate) close_rect: Rect,
    pub(crate) theme_menu_rect: Rect,
    pub(crate) theme_item_rects: [Rect; 12],
    pub(crate) tab_toggle_rect: Rect,
    pub(crate) pred_toggle_rect: Rect,
    pub(crate) mic_rect: Rect,
    /// Scale the theme menu was laid out at (1.0 = natural size; less
    /// when the window is too small for it), for its text and padding.
    pub(crate) menu_scale: f64,
}

/// VIA chip look for one frame: dim with no device, normal with one,
/// or armed (accent ring) while a remap session is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViaChipLook {
    /// No VIA device found: the chip looks dormant.
    Dim,
    /// Device attached, session not active.
    Normal,
    /// Session armed or captured: accent ring; the whole OSK is inert.
    Armed,
    /// No remap support on this platform (osk-win): the chip takes no
    /// space and the layout chip moves up against the mic.
    Hidden,
}

/// What the painters read about the remap session this frame. The
/// Wayland UI maps its live session into it; `--render --via` fakes a
/// state. Rendering stays free of osk-via: a frame is a look plus a
/// line of text.
#[derive(Clone)]
pub(crate) struct ViaFrame {
    pub(crate) chip: ViaChipLook,
    /// None = draw the suggestion pills; Some = one status line in
    /// their place.
    pub(crate) status: Option<String>,
}

impl ViaFrame {
    /// The idle look: normal chip, suggestion pills visible.
    pub(crate) fn idle() -> Self {
        ViaFrame {
            chip: ViaChipLook::Normal,
            status: None,
        }
    }
}

/// What the painters read, snapshotted from the UI state (or built
/// headless by `--render`): surface size, theme, layout, interaction
/// and pointer state, and read-only access to the controller. One
/// copy of the geometry and painting code below serves both the
/// Wayland UI and the PNG renderer.
pub(crate) struct View<'a> {
    /// Logical surface size.
    pub(crate) width: i32,
    pub(crate) height: i32,
    /// Buffer (pixel) size backing the surface; the paint scale.
    pub(crate) buf_w: i32,
    pub(crate) buf_h: i32,
    pub(crate) theme: Theme,
    pub(crate) theme_idx: usize,
    pub(crate) layout_idx: usize,
    pub(crate) grid_cols: i32,
    pub(crate) interaction: Interaction,
    /// Key flashed by the last release or latch.
    pub(crate) flash_key: i32,
    /// Pointer position, surface-local logical pixels.
    pub(crate) px: i32,
    pub(crate) py: i32,
    pub(crate) inside: bool,
    /// Handle hover ease progress for this frame, 0 (resting) .. 1
    /// (fully hovered). Computed by the caller from the recorded
    /// transition time; the Wayland UI recomputes it per frame,
    /// `--render` pins it with `--anim`.
    pub(crate) handle_anim: f64,
    pub(crate) theme_menu_open: bool,
    /// Read access: modifiers, prediction flags, suggestions.
    pub(crate) ctl: &'a Controller,
    /// VIA chip look and status line for this frame.
    pub(crate) via: ViaFrame,
    /// Mic button look.
    #[cfg(feature = "dictation")]
    pub(crate) dict_state: DictState,
    /// Live waveform levels while dictation records: one level per
    /// bar, oldest first, 0..1. Plain data, so render.rs never
    /// depends on cpal; the Wayland UI reads the recorder, `--render`
    /// fakes a fixed pattern.
    #[cfg(feature = "dictation")]
    pub(crate) levels: Vec<f32>,
}

/// How this frame's pointer, interaction and modifiers change one
/// key's look. The default is the key at rest, the look
/// [`KeyCache`] holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct KeyState {
    vis: ModKeyState,
    press: Press,
    hov: bool,
}

/// How far down a key is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Press {
    #[default]
    Up,
    /// Pressed, or flashed after its release.
    Down,
    /// Held past the hold delay, or repeating.
    Held,
}

/// Everything the at-rest keyboard depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CacheKey {
    buf: (i32, i32),
    size: (i32, i32),
    theme_idx: usize,
    layout_idx: usize,
    grid_cols: i32,
    /// Letter keys show capitals (shift or caps).
    upper: bool,
    /// Symbol keys show their shifted labels.
    shift: bool,
}

/// The case and every key at rest, drawn once into a buffer-sized
/// surface. A frame copies it and redraws only the keys whose look
/// differs, so a keystroke repaints one key instead of all of them.
pub(crate) struct KeyCache {
    key: CacheKey,
    surface: ImageSurface,
}

/// The layout at `idx` of [`ALL_LAYOUTS`]: one lookup shared by the
/// UI state and the view.
pub(crate) fn layout_at(idx: usize) -> &'static Layout {
    ALL_LAYOUTS[idx]
}

/// Widest row of the layout, in 1/20u columns.
pub(crate) fn grid_cols(layout: &Layout) -> i32 {
    let mut cols = 0;
    for row in layout.rows {
        let total: i32 = row.keys.iter().map(|k| k.width as i32).sum();
        if total > cols {
            cols = total;
        }
    }
    cols
}

/// Default window size for a layout: one scale unit = U0 pixels, with
/// the spacing budget spent evenly on both axes, so the default keys
/// land at the 1.0 end of the clamp's key-aspect range.
pub(crate) fn default_size(layout: &Layout, cols: i32) -> (i32, i32) {
    let rows = layout.rows.len() as f64;
    let cols_u = cols as f64 / 20.0;
    (
        ((cols_u + 4.0 * metrics::GAP_S) * U0 + 0.5) as i32,
        ((rows + metrics::PILL_S + 6.0 * metrics::GAP_S) * U0 + 0.5) as i32,
    )
}

/// `--render` options; the CLI in main.rs parses them.
pub struct RenderOpts {
    /// PNG output path.
    pub out: PathBuf,
    /// Logical surface size; None = the default window size.
    pub size: Option<(i32, i32)>,
    /// Layout name (case-insensitive); None = the default layout.
    pub layout: Option<String>,
    /// Theme name (case-insensitive); None = the default theme.
    pub theme: Option<String>,
    /// Text typed into the predictor before drawing.
    pub typed: Option<String>,
    /// Label of the key to draw hovered.
    pub hover: Option<String>,
    /// Handle hover-ease progress to draw (0..=1); None = 1.0 when
    /// `hover` puts the pointer on the handle, else 0.0 (resting).
    pub anim: Option<f64>,
    /// Pass the [`RenderOpts::size`] through
    /// [`Metrics::clamp_size`] first, so the render shows what the
    /// resize clamp would produce (used by the extreme showcase
    /// sizes).
    pub clamp: bool,
    /// Faked VIA remap state: "idle", "nodevice", "armed",
    /// "captured", "done", or "error". None = idle.
    pub via: Option<String>,
    /// Draw the frame with the theme menu open.
    pub menu: bool,
    /// Modifier states to draw through the real engine, e.g.
    /// "shift=latched,ctrl=locked,caps".
    pub mods: Option<String>,
    /// Label of the key drawn in its held look.
    pub pressed: Option<String>,
    /// Dictation frame to draw: "recording" (a fake speech-like
    /// level pattern), "silence" (recording with an all-zero level
    /// line), "transcribing" (nothing extra drawn) or "idle".
    /// None = idle.
    #[cfg(feature = "dictation")]
    pub dict: Option<String>,
    /// After the PNG, time this many more frames of the same state
    /// and print the per-frame cost.
    pub bench_frames: usize,
}

/// The `--render --via` state name, parsed into the chip look and
/// status line `--render` fakes for that frame. The texts mirror what
/// a live session reports.
fn parse_via(name: &str) -> Result<ViaFrame, String> {
    Ok(match name {
        "idle" => ViaFrame::idle(),
        "nodevice" => ViaFrame {
            chip: ViaChipLook::Dim,
            status: Some("VIA: no VIA keyboard found, plug one in".to_string()),
        },
        "armed" => ViaFrame {
            chip: ViaChipLook::Armed,
            status: Some("VIA: press and hold a key on the macropad (it will also type)".to_string()),
        },
        "captured" => ViaFrame {
            chip: ViaChipLook::Armed,
            status: Some("VIA: A captured, click a key to remap".to_string()),
        },
        "done" => ViaFrame {
            chip: ViaChipLook::Normal,
            status: Some("VIA: A -> Q".to_string()),
        },
        "error" => ViaFrame {
            chip: ViaChipLook::Normal,
            status: Some("VIA: device did not answer in time".to_string()),
        },
        // The bar osk-win draws: no VIA chip.
        "hidden" => ViaFrame {
            chip: ViaChipLook::Hidden,
            status: None,
        },
        _ => {
            return Err(format!(
                "unknown via state '{name}' (idle|nodevice|armed|captured|done|error|hidden)"
            ))
        }
    })
}

/// The `--render --dict` state name, parsed into the dictation look
/// that frame draws. Levels are plain data; the recording pattern is
/// a fixed fake so the review shots are deterministic.
#[cfg(feature = "dictation")]
fn parse_dict(name: &str) -> Result<(DictState, Vec<f32>), String> {
    Ok(match name {
        "recording" => {
            let levels = fake_wave_levels(WAVE_BARS);
            (DictState::Recording, levels)
        }
        "silence" => (DictState::Recording, vec![0.0; WAVE_BARS]),
        "transcribing" => (DictState::Transcribing, Vec::new()),
        "idle" => (DictState::Idle, Vec::new()),
        _ => {
            return Err(format!(
                "unknown dict state '{name}' (recording|silence|transcribing|idle)"
            ))
        }
    })
}

/// The fake level pattern `--render --dict recording` draws: groups
/// of speech-like bursts with pauses between them, oldest first, so
/// the pattern reads like someone talking.
#[cfg(feature = "dictation")]
fn fake_wave_levels(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let t = match n {
                0 | 1 => 0.0,
                _ => i as f64 / (n - 1) as f64,
            };
            // Slow word-group envelope with pauses, times a faster
            // syllable wobble inside each group.
            let group = ((t * 4.0 * std::f64::consts::PI).sin() * 0.5 + 0.5).powf(1.5);
            let syl = (t * 21.0 * std::f64::consts::PI).sin() * 0.5 + 0.5;
            (group * (0.25 + 0.75 * syl) * 0.95) as f32
        })
        .collect()
}

/// A sink that discards every injection event: `--render` shows the
/// predictor's state but never injects anywhere.
struct DiscardSink;

impl KeySink for DiscardSink {
    fn press(&mut self, _code: u16) {}
    fn release(&mut self, _code: u16) {}
}

/// The layout key that types `ch`: an exact label first (digits,
/// symbols), then a letter key case-insensitively, then the space bar.
fn typed_key(layout: &Layout, ch: char) -> Option<Key> {
    let s = ch.to_string();
    for row in layout.rows {
        for k in row.keys {
            if !k.is_gap() && k.label == Some(s.as_str()) {
                return Some(*k);
            }
        }
    }
    for row in layout.rows {
        for k in row.keys {
            if !k.is_gap() && k.letter && k.label.is_some_and(|l| l.eq_ignore_ascii_case(&s)) {
                return Some(*k);
            }
        }
    }
    if ch == ' ' {
        for row in layout.rows {
            for k in row.keys {
                if !k.is_gap() && k.code_u16() == key::KEY_SPACE {
                    return Some(*k);
                }
            }
        }
    }
    None
}

/// The `--anim` pin, or the natural endpoint for where `--hover`
/// put the pointer: fully eased in on the handle, resting elsewhere.
fn anim_pin(opts: &RenderOpts) -> f64 {
    match opts.anim {
        Some(p) => p.clamp(0.0, 1.0),
        None => {
            if matches!(&opts.hover, Some(h) if h == "handle") {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// Whether a `--hover` name targets a measured bar element ("via",
/// "close", a "pillN" suggestion) instead of a key. These names are
/// resolved after measure_bar; see [`bar_hover_target`].
fn is_bar_hover_name(name: &str) -> bool {
    matches!(name, "layout" | "via" | "theme" | "close")
        || name == "mic"
        || name.starts_with("pill")
}

/// The rect a `--hover` bar name points at: the element's drawn rect
/// from the measurement pass. A "pillN" name indexes the drawn
/// suggestions ("pill" = 0, the primary).
fn bar_hover_target(bar: &BarLayout, name: &str) -> Option<Rect> {
    match name {
        "layout" => Some(bar.layout_rect),
        "via" => Some(bar.via_rect),
        "theme" => Some(bar.theme_rect),
        "close" => Some(bar.close_rect),
        #[cfg(feature = "dictation")]
        "mic" => Some(bar.mic_rect),
        _ => {
            let rest = name.strip_prefix("pill")?;
            let i = if rest.is_empty() {
                0
            } else {
                rest.parse::<usize>().ok()?
            };
            (i < bar.n_sugg).then(|| bar.sugg[i])
        }
    }
}

/// The index (`row * 100 + i`) of the first non-gap key labeled
/// `label`: exact for symbols and digits, case-insensitive for letter
/// keys. Used by `--hover` centering and `--pressed`.
fn key_index(layout: &Layout, label: &str) -> Option<usize> {
    for (r, row) in layout.rows.iter().enumerate() {
        for (i, k) in row.keys.iter().enumerate() {
            if k.is_gap() {
                continue;
            }
            let hit = k.label == Some(label)
                || (k.letter && k.label.is_some_and(|l| l.eq_ignore_ascii_case(label)));
            if hit {
                return Some(r * 100 + i);
            }
        }
    }
    None
}

/// The layout's Caps Lock key (injects KEY_CAPSLOCK through Click::Caps).
fn caps_key(layout: &Layout) -> Option<Key> {
    for row in layout.rows {
        for k in row.keys {
            if let KeyKind::Inject { click: Click::Caps, .. } = k.kind {
                return Some(*k);
            }
        }
    }
    None
}

/// The first layout key that arms modifier slot `slot` through the
/// latch machine (shift is drawn "⇧", so `--mods` cannot search by
/// label). The engine lights only the physical key that armed it, and
/// this is that key.
fn mod_key(layout: &Layout, slot: usize) -> Option<Key> {
    for row in layout.rows {
        for k in row.keys {
            // Latch keys (Shift, Ctrl) and momentary ones (Win, Alt,
            // which lock on a double-click) both arm a slot.
            if let KeyKind::Inject { code, click: Click::Latch | Click::Momentary } = k.kind {
                if mod_index(code.get()) == Some(slot) {
                    return Some(*k);
                }
            }
        }
    }
    None
}

/// The `--render --mods` list: one `key=state` pair per comma (bare
/// "caps" means caps=on), e.g. "shift=latched,ctrl=locked,caps". Each
/// named key walks the real click path in
/// [`Controller::click_key`] — the same latch machine and Clock a
/// live session runs — so the frame draws exactly what that state
/// looks like, no faked colors.
fn apply_mods(ctl: &mut Controller, layout: &Layout, list: &str) -> Result<(), String> {
    for part in list.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, want) = match part.split_once('=') {
            Some((n, s)) => (n.trim(), s.trim()),
            // A bare word can only be "caps", which means caps=on.
            None => ("caps", "on"),
        };
        match name.to_ascii_lowercase().as_str() {
            "caps" => {
                let n = match want.to_ascii_lowercase().as_str() {
                    "on" | "latched" | "locked" | "" => 1,
                    "off" => 2,
                    _ => return Err(format!("unknown caps state '{want}' (on|off)")),
                };
                let k = caps_key(layout).ok_or("no caps key in this layout")?;
                for _ in 0..n {
                    ctl.click_key(&k, 0, &mut DiscardSink);
                }
            }
            // Caps Lock is a plain toggle; modifiers walk the
            // 3-state machine (locked = a second click inside the
            // double-click window).
            "shift" => mods_click(ctl, layout, MOD_SHIFT, want)?,
            "ctrl" | "control" => mods_click(ctl, layout, MOD_CTRL, want)?,
            "alt" => mods_click(ctl, layout, MOD_ALT, want)?,
            "meta" | "win" | "super" => mods_click(ctl, layout, MOD_META, want)?,
            _ => {
                return Err(format!(
                    "unknown modifier '{name}' (shift|ctrl|alt|meta|caps)"
                ))
            }
        }
    }
    Ok(())
}

/// Click the latch key for `slot` until it sits in the state `want`
/// ("latched", "locked", or "off" — the default, so a no-op).
fn mods_click(ctl: &mut Controller, layout: &Layout, slot: usize, want: &str) -> Result<(), String> {
    let clicks = match want.to_ascii_lowercase().as_str() {
        "latched" => 1,
        "locked" => 2,
        "off" | "" => 0,
        _ => return Err(format!("unknown modifier state '{want}' (latched|locked|off)")),
    };
    if clicks > 0 {
        let k = mod_key(layout, slot).ok_or("no key arms this modifier in this layout")?;
        // Two clicks: the second lands inside the double-click
        // window, so the machine reads it as a lock.
        ctl.click_key(&k, 0, &mut DiscardSink);
        if clicks == 2 {
            ctl.click_key(&k, DOUBLE_CLICK_MS - 1, &mut DiscardSink);
        }
    }
    Ok(())
}

/// The controller for a headless render: prediction on with an
/// ephemeral predictor, the `--typed` characters through the real
/// click path (the sink discards the injection events, so the
/// suggestion pills match a live session) and the `--mods` states
/// through the same path, so the frames show what a
/// latched/locked/caps key really draws — nothing faked.
fn headless_ctl(opts: &RenderOpts, layout: &'static Layout) -> Result<Controller, String> {
    let mut ctl = Controller::with_predictor(
        ControllerConfig {
            prediction: true,
            tab_accepts_prediction: false,
        },
        Predictor::ephemeral(),
    );
    if let Some(text) = &opts.typed {
        let mut sink = DiscardSink;
        for ch in text.chars() {
            let k = typed_key(layout, ch).ok_or_else(|| format!("no key for '{ch}'"))?;
            ctl.click_key(&k, now_ms(), &mut sink);
        }
    }
    if let Some(list) = &opts.mods {
        apply_mods(&mut ctl, layout, list)?;
    }
    Ok(ctl)
}

/// The pointer placement `--hover` can do without the measured bar:
/// the resize handle, a named key, or "inside over nothing". Bar
/// element names are placed after measure_bar.
fn place_key_hover(view: &mut View, label: &str) -> Result<(), String> {
    match label {
        // For the design-review shots: the pointer over the resize
        // handle, and over nothing (inside, but on a spot no painter
        // highlights).
        "handle" => {
            let hs = view.handle_size() as f64;
            view.px = (view.width as f64 - hs / 2.0) as i32;
            view.py = (view.height as f64 - hs / 2.0) as i32;
            view.inside = true;
        }
        "none" => {
            let m = view.metrics();
            view.px = (view.width as f64 - m.margin / 2.0) as i32;
            view.py = view.height / 2;
            view.inside = true;
        }
        _ => {
            if !is_bar_hover_name(label) {
                let Some((cx, cy)) = view.key_center(label) else {
                    return Err(format!("no key labeled '{label}'"));
                };
                view.px = cx as i32;
                view.py = cy as i32;
                view.inside = true;
            }
        }
    }
    Ok(())
}

/// The `--pressed` look: hold the named key down inside the
/// auto-repeat window, so the painter takes the held (repeating
/// frame) state a real long touch would show.
fn hold_pressed_key(view: &mut View, layout: &Layout, label: &str) -> Result<(), String> {
    let idx = key_index(layout, label).ok_or_else(|| format!("no key labeled '{label}'"))?;
    let now = now_ms();
    view.interaction = Interaction::Press {
        key: idx,
        press_time: now,
        hold_fired: true,
        rep_next: Some(now + interaction::REPEAT_DELAY_MS),
    };
    Ok(())
}

/// The `--render` mode: draw one frame into a PNG with no Wayland
/// connection. The controller runs over [`Predictor::ephemeral`], so
/// nothing is read from or written to the user's history or config.
#[allow(clippy::too_many_lines)]
pub fn run(opts: RenderOpts) -> Result<(), String> {
    let layout_idx = match &opts.layout {
        Some(name) => ALL_LAYOUTS
            .iter()
            .position(|l| l.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("unknown layout '{name}'"))?,
        None => 0,
    };
    let theme_idx = match &opts.theme {
        Some(name) => THEMES
            .iter()
            .position(|t| t.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("unknown theme '{name}'"))?,
        None => 0,
    };
    let layout = layout_at(layout_idx);
    let cols = grid_cols(layout);
    let (mut width, mut height) =
        opts.size.unwrap_or_else(|| default_size(layout, cols));
    // --clamp: show what the resize clamp would do with this size
    // (the extreme showcase renders pass through it; plain renders
    // keep the raw requested size like the live window would have at
    // that size before any resize).
    if opts.clamp {
        (width, height) = Metrics::clamp_size(width, height, cols, layout.rows.len());
    }

    let ctl = headless_ctl(&opts, layout)?;

    // The `--dict` faked state: plain data in the view, like a live
    // session would pass.
    #[cfg(feature = "dictation")]
    let (dict_state, levels) = match &opts.dict {
        Some(name) => parse_dict(name)?,
        None => (DictState::Idle, Vec::new()),
    };

    let mut view = View {
        width,
        height,
        // A 1x buffer: the PNG is the logical-size frame.
        buf_w: width,
        buf_h: height,
        theme: THEMES[theme_idx],
        theme_idx,
        layout_idx,
        grid_cols: cols,
        interaction: Interaction::Idle,
        flash_key: -1,
        px: 0,
        py: 0,
        inside: false,
        theme_menu_open: opts.menu,
        handle_anim: anim_pin(&opts),
        ctl: &ctl,
        via: match &opts.via {
            Some(state) => parse_via(state)?,
            None => ViaFrame::idle(),
        },
        #[cfg(feature = "dictation")]
        dict_state,
        #[cfg(feature = "dictation")]
        levels,
    };
    if let Some(label) = &opts.hover {
        place_key_hover(&mut view, label)?;
    }
    if let Some(label) = &opts.pressed {
        hold_pressed_key(&mut view, layout, label)?;
    }

    let surface = ImageSurface::create(Format::ARgb32, width, height)
        .map_err(|e| format!("cairo surface {width}x{height}: {e}"))?;
    let meas = Context::new(&surface).map_err(|e| format!("cairo: {e}"))?;
    meas.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
    let bar = view.measure_bar(&meas);
    // A second `--hover` group targets a measured bar element; its
    // rect is only known after measure_bar, so the pointer lands on
    // the drawn rect instead of a guessed spot.
    if let Some(name) = &opts.hover {
        if is_bar_hover_name(name) {
            let Some(r) = bar_hover_target(&bar, name) else {
                return Err(format!("no bar element '{name}' on this frame"));
            };
            view.px = (r.x + r.w / 2.0) as i32;
            view.py = (r.y + r.h / 2.0) as i32;
            view.inside = true;
        }
    }
    drop(meas);
    let cr = Context::new(&surface).map_err(|e| format!("cairo: {e}"))?;
    let mut cache = None;
    view.paint(&cr, &bar, &mut cache);
    drop(cr);
    if opts.bench_frames > 0 {
        bench_frames(&view, &surface, &mut cache, opts.bench_frames)?;
    }
    surface.flush();
    let mut file = File::create(&opts.out).map_err(|e| format!("{}: {e}", opts.out.display()))?;
    surface
        .write_to_png(&mut file)
        .map_err(|e| format!("{}: {e}", opts.out.display()))?;
    Ok(())
}

/// `--bench-frames`: redraw the frame `n` times the way the Wayland
/// UI does (a fresh context, `measure_bar`, `paint`) and print the
/// median, min and max per-frame times, with `measure_bar` alone.
#[allow(clippy::print_stderr)]
fn bench_frames(
    view: &View,
    surface: &ImageSurface,
    cache: &mut Option<KeyCache>,
    n: usize,
) -> Result<(), String> {
    let mut frame = Vec::with_capacity(n);
    let mut measure = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = Instant::now();
        let meas = Context::new(surface).map_err(|e| format!("cairo: {e}"))?;
        meas.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
        let bar = view.measure_bar(&meas);
        drop(meas);
        let t1 = Instant::now();
        let cr = Context::new(surface).map_err(|e| format!("cairo: {e}"))?;
        view.paint(&cr, &bar, cache);
        drop(cr);
        surface.flush();
        let t2 = Instant::now();
        frame.push((t2 - t0).as_secs_f64() * 1e3);
        measure.push((t1 - t0).as_secs_f64() * 1e3);
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[0], v[v.len() - 1])
    };
    let (fm, flo, fhi) = stats(&mut frame);
    let (mm, mlo, mhi) = stats(&mut measure);
    eprintln!(
        "bench: {n} frames, frame median {fm:.3} ms (min {flo:.3}, max {fhi:.3}), \
         measure_bar median {mm:.3} ms (min {mlo:.3}, max {mhi:.3})"
    );
    Ok(())
}

impl View<'_> {
    pub(crate) fn layout(&self) -> &'static Layout {
        layout_at(self.layout_idx)
    }

    /// The center of the first key labeled `label` (exact, or
    /// case-insensitively for letter keys), for `--hover`. One label
    /// search serves both `--hover` (`key_center`) and `--pressed`
    /// (`key_index`).
    fn key_center(&self, label: &str) -> Option<(f64, f64)> {
        let idx = key_index(self.layout(), label)?;
        let (x, y, w, h) = self.key_rect(idx / 100, idx % 100);
        Some((x + w / 2.0, y + h / 2.0))
    }

    // ---- geometry ----

    /// The spacing metrics for this surface size: one scale computed
    /// once per call from the surface size and the layout. Every
    /// measured/painted constant below reads these, never a literal.
    pub(crate) fn metrics(&self) -> Metrics {
        Metrics::new(
            self.width.max(0) as f64,
            self.height.max(0) as f64,
            self.grid_cols,
            self.layout().rows.len(),
        )
    }

    pub(crate) fn bar_h(&self) -> f64 {
        self.metrics().bar_h
    }

    pub(crate) fn key_rect(&self, r: usize, i: usize) -> (f64, f64, f64, f64) {
        let m = self.metrics();
        let layout = self.layout();
        let row_total: i32 = layout.rows[r].keys.iter().map(|k| k.width as i32).sum();
        let mut col = (self.grid_cols - row_total) / 2;
        for j in 0..i {
            col += layout.rows[r].keys[j].width as i32;
        }
        (
            m.margin + col as f64 / 20.0 * m.unit_x,
            m.bar_h + m.margin + r as f64 * m.row_h,
            layout.rows[r].keys[i].width as f64 / 20.0 * m.unit_x,
            m.row_h,
        )
    }

    /// Side of the bottom-right resize handle, in logical pixels.
    pub(crate) fn handle_size(&self) -> i32 {
        // Spacing must not stretch: half a scale unit, floor 24 px.
        ((0.5 * self.metrics().s).max(24.0)) as i32
    }

    /// Whether the rect reaches into one of the case's rounded corner
    /// squares, the only place the case outline clips anything.
    fn in_case_corner(&self, x: f64, y: f64, w: f64, h: f64) -> bool {
        let r = self.metrics().case_r;
        let (cw, ch) = (self.width as f64, self.height as f64);
        (x < r || x + w > cw - r) && (y < r || y + h > ch - r)
    }

    pub(crate) fn handle_hov(&self) -> bool {
        self.inside
            && self.width > 0
            && self.px >= self.width - self.handle_size()
            && self.py >= self.height - self.handle_size()
    }

    // ---- painting ----

    /// Measure the bar elements (suggestions, layout, theme icon, close)
    /// and store their rects for hit testing.
    // Every pill/menu rect measured in one pass so they stay aligned.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn measure_bar(&self, cr: &Context) -> BarLayout {
        let mut bar = BarLayout {
            sugg: [Rect::default(); 5],
            n_sugg: 0,
            text_left: 0.0,
            text_right: 0.0,
            layout_rect: Rect::default(),
            via_rect: Rect::default(),
            theme_rect: Rect::default(),
            close_rect: Rect::default(),
            theme_menu_rect: Rect::default(),
            theme_item_rects: [Rect::default(); 12],
            tab_toggle_rect: Rect::default(),
            pred_toggle_rect: Rect::default(),
            mic_rect: Rect::default(),
            menu_scale: 1.0,
        };
        let m = self.metrics();
        let bh = m.bar_h;
        let fs = m.font_sug();
        let chip_fs = m.font_chip();
        let side_pad = m.pill_pad();

        // 1. Close button on the far right
        let cw = m.close_w();
        let cx = self.width as f64 - cw - m.margin;
        bar.close_rect = Rect {
            x: cx,
            y: 0.0,
            w: cw,
            h: bh,
            idx: 0,
        };

        let chip_pad = m.chip_pad();
        let chip_gap = m.icon;

        // 2. Theme icon button
        let th_w = m.icon_w();
        let th_x = cx - th_w - chip_gap;
        bar.theme_rect = Rect {
            x: th_x,
            y: 0.0,
            w: th_w,
            h: bh,
            idx: 0,
        };

        // 2b. Mic button (dictation), left of the theme icon.
        #[cfg(feature = "dictation")]
        let mic_x = {
            let mic_w = m.icon_w();
            let mx = th_x - mic_w - chip_gap;
            bar.mic_rect = Rect {
                x: mx,
                y: 0.0,
                w: mic_w,
                h: bh,
                idx: 0,
            };
            mx
        };

        // 3. VIA chip ("VIA"), right of the layout chip, left of the
        // mic/theme cluster.
        #[cfg(not(feature = "dictation"))]
        let via_right = th_x;
        #[cfg(feature = "dictation")]
        let via_right = mic_x;
        let (via_x, via_w) = if self.via.chip == ViaChipLook::Hidden {
            (via_right, 0.0)
        } else {
            let via_w = self.text_w(cr, "VIA", chip_fs) + 2.0 * chip_pad;
            (via_right - via_w - chip_gap, via_w)
        };
        bar.via_rect = Rect {
            x: via_x,
            y: 0.0,
            w: via_w,
            h: bh,
            idx: 0,
        };

        // 4. Layout chip (e.g. "65%", "65%+Num"), left of the VIA chip
        let lo_label = self.layout().name;
        let lo_tw = self.text_w(cr, lo_label, chip_fs);
        let lo_w = lo_tw + 2.0 * chip_pad;
        let lo_x = via_x - lo_w - chip_gap;
        bar.layout_rect = Rect {
            x: lo_x,
            y: 0.0,
            w: lo_w,
            h: bh,
            idx: 0,
        };

        // The bar's free area, one definition for the pill centering
        // below and for the VIA status line in paint_bar: from the
        // left margin to one gap before the first chip.
        let text_left = m.margin;
        let text_right = (lo_x - m.g).max(text_left);
        bar.text_left = text_left;
        bar.text_right = text_right;

        // 5. Centered word prediction
        // Surface the owned neural engine's failures (drained from the
        // controller) here, the regularly polled point; osk-core
        // itself never prints.
        for e in self.ctl.take_errors() {
            warn!("osk: neural: {e}");
        }
        // While dictation records the waveform replaces the pills: no
        // pill rects are measured, so nothing invisible stays
        // clickable. Otherwise the prediction list drives them.
        #[cfg(feature = "dictation")]
        let suggs: Vec<&'static str> = if self.dict_state == DictState::Recording {
            Vec::new()
        } else if self.ctl.prediction() {
            self.ctl.suggestions()
        } else {
            Vec::new()
        };
        #[cfg(not(feature = "dictation"))]
        let suggs: Vec<&'static str> = if self.ctl.prediction() {
            self.ctl.suggestions()
        } else {
            Vec::new()
        };
        let avail_left = text_left;
        let avail_right = text_right;
        if !suggs.is_empty() && avail_right > avail_left + 1.3 * m.s {
            let max_avail_w = avail_right - avail_left;
            let pill_gap = m.pill_gap;
            let min_pill = 1.3 * m.s;
            let min_primary = 1.8 * m.s;

            let slot_candidates: &[&[usize]] = if suggs.len() >= 5 {
                &[&[2, 1, 0, 3, 4], &[1, 0, 2], &[0]]
            } else if suggs.len() >= 3 {
                &[&[1, 0, 2], &[0]]
            } else if suggs.len() == 2 {
                &[&[0, 1], &[0]]
            } else {
                &[&[0]]
            };

            for candidates in slot_candidates {
                let mut widths = Vec::with_capacity(candidates.len());
                let mut total_w = 0.0;
                for &idx in *candidates {
                    let word = suggs.get(idx).copied().unwrap_or("");
                    let tw = self.text_w(cr, word, fs);
                    let pw = if idx == 0 {
                        (tw + 2.8 * side_pad).max(min_primary)
                    } else {
                        (tw + 1.8 * side_pad).max(min_pill)
                    };
                    widths.push(pw);
                    total_w += pw;
                }
                total_w += (widths.len() as f64 - 1.0).max(0.0) * pill_gap;

                if total_w <= max_avail_w || candidates.len() == 1 {
                    // Centered in the free area between the margin
                    // and the chips, not on the full width.
                    let center_x = (text_left + text_right) / 2.0;
                    let mut start_x = center_x - total_w / 2.0;
                    if start_x < avail_left {
                        start_x = avail_left;
                    }
                    if start_x + total_w > avail_right {
                        start_x = (avail_right - total_w).max(avail_left);
                    }

                    bar.n_sugg = 0;
                    for (i, &idx) in candidates.iter().enumerate() {
                        let pw = widths[i];
                        if bar.n_sugg < 5 {
                            bar.sugg[bar.n_sugg] = Rect {
                                x: start_x,
                                y: 0.0,
                                w: pw,
                                h: bh,
                                idx,
                            };
                            bar.n_sugg += 1;
                        }
                        start_x += pw + pill_gap;
                    }
                    break;
                }
            }
        }

        // 6. Theme dropdown: one row with the two toggles side by side,
        // above a 3 x 4 grid of theme cards. The menu always fits inside
        // the window below the bar: if its natural size does not, every
        // menu measure (and its text) scales down together.
        if self.theme_menu_open {
            const COLS: usize = 3;
            const ROWS: usize = 4;
            let col_w0 = m.menu_col_w();
            let row_h0 = m.menu_card_h();
            let col_gap0 = m.menu_col_gap();
            let row_gap0 = m.menu_row_gap();
            let pad0 = m.menu_pad();
            let toggle_h0 = m.menu_toggle_h();
            let toggle_gap0 = m.menu_toggle_gap();

            let menu_w0 = COLS as f64 * col_w0 + (COLS - 1) as f64 * col_gap0 + 2.0 * pad0;
            let menu_h0 = pad0
                + toggle_h0
                + toggle_gap0
                + ROWS as f64 * row_h0
                + (ROWS - 1) as f64 * row_gap0
                + pad0;
            let menu_y = bh + m.menu_below_bar();
            let avail_w = (self.width as f64 - 2.0 * m.margin).max(0.0);
            let avail_h = (self.height as f64 - menu_y - m.margin).max(0.0);
            let k = (avail_w / menu_w0).min(avail_h / menu_h0).min(1.0);
            bar.menu_scale = k;

            let (col_w, row_h, col_gap, row_gap) = (col_w0 * k, row_h0 * k, col_gap0 * k, row_gap0 * k);
            let (pad, toggle_h, toggle_gap) = (pad0 * k, toggle_h0 * k, toggle_gap0 * k);
            let menu_w = menu_w0 * k;
            let menu_h = menu_h0 * k;

            let menu_right = (th_x + th_w).min(self.width as f64 - m.margin);
            let menu_x = (menu_right - menu_w).max(m.margin);

            bar.theme_menu_rect = Rect {
                x: menu_x,
                y: menu_y,
                w: menu_w,
                h: menu_h,
                idx: 0,
            };

            // The two toggles share the top row, one gap apart.
            let toggle_w = (menu_w - 2.0 * pad - col_gap) / 2.0;
            bar.pred_toggle_rect = Rect {
                x: menu_x + pad,
                y: menu_y + pad,
                w: toggle_w,
                h: toggle_h,
                idx: 0,
            };
            bar.tab_toggle_rect = Rect {
                x: menu_x + pad + toggle_w + col_gap,
                y: menu_y + pad,
                w: toggle_w,
                h: toggle_h,
                idx: 0,
            };

            let grid_y = menu_y + pad + toggle_h + toggle_gap;
            for idx in 0..COLS * ROWS {
                let col = idx % COLS;
                let row = idx / COLS;
                bar.theme_item_rects[idx] = Rect {
                    x: menu_x + pad + col as f64 * (col_w + col_gap),
                    y: grid_y + row as f64 * (row_h + row_gap),
                    w: col_w,
                    h: row_h,
                    idx,
                };
            }
        }

        bar
    }

    pub(crate) fn paint(&self, cr: &Context, bar: &BarLayout, cache: &mut Option<KeyCache>) {
        cr.identity_matrix();
        cr.reset_clip();
        let key = self.cache_key();
        if cache.as_ref().map(|c| &c.key) != Some(&key) {
            *cache = self.build_cache(key);
        }
        if let Some(c) = cache.as_ref() {
            // A straight pixel copy of the case and every key at rest.
            cr.set_operator(Operator::Source);
            let _ = cr.set_source_surface(&c.surface, 0.0, 0.0);
            let _ = cr.paint();
        }
        self.scale_to_logical(cr);
        cr.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
        if cache.is_none() {
            self.paint_rest(cr);
        }
        self.paint_bar(cr, bar);
        let layout = self.layout();
        for (r, row) in layout.rows.iter().enumerate() {
            for (i, key) in row.keys.iter().enumerate() {
                let (rx, ry, rw, rh) = self.key_rect(r, i);
                let st = self.key_state(key, r * 100 + i, rx, ry, rw, rh);
                // The cache holds the idle look; only a key that looks
                // different this frame is drawn again.
                if st != KeyState::default() || cache.is_none() {
                    self.paint_key(cr, rx, ry, rw, rh, key, st);
                }
            }
        }
        self.paint_handle(cr);
        // A single hairline around the panel.
        cr.set_operator(Operator::Over);
        cr.new_path();
        rrect(
            cr,
            0.5,
            0.5,
            self.width as f64 - 1.0,
            self.height as f64 - 1.0,
            self.metrics().case_r.max(1.0),
        );
        set_src(cr, self.theme.edge);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        if self.theme_menu_open {
            self.paint_theme_menu(cr, bar);
        }
    }

    fn scale_to_logical(&self, cr: &Context) {
        if self.width > 0 && self.height > 0 && self.buf_w > 0 && self.buf_h > 0 {
            cr.scale(
                self.buf_w as f64 / self.width as f64,
                self.buf_h as f64 / self.height as f64,
            );
        }
    }

    /// Everything [`KeyCache`] holds: the case and every key at rest.
    fn paint_rest(&self, cr: &Context) {
        self.paint_window(cr);
        for (r, row) in self.layout().rows.iter().enumerate() {
            for (i, key) in row.keys.iter().enumerate() {
                let (rx, ry, rw, rh) = self.key_rect(r, i);
                self.paint_key(cr, rx, ry, rw, rh, key, KeyState::default());
            }
        }
    }

    /// What the at-rest keyboard depends on.
    fn cache_key(&self) -> CacheKey {
        let mods = self.ctl.mods();
        CacheKey {
            buf: (self.buf_w, self.buf_h),
            size: (self.width, self.height),
            theme_idx: self.theme_idx,
            layout_idx: self.layout_idx,
            grid_cols: self.grid_cols,
            upper: mods.casing().letter_upper(),
            shift: mods.shift_active(),
        }
    }

    /// Draw the at-rest keyboard into a fresh offscreen surface; None
    /// when cairo cannot make one, and the frame draws it directly.
    fn build_cache(&self, key: CacheKey) -> Option<KeyCache> {
        let surface = ImageSurface::create(Format::ARgb32, self.buf_w, self.buf_h).ok()?;
        let cr = Context::new(&surface).ok()?;
        self.scale_to_logical(&cr);
        cr.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
        self.paint_rest(&cr);
        drop(cr);
        surface.flush();
        Some(KeyCache { key, surface })
    }

    fn paint_window(&self, cr: &Context) {
        cr.set_operator(Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = cr.paint();
        cr.set_operator(Operator::Over);
        set_src_rgba(cr, self.theme.case, self.theme.opacity);
        rrect(
            cr,
            0.0,
            0.0,
            self.width as f64,
            self.height as f64,
            self.metrics().case_r,
        );
        let _ = cr.fill();
    }

    fn draw_centered(&self, cr: &Context, s: &str, cx: f64, cy: f64, size: f64, color: u32) {
        cr.set_font_size(size);
        let Some(ext) = cr.text_extents(s).ok() else {
            return;
        };
        set_src(cr, color);
        cr.move_to(
            cx - ext.width() / 2.0 - ext.x_bearing(),
            cy - ext.height() / 2.0 - ext.y_bearing(),
        );
        let _ = cr.show_text(s);
    }

    #[allow(dead_code)]
    fn draw_left(&self, cr: &Context, s: &str, x: f64, cy: f64, size: f64, color: u32) {
        cr.set_font_size(size);
        let Some(ext) = cr.text_extents(s).ok() else {
            return;
        };
        set_src(cr, color);
        cr.move_to(x - ext.x_bearing(), cy - ext.height() / 2.0 - ext.y_bearing());
        let _ = cr.show_text(s);
    }

    fn text_w(&self, cr: &Context, s: &str, size: f64) -> f64 {
        cr.set_font_size(size);
        cr.text_extents(s).map(|e| e.width()).unwrap_or(0.0)
    }

    /// The one visual treatment shared by suggestion pills and the
    /// bar's text/icon chips: the Metrics capsule radius (`pill_r`),
    /// one fill and one 1 px edge stroke — the same fill/border pair
    /// a keycap draws. `r` spans the full bar height; the painted
    /// capsule keeps its Metrics inset and pill height. The callers'
    /// state tables pick the colors; the primary suggestion and an
    /// armed VIA chip stay accent-marked.
    fn paint_bar_box(&self, cr: &Context, r: &Rect, bg: u32, border: Option<u32>) {
        if r.w <= 0.0 {
            return;
        }
        let m = self.metrics();
        let pad_y = (m.bar_h - m.pill_h) / 2.0;
        let rad = m.pill_r;
        cr.new_path();
        rrect(cr, r.x, pad_y, r.w, m.pill_h, rad);
        set_src_rgba(cr, bg, self.theme.opacity);
        let _ = cr.fill();
        if let Some(bcolor) = border {
            cr.new_path();
            rrect(cr, r.x + 0.5, pad_y + 0.5, r.w - 1.0, m.pill_h - 1.0, rad);
            set_src_rgba(cr, bcolor, (self.theme.opacity * 0.7).min(1.0));
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        }
    }

    fn paint_chip(&self, cr: &Context, r: &Rect, label: &str, font_size: f64, bh: f64) {
        if r.w <= 0.0 {
            return;
        }
        let hov = self.inside && r.contains(self.px as f64, self.py as f64);
        // One treatment: the keycap fill, a hairline edge and plain
        // text; hover matches a key's hover.
        let (bg, fg, border) = if hov {
            (self.theme.key_hov, self.theme.keytxt, Some(self.theme.edge))
        } else {
            (self.theme.key, self.theme.keytxt, Some(self.theme.edge))
        };
        self.paint_bar_box(cr, r, bg, border);
        self.draw_centered(cr, label, r.x + r.w / 2.0, bh / 2.0, font_size, fg);
    }

    // Paints the bar's widgets as one page so their coordinates all
    // come from the same BarLayout pass.
    #[allow(clippy::too_many_lines)]
    fn paint_bar(&self, cr: &Context, bar: &BarLayout) {
        let m = self.metrics();
        let bh = m.bar_h;
        let _ = cr.save();
        cr.rectangle(0.0, 0.0, self.width as f64, bh);
        cr.clip();
        rrect(cr, 0.0, 0.0, self.width as f64, self.height as f64, m.case_r);
        cr.clip();
        cr.set_operator(Operator::Source);
        set_src_rgba(cr, self.theme.case, self.theme.opacity);
        cr.rectangle(0.0, 0.0, self.width as f64, bh);
        let _ = cr.fill();
        // Pills and chips sit inside the corner radius, so the rest of
        // the bar keeps only the cheap rectangular clip; a rounded clip
        // makes cairo composite every later op through a mask.
        let _ = cr.restore();
        let _ = cr.save();
        cr.rectangle(0.0, 0.0, self.width as f64, bh);
        cr.clip();
        cr.set_operator(Operator::Over);
        // The bar's free area (where the pills sit): the live
        // waveform while dictation records, the remap status line
        // while a VIA session is up, the suggestion pills otherwise.
        // Transcribing draws nothing extra — no pill-sized text, no
        // status line.
        #[cfg(feature = "dictation")]
        if self.dict_state == DictState::Recording {
            self.paint_waveform(cr, bar);
        } else {
            self.paint_free_area(cr, bar);
        }
        #[cfg(not(feature = "dictation"))]
        self.paint_free_area(cr, bar);

        // Layout chip
        self.paint_chip(
            cr,
            &bar.layout_rect,
            self.layout().name,
            m.font_chip(),
            bh,
        );

        // VIA chip: dim with no device, normal with one, accent ring
        // while the remap session is armed or captured. Same box
        // treatment as the other chips.
        {
            let vr = &bar.via_rect;
            if vr.w > 0.0 {
                let hov = self.inside && vr.contains(self.px as f64, self.py as f64);
                let (bg, fg, border) = match self.via.chip {
                    ViaChipLook::Dim => {
                        (self.theme.key_mod, self.theme.modtxt, Some(self.theme.edge))
                    }
                    ViaChipLook::Normal if hov => {
                        (self.theme.key_hov, self.theme.keytxt, Some(self.theme.edge))
                    }
                    ViaChipLook::Normal => {
                        (self.theme.key, self.theme.keytxt, Some(self.theme.edge))
                    }
                    ViaChipLook::Armed => {
                        (self.theme.on_bg, self.theme.accent, Some(self.theme.accent))
                    }
                    // Zero width: this branch is never drawn.
                    ViaChipLook::Hidden => (self.theme.key_mod, self.theme.modtxt, None),
                };
                self.paint_bar_box(cr, vr, bg, border);
                cr.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
                self.draw_centered(cr, "VIA", vr.x + vr.w / 2.0, bh / 2.0, m.font_chip(), fg);
            }
        }

        // Theme icon button. Same box treatment; the open menu marks
        // the button with the accent.
        let th_hov = self.inside && bar.theme_rect.contains(self.px as f64, self.py as f64);
        let th_active = self.theme_menu_open;
        let (th_bg, th_fg, th_border) = if th_active {
            (self.theme.on_bg, self.theme.accent, Some(self.theme.accent))
        } else if th_hov {
            (self.theme.key_hov, self.theme.keytxt, Some(self.theme.edge))
        } else {
            (self.theme.key, self.theme.keytxt, Some(self.theme.edge))
        };
        self.paint_bar_box(cr, &bar.theme_rect, th_bg, th_border);
        let icon_cy = bh / 2.0;
        // The theme button's glyph: a circle split along the ↗↙
        // diagonal into the theme's background and accent — the
        // active theme at a glance.
        self.draw_contrast_icon(
            cr,
            bar.theme_rect.x + bar.theme_rect.w / 2.0,
            icon_cy,
            m.icon_r(),
            th_fg,
        );

        // Mic button (dictation), same box treatment. While dictation
        // is engaged (recording or transcribing) it takes the armed
        // VIA chip's look — accent ring, not the old filled accent —
        // so the waveform, not the button, carries the visual.
        #[cfg(feature = "dictation")]
        {
            let mic_hov = self.inside && bar.mic_rect.contains(self.px as f64, self.py as f64);
            let armed = self.dict_state != DictState::Idle;
            let (mic_bg, mic_fg, mic_border) = if armed {
                (self.theme.on_bg, self.theme.accent, Some(self.theme.accent))
            } else if mic_hov {
                (self.theme.key_hov, self.theme.keytxt, Some(self.theme.edge))
            } else {
                (self.theme.key, self.theme.keytxt, Some(self.theme.edge))
            };
            self.paint_bar_box(cr, &bar.mic_rect, mic_bg, mic_border);
            let mic_cx = bar.mic_rect.x + bar.mic_rect.w / 2.0;
            let mic_cy = bh / 2.0;
            self.draw_mic_icon(cr, mic_cx, mic_cy, m.s, mic_fg);
        }

        let close_hov = self.inside && bar.close_rect.contains(self.px as f64, self.py as f64);
        // Neutral at rest (the modifier-text tone); the red is
        // reserved for "this is destructive" under the pointer.
        self.draw_centered(
            cr,
            "\u{00d7}",
            bar.close_rect.x + bar.close_rect.w / 2.0,
            bh / 2.0,
            m.font_close(),
            if close_hov { self.theme.close } else { self.theme.modtxt },
        );
        let _ = cr.restore();
    }

    /// The bar's free area when dictation is not recording: the remap
    /// status line while a VIA session reports, the suggestion pills
    /// otherwise.
    fn paint_free_area(&self, cr: &Context, bar: &BarLayout) {
        let m = self.metrics();
        let bh = m.bar_h;
        let fs = m.font_sug();
        if let Some(status) = self.via.status.as_ref() {
            cr.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
            // The line owns the pill area only: from the left margin to
            // one gap before the first chip (BarLayout's free area,
            // measured once), ending in "…" when it does not fit, so
            // it never runs under the chips.
            let x0 = bar.text_left;
            let x1 = bar.text_right;
            let avail = (x1 - x0).max(0.0);
            let mut line = status.clone();
            if self.text_w(cr, &line, fs) > avail {
                let chars: Vec<char> = status.chars().collect();
                let mut n = chars.len();
                loop {
                    line = chars[..n].iter().collect::<String>().trim_end().to_string() + "…";
                    if n == 0 || self.text_w(cr, &line, fs) <= avail {
                        break;
                    }
                    n -= 1;
                }
            }
            self.draw_centered(cr, &line, (x0 + x1) / 2.0, bh / 2.0, fs, self.theme.accent);
        } else {
            // The pills stay between the two gaps the bar budget
            // leaves; paint_bar_box keeps the Metrics pill inset.
            let suggs = self.ctl.suggestions();

            for i in 0..bar.n_sugg {
                let r = bar.sugg[i];
                let hov = self.inside && r.contains(self.px as f64, self.py as f64);
                let is_primary = r.idx == 0;
                let word = suggs.get(r.idx).copied().unwrap_or("");
                if word.is_empty() {
                    continue;
                }

                let rx = r.x;
                let rw = r.w;

                // One treatment for pills and chips (paint_bar_box):
                // keycap fill, hairline edge, capsule radius; the
                // primary suggestion stays accent-marked, hover
                // matches a key's hover.
                let (bg, fg, border) = if is_primary {
                    if hov {
                        (self.theme.key_press, self.theme.ink, None)
                    } else {
                        (self.theme.on_bg, self.theme.on_txt, Some(self.theme.accent))
                    }
                } else if hov {
                    (self.theme.key_hov, self.theme.keytxt, Some(self.theme.edge))
                } else {
                    (self.theme.key, self.theme.keytxt, Some(self.theme.edge))
                };

                // `r` is the full-bar-height rect; the helper keeps
                // its own Metrics inset and pill height.
                self.paint_bar_box(cr, &r, bg, border);

                let weight = if is_primary { FontWeight::Bold } else { FontWeight::Normal };
                cr.select_font_face(FONT, FontSlant::Normal, weight);
                self.draw_centered(cr, word, rx + rw / 2.0, bh / 2.0, fs, fg);
            }
        }
    }

    /// The live waveform while dictation records: [`WAVE_BARS`]
    /// rounded vertical bars in one tight cluster centered on the
    /// keyboard's width, in the theme accent, rolling: oldest on the
    /// left, newest on the right, each finished bar sliding left as
    /// the next fills. The cluster never reaches the chips: when the
    /// room either side of the center is too small, the oldest bars
    /// are dropped first. Heights come from the level (0..1) with a
    /// minimum stub, so silence still shows a calm centered line.
    /// Levels are plain data — this never touches cpal.
    #[cfg(feature = "dictation")]
    fn paint_waveform(&self, cr: &Context, bar: &BarLayout) {
        let n = self.levels.len();
        if n == 0 {
            return;
        }
        let m = self.metrics();
        let x0 = bar.text_left;
        let x1 = bar.text_right;
        let bw = (1.5 * m.g).max(1.0);
        let gap = 0.6 * m.g;
        let step = bw + gap;
        // Centered on the whole keyboard; it may use as much room on
        // each side of the center as the nearer edge of the free area
        // allows, so it never runs under the chips.
        let center = self.width as f64 / 2.0;
        let room = 2.0 * (center - x0).min(x1 - center).max(0.0);
        let fit = ((room + gap) / step).floor() as usize;
        let levels = &self.levels[n - n.min(fit)..];
        if levels.is_empty() {
            return;
        }
        let width = levels.len() as f64 * step - gap;
        let left = center - width / 2.0;
        // The stub keeps silence visible as a calm line rather than a
        // blank bar.
        let max_h = m.pill_h;
        let stub = (0.09 * m.pill_h).max(1.5);
        let cy = m.bar_h / 2.0;
        set_src_rgba(cr, self.theme.accent, self.theme.opacity);
        for (i, &lvl) in levels.iter().enumerate() {
            let lvl = (lvl as f64).clamp(0.0, 1.0);
            let h = (stub + lvl * (max_h - stub)).max(stub);
            let x = left + step * i as f64;
            cr.new_path();
            rrect(cr, x, cy - h / 2.0, bw, h, bw / 2.0);
            let _ = cr.fill();
        }
    }

    /// The theme button's glyph: a circle split along the ↗↙ diagonal
    /// into the theme's background (upper-left half) and its accent
    /// (lower-right half), with a thin ring in the chip's text color.
    fn draw_contrast_icon(&self, cr: &Context, cx: f64, cy: f64, r: f64, color: u32) {
        let q = 3.0 * std::f64::consts::FRAC_PI_4;
        cr.new_path();
        cr.arc(cx, cy, r, q, q + std::f64::consts::PI);
        cr.close_path();
        set_src(cr, self.theme.case);
        let _ = cr.fill();
        cr.new_path();
        cr.arc(cx, cy, r, q - std::f64::consts::PI, q);
        cr.close_path();
        set_src(cr, self.theme.accent);
        let _ = cr.fill();
        cr.new_path();
        cr.arc(cx, cy, r, 0.0, 2.0 * std::f64::consts::PI);
        set_src(cr, color);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
    }

    /// A simple microphone glyph: capsule body, holder arc, and stem.
    #[cfg(feature = "dictation")]
    fn draw_mic_icon(&self, cr: &Context, cx: f64, cy: f64, s: f64, color: u32) {
        set_src(cr, color);
        let w = 0.14 * s;
        let h = 0.22 * s;
        let top = cy - 0.17 * s;
        rrect(cr, cx - w / 2.0, top, w, h, w / 2.0);
        let _ = cr.fill();
        cr.new_path();
        cr.arc(cx, cy - 0.03 * s, 0.13 * s, 0.0, std::f64::consts::PI);
        cr.set_line_width(0.03 * s);
        let _ = cr.stroke();
        cr.new_path();
        cr.move_to(cx, cy + 0.10 * s);
        cr.line_to(cx, cy + 0.17 * s);
        let _ = cr.stroke();
    }

    // One menu canvas: shadow, toggles, and 12 theme cards.
    #[allow(clippy::too_many_lines)]
    fn paint_theme_menu(&self, cr: &Context, bar: &BarLayout) {
        if !self.theme_menu_open || bar.theme_menu_rect.w <= 0.0 {
            return;
        }
        let mr = &bar.theme_menu_rect;
        let m = self.metrics();

        let _ = cr.save();
        // Drop shadow behind menu
        cr.new_path();
        rrect(
            cr,
            mr.x + m.menu_shadow_dx(),
            mr.y + m.menu_shadow_dy(),
            mr.w,
            mr.h,
            m.menu_r(),
        );
        // The theme's own shadow tone (near-black on dark themes, a
        // soft gray on Light), so the shadow never out-contrasts the
        // theme.
        set_src_rgba(cr, self.theme.key_shdw, 0.45);
        let _ = cr.fill();

        // Menu background panel (using current theme's dark case)
        cr.new_path();
        rrect(cr, mr.x, mr.y, mr.w, mr.h, m.menu_r());
        set_src(cr, self.theme.case);
        let _ = cr.fill();

        // Menu border outline in accent
        cr.new_path();
        rrect(cr, mr.x + 0.5, mr.y + 0.5, mr.w - 1.0, mr.h - 1.0, m.menu_r());
        set_src_rgba(cr, self.theme.accent, 0.50);
        cr.set_line_width(1.0);
        let _ = cr.stroke();

        // Text and inner padding follow the menu's fit scale, so a
        // shrunk menu stays proportional.
        let k = bar.menu_scale;
        let fs = m.font_menu() * k;
        let text_pad = m.menu_text_pad() * k;
        let right_pad = m.menu_right_pad() * k;
        // Prediction on/off toggle (top row)
        {
            let pr = &bar.pred_toggle_rect;
            let pr_hov = self.inside && pr.contains(self.px as f64, self.py as f64);
            let active = self.ctl.prediction();
            let (bg, fg) = if active {
                (self.theme.on_bg, self.theme.accent)
            } else if pr_hov {
                (self.theme.key_hov, self.theme.keytxt)
            } else {
                (self.theme.key_mod, self.theme.modtxt)
            };
            cr.new_path();
            rrect(cr, pr.x, pr.y, pr.w, pr.h, m.menu_item_r());
            set_src(cr, bg);
            let _ = cr.fill();
            if active {
                cr.new_path();
                rrect(cr, pr.x + 0.5, pr.y + 0.5, pr.w - 1.0, pr.h - 1.0, m.menu_item_r());
                set_src(cr, self.theme.accent);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            }
            cr.select_font_face(FONT, FontSlant::Normal, if active { FontWeight::Bold } else { FontWeight::Normal });
            self.draw_left(cr, "Prediction", pr.x + text_pad, pr.y + pr.h / 2.0, fs, fg);
            self.draw_centered(cr, if active { "on" } else { "off" }, pr.x + pr.w - right_pad, pr.y + pr.h / 2.0, fs * 0.9, fg);
        }
        // Tab accepts-prediction toggle (second row)
        {
            let tr = &bar.tab_toggle_rect;
            let tr_hov = self.inside && tr.contains(self.px as f64, self.py as f64);
            let active = self.ctl.tab_accepts_prediction();
            let (bg, fg) = if active {
                (self.theme.on_bg, self.theme.accent)
            } else if tr_hov {
                (self.theme.key_hov, self.theme.keytxt)
            } else {
                (self.theme.key_mod, self.theme.modtxt)
            };
            cr.new_path();
            rrect(cr, tr.x, tr.y, tr.w, tr.h, m.menu_item_r());
            set_src(cr, bg);
            let _ = cr.fill();
            if active {
                cr.new_path();
                rrect(cr, tr.x + 0.5, tr.y + 0.5, tr.w - 1.0, tr.h - 1.0, m.menu_item_r());
                set_src(cr, self.theme.accent);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            }
            cr.select_font_face(FONT, FontSlant::Normal, if active { FontWeight::Bold } else { FontWeight::Normal });
            self.draw_left(cr, "Tab accepts", tr.x + text_pad, tr.y + tr.h / 2.0, fs, fg);
            self.draw_centered(cr, if active { "on" } else { "off" }, tr.x + tr.w - right_pad, tr.y + tr.h / 2.0, fs * 0.9, fg);
        }
        for (i, t) in THEMES.iter().enumerate().take(12) {
            let item = &bar.theme_item_rects[i];
            let hov = self.inside && item.contains(self.px as f64, self.py as f64);
            let is_cur = i == self.theme_idx;

            let rx = item.x;
            let ry = item.y;
            let rw = item.w;
            let rh = item.h;

            // Card background in that theme's own case color!
            cr.new_path();
            rrect(cr, rx, ry, rw, rh, m.menu_item_r());
            set_src(cr, t.case);
            let _ = cr.fill();

            // Border styling
            cr.new_path();
            rrect(cr, rx + 0.5, ry + 0.5, rw - 1.0, rh - 1.0, m.menu_item_r());
            if is_cur {
                set_src(cr, t.accent);
                cr.set_line_width(1.5);
                let _ = cr.stroke();
            } else if hov {
                set_src(cr, t.key_press);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            } else {
                set_src(cr, t.edge);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            }

            // Swatch dot on the left colored in that theme's key_press accent
            let dot_x = rx + text_pad;
            let dot_y = ry + rh / 2.0;
            let dot_r = m.menu_dot_r() * k;
            cr.new_path();
            cr.arc(dot_x, dot_y, dot_r, 0.0, 2.0 * std::f64::consts::PI);
            set_src(cr, t.key_press);
            let _ = cr.fill();

            // Theme name rendered in that theme's accent/keytxt
            let weight = if is_cur || hov { FontWeight::Bold } else { FontWeight::Normal };
            cr.select_font_face(FONT, FontSlant::Normal, weight);
            cr.set_font_size(fs);
            let text_color = if is_cur { t.accent } else if hov { t.key_press } else { t.keytxt };
            self.draw_left(cr, t.name, rx + m.menu_name_pad() * k, ry + rh / 2.0, fs, text_color);

            // Active checkmark indicator on the right if current theme
            if is_cur {
                self.draw_centered(cr, "\u{2713}", rx + rw - text_pad, ry + rh / 2.0, fs * 0.9, t.accent);
            }
        }

        let _ = cr.restore();
    }

    /// The bottom-right resize handle: one short arc concentric with
    /// the case corner ("follows the case corner radius"). The radius
    /// keeps every stroked point outside the key rects: a concentric
    /// point enters the key-rect region iff r·cos(θ) and r·sin(θ)
    /// both fall under `key_r`, so the arc radius stays above
    /// √2·key_r, and below the case edge. Every variant draws inside
    /// the bezel margins (within [`Metrics::margin`] of the right or
    /// bottom edge no key rect reaches) and the hit area
    /// ([`View::handle_size`]) is unchanged, so dragging stays as it
    /// is. The arc eases with `handle_anim`: at p=0 it rests dim
    /// (`modtxt`) at its base length and thickness; at p=1 it is
    /// bright `accent`, longer and thicker — a little of the span and
    /// width being lerp'd in, so the entrance reads as one motion.
    fn paint_handle(&self, cr: &Context) {
        let p = self.handle_anim.clamp(0.0, 1.0);
        let m = self.metrics();
        let r = m.case_r - 0.45 * m.margin;
        let ccx = self.width as f64 - m.case_r;
        let ccy = self.height as f64 - m.case_r;
        // Span centered on the 45° corner diagonal; it widens a little
        // as the ease progresses. Growth keeps the whole arc inside
        // the strip (see the radius note above).
        let base = std::f64::consts::FRAC_PI_4 / 2.0;
        let half = base * (1.0 + 0.35 * p);
        let _ = cr.save();
        // Clip the corner strip so the stroke can never reach a key,
        // even at the eased-out longer span.
        cr.rectangle(
            self.width as f64 - self.handle_size() as f64,
            self.height as f64 - self.handle_size() as f64,
            self.handle_size() as f64,
            self.handle_size() as f64,
        );
        cr.clip();
        cr.set_operator(Operator::Over);
        cr.new_path();
        cr.arc(ccx, ccy, r, 2.0 * base - half, 2.0 * base + half);
        set_src(cr, mix_color(self.theme.modtxt, self.theme.accent, p));
        cr.set_line_width(0.055 * m.s * (1.0 + 0.5 * p));
        cr.set_line_cap(cairo::LineCap::Round);
        let _ = cr.stroke();
        let _ = cr.restore();
    }

    /// What this frame's pointer, interaction and modifiers do to the
    /// look of key `idx` drawn at the given cell.
    fn key_state(&self, key: &Key, idx: usize, rx: f64, ry: f64, rw: f64, rh: f64) -> KeyState {
        let (pressed, press_time, repeating) = match self.interaction {
            Interaction::Press {
                key,
                press_time,
                rep_next,
                ..
            } if key == idx => (true, press_time, rep_next.is_some()),
            Interaction::Press { .. }
            | Interaction::Idle
            | Interaction::Move { .. }
            | Interaction::Resize { .. } => (false, 0, false),
        };
        let held = repeating || now_ms().saturating_sub(press_time) >= interaction::HOLD_MS;
        KeyState {
            vis: self.ctl.mods().key_visual(key),
            press: if pressed && held {
                Press::Held
            } else if pressed || self.flash_key == idx as i32 {
                Press::Down
            } else {
                Press::Up
            },
            hov: !key.is_gap()
                && self.inside
                && (self.px as f64) >= rx
                && (self.px as f64) < rx + rw
                && (self.py as f64) >= ry
                && (self.py as f64) < ry + rh,
        }
    }

    #[allow(clippy::too_many_arguments)]
    // Every visual state (idle, latched, hold, press) drawn per key.
    #[allow(clippy::too_many_lines)]
    fn paint_key(&self, cr: &Context, rx: f64, ry: f64, rw: f64, rh: f64, key: &Key, st: KeyState) {
        // A gap with no label is the 0.5u spacing: nothing to draw.
        if key.is_gap() && key.label.is_none() {
            return;
        }
        let m = self.metrics();
        let KeyState { vis, press, hov } = st;
        let on = vis == ModKeyState::Latched || vis == ModKeyState::Locked;
        // Three tiers: character keys (letters, digits, symbols, space)
        // are lifted; modifiers are flat and dimmer; Enter is the only
        // filled accent. Esc is a plain modifier.
        let code = key.code_u16();
        let is_char = key.letter || key.shift_label.is_some() || code == key::KEY_SPACE;
        let is_accent = code == key::KEY_ENTER;
        // (fill, label) for the current state.
        let (bg, label) = if press == Press::Held {
            (self.theme.key_hold, self.theme.ink)
        } else if press == Press::Down {
            (self.theme.key_press, self.theme.ink)
        } else if on {
            (self.theme.on_bg, self.theme.on_txt)
        } else if is_accent {
            (self.theme.accent, self.theme.ink)
        } else if is_char {
            (if hov { self.theme.key_hov } else { self.theme.key }, self.theme.keytxt)
        } else {
            (self.theme.key_mod, if hov { self.theme.keytxt } else { self.theme.modtxt })
        };
        let disp: String;
        if key.letter {
            let c = key.label.and_then(|l| l.chars().next()).unwrap_or(' ');
            let upper = self.ctl.mods().casing().letter_upper();
            disp = if upper {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_ascii_lowercase().to_string()
            };
        } else if let Some(l) = key.label {
            disp = if self.ctl.mods().shift_active() {
                key.shift_label.unwrap_or(l).to_string()
            } else {
                l.to_string()
            };
        } else {
            disp = String::new();
        }
        // The cell is edge-to-edge; the visible gap g is shared: half
        // of it is inset into each neighbouring key.
        let inset = m.g / 2.0;
        let cx = rx + inset;
        let cy = ry + inset;
        let cw = rw - 2.0 * inset;
        let ch = rh - 2.0 * inset;
        let _ = cr.save();
        cr.rectangle(rx, ry, rw, rh);
        cr.clip();
        if self.in_case_corner(rx, ry, rw, rh) {
            rrect(cr, 0.0, 0.0, self.width as f64, self.height as f64, m.case_r);
            cr.clip();
        }
        // SOURCE so a stale neighbor pixel cannot show through the fill.
        cr.set_operator(Operator::Source);
        set_src_rgba(cr, self.theme.case, self.theme.opacity);
        cr.rectangle(rx, ry, rw, rh);
        let _ = cr.fill();
        cr.set_operator(Operator::Over);
        rrect(cr, cx, cy, cw, ch, m.key_r);
        let key_alpha = if self.theme.opacity < 1.0 {
            (self.theme.opacity + 0.08).min(1.0)
        } else {
            1.0
        };
        set_src_rgba(cr, bg, key_alpha);
        let _ = cr.fill();
        rrect(cr, cx + 0.5, cy + 0.5, cw - 1.0, ch - 1.0, m.key_r);
        set_src_rgba(cr, self.theme.edge, key_alpha);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        if !disp.is_empty() {
            // Character keys carry a glyph, modifier keys a word.
            let base_fs = if disp.chars().count() <= 1 {
                m.font_char
            } else {
                m.font_label
            };
            let mut fs = base_fs;
            for line in disp.lines() {
                let n = line.chars().count().max(1) as f64;
                fs = fs.min(cw / (0.62 * n));
            }
            let lines: Vec<&str> = disp.lines().collect();
            if lines.len() == 1 {
                self.draw_centered(cr, lines[0], rx + rw / 2.0, ry + rh / 2.0, fs, label);
            } else {
                let lh = 1.3 * fs;
                let top = ry + rh / 2.0 - (lines.len() as f64 - 1.0) * lh / 2.0;
                for (i, line) in lines.iter().enumerate() {
                    self.draw_centered(cr, line, rx + rw / 2.0, top + i as f64 * lh, fs, label);
                }
            }
        }
        // Caps-lock dot in the key's top-right corner, in the key's
        // own text color (a fixed white is unreadable on Light's pale
        // active fill).
        if vis == ModKeyState::Locked {
            set_src(cr, self.theme.on_txt);
            cr.arc(
                rx + rw - 0.23 * m.s,
                ry + 0.23 * m.s,
                0.08 * m.s,
                0.0,
                2.0 * std::f64::consts::PI,
            );
            let _ = cr.fill();
        }
        let _ = cr.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_shape() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        // Fast start / gentle settle: half the move happens in the
        // first ~20% of the time (1-(1-t)^3 = 0.5 at t ≈ 0.206).
        assert!(ease_out_cubic(0.206) > 0.49);
        assert!(ease_out_cubic(0.5) > 0.87);
        // Inputs outside 0..=1 clamp so a late frame cannot overshoot.
        assert_eq!(ease_out_cubic(-5.0), 0.0);
        assert_eq!(ease_out_cubic(2.0), 1.0);
    }

    #[test]
    fn progress_reaches_exactly_one_and_stays() {
        // The frame-request loop stops when progress is 1.0, so the
        // exact equality at the end of the ease (and after it) is a
        // contract, not an implementation detail.
        assert_eq!(anim_progress(800, 0, 800), 1.0);
        for extra in [1u64, 17, 1_000, 86_400_000] {
            assert_eq!(anim_progress(800 + extra, 0, 800), 1.0);
        }
        // Clock that has not reached `since` yet: clamp to the start.
        assert_eq!(anim_progress(0, 800, 800), 0.0);
        // A zero duration means already settled.
        assert_eq!(anim_progress(0, 0, 0), 1.0);
    }

    #[test]
    fn progress_monotone_inside_the_ease() {
        let mut prev = 0.0;
        for step in [1u64, 10, 40, 80, 159] {
            let p = anim_progress(step, 0, 160);
            assert!(p > prev, "at {step} ms: {p} <= {prev}");
            assert!(p < 1.0, "at {step} ms: {p} finished early");
            prev = p;
        }
    }

    #[test]
    fn mix_color_endpoints_and_midpoint() {
        assert_eq!(mix_color(0x000000, 0xffffff, 0.0), 0x000000);
        assert_eq!(mix_color(0x000000, 0xffffff, 1.0), 0xffffff);
        // Linear per channel; each of the 24 bits must survive.
        assert_eq!(mix_color(0x102030, 0x506070, 0.5), 0x304050);
    }
}
