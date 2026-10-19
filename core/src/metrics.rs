//! One spacing scale for the whole keyboard.
//!
//! [`Metrics::new`] derives every drawn distance, radius and font size
//! from a single scale `s` per surface size, from the surface size and
//! the layout's grid columns and row count. Keys may stretch to fill
//! the window edge to edge; spacing must not. Pure numbers, no cairo,
//! so both front ends read the same values and a future one can too.
//!
//! The scale follows the tighter side, and each side's budget is
//! written in `s` and solved in closed form. The circularity - margin
//! and bar consume `s`, so the leftover row height needs `s`, which
//! needs the leftover - is untangled by asking "how big could one key
//! unit be here?" with the spacing already expressed as fractions of
//! `s`:
//!
//! - horizontal: `unit_x = (width - 2 * margin) / cols_u`, with
//!   `margin = 2 * g` and `g = GAP_S * s`, so
//!   `s_w = width / (cols_u + 4 * GAP_S)`
//! - vertical: `row_h = (height - bar_h - 2 * margin) / rows`, with
//!   `bar_h = pill_h + 2 * g` and `pill_h = PILL_S * s`, so
//!   `s_h = height / (rows + PILL_S + 6 * GAP_S)`
//!
//! and `s = min(s_w, s_h)`: an exact fixed point, no iteration. On the
//! limiting side the leftover unit equals `s`; on the other side the
//! keys stretch. [`Metrics::clamp_size`] reuses the same closed form
//! to stop a window growing past the key-aspect range.

/// Visible gap between neighbouring keys, as a fraction of the scale
/// `s`. Chosen so the gap stays close to the measured gap of the
/// pre-change look at the default size (two 0.04u key insets, about
/// 3.7 px at 1u = 46 px).
pub const GAP_S: f64 = 0.08;

/// Suggestion-bar pill height, as a fraction of `s`. The bar is the
/// pill plus the gap above and below, so `bar_h = (PILL_S + 2*GAP_S)*s`.
pub const PILL_S: f64 = 0.55;

/// Corner radius of a key, as a fraction of `s` (the pre-change
/// 0.16 of the drawn unit).
pub const KEY_R_S: f64 = 0.16;

/// Character-key label font size, as a fraction of `s`.
pub const FONT_CHAR_S: f64 = 0.36;

/// Multi-character modifier-word label font size ("ctrl", "caps",
/// "pgup"), as a fraction of `s`. Same as the character font today; a
/// separate token so the words can be resized once without touching
/// every key.
pub const FONT_LABEL_S: f64 = 0.36;

/// Allowed key aspect range (`unit_x / row_h`) after
/// [`Metrics::clamp_size`]: keys stop growing past this; no padding is
/// ever added, the window just stops growing in that direction.
pub const ASPECT_MIN: f64 = 1.0;

/// See [`ASPECT_MIN`].
pub const ASPECT_MAX: f64 = 1.5;

/// Every spacing metric for one surface size, in logical pixels.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    /// The one scale: pixels per spacing unit. Derived from the
    /// tighter of the horizontal key unit and the row height.
    pub s: f64,
    /// Visible gap between neighbouring keys, and between pills and
    /// icons.
    pub g: f64,
    /// Bezel margin between the window edge and the keys: `2 * g`.
    pub margin: f64,
    /// Top bar height: `pill_h + 2 * g`.
    pub bar_h: f64,
    /// Suggestion-pill height (also the bar chips' height).
    pub pill_h: f64,
    /// Gap between suggestion pills: `g`.
    pub pill_gap: f64,
    /// Spacing around bar icons and chips: `g`.
    pub icon: f64,
    /// Corner radius of a key.
    pub key_r: f64,
    /// Corner radius of the window (case): `key_r + margin`, so the
    /// nested corners stay parallel.
    pub case_r: f64,
    /// Corner radius of a suggestion pill: `pill_h / 2` (capsule).
    pub pill_r: f64,
    /// Character-key label font size.
    pub font_char: f64,
    /// Modifier-word label font size.
    pub font_label: f64,
    /// Key cell unit in x: pixels per 1.0u of key width. Fills the
    /// width edge to edge after the margins; keys stretch.
    pub unit_x: f64,
    /// Key cell height per row; fills the height edge to edge after
    /// the bar and margins; keys stretch.
    pub row_h: f64,
}

impl Metrics {
    /// The metrics for a surface of `width` x `height` logical pixels
    /// showing a layout whose widest row spans `cols` grid columns in
    /// 1/20u steps and has `rows` rows.
    pub fn new(width: f64, height: f64, cols: i32, rows: usize) -> Self {
        let cols_u = (cols.max(1) as f64) / 20.0;
        let rows = (rows.max(1) as f64) / 1.0;
        // Closed form of both budgets above; the smaller wins.
        let s_w = width / (cols_u + 4.0 * GAP_S);
        let s_h = height / (rows + PILL_S + 6.0 * GAP_S);
        let s = if s_w < s_h { s_w } else { s_h };
        let g = GAP_S * s;
        let margin = 2.0 * g;
        let pill_h = PILL_S * s;
        let bar_h = pill_h + 2.0 * g;
        let key_r = KEY_R_S * s;
        Self {
            s,
            g,
            margin,
            bar_h,
            pill_h,
            pill_gap: g,
            icon: g,
            key_r,
            case_r: key_r + margin,
            pill_r: pill_h / 2.0,
            font_char: FONT_CHAR_S * s,
            font_label: FONT_LABEL_S * s,
            unit_x: (width - 2.0 * margin) / cols_u,
            row_h: (height - bar_h - 2.0 * margin) / rows,
        }
    }

    // ---- bar and menu internals, fractions of the one scale ----

    /// Suggestion-pill font size.
    pub fn font_sug(&self) -> f64 {
        0.34 * self.s
    }

    /// Bar chip (layout/theme) font size.
    pub fn font_chip(&self) -> f64 {
        0.30 * self.s
    }

    /// Theme-menu font size.
    pub fn font_menu(&self) -> f64 {
        0.24 * self.s
    }

    /// Close button glyph font size.
    pub fn font_close(&self) -> f64 {
        0.40 * self.s
    }

    /// Palette-icon radius.
    pub fn icon_r(&self) -> f64 {
        0.17 * self.s
    }

    /// Width of an icon button (theme palette / mic).
    pub fn icon_w(&self) -> f64 {
        0.75 * self.s
    }

    /// Width of the close button.
    pub fn close_w(&self) -> f64 {
        0.60 * self.s
    }

    /// Side padding inside a suggestion pill.
    pub fn pill_pad(&self) -> f64 {
        0.35 * self.s
    }

    /// Inner padding of a text chip (layout name).
    pub fn chip_pad(&self) -> f64 {
        0.20 * self.s
    }

    /// Corner radius of a text chip.
    pub fn chip_r(&self) -> f64 {
        0.35 * self.pill_h
    }

    /// Theme-menu card width.
    pub fn menu_col_w(&self) -> f64 {
        2.4 * self.s
    }

    /// Theme-menu card height.
    pub fn menu_card_h(&self) -> f64 {
        0.54 * self.s
    }

    /// Theme-menu column gap.
    pub fn menu_col_gap(&self) -> f64 {
        0.13 * self.s
    }

    /// Theme-menu row gap.
    pub fn menu_row_gap(&self) -> f64 {
        0.09 * self.s
    }

    /// Theme-menu inner padding.
    pub fn menu_pad(&self) -> f64 {
        0.17 * self.s
    }

    /// Theme-menu toggle height.
    pub fn menu_toggle_h(&self) -> f64 {
        0.57 * self.s
    }

    /// Theme-menu gap between the two toggles.
    pub fn menu_toggle_gap(&self) -> f64 {
        0.17 * self.s
    }

    /// Theme-menu panel corner radius.
    pub fn menu_r(&self) -> f64 {
        0.17 * self.s
    }

    /// Theme-menu card corner radius.
    pub fn menu_item_r(&self) -> f64 {
        0.11 * self.s
    }

    /// Theme-menu swatch dot radius.
    pub fn menu_dot_r(&self) -> f64 {
        0.09 * self.s
    }

    /// Text inset from a toggle/card's left edge.
    pub fn menu_text_pad(&self) -> f64 {
        0.22 * self.s
    }

    /// Text inset where a card's name starts (past the dot).
    pub fn menu_name_pad(&self) -> f64 {
        0.43 * self.s
    }

    /// Inset from a toggle/card's right edge for the "on"/checkmark.
    pub fn menu_right_pad(&self) -> f64 {
        0.35 * self.s
    }

    /// Horizontal offset of the menu's drop shadow.
    pub fn menu_shadow_dx(&self) -> f64 {
        0.04 * self.s
    }

    /// Vertical offset of the menu's drop shadow.
    pub fn menu_shadow_dy(&self) -> f64 {
        0.07 * self.s
    }

    /// Gap between the bar and the theme menu below it.
    pub fn menu_below_bar(&self) -> f64 {
        0.04 * self.s
    }

    /// The size the resize path should stop at: the requested size
    /// with the key aspect clamped into `[ASPECT_MIN, ASPECT_MAX]`.
    /// The window stops growing in a direction past the limit; no
    /// padding is added. Aspect below the minimum only binds when the
    /// height implies a taller row than the width allows, above the
    /// maximum only when the width implies a wider key than the
    /// height allows - each side's limit follows from the same closed
    /// form as [`Metrics::new`]. Sizes at or below the limits come
    /// back unchanged.
    pub fn clamp_size(w: i32, h: i32, cols: i32, rows: usize) -> (i32, i32) {
        if w <= 0 || h <= 0 || cols <= 0 || rows == 0 {
            return (w, h);
        }
        let cols_u = cols as f64 / 20.0;
        let rows = rows as f64;
        // The height implies a scale; the widest window that keeps the
        // key aspect at ASPECT_MAX against the so-scaled row height.
        let s_h = h as f64 / (rows + PILL_S + 6.0 * GAP_S);
        let w_max = s_h * (ASPECT_MAX * cols_u + 4.0 * GAP_S);
        let w = (w as f64).min(w_max).floor() as i32;
        // The (possibly clamped) width implies a scale; the tallest
        // window that keeps the key aspect at ASPECT_MIN against it.
        let s_w = w as f64 / (cols_u + 4.0 * GAP_S);
        let h_max = s_w * (rows + PILL_S + 6.0 * GAP_S);
        let h = (h as f64).min(h_max).floor() as i32;
        (w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 65% profile: 320 grid columns (16.0u), 5 rows.
    fn m(w: i32, h: i32) -> Metrics {
        Metrics::new(w as f64, h as f64, 320, 5)
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9 * b.abs().max(1.0), "{a} != {b}");
    }

    /// The key aspect after a clamp is inside the range (rounding
    /// slack included).
    fn assert_in_range(aspect: f64) {
        assert!(
            (ASPECT_MIN - 0.01..=ASPECT_MAX + 0.01).contains(&aspect),
            "aspect {aspect}"
        );
    }

    #[test]
    fn scale_resolves_the_margin_circularity() {
        // The closed form is an exact fixed point: with the margin and
        // bar wetted by s, the leftover unit of the tighter side is s.
        for (w, h) in [(675, 219), (747, 271), (1100, 260), (480, 170), (2000, 200), (600, 500)] {
            let m = m(w, h);
            let ux = (w as f64 - 2.0 * m.margin) / 16.0;
            let rh = (h as f64 - m.bar_h - 2.0 * m.margin) / 5.0;
            close(m.s, ux.min(rh));
            // The stated relations between the tokens hold.
            close(m.g, GAP_S * m.s);
            close(m.margin, 2.0 * m.g);
            close(m.bar_h, m.pill_h + 2.0 * m.g);
            close(m.pill_gap, m.g);
            close(m.icon, m.g);
            close(m.case_r, m.key_r + m.margin);
            close(m.pill_r, m.pill_h / 2.0);
            close(m.font_char, FONT_CHAR_S * m.s);
            close(m.font_label, FONT_LABEL_S * m.s);
            // Edge-to-edge fill, no padding: keys span the window.
            close(2.0 * m.margin + 16.0 * m.unit_x, w as f64);
            close(m.bar_h + 2.0 * m.margin + 5.0 * m.row_h, h as f64);
        }
    }

    #[test]
    fn gap_matches_the_measured_prechange_look() {
        // At the default size (1u = 46 px today) the visible gap was
        // measured at ~3.7 px and the bezel at ~5.5 px.
        let m1 = m(747, 271);
        assert!((m1.s * GAP_S - 3.7).abs() < 0.3, "gap {} px", m1.s * GAP_S);
        // At the user's saved size the gap stays at the same fraction.
        let m2 = m(675, 219);
        assert!((m2.s * GAP_S - 2.9).abs() < 0.2);
    }

    #[test]
    fn aspect_scales_separately_at_each_axis() {
        // Height-limited (1100x260): the keys stretch horizontally,
        // spacing holds.
        let mx = m(1100, 260);
        assert!((mx.unit_x - (1100.0 - 2.0 * mx.margin) / 16.0).abs() < 1e-9);
        assert!(mx.unit_x > mx.s);
        // Width-limited (700x330): the rows stretch vertically.
        let my = m(700, 330);
        assert!((my.unit_x - my.s).abs() < 1e-9);
        assert!(my.row_h > my.s);
    }

    #[test]
    fn clamp_leaves_in_range_sizes_alone() {
        // The user's current saved size is in range and stays put.
        assert_eq!(Metrics::clamp_size(675, 219, 320, 5), (675, 219));
        // Squarish and mid-range sizes too.
        assert_eq!(Metrics::clamp_size(747, 271, 320, 5), (747, 271));
        assert_eq!(Metrics::clamp_size(480, 170, 320, 5), (480, 170));
    }

    #[test]
    fn clamp_stops_width_past_the_aspect_max() {
        let (w, h) = Metrics::clamp_size(1600, 200, 320, 5);
        assert_eq!(h, 200);
        assert!(w < 1600, "width should stop growing: {w}");
        let m = Metrics::new(w as f64, 200.0, 320, 5);
        assert_in_range(m.unit_x / m.row_h);
        // Marginal: the in-window look at the limit touches ASPECT_MAX.
        assert!(m.unit_x / m.row_h > 1.4);
    }

    #[test]
    fn clamp_stops_height_past_the_aspect_min() {
        let (w, h) = Metrics::clamp_size(600, 500, 320, 5);
        assert_eq!(w, 600);
        assert!(h < 500, "height should stop growing: {h}");
        let m = Metrics::new(600.0, h as f64, 320, 5);
        assert_in_range(m.unit_x / m.row_h);
        assert!(m.unit_x / m.row_h < 1.1);
    }

    #[test]
    fn clamp_never_grows_or_pads_a_size() {
        for (w, h) in [(1600, 200), (600, 500), (1100, 260), (700, 330), (200, 180)]
        {
            let (cw, ch) = Metrics::clamp_size(w, h, 320, 5);
            assert!(cw <= w && ch <= h, "{cw}x{ch} grew from {w}x{h}");
        }
    }

    #[test]
    fn clamp_numpad_layout_switch_shrinks_height() {
        // 65%+Num is 20.5u wide; switching 675x219 onto it asks for
        // taller-than-wide keys, so the height stops growing first.
        let (w, h) = Metrics::clamp_size(675, 219, 410, 5);
        assert_eq!(w, 675);
        assert!(h < 219, "height should stop growing: {h}");
        let m = Metrics::new(675.0, h as f64, 410, 5);
        assert_in_range(m.unit_x / m.row_h);
    }

    #[test]
    fn clamp_handles_degenerate_input() {
        assert_eq!(Metrics::clamp_size(0, 0, 320, 5), (0, 0));
        assert_eq!(Metrics::clamp_size(-5, 100, 320, 5), (-5, 100));
        assert_eq!(Metrics::clamp_size(675, 219, 0, 0), (675, 219));
    }
}
