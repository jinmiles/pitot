//! The widgets' layout rules, described in `docs/design-language.md`: text roles, spacing on a 4 px grid, how a
//! widget is put together, and how a value that is not known shows. All in widget pixels, before the text size.

use crate::render::{Canvas, Color, Rect};

use super::palette::{DIM, FAINT, TEXT};

/// Labels, units, column names, and context beside the main values.
pub const LABEL: f32 = 12.0;
/// Rows of a table and values of a dense widget.
pub const BODY: f32 = 14.0;
/// The values a widget is read for at a glance, and the rows of the relative.
pub const VALUE: f32 = 18.0;
/// The one value a small widget is about.
pub const HERO: f32 = 26.0;

/// Space between a widget's edge and its content, and around a block inside it.
pub const PAD: f32 = 8.0;
/// Space between columns, between blocks, and between widgets in the default layout.
pub const GUTTER: f32 = 8.0;

/// The alpha of a divider: a 1 px line of [`TEXT`] between a header or footer and the body.
pub const DIVIDER_ALPHA: f32 = 0.12;

/// Shown for a value that is not known yet where a value is expected beside its label. In a table, an unknown
/// value is left blank instead.
pub const NO_VALUE: &str = "\u{2014}";

/// A divider across the widget at `y`, inset by [`PAD`] on both sides.
pub fn divider(canvas: &mut Canvas<'_>, y: f32, width: f32) {
    canvas.fill(
        Rect::new(PAD, y, (width - 2.0 * PAD).max(0.0), 1.0),
        Color::rgb(TEXT, DIVIDER_ALPHA),
    );
}

/// The color of a value beside its label: [`TEXT`], or [`FAINT`] for [`NO_VALUE`].
pub fn value_color(text: &str) -> Color {
    Color::rgb(if text == NO_VALUE { FAINT } else { TEXT }, 1.0)
}

/// A label's color.
pub const fn label_color() -> Color {
    Color::rgb(DIM, 1.0)
}

/// One line saying why a widget or a block of it is empty, in [`FAINT`], left in `rect`.
pub fn empty_line(canvas: &mut Canvas<'_>, text: &str, rect: Rect) {
    canvas.text(
        text,
        rect,
        BODY,
        crate::render::Align::Left,
        Color::rgb(FAINT, 1.0),
    );
}
