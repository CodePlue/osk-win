//! The wlr-layer-shell cairo UI.
//!
//! The keyboard is a layer surface (layer=top, keyboard_interactivity=none)
//! so it is always on top, never steals keyboard focus, and still receives
//! pointer events. Injected keys land in whatever window the user focused.
//!
//! Buffer contract (what Hyprland expects from a layer client):
//!   * fractional scale + wp_viewport destination = logical size, buffer
//!     scale 1
//!   * damage_buffer in buffer pixels
//!   * opaque region over the whole surface (XRGB, corners opaque black)
//!   * one commit per presented frame, except a key tap which commits
//!     immediately

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::io::{AsFd, AsRawFd};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface, Operator};
use osk_core::events::{complete_word, tap_events, OskEvent, TAP_MAX_EVENTS};
use osk_core::key::{key, Key, Layout};
use osk_core::layout::ALL_LAYOUTS;
use osk_core::mods::{mod_index, ModKeyState, ModifierEngine};
use osk_core::predict::Predictor;
use osk_core::typekey::{type_string, CharMap};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::WpFractionalScaleV1,
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::ZwpRelativePointerV1,
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use osk_backend::{Backend, BackendSink};

// ---- geometry, in key units (1u = one normal key) ----
// Thinner keyboard profile: 0.12u margin, 0.65u top bar, 42px unit.
const MARGIN_U: f64 = 0.12;
const BAR_U: f64 = 0.65;
const INSET_X_U: f64 = 0.040;
const INSET_Y_U: f64 = 0.040;
const KEY_RAD_U: f64 = 0.16;
const CASE_RAD_U: f64 = 0.5;
const FONT_U: f64 = 0.36;
/// Default pixel size of 1u.
const U0: f64 = 46.0;
/// Smallest surface the resize handle allows.
const MIN_W: i32 = 200;
const MIN_H: i32 = 180;
// Linux input code for the left mouse button. wlroots/Hyprland report this
// in wl_pointer.button instead of the Wayland number 1.
const BTN_LEFT: u32 = 272;
const BTN_RIGHT: u32 = 273;
const FLASH_MS: u64 = 150;

// Sans-serif face for the labels; it carries every glyph we draw
// (letters, digits, and the arrow/symbol caps) in one font.
const FONT: &str = "Adwaita Sans";

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub name: &'static str,
    pub case: u32,
    pub key: u32,
    pub key_hov: u32,
    #[allow(dead_code)]
    pub key_hi: u32,
    #[allow(dead_code)]
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
        name: "Light",
        case: 0xe4e7eb,
        key: 0xffffff,
        key_hov: 0xd9e2ec,
        key_hi: 0xf8fafc,
        key_shdw: 0xc1c9d2,
        key_mod: 0xdce1e8,
        keytxt: 0x102a43,
        modtxt: 0x486581,
        accent: 0x0b69a3,
        ink: 0xffffff,
        on_bg: 0xd0e2f2,
        on_txt: 0x0b69a3,
        key_press: 0x0b69a3,
        key_hold: 0x084e7a,
        hair: 0xcbd2d9,
        edge: 0xbcccdc,
        sugtxt: 0x486581,
        sugtxt_first: 0x0b69a3,
        part: 0x0b69a3,
        close: 0xd9534f,
        opacity: 1.0,
    },
];

/// Token for the Wayland connection fd in the poller.
const WL_TOKEN: mio::Token = mio::Token(0);

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

fn debug() -> bool {
    std::env::var_os("OSK_DEBUG").is_some()
}

fn now_ms() -> u64 {
    START.elapsed().as_millis() as u64
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
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    /// Index into the suggestion list (0 for close/layout).
    idx: usize,
}

impl Rect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Bar element positions measured during render.
#[derive(Clone, Copy)]
struct BarLayout {
    sugg: [Rect; 5],
    n_sugg: usize,
    layout_rect: Rect,
    theme_rect: Rect,
    close_rect: Rect,
    theme_menu_rect: Rect,
    theme_item_rects: [Rect; 12],
    dict_rect: Rect,
    dict_menu_rect: Rect,
    dict_tab_toggle_rect: Rect,
    dict_clear_rect: Rect,
    dict_prev_rect: Rect,
    dict_next_rect: Rect,
    dict_remove_rects: [Rect; 8],
    dict_words: [Option<&'static str>; 8],
    n_dict_items: usize,
}

/// One shm frame. The cairo surface owns the pixel buffer; the memfd holds
/// a copy of it for the compositor to mmap. Two frames are kept so the
/// compositor can read one while we draw into the other.
struct Frame {
    surface: Option<ImageSurface>,
    file: Option<File>,
    pool: Option<wl_shm_pool::WlShmPool>,
    buf: Option<wl_buffer::WlBuffer>,
    attached: bool,
    buf_w: i32,
    buf_h: i32,
    stride: i32,
}

impl Frame {
    const fn new() -> Self {
        Self {
            surface: None,
            file: None,
            pool: None,
            buf: None,
            attached: false,
            buf_w: 0,
            buf_h: 0,
            stride: 0,
        }
    }
}

/// Saved preferences and window geometry persisted between runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistentConfig {
    #[serde(default)]
    pub theme: String,
    #[serde(default)]
    pub layout: String,
    #[serde(default = "default_pos_x")]
    pub pos_x: i32,
    #[serde(default = "default_pos_y")]
    pub pos_y: i32,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default)]
    pub tab_accepts_prediction: bool,
}

fn default_pos_x() -> i32 {
    600
}

fn default_pos_y() -> i32 {
    450
}

impl Default for PersistentConfig {
    fn default() -> Self {
        Self {
            theme: "Default".to_string(),
            layout: "65%".to_string(),
            pos_x: default_pos_x(),
            pos_y: default_pos_y(),
            width: 0,
            height: 0,
            tab_accepts_prediction: false,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("OSK_CONFIG") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("osk").join("config.json"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join(".config").join("osk").join("config.json"));
        }
    }
    None
}

impl PersistentConfig {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json.as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

struct State {
    backend: Box<dyn Backend>,
    qh: QueueHandle<State>,
    // bound globals
    comp: Option<wl_compositor::WlCompositor>,
    seat: Option<wl_seat::WlSeat>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    frac_mgr: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    rel_mgr: Option<ZwpRelativePointerManagerV1>,
    // surface objects
    surface: Option<wl_surface::WlSurface>,
    layer: Option<ZwlrLayerSurfaceV1>,
    pointer: Option<wl_pointer::WlPointer>,
    frac: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    rel: Option<ZwpRelativePointerV1>,
    // frames
    frames: [Frame; 2],
    cur_frame: usize,
    attached_w: i32,
    attached_h: i32,
    // geometry (logical)
    width: i32,
    height: i32,
    pos_x: i32,
    pos_y: i32,
    scale_120: i32,
    buf_scale: i32,
    ready: bool,
    // pointer
    px: i32,
    py: i32,
    inside: bool,
    // interaction
    pressed: i32,
    press_time: u64,
    hold_fired: bool,
    // flash: key keeps its pressed look briefly after release
    flash_key: i32,
    flash_until: u64,
    moving: bool,
    resizing: bool,
    rsz_w: i32,
    rsz_h: i32,
    grab_lx: i32,
    grab_ly: i32,
    drag_fx: f64,
    drag_fy: f64,
    // key repeat
    rep_key: i32,
    rep_next: u64,
    grid_cols: i32,
    // engines
    mods: ModifierEngine,
    predictor: Predictor,
    charmap: Option<CharMap>,
    // rendering
    dirty: bool,
    frame_pending: bool,
    frame_gen: u32,
    presented: bool,
    last_commit_ms: u64,
    commit_x: i32,
    commit_y: i32,
    shown_x: i32,
    shown_y: i32,
    last_hot: u32,
    last_geom: (i32, i32, i32, i32),
    // measured bar layout (filled during render)
    sugg: [Rect; 5],
    n_sugg: usize,
    layout_rect: Rect,
    theme_rect: Rect,
    close_rect: Rect,
    layout_idx: usize,
    theme_idx: usize,
    theme: Theme,
    theme_menu_open: bool,
    theme_menu_rect: Rect,
    theme_item_rects: [Rect; 12],
    tab_accepts_prediction: bool,
    dict_menu_open: bool,
    dict_page: usize,
    dict_rect: Rect,
    dict_menu_rect: Rect,
    dict_tab_toggle_rect: Rect,
    dict_clear_rect: Rect,
    dict_prev_rect: Rect,
    dict_next_rect: Rect,
    dict_remove_rects: [Rect; 8],
    dict_words: [Option<&'static str>; 8],
    n_dict_items: usize,
    // control
    quit: bool,
}

impl State {
    fn new(
        backend: Box<dyn Backend>,
        qh: QueueHandle<State>,
    ) -> Self {
        let config = PersistentConfig::load();
        let initial_theme_idx = std::env::var("OSK_THEME")
            .ok()
            .and_then(|name| {
                THEMES.iter().position(|t| t.name.eq_ignore_ascii_case(&name))
            })
            .or_else(|| {
                if !config.theme.is_empty() {
                    THEMES.iter().position(|t| t.name.eq_ignore_ascii_case(&config.theme))
                } else {
                    None
                }
            })
            .unwrap_or(0);
        let theme = THEMES[initial_theme_idx];

        let initial_layout_idx = if !config.layout.is_empty() {
            ALL_LAYOUTS
                .iter()
                .position(|l| l.name.eq_ignore_ascii_case(&config.layout))
                .unwrap_or(0)
        } else {
            0
        };

        let initial_pos_x = if config.pos_x >= 0 { config.pos_x } else { 600 };
        let initial_pos_y = if config.pos_y >= 0 { config.pos_y } else { 450 };
        let initial_w = if config.width >= MIN_W { config.width } else { 0 };
        let initial_h = if config.height >= MIN_H { config.height } else { 0 };

        let mut state = State {
            backend,
            qh,
            comp: None,
            seat: None,
            shm: None,
            layer_shell: None,
            frac_mgr: None,
            viewporter: None,
            rel_mgr: None,
            surface: None,
            layer: None,
            pointer: None,
            frac: None,
            viewport: None,
            rel: None,
            frames: [Frame::new(), Frame::new()],
            cur_frame: 0,
            attached_w: 0,
            attached_h: 0,
            width: initial_w,
            height: initial_h,
            pos_x: initial_pos_x,
            pos_y: initial_pos_y,
            scale_120: 120,
            buf_scale: 1,
            ready: false,
            px: 0,
            py: 0,
            inside: false,
            pressed: -1,
            press_time: 0,
            hold_fired: false,
            flash_key: -1,
            flash_until: 0,
            moving: false,
            resizing: false,
            rsz_w: 0,
            rsz_h: 0,
            grab_lx: 0,
            grab_ly: 0,
            drag_fx: 0.0,
            drag_fy: 0.0,
            rep_key: -1,
            rep_next: 0,
            grid_cols: 0,
            mods: ModifierEngine::new(),
            predictor: Predictor::new(),
            charmap: CharMap::new(),
            dirty: false,
            frame_pending: false,
            frame_gen: 0,
            presented: false,
            last_commit_ms: 0,
            commit_x: 0,
            commit_y: 0,
            shown_x: initial_pos_x,
            shown_y: initial_pos_y,
            last_hot: 0,
            last_geom: (-1, -1, -1, -1),
            sugg: [Rect::default(); 5],
            n_sugg: 0,
            layout_rect: Rect::default(),
            theme_rect: Rect::default(),
            close_rect: Rect::default(),
            theme_menu_open: false,
            theme_menu_rect: Rect::default(),
            theme_item_rects: [Rect::default(); 12],
            tab_accepts_prediction: config.tab_accepts_prediction,
            dict_menu_open: false,
            dict_page: 0,
            dict_rect: Rect::default(),
            dict_menu_rect: Rect::default(),
            dict_tab_toggle_rect: Rect::default(),
            dict_clear_rect: Rect::default(),
            dict_prev_rect: Rect::default(),
            dict_next_rect: Rect::default(),
            dict_remove_rects: [Rect::default(); 8],
            dict_words: [None; 8],
            n_dict_items: 0,
            layout_idx: initial_layout_idx,
            theme_idx: initial_theme_idx,
            theme,
            quit: false,
        };
        state.compute_grid_cols();
        state
    }

    fn save_config(&self) {
        let config = PersistentConfig {
            theme: self.theme.name.to_string(),
            layout: self.layout().name.to_string(),
            pos_x: self.pos_x,
            pos_y: self.pos_y,
            width: self.width,
            height: self.height,
            tab_accepts_prediction: self.tab_accepts_prediction,
        };
        config.save();
    }

    fn layout(&self) -> &'static Layout {
        ALL_LAYOUTS[self.layout_idx]
    }

    fn compute_grid_cols(&mut self) {
        let layout = self.layout();
        let mut cols = 0;
        for row in layout.rows {
            let total: i32 = row.keys.iter().map(|k| k.width as i32).sum();
            if total > cols {
                cols = total;
            }
        }
        self.grid_cols = cols;
    }

    /// Logical pixels -> buffer pixels at the current scale.
    fn buffer_px(&self, logical: i32) -> i32 {
        if logical <= 0 {
            return 0;
        }
        let scale = if self.scale_120 > 0 {
            self.scale_120
        } else {
            120
        };
        if self.viewport.is_some() {
            (logical * scale + 119) / 120
        } else {
            let s = (scale / 120).max(1);
            logical * s
        }
    }

    // ---- geometry ----

    /// Pixels per 1.0u key, with the bezel margin on both sides.
    fn unit_x(&self) -> f64 {
        if self.width <= 0 {
            return 0.0;
        }
        let cols_u = self.grid_cols as f64 / 20.0;
        self.width as f64 / (cols_u + 2.0 * MARGIN_U)
    }

    fn bar_h(&self) -> f64 {
        BAR_U * self.unit_x()
    }

    fn row_h(&self) -> f64 {
        if self.height <= 0 {
            return 0.0;
        }
        let layout = self.layout();
        ((self.height as f64 - self.bar_h() - 2.0 * MARGIN_U * self.unit_x())
            / layout.rows.len() as f64)
            .max(1.0)
    }

    /// The smaller of the horizontal unit and the row height. Every
    /// drawn metric (gap, radius, font) scales with it, so a resize
    /// scales the whole keyboard and the layout holds at any size.
    fn k(&self) -> f64 {
        self.unit_x().min(self.row_h())
    }

    fn key_rect(&self, r: usize, i: usize) -> (f64, f64, f64, f64) {
        let layout = self.layout();
        let u = self.unit_x();
        let row_h = self.row_h();
        let row_total: i32 = layout.rows[r].keys.iter().map(|k| k.width as i32).sum();
        let mut col = (self.grid_cols - row_total) / 2;
        for j in 0..i {
            col += layout.rows[r].keys[j].width as i32;
        }
        (
            MARGIN_U * u + col as f64 / 20.0 * u,
            self.bar_h() + MARGIN_U * u + r as f64 * row_h,
            layout.rows[r].keys[i].width as f64 / 20.0 * u,
            row_h,
        )
    }

    fn key_at(&self, x: i32, y: i32) -> Option<usize> {
        if y < self.bar_h() as i32 || self.width == 0 {
            return None;
        }
        if self.dict_menu_open && self.dict_menu_rect.contains(x as f64, y as f64) {
            return None;
        }
        if self.theme_menu_open && self.theme_menu_rect.contains(x as f64, y as f64) {
            return None;
        }
        let layout = self.layout();
        for (r, row) in layout.rows.iter().enumerate() {
            for (i, k) in row.keys.iter().enumerate() {
                if k.code == 0 {
                    continue;
                }
                let (rx, ry, rw, rh) = self.key_rect(r, i);
                if (x as f64) >= rx
                    && (x as f64) < rx + rw
                    && (y as f64) >= ry
                    && (y as f64) < ry + rh
                {
                    return Some(r * 100 + i);
                }
            }
        }
        None
    }

    /// Side of the bottom-right resize handle, in logical pixels.
    fn handle_size(&self) -> i32 {
        if self.width <= 0 {
            return 24;
        }
        ((0.5 * self.unit_x()).max(24.0)) as i32
    }

    fn handle_hov(&self) -> bool {
        self.inside
            && self.width > 0
            && self.px >= self.width - self.handle_size()
            && self.py >= self.height - self.handle_size()
    }

    fn key_of(&self, idx: usize) -> &Key {
        &self.layout().rows[idx / 100].keys[idx % 100]
    }

    fn apply_geometry(&mut self) {
        let geom = (self.pos_x, self.pos_y, self.width, self.height);
        // Identical re-sends are skipped so a stationary pointer during a
        // drag costs no protocol traffic.
        if geom == self.last_geom {
            return;
        }
        self.last_geom = geom;
        let Some(layer) = self.layer.as_ref() else {
            return;
        };
        // anchor top|left and position via margins: the left/top margins
        // are the surface origin.
        layer.set_anchor(Anchor::Top | Anchor::Left);
        layer.set_margin(self.pos_y, 0, 0, self.pos_x);
        layer.set_size(self.width as u32, self.height as u32);
        self.sync_regions();
        self.sync_viewport();
        self.sync_buffer_scale();
        if self.frame_pending {
            // A commit is already in flight; the requests above take
            // effect on it. The watchdog forces it if the callback never
            // comes.
            self.dirty = true;
            return;
        }
        // A size change has to commit together with a buffer at the new
        // pixel size; a pure move can commit with no new buffer.
        if self.ready && (self.width != self.attached_w || self.height != self.attached_h) {
            self.dirty = true;
            self.request_redraw();
            return;
        }
        // Geometry requests take effect on the next commit. Pace to one
        // commit per presented frame once the surface has been shown;
        // before that, no callback will fire.
        if self.presented {
            self.arm_frame();
        }
        if let Some(surf) = self.surface.as_ref() {
            surf.commit();
        }
        self.commit_x = self.pos_x;
        self.commit_y = self.pos_y;
        self.last_commit_ms = now_ms();
    }

    fn sync_regions(&mut self) {
        let Some(surf) = self.surface.as_ref() else {
            return;
        };
        let Some(comp) = self.comp.as_ref() else {
            return;
        };
        if self.width <= 0 || self.height <= 0 {
            return;
        }
        let region = comp.create_region(&self.qh, ());
        region.add(0, 0, self.width, self.height);
        eprintln!(
            "[dbg] input_region set to 0,0 {}x{}",
            self.width, self.height
        );
        surf.set_input_region(Some(&region));
        region.destroy();
        surf.set_opaque_region(None);
    }

    fn sync_viewport(&mut self) {
        let Some(vp) = self.viewport.as_ref() else {
            return;
        };
        if self.width <= 0 || self.height <= 0 {
            return;
        }
        vp.set_destination(self.width, self.height);
    }

    fn sync_buffer_scale(&mut self) {
        // Fractional-scale path: buffer scale stays 1. Without a viewport,
        // fall back to an integer buffer scale.
        if self.viewport.is_some() {
            return;
        }
        let s = (if self.scale_120 > 0 {
            self.scale_120
        } else {
            120
        } / 120)
            .max(1);
        if s == self.buf_scale {
            return;
        }
        self.buf_scale = s;
        if let Some(surf) = self.surface.as_ref() {
            surf.set_buffer_scale(s);
        }
    }

    fn arm_frame(&mut self) {
        if self.frame_pending {
            return;
        }
        let Some(surf) = self.surface.as_ref() else {
            return;
        };
        self.frame_gen += 1;
        let _cb = surf.frame(&self.qh, self.frame_gen);
        self.frame_pending = true;
    }

    // ---- redraw ----

    fn request_redraw(&mut self) {
        self.dirty = true;
        if !self.frame_pending {
            // No commit in flight: render now; its commit requests the
            // frame callback that presents this and any later coalesced
            // redraws.
            self.dirty = false;
            self.render();
        }
    }

    fn render(&mut self) {
        if !self.ready {
            return;
        }
        let buf_w = self.buffer_px(self.width);
        let buf_h = self.buffer_px(self.height);
        if buf_w <= 0 || buf_h <= 0 {
            return;
        }
        let prefer = self.cur_frame;
        let order = [prefer, (prefer + 1) % 2];
        let mut chosen: Option<usize> = None;
        for &i in &order {
            if self.frames[i].attached {
                continue;
            }
            if self.frame_alloc(i, buf_w, buf_h) {
                chosen = Some(i);
                break;
            }
        }
        let Some(i) = chosen else {
            // Both buffers are in use; a release or the frame callback
            // will retry.
            self.dirty = true;
            return;
        };
        self.cur_frame = i;
        // Take the surface out so the drawing borrows a local, not self.
        let Some(mut surface) = self.frames[i].surface.take() else {
            self.dirty = true;
            return;
        };
        // Measure the bar first: text extents need a live context, and
        // the rects are stored for hit testing.
        let Ok(meas) = Context::new(&surface) else {
            self.frames[i].surface = Some(surface);
            self.dirty = true;
            return;
        };
        meas.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
        let bar = self.measure_bar(&meas);
        drop(meas);
        let Ok(cr) = Context::new(&surface) else {
            self.frames[i].surface = Some(surface);
            self.dirty = true;
            return;
        };
        self.paint(&cr, &bar);
        drop(cr);
        let mut write_ok = false;
        if let Ok(pixels) = surface.data() {
            if let Some(file) = self.frames[i].file.as_mut() {
                // The buffer is the file's first `need` bytes; without rewinding,
                // the second frame appends past the buffer and the compositor
                // keeps showing frame one.
                write_ok = file.seek(SeekFrom::Start(0)).is_ok()
                    && file.write_all(&pixels).is_ok();
            }
        }
        self.frames[i].surface = Some(surface);
        if !write_ok {
            self.dirty = true;
            return;
        }
        if std::env::var("OSK_DEBUG_PPM").is_ok() {
            let px = self.px;
            let py = self.py;
            let inside = self.inside;
            let key_at = self.key_at(px, py);
            let hot = key_at
                .map(|k| format!("k{}", k))
                .unwrap_or_else(|| "none".to_string());
            if let Some(surf) = self.frames[i].surface.as_mut() {
                if let Ok(pixels) = surf.data() {
                    eprintln!(
                        "[dbg-ppm] writing ppm px={} py={} inside={} hot={:?}",
                        px, py, inside, key_at
                    );
                    let mut out = Vec::new();
                    out.extend_from_slice(format!("P6\n{} {}\n255\n", buf_w, buf_h).as_bytes());
                    for p in pixels.chunks(4) {
                        out.push(p[2]);
                        out.push(p[1]);
                        out.push(p[0]);
                    }
                    let _ = std::fs::write(format!("/tmp/osk_render_{}.ppm", hot), out);
                }
            }
        }
        let Some(buf) = self.frames[i].buf.as_ref() else {
            self.dirty = true;
            return;
        };
        self.frames[i].attached = true;
        let Some(surf) = self.surface.clone() else {
            return;
        };
        surf.attach(Some(buf), 0, 0);
        // damage_buffer is buffer pixels; wl_surface.damage would
        // mis-scale under a viewport.
        surf.damage_buffer(0, 0, buf_w, buf_h);
        self.sync_regions();
        self.sync_viewport();
        self.sync_buffer_scale();
        // Arm before the commit so this present is what fires the
        // callback. A key tap may commit while one is already in flight;
        // a second callback would clear frame_pending early and break
        // coalescing.
        if !self.frame_pending {
            self.arm_frame();
        }
        surf.commit();
        self.commit_x = self.pos_x;
        self.commit_y = self.pos_y;
        self.last_commit_ms = now_ms();
        self.attached_w = self.width;
        self.attached_h = self.height;
        self.dirty = false;
        self.sugg = bar.sugg;
        self.n_sugg = bar.n_sugg;
        self.layout_rect = bar.layout_rect;
        self.theme_rect = bar.theme_rect;
        self.close_rect = bar.close_rect;
        self.theme_menu_rect = bar.theme_menu_rect;
        self.theme_item_rects = bar.theme_item_rects;
        self.dict_rect = bar.dict_rect;
        self.dict_menu_rect = bar.dict_menu_rect;
        self.dict_tab_toggle_rect = bar.dict_tab_toggle_rect;
        self.dict_clear_rect = bar.dict_clear_rect;
        self.dict_prev_rect = bar.dict_prev_rect;
        self.dict_next_rect = bar.dict_next_rect;
        self.dict_remove_rects = bar.dict_remove_rects;
        self.dict_words = bar.dict_words;
        self.n_dict_items = bar.n_dict_items;
        self.last_hot = self.hot_sig();
    }

    fn frame_free(&mut self, i: usize) {
        let frame = &mut self.frames[i];
        if let Some(buf) = frame.buf.take() {
            buf.destroy();
        }
        if let Some(pool) = frame.pool.take() {
            pool.destroy();
        }
        frame.surface = None;
        frame.file = None;
        frame.attached = false;
        frame.buf_w = 0;
        frame.buf_h = 0;
        frame.stride = 0;
    }

    fn frame_alloc(&mut self, i: usize, buf_w: i32, buf_h: i32) -> bool {
        if self.frames[i].attached {
            return false;
        }
        if self.frames[i].surface.is_some()
            && self.frames[i].buf_w == buf_w
            && self.frames[i].buf_h == buf_h
        {
            return true;
        }
        self.frame_free(i);
        let stride = ((buf_w * 4) + 63) & !63;
        let need = stride * buf_h;
        if need <= 0 {
            return false;
        }
        // Extra rows so a resize drag grows the pool once, not every
        // motion event.
        let cap = need + stride * 160;
        let memfd = match memfd::MemfdOptions::default().create("osk-frame") {
            Ok(m) => m,
            Err(_) => return false,
        };
        let file = memfd.into_file();
        if file.set_len(cap as u64).is_err() {
            return false;
        }
        let pixels = vec![0u8; need as usize];
        let surface =
            match ImageSurface::create_for_data(pixels, Format::ARgb32, buf_w, buf_h, stride) {
                Ok(s) => s,
                Err(_) => return false,
            };
        let Some(shm) = self.shm.as_ref() else {
            return false;
        };
        let fd = file.as_fd();
        let pool = shm.create_pool(fd, cap, &self.qh, ());
        let buf = pool.create_buffer(
            0,
            buf_w,
            buf_h,
            stride,
            wl_shm::Format::Argb8888,
            &self.qh,
            i,
        );
        self.frames[i].surface = Some(surface);
        self.frames[i].file = Some(file);
        self.frames[i].pool = Some(pool);
        self.frames[i].buf = Some(buf);
        self.frames[i].buf_w = buf_w;
        self.frames[i].buf_h = buf_h;
        self.frames[i].stride = stride;
        true
    }

    // ---- injection ----

    /// Press a key with any active modifiers held, then release them.
    /// Latched modifiers are consumed by this one key; locked ones stay.
    fn tap_key(&mut self, idx: usize) {
        let k = *self.key_of(idx);
        if k.code == 0 {
            return;
        }
        let is_mod = k.moment || k.toggle;
        let shift_active = self.mods.shift_active();
        // When enabled, Tab accepts the top suggestion (like clicking
        // the first pill); Shift+Tab passes through as a plain TAB.
        // By default Tab is a normal Tab key.
        if self.tab_accepts_prediction
            && k.code == osk_core::key::key::KEY_TAB
            && !shift_active
            && !self.predictor.suggestions().is_empty()
        {
            self.click_suggestion(0);
            self.render();
            return;
        }
        let caps = self.mods.caps_lock();
        let sticky = self.mods.sticky();
        let mut ev = [OskEvent {
            code: 0,
            press: false,
        }; TAP_MAX_EVENTS];
        let Ok(n) = tap_events(&k, shift_active, caps, &sticky, &mut ev) else {
            return;
        };
        use osk_core::key::key;
        for e in &ev[..n] {
            if e.press {
                self.backend.press(e.code);
            } else {
                self.backend.release(e.code);
            }
        }
        self.mods.consume_latched();
        match k.code {
            key::KEY_BACKSPACE => self.predictor.backspace(),
            key::KEY_SPACE => self.predictor.space(),
            _ => {
                // A single-character label (letters, symbols, arrows)
                // feeds the predictor; multi-char labels (pgdn, esc)
                // and modifiers break the word.
                let single = k.label.and_then(|l| {
                    let mut it = l.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) => Some(c),
                        _ => None,
                    }
                });
                if k.letter {
                    if let Some(c) = k.label.and_then(|l| l.chars().next()) {
                        self.predictor.char(c);
                    }
                } else if let Some(c) = single {
                    self.predictor.char(c);
                } else if !is_mod {
                    self.predictor.break_word();
                }
            }
        }
        // A tap commits immediately, even with a frame callback in
        // flight.
        self.render();
    }

    fn click_suggestion(&mut self, idx: usize) {
        let suggs = self.predictor.suggestions();
        let Some(word) = suggs.get(idx).copied() else {
            return;
        };
        let Some(map) = &self.charmap else {
            return;
        };
        let partial = self.predictor.partial().to_string();
        let shift_active = self.mods.shift_active();
        let caps = self.mods.caps_lock();
        let mut out = [0 as char; 64];
        let Some(n) = complete_word(word, &partial, shift_active, caps, &mut out) else {
            return;
        };
        let text: String = out[..n].iter().collect();
        let mut sink = BackendSink {
            b: &mut *self.backend,
        };
        let _ = type_string(&mut sink, map, &text, || {});
        self.mods.consume_latched();
        self.predictor.commit(word);
        self.request_redraw();
    }

    // ---- pointer ----

    fn on_motion(&mut self) {
        if self.rep_key >= 0 && self.key_at(self.px, self.py) != Some(self.rep_key as usize) {
            self.rep_key = -1;
        }
        if self.moving {
            // Relative motion (below) is the pointer's own delta and does
            // not change when the surface moves. This surface-local sample
            // is only the fallback for a compositor with no relative
            // pointer.
            if self.rel.is_none() {
                let nx = (self.shown_x + (self.px - self.grab_lx)).max(0);
                let ny = (self.shown_y + (self.py - self.grab_ly)).max(0);
                if nx != self.pos_x || ny != self.pos_y {
                    self.pos_x = nx;
                    self.pos_y = ny;
                    self.apply_geometry();
                }
            }
            return;
        }
        if self.resizing {
            let w = (self.rsz_w + (self.px - self.grab_lx)).max(MIN_W);
            let h = (self.rsz_h + (self.py - self.grab_ly)).max(MIN_H);
            if w != self.width || h != self.height {
                self.width = w;
                self.height = h;
                self.apply_geometry();
                self.request_redraw();
            }
            return;
        }
        // Moving inside the same key (or the bar gap) does not change a
        // pixel; skip the redraw.
        let sig = self.hot_sig();
        if sig != self.last_hot {
            self.last_hot = sig;
            self.request_redraw();
        }
    }

    /// Signature of the hovered elements, for redraw coalescing.
    fn hot_sig(&self) -> u32 {
        let key = self
            .key_at(self.px, self.py)
            .map(|k| k as u32)
            .unwrap_or(0x3ff);
        let sugg = (0..self.n_sugg)
            .find(|&i| self.sugg[i].contains(self.px as f64, self.py as f64))
            .map(|i| i as u32)
            .unwrap_or(0x1f);
        let close = self.close_rect.contains(self.px as f64, self.py as f64) as u32;
        let layout_hov = self.layout_rect.contains(self.px as f64, self.py as f64) as u32;
        let theme_hov = self.theme_rect.contains(self.px as f64, self.py as f64) as u32;
        let menu_item = (0..12)
            .find(|&i| self.theme_item_rects[i].contains(self.px as f64, self.py as f64))
            .map(|i| i as u32)
            .unwrap_or(0xf);
        let menu_open = self.theme_menu_open as u32;
        let dict_hov = self.dict_rect.contains(self.px as f64, self.py as f64) as u32;
        let dict_menu_open = self.dict_menu_open as u32;
        let dict_item = if self.dict_menu_open {
            let mut h = 0u32;
            if self.dict_tab_toggle_rect.contains(self.px as f64, self.py as f64) {
                h = 1;
            } else if self.dict_clear_rect.contains(self.px as f64, self.py as f64) {
                h = 2;
            } else if self.dict_prev_rect.contains(self.px as f64, self.py as f64) {
                h = 3;
            } else if self.dict_next_rect.contains(self.px as f64, self.py as f64) {
                h = 4;
            } else if let Some(i) = (0..self.n_dict_items)
                .find(|&i| self.dict_remove_rects[i].contains(self.px as f64, self.py as f64))
            {
                h = 5 + i as u32;
            }
            h
        } else {
            0
        };
        let handle = self.handle_hov() as u32;
        (key << 20)
            | (sugg << 15)
            | (layout_hov << 10)
            | (theme_hov << 9)
            | (menu_open << 8)
            | (menu_item << 4)
            | (close << 1)
            | (dict_menu_open << 12)
            | (dict_hov << 11)
            | (dict_item << 28)
            | handle
    }

    fn cycle_layout(&mut self) {
        self.pressed = -1;
        self.rep_key = -1;
        self.layout_idx = (self.layout_idx + 1) % ALL_LAYOUTS.len();
        self.compute_grid_cols();
        self.width = self.width.max(MIN_W);
        self.height = self.height.max(MIN_H);
        self.apply_geometry();
        self.save_config();
        self.request_redraw();
    }

    fn on_button(&mut self, pressed: bool) {
        if pressed {
            if self.dict_menu_open {
                if self.dict_tab_toggle_rect.contains(self.px as f64, self.py as f64) {
                    self.tab_accepts_prediction = !self.tab_accepts_prediction;
                    self.save_config();
                    self.request_redraw();
                    return;
                }
                if self.dict_clear_rect.contains(self.px as f64, self.py as f64) {
                    self.predictor.clear_learned();
                    self.dict_page = 0;
                    self.save_config();
                    self.request_redraw();
                    return;
                }
                if self.dict_prev_rect.contains(self.px as f64, self.py as f64) {
                    if self.dict_page > 0 {
                        self.dict_page -= 1;
                    }
                    self.request_redraw();
                    return;
                }
                if self.dict_next_rect.contains(self.px as f64, self.py as f64) {
                    self.dict_page += 1;
                    self.request_redraw();
                    return;
                }
                for i in 0..self.n_dict_items {
                    if self.dict_remove_rects[i].contains(self.px as f64, self.py as f64) {
                        if let Some(w) = self.dict_words[i] {
                            self.predictor.forget_word(w);
                            self.save_config();
                            self.request_redraw();
                            return;
                        }
                    }
                }
                if self.dict_rect.contains(self.px as f64, self.py as f64) {
                    self.dict_menu_open = false;
                    self.request_redraw();
                    return;
                }
                if !self.dict_menu_rect.contains(self.px as f64, self.py as f64) {
                    self.dict_menu_open = false;
                    self.request_redraw();
                    return;
                }
                // Inside the menu without hitting a button: no action.
                return;
            }
            if self.theme_menu_open {
                // Check if user clicked an item in the menu
                for i in 0..12 {
                    if self.theme_item_rects[i].contains(self.px as f64, self.py as f64) {
                        self.theme_idx = i;
                        self.theme = THEMES[i];
                        self.theme_menu_open = false;
                        self.dict_menu_open = false;
                        self.save_config();
                        self.request_redraw();
                        return;
                    }
                }
                // Check if user clicked the theme button again
                if self.theme_rect.contains(self.px as f64, self.py as f64) {
                    self.theme_menu_open = false;
                    self.dict_menu_open = false;
                    self.request_redraw();
                    return;
                }
                // Clicked outside: close the menu
                self.theme_menu_open = false;
                self.dict_menu_open = false;
                self.request_redraw();
                return;
            }

            // close
            if self.close_rect.contains(self.px as f64, self.py as f64) {
                self.save_config();
                self.quit = true;
                return;
            }
            // dict chip
            if self.dict_rect.contains(self.px as f64, self.py as f64) {
                self.dict_menu_open = true;
                self.theme_menu_open = false;
                self.request_redraw();
                return;
            }
            // theme icon button
            if self.theme_rect.contains(self.px as f64, self.py as f64) {
                self.theme_menu_open = true;
                self.dict_menu_open = false;
                self.request_redraw();
                return;
            }
            // layout chip
            if self.layout_rect.contains(self.px as f64, self.py as f64) {
                self.cycle_layout();
                return;
            }
            // suggestion chips
            for i in 0..self.n_sugg {
                if self.sugg[i].contains(self.px as f64, self.py as f64) {
                    self.click_suggestion(self.sugg[i].idx);
                    return;
                }
            }
            // resize handle (bottom-right corner)
            let hs = self.handle_size();
            if self.px >= self.width - hs && self.py >= self.height - hs {
                self.pressed = -1;
                self.rep_key = -1;
                self.resizing = true;
                self.grab_lx = self.px;
                self.grab_ly = self.py;
                self.rsz_w = self.width;
                self.rsz_h = self.height;
                return;
            }
            // bar -> drag the surface
            if self.py < self.bar_h() as i32 {
                self.pressed = -1;
                self.rep_key = -1;
                self.moving = true;
                self.grab_lx = self.px;
                self.grab_ly = self.py;
                // Drop leftover subpixel motion from a previous drag.
                self.drag_fx = 0.0;
                self.drag_fy = 0.0;
                return;
            }
            let Some(k) = self.key_at(self.px, self.py) else {
                return;
            };
            let key = *self.key_of(k);
            // modifier key (shift/ctrl/alt/meta): run the 3-state machine
            // (OFF -> LATCHED -> LOCKED -> OFF; double-click locks)
            if mod_index(key.code).is_some() {
                self.mods.click(key.code, now_ms());
                self.flash_key = k as i32;
                self.flash_until = now_ms() + FLASH_MS;
                self.request_redraw();
                return;
            }
            if key.toggle {
                // caps lock
                self.mods.toggle_caps();
                self.flash_key = k as i32;
                self.flash_until = now_ms() + FLASH_MS;
                self.request_redraw();
                return;
            }
            self.pressed = k as i32;
            self.press_time = now_ms();
            self.hold_fired = false;
            self.tap_key(k);
            if key.repeat {
                self.rep_key = k as i32;
                self.rep_next = now_ms() + 500;
            }
        } else {
            self.rep_key = -1;
            self.hold_fired = false;
            if self.pressed >= 0 {
                self.flash_key = self.pressed;
                self.flash_until = now_ms() + FLASH_MS;
                self.pressed = -1;
                self.request_redraw();
            }
            // End the drag; apply_geometry() guarantees the final
            // position is committed if the drag ended mid-frame.
            if self.moving || self.resizing {
                self.moving = false;
                self.resizing = false;
                self.apply_geometry();
                self.save_config();
            }
        }
    }

    /// Right-click a suggestion pill to forget that word.
    fn on_right_click(&mut self) {
        for i in 0..self.n_sugg {
            if self.sugg[i].contains(self.px as f64, self.py as f64) {
                let suggs = self.predictor.suggestions();
                if let Some(&word) = suggs.get(self.sugg[i].idx) {
                    self.predictor.forget_word(word);
                    self.save_config();
                    self.request_redraw();
                    return;
                }
            }
        }
    }

    /// Relative motion is device motion, not the change in surface-local
    /// position. A drag that adds surface-local samples keeps counting
    /// the same offset until the compositor applies the move, then the
    /// next sample jumps backward.
    fn on_relative_motion(&mut self, dx: f64, dy: f64) {
        if !self.moving {
            return;
        }
        self.drag_fx += dx;
        self.drag_fy += dy;
        let ix = self.drag_fx as i32;
        let iy = self.drag_fy as i32;
        if ix == 0 && iy == 0 {
            return;
        }
        self.drag_fx -= ix as f64;
        self.drag_fy -= iy as f64;
        // Don't store the motion that the screen edge ate, or the window
        // would have to be dragged back by that much before it moved
        // again.
        if self.pos_x + ix < 0 {
            self.drag_fx = 0.0;
        }
        if self.pos_y + iy < 0 {
            self.drag_fy = 0.0;
        }
        self.drag_by(ix, iy);
    }

    fn drag_by(&mut self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        let nx = (self.pos_x + dx).max(0);
        let ny = (self.pos_y + dy).max(0);
        if nx == self.pos_x && ny == self.pos_y {
            return;
        }
        self.pos_x = nx;
        self.pos_y = ny;
        self.apply_geometry();
    }

    // ---- repeat ----

    fn handle_repeat(&mut self) {
        if self.rep_key < 0 {
            return;
        }
        let now = now_ms();
        if now >= self.rep_next {
            self.tap_key(self.rep_key as usize);
            self.rep_next = now + 40;
        }
    }

    fn handle_flash(&mut self) {
        if self.flash_key >= 0 && now_ms() >= self.flash_until {
            self.flash_key = -1;
            self.request_redraw();
        }
    }

    fn handle_hold(&mut self) {
        if self.pressed >= 0 && !self.hold_fired && now_ms() >= self.press_time + 200 {
            self.hold_fired = true;
            self.request_redraw();
        }
    }

    /// Poll timeout: wait for events, or until the next repeat fires,
    /// or until a held-back prefix expires.
    fn rep_timeout(&self) -> Option<Duration> {
        let mut dt: Option<u64> = None;
        if self.rep_key >= 0 {
            dt = Some(self.rep_next.saturating_sub(now_ms()));
        }
        if self.pressed >= 0 && !self.hold_fired {
            let hold_fire = (self.press_time + 200).saturating_sub(now_ms());
            dt = Some(dt.map_or(hold_fire, |d| d.min(hold_fire)));
        }
        if self.flash_key >= 0 {
            let f = self.flash_until.saturating_sub(now_ms());
            dt = Some(dt.map_or(f, |d| d.min(f)));
        }
        let dt = dt?;
        Some(Duration::from_millis(dt.max(1)))
    }

    /// Frame-callback watchdog: if the compositor never fires the
    /// callback for a bufferless geometry commit (some don't), don't
    /// wait for it forever - force the pending commit on.
    fn watchdog(&mut self) {
        if !self.frame_pending || !self.dirty || now_ms() - self.last_commit_ms <= 100 {
            return;
        }
        self.frame_pending = false;
        if self.moving && self.width == self.attached_w && self.height == self.attached_h {
            // Pure move: commit the new margin with the existing buffer.
            self.dirty = false;
            self.arm_frame();
            if let Some(surf) = self.surface.as_ref() {
                surf.commit();
            }
            self.commit_x = self.pos_x;
            self.commit_y = self.pos_y;
            self.last_commit_ms = now_ms();
        } else {
            self.dirty = false;
            self.render();
        }
    }

    // ---- layout switch ----

    // ---- painting ----

    /// Measure the bar elements (suggestions, layout, theme icon, close)
    /// and store their rects for hit testing.
    fn measure_bar(&self, cr: &Context) -> BarLayout {
        let mut bar = BarLayout {
            sugg: [Rect::default(); 5],
            n_sugg: 0,
            layout_rect: Rect::default(),
            theme_rect: Rect::default(),
            close_rect: Rect::default(),
            theme_menu_rect: Rect::default(),
            theme_item_rects: [Rect::default(); 12],
            dict_rect: Rect::default(),
            dict_menu_rect: Rect::default(),
            dict_tab_toggle_rect: Rect::default(),
            dict_clear_rect: Rect::default(),
            dict_prev_rect: Rect::default(),
            dict_next_rect: Rect::default(),
            dict_remove_rects: [Rect::default(); 8],
            dict_words: [None; 8],
            n_dict_items: 0,
        };
        let bh = self.bar_h();
        let k = self.k();
        let fs = 0.34 * k;
        let chip_fs = 0.30 * k;
        let side_pad = 0.35 * k;

        // 1. Close button on the far right
        let cw = 0.6 * k;
        let cx = self.width as f64 - cw - MARGIN_U * k;
        bar.close_rect = Rect {
            x: cx,
            y: 0.0,
            w: cw,
            h: bh,
            idx: 0,
        };

        let chip_pad = 0.20 * k;
        let chip_gap = 0.08 * k;

        // 2. Theme icon button
        let th_w = 0.75 * k;
        let th_x = cx - th_w - chip_gap;
        bar.theme_rect = Rect {
            x: th_x,
            y: 0.0,
            w: th_w,
            h: bh,
            idx: 0,
        };

        // 3. Dict chip
        let dict_label = "Dict";
        let dict_tw = self.text_w(cr, dict_label, chip_fs);
        let dict_w = dict_tw + 2.0 * chip_pad;
        let dict_x = th_x - dict_w - chip_gap;
        bar.dict_rect = Rect {
            x: dict_x,
            y: 0.0,
            w: dict_w,
            h: bh,
            idx: 0,
        };

        // 4. Layout chip (e.g. "65%", "65%+Num")
        let lo_label = self.layout().name;
        let lo_tw = self.text_w(cr, lo_label, chip_fs);
        let lo_w = lo_tw + 2.0 * chip_pad;
        let lo_x = dict_x - lo_w - chip_gap;
        bar.layout_rect = Rect {
            x: lo_x,
            y: 0.0,
            w: lo_w,
            h: bh,
            idx: 0,
        };

        // 4. Centered word prediction
        let suggs = self.predictor.suggestions();
        let avail_left = MARGIN_U * k + 0.1 * k;
        let avail_right = (lo_x - 0.2 * k).max(avail_left);
        if !suggs.is_empty() && avail_right > avail_left + 60.0 {
            let max_avail_w = avail_right - avail_left;
            let pill_gap = 0.12 * k;

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
                        (tw + 2.8 * side_pad).max(1.8 * k)
                    } else {
                        (tw + 1.8 * side_pad).max(1.3 * k)
                    };
                    widths.push(pw);
                    total_w += pw;
                }
                total_w += (widths.len() as f64 - 1.0).max(0.0) * pill_gap;

                if total_w <= max_avail_w || candidates.len() == 1 {
                    let center_x = self.width as f64 / 2.0;
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

        // 6. Theme dropdown menu layout (2 columns x 6 rows)
        if self.theme_menu_open {
            let col_w = 110.0;
            let row_h = 25.0;
            let col_gap = 6.0;
            let row_gap = 4.0;
            let pad = 8.0;

            let menu_w = 2.0 * col_w + col_gap + 2.0 * pad;
            let menu_h = 6.0 * row_h + 5.0 * row_gap + 2.0 * pad;

            let menu_right = (th_x + th_w).min(self.width as f64 - MARGIN_U * k);
            let menu_x = (menu_right - menu_w).max(MARGIN_U * k);
            let menu_y = bh + 2.0;

            bar.theme_menu_rect = Rect {
                x: menu_x,
                y: menu_y,
                w: menu_w,
                h: menu_h,
                idx: 0,
            };

            for idx in 0..12 {
                let col = idx / 6;
                let row = idx % 6;
                let ix = menu_x + pad + col as f64 * (col_w + col_gap);
                let iy = menu_y + pad + row as f64 * (row_h + row_gap);
                bar.theme_item_rects[idx] = Rect {
                    x: ix,
                    y: iy,
                    w: col_w,
                    h: row_h,
                    idx,
                };
            }
        }

        // 7. Dict menu layout
        if self.dict_menu_open {
            let menu_w = 260.0;
            let pad = 8.0;
            let header_h = 26.0;
            let toggle_h = 24.0;
            let item_h = 22.0;
            let footer_h = 24.0;

            let words = self.predictor.learned_words();
            let total_pages = if words.is_empty() {
                1
            } else {
                (words.len() + 7) / 8
            };
            let page = self.dict_page.min(total_pages.saturating_sub(1));
            let n_items = words.len().saturating_sub(page * 8).min(8);
            let rows = n_items.max(if words.is_empty() { 1 } else { 0 });

            let menu_right = (dict_x + dict_w).min(self.width as f64 - MARGIN_U * k);
            let menu_x = (menu_right - menu_w).max(MARGIN_U * k);
            let menu_y = bh + 2.0;
            let menu_h = header_h + toggle_h + rows as f64 * item_h + footer_h;

            bar.dict_menu_rect = Rect {
                x: menu_x,
                y: menu_y,
                w: menu_w,
                h: menu_h,
                idx: 0,
            };

            bar.dict_tab_toggle_rect = Rect {
                x: menu_x + pad,
                y: menu_y + header_h,
                w: menu_w - 2.0 * pad,
                h: toggle_h,
                idx: 0,
            };

            for i in 0..8 {
                if i < n_items {
                    bar.dict_words[i] = Some(words[page * 8 + i].word);
                    bar.dict_remove_rects[i] = Rect {
                        x: menu_x + menu_w - pad - 24.0,
                        y: menu_y + header_h + toggle_h + i as f64 * item_h,
                        w: 24.0,
                        h: item_h,
                        idx: i,
                    };
                } else {
                    bar.dict_words[i] = None;
                }
            }
            bar.n_dict_items = n_items;

            let footer_y = menu_y + header_h + toggle_h + rows as f64 * item_h;
            bar.dict_clear_rect = Rect {
                x: menu_x + pad,
                y: footer_y,
                w: 90.0,
                h: footer_h,
                idx: 0,
            };
            if total_pages > 1 {
                bar.dict_next_rect = Rect {
                    x: menu_x + menu_w - pad - 28.0,
                    y: footer_y,
                    w: 28.0,
                    h: footer_h,
                    idx: 0,
                };
                bar.dict_prev_rect = Rect {
                    x: bar.dict_next_rect.x - 34.0,
                    y: footer_y,
                    w: 28.0,
                    h: footer_h,
                    idx: 0,
                };
            }
        }

        bar
    }

    fn paint(&self, cr: &Context, bar: &BarLayout) {
        cr.identity_matrix();
        cr.reset_clip();
        let frame = &self.frames[self.cur_frame];
        if self.width > 0 && self.height > 0 && frame.buf_w > 0 && frame.buf_h > 0 {
            cr.scale(
                frame.buf_w as f64 / self.width as f64,
                frame.buf_h as f64 / self.height as f64,
            );
        }
        cr.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
        self.paint_window(cr);
        self.paint_bar(cr, bar);
        let layout = self.layout();
        for (r, row) in layout.rows.iter().enumerate() {
            for (i, key) in row.keys.iter().enumerate() {
                let (rx, ry, rw, rh) = self.key_rect(r, i);
                self.paint_key(cr, rx, ry, rw, rh, key, r * 100 + i);
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
            (CASE_RAD_U * self.k()).max(1.0),
        );
        set_src(cr, self.theme.edge);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        if self.theme_menu_open {
            self.paint_theme_menu(cr, bar);
        }
        if self.dict_menu_open {
            self.paint_dict_menu(cr, bar);
        }
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
            CASE_RAD_U * self.k(),
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

    fn paint_chip(&self, cr: &Context, r: &Rect, label: &str, font_size: f64, bh: f64) {
        if r.w <= 0.0 {
            return;
        }
        let hov = self.inside && r.contains(self.px as f64, self.py as f64);
        let pad_y = bh * 0.16;
        let chip_h = bh - 2.0 * pad_y;
        let bg = if hov { self.theme.key_hov } else { self.theme.key_mod };
        let fg = if hov { self.theme.keytxt } else { self.theme.modtxt };
        rrect(cr, r.x, pad_y, r.w, chip_h, chip_h * 0.35);
        set_src(cr, bg);
        let _ = cr.fill();
        self.draw_centered(cr, label, r.x + r.w / 2.0, bh / 2.0, font_size, fg);
    }

    fn paint_bar(&self, cr: &Context, bar: &BarLayout) {
        let bh = self.bar_h();
        let k = self.k();
        let fs = 0.34 * k;
        let _ = cr.save();
        cr.rectangle(0.0, 0.0, self.width as f64, bh);
        cr.clip();
        rrect(cr, 0.0, 0.0, self.width as f64, self.height as f64, CASE_RAD_U * k);
        cr.clip();
        cr.set_operator(Operator::Source);
        set_src_rgba(cr, self.theme.case, self.theme.opacity);
        cr.rectangle(0.0, 0.0, self.width as f64, bh);
        let _ = cr.fill();
        cr.set_operator(Operator::Over);
        let suggs = self.predictor.suggestions();
        let pad_y = 0.12 * bh;
        let ry = pad_y;
        let rh = bh - 2.0 * pad_y;
        let rad = (KEY_RAD_U * k * 0.70).min(rh / 2.0);

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

            let (bg, fg, border) = if is_primary {
                if hov {
                    (self.theme.key_press, self.theme.ink, None)
                } else {
                    (self.theme.on_bg, self.theme.on_txt, Some(self.theme.accent))
                }
            } else {
                if hov {
                    (self.theme.key_hov, self.theme.accent, Some(self.theme.accent))
                } else {
                    (self.theme.key, self.theme.keytxt, Some(self.theme.edge))
                }
            };

            cr.new_path();
            rrect(cr, rx, ry, rw, rh, rad);
            set_src_rgba(cr, bg, self.theme.opacity);
            let _ = cr.fill();

            if let Some(bcolor) = border {
                cr.new_path();
                rrect(cr, rx + 0.5, ry + 0.5, rw - 1.0, rh - 1.0, rad);
                set_src_rgba(cr, bcolor, (self.theme.opacity * 0.7).min(1.0));
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            }

            let weight = if is_primary { FontWeight::Bold } else { FontWeight::Normal };
            cr.select_font_face(FONT, FontSlant::Normal, weight);
            self.draw_centered(cr, word, rx + rw / 2.0, bh / 2.0, fs, fg);
        }

        // Layout chip
        self.paint_chip(
            cr,
            &bar.layout_rect,
            self.layout().name,
            0.30 * k,
            bh,
        );

        // Dict chip
        self.paint_chip(cr, &bar.dict_rect, "Dict", 0.30 * k, bh);
        if self.dict_menu_open {
            let pad_y = bh * 0.16;
            let chip_h = bh - 2.0 * pad_y;
            rrect(
                cr,
                bar.dict_rect.x + 0.5,
                pad_y + 0.5,
                bar.dict_rect.w - 1.0,
                chip_h - 1.0,
                chip_h * 0.35,
            );
            set_src(cr, self.theme.accent);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        }

        // Theme icon button
        let th_hov = self.inside && bar.theme_rect.contains(self.px as f64, self.py as f64);
        let th_active = self.theme_menu_open;
        let (th_bg, th_fg) = if th_active || th_hov {
            (self.theme.on_bg, self.theme.accent)
        } else {
            (self.theme.key_mod, self.theme.modtxt)
        };
        let pad_y = bh * 0.16;
        let chip_h = bh - 2.0 * pad_y;
        rrect(cr, bar.theme_rect.x, pad_y, bar.theme_rect.w, chip_h, chip_h * 0.35);
        set_src(cr, th_bg);
        let _ = cr.fill();
        if th_active {
            rrect(cr, bar.theme_rect.x + 0.5, pad_y + 0.5, bar.theme_rect.w - 1.0, chip_h - 1.0, chip_h * 0.35);
            set_src(cr, self.theme.accent);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        }
        let icon_cx = bar.theme_rect.x + bar.theme_rect.w / 2.0;
        let icon_cy = bh / 2.0;
        self.draw_palette_icon(cr, icon_cx, icon_cy, 0.17 * k, th_fg);

        let close_hov = self.inside && bar.close_rect.contains(self.px as f64, self.py as f64);
        self.draw_centered(
            cr,
            "\u{00d7}",
            bar.close_rect.x + bar.close_rect.w / 2.0,
            bh / 2.0,
            0.4 * k,
            if close_hov { 0xffffff } else { self.theme.close },
        );
        let _ = cr.restore();
    }

    fn draw_palette_icon(&self, cr: &Context, cx: f64, cy: f64, r: f64, color: u32) {
        set_src(cr, color);
        cr.new_path();
        cr.arc(cx, cy, r, 0.0, 2.0 * std::f64::consts::PI);
        cr.set_line_width(1.4);
        let _ = cr.stroke();

        let dot_r = 0.22 * r;
        let dist = 0.50 * r;
        for &deg in &[35.0, 120.0, 215.0] {
            let rad = deg * std::f64::consts::PI / 180.0;
            let dx = cx + dist * rad.cos();
            let dy = cy + dist * rad.sin();
            cr.new_path();
            cr.arc(dx, dy, dot_r, 0.0, 2.0 * std::f64::consts::PI);
            let _ = cr.fill();
        }
    }

    fn paint_theme_menu(&self, cr: &Context, bar: &BarLayout) {
        if !self.theme_menu_open || bar.theme_menu_rect.w <= 0.0 {
            return;
        }
        let mr = &bar.theme_menu_rect;
        let k = self.k();

        let _ = cr.save();
        // Drop shadow behind menu
        cr.new_path();
        rrect(cr, mr.x + 2.0, mr.y + 3.0, mr.w, mr.h, 8.0);
        set_src_rgba(cr, 0x000000, 0.45);
        let _ = cr.fill();

        // Menu background panel (using current theme's dark case)
        cr.new_path();
        rrect(cr, mr.x, mr.y, mr.w, mr.h, 8.0);
        set_src(cr, self.theme.case);
        let _ = cr.fill();

        // Menu border outline in accent
        cr.new_path();
        rrect(cr, mr.x + 0.5, mr.y + 0.5, mr.w - 1.0, mr.h - 1.0, 8.0);
        set_src_rgba(cr, self.theme.accent, 0.50);
        cr.set_line_width(1.0);
        let _ = cr.stroke();

        // Draw 12 theme cards
        let fs = 0.24 * k;
        for i in 0..12 {
            let item = &bar.theme_item_rects[i];
            let t = &THEMES[i];
            let hov = self.inside && item.contains(self.px as f64, self.py as f64);
            let is_cur = i == self.theme_idx;

            let rx = item.x;
            let ry = item.y;
            let rw = item.w;
            let rh = item.h;

            // Card background in that theme's own case color!
            cr.new_path();
            rrect(cr, rx, ry, rw, rh, 5.0);
            set_src(cr, t.case);
            let _ = cr.fill();

            // Border styling
            cr.new_path();
            rrect(cr, rx + 0.5, ry + 0.5, rw - 1.0, rh - 1.0, 5.0);
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
            let dot_x = rx + 10.0;
            let dot_y = ry + rh / 2.0;
            let dot_r = 4.0;
            cr.new_path();
            cr.arc(dot_x, dot_y, dot_r, 0.0, 2.0 * std::f64::consts::PI);
            set_src(cr, t.key_press);
            let _ = cr.fill();

            // Theme name rendered in that theme's accent/keytxt
            let weight = if is_cur || hov { FontWeight::Bold } else { FontWeight::Normal };
            cr.select_font_face(FONT, FontSlant::Normal, weight);
            cr.set_font_size(fs);
            let text_color = if is_cur { t.accent } else if hov { t.key_press } else { t.keytxt };
            self.draw_left(cr, t.name, rx + 20.0, ry + rh / 2.0, fs, text_color);

            // Active checkmark indicator on the right if current theme
            if is_cur {
                self.draw_centered(cr, "\u{2713}", rx + rw - 10.0, ry + rh / 2.0, fs * 0.9, t.accent);
            }
        }

        let _ = cr.restore();
    }

    fn paint_dict_menu(&self, cr: &Context, bar: &BarLayout) {
        if !self.dict_menu_open || bar.dict_menu_rect.w <= 0.0 {
            return;
        }
        let mr = &bar.dict_menu_rect;
        let k = self.k();
        let fs = 0.24 * k;
        let pad = 8.0;
        let header_h = 26.0;
        let toggle_h = 24.0;
        let item_h = 22.0;
        let footer_h = 24.0;

        let words = self.predictor.learned_words();
        let total = words.len();
        let total_pages = if total == 0 {
            1
        } else {
            (total + 7) / 8
        };
        let page = self.dict_page.min(total_pages.saturating_sub(1));

        let _ = cr.save();
        // Drop shadow behind the menu
        cr.new_path();
        rrect(cr, mr.x + 2.0, mr.y + 3.0, mr.w, mr.h, 8.0);
        set_src_rgba(cr, 0x000000, 0.45);
        let _ = cr.fill();

        // Panel background with accent outline
        cr.new_path();
        rrect(cr, mr.x, mr.y, mr.w, mr.h, 8.0);
        set_src(cr, self.theme.case);
        let _ = cr.fill();
        cr.new_path();
        rrect(cr, mr.x + 0.5, mr.y + 0.5, mr.w - 1.0, mr.h - 1.0, 8.0);
        set_src_rgba(cr, self.theme.accent, 0.50);
        cr.set_line_width(1.0);
        let _ = cr.stroke();

        // Header
        cr.select_font_face(FONT, FontSlant::Normal, FontWeight::Bold);
        self.draw_left(
            cr,
            &format!("Learned Words ({total})"),
            mr.x + pad,
            mr.y + header_h / 2.0,
            fs,
            self.theme.on_txt,
        );

        // Tab behavior toggle
        let tr = &bar.dict_tab_toggle_rect;
        let tr_hov = self.inside && tr.contains(self.px as f64, self.py as f64);
        let active = self.tab_accepts_prediction;
        let (bg, fg) = if active {
            (self.theme.on_bg, self.theme.accent)
        } else if tr_hov {
            (self.theme.key_hov, self.theme.keytxt)
        } else {
            (self.theme.key_mod, self.theme.modtxt)
        };
        cr.new_path();
        rrect(cr, tr.x, tr.y, tr.w, tr.h, 5.0);
        set_src(cr, bg);
        let _ = cr.fill();
        if active {
            cr.new_path();
            rrect(cr, tr.x + 0.5, tr.y + 0.5, tr.w - 1.0, tr.h - 1.0, 5.0);
            set_src(cr, self.theme.accent);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        }
        cr.select_font_face(FONT, FontSlant::Normal, if active { FontWeight::Bold } else { FontWeight::Normal });
        self.draw_centered(
            cr,
            if active { "Tab: Predict" } else { "Tab: Normal" },
            tr.x + tr.w / 2.0,
            tr.y + tr.h / 2.0,
            fs,
            fg,
        );

        // Word rows
        cr.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
        if total == 0 {
            self.draw_centered(
                cr,
                "No learned words",
                mr.x + mr.w / 2.0,
                mr.y + header_h + toggle_h + item_h / 2.0,
                fs,
                self.theme.modtxt,
            );
        }
        for i in 0..bar.n_dict_items {
            let Some(word) = bar.dict_words[i] else {
                continue;
            };
            let w = &words[page * 8 + i];
            let ry = mr.y + header_h + toggle_h + i as f64 * item_h;
            self.draw_left(cr, word, mr.x + pad, ry + item_h / 2.0, fs, self.theme.keytxt);
            let count = format!("x{}", w.count);
            let cw = self.text_w(cr, &count, fs);
            self.draw_left(
                cr,
                &count,
                bar.dict_remove_rects[i].x - cw - 6.0,
                ry + item_h / 2.0,
                fs,
                self.theme.modtxt,
            );
            let rr = &bar.dict_remove_rects[i];
            let rr_hov = self.inside && rr.contains(self.px as f64, self.py as f64);
            self.draw_centered(
                cr,
                "\u{00d7}",
                rr.x + rr.w / 2.0,
                rr.y + rr.h / 2.0,
                fs,
                if rr_hov { 0xffffff } else { self.theme.close },
            );
        }

        // Footer
        let rows = bar.n_dict_items.max(if total == 0 { 1 } else { 0 });
        let footer_y = mr.y + header_h + toggle_h + rows as f64 * item_h;
        let clr = &bar.dict_clear_rect;
        let clr_hov = self.inside && clr.contains(self.px as f64, self.py as f64);
        cr.new_path();
        rrect(cr, clr.x, clr.y, clr.w, clr.h, 5.0);
        set_src(cr, if clr_hov { self.theme.key_hov } else { self.theme.key_mod });
        let _ = cr.fill();
        self.draw_centered(
            cr,
            "Clear All",
            clr.x + clr.w / 2.0,
            clr.y + clr.h / 2.0,
            fs,
            if clr_hov { self.theme.keytxt } else { self.theme.modtxt },
        );
        if total_pages > 1 {
            let pr = &bar.dict_prev_rect;
            let pr_hov = self.inside && pr.contains(self.px as f64, self.py as f64);
            cr.new_path();
            rrect(cr, pr.x, pr.y, pr.w, pr.h, 5.0);
            set_src(cr, if pr_hov { self.theme.key_hov } else { self.theme.key_mod });
            let _ = cr.fill();
            self.draw_centered(
                cr,
                "\u{25c0}",
                pr.x + pr.w / 2.0,
                pr.y + pr.h / 2.0,
                fs,
                self.theme.keytxt,
            );
            let nr = &bar.dict_next_rect;
            let nr_hov = self.inside && nr.contains(self.px as f64, self.py as f64);
            cr.new_path();
            rrect(cr, nr.x, nr.y, nr.w, nr.h, 5.0);
            set_src(cr, if nr_hov { self.theme.key_hov } else { self.theme.key_mod });
            let _ = cr.fill();
            self.draw_centered(
                cr,
                "\u{25b6}",
                nr.x + nr.w / 2.0,
                nr.y + nr.h / 2.0,
                fs,
                self.theme.keytxt,
            );
            self.draw_centered(
                cr,
                &format!("Page {}/{}", page + 1, total_pages),
                mr.x + mr.w / 2.0,
                footer_y + footer_h / 2.0,
                fs,
                self.theme.modtxt,
            );
        }

        let _ = cr.restore();
    }

    /// Three diagonal ticks in the bottom-right corner.
    fn paint_handle(&self, cr: &Context) {
        let hs = self.handle_size() as f64;
        let hx = self.width as f64 - hs;
        let hy = self.height as f64 - hs;
        let hov = self.handle_hov();
        let _ = cr.save();
        cr.rectangle(hx, hy, hs, hs);
        cr.clip();
        cr.set_operator(Operator::Over);
        set_src(cr, if hov { 0x888888 } else { 0x555555 });
        for i in 0..3 {
            let off = 0.25 * hs * i as f64;
            cr.move_to(hx + 0.17 * hs + off, hy + hs - 0.17 * hs);
            cr.line_to(hx + hs - 0.17 * hs, hy + 0.17 * hs + off);
        }
        cr.set_line_width(0.08 * hs);
        let _ = cr.stroke();
        let _ = cr.restore();
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_key(&self, cr: &Context, rx: f64, ry: f64, rw: f64, rh: f64, key: &Key, idx: usize) {
        // A code-0 key with no label is the 0.5u gap: nothing to draw.
        if key.code == 0 && key.label.is_none() {
            return;
        }
        let k = self.k();
        let vis = self.mods.key_visual(key);
        let on = vis == ModKeyState::Latched || vis == ModKeyState::Locked;
        let pressed = self.pressed == idx as i32;
        let flashed = self.flash_key == idx as i32;
        let is_held = pressed && (self.rep_key == idx as i32 || now_ms().saturating_sub(self.press_time) >= 200);
        let hov = key.code != 0
            && self.inside
            && (self.px as f64) >= rx
            && (self.px as f64) < rx + rw
            && (self.py as f64) >= ry
            && (self.py as f64) < ry + rh;
        // Three tiers: character keys (letters, digits, symbols, space)
        // are lifted; modifiers are flat and dimmer; Enter is the only
        // filled accent. Esc is a plain modifier.
        let is_char = key.letter || key.shift_label.is_some() || key.code == key::KEY_SPACE;
        let is_accent = key.code == key::KEY_ENTER;
        // (fill, label) for the current state.
        let (bg, label) = if is_held {
            (self.theme.key_hold, self.theme.ink)
        } else if pressed || flashed {
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
            let upper = self.mods.shift_active() ^ self.mods.caps_lock();
            disp = if upper {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_ascii_lowercase().to_string()
            };
        } else if let Some(l) = key.label {
            disp = if self.mods.shift_active() {
                key.shift_label.unwrap_or(l).to_string()
            } else {
                l.to_string()
            };
        } else {
            disp = String::new();
        }
        let cx = rx + INSET_X_U * k;
        let cy = ry + INSET_Y_U * k;
        let cw = rw - 2.0 * INSET_X_U * k;
        let ch = rh - 2.0 * INSET_Y_U * k;
        let _ = cr.save();
        cr.rectangle(rx, ry, rw, rh);
        cr.clip();
        rrect(cr, 0.0, 0.0, self.width as f64, self.height as f64, CASE_RAD_U * k);
        cr.clip();
        // SOURCE so a stale neighbor pixel cannot show through the fill.
        cr.set_operator(Operator::Source);
        set_src_rgba(cr, self.theme.case, self.theme.opacity);
        cr.rectangle(rx, ry, rw, rh);
        let _ = cr.fill();
        cr.set_operator(Operator::Over);
        rrect(cr, cx, cy, cw, ch, KEY_RAD_U * k);
        let key_alpha = if self.theme.opacity < 1.0 {
            (self.theme.opacity + 0.08).min(1.0)
        } else {
            1.0
        };
        set_src_rgba(cr, bg, key_alpha);
        let _ = cr.fill();
        rrect(cr, cx + 0.5, cy + 0.5, cw - 1.0, ch - 1.0, KEY_RAD_U * k);
        set_src_rgba(cr, self.theme.edge, key_alpha);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        if !disp.is_empty() {
            let mut fs = FONT_U * k;
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
        if vis == ModKeyState::Locked {
            set_src(cr, 0xffffff);
            cr.arc(rx + rw - 0.23 * k, ry + 0.23 * k, 0.08 * k, 0.0, 2.0 * std::f64::consts::PI);
            let _ = cr.fill();
        }
        let _ = cr.restore();
    }

    fn ensure_rel(&mut self) {
        if self.rel.is_some() {
            return;
        }
        let (Some(mgr), Some(pointer)) = (self.rel_mgr.as_ref(), self.pointer.as_ref()) else {
            return;
        };
        self.rel = Some(mgr.get_relative_pointer(pointer, &self.qh, ()));
    }
}

// ---- Dispatch impls ----

impl Dispatch<wl_registry::WlRegistry, GlobalListContents, State> for State {
    fn event(
        _s: &mut State,
        _p: &wl_registry::WlRegistry,
        _e: wl_registry::Event,
        _u: &GlobalListContents,
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<wl_compositor::WlCompositor, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &wl_compositor::WlCompositor,
        _e: wl_compositor::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, (), State> for State {
    fn event(
        state: &mut State,
        _p: &wl_surface::WlSurface,
        event: wl_surface::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event {
            // Fractional scale is authoritative. This integer event is
            // the fallback for compositors that only advertise
            // wl_surface v6.
            if state.frac.is_none() && factor >= 1 {
                state.scale_120 = factor * 120;
                if state.ready {
                    state.request_redraw();
                }
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, (), State> for State {
    fn event(
        state: &mut State,
        p: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if capabilities
                .into_result()
                .is_ok_and(|caps| caps.contains(wl_seat::Capability::Pointer))
                && state.pointer.is_none()
            {
                eprintln!("[dbg] seat has pointer, creating WlPointer");
                state.pointer = Some(p.get_pointer(&state.qh, ()));
                state.ensure_rel();
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, (), State> for State {
    fn event(
        state: &mut State,
        _p: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                eprintln!("[dbg] pointer ENTER at ({surface_x}, {surface_y})");
                state.inside = true;
                state.px = surface_x.round() as i32;
                state.py = surface_y.round() as i32;
                state.request_redraw();
            }
            wl_pointer::Event::Leave { .. } => {
                // The button release is delivered to the surface under
                // the pointer at release time. If the pointer left while
                // the button was held, we never see it, so clear every
                // press/drag flag or it sticks. Drag state is already
                // committed by apply_geometry on each motion.
                state.inside = false;
                if state.pressed >= 0 {
                    state.flash_key = state.pressed;
                    state.flash_until = now_ms() + FLASH_MS;
                }
                state.pressed = -1;
                state.rep_key = -1;
                let was_drag = state.moving || state.resizing;
                state.moving = false;
                state.resizing = false;
                if was_drag {
                    state.save_config();
                }
                state.request_redraw();
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.px = surface_x.round() as i32;
                state.py = surface_y.round() as i32;
                state.on_motion();
            }
            wl_pointer::Event::Button {
                button,
                state: bstate,
                ..
            } if button == 1 || button == BTN_LEFT => {
                // wlroots/Hyprland report the raw Linux code (BTN_LEFT=272);
                // other compositors use the Wayland number (1). Accept both.
                let pressed = bstate
                    .into_result()
                    .is_ok_and(|s| s == wl_pointer::ButtonState::Pressed);
                state.on_button(pressed);
            }
            wl_pointer::Event::Button {
                button,
                state: bstate,
                ..
            } if button == 2 || button == 3 || button == BTN_RIGHT => {
                // Wayland numbers 2/3 and the raw Linux code (BTN_RIGHT=273).
                let pressed = bstate
                    .into_result()
                    .is_ok_and(|s| s == wl_pointer::ButtonState::Pressed);
                if pressed {
                    state.on_right_click();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_shm::WlShm, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &wl_shm::WlShm,
        _e: wl_shm::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &wl_shm_pool::WlShmPool,
        _e: wl_shm_pool::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize, State> for State {
    fn event(
        state: &mut State,
        _p: &wl_buffer::WlBuffer,
        _e: wl_buffer::Event,
        udata: &usize,
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
        state.frames[*udata].attached = false;
        // render() bails when both buffers are busy and sets dirty. The
        // release is the signal that a buffer can be painted again.
        if state.dirty && !state.frame_pending && state.ready {
            state.request_redraw();
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u32, State> for State {
    fn event(
        state: &mut State,
        _p: &wl_callback::WlCallback,
        _e: wl_callback::Event,
        gen: &u32,
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
        if *gen != state.frame_gen {
            return;
        }
        state.frame_pending = false;
        state.presented = true;
        if debug() {
            eprintln!(
                "ui present {}x{} scale {}/120",
                state.width, state.height, state.scale_120
            );
        }
        // This callback is the present of the commit that stored
        // commit_*; pointer samples from here on are in that space.
        state.shown_x = state.commit_x;
        state.shown_y = state.commit_y;
        if !state.dirty {
            return;
        }
        state.dirty = false;
        if state.moving && state.width == state.attached_w && state.height == state.attached_h {
            // Buffer content did not change. Commit the latest margin;
            // the callback is part of this commit so the next present
            // wakes us.
            state.arm_frame();
            if let Some(surf) = state.surface.as_ref() {
                surf.commit();
            }
            state.commit_x = state.pos_x;
            state.commit_y = state.pos_y;
            state.last_commit_ms = now_ms();
        } else {
            state.render();
        }
    }
}

impl Dispatch<wl_region::WlRegion, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &wl_region::WlRegion,
        _e: wl_region::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwlrLayerShellV1, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &ZwlrLayerShellV1,
        _e: wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, (), State> for State {
    fn event(
        state: &mut State,
        p: &ZwlrLayerSurfaceV1,
        event: wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event;
        match event {
            Event::Configure {
                serial,
                width,
                height,
            } => {
                p.ack_configure(serial);
                // use our requested size (the hint is advisory)
                if state.width > 0 && state.height > 0 {
                    if width != state.width as u32 || height != state.height as u32 {
                        state.width = if width > 0 { width as i32 } else { state.width };
                        state.height = if height > 0 {
                            height as i32
                        } else {
                            state.height
                        };
                    }
                } else {
                    state.width = if width > 0 { width as i32 } else { 720 };
                    state.height = if height > 0 { height as i32 } else { 340 };
                }
                state.ready = true;
                state.request_redraw();
            }
            Event::Closed => {
                state.save_config();
                state.quit = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<WpFractionalScaleManagerV1, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &WpFractionalScaleManagerV1,
        _e: wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, (), State> for State {
    fn event(
        state: &mut State,
        _p: &WpFractionalScaleV1,
        event: wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
        use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::Event;
        if let Event::PreferredScale { scale } = event {
            state.scale_120 = (scale as i32).max(1);
            if state.ready {
                state.request_redraw();
            }
        }
    }
}

impl Dispatch<WpViewporter, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &WpViewporter,
        _e: wayland_protocols::wp::viewporter::client::wp_viewporter::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WpViewport, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &WpViewport,
        _e: wayland_protocols::wp::viewporter::client::wp_viewport::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwpRelativePointerManagerV1, (), State> for State {
    fn event(
        _s: &mut State,
        _p: &ZwpRelativePointerManagerV1,
        _e: wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwpRelativePointerV1, (), State> for State {
    fn event(
        state: &mut State,
        _p: &ZwpRelativePointerV1,
        event: wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::Event,
        _u: &(),
        _c: &Connection,
        _q: &QueueHandle<State>,
    ) {
        use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::Event;
        if let Event::RelativeMotion { dx, dy, .. } = event {
            state.on_relative_motion(dx, dy);
        }
    }
}

/// The layer-shell UI. Runs until the keyboard is closed or the display
/// disconnects.
pub fn run(backend: Box<dyn Backend>) -> i32 {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("osk: no wayland display: {e}");
            return 1;
        }
    };
    let (globals, mut sq) = match registry_queue_init::<State>(&conn) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("osk: registry init failed: {e}");
            return 1;
        }
    };
    let qh = sq.handle();
    let mut state = State::new(backend, qh.clone());

    // Bind the globals we need.
    let comp: wl_compositor::WlCompositor = match globals.bind(&qh, 1..=7, ()) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("osk: no wl_compositor");
            return 1;
        }
    };
    let shm: wl_shm::WlShm = match globals.bind(&qh, 1..=1, ()) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("osk: no wl_shm");
            return 1;
        }
    };
    let layer_shell: ZwlrLayerShellV1 = match globals.bind(&qh, 1..=4, ()) {
        Ok(l) => l,
        Err(_) => {
            eprintln!("osk: no zwlr_layer_shell_v1");
            return 1;
        }
    };
    state.comp = Some(comp.clone());
    state.shm = Some(shm);
    state.layer_shell = Some(layer_shell.clone());
    state.seat = globals.bind(&qh, 1..=7, ()).ok();
    state.frac_mgr = globals.bind(&qh, 1..=1, ()).ok();
    state.viewporter = globals.bind(&qh, 1..=1, ()).ok();
    state.rel_mgr = globals.bind(&qh, 1..=1, ()).ok();

    // Create the surface and its role objects.
    let surface = comp.create_surface(&qh, ());
    state.surface = Some(surface.clone());
    if let Some(mgr) = state.frac_mgr.as_ref() {
        state.frac = Some(mgr.get_fractional_scale(&surface, &qh, ()));
    }
    if let Some(vp) = state.viewporter.as_ref() {
        state.viewport = Some(vp.get_viewport(&surface, &qh, ()));
    }
    // Pick up the preferred scale before the first buffer, so the first
    // frame is already at the output scale instead of a blurry 1x upload.
    let _ = sq.roundtrip(&mut state);

    let layer =
        layer_shell.get_layer_surface(&surface, None, Layer::Top, "osk".to_string(), &qh, ());
    state.layer = Some(layer.clone());
    // Never take keyboard focus: keys always go to the user's focused
    // window.
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);

    // Default size: 1u = U0 pixels, like the reference keyboard (if no valid saved size).
    let rows = state.layout().rows.len() as f64;
    if state.width < MIN_W {
        state.width = ((state.grid_cols as f64 / 20.0 + 2.0 * MARGIN_U) * U0 + 0.5) as i32;
    }
    if state.height < MIN_H {
        state.height = ((BAR_U + rows + 2.0 * MARGIN_U) * U0 + 0.5) as i32;
    }
    state.apply_geometry();

    // Event loop: poll the connection fd with a timeout so key repeat
    // fires even when no compositor events arrive (the C reference
    // polls with repeat_timeout(); blocking_dispatch alone would sleep
    // forever with a held key).
    let mut poller = match mio::Poll::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("osk: poll init failed: {e}");
            return 1;
        }
    };
    {
        let raw = conn.as_fd().as_raw_fd();
        let mut src = mio::unix::SourceFd(&raw);
        if let Err(e) = poller
            .registry()
            .register(&mut src, WL_TOKEN, mio::Interest::READABLE)
        {
            eprintln!("osk: poll register failed: {e}");
            return 1;
        }
    }
    let mut events = mio::Events::with_capacity(4);
    loop {
        state.handle_repeat();
        state.handle_flash();
        state.handle_hold();
        if state.quit {
            break;
        }
        state.watchdog();
        if let Err(e) = conn.flush() {
            eprintln!("osk: display flush error: {e}");
            break;
        }
        // prepare_read must be created before polling the fd.
        let guard = conn.prepare_read();
        if guard.is_none() {
            // Events are already in the queue; dispatch and loop.
            if let Err(e) = sq.dispatch_pending(&mut state) {
                eprintln!("osk: display connection error: {e}");
                break;
            }
            continue;
        }
        let guard = guard.unwrap();
        let mut wl_ready = events.iter().any(|e| e.token() == WL_TOKEN);
        if !wl_ready {
            if let Err(e) = poller.poll(&mut events, state.rep_timeout()) {
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                eprintln!("osk: poll error: {e}");
                break;
            }
        }
        wl_ready = events.iter().any(|e| e.token() == WL_TOKEN);
        if wl_ready {
            if let Err(e) = guard.read() {
                let io_err = match e {
                    WaylandError::Io(e) => e,
                    other => {
                        eprintln!("osk: display read error: {other:?}");
                        break;
                    }
                };
                // Spurious readiness: nothing to read yet.
                if io_err.kind() != std::io::ErrorKind::WouldBlock {
                    eprintln!("osk: display read error: {io_err}");
                    break;
                }
            }
        }
        events.clear();
        if let Err(e) = sq.dispatch_pending(&mut state) {
            eprintln!("osk: display connection error: {e}");
            break;
        }
        state.backend.pump();
    }

    state.save_config();
    state.backend.destroy();
    0
}
