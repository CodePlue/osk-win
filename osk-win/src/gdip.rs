//! Thin GDI wrapper that exposes a Cairo-like drawing surface.
//!
//! The UI paint code was written against `cairo::Context`. This module
//! provides a `Canvas` whose method names and signatures match the subset of
//! the Cairo API the UI uses, backed by GDI drawing into a 32-bit back buffer
//! (a memory DC + DIB section). GDI is used instead of GDI+ because wine's
//! GDI+ is incomplete; the OSK's drawing (rounded rects + text) is well
//! covered by GDI.

use std::ffi::c_void;
use std::ptr;

type HDC = *mut c_void;
type HGDIOBJ = *mut c_void;
type HFONT = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BITMAPINFOHEADER {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_xpels_per_meter: i32,
    bi_ypels_per_meter: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

#[repr(C)]
struct BITMAPINFO {
    bmi_header: BITMAPINFOHEADER,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LOGFONTW {
    lf_height: i32,
    lf_width: i32,
    lf_escapement: i32,
    lf_orientation: i32,
    lf_weight: i32,
    lf_italic: u8,
    lf_underline: u8,
    lf_strike: u8,
    lf_charset: u8,
    lf_outtype: u8,
    lf_cliptype: u8,
    lf_quality: u8,
    lf_pitchandfamily: u8,
    lf_face_name: [u16; 32],
}

const TRANSPARENT: u32 = 1;
const SRCCOPY: u32 = 0x00CC0020;
const BI_RGB: u32 = 0;
const DIB_RGB_COLORS: u32 = 0;
const PS_SOLID: i32 = 0;
const NULL_PEN: i32 = 5;
const FW_NORMAL: i32 = 400;
const FW_BOLD: i32 = 700;
const DEFAULT_CHARSET: u8 = 1;
const OUT_DEFAULT_PRECIS: u8 = 0;
const CLIP_DEFAULT_PRECIS: u8 = 0;
const DEFAULT_QUALITY: u8 = 0;
const DEFAULT_PITCH: u8 = 0;
const OBJ_FONT: i32 = 7;
const NULL_BRUSH: i32 = 5;

/// A shape being built before fill/stroke.
#[derive(Clone, Copy, Default)]
enum Shape {
    #[default]
    None,
    Rect { x: i32, y: i32, w: i32, h: i32 },
    RRect { x: i32, y: i32, w: i32, h: i32, rad: i32 },
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hdc: HDC) -> HDC;
    fn DeleteDC(hdc: HDC) -> i32;
    fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BITMAPINFO,
        usage: u32,
        ppv_bits: *mut *mut c_void,
        hsection: *mut c_void,
        offset: u32,
    ) -> HGDIOBJ;
    fn DeleteObject(h: HGDIOBJ) -> i32;
    fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    fn BitBlt(
        hdc: HDC,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        hdc_src: HDC,
        x_src: i32,
        y_src: i32,
        rop: u32,
    ) -> i32;
    fn FillRect(hdc: HDC, lprc: *const RECT, hbr: HGDIOBJ) -> i32;
    fn FrameRect(hdc: HDC, lprc: *const RECT, hpen: HGDIOBJ) -> i32;
    fn RoundRect(
        hdc: HDC,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        ellipse_w: i32,
        ellipse_h: i32,
    ) -> i32;
    fn CreateSolidBrush(color: u32) -> HGDIOBJ;
    fn CreatePen(style: i32, width: i32, color: u32) -> HGDIOBJ;
    fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u8,
        underline: u8,
        strikeout: u8,
        charset: u8,
        outtype: u8,
        cliptype: u8,
        quality: u8,
        pitchandfamily: u8,
        face: *const u16,
    ) -> HFONT;
    fn TextOutW(hdc: HDC, x: i32, y: i32, str: *const u16, count: i32) -> i32;
    fn GetTextExtentPoint32W(
        hdc: HDC,
        str: *const u16,
        count: i32,
        size: *mut POINT,
    ) -> i32;
    fn SetBkMode(hdc: HDC, mode: u32) -> u32;
    fn SetTextColor(hdc: HDC, color: u32) -> u32;
    fn SetBkColor(hdc: HDC, color: u32) -> u32;
    fn SaveDC(hdc: HDC) -> i32;
    fn RestoreDC(hdc: HDC, to: i32) -> i32;
    fn CreateRectRgn(left: i32, top: i32, right: i32, bottom: i32) -> HGDIOBJ;
    fn SelectClipRgn(hdc: HDC, hrgn: HGDIOBJ) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn GetDC(hwnd: *mut c_void) -> HDC;
    fn ReleaseDC(hwnd: *mut c_void, hdc: HDC) -> i32;
}

/// Measured text extents. Bearings are reported as zero, which makes the
/// UI's bearing-based centering math position text by its top-left corner.
#[derive(Clone, Copy, Default)]
pub struct TextExtents {
    width: f64,
    height: f64,
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
        0.0
    }
}

/// A Cairo-compatible drawing surface over a memory DC + DIB section.
pub struct Canvas {
    mem_dc: HDC,
    hbitmap: HGDIOBJ,
    old_bitmap: HGDIOBJ,
    pixels: *mut u8,
    pub w: i32,
    pub h: i32,
    color: u32,
    line_width: i32,
    font: HFONT,
    old_font: HFONT,
    font_size: f32,
    font_bold: bool,
    cur_x: i32,
    cur_y: i32,
    shape: Shape,
    states: Vec<i32>,
}

/// Convert an 0xRRGGBB color to a GDI COLORREF (0x00BBGGRR).
fn colorref(rgb: u32) -> u32 {
    let r = rgb & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = (rgb >> 16) & 0xff;
    (b << 16) | (g << 8) | r
}

impl Canvas {
    /// Create a canvas that draws into a fresh `w`x`h` 32-bit back buffer.
    pub fn new_bitmap(w: i32, h: i32) -> Option<Self> {
        let w = w.max(1);
        let h = h.max(1);
        // Top-down 32bpp DIB (negative biHeight).
        let bmi = BITMAPINFO {
            bmi_header: BITMAPINFOHEADER {
                bi_size: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                bi_width: w,
                bi_height: -h,
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: BI_RGB,
                bi_size_image: 0,
                bi_xpels_per_meter: 0,
                bi_ypels_per_meter: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
        };
        let mut pixels: *mut c_void = ptr::null_mut();
        let hbitmap = unsafe {
            CreateDIBSection(ptr::null_mut(), &bmi, DIB_RGB_COLORS, &mut pixels, ptr::null_mut(), 0)
        };
        if hbitmap.is_null() {
            return None;
        }
        let mem_dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
        if mem_dc.is_null() {
            unsafe { DeleteObject(hbitmap) };
            return None;
        }
        let old_bitmap = unsafe { SelectObject(mem_dc, hbitmap) };
        unsafe { SetBkMode(mem_dc, TRANSPARENT) };
        // A default font.
        let font = Self::make_font(12.0, false);
        let old_font = unsafe { SelectObject(mem_dc, font as HGDIOBJ) };
        Some(Self {
            mem_dc,
            hbitmap,
            old_bitmap,
            pixels: pixels as *mut u8,
            w,
            h,
            color: 0xff000000,
            line_width: 1,
            font,
            old_font,
            font_size: 12.0,
            font_bold: false,
            cur_x: 0,
            cur_y: 0,
            shape: Shape::None,
            states: Vec::new(),
        })
    }

    fn make_font(size: f32, bold: bool) -> HFONT {
        let mut face = [0u16; 32];
        let name: Vec<u16> = "Segoe UI".encode_utf16().collect();
        for (i, c) in name.iter().enumerate() {
            if i < 31 {
                face[i] = *c;
            }
        }
        unsafe {
            CreateFontW(
                -(size as i32),
                0,
                0,
                0,
                if bold { FW_BOLD } else { FW_NORMAL },
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                DEFAULT_QUALITY,
                DEFAULT_PITCH,
                face.as_ptr(),
            )
        }
    }

    /// Clear the back buffer to transparent.
    pub fn clear(&self) {
        unsafe {
            std::ptr::write_bytes(self.pixels, 0, (self.w * self.h * 4) as usize);
        }
    }

    /// Blit the back buffer onto a window device context.
    pub fn blit_to_hdc(&self, win_hdc: HDC) {
        unsafe { BitBlt(win_hdc, 0, 0, self.w, self.h, self.mem_dc, 0, 0, SRCCOPY) };
    }

    fn select_font(&mut self, size: f32, bold: bool) {
        // Delete the old font, create a new one, select it.
        unsafe {
            if !self.font.is_null() {
                DeleteObject(self.font as HGDIOBJ);
            }
        }
        let font = Self::make_font(size, bold);
        unsafe { SelectObject(self.mem_dc, font as HGDIOBJ) };
        self.font = font;
    }

    fn render_shape(&self, fill: bool) {
        match self.shape {
            Shape::None => {}
            Shape::Rect { x, y, w, h } => {
                let rc = RECT {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                };
                unsafe {
                    if fill {
                        let br = CreateSolidBrush(self.color);
                        FillRect(self.mem_dc, &rc, br);
                        DeleteObject(br);
                    } else {
                        let pen = CreatePen(PS_SOLID, self.line_width, self.color);
                        FrameRect(self.mem_dc, &rc, pen);
                        DeleteObject(pen);
                    }
                }
            }
            Shape::RRect { x, y, w, h, rad } => {
                unsafe {
                    if fill {
                        let br = CreateSolidBrush(self.color);
                        let pen = CreatePen(PS_SOLID, 1, self.color);
                        let ob_br = SelectObject(self.mem_dc, br);
                        let ob_pen = SelectObject(self.mem_dc, pen);
                        RoundRect(self.mem_dc, x, y, x + w, y + h, rad * 2, rad * 2);
                        SelectObject(self.mem_dc, ob_br);
                        SelectObject(self.mem_dc, ob_pen);
                        DeleteObject(br);
                        DeleteObject(pen);
                    } else {
                        let pen = CreatePen(PS_SOLID, self.line_width, self.color);
                        let ob_pen = SelectObject(self.mem_dc, pen);
                        let ob_br = SelectObject(self.mem_dc, NULL_BRUSH as HGDIOBJ);
                        RoundRect(self.mem_dc, x, y, x + w, y + h, rad * 2, rad * 2);
                        SelectObject(self.mem_dc, ob_pen);
                        SelectObject(self.mem_dc, ob_br);
                        DeleteObject(pen);
                    }
                }
            }
        }
    }

    // ---- Cairo-compatible surface ----

    pub fn set_source_rgba(&mut self, r: f64, g: f64, b: f64, a: f64) {
        // GDI is opaque; alpha is ignored (the OSK is rendered opaque).
        let r8 = (r.clamp(0.0, 1.0) * 255.0) as u8;
        let g8 = (g.clamp(0.0, 1.0) * 255.0) as u8;
        let b8 = (b.clamp(0.0, 1.0) * 255.0) as u8;
        self.color = ((b8 as u32) << 16) | ((g8 as u32) << 8) | (r8 as u32);
        let _ = a;
    }

    pub fn set_line_width(&mut self, w: f64) {
        self.line_width = (w as i32).max(1);
    }

    pub fn new_path(&mut self) {
        self.shape = Shape::None;
    }

    pub fn new_sub_path(&mut self) {
        // A rounded rect is built as a subpath of arcs; we set the shape
        // when the arcs are recognized.
    }

    pub fn close_path(&mut self) {}

    pub fn move_to(&mut self, x: f64, y: f64) {
        self.cur_x = x as i32;
        self.cur_y = y as i32;
    }

    pub fn line_to(&mut self, _x: f64, _y: f64) {}

    /// No-op: rounded rects are set directly via `rrect`.
    pub fn arc(&mut self, _cx: f64, _cy: f64, _r: f64, _a1: f64, _a2: f64) {}

    /// Directly set the current shape to a rounded rectangle (used by the
    /// ported `rrect` helper).
    pub fn rrect(&mut self, x: f64, y: f64, w: f64, h: f64, rad: f64) {
        self.shape = Shape::RRect {
            x: x as i32,
            y: y as i32,
            w: w as i32,
            h: h as i32,
            rad: rad as i32,
        };
    }

    pub fn rectangle(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.shape = Shape::Rect {
            x: x as i32,
            y: y as i32,
            w: w as i32,
            h: h as i32,
        };
    }

    pub fn fill(&mut self) {
        self.render_shape(true);
    }

    pub fn stroke(&mut self) {
        self.render_shape(false);
    }

    pub fn clip(&mut self) {
        // Clip to the bounding box of the current shape.
        if let Some((x, y, w, h)) = self.shape_bbox() {
            unsafe {
                let rgn = CreateRectRgn(x, y, x + w, y + h);
                if !rgn.is_null() {
                    SelectClipRgn(self.mem_dc, rgn);
                    DeleteObject(rgn);
                }
            }
        }
    }

    fn shape_bbox(&self) -> Option<(i32, i32, i32, i32)> {
        match self.shape {
            Shape::Rect { x, y, w, h } => Some((x, y, w, h)),
            Shape::RRect { x, y, w, h, .. } => Some((x, y, w, h)),
            Shape::None => None,
        }
    }

    pub fn reset_clip(&mut self) {
        unsafe {
            let full = CreateRectRgn(0, 0, self.w, self.h);
            if !full.is_null() {
                SelectClipRgn(self.mem_dc, full);
                DeleteObject(full);
            }
        }
    }

    pub fn save(&mut self) -> i32 {
        let token = unsafe { SaveDC(self.mem_dc) };
        if token > 0 {
            self.states.push(token);
        }
        token
    }

    pub fn restore(&mut self) {
        if let Some(token) = self.states.pop() {
            unsafe { RestoreDC(self.mem_dc, token) };
        }
    }

    pub fn set_font_size(&mut self, size: f64) {
        self.font_size = size as f32;
        self.select_font(self.font_size, self.font_bold);
    }

    pub fn select_font_face(&mut self, _family: &str, _slant: u32, weight: u32) {
        self.font_bold = weight != 0;
        self.select_font(self.font_size, self.font_bold);
    }

    pub fn show_text(&mut self, s: &str) {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            SetTextColor(self.mem_dc, self.color);
            SetBkColor(self.mem_dc, 0);
            TextOutW(self.mem_dc, self.cur_x, self.cur_y, utf16.as_ptr(), utf16.len() as i32);
        }
    }

    pub fn text_extents(&mut self, s: &str) -> Option<TextExtents> {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        let mut size = POINT { x: 0, y: 0 };
        let status = unsafe {
            GetTextExtentPoint32W(self.mem_dc, utf16.as_ptr(), utf16.len() as i32, &mut size)
        };
        if status == 0 {
            return None;
        }
        Some(TextExtents {
            width: size.x as f64,
            height: size.y as f64,
        })
    }

    pub fn paint(&mut self) {
        // Fill the whole buffer with the current color (for the clear, the
        // caller cleared the buffer first; this is a no-op in that case).
        let rc = RECT {
            left: 0,
            top: 0,
            right: self.w,
            bottom: self.h,
        };
        unsafe {
            let br = CreateSolidBrush(self.color);
            FillRect(self.mem_dc, &rc, br);
            DeleteObject(br);
        }
    }

    pub fn set_operator(&mut self, _op: u32) {}

    pub fn identity_matrix(&mut self) {}

    pub fn scale(&mut self, _sx: f64, _sy: f64) {
        // Rendered at 1:1; no scaling.
    }

    /// Write the back buffer to a binary PPM (for verification).
    pub fn dump_ppm(&self, path: &str) -> bool {
        let mut out = Vec::with_capacity(3 * self.w as usize * self.h as usize + 32);
        out.extend_from_slice(format!("P6\n{} {}\n255\n", self.w, self.h).as_bytes());
        unsafe {
            for y in 0..self.h {
                let row = self.pixels.add((y * self.w) as usize * 4);
                for x in 0..self.w {
                    let p = row.add((x * 4) as usize);
                    // 32bpp DIB is stored B,G,R,A in memory; PPM wants R,G,B.
                    out.push(*p.add(2));
                    out.push(*p.add(1));
                    out.push(*p);
                }
            }
        }
        std::fs::write(path, out).is_ok()
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            // Restore the DC to its original objects.
            if !self.old_font.is_null() {
                SelectObject(self.mem_dc, self.old_font as HGDIOBJ);
            }
            if !self.font.is_null() {
                DeleteObject(self.font as HGDIOBJ);
            }
            if !self.old_bitmap.is_null() {
                SelectObject(self.mem_dc, self.old_bitmap);
            }
            DeleteDC(self.mem_dc);
            DeleteObject(self.hbitmap);
        }
    }
}
