//! The window's look: colors, sizes, and the font, in one place so a redesign changes this file.

use gpui::{Pixels, Rgba, px, rgb};

pub const FONT: &str = "Segoe UI";

/// Window size in the UI framework's pixels (scaled with the display).
pub const WINDOW_WIDTH: f32 = 400.0;
pub const WINDOW_HEIGHT: f32 = 920.0;

pub fn background() -> Rgba {
    rgb(0x0014_181D)
}

pub fn text() -> Rgba {
    rgb(0x00E6_E9ED)
}

pub fn text_dim() -> Rgba {
    rgb(0x008B_949E)
}

pub fn row_hover() -> Rgba {
    rgb(0x0021_272E)
}

pub fn row_pressed() -> Rgba {
    rgb(0x002A_313A)
}

/// The same blue as the overlay's move handles.
pub fn accent() -> Rgba {
    rgb(0x003B_82F6)
}

pub fn switch_off() -> Rgba {
    rgb(0x003A_414A)
}

pub fn knob() -> Rgba {
    rgb(0x00FF_FFFF)
}

pub fn running() -> Rgba {
    rgb(0x003F_B950)
}

pub fn stopped() -> Rgba {
    rgb(0x00E5_534B)
}

pub fn border() -> Rgba {
    rgb(0x0030_363D)
}

pub fn title_size() -> Pixels {
    px(20.0)
}

pub fn text_size() -> Pixels {
    px(14.0)
}

pub fn small_size() -> Pixels {
    px(12.0)
}

/// Space from the window's edges to the content.
pub fn gutter() -> Pixels {
    px(16.0)
}

pub fn row_padding() -> Pixels {
    px(8.0)
}

pub fn radius() -> Pixels {
    px(6.0)
}

pub fn switch_width() -> Pixels {
    px(36.0)
}

pub fn switch_height() -> Pixels {
    px(20.0)
}

/// Thickness of a slider's track.
pub fn slider_height() -> Pixels {
    px(4.0)
}

/// Diameter of the slider's knob, the same as a switch's.
pub fn slider_knob() -> Pixels {
    px(16.0)
}

/// Opacity of a switch that cannot be used.
pub const DISABLED_OPACITY: f32 = 0.4;
