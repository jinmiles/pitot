//! Input trace: throttle, brake (with ABS), clutch, and steering over the last few seconds, live pedal bars,
//! and a steering wheel icon that turns with the wheel.
//!
//! The trace is the last samples of a ring, two pixels each with the newest at the right, drawn as one line per
//! input whenever new samples arrive. Every sample from the pipeline is drawn, including those that queued while a
//! frame was late: an update takes every sample queued since the last one, and the ring holds the whole trace.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use crate::config::{NoOptions, WidgetConfig};
use crate::pipeline::{InputSample, View};
use crate::render::{Canvas, Color, Rect};

use super::{Build, LayerSpec, MAX_LAYERS, Redraw, Register, Widget, palette};

/// The line of one input through the samples `range` of the trace: from the sample before the first (or the first,
/// at the ring's start) to the last, `level` giving each sample's height from 0 to 1.
struct Trace<'a> {
    widget: &'a InputTraceWidget,
    level: fn(&InputSample) -> f32,
}

impl Trace<'_> {
    fn points(&self, from: u64, to: u64) -> impl Iterator<Item = (f32, f32)> + '_ {
        let w = self.widget;
        let previous = if from > w.first {
            w.sample(from - 1)
        } else {
            w.sample(from)
        };
        let start = (
            w.sample_x(from) - PX_PER_SAMPLE as f32,
            w.y((self.level)(previous)),
        );
        std::iter::once(start)
            .chain((from..to).map(move |index| (w.sample_x(index), w.y((self.level)(w.sample(index))))))
    }
}

const PX_PER_SAMPLE: u32 = 2;
/// Live pedal bars: width and the gap before each bar.
const BAR_W: f32 = 12.0;
const BAR_GAP: f32 = 4.0;
/// The three pedal bars with their gaps.
const PEDALS_WIDTH: u32 = 52;
/// Largest steering wheel icon, in pixels.
const WHEEL_MAX: u32 = 72;
const PAD: u32 = 6;
/// A second of trace beside the pedal bars and a small wheel.
const MIN_SIZE: (u32, u32) = (240, 60);
/// Trace line width.
const LINE: f32 = 2.5;

const THROTTLE: Color = Color::rgb(palette::THROTTLE, 1.0);
const BRAKE: Color = Color::rgb(palette::BRAKE, 1.0);
const ABS: Color = Color::rgb(palette::CAUTION, 1.0);
const CLUTCH: Color = Color::rgb(palette::CLUTCH, 1.0);
const STEERING: Color = Color::rgb(palette::DIM, 0.9);
const WHEEL: Color = Color::rgb(palette::TEXT, 1.0);
/// The wheel's top: the player's steering, in the player's color.
const WHEEL_MARK: Color = Color::rgb(palette::PLAYER, 1.0);
const GRID: Color = Color::rgb(0xFF_FFFF, 0.08);
const TRACK: Color = Color::rgb(0xFF_FFFF, 0.10);

/// Displayed clutch: the sim reports 1 for engaged (pedal up).
fn clutch_pedal(sample: &InputSample) -> f32 {
    1.0 - sample.clutch
}

/// Steering as 0..1 with 0.5 straight ahead, over plus or minus half a turn.
fn steering_level(sample: &InputSample) -> f32 {
    0.5 - (sample.steering / PI).clamp(-1.0, 1.0) * 0.5
}

/// Steering wheel icon ported from Tabler Icons `steering-wheel` (MIT): a 24-unit box centered on the origin with
/// the rim, the hub, and three spokes. The stroke is thinner than the icon's 2 units, which reads better large.
const ICON_BOX: f32 = 24.0;
const ICON_RIM: f32 = 9.0;
const ICON_HUB: f32 = 2.0;
const ICON_STROKE: f32 = 1.5;
const ICON_SPOKES: [((f32, f32), (f32, f32)); 3] = [
    ((0.0, 2.0), (0.0, 9.0)),
    ((-2.0, 0.0), (-8.75, -2.0)),
    ((2.0, 0.0), (8.75, -2.0)),
];
/// Center marker at the top of the rim (not part of the icon), so the wheel's turn reads at a glance like on a
/// racing wheel: a thick chord across 12 degrees either side of the top.
const ICON_MARK: ((f32, f32), (f32, f32)) = ((-1.87, -8.8), (1.87, -8.8));
const ICON_MARK_STROKE: f32 = 3.0;

/// Turns a point by the wheel angle (radians, positive to the left) on a y-down screen.
fn turn((x, y): (f32, f32), angle: f32) -> (f32, f32) {
    let (sin, cos) = angle.sin_cos();
    (x * cos + y * sin, y * cos - x * sin)
}

/// Steering wheel icon size for a widget height.
fn wheel_size(height: u32) -> u32 {
    (height - 2 * PAD).min(WHEEL_MAX)
}

/// Width of the live area right of the trace: the pedal bars, then the wheel.
fn side_width(height: u32) -> u32 {
    PEDALS_WIDTH + wheel_size(height)
}

/// Live pedal bars from left to right, in the order of the pedals: clutch, brake, throttle.
fn pedal_bars(sample: &InputSample) -> [(f32, Color); 3] {
    [
        (clutch_pedal(sample), CLUTCH),
        (sample.brake, if sample.abs_active { ABS } else { BRAKE }),
        (sample.throttle, THROTTLE),
    ]
}

/// Live pedal levels and wheel angle rounded to what the bars and the wheel can show, to skip redraws that change
/// nothing.
fn bar_levels(sample: &InputSample, height: f32) -> [i32; 5] {
    let px = |v: f32| (v.clamp(0.0, 1.0) * height) as i32;
    [
        px(clutch_pedal(sample)),
        px(sample.brake),
        px(sample.throttle),
        // Whole degrees: a degree moves the rim of the largest wheel by about half a pixel.
        sample.steering.to_degrees().round() as i32,
        i32::from(sample.abs_active),
    ]
}

/// The input trace widget.
#[derive(Debug)]
pub struct InputTraceWidget {
    width: u32,
    height: u32,
    opacity: f32,
    trace: Rect,
    /// Trace width in columns, a multiple of `PX_PER_SAMPLE`.
    columns: u32,
    /// Ring of the most recent samples, enough to redraw the whole visible trace.
    history: Box<[InputSample]>,
    /// Samples appended since the last reset.
    total: u64,
    /// The oldest sample the ring holds: 0, or after a resize the oldest it kept.
    first: u64,
    latest: InputSample,
    bars: [i32; 5],
    missed: u64,
    missed_shown: u64,
}

impl InputTraceWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        let mut widget = Self {
            width: 0,
            height: 0,
            opacity: config.opacity,
            trace: Rect::default(),
            columns: PX_PER_SAMPLE,
            history: Box::default(),
            total: 0,
            first: 0,
            latest: InputSample::default(),
            bars: [-1; 5],
            missed: 0,
            missed_shown: 0,
        };
        widget.resize(config.width.unwrap_or(600), config.height.unwrap_or(120));
        widget
    }

    fn sample(&self, index: u64) -> &InputSample {
        &self.history[(index % self.history.len() as u64) as usize]
    }

    /// Where sample `index` ends in the trace: the newest at its right edge, each older one two pixels further left.
    fn sample_x(&self, index: u64) -> f32 {
        let back = self.total.saturating_sub(1).saturating_sub(index);
        self.columns as f32 - back as f32 * PX_PER_SAMPLE as f32
    }

    /// Oldest sample still on screen.
    fn first_visible(&self) -> u64 {
        self.total
            .saturating_sub(u64::from(self.columns / PX_PER_SAMPLE))
            .max(self.first)
    }

    /// The live area right of the trace: pedal bars and the steering wheel.
    fn bars_rect(&self) -> Rect {
        let side = side_width(self.height);
        let x = (self.width - side - PAD) as f32;
        Rect::new(x, PAD as f32, side as f32, (self.height - 2 * PAD) as f32)
    }

    /// Box of the steering wheel icon, right of the pedal bars and vertically centered.
    fn wheel_rect(&self) -> Rect {
        let bars = self.bars_rect();
        let size = wheel_size(self.height) as f32;
        Rect::new(
            bars.x + PEDALS_WIDTH as f32,
            bars.y + (bars.h - size) / 2.0,
            size,
            size,
        )
    }

    fn y(&self, level: f32) -> f32 {
        // Half a line from each edge, so a line at zero or full stays whole.
        let inner = self.trace.h - LINE;
        LINE / 2.0 + (1.0 - level.clamp(0.0, 1.0)) * inner
    }

    fn draw_background(&self, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill_background(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(palette::SURFACE, self.opacity),
        );
        let t = self.trace;
        for level in [0.25, 0.5, 0.75] {
            let y = t.y + self.y(level);
            canvas.line((t.x, y), (t.x + t.w, y), 1.0, GRID);
        }
        let bars = self.bars_rect();
        let s = &self.latest;
        for (n, (level, color)) in pedal_bars(s).into_iter().enumerate() {
            let x = bars.x + BAR_GAP + n as f32 * (BAR_W + BAR_GAP);
            canvas.fill(Rect::new(x, bars.y, BAR_W, bars.h), TRACK);
            let fill = level.clamp(0.0, 1.0) * bars.h;
            canvas.fill(Rect::new(x, bars.y + bars.h - fill, BAR_W, fill), color);
        }
        self.draw_wheel(canvas, s.steering);
        if self.missed > 0 {
            // Visible proof that samples were lost; normally never shown.
            canvas.fill(Rect::new(bars.x, bars.y, bars.w, 3.0), BRAKE);
        }
    }

    fn draw_wheel(&self, canvas: &mut Canvas<'_>, angle: f32) {
        let r = self.wheel_rect();
        let scale = r.w / ICON_BOX;
        let center = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let stroke = ICON_STROKE * scale;
        canvas.circle(center, ICON_RIM * scale, stroke, WHEEL);
        canvas.circle(center, ICON_HUB * scale, stroke, WHEEL);
        let at = |point: (f32, f32)| {
            let (x, y) = turn(point, angle);
            (center.0 + x * scale, center.1 + y * scale)
        };
        let spokes = ICON_SPOKES.map(|(from, to)| (at(from), at(to)));
        canvas.lines(&spokes, stroke, WHEEL);
        let (from, to) = ICON_MARK;
        canvas.line(at(from), at(to), ICON_MARK_STROKE * scale, WHEEL_MARK);
    }

    /// The visible samples as one line per input, oldest at the left. The brake's line changes color where ABS
    /// starts or ends.
    fn draw_trace(&self, canvas: &mut Canvas<'_>) {
        let (from, to) = (self.first_visible(), self.total);
        if to <= from {
            return;
        }
        let line = |canvas: &mut Canvas<'_>, level: fn(&InputSample) -> f32, a: u64, b: u64, color| {
            let trace = Trace { widget: self, level };
            canvas.stroke(trace.points(a, b), false, LINE, color);
        };
        line(canvas, steering_level, from, to, STEERING);
        line(canvas, clutch_pedal, from, to, CLUTCH);
        line(canvas, |s| s.throttle, from, to, THROTTLE);
        let brake_color = |index: u64| {
            if self.sample(index).abs_active {
                ABS
            } else {
                BRAKE
            }
        };
        let mut run = from;
        for index in from + 1..=to {
            if index == to || brake_color(index) != brake_color(run) {
                line(canvas, |s| s.brake, run, index, brake_color(run));
                run = index;
            }
        }
    }
}

impl Register for InputTraceWidget {
    type Vars = ();
    type Options = NoOptions;

    fn build(cx: &Build<'_>, _: ()) -> Self {
        Self::new(cx.config)
    }
}

impl Widget for InputTraceWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// No text: the trace, the bars, and the wheel fill the box at any text size, and the trace keeps two screen
    /// pixels per sample.
    fn scales_with_text(&self) -> bool {
        false
    }

    fn min_size(&self) -> (u32, u32) {
        MIN_SIZE
    }

    fn resize(&mut self, width: u32, height: u32) {
        let height = height.max(MIN_SIZE.1);
        let side = side_width(height);
        let width = width
            .max(MIN_SIZE.0)
            .max(side + 4 * PAD + 2 * PX_PER_SAMPLE);
        let columns = (width - side - 3 * PAD) / PX_PER_SAMPLE * PX_PER_SAMPLE;
        let visible = (columns / PX_PER_SAMPLE) as usize + 1;
        // The newest samples move to the new ring, as many as both hold; a wider trace fills in from the right.
        let kept = (self.total - self.first)
            .min(self.history.len() as u64)
            .min(visible as u64);
        let mut history = vec![InputSample::default(); visible].into_boxed_slice();
        for index in self.total - kept..self.total {
            history[(index % visible as u64) as usize] = *self.sample(index);
        }
        self.history = history;
        self.first = self.total - kept;
        self.width = width;
        self.height = height;
        self.columns = columns;
        self.trace = Rect::new(
            PAD as f32,
            PAD as f32,
            columns as f32,
            (height - 2 * PAD) as f32,
        );
        // The bars have a new height: draw them at the next update.
        self.bars = [-1; 5];
    }

    fn layers(&self) -> Vec<LayerSpec> {
        vec![
            LayerSpec {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            },
            LayerSpec {
                x: self.trace.x as i32,
                y: self.trace.y as i32,
                width: self.columns,
                height: self.trace.h as u32,
            },
        ]
    }

    fn interval(&self) -> Duration {
        // Every sample is drawn; a frame draws all samples queued since the last one.
        Duration::from_secs_f64(1.0 / 60.0)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        let first = self.total;
        let len = self.history.len() as u64;
        let (history, total, latest, missed) = (
            &mut self.history,
            &mut self.total,
            &mut self.latest,
            &mut self.missed,
        );
        view.drain_samples(|sample| {
            history[(*total % len) as usize] = sample;
            *total += 1;
            *latest = sample;
            *missed += u64::from(sample.missed_before);
        });
        if self.total > first {
            redraw[1] = Redraw::All;
        }
        let bars = bar_levels(&self.latest, self.bars_rect().h);
        if bars != self.bars || self.missed != self.missed_shown {
            self.bars = bars;
            self.missed_shown = self.missed;
            redraw[0] = Redraw::Area(self.bars_rect());
        }
    }

    fn draw(&self, layer: usize, canvas: &mut Canvas<'_>) {
        match layer {
            0 => self.draw_background(canvas),
            _ => self.draw_trace(canvas),
        }
    }

    fn reset(&mut self) {
        self.total = 0;
        self.first = 0;
        self.latest = InputSample::default();
        self.bars = [-1; 5];
        self.missed = 0;
        self.missed_shown = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget() -> InputTraceWidget {
        let config = WidgetConfig {
            enabled: true,
            monitor: None,
            x: 0,
            y: 0,
            width: Some(600),
            height: Some(120),
            rate_hz: None,
            opacity: 0.8,
            options: toml::Table::new(),
        };
        InputTraceWidget::new(&config)
    }

    #[test]
    fn trace_geometry_fits_the_widget() {
        let w = widget();
        assert_eq!(w.columns % PX_PER_SAMPLE, 0);
        assert!(w.trace.x + w.trace.w <= w.bars_rect().x);
        assert_eq!(w.history.len() as u32, w.columns / PX_PER_SAMPLE + 1);
        let layers = w.layers();
        assert_eq!(layers[1].width, w.columns);
    }

    #[test]
    fn the_newest_sample_ends_at_the_right_edge() {
        let mut w = widget();
        let samples = u64::from(w.columns / PX_PER_SAMPLE);
        w.total = samples * 3 + 5;
        let newest = w.total - 1;
        assert_eq!(w.sample_x(newest), w.columns as f32);
        assert_eq!(w.sample_x(newest - 1), (w.columns - PX_PER_SAMPLE) as f32);
        // The oldest visible sample's line starts at the left edge.
        let oldest = w.first_visible();
        assert_eq!(w.sample_x(oldest) - PX_PER_SAMPLE as f32, 0.0);
        // Its line starts from the sample before it, which the ring still holds.
        let trace = Trace {
            widget: &w,
            level: |s| s.throttle,
        };
        let points: Vec<(f32, f32)> = trace.points(oldest, w.total).collect();
        assert_eq!(points.len() as u64, w.total - oldest + 1);
        assert_eq!(points[0].0, 0.0);
        assert_eq!(points.last().map(|p| p.0), Some(w.columns as f32));
    }

    #[test]
    fn bars_follow_the_pedals() {
        let sample = InputSample {
            clutch: 0.0,
            brake: 0.5,
            throttle: 0.25,
            ..InputSample::default()
        };
        let bars = pedal_bars(&sample);
        assert_eq!(bars.map(|(level, _)| level), [1.0, 0.5, 0.25]);
        let [clutch, brake, throttle] = bars.map(|(_, c)| c);
        assert!(clutch.b > clutch.r && clutch.b > clutch.g, "clutch is blue");
        assert!(brake.r > brake.g && brake.r > brake.b, "brake is red");
        assert!(
            throttle.g > throttle.r && throttle.g > throttle.b,
            "throttle is green"
        );
        let abs = pedal_bars(&InputSample {
            abs_active: true,
            ..sample
        })[1]
            .1;
        assert!(
            abs.r > 0.85 && abs.g > 0.7 && abs.b < abs.g - 0.3,
            "brake turns yellow under ABS"
        );
    }

    #[test]
    fn wheel_turns_with_the_steering() {
        // A left turn (positive angle) moves the top of the wheel to the left on screen.
        let (x, y) = turn((0.0, -1.0), PI / 2.0);
        assert!((x + 1.0).abs() < 1e-6 && y.abs() < 1e-6, "({x}, {y})");
        let (x, y) = turn((0.0, -1.0), -PI / 2.0);
        assert!((x - 1.0).abs() < 1e-6 && y.abs() < 1e-6, "({x}, {y})");
        let w = widget();
        let (wheel, bars) = (w.wheel_rect(), w.bars_rect());
        assert_eq!((wheel.w, wheel.h), (WHEEL_MAX as f32, WHEEL_MAX as f32));
        assert!(wheel.x + wheel.w <= bars.x + bars.w);
        assert!(wheel.y >= bars.y && wheel.y + wheel.h <= bars.y + bars.h);
        // Redrawn for a whole degree of change, not less.
        let level = |degrees: f32| {
            let sample = InputSample {
                steering: degrees.to_radians(),
                ..InputSample::default()
            };
            bar_levels(&sample, 100.0)[3]
        };
        assert_eq!(level(10.2), level(9.9));
        assert_ne!(level(10.6), level(10.2));
        assert_eq!(level(-450.0), -450, "angles beyond half a turn");
    }

    #[test]
    fn marker_shows_the_top_of_the_wheel() {
        let middle = |angle: f32| {
            let (a, b) = (turn(ICON_MARK.0, angle), turn(ICON_MARK.1, angle));
            ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
        };
        let (x, y) = middle(0.0);
        assert!(
            x.abs() < 1e-6 && y < -8.0,
            "straight: at the top ({x}, {y})"
        );
        let (x, y) = middle(PI / 4.0);
        assert!(x < -5.0 && y < -5.0, "left: up and to the left ({x}, {y})");
        let (x, y) = middle(-PI / 4.0);
        assert!(x > 5.0 && y < -5.0, "right: up and to the right ({x}, {y})");
        let (x, y) = middle(PI);
        assert!(
            x.abs() < 1e-3 && y > 8.0,
            "half a turn: at the bottom ({x}, {y})"
        );
    }

    #[test]
    fn pedal_mapping() {
        let sample = InputSample {
            clutch: 1.0,
            steering: PI / 2.0,
            ..InputSample::default()
        };
        assert_eq!(
            clutch_pedal(&sample),
            0.0,
            "engaged clutch shows as released pedal"
        );
        assert_eq!(steering_level(&sample), 0.25);
        let w = widget();
        // Full and zero pedal keep the whole line inside the trace.
        assert_eq!(w.y(1.0), LINE / 2.0);
        assert_eq!(w.y(0.0), w.trace.h - LINE / 2.0);
    }
}
