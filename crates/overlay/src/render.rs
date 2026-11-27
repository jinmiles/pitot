//! Drawing for widgets, on GPUI's painting: [`Canvas`] turns a widget's fills, lines, circles, paths, and text into
//! GPUI quads, paths, and glyphs in the widget's window. GPUI owns the GPU device, the text system, and the
//! glyph atlas; nothing here calls Direct3D, DirectWrite, or DirectComposition.
//!
//! Widgets draw in widget pixels: screen pixels at the default text size, and `text_scale` screen pixels each for a
//! widget that scales with the text. The canvas maps them to screen pixels so whole widget pixels land on whole
//! screen pixels, then to GPUI's logical pixels at the window's DPI.
//!
//! GPUI draws a window whole whenever it draws it, so a widget's every shape and every line of text is painted
//! again each time. What a window keeps between paints ([`PaintCache`]) keeps that from shaping text and
//! tessellating with fresh buffers each time: lines of text are shaped once and kept while they are drawn, numbers
//! are drawn from glyphs measured once per size, and strokes are tessellated by one retained lyon tessellator
//! straight into GPUI paths.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    Bounds, ContentMask, Font, FontFeatures, FontId, FontStyle, FontWeight, GlyphId, Hsla,
    LineLayout, Path, Pixels, Point, SharedString, StrokeOptions, TextRun, Window, point, px, quad,
    size,
};
use lyon::math::Point as LyonPoint;
use lyon::tessellation::{
    BuffersBuilder, LineCap, LineJoin, StrokeTessellator, StrokeVertex, VertexBuffers,
};

use crate::config;

/// Evaluates `$e`, counting the heap allocations it makes as a paint's `$kind` in a `--features alloc-count` build
/// (see [`crate::alloc_count::PaintKind`]).
macro_rules! counted {
    ($kind:ident, $e:expr) => {{
        #[cfg(feature = "alloc-count")]
        let _counted = crate::alloc_count::Phase::In(crate::alloc_count::PaintKind::$kind).enter();
        $e
    }};
}

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

/// A line segment from one point to another, in widget pixels.
pub type Segment = ((f32, f32), (f32, f32));

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

/// Characters drawn from glyphs measured alone ([`Glyphs`]) when a whole line is made of them: those of numbers. In
/// Segoe UI bold they have no kerning or contextual forms between them: shaped whole by GPUI, every pair and
/// triple of them at the font sizes of every text size gives the glyphs measured alone, each exactly where the
/// advances of those before it end, and the sum of the advances as the width (trailing spaces included).
const GLYPH_CHARS: &[u8] = NUMBER_CHARS.as_bytes();

/// For each ASCII byte, its index in [`GLYPH_CHARS`] plus one, or 0 for a byte not in it.
const GLYPH_INDEX: [u8; 128] = {
    let mut table = [0u8; 128];
    let mut i = 0;
    while i < GLYPH_CHARS.len() {
        table[GLYPH_CHARS[i] as usize] = i as u8 + 1;
        i += 1;
    }
    table
};

/// The longest line drawn from measured glyphs; longer ones are shaped.
const MAX_GLYPH_LINE: usize = 24;

/// Corner radius of panels, chips, badges, bands, and blocks, in widget pixels.
pub const RADIUS: f32 = 4.0;

/// The alpha of the shadow under text when backgrounds are off (`[general] background_opacity` = 0). It falls as the
/// panel behind the text grows opaque, to none on an opaque panel: the shadow keeps text readable over a bright
/// scene, and is not needed where the panel already does that.
const SHADOW_ALPHA: f32 = 0.9;

/// The alpha of the shadow under text at `[general] background_opacity` = `percent`.
fn shadow_alpha(percent: u8) -> f32 {
    let panel = config::background_alpha(config::DEFAULT_WIDGET_OPACITY, percent);
    SHADOW_ALPHA * (1.0 - panel).clamp(0.0, 1.0)
}

/// The widgets' font: Segoe UI bold, whose digits all have one width, so a number that changes does not move.
/// Made once per paint state so drawing text clones it without allocating.
pub fn widget_font() -> Font {
    Font {
        family: SharedString::new_static("Segoe UI"),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::BOLD,
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

/// A line's ends and width in layer pixels at `scale` layer pixels per widget pixel. A level or upright line keeps
/// its edges on whole pixels when they were (see [`scaled_edge`]), so a hairline stays sharp.
fn layer_line(
    scale: f32,
    from: (f32, f32),
    to: (f32, f32),
    width: f32,
) -> ((f32, f32), (f32, f32), f32) {
    let s = scale;
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
        ((from.0 * s, from.1 * s), (to.0 * s, to.1 * s), width * s)
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

/// What a widget window keeps from one paint to the next: its lines of text and number glyphs, and the stroke
/// tessellator with its buffers. Painting with it allocates only in the GPUI calls that must (a path's vertices,
/// handed to GPUI's scene), and when text is drawn that was not drawn in the previous paint.
#[derive(Default)]
pub struct PaintCache {
    text: TextCache,
    strokes: Strokes,
}

impl PaintCache {
    /// Ends a paint of the whole window: lines of text that were not drawn in it are let go.
    pub fn finish_paint(&mut self) {
        self.text.finish_paint();
    }
}

impl std::fmt::Debug for PaintCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaintCache")
            .field("sizes", &self.text.sizes.len())
            .finish_non_exhaustive()
    }
}

/// A glyph measured alone: its id and advance, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Measured {
    NotYet,
    Glyph(GlyphId, f32),
    /// Shaped alone it is not one glyph of the line's font: such lines are shaped whole.
    No,
}

/// The glyphs of [`GLYPH_CHARS`] at one font size, each measured from GPUI's layout of it alone the first time it
/// is drawn.
#[derive(Debug, Clone)]
struct Glyphs {
    font_id: Option<FontId>,
    /// The line box's ascent and descent, which do not depend on the characters.
    ascent: f32,
    descent: f32,
    chars: [Measured; GLYPH_CHARS.len()],
}

impl Default for Glyphs {
    fn default() -> Self {
        Self {
            font_id: None,
            ascent: 0.0,
            descent: 0.0,
            chars: [Measured::NotYet; GLYPH_CHARS.len()],
        }
    }
}

/// Lines of text at one font size, each with the paint it was last drawn in.
#[derive(Default)]
struct SizedText {
    /// The font size in logical pixels.
    font_size: f32,
    lines: HashMap<String, (Arc<LineLayout>, u64)>,
    glyphs: Glyphs,
    /// The paint the size was last drawn in.
    used: u64,
}

/// A window's text: shaped lines and number glyphs per font size, for the window's font.
#[derive(Default)]
struct TextCache {
    font: Option<Font>,
    sizes: Vec<SizedText>,
    /// Paints finished.
    paint: u64,
}

impl TextCache {
    /// Text from now on is in `font`; text kept in another font is let go.
    fn use_font(&mut self, font: &Font) {
        if self.font.as_ref() != Some(font) {
            self.font = Some(font.clone());
            self.sizes.clear();
        }
    }

    /// The text at `font_size`.
    fn sized(&mut self, font_size: f32) -> &mut SizedText {
        let paint = self.paint;
        let index = match self.sizes.iter().position(|s| s.font_size == font_size) {
            Some(index) => index,
            None => {
                self.sizes.push(SizedText {
                    font_size,
                    ..SizedText::default()
                });
                self.sizes.len() - 1
            }
        };
        let sized = &mut self.sizes[index];
        sized.used = paint;
        sized
    }

    fn finish_paint(&mut self) {
        let paint = self.paint;
        self.sizes.retain(|s| s.used == paint);
        for sized in &mut self.sizes {
            sized.lines.retain(|_, (_, used)| *used == paint);
        }
        self.paint += 1;
    }
}

/// A line of text ready to paint, in logical pixels.
#[allow(
    clippy::large_enum_variant,
    reason = "on the stack, so drawing a number does not allocate"
)]
enum Line {
    /// Shaped by GPUI.
    Shaped(Arc<LineLayout>),
    /// Drawn from measured glyphs: the first `len` of `glyphs`, each with its x from the line's start.
    Glyphs {
        font_id: FontId,
        glyphs: [(GlyphId, f32); MAX_GLYPH_LINE],
        len: usize,
        width: f32,
        ascent: f32,
        descent: f32,
    },
}

impl Line {
    fn width(&self) -> f32 {
        match self {
            Self::Shaped(layout) => f32::from(layout.width),
            Self::Glyphs { width, .. } => *width,
        }
    }

    fn ascent(&self) -> f32 {
        match self {
            Self::Shaped(layout) => f32::from(layout.ascent),
            Self::Glyphs { ascent, .. } => *ascent,
        }
    }

    fn descent(&self) -> f32 {
        match self {
            Self::Shaped(layout) => f32::from(layout.descent),
            Self::Glyphs { descent, .. } => *descent,
        }
    }
}

/// lyon's stroke tessellator and its output, kept for every stroke a window draws.
struct Strokes {
    tessellator: StrokeTessellator,
    buffers: VertexBuffers<LyonPoint, u32>,
}

impl Default for Strokes {
    fn default() -> Self {
        Self {
            tessellator: StrokeTessellator::new(),
            buffers: VertexBuffers::new(),
        }
    }
}

/// Leaves out the points of a line that lie inside a level or upright run of it, which change nothing that is
/// drawn: the line through the run's ends covers what the line through every point covers, joins included. The
/// first and last points stay. Points are in GPUI's logical pixels, as the tessellator gets them.
#[derive(Debug, Default)]
struct RunFilter {
    last: Option<LyonPoint>,
    pending: Option<LyonPoint>,
}

impl RunFilter {
    /// Takes the next point, and returns the point to pass on, if any.
    fn push(&mut self, next: LyonPoint) -> Option<LyonPoint> {
        let Some(last) = self.last else {
            self.last = Some(next);
            return Some(next);
        };
        let pending = self.pending.replace(next)?;
        let between = |a: f32, b: f32, c: f32| (a <= b && b <= c) || (a >= b && b >= c);
        let level =
            last.y == pending.y && pending.y == next.y && between(last.x, pending.x, next.x);
        let upright =
            last.x == pending.x && pending.x == next.x && between(last.y, pending.y, next.y);
        if level || upright {
            None
        } else {
            self.last = Some(pending);
            Some(pending)
        }
    }

    /// The point still held back, which ends the line.
    fn finish(&mut self) -> Option<LyonPoint> {
        self.pending.take()
    }
}

/// Draws one layer of a widget into `window` with `draw`, clipped to the layer, with what the window keeps between
/// paints. Called from a widget window's paint, which calls [`PaintCache::finish_paint`] after its last layer.
pub fn paint_layer(
    window: &mut Window,
    style: &Style,
    cache: &mut PaintCache,
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
    cache.text.use_font(&style.font);
    window.with_content_mask(Some(mask), |window| {
        let mut canvas = Canvas {
            window,
            style,
            cache,
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
    cache: &'a mut PaintCache,
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

    /// Fills a rectangle given in layer pixels, its corners rounded by `radius` layer pixels.
    fn fill_layer(&mut self, rect: Rect, color: Color, radius: f32) {
        if rect.w <= 0.0 || rect.h <= 0.0 || color.a <= 0.0 {
            return;
        }
        let bounds = self.window_bounds(rect);
        let quad = gpui::fill(bounds, color.hsla());
        let quad = if radius > 0.0 {
            // No larger than half the shorter side, so a thin bar stays a bar.
            let radius = radius.min(rect.w / 2.0).min(rect.h / 2.0);
            quad.corner_radii(px(radius * self.logical))
        } else {
            quad
        };
        counted!(Draw, self.window.paint_quad(quad));
    }

    /// Fills a rectangle with square corners: lines, graph bars, and other marks that meet edge to edge.
    pub fn fill(&mut self, rect: Rect, color: Color) {
        let rect = self.layer_rect(rect);
        self.fill_layer(rect, color, 0.0);
    }

    /// Fills a rectangle with corners rounded by [`RADIUS`]: a chip, a badge, a row's band, a block.
    pub fn fill_rounded(&mut self, rect: Rect, color: Color) {
        let rect = self.layer_rect(rect);
        let radius = RADIUS * self.scale;
        self.fill_layer(rect, color, radius);
    }

    /// Fills `rect` with a widget's background, corners rounded by [`RADIUS`]: its panel, or a band or block drawn
    /// as background, such as a class header. `color`'s alpha is the background's own (from the widget's
    /// `opacity`), which `[general] background_opacity` scales (see [`config::background_alpha`]). Text, graphs,
    /// lines, and markers stay as they are.
    pub fn fill_background(&mut self, rect: Rect, color: Color) {
        let a = config::background_alpha(color.a, self.style.background);
        if a > 0.0 {
            self.fill_rounded(rect, Color { a, ..color });
        }
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Color) {
        self.lines(&[(from, to)], width, color);
    }

    /// Draws each of `segments` as [`Canvas::line`] does: level and upright ones as rectangles, as sharp as fills,
    /// and the others with flat ends in one path.
    pub fn lines(&mut self, segments: &[Segment], width: f32, color: Color) {
        let scale = self.scale;
        let mut slanted = false;
        let mut path_width = 0.0;
        for &(from, to) in segments {
            let (from, to, width) = layer_line(scale, from, to, width);
            if width <= 0.0 {
                continue;
            }
            let half = width / 2.0;
            if from.1 == to.1 {
                let (x0, x1) = (from.0.min(to.0), from.0.max(to.0));
                self.fill_layer(Rect::new(x0, from.1 - half, x1 - x0, width), color, 0.0);
            } else if from.0 == to.0 {
                let (y0, y1) = (from.1.min(to.1), from.1.max(to.1));
                self.fill_layer(Rect::new(from.0 - half, y0, width, y1 - y0), color, 0.0);
            } else {
                slanted = true;
                path_width = width;
            }
        }
        if !slanted || color.a <= 0.0 {
            return;
        }
        // Each slanted segment is a sub-path of its own, as a path of one line each would be.
        let (origin, logical) = (self.origin, self.logical);
        let options = StrokeOptions::default().with_line_width(path_width * logical);
        let at =
            |(x, y): (f32, f32)| LyonPoint::new((origin.0 + x) * logical, (origin.1 + y) * logical);
        let strokes = &mut self.cache.strokes;
        strokes.buffers.vertices.clear();
        strokes.buffers.indices.clear();
        let built = {
            let mut output =
                BuffersBuilder::new(&mut strokes.buffers, |v: StrokeVertex| v.position());
            let mut builder = strokes.tessellator.builder(&options, &mut output);
            for &(from, to) in segments {
                let (from, to, width) = layer_line(scale, from, to, width);
                if width <= 0.0 || from.1 == to.1 || from.0 == to.0 {
                    continue;
                }
                builder.begin(at(from));
                builder.line_to(at(to));
                builder.end(false);
            }
            counted!(Path, builder.build())
        };
        if built.is_ok() {
            self.paint_strokes(color);
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
        counted!(
            Draw,
            self.window.paint_quad(quad(
                bounds,
                px(outer * self.logical),
                gpui::transparent_black(),
                px(w * self.logical),
                color.hsla(),
                gpui::BorderStyle::Solid,
            ))
        );
    }

    /// Fills a circle around `center`.
    pub fn fill_circle(&mut self, center: (f32, f32), radius: f32, color: Color) {
        let (cx, cy) = self.layer_point(center);
        let r = radius * self.scale;
        if r <= 0.0 || color.a <= 0.0 {
            return;
        }
        let bounds = self.window_bounds(Rect::new(cx - r, cy - r, r * 2.0, r * 2.0));
        counted!(
            Draw,
            self.window.paint_quad(quad(
                bounds,
                px(r * self.logical),
                color.hsla(),
                px(0.0),
                gpui::transparent_black(),
                gpui::BorderStyle::Solid,
            ))
        );
    }

    /// Strokes the line through `points` with round joins, back to the first point when `closed`.
    pub fn polyline(&mut self, points: &[[f32; 2]], closed: bool, width: f32, color: Color) {
        self.stroke(points.iter().map(|p| (p[0], p[1])), closed, width, color);
    }

    /// Strokes the line through `points` (widget pixels) with round joins and ends, back to the first point when
    /// `closed`. Fewer than two points draw nothing. Points inside a level or upright run are left out, which
    /// changes nothing drawn ([`RunFilter`]).
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
        let (origin, logical, scale) = (self.origin, self.logical, self.scale);
        let options = StrokeOptions::default()
            .with_line_width(width * logical)
            .with_line_join(LineJoin::Round)
            .with_line_cap(LineCap::Round);
        let strokes = &mut self.cache.strokes;
        strokes.buffers.vertices.clear();
        strokes.buffers.indices.clear();
        let (count, built) = {
            let mut output =
                BuffersBuilder::new(&mut strokes.buffers, |v: StrokeVertex| v.position());
            let mut builder = strokes.tessellator.builder(&options, &mut output);
            let mut filter = RunFilter::default();
            let mut count = 0usize;
            let mut pass = |p: LyonPoint, count: &mut usize| {
                if *count == 0 {
                    builder.begin(p);
                } else {
                    builder.line_to(p);
                }
                *count += 1;
            };
            for (x, y) in points {
                let p = LyonPoint::new(
                    (origin.0 + x * scale) * logical,
                    (origin.1 + y * scale) * logical,
                );
                if let Some(p) = filter.push(p) {
                    pass(p, &mut count);
                }
            }
            if let Some(p) = filter.finish() {
                pass(p, &mut count);
            }
            if count == 0 {
                (0, Ok(()))
            } else {
                builder.end(closed && count > 1);
                (count, counted!(Path, builder.build()))
            }
        };
        if count >= 2 && built.is_ok() {
            self.paint_strokes(color);
        }
    }

    /// Paints the tessellator's output as one GPUI path, triangle by triangle as GPUI's own path builder makes it.
    fn paint_strokes(&mut self, color: Color) {
        let buffers = &self.cache.strokes.buffers;
        let Some(first) = buffers.vertices.first() else {
            return;
        };
        if buffers.indices.len() < 3 {
            return;
        }
        let at = |index: u32| {
            let v = buffers.vertices[index as usize];
            point(px(v.x), px(v.y))
        };
        let solid = (point(0., 1.), point(0., 1.), point(0., 1.));
        let path = counted!(Path, {
            let mut path = Path::new(point(px(first.x), px(first.y)));
            path.vertices.reserve_exact(buffers.indices.len());
            for triangle in buffers.indices.as_chunks::<3>().0 {
                path.push_triangle((at(triangle[0]), at(triangle[1]), at(triangle[2])), solid);
            }
            path
        });
        counted!(Path, self.window.paint_path(path, color.hsla()));
    }

    /// The font size in logical pixels for a size in widget pixels: the next of [`TEXT_SIZES`] up, times the text
    /// scale.
    fn font_size(&self, size: f32) -> f32 {
        TEXT_SIZES[size_index(size)] * self.style.text_scale * self.logical
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

    /// `text` at `font_size` (logical pixels), ready to paint: from measured glyphs when it is made of
    /// [`GLYPH_CHARS`], else shaped by GPUI once and kept while it is drawn in every paint.
    fn text_line(&mut self, text: &str, font_size: f32) -> Line {
        if let Some(line) = self.glyph_line(text, font_size) {
            return line;
        }
        let paint = self.cache.text.paint;
        let sized = self.cache.text.sized(font_size);
        if let Some((layout, used)) = sized.lines.get_mut(text) {
            *used = paint;
            return Line::Shaped(Arc::clone(layout));
        }
        let run = self.run(text, Color::WHITE);
        let layout = counted!(
            Text,
            self.window
                .text_system()
                .layout_line(text, px(font_size), &[run], None)
        );
        // Kept under a copy of the text: the overlay's own allocation, made only for text new to the window.
        self.cache
            .text
            .sized(font_size)
            .lines
            .insert(text.to_owned(), (Arc::clone(&layout), paint));
        Line::Shaped(layout)
    }

    /// `text` from glyphs measured alone, when every character is one of [`GLYPH_CHARS`] and it is at most
    /// [`MAX_GLYPH_LINE`] long.
    fn glyph_line(&mut self, text: &str, font_size: f32) -> Option<Line> {
        let bytes = text.as_bytes();
        if bytes.len() > MAX_GLYPH_LINE {
            return None;
        }
        let mut indices = [0usize; MAX_GLYPH_LINE];
        for (slot, &byte) in indices.iter_mut().zip(bytes) {
            let index = *GLYPH_INDEX.get(usize::from(byte))?;
            *slot = usize::from(index.checked_sub(1)?);
        }
        let indices = &indices[..bytes.len()];
        loop {
            let measured = &self.cache.text.sized(font_size).glyphs;
            let mut glyphs = [(GlyphId(0), 0.0); MAX_GLYPH_LINE];
            let mut x = 0.0;
            let mut missing = None;
            for (n, &index) in indices.iter().enumerate() {
                match measured.chars[index] {
                    Measured::Glyph(id, advance) => {
                        glyphs[n] = (id, x);
                        x += advance;
                    }
                    Measured::No => return None,
                    Measured::NotYet => {
                        missing = Some(index);
                        break;
                    }
                }
            }
            match missing {
                None => {
                    return Some(Line::Glyphs {
                        font_id: measured.font_id?,
                        glyphs,
                        len: indices.len(),
                        width: x,
                        ascent: measured.ascent,
                        descent: measured.descent,
                    });
                }
                // Measured once per size; then the line is put together again.
                Some(index) => self.measure(index, font_size),
            }
        }
    }

    /// Measures `GLYPH_CHARS[index]` at `font_size` from GPUI's layout of it alone.
    fn measure(&mut self, index: usize, font_size: f32) {
        let Some(text) = GLYPH_CHARS
            .get(index..=index)
            .and_then(|c| std::str::from_utf8(c).ok())
        else {
            if let Some(c) = self.cache.text.sized(font_size).glyphs.chars.get_mut(index) {
                *c = Measured::No;
            }
            return;
        };
        let run = self.run(text, Color::WHITE);
        let layout = counted!(
            Text,
            self.window
                .text_system()
                .layout_line(text, px(font_size), &[run], None)
        );
        let glyphs = &mut self.cache.text.sized(font_size).glyphs;
        let alone = match layout.runs.as_slice() {
            [run] if glyphs.font_id.is_none_or(|id| id == run.font_id) => {
                match run.glyphs.as_slice() {
                    [glyph] if !glyph.is_emoji && glyph.position == Point::default() => {
                        Some((run.font_id, glyph.id))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let Some((font_id, id)) = alone else {
            glyphs.chars[index] = Measured::No;
            return;
        };
        glyphs.font_id = Some(font_id);
        glyphs.ascent = f32::from(layout.ascent);
        glyphs.descent = f32::from(layout.descent);
        glyphs.chars[index] = Measured::Glyph(id, f32::from(layout.width));
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
        let line = self.text_line(text, font_size);
        let width = line.width() / self.logical;
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
        let line_height = (line.ascent() + line.descent()) / self.logical;
        // On a whole pixel, so text stays as sharp as it was before GPUI drew it.
        let baseline =
            (rect.y + (rect.h - line_height) / 2.0 + line.ascent() / self.logical).round();
        let origin = self.at(x, baseline);
        let mask = ContentMask {
            bounds: self.window_bounds(rect),
        };
        let font_size = px(font_size);
        // A shadow one screen pixel down and right, under text the panel does not fully set off from the scene.
        let shadow = shadow_alpha(self.style.background) * color.a;
        let step = px(self.logical);
        let passes = [(step, Color::rgb(0x00_0000, shadow)), (px(0.0), color)];
        let passes = if shadow > 0.0 {
            &passes[..]
        } else {
            &passes[1..]
        };
        self.window.with_content_mask(Some(mask), |window| {
            for &(offset, color) in passes {
                let hsla = color.hsla();
                let origin = point(origin.x + offset, origin.y + offset);
                match &line {
                    Line::Shaped(layout) => {
                        for run in &layout.runs {
                            for glyph in &run.glyphs {
                                // An emoji draws in its own colors, so it has no shadow.
                                if glyph.is_emoji && offset > px(0.0) {
                                    continue;
                                }
                                let at =
                                    point(origin.x + glyph.position.x, origin.y + glyph.position.y);
                                let _ = counted!(
                                    Draw,
                                    if glyph.is_emoji {
                                        window.paint_emoji(
                                            at,
                                            run.font_id,
                                            glyph.id,
                                            layout.font_size,
                                        )
                                    } else {
                                        window.paint_glyph(
                                            at,
                                            run.font_id,
                                            glyph.id,
                                            layout.font_size,
                                            hsla,
                                        )
                                    }
                                );
                            }
                        }
                    }
                    Line::Glyphs {
                        font_id,
                        glyphs,
                        len,
                        ..
                    } => {
                        for &(id, x) in &glyphs[..*len] {
                            let at = point(origin.x + px(x), origin.y);
                            let _ = counted!(
                                Draw,
                                window.paint_glyph(at, *font_id, id, font_size, hsla)
                            );
                        }
                    }
                }
            }
        });
    }

    /// Whether `text` at `size` fits the width of `rect` (both in widget pixels), as GPUI's text system measures
    /// it.
    pub fn fits(&mut self, text: &str, rect: Rect, size: f32) -> bool {
        if text.is_empty() {
            return true;
        }
        let font_size = self.font_size(size);
        let width = self.text_line(text, font_size).width();
        width / self.logical <= self.layer_rect(rect).w + 0.5
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
    fn glyph_characters_are_single_bytes() {
        for &c in GLYPH_CHARS {
            assert!(c.is_ascii(), "{c}");
        }
    }

    #[test]
    fn colors_keep_their_alpha() {
        let c = Color::rgb(0x14_1414, 0.82);
        assert!((c.hsla().a - 0.82).abs() < 1e-6);
        assert_eq!(Color::TRANSPARENT.hsla().a, 0.0);
    }

    /// Runs `points` through a [`RunFilter`].
    fn filtered(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
        let mut filter = RunFilter::default();
        let mut out: Vec<(f32, f32)> = points
            .iter()
            .filter_map(|&(x, y)| filter.push(LyonPoint::new(x, y)))
            .map(|p| (p.x, p.y))
            .collect();
        out.extend(filter.finish().map(|p| (p.x, p.y)));
        out
    }

    #[test]
    fn level_and_upright_runs_keep_only_their_ends() {
        // A pedal at zero, pressed, then held: the level runs keep their ends, the ramp every point.
        let line = [
            (0.0, 10.0),
            (2.0, 10.0),
            (4.0, 10.0),
            (6.0, 10.0),
            (8.0, 6.0),
            (10.0, 2.0),
            (12.0, 2.0),
            (14.0, 2.0),
        ];
        assert_eq!(
            filtered(&line),
            [
                (0.0, 10.0),
                (6.0, 10.0),
                (8.0, 6.0),
                (10.0, 2.0),
                (14.0, 2.0)
            ]
        );
        // An upright run, as a closed outline has.
        assert_eq!(
            filtered(&[(0.0, 0.0), (0.0, 1.0), (0.0, 2.0), (3.0, 2.0)]),
            [(0.0, 0.0), (0.0, 2.0), (3.0, 2.0)]
        );
        // Going back over a level run is not a run: the turn stays.
        assert_eq!(
            filtered(&[(0.0, 0.0), (4.0, 0.0), (2.0, 0.0)]),
            [(0.0, 0.0), (4.0, 0.0), (2.0, 0.0)]
        );
        // Short lines stay as they are.
        assert_eq!(filtered(&[(1.0, 1.0)]), [(1.0, 1.0)]);
        assert_eq!(
            filtered(&[(1.0, 1.0), (3.0, 1.0)]),
            [(1.0, 1.0), (3.0, 1.0)]
        );
        assert!(filtered(&[]).is_empty());
    }

    /// The triangles lyon makes for the line through `points` with the canvas's stroke options.
    fn tessellated(points: &[(f32, f32)]) -> Vec<[LyonPoint; 3]> {
        let mut strokes = Strokes::default();
        let options = StrokeOptions::default()
            .with_line_width(2.5)
            .with_line_join(LineJoin::Round)
            .with_line_cap(LineCap::Round);
        let mut output = BuffersBuilder::new(&mut strokes.buffers, |v: StrokeVertex| v.position());
        let mut builder = strokes.tessellator.builder(&options, &mut output);
        builder.begin(LyonPoint::new(points[0].0, points[0].1));
        for p in &points[1..] {
            builder.line_to(LyonPoint::new(p.0, p.1));
        }
        builder.end(false);
        builder.build().expect("tessellated");
        let v = &strokes.buffers.vertices;
        strokes
            .buffers
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| [v[t[0] as usize], v[t[1] as usize], v[t[2] as usize]])
            .collect()
    }

    /// Which samples of a fine grid over the box from `low` to `high` the triangles cover, each sample counted once
    /// however many triangles hold it.
    fn covered(triangles: &[[LyonPoint; 3]], low: (f32, f32), high: (f32, f32)) -> Vec<bool> {
        let inside = |t: &[LyonPoint; 3], x: f32, y: f32| {
            let edge =
                |a: LyonPoint, b: LyonPoint| (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x);
            let (e0, e1, e2) = (edge(t[0], t[1]), edge(t[1], t[2]), edge(t[2], t[0]));
            (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0)
        };
        let step = 0.125;
        let mut samples = Vec::new();
        let mut y = low.1 + step / 2.0;
        while y < high.1 {
            let mut x = low.0 + step / 2.0;
            while x < high.0 {
                samples.push(triangles.iter().any(|t| inside(t, x, y)));
                x += step;
            }
            y += step;
        }
        samples
    }

    #[test]
    fn leaving_out_run_points_covers_the_same_pixels() {
        // A trace with level runs at the top and bottom, a ramp, and a spike.
        let line: Vec<(f32, f32)> = [
            10.0, 10.0, 10.0, 10.0, 7.0, 3.0, 1.25, 1.25, 1.25, 1.25, 1.25, 5.0, 1.25, 1.25, 10.0,
            10.0,
        ]
        .iter()
        .enumerate()
        .map(|(i, &y)| (i as f32 * 2.0, y))
        .collect();
        let kept = filtered(&line);
        assert!(kept.len() < line.len(), "runs shortened: {kept:?}");
        let (low, high) = ((-3.0, -3.0), (34.0, 14.0));
        let all = covered(&tessellated(&line), low, high);
        let fewer = covered(&tessellated(&kept), low, high);
        let differ = all.iter().zip(&fewer).filter(|(a, b)| a != b).count();
        assert_eq!(differ, 0, "of {} samples", all.len());
        assert!(
            tessellated(&kept).len() < tessellated(&line).len(),
            "fewer triangles"
        );
    }
}
