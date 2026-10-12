//! The Win32/GDI on-screen keyboard.
//!
//! The keyboard is a topmost, non-activating popup window. `MA_NOACTIVATE`
//! in the window procedure keeps it from stealing focus, so injected keys
//! land in whatever window the user focused. Pointer events are delivered
//! normally because the window is not `WS_EX_TRANSPARENT`.
//!
//! Rendering: the keyboard is drawn into a 32-bit back buffer (the
//! [`Canvas`]) and blitted to the window DC on `WM_PAINT`. There is no
//! compositor to negotiate with, so a resize is a plain `SetWindowPos`.

use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetDC, InvalidateRect, PAINTSTRUCT, ReleaseDC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
    PostQuitMessage, RegisterClassExW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage,
    WNDCLASSEXW, CS_HREDRAW, CS_VREDRAW, MSG, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WM_DESTROY, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN, WM_SIZE, WM_TIMER,
};

use crate::backend::{Backend, BackendSink};
use crate::gdip::Canvas;
use oskwin_core::events::{complete_word, tap_events, OskEvent, TAP_MAX_EVENTS};
use oskwin_core::key::{key, Key, Layout};
use oskwin_core::layout::ALL_LAYOUTS;
use oskwin_core::mods::{mod_index, ModKeyState, ModifierEngine};
use oskwin_core::predict::Predictor;
use oskwin_core::typekey::{type_string, CharMap};

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
const FLASH_MS: u64 = 150;

// Sans-serif face for the labels; it carries every glyph we draw
// (letters, digits, and the arrow/symbol caps) in one font.
const FONT: &str = "Segoe UI";

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

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

fn now_ms() -> u64 {
    START.elapsed().as_millis() as u64
}

fn set_src_rgba(cr: &mut Canvas, argb: u32, alpha: f64) {
    cr.set_source_rgba(
        ((argb >> 16) & 0xff) as f64 / 255.0,
        ((argb >> 8) & 0xff) as f64 / 255.0,
        (argb & 0xff) as f64 / 255.0,
        alpha,
    );
}

fn set_src(cr: &mut Canvas, argb: u32) {
    set_src_rgba(cr, argb, 1.0);
}

/// Rounded rectangle. The radius is clamped to half the smaller side so the
/// arcs never overlap.
fn rrect(cr: &mut Canvas, x: f64, y: f64, w: f64, h: f64, rad: f64) {
    let mut r = rad;
    if r > w / 2.0 {
        r = w / 2.0;
    }
    if r > h / 2.0 {
        r = h / 2.0;
    }
    cr.rrect(x, y, w, h, r);
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

pub struct State {
    backend: Box<dyn Backend>,
    /// Window handle, set once the window is created (0 before that).
    pub hwnd: isize,
    /// Cached back buffer, recreated when the window is resized.
    canvas: Option<Canvas>,
    // geometry (logical)
    width: i32,
    height: i32,
    pos_x: i32,
    pos_y: i32,
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
    // window position at the start of a bar drag (drag base)
    shown_x: i32,
    shown_y: i32,
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
    pub fn new(backend: Box<dyn Backend>) -> Self {
        let config = PersistentConfig::load();
        let initial_theme_idx = std::env::var("OSK_THEME")
            .ok()
            .and_then(|name| THEMES.iter().position(|t| t.name.eq_ignore_ascii_case(&name)))
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
            hwnd: 0,
            canvas: None,
            width: initial_w,
            height: initial_h,
            pos_x: initial_pos_x,
            pos_y: initial_pos_y,
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
            shown_x: initial_pos_x,
            shown_y: initial_pos_y,
            rep_key: -1,
            rep_next: 0,
            grid_cols: 0,
            mods: ModifierEngine::new(),
            predictor: Predictor::new(),
            charmap: CharMap::new(),
            dirty: false,
            last_hot: 0,
            last_geom: (-1, -1, -1, -1),
            sugg: [Rect::default(); 5],
            n_sugg: 0,
            layout_rect: Rect::default(),
            theme_rect: Rect::default(),
            close_rect: Rect::default(),
            layout_idx: initial_layout_idx,
            theme_idx: initial_theme_idx,
            theme,
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
        // drag costs no API traffic.
        if geom == self.last_geom {
            return;
        }
        self.last_geom = geom;
        if self.hwnd != 0 {
            let _ = unsafe {
                SetWindowPos(
                    HWND(self.hwnd as *mut _),
                    None,
                    self.pos_x,
                    self.pos_y,
                    self.width,
                    self.height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
        }
    }

    // ---- redraw ----

    /// Mark the window invalid; the message loop turns that into a
    /// `WM_PAINT` that calls [`State::render`].
    fn request_redraw(&self) {
        if self.hwnd != 0 {
            let _ = unsafe { InvalidateRect(Some(HWND(self.hwnd as *mut _)), None, false) };
        }
    }

    /// Draw the keyboard into the back buffer and blit it to the window.
    fn render(&mut self) {
        if self.width <= 0 || self.height <= 0 || self.hwnd == 0 {
            return;
        }
        let mut canvas = match self.canvas.take() {
            Some(c) if c.w == self.width && c.h == self.height => c,
            _ => match Canvas::new_bitmap(self.width, self.height) {
                Some(c) => c,
                None => return,
            },
        };
        canvas.clear();
        let bar = self.measure_bar(&mut canvas);
        self.paint(&mut canvas, &bar);
        let hwnd = HWND(self.hwnd as *mut _);
        let hdc = unsafe { GetDC(Some(hwnd)) };
        canvas.blit_to_hdc(hdc.0);
        unsafe {
            ReleaseDC(Some(hwnd), hdc);
        }
        self.canvas = Some(canvas);
        self.dirty = false;
    }

    /// Render the keyboard to a PPM file (headless verification).
    pub fn render_to_ppm(&mut self, path: &str) -> bool {
        let rows = self.layout().rows.len() as f64;
        if self.width < MIN_W {
            self.width = ((self.grid_cols as f64 / 20.0 + 2.0 * MARGIN_U) * U0 + 0.5) as i32;
        }
        if self.height < MIN_H {
            self.height = ((BAR_U + rows + 2.0 * MARGIN_U) * U0 + 0.5) as i32;
        }
        let Some(mut canvas) = Canvas::new_bitmap(self.width, self.height) else {
            return false;
        };
        canvas.clear();
        let bar = self.measure_bar(&mut canvas);
        self.paint(&mut canvas, &bar);
        canvas.dump_ppm(path)
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
            && k.code == key::KEY_TAB
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
        // A tap repaints immediately.
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
            // `shown_*` is the window position at grab time; the pointer's
            // window-local offset from the grab point is the drag delta.
            let nx = (self.shown_x + (self.px - self.grab_lx)).max(0);
            let ny = (self.shown_y + (self.py - self.grab_ly)).max(0);
            if nx != self.pos_x || ny != self.pos_y {
                self.pos_x = nx;
                self.pos_y = ny;
                self.apply_geometry();
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
                self.shown_x = self.pos_x;
                self.shown_y = self.pos_y;
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

    // ---- painting ----

    /// Measure the bar elements (suggestions, layout, theme icon, close)
    /// and store their rects for hit testing.
    fn measure_bar(&self, cr: &mut Canvas) -> BarLayout {
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

    fn paint(&self, cr: &mut Canvas, bar: &BarLayout) {
        cr.reset_clip();
        cr.select_font_face(FONT, 0, 0);
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

    fn paint_window(&self, cr: &mut Canvas) {
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

    fn draw_centered(&self, cr: &mut Canvas, s: &str, cx: f64, cy: f64, size: f64, color: u32) {
        cr.set_font_size(size);
        let Some(ext) = cr.text_extents(s) else {
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
    fn draw_left(&self, cr: &mut Canvas, s: &str, x: f64, cy: f64, size: f64, color: u32) {
        cr.set_font_size(size);
        let Some(ext) = cr.text_extents(s) else {
            return;
        };
        set_src(cr, color);
        cr.move_to(x - ext.x_bearing(), cy - ext.height() / 2.0 - ext.y_bearing());
        let _ = cr.show_text(s);
    }

    fn text_w(&self, cr: &mut Canvas, s: &str, size: f64) -> f64 {
        cr.set_font_size(size);
        cr.text_extents(s).map(|e| e.width()).unwrap_or(0.0)
    }

    fn paint_chip(&self, cr: &mut Canvas, r: &Rect, label: &str, font_size: f64, bh: f64) {
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

    fn paint_bar(&self, cr: &mut Canvas, bar: &BarLayout) {
        let bh = self.bar_h();
        let k = self.k();
        let fs = 0.34 * k;
        let _ = cr.save();
        cr.rectangle(0.0, 0.0, self.width as f64, bh);
        cr.clip();
        rrect(cr, 0.0, 0.0, self.width as f64, self.height as f64, CASE_RAD_U * k);
        cr.clip();
        set_src_rgba(cr, self.theme.case, self.theme.opacity);
        cr.rectangle(0.0, 0.0, self.width as f64, bh);
        let _ = cr.fill();
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

            let weight = if is_primary { 1 } else { 0 };
            cr.select_font_face(FONT, 0, weight);
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
            rrect(
                cr,
                bar.theme_rect.x + 0.5,
                pad_y + 0.5,
                bar.theme_rect.w - 1.0,
                chip_h - 1.0,
                chip_h * 0.35,
            );
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

    fn draw_palette_icon(&self, cr: &mut Canvas, cx: f64, cy: f64, r: f64, color: u32) {
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

    fn paint_theme_menu(&self, cr: &mut Canvas, bar: &BarLayout) {
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
            let weight = if is_cur || hov { 1 } else { 0 };
            cr.select_font_face(FONT, 0, weight);
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

    fn paint_dict_menu(&self, cr: &mut Canvas, bar: &BarLayout) {
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
        cr.select_font_face(FONT, 0, 1);
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
        cr.select_font_face(FONT, 0, if active { 1 } else { 0 });
        self.draw_centered(
            cr,
            if active { "Tab: Predict" } else { "Tab: Normal" },
            tr.x + tr.w / 2.0,
            tr.y + tr.h / 2.0,
            fs,
            fg,
        );

        // Word rows
        cr.select_font_face(FONT, 0, 0);
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
    fn paint_handle(&self, cr: &mut Canvas) {
        let hs = self.handle_size() as f64;
        let hx = self.width as f64 - hs;
        let hy = self.height as f64 - hs;
        let hov = self.handle_hov();
        let _ = cr.save();
        cr.rectangle(hx, hy, hs, hs);
        cr.clip();
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
    fn paint_key(&self, cr: &mut Canvas, rx: f64, ry: f64, rw: f64, rh: f64, key: &Key, idx: usize) {
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
        set_src_rgba(cr, self.theme.case, self.theme.opacity);
        cr.rectangle(rx, ry, rw, rh);
        let _ = cr.fill();
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
}

/// The keyboard state, shared with the window procedure through a raw
/// pointer (the Win32 callback has no way to carry user data here).
static mut STATE_PTR: *mut State = std::ptr::null_mut();

/// Window procedure. `WM_MOUSEACTIVATE` returns `MA_NOACTIVATE` (3) so the
/// OSK never takes keyboard focus from the active window.
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if STATE_PTR.is_null() {
        return DefWindowProcW(hwnd, msg, _wparam, lparam);
    }
    let state = &mut *STATE_PTR;
    match msg {
        WM_MOUSEACTIVATE => LRESULT(3),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            state.render();
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER => {
            state.handle_repeat();
            state.handle_flash();
            state.handle_hold();
            state.backend.pump();
            if state.quit {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            state.px = x;
            state.py = y;
            state.inside = x >= 0 && x < state.width && y >= 0 && y < state.height;
            state.on_motion();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            state.px = x;
            state.py = y;
            state.inside = true;
            state.on_button(true);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            state.px = x;
            state.py = y;
            state.on_button(false);
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            state.px = x;
            state.py = y;
            state.on_right_click();
            LRESULT(0)
        }
        WM_SIZE => {
            let w = lparam.0 as u16 as i32;
            let h = (lparam.0 >> 16) as u16 as i32;
            if w != state.width || h != state.height {
                state.width = w;
                state.height = h;
                state.dirty = true;
                state.request_redraw();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            state.quit = true;
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, _wparam, lparam),
    }
}

/// The Win32 UI. Runs until the keyboard is closed or the window is
/// destroyed.
pub fn run(backend: Box<dyn Backend>) -> i32 {
    let mut state = Box::new(State::new(backend));

    // Default size: 1u = U0 pixels (if no valid saved size).
    let rows = state.layout().rows.len() as f64;
    if state.width < MIN_W {
        state.width = ((state.grid_cols as f64 / 20.0 + 2.0 * MARGIN_U) * U0 + 0.5) as i32;
    }
    if state.height < MIN_H {
        state.height = ((BAR_U + rows + 2.0 * MARGIN_U) * U0 + 0.5) as i32;
    }

    let class_name: Vec<u16> = "osk-win-window\0".encode_utf16().collect();
    let title: Vec<u16> = "OSK\0".encode_utf16().collect();
    let hinst = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            eprintln!("osk: GetModuleHandleW failed: {e}");
            return 1;
        }
    };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE(hinst.0),
        lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
        ..Default::default()
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        eprintln!("osk: RegisterClassExW failed");
        return 1;
    }
    unsafe {
        STATE_PTR = &mut *state as *mut State;
    }
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            state.pos_x,
            state.pos_y,
            state.width,
            state.height,
            None,
            None,
            Some(HINSTANCE(hinst.0)),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            unsafe { STATE_PTR = std::ptr::null_mut(); }
            eprintln!("osk: CreateWindowExW failed: {e}");
            return 1;
        }
    };
    state.hwnd = hwnd.0 as isize;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), 1, 16, None);
    }
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if state.quit {
            break;
        }
    }
    unsafe {
        let _ = KillTimer(Some(hwnd), 1);
        let _ = DestroyWindow(hwnd);
        STATE_PTR = std::ptr::null_mut();
    }
    state.save_config();
    state.backend.destroy();
    0
}
