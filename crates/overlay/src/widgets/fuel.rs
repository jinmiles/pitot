//! Fuel calculator: current fuel, use per lap, laps left, and fuel needed to finish.
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

#[derive(Debug, Clone, PartialEq)]
struct Lines {
    fuel: FixedText,
    per_lap: FixedText,
    laps: FixedText,
    need: FixedText,
    short: bool,
}

impl Lines {
    fn new() -> Self {
        Self {
            fuel: FixedText::new(16),
            per_lap: FixedText::new(16),
            laps: FixedText::new(16),
            need: FixedText::new(24),
            short: false,
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
            width: config.width.unwrap_or(300),
            height: config.height.unwrap_or(120).max(48),
            opacity: config.opacity,
            tracker: FuelTracker::default(),
            lines: Lines::new(),
            next: Lines::new(),
        }
    }
}

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
        next.fuel.clear();
        next.per_lap.clear();
        next.laps.clear();
        next.need.clear();
        next.short = false;
        if let Some(fuel) = snap.f32(vars.fuel_level, 0) {
            let laps_completed = snap.i32(vars.lap_completed, 0).unwrap_or(0);
            let on_pit = snap.bool(vars.on_pit_road, 0).unwrap_or(false);
            let flags = snap.bitfield(vars.session_flags, 0).unwrap_or(0);
            let green = !on_pit && is_green(snap.i32(vars.session_state, 0), flags);
            self.tracker.observe(laps_completed, fuel, green);
            let _ = write!(next.fuel, "{fuel:.1} L");
            if let Some(per_lap) = self.tracker.per_lap() {
                let _ = write!(next.per_lap, "{per_lap:.2} L/lap");
                let _ = write!(next.laps, "{:.1} laps", fuel / per_lap);
                let lap_time = snap
                    .f32(vars.lap_last_time, 0)
                    .filter(|&t| t > 0.0)
                    .or_else(|| view.session()?.driver_info.est_lap_time.map(|t| t as f32));
                let remaining = laps_to_go(
                    snap.i32(vars.session_laps_remain, 0),
                    snap.f64(vars.session_time_remain, 0),
                    lap_time,
                );
                if let Some(laps) = remaining {
                    let need = laps * per_lap - fuel;
                    next.short = need > 0.0;
                    if need > 0.0 {
                        let _ = write!(next.need, "add {need:.1} L");
                    } else {
                        let _ = write!(next.need, "{:.1} L spare", -need);
                    }
                }
            } else {
                next.per_lap.set("measuring");
            }
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
            Color::rgb(0x10_1418, self.opacity),
        );
        let white = Color::rgb(0xF0_F2F5, 1.0);
        let dim = Color::rgb(0xA0_A6AE, 1.0);
        let half = h / 2.0;
        canvas.text(
            "FUEL",
            Rect::new(12.0, 4.0, 60.0, 18.0),
            12.0,
            Align::Left,
            dim,
        );
        canvas.text(
            self.lines.fuel.as_str(),
            Rect::new(12.0, 18.0, w / 2.0, half - 14.0),
            26.0,
            Align::Left,
            white,
        );
        canvas.text(
            self.lines.per_lap.as_str(),
            Rect::new(w / 2.0, 6.0, w / 2.0 - 12.0, half / 2.0),
            14.0,
            Align::Right,
            dim,
        );
        canvas.text(
            self.lines.laps.as_str(),
            Rect::new(w / 2.0, 6.0 + half / 2.0, w / 2.0 - 12.0, half / 2.0),
            14.0,
            Align::Right,
            white,
        );
        let need = if self.lines.short {
            Color::rgb(0xF2_A33A, 1.0)
        } else {
            Color::rgb(0x3D_D68C, 1.0)
        };
        canvas.text(
            self.lines.need.as_str(),
            Rect::new(12.0, half + 4.0, w - 24.0, half - 8.0),
            18.0,
            Align::Left,
            need,
        );
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
