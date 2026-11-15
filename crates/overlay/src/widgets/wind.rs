//! Wind: one speed in km/h, and a dial that turns with the car (its nose at the top) with an arrow showing where
//! the air flows relative to the car, so where the wind hits the car reads at a glance.
//!
//! `YawNorth` is the car's compass heading, clockwise from north: it matches the course over ground from an
//! `.ibt`'s latitude and longitude within 0.2 degrees. `WindDir` is taken as the compass direction the wind comes
//! from, clockwise from north, the meteorological convention (iRDashies reads it the same way). The SDK's
//! description does not say which, so this needs a check against a sim session with a set wind direction.

use std::f32::consts::{PI, TAU};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::config::{NoOptions, WidgetConfig};
use crate::pipeline::{View, var_group};
use crate::render::{Align, Canvas, Color, Rect};

use super::text::FixedText;
use super::{Build, MAX_LAYERS, Redraw, Register, Widget};

var_group! {
    /// The variables the wind widget reads. `YawNorth` is in `Vars` too, for the track map; it is copied once.
    pub struct WindVars {
        wind_vel => "WindVel",
        wind_dir => "WindDir",
        yaw_north => "YawNorth",
    }
}

/// Angles are kept and drawn in steps of this many degrees, so a redraw happens only when the drawing moves.
const STEP_DEGREES: f32 = 2.0;
const STEPS: i32 = (360.0 / STEP_DEGREES) as i32;
/// Below this speed (km/h) the air counts as calm: no direction.
const CALM_KMH: f32 = 1.0;

const WHITE: u32 = 0xF0_F2F5;
const DIM: u32 = 0xA0_A6AE;
const BACKGROUND: u32 = 0x10_1418;
/// The car's nose at the top of the dial, in the steering wheel marker's color.
const NOSE: u32 = 0xFF_8A3D;
/// Arrow colors by speed: light, moderate, strong, very strong.
const LEVELS: [(f32, u32); 4] = [
    (0.0, 0xA0_A6AE),
    (10.0, 0xF0_F2F5),
    (25.0, 0xF2_C14E),
    (40.0, 0xFF_6B5E),
];

/// Where the wind comes from, clockwise from the car's nose, in radians from 0 to 2π.
pub(crate) fn relative(wind_dir: f32, heading: f32) -> f32 {
    (wind_dir - heading).rem_euclid(TAU)
}

/// The compass point (of eight) of a direction in radians clockwise from north.
pub(crate) fn compass(direction: f32) -> &'static str {
    const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    let eighth = (direction.rem_euclid(TAU) / (TAU / 8.0)).round() as usize;
    POINTS[eighth % 8]
}

/// An angle in whole steps, from 0 to `STEPS - 1`.
fn step(angle: f32) -> i32 {
    ((angle.rem_euclid(TAU).to_degrees() / STEP_DEGREES).round() as i32).rem_euclid(STEPS)
}

fn step_angle(step: i32) -> f32 {
    (step as f32 * STEP_DEGREES).to_radians()
}

/// A point at `angle` radians clockwise from up, `radius` from `center`, on a y-down screen.
fn at(center: (f32, f32), radius: f32, angle: f32) -> (f32, f32) {
    let (sin, cos) = angle.sin_cos();
    (center.0 + radius * sin, center.1 - radius * cos)
}

/// The arrow for a wind from `relative`: its tail on the upwind side, its head on the downwind side, and the two
/// ends of the arrowhead.
pub(crate) fn arrow(center: (f32, f32), radius: f32, relative: f32) -> [(f32, f32); 4] {
    let length = radius * 0.72;
    let tail = at(center, length, relative);
    let head = at(center, length, relative + PI);
    let wing = radius * 0.3;
    // The wings point back toward the tail, 28 degrees either side.
    let back = relative;
    let spread = 28f32.to_radians();
    [
        tail,
        head,
        at(head, wing, back - spread),
        at(head, wing, back + spread),
    ]
}

/// What the widget shows.
#[derive(Debug, Clone, PartialEq)]
struct Reading {
    /// Wind speed in whole km/h, or dashes.
    speed: FixedText,
    /// Compass point the wind comes from, empty when calm or unknown.
    from: &'static str,
    /// Where the wind comes from relative to the nose, in steps; `None` when calm or without a heading.
    arrow: Option<i32>,
    /// Where north is on the dial, in steps; `None` without a heading.
    north: Option<i32>,
    /// Index into [`LEVELS`].
    level: usize,
}

impl Reading {
    fn new() -> Self {
        Self {
            speed: FixedText::new(8),
            from: "",
            arrow: None,
            north: None,
            level: 0,
        }
    }

    /// Fills from the wind speed (m/s), the direction it comes from, and the car's heading, any of them unknown.
    fn fill(&mut self, speed: Option<f32>, wind_dir: Option<f32>, heading: Option<f32>) {
        self.speed.set("--");
        self.from = "";
        self.arrow = None;
        let heading = heading.filter(|h| h.is_finite());
        self.north = heading.map(|h| step(-h));
        self.level = 0;
        let Some(kmh) = speed
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| s * 3.6)
        else {
            return;
        };
        self.speed.clear();
        let _ = write!(self.speed, "{kmh:.0}");
        self.level = LEVELS.iter().rposition(|&(at, _)| kmh >= at).unwrap_or(0);
        let Some(dir) = wind_dir.filter(|d| d.is_finite()) else {
            return;
        };
        if kmh < CALM_KMH {
            return;
        }
        self.from = compass(dir);
        self.arrow = heading.map(|heading| step(relative(dir, heading)));
    }
}

/// The wind widget.
#[derive(Debug)]
pub struct WindWidget {
    width: u32,
    height: u32,
    opacity: f32,
    vars: WindVars,
    shown: Reading,
    next: Reading,
}

impl WindWidget {
    /// A wind widget reading `vars`, from [`View::resolve`].
    pub fn new(config: &WidgetConfig, vars: WindVars) -> Self {
        Self {
            width: config.width.unwrap_or(200).max(MIN_SIZE.0),
            height: config.height.unwrap_or(170).max(MIN_SIZE.1),
            opacity: config.opacity,
            vars,
            shown: Reading::new(),
            next: Reading::new(),
        }
    }

    /// The dial's center and radius: left of the text column, as large as fits.
    fn dial(&self) -> ((f32, f32), f32) {
        let (w, h) = (self.width as f32, self.height as f32);
        let room = (w - COLUMN - 2.0 * PAD).min(h - 2.0 * PAD);
        let radius = (room / 2.0 - 2.0).max(10.0);
        ((PAD + room / 2.0, h / 2.0), radius)
    }
}

/// Width of the text column on the right.
const COLUMN: f32 = 64.0;
/// Where the speed's number ends in the text column: two digits at the speed's size fit before it.
const SPEED_END: f32 = 30.0;
const PAD: f32 = 8.0;
/// The text column's lines apart, and a dial beside it.
const MIN_SIZE: (u32, u32) = (160, 140);

impl Register for WindWidget {
    type Vars = WindVars;
    type Options = NoOptions;

    fn build(cx: &Build<'_>, vars: WindVars) -> Self {
        Self::new(cx.config, vars)
    }
}

impl Widget for WindWidget {
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
        Duration::from_millis(200)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        let (snap, vars) = (view.snapshot(), self.vars);
        self.next.fill(
            snap.f32(vars.wind_vel, 0),
            snap.f32(vars.wind_dir, 0),
            snap.f32(vars.yaw_north, 0),
        );
        if self.next != self.shown {
            std::mem::swap(&mut self.shown, &mut self.next);
            redraw[0] = Redraw::All;
        }
    }

    fn draw(&self, _layer: usize, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill_background(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(BACKGROUND, self.opacity),
        );
        let dim = Color::rgb(DIM, 1.0);
        let white = Color::rgb(WHITE, 1.0);
        let r = &self.shown;

        // The dial: a ring, the nose at the top, north and the other compass points turning with the car.
        let (center, radius) = self.dial();
        canvas.circle(center, radius, 1.5, Color::rgb(DIM, 0.5));
        let nose = 7f32.to_radians();
        canvas.line(
            at(center, radius, -nose),
            at(center, radius, nose),
            3.0,
            Color::rgb(NOSE, 1.0),
        );
        if let Some(north) = r.north {
            let north = step_angle(north);
            for quarter in 1..4 {
                let angle = north + quarter as f32 * PI / 2.0;
                canvas.line(
                    at(center, radius - 6.0, angle),
                    at(center, radius, angle),
                    1.5,
                    Color::rgb(DIM, 0.6),
                );
            }
            let (x, y) = at(center, radius - 11.0, north);
            canvas.text(
                "N",
                Rect::new(x - 8.0, y - 8.0, 16.0, 16.0),
                11.0,
                Align::Center,
                dim,
            );
        }
        if let Some(from) = r.arrow {
            let [tail, head, left, right] = arrow(center, radius, step_angle(from));
            let ink = Color::rgb(LEVELS[r.level].1, 1.0);
            canvas.line(tail, head, 3.0, ink);
            canvas.line(head, left, 3.0, ink);
            canvas.line(head, right, 3.0, ink);
        }

        // The text column, centered beside the dial: the speed with its unit, and where the wind comes from.
        let x = w - COLUMN - 4.0;
        let row = |top: f32, height: f32| Rect::new(x, top * h / 170.0, COLUMN, height * h / 170.0);
        canvas.text("Wind", row(40.0, 16.0), 11.0, Align::Left, dim);
        // The number ends at a fixed point and the unit follows it, so one to three digits keep the unit in place.
        let value = row(56.0, 30.0);
        canvas.text(
            r.speed.as_str(),
            Rect::new(x - 12.0, value.y, 12.0 + SPEED_END, value.h),
            24.0,
            Align::Right,
            white,
        );
        // Lowered so the smaller unit sits on the number's baseline.
        let unit = SPEED_END + 3.0;
        canvas.text(
            "km/h",
            Rect::new(x + unit, value.y + 6.0, COLUMN - unit, value.h),
            11.0,
            Align::Left,
            dim,
        );
        if !r.from.is_empty() {
            canvas.text("from", row(92.0, 14.0), 11.0, Align::Left, dim);
            canvas.text(r.from, row(106.0, 22.0), 16.0, Align::Left, white);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn relative_direction_turns_with_the_car() {
        // A north wind (from 0) on a car heading north is a headwind; heading east, it comes from the left.
        assert!(close(relative(0.0, 0.0), 0.0));
        assert!(close(relative(0.0, PI / 2.0), 3.0 * PI / 2.0));
        assert!(
            close(relative(PI, PI / 2.0), PI / 2.0),
            "from the south heading east: from the right"
        );
        assert!(close(relative(0.1, 6.2), (0.1 - 6.2f32).rem_euclid(TAU)));
    }

    #[test]
    fn compass_points() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(TAU - 0.1), "N");
        assert_eq!(compass(PI / 4.0), "NE");
        assert_eq!(compass(PI), "S");
        assert_eq!(
            compass(4.688),
            "W",
            "Sebring .ibt at the start: 268.6 degrees"
        );
        assert_eq!(compass(-PI / 2.0), "W");
    }

    #[test]
    fn the_arrow_points_downwind() {
        let center = (100.0, 100.0);
        // Headwind: the air comes at the car from the front, so the arrow points down the dial.
        let [tail, head, ..] = arrow(center, 50.0, 0.0);
        assert!(tail.1 < center.1 && head.1 > center.1);
        assert!(close(tail.0, 100.0) && close(head.0, 100.0));
        // From the right: the arrow points left.
        let [tail, head, ..] = arrow(center, 50.0, PI / 2.0);
        assert!(tail.0 > center.0 && head.0 < center.0);
        // The arrowhead's wings sit behind the head, toward the tail.
        let [_, head, left, right] = arrow(center, 50.0, 0.0);
        assert!(left.1 < head.1 && right.1 < head.1);
        assert!(left.0 != right.0);
    }

    #[test]
    fn reading_shows_one_speed_and_direction() {
        let mut r = Reading::new();
        // 5 m/s from the west (270 degrees), car heading north: from the left, 18 km/h.
        r.fill(Some(5.0), Some(3.0 * PI / 2.0), Some(0.0));
        assert_eq!(r.speed.as_str(), "18");
        assert_eq!(r.from, "W");
        assert_eq!(r.arrow, Some(135), "270 degrees in 2-degree steps");
        assert_eq!(r.north, Some(0));
        assert_eq!(r.level, 1, "18 km/h is moderate");
        // Heading west into it: a headwind, the same speed.
        r.fill(Some(5.0), Some(3.0 * PI / 2.0), Some(3.0 * PI / 2.0));
        assert_eq!(r.speed.as_str(), "18");
        assert_eq!(r.arrow, Some(0));
        assert_eq!(
            r.north,
            Some(45),
            "north is to the right of a car heading west"
        );
        // Heading east: a tailwind, still the same speed.
        r.fill(Some(5.0), Some(3.0 * PI / 2.0), Some(PI / 2.0));
        assert_eq!(r.speed.as_str(), "18");
        assert_eq!(r.arrow, Some(90));
        // Whole km/h, also for a strong wind: 30 m/s is 108 km/h.
        r.fill(Some(30.0), Some(0.0), Some(0.0));
        assert_eq!(r.speed.as_str(), "108");
        assert_eq!(r.level, 3);
    }

    #[test]
    fn reading_without_data_or_wind() {
        let mut r = Reading::new();
        r.fill(None, None, None);
        assert_eq!(r.speed.as_str(), "--");
        assert_eq!((r.arrow, r.north, r.from), (None, None, ""));
        r.fill(Some(0.1), Some(1.0), Some(0.0));
        assert_eq!(r.speed.as_str(), "0");
        assert_eq!((r.arrow, r.from), (None, ""), "calm");
        r.fill(Some(5.0), Some(1.0), None);
        assert_eq!(r.from, "NE", "direction without a heading: 57 degrees");
        assert_eq!(r.arrow, None);
        assert_eq!(r.speed.as_str(), "18");
    }

    #[test]
    fn small_turns_do_not_redraw() {
        let (mut a, mut b) = (Reading::new(), Reading::new());
        a.fill(Some(5.0), Some(1.0), Some(0.300));
        b.fill(Some(5.01), Some(1.0), Some(0.302));
        assert_eq!(a, b, "same step and same rounded speed");
        b.fill(Some(5.0), Some(1.0), Some(0.36));
        assert_ne!(a, b);
    }
}
