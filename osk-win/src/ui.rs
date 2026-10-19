//! The Win32 host: a topmost, non-activating popup window running the
//! Linux osk's UI.
//!
//! Everything a click does is the shared [`Controller`]; the pointer
//! state is the shared [`Interaction`]; a frame paints the shared
//! [`View`] from `render.rs`. This file is the Win32 side of the Linux
//! `ui.rs`: the window, its messages, and a timer armed only for the
//! next deadline (key repeat, hold, flash, the handle ease, dictation),
//! so an idle keyboard runs nothing.
//!
//! `WM_MOUSEACTIVATE` returns `MA_NOACTIVATE`, so the keyboard never
//! takes focus and injected keys land in the window the user focused.

use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, UpdateWindow, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, GetWindowRect, KillTimer, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    HHOOK, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_APP, WM_MBUTTONUP, WM_RBUTTONUP, LoadCursorW, PostQuitMessage, RegisterClassExW, SetCursor, SetTimer, SetWindowPos,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, IDC_ARROW, IDC_SIZENWSE, MSG,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, WM_DESTROY, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN,
    WM_SETCURSOR, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use osk_core::controller::{Clicked, Controller, ControllerConfig};
use osk_core::interaction::{self, Event as IEvent, Interaction};
use osk_core::key::{key, Click, Key, KeyKind, Layout};
use osk_core::mods::{ModKeyState, DOUBLE_CLICK_MS, MOD_ALT};
use osk_core::layout::ALL_LAYOUTS;
use osk_core::metrics::Metrics;
use osk_core::neural::NeuralEngine;
use osk_core::predict::Predictor;
#[cfg(feature = "dictation")]
use osk_core::typekey::{type_text, KeySink};

use crate::backend::Backend;
use crate::cairo::{Context, FontSlant, FontWeight, Format, ImageSurface};
use crate::history::{load_history, save_history_if_dirty};
#[cfg(feature = "dictation")]
use crate::render::DictState;
use crate::render::{
    self, now_ms, BarLayout, KeyCache, Rect, Theme, THEMES, View, FONT, ViaChipLook, ViaFrame,
};

/// Smallest surface the resize handle allows.
const MIN_W: i32 = 200;
const MIN_H: i32 = 180;
const FLASH_MS: u64 = 60;
/// Frame pacing of the handle ease and the dictation waveform.
const ANIM_FRAME_MS: u64 = 16;
#[cfg(feature = "dictation")]
const DICT_FRAME_MS: u64 = 33;

const WM_MOUSELEAVE: u32 = 0x02A3;
const MA_NOACTIVATE: isize = 3;
const TIMER_ID: usize = 1;

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
    #[serde(default = "default_prediction")]
    pub prediction: bool,
}

fn default_pos_x() -> i32 {
    600
}

fn default_pos_y() -> i32 {
    450
}

fn default_prediction() -> bool {
    true
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
            prediction: true,
        }
    }
}

/// `%OSK_CONFIG%`, else `%APPDATA%\osk\config.json` (or the local or
/// profile AppData).
fn config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("OSK_CONFIG") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Ok(dir) = std::env::var(var) {
            let trimmed = dir.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed).join("osk").join("config.json"));
            }
        }
    }
    let profile = std::env::var("USERPROFILE").ok()?;
    let trimmed = profile.trim();
    (!trimmed.is_empty()).then(|| {
        PathBuf::from(trimmed).join("AppData").join("Roaming").join("osk").join("config.json")
    })
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
        // Windows refuses to rename over an existing file, so the old
        // one goes first; the plain write is the last resort.
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json.as_bytes()).is_ok() {
            if std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&path);
                if std::fs::rename(&tmp, &path).is_err() {
                    let _ = std::fs::write(&path, json.as_bytes());
                }
            }
        } else {
            let _ = std::fs::write(&path, json.as_bytes());
        }
    }
}

// The UI has a handful of independent flags; the real interaction
// state lives in `interaction`, so no reset can miss it.
#[allow(clippy::struct_excessive_bools)]
pub struct State {
    backend: Box<dyn Backend>,
    /// Window handle, 0 until the window exists.
    hwnd: isize,
    /// The back buffer a frame is drawn into, then copied to the window.
    surface: Option<ImageSurface>,
    /// The at-rest keyboard, reused until what it depends on changes.
    key_cache: Option<KeyCache>,
    width: i32,
    height: i32,
    pos_x: i32,
    pos_y: i32,
    // pointer, client coordinates
    px: i32,
    py: i32,
    inside: bool,
    interaction: Interaction,
    /// Window position when a drag began. Windows moves the window
    /// under the pointer, so a drag follows the pointer in screen
    /// coordinates from here (client coordinates jitter).
    drag_origin: (i32, i32),
    flash_key: i32,
    flash_until: u64,
    /// The Alt key this osk holds down in Windows, if any.
    alt_down: Option<u16>,
    /// The mouse hook watching for a click elsewhere while Alt is latched.
    mouse_hook: Option<HHOOK>,
    grid_cols: i32,
    handle_hover: bool,
    handle_anim_since: Option<u64>,
    ctl: Controller,
    last_hot: u32,
    last_geom: (i32, i32, i32, i32),
    // measured bar layout, filled during render
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
    tab_toggle_rect: Rect,
    pred_toggle_rect: Rect,
    mic_rect: Rect,
    #[cfg(feature = "dictation")]
    dict_state: DictState,
    #[cfg(feature = "dictation")]
    recorder: Option<crate::dictate::Recorder>,
    #[cfg(feature = "dictation")]
    dict_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    #[cfg(feature = "dictation")]
    dict_frame_due: u64,
    quit: bool,
}

/// Alt latches like Ctrl on Windows, and [`State::sync_alt`] holds it
/// down while latched, as the Windows osk does. The shared layouts make
/// it a momentary tap, but a lone Alt press+release opens the focused
/// app's menu bar (KeyTips, accelerator underlines) and leaves it showing.
fn windows_click(mut k: Key) -> Key {
    if let KeyKind::Inject { code, click } = &mut k.kind {
        if *click == Click::Momentary && is_alt(code.get()) {
            *click = Click::Latch;
        }
    }
    k
}

fn is_alt(code: u16) -> bool {
    matches!(code, key::KEY_LEFTALT | key::KEY_RIGHTALT)
}

/// Injects through the backend, tracking whether Alt is down: a tap
/// under a latched Alt sends its own Alt press and release.
struct AltSink<'a> {
    b: &'a mut dyn Backend,
    alt: &'a mut Option<u16>,
}

impl KeySink for AltSink<'_> {
    fn press(&mut self, code: u16) {
        if is_alt(code) {
            *self.alt = Some(code);
        }
        self.b.press(code);
    }
    fn release(&mut self, code: u16) {
        if is_alt(code) {
            *self.alt = None;
        }
        self.b.release(code);
    }
}

/// Posted by [`mouse_hook`] when a mouse button comes up outside the osk.
const WM_CLICK_ELSEWHERE: u32 = WM_APP + 1;
const HC_ACTION: i32 = 0;
static OSK_HWND: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION && matches!(wparam.0 as u32, WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP) {
        let pt = unsafe { (*(lparam.0 as *const MSLLHOOKSTRUCT)).pt };
        let hwnd = HWND(OSK_HWND.load(Ordering::Relaxed) as *mut _);
        let mut r = RECT::default();
        let inside = unsafe { GetWindowRect(hwnd, &mut r) }.is_ok()
            && (r.left..r.right).contains(&pt.x)
            && (r.top..r.bottom).contains(&pt.y);
        if !inside {
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLICK_ELSEWHERE, WPARAM(0), LPARAM(0)) };
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

impl State {
    pub fn new(backend: Box<dyn Backend>, neural: Option<NeuralEngine>) -> Self {
        let config = PersistentConfig::load();
        let theme_idx = std::env::var("OSK_THEME")
            .ok()
            .and_then(|name| THEMES.iter().position(|t| t.name.eq_ignore_ascii_case(&name)))
            .or_else(|| THEMES.iter().position(|t| t.name.eq_ignore_ascii_case(&config.theme)))
            .unwrap_or(0);
        let layout_idx = ALL_LAYOUTS
            .iter()
            .position(|l| l.name.eq_ignore_ascii_case(&config.layout))
            .unwrap_or(0);
        let layout = render::layout_at(layout_idx);
        let grid_cols = render::grid_cols(layout);
        let (default_w, default_h) = render::default_size(layout, grid_cols);
        let mut ctl = Controller::with_neural(
            ControllerConfig {
                prediction: config.prediction,
                tab_accepts_prediction: config.tab_accepts_prediction,
            },
            Predictor::new(),
            neural,
        );
        // The predictor starts empty; the host loads the learned history
        // once and saves it whenever a commit flags it dirty.
        load_history(ctl.predictor_mut());
        State {
            backend,
            hwnd: 0,
            surface: None,
            key_cache: None,
            width: if config.width >= MIN_W { config.width } else { default_w },
            height: if config.height >= MIN_H { config.height } else { default_h },
            pos_x: config.pos_x,
            pos_y: config.pos_y,
            px: 0,
            py: 0,
            inside: false,
            interaction: Interaction::Idle,
            drag_origin: (0, 0),
            flash_key: -1,
            flash_until: 0,
            alt_down: None,
            mouse_hook: None,
            grid_cols,
            handle_hover: false,
            handle_anim_since: None,
            ctl,
            last_hot: 0,
            last_geom: (-1, -1, -1, -1),
            sugg: [Rect::default(); 5],
            n_sugg: 0,
            layout_rect: Rect::default(),
            theme_rect: Rect::default(),
            close_rect: Rect::default(),
            layout_idx,
            theme_idx,
            theme: THEMES[theme_idx],
            theme_menu_open: false,
            theme_menu_rect: Rect::default(),
            theme_item_rects: [Rect::default(); 12],
            tab_toggle_rect: Rect::default(),
            pred_toggle_rect: Rect::default(),
            mic_rect: Rect::default(),
            #[cfg(feature = "dictation")]
            dict_state: DictState::Idle,
            #[cfg(feature = "dictation")]
            recorder: None,
            #[cfg(feature = "dictation")]
            dict_rx: None,
            #[cfg(feature = "dictation")]
            dict_frame_due: 0,
            quit: false,
        }
    }

    fn save_config(&self) {
        PersistentConfig {
            theme: self.theme.name.to_string(),
            layout: self.layout().name.to_string(),
            pos_x: self.pos_x,
            pos_y: self.pos_y,
            width: self.width,
            height: self.height,
            tab_accepts_prediction: self.ctl.tab_accepts_prediction(),
            prediction: self.ctl.prediction(),
        }
        .save();
    }

    /// Persist learned history when a commit flagged it dirty. A failed
    /// write is user-facing and must not be silent.
    fn save_history(&mut self) {
        if let Err(e) = save_history_if_dirty(self.ctl.predictor_mut()) {
            warn!("osk: saving learned history failed: {e}");
        }
    }

    fn layout(&self) -> &'static Layout {
        render::layout_at(self.layout_idx)
    }

    // ---- handle hover ease ----

    /// When the handle hover state flips, timestamp the flip so the
    /// arc's ease runs from the transition.
    fn sync_handle_anim(&mut self) {
        let hov = self.view().handle_hov();
        if hov != self.handle_hover {
            // A reversal mid-ease continues from the arc's current look
            // (t = 1 - cbrt(1 - y) inverts ease_out_cubic).
            let shown = self.handle_anim();
            let from = if hov { shown } else { 1.0 - shown };
            let t0 = 1.0 - (1.0 - from).cbrt();
            let back = (t0 * render::HANDLE_ANIM_MS as f64) as u64;
            self.handle_hover = hov;
            self.handle_anim_since = Some(now_ms().saturating_sub(back));
            self.request_redraw();
        }
    }

    fn handle_anim(&self) -> f64 {
        let Some(since) = self.handle_anim_since else {
            return 0.0;
        };
        let t = render::anim_progress(now_ms(), since, render::HANDLE_ANIM_MS);
        if self.handle_hover {
            t
        } else {
            1.0 - t
        }
    }

    fn handle_anim_active(&self) -> bool {
        self.handle_anim_since
            .is_some_and(|since| now_ms().saturating_sub(since) < render::HANDLE_ANIM_MS)
    }

    // ---- geometry ----

    fn key_at(&self, x: i32, y: i32) -> Option<usize> {
        let v = self.view();
        if y < v.bar_h() as i32 || self.width == 0 {
            return None;
        }
        if self.theme_menu_open && self.theme_menu_rect.contains(x as f64, y as f64) {
            return None;
        }
        for (r, row) in self.layout().rows.iter().enumerate() {
            for (i, k) in row.keys.iter().enumerate() {
                if k.is_gap() {
                    continue;
                }
                let (rx, ry, rw, rh) = v.key_rect(r, i);
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

    fn key_of(&self, idx: usize) -> &Key {
        &self.layout().rows[idx / 100].keys[idx % 100]
    }

    fn apply_geometry(&mut self) {
        let geom = (self.pos_x, self.pos_y, self.width, self.height);
        // Identical re-sends are skipped so a stationary pointer during
        // a drag costs nothing.
        if geom == self.last_geom {
            return;
        }
        let size_changed = self.width != self.last_geom.2 || self.height != self.last_geom.3;
        self.last_geom = geom;
        if self.hwnd == 0 {
            return;
        }
        let mut flags = SWP_NOZORDER | SWP_NOACTIVATE;
        if !size_changed {
            flags |= SWP_NOSIZE;
        }
        let _ = unsafe {
            SetWindowPos(
                HWND(self.hwnd as *mut _),
                None,
                self.pos_x,
                self.pos_y,
                self.width,
                self.height,
                flags,
            )
        };
    }

    /// The drawing snapshot for this frame: exactly what the render
    /// module's painters read. Hit testing reads the same geometry
    /// through it, so the drawn and hit rects cannot drift.
    fn view(&self) -> View<'_> {
        View {
            width: self.width,
            height: self.height,
            buf_w: self.width,
            buf_h: self.height,
            theme: self.theme,
            theme_idx: self.theme_idx,
            layout_idx: self.layout_idx,
            grid_cols: self.grid_cols,
            interaction: self.interaction,
            flash_key: self.flash_key,
            px: self.px,
            py: self.py,
            inside: self.inside,
            handle_anim: self.handle_anim(),
            theme_menu_open: self.theme_menu_open,
            ctl: &self.ctl,
            via: ViaFrame {
                chip: ViaChipLook::Hidden,
                status: None,
            },
            #[cfg(feature = "dictation")]
            dict_state: self.dict_state,
            #[cfg(feature = "dictation")]
            levels: self.wave_levels(),
        }
    }

    #[cfg(feature = "dictation")]
    fn wave_levels(&self) -> Vec<f32> {
        if self.dict_state == DictState::Recording {
            if let Some(rec) = self.recorder.as_ref() {
                return rec.levels(render::WAVE_BARS);
            }
        }
        Vec::new()
    }

    // ---- redraw ----

    /// Mark the window invalid; the message loop turns that into a
    /// `WM_PAINT`, which coalesces any number of requests.
    fn request_redraw(&self) {
        if self.hwnd != 0 {
            let _ = unsafe { InvalidateRect(Some(HWND(self.hwnd as *mut _)), None, false) };
        }
    }

    /// Paint right away: a tap shows its press without waiting for the
    /// message queue.
    fn redraw_now(&self) {
        self.request_redraw();
        if self.hwnd != 0 {
            let _ = unsafe { UpdateWindow(HWND(self.hwnd as *mut _)) };
        }
    }

    /// Draw the frame into the back buffer and copy it to `hdc`.
    fn paint_to(&mut self, hdc: *mut std::ffi::c_void) {
        if self.width <= 0 || self.height <= 0 {
            return;
        }
        let surface = match self.surface.take() {
            Some(s) if s.width() == self.width && s.height() == self.height => s,
            _ => match ImageSurface::create(Format::ARgb32, self.width, self.height) {
                Ok(s) => s,
                Err(e) => {
                    warn!("osk: back buffer {}x{}: {e}", self.width, self.height);
                    return;
                }
            },
        };
        let Some(bar) = self.draw(&surface) else {
            self.surface = Some(surface);
            return;
        };
        surface.blit_to_hdc(hdc);
        self.surface = Some(surface);
        self.sugg = bar.sugg;
        self.n_sugg = bar.n_sugg;
        self.layout_rect = bar.layout_rect;
        self.theme_rect = bar.theme_rect;
        self.close_rect = bar.close_rect;
        self.mic_rect = bar.mic_rect;
        self.theme_menu_rect = bar.theme_menu_rect;
        self.theme_item_rects = bar.theme_item_rects;
        self.tab_toggle_rect = bar.tab_toggle_rect;
        self.pred_toggle_rect = bar.pred_toggle_rect;
        self.last_hot = self.hot_sig();
    }

    /// Measure the bar and paint the view into `surface`.
    fn draw(&mut self, surface: &ImageSurface) -> Option<BarLayout> {
        let meas = Context::new(surface).ok()?;
        meas.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
        let bar = self.view().measure_bar(&meas);
        drop(meas);
        let cr = Context::new(surface).ok()?;
        // Out of self while the view borrows it.
        let mut cache = self.key_cache.take();
        self.view().paint(&cr, &bar, &mut cache);
        self.key_cache = cache;
        Some(bar)
    }

    // ---- input ----

    /// Hold Alt down in Windows exactly while the controller has it
    /// latched or locked, so other apps see it held (Alt+click, Alt+drag).
    fn sync_alt(&mut self) {
        let mods = self.ctl.mods();
        let want = mods.state(MOD_ALT).is_active().then(|| mods.code(MOD_ALT));
        let latched = mods.state(MOD_ALT) == ModKeyState::Latched;
        self.set_alt(want);
        self.watch_mouse(latched);
    }

    /// Watch for a mouse click in another window: it is the "next key" a
    /// latched Alt applies to, as in the Windows osk (Alt+click).
    fn watch_mouse(&mut self, on: bool) {
        if on == self.mouse_hook.is_some() || self.hwnd == 0 {
            return;
        }
        if let Some(hook) = self.mouse_hook.take() {
            let _ = unsafe { UnhookWindowsHookEx(hook) };
            return;
        }
        OSK_HWND.store(self.hwnd, Ordering::Relaxed);
        match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) } {
            Ok(hook) => self.mouse_hook = Some(hook),
            Err(e) => warn!("osk: no mouse hook, Alt+click keeps Alt down: {e}"),
        }
    }

    /// A mouse click elsewhere finished under a latched Alt: release it.
    fn on_click_elsewhere(&mut self) {
        let mods = self.ctl.mods();
        if mods.state(MOD_ALT) != ModKeyState::Latched {
            return;
        }
        // A click outside the double-click window takes the latch to off.
        let alt = windows_click(Key::momentary("alt", mods.code(MOD_ALT), 25));
        let mut sink = AltSink {
            b: self.backend.as_mut(),
            alt: &mut self.alt_down,
        };
        self.ctl.click_key(&alt, now_ms() + DOUBLE_CLICK_MS, &mut sink);
        self.sync_alt();
        self.request_redraw();
    }

    fn set_alt(&mut self, want: Option<u16>) {
        if want == self.alt_down {
            return;
        }
        let mut sink = AltSink {
            b: self.backend.as_mut(),
            alt: &mut self.alt_down,
        };
        if let Some(code) = *sink.alt {
            sink.release(code);
        }
        if let Some(code) = want {
            sink.press(code);
        }
    }

    fn tap_key(&mut self, idx: usize) {
        let k = *self.key_of(idx);
        let mut sink = AltSink {
            b: self.backend.as_mut(),
            alt: &mut self.alt_down,
        };
        self.ctl.tap_key(&k, &mut sink);
        self.sync_alt();
        self.redraw_now();
        self.save_history();
    }

    fn click_suggestion(&mut self, idx: usize) {
        let mut sink = AltSink {
            b: self.backend.as_mut(),
            alt: &mut self.alt_down,
        };
        self.ctl.commit_suggestion(idx, &mut sink).ok();
        self.sync_alt();
        self.save_history();
        self.request_redraw();
    }

    /// Feed one interaction event through the pure state machine and
    /// apply the side effects it reports (flash, geometry, config save).
    fn interact(&mut self, ev: IEvent) {
        let step = interaction::step(self.interaction, ev);
        if let Some(key) = step.flash {
            self.flash_key = key as i32;
            self.flash_until = now_ms() + FLASH_MS;
            self.request_redraw();
        }
        if step.apply_geometry {
            self.apply_geometry();
        }
        if step.save_config {
            self.save_config();
        }
        self.interaction = step.next;
    }

    /// The pointer in screen coordinates; drags and resizes follow it.
    fn cursor() -> (i32, i32) {
        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        (pt.x, pt.y)
    }

    fn on_motion(&mut self) {
        self.sync_handle_anim();
        // A repeat stops once the pointer leaves the repeating key, but
        // the key stays pressed (its held look is time-based).
        if let Interaction::Press {
            key,
            rep_next: Some(_),
            ..
        } = self.interaction
        {
            if self.key_at(self.px, self.py) != Some(key) {
                if let Interaction::Press { rep_next, .. } = &mut self.interaction {
                    *rep_next = None;
                }
            }
        }
        match self.interaction {
            Interaction::Move { .. } | Interaction::Resize { .. } => {
                self.drag_to(Self::cursor());
                return;
            }
            Interaction::Press { .. } | Interaction::Idle => {}
        }
        // Moving inside the same key does not change a pixel.
        let sig = self.hot_sig();
        if sig != self.last_hot {
            self.last_hot = sig;
            self.request_redraw();
        }
    }

    /// Follow the pointer at screen position `pt` while dragging or
    /// resizing. Both started from the screen position in the
    /// interaction's grab point.
    fn drag_to(&mut self, (x, y): (i32, i32)) {
        match self.interaction {
            Interaction::Move { grab_x, grab_y, .. } => {
                let nx = self.drag_origin.0 + (x - grab_x);
                let ny = self.drag_origin.1 + (y - grab_y);
                if nx != self.pos_x || ny != self.pos_y {
                    self.pos_x = nx;
                    self.pos_y = ny;
                    self.apply_geometry();
                }
            }
            Interaction::Resize {
                grab_x,
                grab_y,
                w,
                h,
            } => {
                // Stretch limit: past the key aspect's edge the window
                // stops growing in that direction.
                let (cw, ch) = Metrics::clamp_size(
                    (w + (x - grab_x)).max(MIN_W),
                    (h + (y - grab_y)).max(MIN_H),
                    self.grid_cols,
                    self.layout().rows.len(),
                );
                let (nw, nh) = (cw.max(MIN_W), ch.max(MIN_H));
                if nw != self.width || nh != self.height {
                    self.width = nw;
                    self.height = nh;
                    self.apply_geometry();
                    self.request_redraw();
                }
            }
            Interaction::Press { .. } | Interaction::Idle => {}
        }
    }

    /// Signature of the hovered elements, for redraw coalescing.
    fn hot_sig(&self) -> u32 {
        let at = |r: &Rect| u32::from(r.contains(self.px as f64, self.py as f64));
        let key = self.key_at(self.px, self.py).map_or(0x3ff, |k| k as u32);
        let sugg = (0..self.n_sugg)
            .find(|&i| self.sugg[i].contains(self.px as f64, self.py as f64))
            .map_or(0x1f, |i| i as u32);
        let menu_item = (0..12)
            .find(|&i| self.theme_item_rects[i].contains(self.px as f64, self.py as f64))
            .map_or(0xf, |i| i as u32);
        (key << 20)
            | (sugg << 15)
            | (at(&self.pred_toggle_rect) << 12)
            | (at(&self.tab_toggle_rect) << 11)
            | (at(&self.layout_rect) << 10)
            | (at(&self.theme_rect) << 9)
            | (u32::from(self.theme_menu_open) << 8)
            | (menu_item << 4)
            | (at(&self.mic_rect) << 2)
            | (at(&self.close_rect) << 1)
            | u32::from(self.view().handle_hov())
    }

    fn cycle_layout(&mut self) {
        self.interaction = Interaction::Idle;
        self.layout_idx = (self.layout_idx + 1) % ALL_LAYOUTS.len();
        self.grid_cols = render::grid_cols(self.layout());
        // The new grid wants its own stretch limit.
        let (w, h) = Metrics::clamp_size(
            self.width.max(MIN_W),
            self.height.max(MIN_H),
            self.grid_cols,
            self.layout().rows.len(),
        );
        self.width = w.max(MIN_W);
        self.height = h.max(MIN_H);
        self.apply_geometry();
        self.save_config();
        self.request_redraw();
    }

    // One pointer-press dispatcher: hit-test order mirrors the visual
    // layering, as on Linux.
    #[allow(clippy::too_many_lines)]
    fn on_button(&mut self, pressed: bool) {
        if !pressed {
            // Release the held key (flash it) or end and persist the drag.
            self.interact(IEvent::Release);
            return;
        }
        let (x, y) = (self.px as f64, self.py as f64);
        if self.theme_menu_open {
            if self.pred_toggle_rect.contains(x, y) {
                self.ctl.set_prediction(!self.ctl.prediction());
            } else if self.tab_toggle_rect.contains(x, y) {
                self.ctl.set_tab_accepts_prediction(!self.ctl.tab_accepts_prediction());
            } else if let Some(i) = (0..THEMES.len().min(12)).find(|&i| self.theme_item_rects[i].contains(x, y)) {
                self.theme_idx = i;
                self.theme = THEMES[i];
                self.theme_menu_open = false;
            } else {
                // The theme button again, or outside: close the menu.
                self.theme_menu_open = false;
                self.request_redraw();
                return;
            }
            self.save_config();
            self.request_redraw();
            return;
        }
        #[cfg(feature = "dictation")]
        if self.mic_rect.contains(x, y) {
            self.on_mic();
            return;
        }
        if self.close_rect.contains(x, y) {
            self.save_config();
            self.quit = true;
            return;
        }
        if self.theme_rect.contains(x, y) {
            self.theme_menu_open = true;
            self.request_redraw();
            return;
        }
        if self.layout_rect.contains(x, y) {
            self.cycle_layout();
            return;
        }
        if let Some(i) = (0..self.n_sugg).find(|&i| self.sugg[i].contains(x, y)) {
            self.click_suggestion(self.sugg[i].idx);
            return;
        }
        let (sx, sy) = Self::cursor();
        let hs = self.view().handle_size();
        if self.px >= self.width - hs && self.py >= self.height - hs {
            self.interact(IEvent::StartResize {
                x: sx,
                y: sy,
                w: self.width,
                h: self.height,
            });
            return;
        }
        if self.py < self.view().bar_h() as i32 {
            self.drag_origin = (self.pos_x, self.pos_y);
            self.interact(IEvent::StartMove { x: sx, y: sy });
            return;
        }
        let Some(k) = self.key_at(self.px, self.py) else {
            return;
        };
        // While dictation records, any key stops it and transcribes,
        // so the hand never travels back to the small mic button. The
        // key is the stop gesture only; nothing is typed.
        #[cfg(feature = "dictation")]
        if self.dict_state == DictState::Recording {
            self.on_mic();
            return;
        }
        let key = windows_click(*self.key_of(k));
        // All keyboard logic lives in the controller.
        let mut sink = AltSink {
            b: self.backend.as_mut(),
            alt: &mut self.alt_down,
        };
        let clicked = self.ctl.click_key(&key, now_ms(), &mut sink);
        self.sync_alt();
        match clicked {
            Clicked::Inert => {}
            Clicked::Momentary => {
                self.interact(IEvent::Press {
                    key: k,
                    repeat: false,
                    now: now_ms(),
                });
                self.redraw_now();
            }
            Clicked::Latch(..) | Clicked::Caps(_) => {
                self.flash_key = k as i32;
                self.flash_until = now_ms() + FLASH_MS;
                self.request_redraw();
            }
            Clicked::Tapped { .. } => {
                self.interact(IEvent::Press {
                    key: k,
                    repeat: key.repeat,
                    now: now_ms(),
                });
                self.redraw_now();
                self.save_history();
            }
        }
    }

    /// Right-click a suggestion pill to forget that word.
    fn on_right_click(&mut self) {
        let (x, y) = (self.px as f64, self.py as f64);
        let Some(i) = (0..self.n_sugg).find(|&i| self.sugg[i].contains(x, y)) else {
            return;
        };
        if let Some(&word) = self.ctl.suggestions().get(self.sugg[i].idx) {
            self.ctl.forget_word(word);
            self.save_history();
            self.request_redraw();
        }
    }

    /// The pointer left the window. During a drag or resize the window
    /// holds the mouse capture and moves under the pointer, so a leave
    /// then is not the end of the gesture.
    fn on_leave(&mut self) {
        if matches!(self.interaction, Interaction::Move { .. } | Interaction::Resize { .. }) {
            return;
        }
        self.inside = false;
        self.interact(IEvent::Leave);
        self.sync_handle_anim();
        self.request_redraw();
    }

    // ---- dictation ----

    /// Mic button: start or stop dictation.
    #[cfg(feature = "dictation")]
    fn on_mic(&mut self) {
        match self.dict_state {
            DictState::Idle => match crate::dictate::Recorder::start() {
                Ok(rec) => {
                    self.recorder = Some(rec);
                    self.dict_state = DictState::Recording;
                    self.dict_frame_due = now_ms();
                    self.request_redraw();
                }
                Err(e) => warn!("[dictate] {e}"),
            },
            DictState::Recording => {
                let Some(rec) = self.recorder.take() else {
                    self.dict_state = DictState::Idle;
                    self.request_redraw();
                    return;
                };
                let rate = rec.rate;
                let raw = rec.stop();
                let (tx, rx) = std::sync::mpsc::channel();
                self.dict_rx = Some(rx);
                self.dict_state = DictState::Transcribing;
                self.request_redraw();
                std::thread::spawn(move || {
                    let pcm = crate::dictate::resample_to_16k(&raw, rate as usize);
                    let _ = tx.send(crate::dictate::transcribe(&pcm));
                });
            }
            DictState::Transcribing => {}
        }
    }

    /// A transcription finished: type the text and return to idle.
    #[cfg(feature = "dictation")]
    fn finish_dictation(&mut self, result: Result<String, String>) {
        self.dict_state = DictState::Idle;
        self.dict_rx = None;
        self.request_redraw();
        match result {
            Ok(text) if !text.is_empty() => {
                if let Some(map) = self.ctl.charmap() {
                    let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
                    if let Err(e) = type_text(
                        &mut AltSink {
                            b: self.backend.as_mut(),
                            alt: &mut self.alt_down,
                        },
                        map,
                        &text,
                        pause,
                    ) {
                        warn!("[dictate] failed to type: {e:?}");
                    }
                    self.sync_alt();
                }
            }
            Ok(_) => {}
            Err(e) => warn!("[dictate] {e}"),
        }
    }

    // ---- timed work ----

    /// Everything the timer services: key repeat, the held look, the
    /// flash, the controller's deferred Win tap, the handle ease, the
    /// waveform and a finished transcription.
    fn tick(&mut self) {
        let now = now_ms();
        let repeat = match &mut self.interaction {
            Interaction::Press {
                key,
                rep_next: Some(next),
                ..
            } if now >= *next => {
                *next = now + 40;
                Some(*key)
            }
            _ => None,
        };
        if let Some(k) = repeat {
            self.tap_key(k);
        }
        if let Interaction::Press {
            press_time,
            hold_fired,
            ..
        } = &mut self.interaction
        {
            if !*hold_fired && now >= *press_time + interaction::HOLD_MS {
                *hold_fired = true;
                self.request_redraw();
            }
        }
        if self.flash_key >= 0 && now >= self.flash_until {
            self.flash_key = -1;
            self.request_redraw();
        }
        let mut sink = AltSink {
            b: self.backend.as_mut(),
            alt: &mut self.alt_down,
        };
        self.ctl.tick(now, &mut sink);
        self.sync_alt();
        if self.handle_anim_since.is_some_and(|s| now.saturating_sub(s) <= render::HANDLE_ANIM_MS + ANIM_FRAME_MS) {
            self.request_redraw();
        }
        #[cfg(feature = "dictation")]
        {
            if self.dict_state == DictState::Recording && now >= self.dict_frame_due {
                self.request_redraw();
                self.dict_frame_due = now + DICT_FRAME_MS;
            }
            let done = self.dict_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            if let Some(result) = done {
                self.finish_dictation(result);
            }
        }
        self.backend.pump();
    }

    /// Milliseconds until the timer has work, or None when nothing is
    /// due, so an idle keyboard arms no timer.
    fn next_wake(&self) -> Option<u64> {
        let now = now_ms();
        let mut due: Vec<u64> = Vec::new();
        if let Interaction::Press {
            press_time,
            hold_fired,
            rep_next,
            ..
        } = self.interaction
        {
            due.extend(rep_next);
            if !hold_fired {
                due.push(press_time + interaction::HOLD_MS);
            }
        }
        if self.flash_key >= 0 {
            due.push(self.flash_until);
        }
        due.extend(self.ctl.tick_due());
        if self.handle_anim_active() {
            due.push(now + ANIM_FRAME_MS);
        }
        #[cfg(feature = "dictation")]
        match self.dict_state {
            DictState::Recording => due.push(self.dict_frame_due),
            // Poll for the result while a transcription runs.
            DictState::Transcribing => due.push(now + 50),
            DictState::Idle => {}
        }
        due.into_iter().min().map(|d| d.saturating_sub(now).max(1))
    }

    /// Arm the timer for the next deadline, or stop it.
    fn arm_timer(&self) {
        if self.hwnd == 0 {
            return;
        }
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            match self.next_wake() {
                Some(ms) => {
                    SetTimer(Some(hwnd), TIMER_ID, ms.min(u64::from(u32::MAX)) as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(hwnd), TIMER_ID);
                }
            }
        }
    }
}

/// The window procedure reaches the state through this pointer; it is
/// set while [`run`] owns the state and the window exists.
static mut STATE_PTR: *mut State = std::ptr::null_mut();

fn lparam_xy(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xffff) as i16 as i32,
        ((lparam.0 >> 16) & 0xffff) as i16 as i32,
    )
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = unsafe { STATE_PTR };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };
    let result = match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            state.paint_to(hdc.0);
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        WM_CLICK_ELSEWHERE => {
            state.on_click_elsewhere();
            LRESULT(0)
        }
        WM_TIMER => {
            state.tick();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lparam);
            let was_inside = state.inside;
            state.px = x;
            state.py = y;
            state.inside = x >= 0 && x < state.width && y >= 0 && y < state.height;
            if !was_inside && state.inside {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = unsafe { TrackMouseEvent(&mut tme) };
            }
            state.on_motion();
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            state.on_leave();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            unsafe { SetCapture(hwnd) };
            (state.px, state.py) = lparam_xy(lparam);
            state.inside = true;
            state.on_button(true);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = unsafe { ReleaseCapture() };
            (state.px, state.py) = lparam_xy(lparam);
            state.on_button(false);
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            (state.px, state.py) = lparam_xy(lparam);
            state.on_right_click();
            LRESULT(0)
        }
        WM_SIZE => {
            let w = (lparam.0 & 0xffff) as i32;
            let h = ((lparam.0 >> 16) & 0xffff) as i32;
            if w > 0 && h > 0 && (w != state.width || h != state.height) {
                state.width = w;
                state.height = h;
            }
            state.request_redraw();
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // The resize corner shows the diagonal resize cursor.
            let shape = if state.view().handle_hov() { IDC_SIZENWSE } else { IDC_ARROW };
            if let Ok(c) = unsafe { LoadCursorW(None, shape) } {
                unsafe { SetCursor(Some(c)) };
            }
            LRESULT(1)
        }
        WM_DESTROY => {
            state.quit = true;
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    if state.quit && msg != WM_DESTROY {
        let _ = unsafe { DestroyWindow(hwnd) };
    } else {
        state.arm_timer();
    }
    result
}

/// The Win32 UI. Runs until the keyboard is closed.
pub fn run(backend: Box<dyn Backend>, neural: Option<NeuralEngine>) -> i32 {
    let mut state = Box::new(State::new(backend, neural));
    let class_name: Vec<u16> = "osk-win-window\0".encode_utf16().collect();
    let title: Vec<u16> = "OSK\0".encode_utf16().collect();
    let hinst = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            warn!("osk: GetModuleHandleW failed: {e}");
            return 1;
        }
    };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE(hinst.0),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } == 0 {
        warn!("osk: RegisterClassExW failed");
        return 1;
    }
    unsafe { STATE_PTR = &mut *state as *mut State };
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
            unsafe { STATE_PTR = std::ptr::null_mut() };
            warn!("osk: CreateWindowExW failed: {e}");
            return 1;
        }
    };
    state.hwnd = hwnd.0 as isize;
    state.last_geom = (state.pos_x, state.pos_y, state.width, state.height);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    unsafe { STATE_PTR = std::ptr::null_mut() };
    state.save_config();
    state.save_history();
    // Never leave Alt stuck down in Windows.
    state.watch_mouse(false);
    state.set_alt(None);
    state.backend.destroy();
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use osk_core::key::key;
    use osk_core::mods::{ModKeyState, DOUBLE_CLICK_MS, MOD_META};

    /// Counts the key presses it is asked to inject, and logs every
    /// event as (code, down).
    struct MockBackend(std::rc::Rc<std::cell::Cell<usize>>, Events);

    type Events = std::rc::Rc<std::cell::RefCell<Vec<(u16, bool)>>>;

    impl Backend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn init(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn press(&mut self, code: u16) {
            self.0.set(self.0.get() + 1);
            self.1.borrow_mut().push((code, true));
        }
        fn release(&mut self, code: u16) {
            self.1.borrow_mut().push((code, false));
        }
        fn pump(&mut self) {}
        fn destroy(&mut self) {}
    }

    fn state() -> State {
        state_counting(Default::default())
    }

    fn state_counting(presses: std::rc::Rc<std::cell::Cell<usize>>) -> State {
        state_with(presses, Default::default())
    }

    fn state_logging(events: Events) -> State {
        state_with(Default::default(), events)
    }

    fn state_with(presses: std::rc::Rc<std::cell::Cell<usize>>, events: Events) -> State {
        let mut s = State::new(Box::new(MockBackend(presses, events)), None);
        s.width = 800;
        s.height = 300;
        s
    }

    /// Point the pointer at the center of the key with code `code`.
    fn point_at(s: &mut State, code: u16) -> usize {
        let layout = s.layout();
        let idx = layout
            .rows
            .iter()
            .enumerate()
            .find_map(|(r, row)| {
                row.keys.iter().position(|k| k.code_u16() == code).map(|i| r * 100 + i)
            })
            .expect("key in the layout");
        let (x, y, w, h) = s.view().key_rect(idx / 100, idx % 100);
        s.px = (x + w / 2.0) as i32;
        s.py = (y + h / 2.0) as i32;
        s.inside = true;
        idx
    }

    #[test]
    fn win_is_a_momentary_tap_not_a_latch() {
        let mut s = state();
        let idx = point_at(&mut s, key::KEY_LEFTMETA);
        s.on_button(true);
        assert_eq!(s.ctl.mods().state(MOD_META), ModKeyState::Off, "Win must not latch");
        assert!(matches!(s.interaction, Interaction::Press { key, .. } if key == idx));
        s.on_button(false);
        assert_eq!(s.flash_key, idx as i32);
        assert_eq!(s.interaction, Interaction::Idle);
    }

    fn click(s: &mut State, code: u16) {
        point_at(s, code);
        s.on_button(true);
        s.on_button(false);
    }

    #[test]
    fn alt_is_held_down_until_the_next_key() {
        // Like the Windows osk: Alt goes down at once and stays down,
        // never a lone tap (that would open the focused app's menu bar).
        let events = Events::default();
        let mut s = state_logging(events.clone());
        let alt = key::KEY_LEFTALT;
        click(&mut s, alt);
        assert_eq!(s.ctl.mods().state(MOD_ALT), ModKeyState::Latched);
        assert_eq!(*events.borrow(), [(alt, true)], "Alt is down and stays down");
        // The next key goes out under Alt, which is then released.
        click(&mut s, key::KEY_TAB);
        assert_eq!(s.ctl.mods().state(MOD_ALT), ModKeyState::Off);
        let ev = events.borrow();
        assert!(ev.contains(&(key::KEY_TAB, true)));
        assert_eq!(ev.last(), Some(&(alt, false)), "Alt released after the key");
        assert_eq!(s.alt_down, None);
    }

    #[test]
    fn clicking_alt_again_releases_it() {
        let events = Events::default();
        let mut s = state_logging(events.clone());
        let alt = key::KEY_LEFTALT;
        click(&mut s, alt);
        std::thread::sleep(std::time::Duration::from_millis(DOUBLE_CLICK_MS + 50));
        click(&mut s, alt);
        assert_eq!(*events.borrow(), [(alt, true), (alt, false)]);
        assert_eq!(s.alt_down, None);
    }

    #[test]
    fn a_click_elsewhere_releases_a_latched_alt() {
        let events = Events::default();
        let mut s = state_logging(events.clone());
        let alt = key::KEY_LEFTALT;
        click(&mut s, alt);
        s.on_click_elsewhere();
        assert_eq!(*events.borrow(), [(alt, true), (alt, false)]);
        assert_eq!(s.ctl.mods().state(MOD_ALT), ModKeyState::Off);
        // The release is no click on the key: a new click latches again.
        click(&mut s, alt);
        assert_eq!(s.ctl.mods().state(MOD_ALT), ModKeyState::Latched);
    }

    #[test]
    fn a_click_elsewhere_keeps_a_locked_alt_down() {
        let events = Events::default();
        let mut s = state_logging(events.clone());
        let alt = key::KEY_LEFTALT;
        click(&mut s, alt);
        click(&mut s, alt);
        s.on_click_elsewhere();
        assert_eq!(s.ctl.mods().state(MOD_ALT), ModKeyState::Locked);
        assert_eq!(s.alt_down, Some(alt));
    }

    #[test]
    fn a_locked_alt_stays_down_across_keys() {
        let events = Events::default();
        let mut s = state_logging(events.clone());
        let alt = key::KEY_LEFTALT;
        click(&mut s, alt);
        click(&mut s, alt);
        assert_eq!(s.ctl.mods().state(MOD_ALT), ModKeyState::Locked);
        click(&mut s, key::KEY_TAB);
        assert_eq!(s.alt_down, Some(alt), "still held after the key");
        assert_eq!(events.borrow().last(), Some(&(alt, true)));
    }

    #[test]
    fn a_drag_and_a_resize_follow_the_pointer_in_screen_coordinates() {
        let mut s = state();
        (s.pos_x, s.pos_y) = (100, 100);
        s.drag_origin = (100, 100);
        s.interaction = Interaction::Move {
            grab_x: 150,
            grab_y: 120,
            fx: 0.0,
            fy: 0.0,
        };
        s.drag_to((180, 140));
        assert_eq!((s.pos_x, s.pos_y), (130, 120));

        s.interaction = Interaction::Resize {
            grab_x: 750,
            grab_y: 420,
            w: 600,
            h: 300,
        };
        s.drag_to((780, 440));
        let (w, h) = Metrics::clamp_size(630, 320, s.grid_cols, s.layout().rows.len());
        assert_eq!((s.width, s.height), (w.max(MIN_W), h.max(MIN_H)));
    }

    #[cfg(feature = "dictation")]
    #[test]
    fn a_key_stops_recording_instead_of_typing() {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut s = state_counting(presses.clone());
        s.dict_state = DictState::Recording;
        point_at(&mut s, key::KEY_A);
        s.on_button(true);
        s.on_button(false);
        assert!(s.dict_state != DictState::Recording, "the key ended the recording");
        assert_eq!(presses.get(), 0, "the key typed nothing");
        assert_eq!(s.interaction, Interaction::Idle);
        // Once it is over, the same key types again.
        point_at(&mut s, key::KEY_A);
        s.on_button(true);
        assert_eq!(presses.get(), 1);
    }

    #[test]
    fn nothing_is_due_at_rest() {
        let s = state();
        assert_eq!(s.next_wake(), None);
    }

    #[test]
    fn persistent_config_round_trips() {
        let cfg = PersistentConfig {
            pos_x: 250,
            pos_y: 350,
            width: 750,
            height: 280,
            theme: "Nord".to_string(),
            layout: "65%+Num".to_string(),
            tab_accepts_prediction: true,
            prediction: false,
        };
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let loaded: PersistentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!((loaded.pos_x, loaded.pos_y, loaded.width, loaded.height), (250, 350, 750, 280));
        assert_eq!((loaded.theme.as_str(), loaded.layout.as_str()), ("Nord", "65%+Num"));
        assert!(loaded.tab_accepts_prediction && !loaded.prediction);
    }
}
