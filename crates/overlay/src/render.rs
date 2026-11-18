//! Drawing for widgets, on GPUI's painting: [`Canvas`] turns a widget's fills, lines, circles, paths, and text into
//! GPUI quads, paths, and glyphs in the widget's window. GPUI owns the GPU device, the text system, and the
//! glyph atlas; nothing here calls Direct3D, DirectWrite, or DirectComposition.
//!
//! Widgets draw in widget pixels: screen pixels at the default text size, and `text_scale` screen pixels each for a
//! widget that scales with the text. The canvas maps them to screen pixels so whole widget pixels land on whole
//! screen pixels, then to GPUI's logical pixels at the window's DPI.

use gpui::{
    Bounds, ContentMask, Font, FontFeatures, FontStyle, FontWeight, Hsla, PathBuilder, PathStyle,
    Pixels, Point, SharedString, StrokeOptions, TextRun, Window, point, px, quad, size,
};
use lyon::tessellation::{LineCap, LineJoin};

use crate::config;

/// Straight (non-premultiplied) RGBA color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0x0000_0000);
    pub const WHITE: Self = Self::rgba(0xFFFF_FFFF);

    /// From `0xRRGGBBAA`.
    pub const fn rgba(v: u32) -> Self {
        Self {
            r: ((v >> 24) & 0xFF) as f32 / 255.0,
            g: ((v >> 16) & 0xFF) as f32 / 255.0,
            b: ((v >> 8) & 0xFF) as f32 / 255.0,
            a: (v & 0xFF) as f32 / 255.0,
        }
    }

    /// From `0xRRGGBB` and an alpha.
    pub const fn rgb(v: u32, a: f32) -> Self {
        let c = Self::rgba(v << 8);
        Self { a, ..c }
    }

    fn hsla(self) -> Hsla {
        gpui::Rgba {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
        .into()
    }
}

/// A rectangle in widget or layer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// Horizontal text alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Text sizes available to widgets, in widget pixels. A requested size takes the next of them up, times the text
/// scale.
pub const TEXT_SIZES: [f32; 4] = [12.0, 14.0, 18.0, 26.0];

/// The index into [`TEXT_SIZES`] of the size a widget asks for: the next one up, or the largest.
fn size_index(size: f32) -> usize {
    TEXT_SIZES
        .iter()
        .position(|&s| s >= size)
        .unwrap_or(TEXT_SIZES.len() - 1)
}

/// Characters of numbers, gaps, times, and units. At a text scale other than 1, text made only of them that is
/// wider than its box is left out whole rather than cut, so a number never shows with digits missing.
const NUMBER_CHARS: &str = "0123456789.,:+-/#%kL ";

fn is_number(text: &str) -> bool {
    text.chars().all(|c| NUMBER_CHARS.contains(c))
}

/// The widgets' font: Segoe UI semibold, made once per paint state so drawing text clones it without allocating.
pub fn widget_font() -> Font {
    Font {
        family: SharedString::new_static("Segoe UI"),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::SEMIBOLD,
        style: FontStyle::Normal,
    }
}

/// The scale [`Canvas`] accepts: [`config::text_scale`]'s range, and 1 for anything that is not a number.
pub fn valid_scale(scale: f32) -> f32 {
    let (min, max) = (
        config::text_scale(config::MIN_TEXT_SIZE),
        config::text_scale(config::MAX_TEXT_SIZE),
    );
    if scale.is_finite() {
        scale.clamp(min, max)
    } else {
        1.0
    }
}

/// `v` widget pixels in layer pixels at `scale`. A whole number of widget pixels lands on a whole layer pixel, so
/// edges that are sharp at the default text size stay sharp and edges that meet stay together.
fn scaled_edge(v: f32, scale: f32) -> f32 {
    let p = v * scale;
    if v == v.round() { p.round() } else { p }
}

/// The edges `low` and `high` (`low <= high`) of a span in widget pixels, in layer pixels at `scale`: see
/// [`scaled_edge`]. A span of at least a pixel keeps at least a pixel, so hairlines do not vanish.
fn scaled_span(low: f32, high: f32, scale: f32) -> (f32, f32) {
    let (a, b) = (scaled_edge(low, scale), scaled_edge(high, scale));
    if high - low >= 1.0 && b - a < 1.0 {
        (a, a + 1.0)
    } else {
        (a, b)
    }
}

/// Where one layer of a widget is drawn in its window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerPlace {
    /// The layer's top-left corner in the window, in screen pixels.
    pub x: f32,
    pub y: f32,
    /// The layer's size in screen pixels.
    pub width: f32,
    pub height: f32,
    /// Screen pixels per widget pixel: the text scale for a widget that scales with the text, else 1.
    pub scale: f32,
}

/// What a canvas draws with besides the window: the font, `[general] background_opacity`, and the text scale.
#[derive(Debug, Clone)]
pub struct Style {
    pub font: Font,
    /// `[general] background_opacity`, in percent, for [`Canvas::fill_background`].
    pub background: u8,
    /// `[general] text_size` as a scale ([`config::text_scale`]): text is drawn at [`TEXT_SIZES`] times it.
    pub text_scale: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font: widget_font(),
            background: config::DEFAULT_BACKGROUND_OPACITY,
            text_scale: 1.0,
        }
    }
}

/// Draws one layer of a widget into `window` with `draw`, clipped to the layer. Called from a widget window's
/// paint.
pub fn paint_layer(
    window: &mut Window,
    style: &Style,
    place: LayerPlace,
    draw: impl FnOnce(&mut Canvas<'_>),
) {
    if place.width <= 0.0 || place.height <= 0.0 {
        return;
    }
    // Logical pixels per screen pixel at the window's DPI.
    let logical = 1.0 / window.scale_factor().max(0.01);
    let scale = if place.scale.is_finite() && place.scale > 0.0 {
        place.scale
    } else {
        1.0
    };
    let mask = ContentMask {
        bounds: Bounds::new(
            point(px(place.x * logical), px(place.y * logical)),
            size(px(place.width * logical), px(place.height * logical)),
        ),
    };
    let bounds = (place.width / scale, place.height / scale);
    window.with_content_mask(Some(mask), |window| {
        let mut canvas = Canvas {
            window,
            style,
            logical,
            origin: (place.x, place.y),
            scale,
            bounds,
            area: Rect::new(0.0, 0.0, bounds.0, bounds.1),
        };
        draw(&mut canvas);
    });
}

/// Drawing API for widgets over GPUI's painting. Widgets draw in widget pixels: layer pixels at the default text
/// size, and the text scale's layer pixels each for a widget that scales with the text, so its text, rows, and
/// spacing grow together within its layer. At a scale other than 1, text that does not fit is left out whole
/// instead of cut: text outside the layer, and numbers wider than their box.
pub struct Canvas<'a> {
    window: &'a mut Window,
    style: &'a Style,
    /// Logical pixels per layer pixel.
    logical: f32,
    /// The layer's top-left corner in the window, in screen pixels.
    origin: (f32, f32),
    /// Layer pixels per widget pixel.
    scale: f32,
    /// The layer's width and height in widget pixels.
    bounds: (f32, f32),
    /// The area being drawn, in widget pixels: the whole layer.
    area: Rect,
}

impl Canvas<'_> {
    /// The area being drawn, in widget pixels. Every draw draws the whole layer.
    pub fn area(&self) -> Rect {
        self.area
    }

    /// A point in layer pixels to the window, in logical pixels.
    fn at(&self, x: f32, y: f32) -> Point<Pixels> {
        point(
            px((self.origin.0 + x) * self.logical),
            px((self.origin.1 + y) * self.logical),
        )
    }

    /// A rectangle in layer pixels to the window, in logical pixels.
    fn window_bounds(&self, rect: Rect) -> Bounds<Pixels> {
        Bounds::new(
            self.at(rect.x, rect.y),
            size(px(rect.w * self.logical), px(rect.h * self.logical)),
        )
    }

    /// `rect` in layer pixels: see [`scaled_edge`].
    fn layer_rect(&self, rect: Rect) -> Rect {
        if self.scale == 1.0 {
            return rect;
        }
        let (x0, x1) = scaled_span(rect.x, rect.x + rect.w, self.scale);
        let (y0, y1) = scaled_span(rect.y, rect.y + rect.h, self.scale);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// A point in layer pixels.
    fn layer_point(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (x * self.scale, y * self.scale)
    }

    /// Fills a rectangle given in layer pixels.
    fn fill_layer(&mut self, rect: Rect, color: Color) {
        if rect.w <= 0.0 || rect.h <= 0.0 || color.a <= 0.0 {
            return;
        }
        let bounds = self.window_bounds(rect);
        self.window.paint_quad(gpui::fill(bounds, color.hsla()));
    }

    pub fn fill(&mut self, rect: Rect, color: Color) {
        let rect = self.layer_rect(rect);
        self.fill_layer(rect, color);
    }

    /// Fills `rect` with a widget's background: its panel, or a band behind content such as a header. `color`'s
    /// alpha is the background's own (from the widget's `opacity`), which `[general] background_opacity` scales
    /// (see [`config::background_alpha`]). Text, graphs, lines, and markers use [`Canvas::fill`] and stay as they
    /// are.
    pub fn fill_background(&mut self, rect: Rect, color: Color) {
        let a = config::background_alpha(color.a, self.style.background);
        if a > 0.0 {
            self.fill(rect, Color { a, ..color });
        }
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Color) {
        let (from, to, width) = self.layer_line(from, to, width);
        if width <= 0.0 {
            return;
        }
        // Level and upright lines are rectangles, as sharp as fills; others are stroked paths with flat ends.
        let half = width / 2.0;
        if from.1 == to.1 {
            let (x0, x1) = (from.0.min(to.0), from.0.max(to.0));
            self.fill_layer(Rect::new(x0, from.1 - half, x1 - x0, width), color);
        } else if from.0 == to.0 {
            let (y0, y1) = (from.1.min(to.1), from.1.max(to.1));
            self.fill_layer(Rect::new(from.0 - half, y0, width, y1 - y0), color);
        } else {
            let mut path = PathBuilder::stroke(px(width * self.logical));
            path.move_to(self.at(from.0, from.1));
            path.line_to(self.at(to.0, to.1));
            if let Ok(path) = path.build() {
                self.window.paint_path(path, color.hsla());
            }
        }
    }

    /// A line's ends and width in layer pixels. A level or upright line keeps its edges on whole pixels when they
    /// were (see [`scaled_edge`]), so a hairline stays sharp.
    fn layer_line(
        &self,
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
    ) -> ((f32, f32), (f32, f32), f32) {
        let s = self.scale;
        if s == 1.0 {
            return (from, to, width);
        }
        let half = width.abs() / 2.0;
        if from.1 == to.1 {
            let (top, bottom) = scaled_span(from.1 - half, from.1 + half, s);
            let y = (top + bottom) / 2.0;
            let (x0, x1) = (scaled_edge(from.0, s), scaled_edge(to.0, s));
            ((x0, y), (x1, y), bottom - top)
        } else if from.0 == to.0 {
            let (left, right) = scaled_span(from.0 - half, from.0 + half, s);
            let x = (left + right) / 2.0;
            let (y0, y1) = (scaled_edge(from.1, s), scaled_edge(to.1, s));
            ((x, y0), (x, y1), right - left)
        } else {
            (self.layer_point(from), self.layer_point(to), width * s)
        }
    }

    /// Strokes a circle around `center`, the stroke centered on the radius.
    pub fn circle(&mut self, center: (f32, f32), radius: f32, width: f32, color: Color) {
        let (cx, cy) = self.layer_point(center);
        let (r, w) = (radius * self.scale, width * self.scale);
        let outer = r + w / 2.0;
        if outer <= 0.0 || w <= 0.0 || color.a <= 0.0 {
            return;
        }
        let bounds =
            self.window_bounds(Rect::new(cx - outer, cy - outer, outer * 2.0, outer * 2.0));
        self.window.paint_quad(quad(
            bounds,
            px(outer * self.logical),
            gpui::transparent_black(),
            px(w * self.logical),
            color.hsla(),
            gpui::BorderStyle::Solid,
        ));
    }

    /// Fills a circle around `center`.
    pub fn fill_circle(&mut self, center: (f32, f32), radius: f32, color: Color) {
        let (cx, cy) = self.layer_point(center);
        let r = radius * self.scale;
        if r <= 0.0 || color.a <= 0.0 {
            return;
        }
        let bounds = self.window_bounds(Rect::new(cx - r, cy - r, r * 2.0, r * 2.0));
        self.window.paint_quad(quad(
            bounds,
            px(r * self.logical),
            color.hsla(),
            px(0.0),
            gpui::transparent_black(),
            gpui::BorderStyle::Solid,
        ));
    }

    /// Strokes the line through `points` with round joins, back to the first point when `closed`.
    pub fn polyline(&mut self, points: &[[f32; 2]], closed: bool, width: f32, color: Color) {
        self.stroke(points.iter().map(|p| (p[0], p[1])), closed, width, color);
    }

    /// Strokes the line through `points` (widget pixels) with round joins and ends, back to the first point when
    /// `closed`. Fewer than two points draw nothing.
    pub fn stroke(
        &mut self,
        points: impl IntoIterator<Item = (f32, f32)>,
        closed: bool,
        width: f32,
        color: Color,
    ) {
        let width = width * self.scale;
        if width <= 0.0 || color.a <= 0.0 {
            return;
        }
        let options = StrokeOptions::default()
            .with_line_width(width * self.logical)
            .with_line_join(LineJoin::Round)
            .with_line_cap(LineCap::Round);
        let mut path = PathBuilder::default().with_style(PathStyle::Stroke(options));
        let mut count = 0usize;
        for p in points {
            let (x, y) = self.layer_point(p);
            if count == 0 {
                path.move_to(self.at(x, y));
            } else {
                path.line_to(self.at(x, y));
            }
            count += 1;
        }
        if count < 2 {
            return;
        }
        if closed {
            path.close();
        }
        if let Ok(path) = path.build() {
            self.window.paint_path(path, color.hsla());
        }
    }

    /// The font size in logical pixels for a size in widget pixels: the next of [`TEXT_SIZES`] up, times the text
    /// scale.
    fn font_size(&self, size: f32) -> Pixels {
        px(TEXT_SIZES[size_index(size)] * self.style.text_scale * self.logical)
    }

    fn run(&self, text: &str, color: Color) -> TextRun {
        TextRun {
            len: text.len(),
            font: self.style.font.clone(),
            color: color.hsla(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    /// Draws one line of text vertically centered in `rect`, clipped to it. `size` is in widget pixels, from the
    /// next of [`TEXT_SIZES`] up.
    ///
    /// At a text scale other than 1, a `rect` that reaches outside the layer draws nothing, and a number wider
    /// than `rect` draws nothing, so text that does not fit a widget at a large text size is left out whole rather
    /// than cut at the widget's edge.
    pub fn text(&mut self, text: &str, rect: Rect, size: f32, align: Align, color: Color) {
        if text.is_empty() || color.a <= 0.0 {
            return;
        }
        if self.scale != 1.0 && !self.inside(rect) {
            return;
        }
        let rect = self.layer_rect(rect);
        let font_size = self.font_size(size);
        let run = self.run(text, color);
        let layout = self
            .window
            .text_system()
            .layout_line(text, font_size, &[run], None);
        let width = f32::from(layout.width) / self.logical;
        if self.scale != 1.0 && width > rect.w + 0.5 && is_number(text) {
            return;
        }
        let x = match align {
            Align::Left => rect.x,
            Align::Center => rect.x + (rect.w - width) / 2.0,
            Align::Right => rect.x + rect.w - width,
        };
        // The line box is the font's ascent and descent, centered in the rectangle as DirectWrite centers a
        // paragraph; glyph positions are relative to the baseline.
        let line = f32::from(layout.ascent + layout.descent) / self.logical;
        // On a whole pixel, so text stays as sharp as it was before GPUI drew it.
        let baseline =
            (rect.y + (rect.h - line) / 2.0 + f32::from(layout.ascent) / self.logical).round();
        let origin = self.at(x, baseline);
        let mask = ContentMask {
            bounds: self.window_bounds(rect),
        };
        let hsla = color.hsla();
        self.window.with_content_mask(Some(mask), |window| {
            for run in &layout.runs {
                for glyph in &run.glyphs {
                    let at = point(origin.x + glyph.position.x, origin.y + glyph.position.y);
                    let _ = if glyph.is_emoji {
                        window.paint_emoji(at, run.font_id, glyph.id, layout.font_size)
                    } else {
                        window.paint_glyph(at, run.font_id, glyph.id, layout.font_size, hsla)
                    };
                }
            }
        });
    }

    /// Whether `text` at `size` fits the width of `rect` (both in widget pixels), as GPUI's text system measures
    /// it.
    pub fn fits(&self, text: &str, rect: Rect, size: f32) -> bool {
        if text.is_empty() {
            return true;
        }
        let font_size = self.font_size(size);
        let run = self.run(text, Color::WHITE);
        let layout = self
            .window
            .text_system()
            .layout_line(text, font_size, &[run], None);
        f32::from(layout.width) / self.logical <= self.layer_rect(rect).w + 0.5
    }

    /// Whether `rect`, in widget pixels, lies within the layer. A pixel of slack covers the layer's size in widget
    /// pixels, which is rarely whole.
    fn inside(&self, rect: Rect) -> bool {
        let (w, h) = self.bounds;
        rect.x >= -1.0 && rect.y >= -1.0 && rect.x + rect.w <= w + 1.0 && rect.y + rect.h <= h + 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_widget_pixels_land_on_whole_layer_pixels() {
        for scale in [0.7f32, 0.75, 1.15, 1.25, 1.35, 1.5] {
            for v in 0..400 {
                let p = scaled_edge(v as f32, scale);
                assert_eq!(p, p.round(), "{v} at {scale}");
                assert!((p - v as f32 * scale).abs() <= 0.5, "{v} at {scale}");
            }
        }
        // Between pixels, a coordinate is scaled as it is.
        assert_eq!(scaled_edge(10.5, 1.5), 15.75);
        assert_eq!(scaled_edge(33.25, 1.0), 33.25);
        // At the default text size nothing moves.
        assert_eq!(scaled_edge(17.0, 1.0), 17.0);
    }

    #[test]
    fn spans_keep_their_edges_together_and_hairlines_visible() {
        // Two rows that meet still meet.
        let (_, end) = scaled_span(0.0, 24.0, 1.15);
        let (start, _) = scaled_span(24.0, 48.0, 1.15);
        assert_eq!(end, start);
        // A one pixel line keeps a whole pixel at any scale.
        for scale in [0.7f32, 0.75, 1.15, 1.5] {
            for at in 0..100 {
                let (a, b) = scaled_span(at as f32, at as f32 + 1.0, scale);
                assert!(b - a >= 1.0, "{at} at {scale}: {a}..{b}");
                assert_eq!((a, b), (a.round(), b.round()), "sharp");
            }
        }
        assert_eq!(scaled_span(2.0, 3.0, 0.75), (2.0, 3.0), "not collapsed");
        assert_eq!(
            scaled_span(0.25, 0.75, 0.75),
            (0.1875, 0.5625),
            "thinner lines as they are"
        );
    }

    #[test]
    fn scales_outside_the_setting_are_brought_into_it() {
        assert_eq!(valid_scale(1.0), 1.0);
        assert_eq!(valid_scale(1.25), 1.25);
        assert_eq!(valid_scale(0.2), 0.7);
        assert_eq!(valid_scale(9.0), 1.5);
        assert_eq!(valid_scale(f32::NAN), 1.0);
        assert_eq!(valid_scale(f32::INFINITY), 1.0);
    }

    #[test]
    fn requested_text_sizes_pick_the_next_size_up() {
        assert_eq!(size_index(11.0), 0);
        assert_eq!(size_index(13.0), 1);
        assert_eq!(size_index(24.0), 3);
        assert_eq!(size_index(40.0), 3, "the largest");
    }

    #[test]
    fn numbers_are_told_from_other_text() {
        assert!(is_number("1:45.345"));
        assert!(is_number("+12.4 L"));
        assert!(!is_number("PIT"));
        assert!(!is_number("Lap 3"));
    }

    #[test]
    fn colors_keep_their_alpha() {
        let c = Color::rgb(0x10_1418, 0.82);
        assert!((c.hsla().a - 0.82).abs() < 1e-6);
        assert_eq!(Color::TRANSPARENT.hsla().a, 0.0);
    }
}
