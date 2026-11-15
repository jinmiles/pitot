//! A small status panel: connection state, tick, cars, and session info. Visible in every state.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::config::NoOptions;
use crate::pipeline::{Display, View};
use crate::render::{Align, Canvas, Color, Rect};

use super::{Build, MAX_LAYERS, Redraw, Register, Widget};

const WIDTH: u32 = 300;
const HEIGHT: u32 = 54;
/// The two lines, and room for the first words of the second.
const MIN_SIZE: (u32, u32) = (160, HEIGHT);

/// Shows that the overlay runs and what it receives.
#[derive(Debug)]
pub struct StatusWidget {
    width: u32,
    height: u32,
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
            width: WIDTH,
            height: HEIGHT,
            line: String::with_capacity(96),
            next: String::with_capacity(96),
            display: Display::Disconnected,
            opacity,
        }
    }
}

impl Register for StatusWidget {
    type Vars = ();
    type Options = NoOptions;

    fn build(cx: &Build<'_>, _: ()) -> Self {
        let mut widget = Self::new(cx.config.opacity);
        let config = cx.config;
        widget.resize(
            config.width.unwrap_or(WIDTH),
            config.height.unwrap_or(HEIGHT),
        );
        widget
    }
}

impl Widget for StatusWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn min_size(&self) -> (u32, u32) {
        MIN_SIZE
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(MIN_SIZE.0);
        self.height = height.max(MIN_SIZE.1);
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(250)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        self.display = view.display();
        self.next.clear();
        match self.display {
            Display::Disconnected => self.next.push_str("waiting for the sim"),
            state => {
                let drivers = view
                    .session()
                    .map_or(0, |s| s.driver_info.drivers.iter().flatten().count());
                let label = match state {
                    Display::Driving => "driving",
                    Display::Replay => "replay",
                    _ => "idle",
                };
                let _ = write!(
                    self.next,
                    "{label} | tick {} | {} cars | {drivers} drivers",
                    view.snapshot().tick(),
                    view.car_count()
                );
            }
        }
        if self.next != self.line {
            std::mem::swap(&mut self.line, &mut self.next);
            redraw[0] = Redraw::All;
        }
    }

    fn draw(&self, _layer: usize, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(0x10_1418, self.opacity),
        );
        let accent = match self.display {
            Display::Driving => Color::rgb(0x3D_D68C, 1.0),
            Display::Idle | Display::Replay => Color::rgb(0xF2_C14E, 1.0),
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
