//! Wind: one speed in km/h, and a dial that turns with the car (its nose at the top) with an arrow showing where
//! the air flows relative to the car, so where the wind hits the car reads at a glance. The arrow is the only sign
//! of the direction: no compass point is written out.
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

use super::design::{HERO, LABEL, NO_VALUE, PAD, value_color};
use super::palette::{CAUTION, DIM, LOSS, PLAYER, SURFACE, TEXT};
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

/// Arrow colors by speed: light, moderate, strong (to watch), very strong.
const LEVELS: [(f32, u32); 4] = [(0.0, DIM), (10.0, TEXT), (25.0, CAUTION), (40.0, LOSS)];

/// Where the wind comes from, clockwise from the car's nose, in radians from 0 to 2π.
pub(crate) fn relative(wind_dir: f32, heading: f32) -> f32 {
    (wind_dir - heading).rem_euclid(TAU)
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
            arrow: None,
            north: None,
            level: 0,
        }
    }

    /// Fills from the wind speed (m/s), the direction it comes from, and the car's heading, any of them unknown.
    fn fill(&mut self, speed: Option<f32>, wind_dir: Option<f32>, heading: Option<f32>) {
        self.speed.set(NO_VALUE);
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
}

/// Width of the text column on the right.
const COLUMN: f32 = 64.0;
/// Where the speed's number ends in the text column: two digits at the speed's size fit before it.
const SPEED_END: f32 = 30.0;
/// Heights of the text column's two lines: the `Wind` label, and the speed with its unit.
const LABEL_HEIGHT: f32 = 16.0;
const VALUE_HEIGHT: f32 = 30.0;
/// The text column's lines apart, and a small dial beside it: a default-sized widget at the largest text size.
const MIN_SIZE: (u32, u32) = (130, 110);

/// The dial's center and radius in a widget `width` by `height`: left of the text column, as large as fits.
fn dial(width: f32, height: f32) -> ((f32, f32), f32) {
    let room = (width - COLUMN - 2.0 * PAD).min(height - 2.0 * PAD);
    let radius = (room / 2.0 - 2.0).max(10.0);
    ((PAD + room / 2.0, height / 2.0), radius)
}

/// One line of text in the text column.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ColumnText<'a> {
    text: &'a str,
    rect: Rect,
    size: f32,
    align: Align,
    /// Drawn dim (a label or unit) rather than white (a value).
    dim: bool,
}

/// The text column right of the dial: the `Wind` label above the speed and its unit, the pair centered on the
/// dial's middle. The direction is the arrow's alone, so nothing here depends on it.
fn column(reading: &Reading, width: f32, height: f32) -> [ColumnText<'_>; 3] {
    let x = width - COLUMN - PAD;
    // The label's line box starts a little below its rectangle's top and the speed's baseline sits a little above
    // its rectangle's bottom by about as much, so centering the rectangles centers the ink.
    let top = height / 2.0 - (LABEL_HEIGHT + VALUE_HEIGHT) / 2.0;
    let value = top + LABEL_HEIGHT;
    // The number ends at a fixed point and the unit follows it, so one to three digits keep the unit in place.
    let unit = SPEED_END + 3.0;
    [
        ColumnText {
            text: "Wind",
            rect: Rect::new(x, top, COLUMN, LABEL_HEIGHT),
            size: LABEL,
            align: Align::Left,
            dim: true,
        },
        ColumnText {
            text: reading.speed.as_str(),
            rect: Rect::new(x - 12.0, value, 12.0 + SPEED_END, VALUE_HEIGHT),
            size: HERO,
            align: Align::Right,
            dim: false,
        },
        // Lowered so the smaller unit sits on the number's baseline.
        ColumnText {
            text: "km/h",
            rect: Rect::new(x + unit, value + 6.0, COLUMN - unit, VALUE_HEIGHT),
            size: LABEL,
            align: Align::Left,
            dim: true,
        },
    ]
}

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
        Duration::from_millis(100)
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
            Color::rgb(SURFACE, self.opacity),
        );
        let dim = Color::rgb(DIM, 1.0);
        let white = Color::rgb(TEXT, 1.0);
        let r = &self.shown;

        // The dial: a ring, the nose at the top, north and the other compass points turning with the car.
        let (center, radius) = dial(w, h);
        canvas.circle(center, radius, 1.5, Color::rgb(DIM, 0.5));
        // The player's car's nose, in the player's color.
        let nose = 7f32.to_radians();
        canvas.line(
            at(center, radius, -nose),
            at(center, radius, nose),
            3.0,
            Color::rgb(PLAYER, 1.0),
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
                LABEL,
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

        for line in column(r, w, h) {
            let color = if line.dim {
                dim
            } else if line.text == NO_VALUE {
                value_color(line.text)
            } else {
                white
            };
            canvas.text(line.text, line.rect, line.size, line.align, color);
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
    fn reading_shows_one_speed_and_the_arrow() {
        let mut r = Reading::new();
        // 5 m/s from the west (270 degrees), car heading north: from the left, 18 km/h.
        r.fill(Some(5.0), Some(3.0 * PI / 2.0), Some(0.0));
        assert_eq!(r.speed.as_str(), "18");
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
        assert_eq!(r.speed.as_str(), NO_VALUE);
        assert_eq!((r.arrow, r.north), (None, None));
        r.fill(Some(0.1), Some(1.0), Some(0.0));
        assert_eq!(r.speed.as_str(), "0");
        assert_eq!(r.arrow, None, "calm");
        r.fill(Some(5.0), Some(1.0), None);
        assert_eq!(r.arrow, None, "a direction without a heading has no arrow");
        assert_eq!(r.speed.as_str(), "18");
        // Without a heading nothing shown depends on the direction, so a new direction does not redraw.
        let mut other = Reading::new();
        other.fill(Some(5.0), Some(4.0), None);
        assert_eq!(r, other);
    }

    #[test]
    fn no_direction_text_is_drawn() {
        const DIRECTION_TEXT: [&str; 9] = ["from", "N", "NE", "E", "SE", "S", "SW", "W", "NW"];
        let mut r = Reading::new();
        // Every eighth of the compass, with and without a heading, calm, and without data.
        let mut cases = Vec::new();
        for eighth in 0..8 {
            let dir = eighth as f32 * TAU / 8.0;
            cases.push((Some(5.0), Some(dir), Some(0.0)));
            cases.push((Some(5.0), Some(dir), None));
        }
        cases.push((Some(0.1), Some(1.0), Some(0.0)));
        cases.push((None, None, None));
        for (speed, dir, heading) in cases {
            r.fill(speed, dir, heading);
            let texts = column(&r, 200.0, 170.0).map(|line| line.text);
            assert_eq!(texts, ["Wind", r.speed.as_str(), "km/h"], "{dir:?}");
            assert!(
                texts.iter().all(|text| !DIRECTION_TEXT.contains(text)),
                "{texts:?}"
            );
        }
    }

    #[test]
    fn the_text_column_is_centered_beside_the_dial() {
        let mut r = Reading::new();
        r.fill(Some(30.0), Some(0.0), Some(0.0));
        assert_eq!(r.speed.as_str(), "108", "three digits, the widest speed");
        // In widget pixels: the default size, the smallest, and the default box at text sizes 150 and 70.
        for (w, h) in [(200.0, 170.0), (130.0, 110.0), (134.0, 114.0), (286.0, 243.0)] {
            let [label, speed, unit] = column(&r, w, h).map(|line| line.rect);
            let (center, radius) = dial(w, h);
            let middle = (label.y + speed.y + speed.h) / 2.0;
            assert!((middle - center.1).abs() < 0.01, "{w}x{h}: {middle}");
            assert!((center.1 - h / 2.0).abs() < 0.01, "{w}x{h}");
            // The label and the unit start right of the dial; the right-aligned speed ends right of it.
            assert!(label.x > center.0 + radius, "{w}x{h}");
            assert!(unit.x > center.0 + radius, "{w}x{h}");
            assert!(speed.x + speed.w > center.0 + radius, "{w}x{h}");
            assert_eq!(label.y + label.h, speed.y, "{w}x{h}: the speed under the label");
            for rect in [label, speed, unit] {
                assert!(rect.x >= 0.0 && rect.y >= 0.0, "{w}x{h}: {rect:?}");
                assert!(rect.x + rect.w <= w && rect.y + rect.h <= h, "{w}x{h}: {rect:?}");
            }
        }
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
