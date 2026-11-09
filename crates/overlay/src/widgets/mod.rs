//! Widgets: each owns its state, decides when it needs redrawing, and draws through a [`Canvas`].

mod status;

use std::time::{Duration, Instant};

pub use status::StatusWidget;

use crate::pipeline::{Display, View};
use crate::render::{Canvas, Rect};

/// What a widget needs redrawn after an update.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Redraw {
    Nothing,
    All,
    /// Only this area, in layer pixels.
    Area(Rect),
}

pub trait Widget {
    /// Layer size in pixels.
    fn size(&self) -> (u32, u32);

    /// How often [`Widget::update`] runs.
    fn interval(&self) -> Duration;

    /// Takes the latest data. Must not allocate after warm-up.
    fn update(&mut self, view: &mut View, now: Instant) -> Redraw;

    /// Draws the area requested by the last update. Must not allocate after warm-up.
    fn draw(&self, canvas: &mut Canvas<'_>);

    /// Whether the widget shows in this display state. Hidden widgets cost no drawing.
    fn visible(&self, display: Display) -> bool {
        display == Display::Driving
    }

    /// Called when the connection changes, so per-session state can reset.
    fn reset(&mut self) {}
}
