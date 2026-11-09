//! Input trace: throttle, brake (with ABS), clutch, and steering over the last few seconds, and live bars.
//!
//! The trace is a scrolling layer: each sample owns two columns of a ring surface, only new samples' columns
//! are drawn, and scrolling moves the visuals instead of redrawing. Every sample from the pipeline is drawn,
//! including those that queued while a frame was late.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use crate::config::WidgetConfig;
use crate::pipeline::{InputSample, View};
use crate::render::{Canvas, Color, Rect};

use super::{LayerSpec, MAX_LAYERS, Redraw, Widget};

const PX_PER_SAMPLE: u32 = 2;
const BARS_WIDTH: u32 = 64;
const PAD: u32 = 6;
const LINE: f32 = 2.0;

const THROTTLE: Color = Color::rgb(0x3D_D68C, 1.0);
const BRAKE: Color = Color::rgb(0xF0_524F, 1.0);
const ABS: Color = Color::rgb(0xF2_A33A, 1.0);
const CLUTCH: Color = Color::rgb(0x4E_A6F2, 1.0);
const STEERING: Color = Color::rgb(0xC8_CCD2, 0.8);
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

/// Live pedal bars from left to right, in the order of the pedals: clutch, brake, throttle.
fn pedal_bars(sample: &InputSample) -> [(f32, Color); 3] {
    [
        (clutch_pedal(sample), CLUTCH),
        (sample.brake, if sample.abs_active { ABS } else { BRAKE }),
        (sample.throttle, THROTTLE),
    ]
}

/// Live pedal levels rounded to what the bars can show, to skip redraws that change nothing.
fn bar_levels(sample: &InputSample, height: f32) -> [i32; 5] {
    let px = |v: f32| (v.clamp(0.0, 1.0) * height) as i32;
    [
        px(clutch_pedal(sample)),
        px(sample.brake),
        px(sample.throttle),
        px(steering_level(sample)),
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
    latest: InputSample,
    bars: [i32; 5],
    missed: u64,
    missed_shown: u64,
}

impl InputTraceWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        let width = config
            .width
            .unwrap_or(600)
            .max(BARS_WIDTH + 4 * PAD + 2 * PX_PER_SAMPLE);
        let height = config.height.unwrap_or(120).max(4 * PAD);
        let columns = (width - BARS_WIDTH - 3 * PAD) / PX_PER_SAMPLE * PX_PER_SAMPLE;
        let trace = Rect::new(
            PAD as f32,
            PAD as f32,
            columns as f32,
            (height - 2 * PAD) as f32,
        );
        let visible = (columns / PX_PER_SAMPLE) as usize + 1;
        Self {
            width,
            height,
            opacity: config.opacity,
            trace,
            columns,
            history: vec![InputSample::default(); visible].into_boxed_slice(),
            total: 0,
            latest: InputSample::default(),
            bars: [-1; 5],
            missed: 0,
            missed_shown: 0,
        }
    }

    fn column(&self, index: u64) -> u32 {
        ((index * u64::from(PX_PER_SAMPLE)) % u64::from(self.columns)) as u32
    }

    fn sample(&self, index: u64) -> &InputSample {
        &self.history[(index % self.history.len() as u64) as usize]
    }

    /// Oldest sample still on screen.
    fn first_visible(&self) -> u64 {
        self.total
            .saturating_sub(u64::from(self.columns / PX_PER_SAMPLE))
    }

    fn bars_rect(&self) -> Rect {
        let x = (self.width - BARS_WIDTH - PAD) as f32;
        Rect::new(
            x,
            PAD as f32,
            BARS_WIDTH as f32,
            (self.height - 2 * PAD) as f32,
        )
    }

    fn y(&self, level: f32) -> f32 {
        let inner = self.trace.h - 2.0;
        1.0 + (1.0 - level.clamp(0.0, 1.0)) * inner
    }

    /// Columns `[from, to)` of the trace, which may wrap around the ring.
    fn columns_area(&self, from: u32, to: u32) -> Redraw {
        let h = self.trace.h;
        let rect = |x0: u32, x1: u32| Rect::new(x0 as f32, 0.0, (x1 - x0) as f32, h);
        if from < to {
            Redraw::Area(rect(from, to))
        } else {
            Redraw::Areas(rect(from, self.columns), rect(0, to))
        }
    }

    fn draw_background(&self, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(0x10_1418, self.opacity),
        );
        let t = self.trace;
        for level in [0.25, 0.5, 0.75] {
            let y = t.y + self.y(level);
            canvas.line((t.x, y), (t.x + t.w, y), 1.0, GRID);
        }
        let bars = self.bars_rect();
        let bar_w = 12.0;
        let gap = (bars.w - 4.0 * bar_w) / 5.0;
        let s = &self.latest;
        for (n, (level, color)) in pedal_bars(s).into_iter().enumerate() {
            let x = bars.x + gap + n as f32 * (bar_w + gap);
            canvas.fill(Rect::new(x, bars.y, bar_w, bars.h), TRACK);
            let fill = level.clamp(0.0, 1.0) * bars.h;
            canvas.fill(Rect::new(x, bars.y + bars.h - fill, bar_w, fill), color);
        }
        // Steering: a marker that moves left and right in the fourth column.
        let x = bars.x + gap + 3.0 * (bar_w + gap);
        canvas.fill(Rect::new(x, bars.y, bar_w, bars.h), TRACK);
        let y = bars.y + steering_level(s) * (bars.h - 4.0);
        canvas.fill(Rect::new(x, y, bar_w, 4.0), STEERING);
        if self.missed > 0 {
            // Visible proof that samples were lost; normally never shown.
            canvas.fill(Rect::new(bars.x, bars.y, bars.w, 3.0), BRAKE);
        }
    }

    fn draw_trace(&self, canvas: &mut Canvas<'_>) {
        let area = canvas.area();
        let (left, right) = (area.x as u32, (area.x + area.w) as u32);
        for index in self.first_visible()..self.total {
            let x = self.column(index);
            if x + PX_PER_SAMPLE <= left || x >= right {
                continue;
            }
            let current = self.sample(index);
            // Each segment covers exactly this sample's columns, from the previous value to this one, so
            // segments join without gaps and a wrap never draws across the trace.
            let previous = if index > 0 {
                self.sample(index - 1)
            } else {
                current
            };
            let (x0, x1) = (x as f32, (x + PX_PER_SAMPLE) as f32);
            let mut segment = |a: f32, b: f32, color: Color| {
                canvas.line((x0, self.y(a)), (x1, self.y(b)), LINE, color);
            };
            segment(steering_level(previous), steering_level(current), STEERING);
            segment(clutch_pedal(previous), clutch_pedal(current), CLUTCH);
            segment(previous.throttle, current.throttle, THROTTLE);
            let brake = if current.abs_active { ABS } else { BRAKE };
            segment(previous.brake, current.brake, brake);
        }
    }
}

impl Widget for InputTraceWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn layers(&self) -> Vec<LayerSpec> {
        vec![
            LayerSpec {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
                scrolling: false,
            },
            LayerSpec {
                x: self.trace.x as i32,
                y: self.trace.y as i32,
                width: self.columns,
                height: self.trace.h as u32,
                scrolling: true,
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
        let added = self.total - first;
        if added > 0 {
            redraw[1] = if added >= u64::from(self.columns / PX_PER_SAMPLE) {
                Redraw::All
            } else {
                let to = self.column(self.total - 1) + PX_PER_SAMPLE;
                self.columns_area(self.column(first), to)
            };
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

    fn scroll(&self, _layer: usize) -> u32 {
        self.column(self.total)
    }

    fn reset(&mut self) {
        self.total = 0;
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
        assert!(layers[1].scrolling && !layers[0].scrolling);
        assert_eq!(layers[1].width, w.columns);
    }

    #[test]
    fn columns_wrap_around_the_ring() {
        let w = widget();
        let samples = u64::from(w.columns / PX_PER_SAMPLE);
        assert_eq!(w.column(0), 0);
        assert_eq!(w.column(samples), 0, "one full ring later");
        assert_eq!(w.column(samples - 1), w.columns - PX_PER_SAMPLE);
        match w.columns_area(w.columns - 4, 2) {
            Redraw::Areas(a, b) => {
                assert_eq!((a.x, a.w), ((w.columns - 4) as f32, 4.0));
                assert_eq!((b.x, b.w), (0.0, 2.0));
            }
            other => panic!("expected two areas, got {other:?}"),
        }
        assert!(matches!(w.columns_area(10, 14), Redraw::Area(r) if r.x == 10.0 && r.w == 4.0));
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
        assert_eq!(w.y(1.0), 1.0);
        assert_eq!(w.y(0.0), w.trace.h - 1.0);
    }
}
