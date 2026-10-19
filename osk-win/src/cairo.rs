//! The subset of the cairo API that `render.rs` draws with, on GDI.
//!
//! `render.rs` is copied unchanged from the Linux osk, where it paints
//! with cairo; this module gives it the same names and semantics on a
//! GDI memory DC, so a sync stays a plain copy. Paths map onto GDI
//! paths (arcs flattened to line segments), fills and strokes onto
//! `FillPath` and `StrokePath`, a clip onto `SelectClipPath` with
//! `RGN_AND` (it narrows the current clip, as cairo's does), and text
//! is placed by its baseline, as cairo places it.
//!
//! GDI draws opaque and without anti-aliasing: alpha and the compositing
//! operator are accepted and ignored.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::ffi::c_void;
use std::io::Write;
use std::ptr;
use std::rc::Rc;

type Hdc = *mut c_void;
type Hgdiobj = *mut c_void;

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_ppm: i32,
    y_ppm: i32,
    clr_used: u32,
    clr_important: u32,
}

#[repr(C)]
struct GdiPoint {
    x: i32,
    y: i32,
}

#[repr(C)]
struct LogBrush {
    style: u32,
    color: u32,
    hatch: usize,
}

#[repr(C)]
#[derive(Default)]
struct TextMetricW {
    height: i32,
    ascent: i32,
    descent: i32,
    internal_leading: i32,
    external_leading: i32,
    ave_char_width: i32,
    max_char_width: i32,
    weight: i32,
    overhang: i32,
    digitized_aspect_x: i32,
    digitized_aspect_y: i32,
    first_char: u16,
    last_char: u16,
    default_char: u16,
    break_char: u16,
    italic: u8,
    underlined: u8,
    struck_out: u8,
    pitch_and_family: u8,
    char_set: u8,
}

const SRCCOPY: u32 = 0x00CC_0020;
const TRANSPARENT: i32 = 1;
const WINDING: i32 = 2;
const RGN_AND: i32 = 1;
const TA_BASELINE: u32 = 24;
const BS_SOLID: u32 = 0;
const PS_GEOMETRIC: u32 = 0x0001_0000;
const PS_SOLID: u32 = 0;
const PS_ENDCAP_ROUND: u32 = 0;
const PS_ENDCAP_FLAT: u32 = 0x0000_0200;
const PS_JOIN_ROUND: u32 = 0;
const FW_NORMAL: i32 = 400;
const FW_BOLD: i32 = 700;
const DEFAULT_CHARSET: u32 = 1;
const ANTIALIASED_QUALITY: u32 = 4;

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hdc: Hdc) -> Hdc;
    fn DeleteDC(hdc: Hdc) -> i32;
    fn CreateDIBSection(
        hdc: Hdc,
        info: *const BitmapInfoHeader,
        usage: u32,
        bits: *mut *mut c_void,
        section: *mut c_void,
        offset: u32,
    ) -> Hgdiobj;
    fn SelectObject(hdc: Hdc, obj: Hgdiobj) -> Hgdiobj;
    fn DeleteObject(obj: Hgdiobj) -> i32;
    fn GetStockObject(i: i32) -> Hgdiobj;
    fn BitBlt(
        dst: Hdc,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        src: Hdc,
        sx: i32,
        sy: i32,
        rop: u32,
    ) -> i32;
    fn GdiFlush() -> i32;
    fn SetBkMode(hdc: Hdc, mode: i32) -> i32;
    fn SetPolyFillMode(hdc: Hdc, mode: i32) -> i32;
    fn SetTextAlign(hdc: Hdc, align: u32) -> u32;
    fn SetTextColor(hdc: Hdc, color: u32) -> u32;
    fn BeginPath(hdc: Hdc) -> i32;
    fn EndPath(hdc: Hdc) -> i32;
    fn AbortPath(hdc: Hdc) -> i32;
    fn MoveToEx(hdc: Hdc, x: i32, y: i32, old: *mut GdiPoint) -> i32;
    fn PolylineTo(hdc: Hdc, pts: *const GdiPoint, n: u32) -> i32;
    fn CloseFigure(hdc: Hdc) -> i32;
    fn FillPath(hdc: Hdc) -> i32;
    fn StrokePath(hdc: Hdc) -> i32;
    fn SelectClipPath(hdc: Hdc, mode: i32) -> i32;
    fn SelectClipRgn(hdc: Hdc, rgn: Hgdiobj) -> i32;
    fn SaveDC(hdc: Hdc) -> i32;
    fn RestoreDC(hdc: Hdc, saved: i32) -> i32;
    fn CreateSolidBrush(color: u32) -> Hgdiobj;
    fn ExtCreatePen(style: u32, width: u32, brush: *const LogBrush, n: u32, styles: *const u32)
        -> Hgdiobj;
    fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strikeout: u32,
        charset: u32,
        out_precision: u32,
        clip_precision: u32,
        quality: u32,
        pitch_and_family: u32,
        face: *const u16,
    ) -> Hgdiobj;
    fn TextOutW(hdc: Hdc, x: i32, y: i32, s: *const u16, n: i32) -> i32;
    fn GetTextExtentPoint32W(hdc: Hdc, s: *const u16, n: i32, size: *mut GdiPoint) -> i32;
    fn GetTextMetricsW(hdc: Hdc, tm: *mut TextMetricW) -> i32;
}

const NULL_PEN: i32 = 8;
const NULL_BRUSH: i32 = 5;

/// A failed GDI call. cairo's calls return `Result`; render.rs ignores
/// most of them with `let _ =`.
#[derive(Debug, Clone, Copy)]
pub struct Error;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GDI drawing failed")
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Format {
    ARgb32,
}

#[derive(Clone, Copy, Debug)]
pub enum FontSlant {
    Normal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontWeight {
    Normal,
    Bold,
}

#[derive(Clone, Copy, Debug)]
pub enum Operator {
    Source,
    Over,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
}

/// Measured text, in the same frame cairo reports it: the bearings put
/// the ink box relative to the baseline origin.
#[derive(Clone, Copy, Debug)]
pub struct TextExtents {
    width: f64,
    height: f64,
    y_bearing: f64,
}

impl TextExtents {
    pub fn width(&self) -> f64 {
        self.width
    }
    pub fn height(&self) -> f64 {
        self.height
    }
    pub fn x_bearing(&self) -> f64 {
        0.0
    }
    pub fn y_bearing(&self) -> f64 {
        self.y_bearing
    }
}

/// The pixels and the DC they are selected into, plus the fonts made
/// for that DC.
struct Surface {
    dc: Hdc,
    bitmap: Hgdiobj,
    old_bitmap: Hgdiobj,
    pixels: *mut u8,
    w: i32,
    h: i32,
    /// Fonts by (pixel height x 4, bold); a frame sets the size for
    /// every label, and making a GDI font each time is slow.
    fonts: RefCell<HashMap<(i32, bool), Hgdiobj>>,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, GetStockObject(17 /* DEFAULT_GUI_FONT */));
            for (_, f) in self.fonts.borrow_mut().drain() {
                DeleteObject(f);
            }
            SelectObject(self.dc, self.old_bitmap);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

/// A 32-bit image, like cairo's ARGB32 image surface.
#[derive(Clone)]
pub struct ImageSurface(Rc<Surface>);

impl ImageSurface {
    pub fn create(_format: Format, w: i32, h: i32) -> Result<Self, Error> {
        let (w, h) = (w.max(1), h.max(1));
        let info = BitmapInfoHeader {
            size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            width: w,
            // Negative: rows top-down, like cairo's.
            height: -h,
            planes: 1,
            bit_count: 32,
            compression: 0,
            size_image: 0,
            x_ppm: 0,
            y_ppm: 0,
            clr_used: 0,
            clr_important: 0,
        };
        let mut pixels: *mut c_void = ptr::null_mut();
        let bitmap =
            unsafe { CreateDIBSection(ptr::null_mut(), &info, 0, &mut pixels, ptr::null_mut(), 0) };
        if bitmap.is_null() || pixels.is_null() {
            return Err(Error);
        }
        let dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
        if dc.is_null() {
            unsafe { DeleteObject(bitmap) };
            return Err(Error);
        }
        let old_bitmap = unsafe { SelectObject(dc, bitmap) };
        unsafe {
            SetBkMode(dc, TRANSPARENT);
            SetPolyFillMode(dc, WINDING);
            SetTextAlign(dc, TA_BASELINE);
        }
        crate::gdip::ensure_embedded_font();
        Ok(Self(Rc::new(Surface {
            dc,
            bitmap,
            old_bitmap,
            pixels: pixels as *mut u8,
            w,
            h,
            fonts: RefCell::new(HashMap::new()),
        })))
    }

    pub fn width(&self) -> i32 {
        self.0.w
    }

    pub fn height(&self) -> i32 {
        self.0.h
    }

    pub fn flush(&self) {
        unsafe { GdiFlush() };
    }

    /// Copy the image onto a window's device context at (0, 0).
    pub fn blit_to_hdc(&self, hdc: *mut c_void) {
        self.flush();
        unsafe { BitBlt(hdc, 0, 0, self.0.w, self.0.h, self.0.dc, 0, 0, SRCCOPY) };
    }

    /// Write the image as a PNG (8-bit RGB, stored deflate blocks).
    pub fn write_to_png<W: Write>(&self, out: &mut W) -> Result<(), Error> {
        self.flush();
        let (w, h) = (self.0.w as usize, self.0.h as usize);
        let mut raw = Vec::with_capacity(h * (1 + 3 * w));
        let pixels = unsafe { std::slice::from_raw_parts(self.0.pixels, w * h * 4) };
        for row in pixels.chunks(w * 4) {
            raw.push(0);
            for px in row.chunks(4) {
                raw.extend_from_slice(&[px[2], px[1], px[0]]);
            }
        }
        png::write(out, w as u32, h as u32, &raw).map_err(|_| Error)
    }
}

/// One subpath in device pixels.
#[derive(Default)]
struct SubPath {
    pts: Vec<(f64, f64)>,
    closed: bool,
}

#[derive(Clone, Copy)]
struct Gstate {
    color: u32,
    line_width: f64,
    line_cap: LineCap,
    sx: f64,
    sy: f64,
    font_size: f64,
    bold: bool,
    source: bool,
}

struct State {
    g: Gstate,
    stack: Vec<(Gstate, i32)>,
    path: Vec<SubPath>,
    /// The current point in device pixels.
    cur: Option<(f64, f64)>,
    source: Option<ImageSurface>,
}

/// A drawing context on an [`ImageSurface`], like cairo's.
pub struct Context {
    surface: ImageSurface,
    st: RefCell<State>,
}

impl Context {
    pub fn new(surface: &ImageSurface) -> Result<Self, Error> {
        let dc = surface.0.dc;
        // A fresh context starts unclipped, like cairo's.
        unsafe { SelectClipRgn(dc, ptr::null_mut()) };
        Ok(Self {
            surface: surface.clone(),
            st: RefCell::new(State {
                g: Gstate {
                    color: 0,
                    line_width: 2.0,
                    line_cap: LineCap::Butt,
                    sx: 1.0,
                    sy: 1.0,
                    font_size: 10.0,
                    bold: false,
                    source: false,
                },
                stack: Vec::new(),
                path: Vec::new(),
                cur: None,
                source: None,
            }),
        })
    }

    fn dc(&self) -> Hdc {
        self.surface.0.dc
    }

    fn device(&self, x: f64, y: f64) -> (f64, f64) {
        let g = self.st.borrow().g;
        (x * g.sx, y * g.sy)
    }

    // ---- state ----

    pub fn save(&self) -> Result<(), Error> {
        let token = unsafe { SaveDC(self.dc()) };
        let mut st = self.st.borrow_mut();
        let g = st.g;
        st.stack.push((g, token));
        Ok(())
    }

    pub fn restore(&self) -> Result<(), Error> {
        let mut st = self.st.borrow_mut();
        if let Some((g, token)) = st.stack.pop() {
            st.g = g;
            unsafe { RestoreDC(self.dc(), token) };
        }
        Ok(())
    }

    pub fn identity_matrix(&self) {
        let mut st = self.st.borrow_mut();
        st.g.sx = 1.0;
        st.g.sy = 1.0;
    }

    pub fn scale(&self, sx: f64, sy: f64) {
        let mut st = self.st.borrow_mut();
        st.g.sx *= sx;
        st.g.sy *= sy;
    }

    pub fn set_operator(&self, _op: Operator) {}

    pub fn set_source_rgba(&self, r: f64, g: f64, b: f64, _a: f64) {
        let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        let mut st = self.st.borrow_mut();
        st.g.color = (to8(b) << 16) | (to8(g) << 8) | to8(r);
        st.g.source = false;
        st.source = None;
    }

    /// Make `surface` the source: the next `paint` copies it, placed
    /// at (x, y) in device pixels.
    pub fn set_source_surface(&self, surface: &ImageSurface, _x: f64, _y: f64) -> Result<(), Error> {
        let mut st = self.st.borrow_mut();
        st.source = Some(surface.clone());
        st.g.source = true;
        Ok(())
    }

    pub fn set_line_width(&self, w: f64) {
        self.st.borrow_mut().g.line_width = w;
    }

    pub fn set_line_cap(&self, cap: LineCap) {
        self.st.borrow_mut().g.line_cap = cap;
    }

    // ---- paths ----

    pub fn new_path(&self) {
        let mut st = self.st.borrow_mut();
        st.path.clear();
        st.cur = None;
    }

    pub fn new_sub_path(&self) {
        self.st.borrow_mut().cur = None;
    }

    pub fn move_to(&self, x: f64, y: f64) {
        let p = self.device(x, y);
        let mut st = self.st.borrow_mut();
        st.path.push(SubPath {
            pts: vec![p],
            closed: false,
        });
        st.cur = Some(p);
    }

    fn line_to_device(&self, p: (f64, f64)) {
        let mut st = self.st.borrow_mut();
        if st.cur.is_none() || st.path.last().is_none_or(|s| s.closed) {
            st.path.push(SubPath::default());
        }
        if let Some(sub) = st.path.last_mut() {
            sub.pts.push(p);
        }
        st.cur = Some(p);
    }

    pub fn line_to(&self, x: f64, y: f64) {
        self.line_to_device(self.device(x, y));
    }

    /// An arc from `a1` to `a2` radians, increasing like cairo's. With
    /// a current point, a line joins it to the arc's start.
    pub fn arc(&self, cx: f64, cy: f64, r: f64, a1: f64, a2: f64) {
        let mut a2 = a2;
        while a2 < a1 {
            a2 += 2.0 * PI;
        }
        let (sx, sy) = {
            let g = self.st.borrow().g;
            (g.sx, g.sy)
        };
        let span = a2 - a1;
        let steps = ((span * r * sx.max(sy)).ceil() as usize).clamp(2, 96);
        for i in 0..=steps {
            let a = a1 + span * i as f64 / steps as f64;
            self.line_to_device(((cx + r * a.cos()) * sx, (cy + r * a.sin()) * sy));
        }
    }

    pub fn close_path(&self) {
        let mut st = self.st.borrow_mut();
        if let Some(sub) = st.path.last_mut() {
            sub.closed = true;
        }
    }

    pub fn rectangle(&self, x: f64, y: f64, w: f64, h: f64) {
        self.move_to(x, y);
        self.line_to(x + w, y);
        self.line_to(x + w, y + h);
        self.line_to(x, y + h);
        self.close_path();
    }

    /// Hand the recorded path to GDI as its current path.
    fn emit_path(&self) {
        let dc = self.dc();
        let st = self.st.borrow();
        unsafe {
            BeginPath(dc);
            for sub in &st.path {
                let Some(&(x0, y0)) = sub.pts.first() else {
                    continue;
                };
                MoveToEx(dc, x0.round() as i32, y0.round() as i32, ptr::null_mut());
                let rest: Vec<GdiPoint> = sub.pts[1..]
                    .iter()
                    .map(|&(x, y)| GdiPoint {
                        x: x.round() as i32,
                        y: y.round() as i32,
                    })
                    .collect();
                if !rest.is_empty() {
                    PolylineTo(dc, rest.as_ptr(), rest.len() as u32);
                }
                if sub.closed {
                    CloseFigure(dc);
                }
            }
            EndPath(dc);
        }
    }

    pub fn fill(&self) -> Result<(), Error> {
        self.emit_path();
        let dc = self.dc();
        let color = self.st.borrow().g.color;
        unsafe {
            let brush = CreateSolidBrush(color);
            let old_brush = SelectObject(dc, brush);
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            FillPath(dc);
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            DeleteObject(brush);
        }
        self.new_path();
        Ok(())
    }

    pub fn stroke(&self) -> Result<(), Error> {
        self.emit_path();
        let dc = self.dc();
        let g = self.st.borrow().g;
        let width = (g.line_width * g.sx.max(g.sy)).round().max(1.0) as u32;
        let cap = match g.line_cap {
            LineCap::Round => PS_ENDCAP_ROUND,
            LineCap::Butt => PS_ENDCAP_FLAT,
        };
        let brush = LogBrush {
            style: BS_SOLID,
            color: g.color,
            hatch: 0,
        };
        unsafe {
            let pen = ExtCreatePen(
                PS_GEOMETRIC | PS_SOLID | cap | PS_JOIN_ROUND,
                width,
                &brush,
                0,
                ptr::null(),
            );
            let old_pen = SelectObject(dc, pen);
            let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
            StrokePath(dc);
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            DeleteObject(pen);
        }
        self.new_path();
        Ok(())
    }

    /// Narrow the clip to the current path, as cairo's `clip` does.
    pub fn clip(&self) {
        self.emit_path();
        unsafe { SelectClipPath(self.dc(), RGN_AND) };
        self.new_path();
    }

    pub fn reset_clip(&self) {
        unsafe { SelectClipRgn(self.dc(), ptr::null_mut()) };
    }

    /// Fill the clip with the source color, or copy the source surface.
    pub fn paint(&self) -> Result<(), Error> {
        let dc = self.dc();
        let (w, h) = (self.surface.0.w, self.surface.0.h);
        let source = self.st.borrow().source.clone();
        if let Some(src) = source {
            src.flush();
            unsafe { BitBlt(dc, 0, 0, w, h, src.0.dc, 0, 0, SRCCOPY) };
            return Ok(());
        }
        unsafe { AbortPath(dc) };
        self.new_path();
        let g = self.st.borrow().g;
        self.rectangle(0.0, 0.0, f64::from(w) / g.sx, f64::from(h) / g.sy);
        self.fill()
    }

    // ---- text ----

    pub fn select_font_face(&self, _family: &str, _slant: FontSlant, weight: FontWeight) {
        self.st.borrow_mut().g.bold = weight == FontWeight::Bold;
    }

    pub fn set_font_size(&self, size: f64) {
        self.st.borrow_mut().g.font_size = size;
    }

    /// Select the font for the current size and weight; returns the
    /// device-to-user scale for measurements.
    fn use_font(&self) -> f64 {
        let g = self.st.borrow().g;
        let px = g.font_size * g.sy;
        let key = ((px * 4.0).round() as i32, g.bold);
        let surface = &self.surface.0;
        let mut fonts = surface.fonts.borrow_mut();
        let font = *fonts.entry(key).or_insert_with(|| {
            let face: Vec<u16> = "Adwaita Sans\0".encode_utf16().collect();
            unsafe {
                CreateFontW(
                    // Negative: the em height, which is what cairo's
                    // font size means.
                    -(px.round().max(1.0) as i32),
                    0,
                    0,
                    0,
                    if g.bold { FW_BOLD } else { FW_NORMAL },
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    0,
                    0,
                    ANTIALIASED_QUALITY,
                    0,
                    face.as_ptr(),
                )
            }
        });
        unsafe { SelectObject(surface.dc, font) };
        g.sy
    }

    pub fn text_extents(&self, s: &str) -> Result<TextExtents, Error> {
        let sy = self.use_font();
        let sx = self.st.borrow().g.sx;
        let text: Vec<u16> = s.encode_utf16().collect();
        let mut size = GdiPoint { x: 0, y: 0 };
        let mut tm = TextMetricW::default();
        let ok = unsafe {
            GetTextExtentPoint32W(self.dc(), text.as_ptr(), text.len() as i32, &mut size) != 0
                && GetTextMetricsW(self.dc(), &mut tm) != 0
        };
        if !ok {
            return Err(Error);
        }
        // GDI measures the cell (ascent + descent); reporting it with
        // the ascent as the bearing centers the cell where cairo would
        // center the ink.
        Ok(TextExtents {
            width: f64::from(size.x) / sx,
            height: f64::from(tm.ascent + tm.descent) / sy,
            y_bearing: -f64::from(tm.ascent) / sy,
        })
    }

    pub fn show_text(&self, s: &str) -> Result<(), Error> {
        self.use_font();
        let (x, y) = self.st.borrow().cur.unwrap_or((0.0, 0.0));
        let color = self.st.borrow().g.color;
        let text: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            SetTextColor(self.dc(), color);
            TextOutW(self.dc(), x.round() as i32, y.round() as i32, text.as_ptr(), text.len() as i32);
        }
        Ok(())
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let dc = self.dc();
        let st = self.st.get_mut();
        while let Some((_, token)) = st.stack.pop() {
            unsafe { RestoreDC(dc, token) };
        }
        unsafe { SelectClipRgn(dc, ptr::null_mut()) };
    }
}

/// A minimal PNG encoder: 8-bit RGB rows, stored (uncompressed)
/// deflate blocks. Enough for `--render` screenshots without a crate.
mod png {
    use std::io::{self, Write};

    fn crc32(bytes: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in bytes {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }

    fn chunk<W: Write>(out: &mut W, kind: &[u8; 4], data: &[u8]) -> io::Result<()> {
        out.write_all(&(data.len() as u32).to_be_bytes())?;
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.write_all(&body)?;
        out.write_all(&crc32(&body).to_be_bytes())
    }

    pub fn write<W: Write>(out: &mut W, w: u32, h: u32, raw: &[u8]) -> io::Result<()> {
        out.write_all(b"\x89PNG\r\n\x1a\n")?;
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(out, b"IHDR", &ihdr)?;
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
        for (i, block) in blocks.iter().enumerate() {
            z.push(u8::from(i + 1 == blocks.len()));
            let n = block.len() as u16;
            z.extend_from_slice(&n.to_le_bytes());
            z.extend_from_slice(&(!n).to_le_bytes());
            z.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in raw {
            a = (a + u32::from(x)) % 65_521;
            b = (b + a) % 65_521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        chunk(out, b"IDAT", &z)?;
        chunk(out, b"IEND", &[])
    }
}
