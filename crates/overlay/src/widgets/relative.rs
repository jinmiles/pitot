//! Relative: the cars nearest on track ahead of and behind the player, with time gaps.
//!
//! Gaps come from `CarIdxEstTime` (the sim's estimate of each car's time into the lap), as differences to the
//! player normalized to half a lap either way, so cars across the start line are handled. When estimated times
//! are missing, the distance difference times the player's lap time is used instead.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::config::WidgetConfig;
use crate::pipeline::View;
use crate::render::{Align, Canvas, Color, Rect};

use super::text::FixedText;
use super::{MAX_LAYERS, Redraw, Widget};

/// Cars shown on each side of the player.
const SIDE: usize = 3;
const ROWS: usize = 2 * SIDE + 1;
/// Lap time used when neither the session nor the class gives one.
const FALLBACK_LAP: f32 = 90.0;

/// `CarIdxTrackSurface` value for a car that is not in the world.
const NOT_IN_WORLD: i32 = -1;

/// Normalizes a gap in seconds to the range of half a lap either way.
pub(crate) fn normalize_gap(gap: f32, lap: f32) -> f32 {
    let half = lap / 2.0;
    let mut gap = gap % lap;
    if gap > half {
        gap -= lap;
    } else if gap < -half {
        gap += lap;
    }
    gap
}

/// Laps between a car and the player, accounting for one of them having just crossed the line: positive when
/// the car is laps ahead (lapping the player), negative when it is laps behind.
pub(crate) fn lap_difference(
    car_lap: i32,
    car_pct: f32,
    player_lap: i32,
    player_pct: f32,
    gap: f32,
) -> i32 {
    let mut diff = car_lap - player_lap;
    if gap > 0.0 && car_pct < player_pct {
        diff -= 1;
    } else if gap < 0.0 && car_pct > player_pct {
        diff += 1;
    }
    diff
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Laps {
    Same,
    /// The car is at least a lap ahead.
    Ahead,
    /// The car is at least a lap behind.
    Behind,
}

#[derive(Debug, Clone, PartialEq)]
struct Row {
    used: bool,
    player: bool,
    in_pit: bool,
    laps: Laps,
    class_color: u32,
    number: FixedText,
    name: FixedText,
    rating: FixedText,
    gap: FixedText,
}

impl Row {
    fn new() -> Self {
        Self {
            used: false,
            player: false,
            in_pit: false,
            laps: Laps::Same,
            class_color: 0,
            number: FixedText::new(8),
            name: FixedText::new(64),
            rating: FixedText::new(8),
            gap: FixedText::new(12),
        }
    }
}

/// The relative widget.
#[derive(Debug)]
pub struct RelativeWidget {
    width: u32,
    height: u32,
    opacity: f32,
    rows: Vec<Row>,
    next: Vec<Row>,
    /// (gap, car index) of cars on track this update; capacity grows to the car count once.
    cars: Vec<(f32, usize)>,
}

impl RelativeWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        Self {
            width: config.width.unwrap_or(430),
            height: config.height.unwrap_or(280).max(ROWS as u32 * 12),
            opacity: config.opacity,
            rows: (0..ROWS).map(|_| Row::new()).collect(),
            next: (0..ROWS).map(|_| Row::new()).collect(),
            cars: Vec::with_capacity(72),
        }
    }

    fn fill(&mut self, view: &View) {
        for row in &mut self.next {
            row.used = false;
        }
        let snap = view.snapshot();
        let vars = view.vars();
        let session = view.session();
        let Some(player) = snap
            .i32(vars.player_car_idx, 0)
            .and_then(|i| usize::try_from(i).ok())
            .or_else(|| session.and_then(|s| s.driver_info.player_car_idx))
        else {
            return;
        };
        let driver = |idx: usize| session.and_then(|s| s.driver_info.drivers.get(idx)?.as_ref());
        let lap = session
            .and_then(|s| s.driver_info.est_lap_time)
            .map(|t| t as f32)
            .or_else(|| {
                driver(player)
                    .and_then(|d| d.car_class_est_lap_time)
                    .map(|t| t as f32)
            })
            .filter(|&t| t > 1.0)
            .unwrap_or(FALLBACK_LAP);
        let pct = |i: usize| snap.f32(vars.car_lap_dist_pct, i).unwrap_or(-1.0);
        let est = |i: usize| snap.f32(vars.car_est_time, i).unwrap_or(0.0);
        let (player_pct, player_est) = (pct(player), est(player));
        if player_pct < 0.0 {
            return;
        }
        self.cars.clear();
        for i in 0..snap.count(vars.car_lap_dist_pct) {
            if i == player || pct(i) < 0.0 {
                continue;
            }
            if snap.i32(vars.car_track_surface, i) == Some(NOT_IN_WORLD) {
                continue;
            }
            if driver(i).is_some_and(|d| d.is_pace_car || d.is_spectator) {
                continue;
            }
            let gap = if est(i) > 0.0 && player_est > 0.0 {
                normalize_gap(est(i) - player_est, lap)
            } else {
                normalize_gap((pct(i) - player_pct) * lap, lap)
            };
            self.cars.push((gap, i));
        }
        // Nearest ahead (smallest positive gaps) and nearest behind (largest negative gaps).
        self.cars.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let split = self.cars.partition_point(|&(gap, _)| gap <= 0.0);
        let behind = self.cars[..split].iter().rev().take(SIDE);
        let ahead = self.cars[split..].iter().take(SIDE);
        let lap_of = |i: usize| snap.i32(vars.car_lap, i).unwrap_or(0);
        // Rows: farthest ahead at the top, the player in the middle, farthest behind at the bottom.
        let mut slots: [Option<(f32, usize)>; ROWS] = [None; ROWS];
        for (n, &car) in ahead.enumerate() {
            slots[SIDE - 1 - n] = Some(car);
        }
        slots[SIDE] = Some((0.0, player));
        for (n, &car) in behind.enumerate() {
            slots[SIDE + 1 + n] = Some(car);
        }
        for (row, slot) in self.next.iter_mut().zip(slots) {
            let Some((gap, i)) = slot else {
                continue;
            };
            let info = driver(i);
            row.used = true;
            row.player = i == player;
            row.in_pit = snap.bool(vars.car_on_pit_road, i).unwrap_or(false);
            row.laps = match lap_difference(lap_of(i), pct(i), lap_of(player), player_pct, gap) {
                d if d > 0 && !row.player => Laps::Ahead,
                d if d < 0 && !row.player => Laps::Behind,
                _ => Laps::Same,
            };
            row.class_color = info.and_then(|d| d.car_class_color).unwrap_or(0xFF_FFFF);
            row.number.clear();
            let _ = write!(
                row.number,
                "#{}",
                info.and_then(|d| d.car_number.as_deref()).unwrap_or("?")
            );
            row.name
                .set(info.and_then(|d| d.user_name.as_deref()).unwrap_or(""));
            row.rating.clear();
            if let Some(rating) = info.and_then(|d| d.irating).filter(|&r| r > 0) {
                let _ = write!(row.rating, "{:.1}k", rating as f32 / 1000.0);
            }
            row.gap.clear();
            if !row.player {
                // As in the sim's own relative: cars ahead show a negative gap.
                let _ = write!(row.gap, "{:+.1}", -gap);
            }
        }
    }
}

impl Widget for RelativeWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(100)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        self.fill(view);
        if self.next != self.rows {
            std::mem::swap(&mut self.rows, &mut self.next);
            redraw[0] = Redraw::All;
        }
    }

    fn draw(&self, _layer: usize, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(0x10_1418, self.opacity),
        );
        let row_h = h / ROWS as f32;
        let size = (row_h * 0.45).clamp(12.0, 18.0);
        for (n, row) in self.rows.iter().enumerate() {
            if !row.used {
                continue;
            }
            let y = n as f32 * row_h;
            if row.player {
                canvas.fill(Rect::new(0.0, y, w, row_h), Color::rgb(0xFF_FFFF, 0.10));
            }
            canvas.fill(
                Rect::new(0.0, y + 2.0, 4.0, row_h - 4.0),
                Color::rgb(row.class_color, 1.0),
            );
            let base = match row.laps {
                Laps::Ahead => 0xFF_8A80,
                Laps::Behind => 0x82_B1FF,
                Laps::Same => 0xF0_F2F5,
            };
            let text = Color::rgb(base, if row.in_pit { 0.45 } else { 1.0 });
            let dim = Color::rgb(0xA0_A6AE, if row.in_pit { 0.45 } else { 1.0 });
            canvas.text(
                row.number.as_str(),
                Rect::new(10.0, y, 48.0, row_h),
                size,
                Align::Right,
                dim,
            );
            canvas.text(
                row.name.as_str(),
                Rect::new(66.0, y, w - 66.0 - 132.0, row_h),
                size,
                Align::Left,
                text,
            );
            let rating_or_pit = if row.in_pit {
                "PIT"
            } else {
                row.rating.as_str()
            };
            canvas.text(
                rating_or_pit,
                Rect::new(w - 130.0, y, 54.0, row_h),
                size,
                Align::Right,
                dim,
            );
            canvas.text(
                row.gap.as_str(),
                Rect::new(w - 72.0, y, 64.0, row_h),
                size,
                Align::Right,
                text,
            );
        }
    }

    fn reset(&mut self) {
        for row in self.rows.iter_mut().chain(self.next.iter_mut()) {
            row.used = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_wrap_at_half_a_lap() {
        assert_eq!(normalize_gap(2.0, 100.0), 2.0);
        assert_eq!(normalize_gap(-2.0, 100.0), -2.0);
        assert_eq!(
            normalize_gap(98.0, 100.0),
            -2.0,
            "just behind across the line"
        );
        assert_eq!(
            normalize_gap(-98.0, 100.0),
            2.0,
            "just ahead across the line"
        );
        assert_eq!(normalize_gap(150.0, 100.0), 50.0);
    }

    #[test]
    fn lap_difference_handles_the_line() {
        // Same lap, car ahead on track.
        assert_eq!(lap_difference(5, 0.6, 5, 0.5, 1.0), 0);
        // Car just crossed the line ahead of the player: its lap counter is one higher, same lap really.
        assert_eq!(lap_difference(6, 0.02, 5, 0.98, 1.0), 0);
        // Player just crossed the line, car just behind: the player's counter is one higher, same lap.
        assert_eq!(lap_difference(5, 0.98, 6, 0.02, -1.0), 0);
        // Car a lap ahead and close behind on track (it is about to lap the player).
        assert_eq!(lap_difference(6, 0.45, 5, 0.5, -2.0), 1);
        // Car a lap down, just ahead on track.
        assert_eq!(lap_difference(4, 0.55, 5, 0.5, 2.0), -1);
    }
}
