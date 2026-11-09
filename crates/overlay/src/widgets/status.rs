//! A small status panel: connection state, tick, cars, and session info. Visible in every state.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::pipeline::{Display, View};
use crate::render::{Align, Canvas, Color, Rect};

use super::{Redraw, Widget};

const WIDTH: u32 = 300;
const HEIGHT: u32 = 54;

/// Shows that the overlay runs and what it receives.
#[derive(Debug)]
pub struct StatusWidget {
    line: String,
    next: String,
    display: Display,
    opacity: f32,
}

impl Default for StatusWidget {
    fn default() -> Self {
        Self::new(0.82)
    }
}

impl StatusWidget {
    /// `opacity` is the background opacity, 0 to 1.
    pub fn new(opacity: f32) -> Self {
        Self {
            line: String::with_capacity(96),
            next: String::with_capacity(96),
            display: Display::Disconnected,
            opacity,
        }
    }
}

impl Widget for StatusWidget {
    fn size(&self) -> (u32, u32) {
        (WIDTH, HEIGHT)
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(250)
    }

    fn update(&mut self, view: &mut View, _now: Instant) -> Redraw {
        self.display = view.display();
        self.next.clear();
        match self.display {
            Display::Disconnected => self.next.push_str("waiting for the sim"),
            state => {
                let drivers = view
                    .session()
                    .map_or(0, |s| s.driver_info.drivers.iter().flatten().count());
                let label = if state == Display::Driving {
                    "driving"
                } else {
                    "idle"
                };
                let _ = write!(
                    self.next,
                    "{label} | tick {} | {} cars | {drivers} drivers",
                    view.snapshot().tick(),
                    view.car_count()
                );
            }
        }
        if self.next == self.line {
            return Redraw::Nothing;
        }
        std::mem::swap(&mut self.line, &mut self.next);
        Redraw::All
    }

    fn draw(&self, canvas: &mut Canvas<'_>) {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        canvas.fill(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(0x10_1418, self.opacity),
        );
        let accent = match self.display {
            Display::Driving => Color::rgb(0x3D_D68C, 1.0),
            Display::Idle => Color::rgb(0xF2_C14E, 1.0),
            Display::Disconnected => Color::rgb(0x8A_9099, 1.0),
        };
        canvas.fill(Rect::new(0.0, 0.0, 4.0, h), accent);
        canvas.text(
            "PITOT",
            Rect::new(14.0, 4.0, 120.0, 22.0),
            14.0,
            Align::Left,
            Color::WHITE,
        );
        canvas.text(
            &self.line,
            Rect::new(14.0, 26.0, w - 20.0, 22.0),
            12.0,
            Align::Left,
            Color::rgb(0xD0_D4DA, 1.0),
        );
    }

    fn visible(&self, _display: Display) -> bool {
        true
    }
}
