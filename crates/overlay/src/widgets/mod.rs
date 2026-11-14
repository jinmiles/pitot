//! Widgets: each owns its state, decides when and what to redraw, and draws through a [`Canvas`].
//!
//! A widget has one or more layers (its own composition surfaces), for example a static background and a
//! scrolling trace. Each update reports what to redraw per layer.

mod fuel;
mod inputs;
mod pit;
mod pitstops;
mod relative;
mod sectors;
mod standings;
mod status;
mod text;
mod timing;
mod tires;
mod trackclock;
mod trackmap;
mod wind;

use std::time::{Duration, Instant};

pub use fuel::FuelWidget;
pub use inputs::InputTraceWidget;
pub use pit::PitWidget;
pub(crate) use pitstops::{PitTracker, SharedPitTracker};
pub use relative::RelativeWidget;
pub use sectors::SectorsWidget;
pub use standings::StandingsWidget;
pub use status::StatusWidget;
pub use tires::TiresWidget;
pub(crate) use trackclock::{SharedTrackClock, TrackClock};
pub use trackmap::TrackMapWidget;
pub use wind::WindWidget;

use crate::pipeline::{Display, View};
use crate::render::{Canvas, Rect};

/// Most layers a widget may have.
pub const MAX_LAYERS: usize = 2;

/// What a layer needs redrawn after an update.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Redraw {
    Nothing,
    All,
    /// Only this area, in layer pixels.
    Area(Rect),
    /// Two areas, for example a range of columns that wraps around a scrolling layer.
    Areas(Rect, Rect),
}

/// A widget layer, positioned in widget pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerSpec {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// The surface is a ring of columns shown from [`Widget::scroll`].
    pub scrolling: bool,
}

pub trait Widget {
    /// Size in pixels.
    fn size(&self) -> (u32, u32);

    /// The widget's layers, bottom to top. Called when the widget is placed.
    fn layers(&self) -> Vec<LayerSpec> {
        let (width, height) = self.size();
        vec![LayerSpec {
            x: 0,
            y: 0,
            width,
            height,
            scrolling: false,
        }]
    }

    /// How often [`Widget::update`] runs unless the configuration sets a rate.
    fn interval(&self) -> Duration;

    /// Takes the latest data and fills in what each layer needs redrawn. Must not allocate after warm-up.
    fn update(&mut self, view: &mut View, now: Instant, redraw: &mut [Redraw; MAX_LAYERS]);

    /// Draws `layer`. The canvas is clipped to the area being redrawn. Must not allocate after warm-up.
    fn draw(&self, layer: usize, canvas: &mut Canvas<'_>);

    /// First column to show for a scrolling layer.
    fn scroll(&self, _layer: usize) -> u32 {
        0
    }

    /// Whether the widget shows in this display state. Hidden widgets cost no drawing.
    fn visible(&self, display: Display) -> bool {
        display == Display::Driving
    }

    /// Called when the connection changes, so per-session state can reset.
    fn reset(&mut self) {}
}
