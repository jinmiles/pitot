//! Fuel calculator: current fuel and laps left in the race, then by the average of recent clean laps and by the
//! last clean lap: fuel per lap, laps the fuel lasts, fuel to add, and fuel left at the end.
//!
//! Use per lap is the average of the last few clean laps: full laps driven under green-flag running, without
//! visiting pit road and without fuel being added. Laps to go come from the session's remaining laps, or from the time
//! remaining divided by the lap time for timed sessions.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use pitot_irsdk::flags::{session_flags, session_state};

use crate::config::WidgetConfig;
use crate::pipeline::View;
use crate::render::{Align, Canvas, Color, Rect};

use super::text::FixedText;
use super::{MAX_LAYERS, Redraw, Widget};

/// Clean laps averaged for use per lap.
const LAPS_AVERAGED: usize = 5;
/// `SessionLapsRemainEx` value for a session without a lap limit.
const UNLIMITED_LAPS: i32 = 32767;
/// Flags that stop green-flag running for everyone: a full-course caution or a red flag.
const NOT_GREEN_FLAGS: u32 = session_flags::CAUTION
    | session_flags::CAUTION_WAVING
    | session_flags::ONE_LAP_TO_GREEN
    | session_flags::RED;

/// Whether the session is under green-flag running. Only states that are clearly not (getting in the car,
/// warm-up, pace laps, cool-down) and full-course cautions count against it; an unknown state counts as green.
pub(crate) fn is_green(state: Option<i32>, flags: u32) -> bool {
    let paced = state.is_some_and(|s| {
        matches!(
            s,
            session_state::GET_IN_CAR
                | session_state::WARMUP
                | session_state::PARADE_LAPS
                | session_state::COOL_DOWN
        )
    });
    !paced && flags & NOT_GREEN_FLAGS == 0
}

/// Tracks fuel used per clean lap.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct FuelTracker {
    last_completed: Option<i32>,
    lap_start_fuel: Option<f32>,
    last_fuel: Option<f32>,
    clean: bool,
    usage: [f32; LAPS_AVERAGED],
    count: usize,
    next: usize,
}

impl FuelTracker {
    /// Feeds the latest values; call regularly (at least once per lap). `green` is false while the car is on pit
    /// road or the session is not under green-flag running; a lap with any such moment is not counted.
    pub(crate) fn observe(&mut self, laps_completed: i32, fuel: f32, green: bool) {
        if !green {
            self.clean = false;
        }
        if self.last_fuel.is_some_and(|last| fuel > last + 0.05) {
            // Fuel was added: this lap does not count.
            self.clean = false;
        }
        self.last_fuel = Some(fuel);
        if self.last_completed == Some(laps_completed) {
            return;
        }
        let one_more = self
            .last_completed
            .is_some_and(|last| laps_completed == last + 1);
        if one_more
            && self.clean
            && let Some(start) = self.lap_start_fuel
            && start - fuel > 0.01
        {
            self.usage[self.next] = start - fuel;
            self.next = (self.next + 1) % LAPS_AVERAGED;
            self.count = (self.count + 1).min(LAPS_AVERAGED);
        }
        // Only a lap seen from its start can count, not the one in progress when tracking began.
        self.clean = green && self.last_completed.is_some();
        self.last_completed = Some(laps_completed);
        self.lap_start_fuel = Some(fuel);
    }

    /// Average fuel per clean lap, once at least one clean lap was seen.
    pub(crate) fn per_lap(&self) -> Option<f32> {
        (self.count > 0).then(|| self.usage[..self.count].iter().sum::<f32>() / self.count as f32)
    }

    /// Fuel used on the most recent clean lap.
    pub(crate) fn last_lap(&self) -> Option<f32> {
        (self.count > 0).then(|| self.usage[(self.next + LAPS_AVERAGED - 1) % LAPS_AVERAGED])
    }
}

/// Laps still to drive: the session's remaining laps, or for a timed session the time left over the lap time,
/// plus the lap in progress when time runs out.
pub(crate) fn laps_to_go(
    laps_remaining: Option<i32>,
    time_remaining: Option<f64>,
    lap_time: Option<f32>,
) -> Option<f32> {
    match laps_remaining {
        Some(laps) if laps > 0 && laps < UNLIMITED_LAPS => Some(laps as f32),
        _ => {
            let time = time_remaining.filter(|&t| t > 0.0 && t < 1.0e6)? as f32;
            let lap = lap_time.filter(|&t| t > 1.0)?;
            Some((time / lap).ceil() + 1.0)
        }
    }
}

/// One basis for the estimates: the average of recent clean laps, or the last clean lap.
#[derive(Debug, Clone, PartialEq)]
struct Estimate {
    per_lap: FixedText,
    laps_left: FixedText,
    refuel: FixedText,
    at_end: FixedText,
    /// The fuel left at the end is negative: a stop is needed.
    short: bool,
}

impl Estimate {
    fn new() -> Self {
        Self {
            per_lap: FixedText::new(12),
            laps_left: FixedText::new(12),
            refuel: FixedText::new(12),
            at_end: FixedText::new(12),
            short: false,
        }
    }

    /// Fills the values for `per_lap` fuel per lap, or dashes without it. `to_go` is the laps left in the race.
    fn fill(&mut self, fuel: f32, per_lap: Option<f32>, to_go: Option<f32>) {
        for text in [
            &mut self.per_lap,
            &mut self.laps_left,
            &mut self.refuel,
            &mut self.at_end,
        ] {
            text.set(NO_VALUE);
        }
        self.short = false;
        let Some(per_lap) = per_lap.filter(|&p| p > 0.0) else {
            return;
        };
        write_value(&mut self.per_lap, per_lap);
        write_value(&mut self.laps_left, fuel / per_lap);
        if let Some(to_go) = to_go {
            let at_end = fuel - to_go * per_lap;
            self.short = at_end < 0.0;
            write_value(&mut self.refuel, (-at_end).max(0.0));
            write_value(&mut self.at_end, at_end);
        }
    }
}

/// Shown for a value not known yet.
const NO_VALUE: &str = "--.--";

fn write_value(out: &mut FixedText, value: f32) {
    out.clear();
    let _ = write!(out, "{value:.2}");
}

#[derive(Debug, Clone, PartialEq)]
struct Lines {
    fuel: FixedText,
    laps_to_go: FixedText,
    average: Estimate,
    last: Estimate,
    /// Lights the pit sign: at the average, the fuel does not last to the end.
    pit: bool,
}

impl Lines {
    fn new() -> Self {
        Self {
            fuel: FixedText::new(12),
            laps_to_go: FixedText::new(12),
            average: Estimate::new(),
            last: Estimate::new(),
            pit: false,
        }
    }
}

/// The fuel widget.
#[derive(Debug)]
pub struct FuelWidget {
    width: u32,
    height: u32,
    opacity: f32,
    tracker: FuelTracker,
    lines: Lines,
    next: Lines,
}

impl FuelWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        Self {
            width: config.width.unwrap_or(420),
            height: config.height.unwrap_or(150).max(90),
            opacity: config.opacity,
            tracker: FuelTracker::default(),
            lines: Lines::new(),
            next: Lines::new(),
        }
    }
}

const WHITE: u32 = 0xF0_F2F5;
const DIM: u32 = 0xA0_A6AE;
const AVERAGE: u32 = 0x82_B1FF;
const LAST: u32 = 0xF2_C14E;
const SHORT: u32 = 0xFF_6B5E;
const BACKGROUND: u32 = 0x10_1418;

impl Widget for FuelWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        let (snap, vars) = (view.snapshot(), view.vars());
        let next = &mut self.next;
        next.fuel.set(NO_VALUE);
        next.laps_to_go.set(NO_VALUE);
        next.pit = false;
        if let Some(fuel) = snap.f32(vars.fuel_level, 0) {
            let laps_completed = snap.i32(vars.lap_completed, 0).unwrap_or(0);
            let on_pit = snap.bool(vars.on_pit_road, 0).unwrap_or(false);
            let flags = snap.bitfield(vars.session_flags, 0).unwrap_or(0);
            let green = !on_pit && is_green(snap.i32(vars.session_state, 0), flags);
            self.tracker.observe(laps_completed, fuel, green);
            write_value(&mut next.fuel, fuel);
            let lap_time = snap
                .f32(vars.lap_last_time, 0)
                .filter(|&t| t > 0.0)
                .or_else(|| view.session()?.driver_info.est_lap_time.map(|t| t as f32));
            let to_go = laps_to_go(
                snap.i32(vars.session_laps_remain, 0),
                snap.f64(vars.session_time_remain, 0),
                lap_time,
            );
            if let Some(laps) = to_go {
                write_value(&mut next.laps_to_go, laps);
            }
            next.average.fill(fuel, self.tracker.per_lap(), to_go);
            next.last.fill(fuel, self.tracker.last_lap(), to_go);
            next.pit = next.average.short;
        } else {
            next.average.fill(0.0, None, None);
            next.last.fill(0.0, None, None);
        }
        if self.next != self.lines {
            std::mem::swap(&mut self.lines, &mut self.next);
            redraw[0] = Redraw::All;
        }
    }

    fn draw(&self, _layer: usize, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(BACKGROUND, self.opacity),
        );
        let dim = Color::rgb(DIM, 1.0);
        let white = Color::rgb(WHITE, 1.0);
        // Six lines: labels and values of the current state, then the average's and the last lap's.
        let line = h / 6.0;
        let column = (w - 24.0) / 4.0;
        let label = (line * 0.5).clamp(10.0, 13.0);
        let value = (line * 0.8).clamp(14.0, 24.0);
        let cell = |col: f32, row: f32| Rect::new(12.0 + col * column, row * line, column, line);
        let l = &self.lines;
        canvas.text("Fuel Level", cell(0.0, 0.0), label, Align::Left, dim);
        canvas.text("Laps in Race", cell(1.0, 0.0), label, Align::Left, dim);
        canvas.text(l.fuel.as_str(), cell(0.0, 1.0), value, Align::Left, white);
        canvas.text(
            l.laps_to_go.as_str(),
            cell(1.0, 1.0),
            value,
            Align::Left,
            white,
        );
        // The pit sign, lit when the fuel does not last to the end.
        let sign = Rect::new(
            12.0 + 3.0 * column + column * 0.2,
            line * 0.4,
            column * 0.6,
            line * 1.2,
        );
        let (frame, ink) = if l.pit {
            (Color::rgb(SHORT, 1.0), Color::rgb(SHORT, 1.0))
        } else {
            (Color::rgb(DIM, 0.35), Color::rgb(DIM, 0.35))
        };
        let (x0, y0, x1, y1) = (sign.x, sign.y, sign.x + sign.w, sign.y + sign.h);
        canvas.line((x0, y0), (x1, y0), 1.5, frame);
        canvas.line((x0, y1), (x1, y1), 1.5, frame);
        canvas.line((x0, y0), (x0, y1), 1.5, frame);
        canvas.line((x1, y0), (x1, y1), 1.5, frame);
        canvas.text("PIT", sign, label + 2.0, Align::Center, ink);

        for (col, name) in ["Average", "Laps Remain", "Refuel", "Fuel at End"]
            .iter()
            .enumerate()
        {
            canvas.text(
                name,
                cell(col as f32, 2.0),
                label,
                Align::Left,
                if col == 0 {
                    Color::rgb(AVERAGE, 1.0)
                } else {
                    dim
                },
            );
        }
        canvas.text(
            "Last",
            cell(0.0, 4.0),
            label,
            Align::Left,
            Color::rgb(LAST, 1.0),
        );
        for (row, estimate, color) in [(3.0, &l.average, AVERAGE), (5.0, &l.last, LAST)] {
            let at_end = if estimate.short { SHORT } else { color };
            for (col, text, ink) in [
                (0.0, &estimate.per_lap, color),
                (1.0, &estimate.laps_left, color),
                (2.0, &estimate.refuel, color),
                (3.0, &estimate.at_end, at_end),
            ] {
                canvas.text(
                    text.as_str(),
                    cell(col, row),
                    value,
                    Align::Left,
                    Color::rgb(ink, 1.0),
                );
            }
        }
    }

    fn reset(&mut self) {
        self.tracker = FuelTracker::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_clean_laps_only() {
        let mut t = FuelTracker::default();
        t.observe(-1, 51.0, true); // tracking starts mid-lap
        t.observe(0, 50.0, true);
        assert_eq!(t.per_lap(), None, "partial first lap ignored");
        t.observe(0, 48.5, true);
        t.observe(1, 47.0, true); // lap 1: 3.0 L
        assert_eq!(t.per_lap(), Some(3.0));
        t.observe(1, 46.0, false); // pit road or caution during lap 2
        t.observe(2, 44.0, true);
        assert_eq!(t.per_lap(), Some(3.0), "pit or caution lap ignored");
        t.observe(2, 60.0, true); // refuel during lap 3
        t.observe(3, 57.0, true);
        assert_eq!(t.per_lap(), Some(3.0), "refuel lap ignored");
        t.observe(4, 53.0, true); // lap 4: 4.0 L
        assert_eq!(t.per_lap(), Some(3.5));
        assert_eq!(t.last_lap(), Some(4.0));
    }

    #[test]
    fn estimates_by_average_and_last_lap() {
        let mut e = Estimate::new();
        e.fill(52.58, None, Some(12.0));
        assert_eq!(e.per_lap.as_str(), NO_VALUE, "measuring");
        e.fill(52.58, Some(4.44), Some(12.0));
        assert_eq!(e.per_lap.as_str(), "4.44");
        assert_eq!(e.laps_left.as_str(), "11.84");
        assert_eq!(e.refuel.as_str(), "0.70");
        assert_eq!(e.at_end.as_str(), "-0.70");
        assert!(e.short);
        e.fill(60.0, Some(4.44), Some(12.0));
        assert_eq!(e.refuel.as_str(), "0.00");
        assert_eq!(e.at_end.as_str(), "6.72");
        assert!(!e.short);
        e.fill(60.0, Some(4.44), None);
        assert_eq!(e.refuel.as_str(), NO_VALUE, "no race length");
    }

    #[test]
    fn keeps_only_recent_laps() {
        let mut t = FuelTracker::default();
        let mut fuel = 100.0;
        t.observe(0, fuel, true);
        for lap in 1..=11 {
            fuel -= if lap <= 6 { 2.0 } else { 3.0 };
            t.observe(lap, fuel, true);
        }
        assert_eq!(t.per_lap(), Some(3.0), "last five laps only");
    }

    #[test]
    fn pace_laps_before_the_start_are_ignored() {
        // Rolling start from a real race recording: the lap ending at the first line crossing includes
        // warm-up and pace laps (4.96 L against about 3.9 L per green lap).
        let mut t = FuelTracker::default();
        let pace = is_green(Some(session_state::PARADE_LAPS), 0);
        t.observe(0, 60.0, is_green(Some(session_state::GET_IN_CAR), 0));
        t.observe(-1, 60.0, is_green(Some(session_state::WARMUP), 0));
        t.observe(-1, 57.0, pace);
        t.observe(0, 55.04, is_green(Some(session_state::RACING), 0));
        assert_eq!(t.per_lap(), None, "start lap ignored");
        t.observe(1, 51.19, true);
        assert!((t.per_lap().unwrap_or(0.0) - 3.85).abs() < 0.01);
    }

    #[test]
    fn green_running() {
        assert!(is_green(Some(session_state::RACING), session_flags::GREEN));
        assert!(is_green(
            Some(session_state::CHECKERED),
            session_flags::CHECKERED
        ));
        assert!(is_green(None, 0), "unknown state");
        assert!(
            is_green(Some(session_state::RACING), session_flags::YELLOW),
            "local yellow"
        );
        assert!(!is_green(Some(session_state::PARADE_LAPS), 0));
        assert!(!is_green(
            Some(session_state::RACING),
            session_flags::CAUTION
        ));
        assert!(!is_green(
            Some(session_state::RACING),
            session_flags::CAUTION_WAVING
        ));
    }

    #[test]
    fn laps_to_go_by_laps_or_time() {
        assert_eq!(laps_to_go(Some(12), Some(900.0), Some(100.0)), Some(12.0));
        assert_eq!(
            laps_to_go(Some(UNLIMITED_LAPS), Some(950.0), Some(100.0)),
            Some(11.0)
        );
        assert_eq!(laps_to_go(None, Some(950.0), None), None);
        assert_eq!(
            laps_to_go(Some(UNLIMITED_LAPS), Some(604_800.0 * 2.0), Some(100.0)),
            None,
            "unlimited time"
        );
    }
}
