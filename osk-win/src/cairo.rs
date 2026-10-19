//! The subset of the cairo API that `render.rs` draws with, powered by
//! tiny-skia and fontdue with a GDI backbuffer for blitting to the window HDC.
//!
//! Vector paths (arcs, rounded rects, lines, strokes, fills, clips) and text
//! are rendered with subpixel anti-aliasing and alpha blending into a tiny-skia
//! `Pixmap`. When `blit_to_hdc` is called, the pixels are transferred to the
//! GDI DIB section and blitted to the window device context.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::ffi::c_void;
use std::io::Write;
use std::ptr;
use std::rc::Rc;
use std::sync::LazyLock;

use fontdue::{Font, FontSettings};
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, LineCap as SkiaLineCap, LineJoin,
    Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform,
};

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

const SRCCOPY: u32 = 0x00CC_0020;

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
}

static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/AdwaitaSans-Regular.ttf");
static FONT: LazyLock<Font> = LazyLock::new(|| {
    Font::from_bytes(EMBEDDED_FONT, FontSettings::default()).expect("invalid embedded font")
});

#[derive(Debug, Clone, Copy)]
pub struct Error;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Drawing failed")
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Source,
    Over,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
}

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

struct Surface {
    pixmap: RefCell<Pixmap>,
    dc: Hdc,
    bitmap: Hgdiobj,
    old_bitmap: Hgdiobj,
    pixels: *mut u8,
    w: i32,
    h: i32,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old_bitmap);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

#[derive(Clone)]
pub struct ImageSurface(Rc<Surface>);

impl ImageSurface {
    pub fn create(_format: Format, w: i32, h: i32) -> Result<Self, Error> {
        let (w, h) = (w.max(1), h.max(1));
        let pixmap = Pixmap::new(w as u32, h as u32).ok_or(Error)?;

        let info = BitmapInfoHeader {
            size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            width: w,
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
        let bitmap = unsafe {
            CreateDIBSection(ptr::null_mut(), &info, 0, &mut pixels, ptr::null_mut(), 0)
        };
        if bitmap.is_null() || pixels.is_null() {
            return Err(Error);
        }
        let dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
        if dc.is_null() {
            unsafe { DeleteObject(bitmap) };
            return Err(Error);
        }
        let old_bitmap = unsafe { SelectObject(dc, bitmap) };

        Ok(Self(Rc::new(Surface {
            pixmap: RefCell::new(pixmap),
            dc,
            bitmap,
            old_bitmap,
            pixels: pixels as *mut u8,
            w,
            h,
        })))
    }

    pub fn width(&self) -> i32 {
        self.0.w
    }

    pub fn height(&self) -> i32 {
        self.0.h
    }

    pub fn flush(&self) {
        let pm = self.0.pixmap.borrow();
        let src = pm.data();
        let dst = unsafe {
            std::slice::from_raw_parts_mut(self.0.pixels, (self.0.w * self.0.h * 4) as usize)
        };
        let (src_chunks, _) = src.as_chunks::<4>();
        let (dst_chunks, _) = dst.as_chunks_mut::<4>();
        for (s, d) in src_chunks.iter().zip(dst_chunks.iter_mut()) {
            d[0] = s[2]; // B
            d[1] = s[1]; // G
            d[2] = s[0]; // R
            d[3] = s[3]; // A
        }
        unsafe { GdiFlush() };
    }

    pub fn blit_to_hdc(&self, hdc: *mut c_void) {
        self.flush();
        unsafe { BitBlt(hdc, 0, 0, self.0.w, self.0.h, self.0.dc, 0, 0, SRCCOPY) };
    }

    pub fn write_to_png<W: Write>(&self, out: &mut W) -> Result<(), Error> {
        let pm = self.0.pixmap.borrow();
        let png = pm.encode_png().map_err(|_| Error)?;
        out.write_all(&png).map_err(|_| Error)
    }
}

#[derive(Clone)]
struct Gstate {
    color: Color,
    line_width: f32,
    line_cap: LineCap,
    operator: Operator,
    transform: Transform,
    font_size: f32,
    bold: bool,
    clip_mask: Option<Mask>,
    source_surface: Option<ImageSurface>,
}

struct State {
    g: Gstate,
    stack: Vec<Gstate>,
    pb: PathBuilder,
    cur: Option<(f64, f64)>,
}

pub struct Context {
    surface: ImageSurface,
    st: RefCell<State>,
}

impl Context {
    pub fn new(surface: &ImageSurface) -> Result<Self, Error> {
        Ok(Self {
            surface: surface.clone(),
            st: RefCell::new(State {
                g: Gstate {
                    color: Color::from_rgba8(0, 0, 0, 255),
                    line_width: 2.0,
                    line_cap: LineCap::Butt,
                    operator: Operator::Over,
                    transform: Transform::identity(),
                    font_size: 10.0,
                    bold: false,
                    clip_mask: None,
                    source_surface: None,
                },
                stack: Vec::new(),
                pb: PathBuilder::new(),
                cur: None,
            }),
        })
    }

    pub fn save(&self) -> Result<(), Error> {
        let mut st = self.st.borrow_mut();
        let g = st.g.clone();
        st.stack.push(g);
        Ok(())
    }

    pub fn restore(&self) -> Result<(), Error> {
        let mut st = self.st.borrow_mut();
        if let Some(g) = st.stack.pop() {
            st.g = g;
        }
        Ok(())
    }

    pub fn identity_matrix(&self) {
        self.st.borrow_mut().g.transform = Transform::identity();
    }

    pub fn scale(&self, sx: f64, sy: f64) {
        let mut st = self.st.borrow_mut();
        st.g.transform = st.g.transform.pre_scale(sx as f32, sy as f32);
    }

    pub fn set_operator(&self, op: Operator) {
        self.st.borrow_mut().g.operator = op;
    }

    pub fn set_source_rgba(&self, r: f64, g: f64, b: f64, a: f64) {
        let mut st = self.st.borrow_mut();
        st.g.color = Color::from_rgba(r as f32, g as f32, b as f32, a as f32).unwrap_or(Color::BLACK);
        st.g.source_surface = None;
    }

    pub fn set_source_surface(&self, surface: &ImageSurface, _x: f64, _y: f64) -> Result<(), Error> {
        let mut st = self.st.borrow_mut();
        st.g.source_surface = Some(surface.clone());
        Ok(())
    }

    pub fn set_line_width(&self, w: f64) {
        self.st.borrow_mut().g.line_width = w as f32;
    }

    pub fn set_line_cap(&self, cap: LineCap) {
        self.st.borrow_mut().g.line_cap = cap;
    }

    pub fn new_path(&self) {
        let mut st = self.st.borrow_mut();
        st.pb = PathBuilder::new();
        st.cur = None;
    }

    pub fn new_sub_path(&self) {
        self.st.borrow_mut().cur = None;
    }

    pub fn move_to(&self, x: f64, y: f64) {
        let mut st = self.st.borrow_mut();
        st.pb.move_to(x as f32, y as f32);
        st.cur = Some((x, y));
    }

    pub fn line_to(&self, x: f64, y: f64) {
        let mut st = self.st.borrow_mut();
        if st.cur.is_none() {
            st.pb.move_to(x as f32, y as f32);
        } else {
            st.pb.line_to(x as f32, y as f32);
        }
        st.cur = Some((x, y));
    }

    pub fn arc(&self, cx: f64, cy: f64, r: f64, a1: f64, a2: f64) {
        let mut a2 = a2;
        while a2 < a1 {
            a2 += 2.0 * PI;
        }
        let p0x = cx + r * a1.cos();
        let p0y = cy + r * a1.sin();
        let had_cur = self.st.borrow().cur.is_some();
        if had_cur {
            self.line_to(p0x, p0y);
        } else {
            self.move_to(p0x, p0y);
        }

        let mut cur_angle = a1;
        while cur_angle < a2 {
            let sweep = (a2 - cur_angle).min(PI / 2.0);
            let next_angle = cur_angle + sweep;
            let k = 4.0 / 3.0 * (sweep / 4.0).tan();

            let cos1 = cur_angle.cos();
            let sin1 = cur_angle.sin();
            let cos2 = next_angle.cos();
            let sin2 = next_angle.sin();

            let cp0x = cx + r * cos1;
            let cp0y = cy + r * sin1;
            let p1x = cp0x - k * r * sin1;
            let p1y = cp0y + k * r * cos1;

            let p3x = cx + r * cos2;
            let p3y = cy + r * sin2;
            let p2x = p3x + k * r * sin2;
            let p2y = p3y - k * r * cos2;

            self.st.borrow_mut().pb.cubic_to(
                p1x as f32, p1y as f32,
                p2x as f32, p2y as f32,
                p3x as f32, p3y as f32,
            );
            cur_angle = next_angle;
        }
        self.st.borrow_mut().cur = Some((cx + r * a2.cos(), cy + r * a2.sin()));
    }

    pub fn close_path(&self) {
        let mut st = self.st.borrow_mut();
        st.pb.close();
    }

    pub fn rectangle(&self, x: f64, y: f64, w: f64, h: f64) {
        self.move_to(x, y);
        self.line_to(x + w, y);
        self.line_to(x + w, y + h);
        self.line_to(x, y + h);
        self.close_path();
    }

    pub fn fill(&self) -> Result<(), Error> {
        let mut st = self.st.borrow_mut();
        let pb = std::mem::replace(&mut st.pb, PathBuilder::new());
        st.cur = None;
        let Some(path) = pb.finish() else {
            return Ok(());
        };
        let mut paint = Paint::default();
        paint.set_color(st.g.color);
        paint.blend_mode = match st.g.operator {
            Operator::Source => BlendMode::Source,
            Operator::Over => BlendMode::SourceOver,
        };
        paint.anti_alias = true;
        let mut pm = self.surface.0.pixmap.borrow_mut();
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            st.g.transform,
            st.g.clip_mask.as_ref(),
        );
        Ok(())
    }

    pub fn stroke(&self) -> Result<(), Error> {
        let mut st = self.st.borrow_mut();
        let pb = std::mem::replace(&mut st.pb, PathBuilder::new());
        st.cur = None;
        let Some(path) = pb.finish() else {
            return Ok(());
        };
        let mut paint = Paint::default();
        paint.set_color(st.g.color);
        paint.blend_mode = match st.g.operator {
            Operator::Source => BlendMode::Source,
            Operator::Over => BlendMode::SourceOver,
        };
        paint.anti_alias = true;
        let stroke = Stroke {
            width: st.g.line_width,
            miter_limit: 4.0,
            line_cap: match st.g.line_cap {
                LineCap::Butt => SkiaLineCap::Butt,
                LineCap::Round => SkiaLineCap::Round,
            },
            line_join: LineJoin::Round,
            dash: None,
        };
        let mut pm = self.surface.0.pixmap.borrow_mut();
        pm.stroke_path(
            &path,
            &paint,
            &stroke,
            st.g.transform,
            st.g.clip_mask.as_ref(),
        );
        Ok(())
    }

    pub fn clip(&self) {
        let mut st = self.st.borrow_mut();
        let pb = std::mem::replace(&mut st.pb, PathBuilder::new());
        st.cur = None;
        let Some(path) = pb.finish() else {
            return;
        };
        let (w, h) = (self.surface.0.w as u32, self.surface.0.h as u32);
        let ts = st.g.transform;
        let has_curves = path.segments().any(|seg| matches!(seg, tiny_skia::PathSegment::CubicTo(..) | tiny_skia::PathSegment::QuadTo(..)));
        if let Some(ref mut mask) = st.g.clip_mask {
            mask.intersect_path(&path, FillRule::Winding, has_curves, ts);
        } else {
            let mut mask = match Mask::new(w, h) {
                Some(m) => m,
                None => return,
            };
            mask.fill_path(&path, FillRule::Winding, has_curves, ts);
            st.g.clip_mask = Some(mask);
        }
    }

    pub fn reset_clip(&self) {
        self.st.borrow_mut().g.clip_mask = None;
    }

    pub fn paint(&self) -> Result<(), Error> {
        let mut st = self.st.borrow_mut();
        let (w, h) = (self.surface.0.w as u32, self.surface.0.h as u32);
        if let Some(src) = st.g.source_surface.take() {
            let src_pm = src.0.pixmap.borrow();
            let mut pm = self.surface.0.pixmap.borrow_mut();
            if st.g.transform.is_identity() && st.g.clip_mask.is_none() && st.g.operator == Operator::Source && pm.width() == src_pm.width() && pm.height() == src_pm.height() {
                pm.data_mut().copy_from_slice(src_pm.data());
                return Ok(());
            }
            let paint = PixmapPaint {
                opacity: 1.0,
                blend_mode: match st.g.operator {
                    Operator::Source => BlendMode::Source,
                    Operator::Over => BlendMode::SourceOver,
                },
                quality: FilterQuality::Nearest,
            };
            pm.draw_pixmap(0, 0, src_pm.as_ref(), &paint, st.g.transform, st.g.clip_mask.as_ref());
            return Ok(());
        }

        let Some(rect) = Rect::from_xywh(0.0, 0.0, w as f32, h as f32) else {
            return Ok(());
        };
        let mut paint = Paint::default();
        paint.set_color(st.g.color);
        paint.blend_mode = match st.g.operator {
            Operator::Source => BlendMode::Source,
            Operator::Over => BlendMode::SourceOver,
        };
        paint.anti_alias = false;
        let mut pm = self.surface.0.pixmap.borrow_mut();
        pm.fill_rect(rect, &paint, Transform::identity(), st.g.clip_mask.as_ref());
        Ok(())
    }

    pub fn select_font_face(&self, _family: &str, _slant: FontSlant, weight: FontWeight) {
        self.st.borrow_mut().g.bold = weight == FontWeight::Bold;
    }

    pub fn set_font_size(&self, size: f64) {
        self.st.borrow_mut().g.font_size = size as f32;
    }

    pub fn text_extents(&self, s: &str) -> Result<TextExtents, Error> {
        let st = self.st.borrow();
        let font_size = st.g.font_size;
        let mut total_w = 0.0f32;
        for ch in s.chars() {
            let metrics = FONT.metrics(ch, font_size);
            total_w += metrics.advance_width;
        }
        let line = FONT.horizontal_line_metrics(font_size).unwrap_or(fontdue::LineMetrics {
            ascent: font_size * 0.8,
            descent: -font_size * 0.2,
            line_gap: 0.0,
            new_line_size: font_size * 1.2,
        });
        let (sx, sy) = (st.g.transform.sx as f64, st.g.transform.sy as f64);
        Ok(TextExtents {
            width: total_w as f64 / sx.max(0.001),
            height: (line.ascent - line.descent) as f64 / sy.max(0.001),
            y_bearing: -line.ascent as f64 / sy.max(0.001),
        })
    }

    pub fn show_text(&self, s: &str) -> Result<(), Error> {
        let st = self.st.borrow();
        let (cur_x, cur_y) = st.cur.unwrap_or((0.0, 0.0));
        let mut pt = tiny_skia::Point::from_xy(cur_x as f32, cur_y as f32);
        st.g.transform.map_point(&mut pt);
        let mut base_x = pt.x;
        let base_y = pt.y;
        let px_size = st.g.font_size * st.g.transform.sy;
        let mut pm = self.surface.0.pixmap.borrow_mut();
        let pw = pm.width() as i32;
        let ph = pm.height() as i32;
        let color = st.g.color;
        let base_r = color.red();
        let base_g = color.green();
        let base_b = color.blue();
        let base_a = color.alpha();

        for ch in s.chars() {
            let (metrics, bitmap) = FONT.rasterize(ch, px_size);
            let gx = (base_x + metrics.xmin as f32).round() as i32;
            let gy = (base_y - (metrics.ymin + metrics.height as i32) as f32).round() as i32;

            let gw = metrics.width as i32;
            let gh = metrics.height as i32;

            let passes = if st.g.bold { 2 } else { 1 };
            for pass in 0..passes {
                let offset_x = if pass == 1 { 1 } else { 0 };
                for ly in 0..gh {
                    let py = gy + ly;
                    if py < 0 || py >= ph {
                        continue;
                    }
                    for lx in 0..gw {
                        let px = gx + lx + offset_x;
                        if px < 0 || px >= pw {
                            continue;
                        }
                        let cov = bitmap[(ly * gw + lx) as usize];
                        if cov == 0 {
                            continue;
                        }
                        let mut alpha_f = (cov as f32 / 255.0) * base_a;
                        if let Some(ref mask) = st.g.clip_mask {
                            let mask_cov = mask.data()[(py * pw + px) as usize];
                            if mask_cov == 0 {
                                continue;
                            }
                            alpha_f *= mask_cov as f32 / 255.0;
                        }

                        let p_idx = ((py * pw + px) * 4) as usize;
                        let data = pm.data_mut();
                        let dst_r = data[p_idx] as f32 / 255.0;
                        let dst_g = data[p_idx + 1] as f32 / 255.0;
                        let dst_b = data[p_idx + 2] as f32 / 255.0;
                        let dst_a = data[p_idx + 3] as f32 / 255.0;

                        let src_r = base_r * alpha_f;
                        let src_g = base_g * alpha_f;
                        let src_b = base_b * alpha_f;
                        let src_a = alpha_f;

                        let inv_a = 1.0 - src_a;
                        let out_r = ((src_r + dst_r * inv_a).clamp(0.0, 1.0) * 255.0).round() as u8;
                        let out_g = ((src_g + dst_g * inv_a).clamp(0.0, 1.0) * 255.0).round() as u8;
                        let out_b = ((src_b + dst_b * inv_a).clamp(0.0, 1.0) * 255.0).round() as u8;
                        let out_a = ((src_a + dst_a * inv_a).clamp(0.0, 1.0) * 255.0).round() as u8;

                        data[p_idx] = out_r;
                        data[p_idx + 1] = out_g;
                        data[p_idx + 2] = out_b;
                        data[p_idx + 3] = out_a;
                    }
                }
            }
            base_x += metrics.advance_width * st.g.transform.sx;
        }
        Ok(())
    }
}
