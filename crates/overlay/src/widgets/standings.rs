//! Standings: race order grouped by class, fastest class first.
//!
//! Each class shows its leaders, plus the player's row when the player is further back. Order comes from
//! `CarIdxClassPosition` (cars without a position yet sort after those with one), gaps from `CarIdxF2Time`
//! relative to the class leader, and names, ratings, and licenses from session info.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use pitot_irsdk::session::SessionInfo;

use crate::config::WidgetConfig;
use crate::pipeline::View;
use crate::render::{Align, Canvas, Color, Rect};

use super::text::FixedText;
use super::{MAX_LAYERS, Redraw, Widget};

const ROW_HEIGHT: f32 = 24.0;
/// Leaders shown per class before the player's row.
const LEADERS: usize = 4;
const MAX_CLASSES: usize = 8;

/// `m:ss.s`, or nothing for a missing time.
pub(crate) fn write_lap_time(out: &mut FixedText, seconds: f32) {
    out.clear();
    if seconds > 0.0 {
        // Round to tenths first, so 59.96 s shows as 1:00.0, not 0:60.0.
        let tenths = (seconds * 10.0).round() as u32;
        let (minutes, rest) = (tenths / 600, tenths % 600);
        let _ = write!(out, "{minutes}:{:02}.{}", rest / 10, rest % 10);
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Row {
    Empty,
    Class { color: u32, name: FixedText },
    Car(CarRow),
}

#[derive(Debug, Clone, PartialEq)]
struct CarRow {
    player: bool,
    in_pit: bool,
    color: u32,
    position: FixedText,
    number: FixedText,
    name: FixedText,
    rating: FixedText,
    license: FixedText,
    license_color: u32,
    last_lap: FixedText,
    gap: FixedText,
}

impl CarRow {
    fn new() -> Self {
        Self {
            player: false,
            in_pit: false,
            color: 0,
            position: FixedText::new(4),
            number: FixedText::new(8),
            name: FixedText::new(64),
            rating: FixedText::new(8),
            license: FixedText::new(8),
            license_color: 0,
            last_lap: FixedText::new(12),
            gap: FixedText::new(12),
        }
    }
}

/// One car's sort key: class rank, then class position (unplaced cars last), then index.
#[derive(Debug, Clone, Copy)]
struct Entry {
    class_rank: usize,
    position: i32,
    car: usize,
}

/// The standings widget.
#[derive(Debug)]
pub struct StandingsWidget {
    width: u32,
    height: u32,
    opacity: f32,
    rows: Vec<Row>,
    next: Vec<Row>,
    /// Spare car rows and class names, reused so updates do not allocate.
    spare: Vec<CarRow>,
    spare_names: Vec<FixedText>,
    entries: Vec<Entry>,
}

impl StandingsWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        let width = config.width.unwrap_or(480);
        let height = config.height.unwrap_or(560).max(ROW_HEIGHT as u32 * 2);
        let rows = (height as f32 / ROW_HEIGHT) as usize;
        Self {
            width,
            height,
            opacity: config.opacity,
            rows: vec![Row::Empty; rows],
            next: vec![Row::Empty; rows],
            spare: (0..rows).map(|_| CarRow::new()).collect(),
            spare_names: (0..MAX_CLASSES).map(|_| FixedText::new(24)).collect(),
            entries: Vec::with_capacity(72),
        }
    }

    /// Moves car rows from `next` back to the spare pool and blanks every row.
    fn recycle(&mut self) {
        for row in &mut self.next {
            match std::mem::replace(row, Row::Empty) {
                Row::Car(car) => self.spare.push(car),
                Row::Class { name, .. } => self.spare_names.push(name),
                Row::Empty => {}
            }
        }
    }

    fn fill(&mut self, view: &View) {
        self.recycle();
        let (snap, vars) = (view.snapshot(), view.vars());
        let Some(session) = view.session() else {
            return;
        };
        let player = snap
            .i32(vars.player_car_idx, 0)
            .and_then(|i| usize::try_from(i).ok())
            .or(session.driver_info.player_car_idx);
        // Classes present, fastest first.
        let mut classes: [(i64, i64); MAX_CLASSES] = [(0, 0); MAX_CLASSES];
        let mut class_count = 0;
        for driver in session.driver_info.drivers.iter().flatten() {
            if driver.is_pace_car || driver.is_spectator {
                continue;
            }
            let id = driver.car_class_id.unwrap_or(0);
            if !classes[..class_count].iter().any(|c| c.1 == id) && class_count < MAX_CLASSES {
                classes[class_count] = (driver.car_class_rel_speed.unwrap_or(0), id);
                class_count += 1;
            }
        }
        classes[..class_count].sort_unstable_by_key(|c| std::cmp::Reverse(c.0));

        self.entries.clear();
        for (car, driver) in session.driver_info.drivers.iter().enumerate() {
            let Some(driver) = driver else { continue };
            if driver.is_pace_car || driver.is_spectator {
                continue;
            }
            let id = driver.car_class_id.unwrap_or(0);
            let class_rank = classes[..class_count]
                .iter()
                .position(|c| c.1 == id)
                .unwrap_or(MAX_CLASSES);
            let position = snap
                .i32(vars.car_class_position, car)
                .filter(|&p| p > 0)
                .unwrap_or(i32::MAX);
            self.entries.push(Entry {
                class_rank,
                position,
                car,
            });
        }
        self.entries
            .sort_unstable_by_key(|e| (e.class_rank, e.position, e.car));

        let mut next_row = 0;
        let rows = self.next.len();
        let mut start = 0;
        while start < self.entries.len() && next_row < rows {
            let rank = self.entries[start].class_rank;
            let end = start
                + self.entries[start..]
                    .iter()
                    .take_while(|e| e.class_rank == rank)
                    .count();
            let leader = self.entries[start].car;
            let leader_f2 = snap.f32(vars.car_f2_time, leader).unwrap_or(0.0);
            // Class header, when several classes share the board.
            if class_count > 1 {
                let driver = session.driver_info.drivers[leader].as_ref();
                let mut name = self.spare_names.pop().unwrap_or_else(|| FixedText::new(24));
                // AI sessions leave the class name empty; the leader's car name is the next best label.
                let label = driver
                    .and_then(|d| {
                        d.car_class_short_name
                            .as_deref()
                            .filter(|n| !n.trim().is_empty())
                    })
                    .or_else(|| driver.and_then(|d| d.car_screen_name_short.as_deref()))
                    .unwrap_or("Class");
                name.set(label);
                self.next[next_row] = Row::Class {
                    color: driver.and_then(|d| d.car_class_color).unwrap_or(0xFF_FFFF),
                    name,
                };
                next_row += 1;
            }
            let player_index =
                player.and_then(|p| self.entries[start..end].iter().position(|e| e.car == p));
            for (offset, entry) in self.entries[start..end].iter().enumerate() {
                if next_row >= rows {
                    break;
                }
                let is_player = Some(offset) == player_index;
                if offset >= LEADERS && !is_player {
                    continue;
                }
                let mut row = self.spare.pop().unwrap_or_else(CarRow::new);
                write_car(
                    &mut row,
                    session,
                    view,
                    entry.car,
                    is_player,
                    leader_f2,
                    entry.position,
                );
                self.next[next_row] = Row::Car(row);
                next_row += 1;
            }
            start = end;
        }
    }
}

fn write_car(
    row: &mut CarRow,
    session: &SessionInfo,
    view: &View,
    car: usize,
    player: bool,
    leader_f2: f32,
    position: i32,
) {
    let (snap, vars) = (view.snapshot(), view.vars());
    let driver = session
        .driver_info
        .drivers
        .get(car)
        .and_then(Option::as_ref);
    row.player = player;
    row.in_pit = snap.bool(vars.car_on_pit_road, car).unwrap_or(false);
    row.color = driver.and_then(|d| d.car_class_color).unwrap_or(0xFF_FFFF);
    row.position.clear();
    if position != i32::MAX {
        let _ = write!(row.position, "{position}");
    }
    row.number.clear();
    let _ = write!(
        row.number,
        "#{}",
        driver.and_then(|d| d.car_number.as_deref()).unwrap_or("?")
    );
    row.name
        .set(driver.and_then(|d| d.user_name.as_deref()).unwrap_or(""));
    row.rating.clear();
    if let Some(rating) = driver.and_then(|d| d.irating).filter(|&r| r > 0) {
        let _ = write!(row.rating, "{:.1}k", rating as f32 / 1000.0);
    }
    row.license
        .set(driver.and_then(|d| d.license.as_deref()).unwrap_or(""));
    row.license_color = driver.and_then(|d| d.license_color).unwrap_or(0x80_8080);
    write_lap_time(
        &mut row.last_lap,
        snap.f32(vars.car_last_lap_time, car).unwrap_or(-1.0),
    );
    row.gap.clear();
    let f2 = snap.f32(vars.car_f2_time, car).unwrap_or(0.0);
    // F2Time is not always maintained (for example before the first timed lap); hide gaps that read as zero.
    if position > 1 && position != i32::MAX && f2 - leader_f2 >= 0.05 {
        let _ = write!(row.gap, "+{:.1}", f2 - leader_f2);
    }
}

impl Widget for StandingsWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(250)
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
        let size = 13.0;
        let white = Color::rgb(0xF0_F2F5, 1.0);
        let dim = Color::rgb(0xA0_A6AE, 1.0);
        for (n, row) in self.rows.iter().enumerate() {
            let y = n as f32 * ROW_HEIGHT;
            match row {
                Row::Empty => {}
                Row::Class { color, name } => {
                    canvas.fill(
                        Rect::new(0.0, y + ROW_HEIGHT - 3.0, w, 2.0),
                        Color::rgb(*color, 0.9),
                    );
                    canvas.text(
                        name.as_str(),
                        Rect::new(8.0, y, w - 16.0, ROW_HEIGHT),
                        size,
                        Align::Left,
                        Color::rgb(*color, 1.0),
                    );
                }
                Row::Car(car) => {
                    if car.player {
                        canvas.fill(
                            Rect::new(0.0, y, w, ROW_HEIGHT),
                            Color::rgb(0xFF_FFFF, 0.10),
                        );
                    }
                    let alpha = if car.in_pit { 0.45 } else { 1.0 };
                    let text = Color { a: alpha, ..white };
                    let muted = Color { a: alpha, ..dim };
                    canvas.fill(
                        Rect::new(0.0, y + 2.0, 4.0, ROW_HEIGHT - 4.0),
                        Color::rgb(car.color, 1.0),
                    );
                    canvas.text(
                        car.position.as_str(),
                        Rect::new(6.0, y, 26.0, ROW_HEIGHT),
                        size,
                        Align::Right,
                        text,
                    );
                    canvas.text(
                        car.number.as_str(),
                        Rect::new(34.0, y, 44.0, ROW_HEIGHT),
                        size,
                        Align::Right,
                        muted,
                    );
                    canvas.text(
                        car.name.as_str(),
                        Rect::new(84.0, y, w - 84.0 - 236.0, ROW_HEIGHT),
                        size,
                        Align::Left,
                        text,
                    );
                    canvas.text(
                        car.rating.as_str(),
                        Rect::new(w - 232.0, y, 44.0, ROW_HEIGHT),
                        size,
                        Align::Right,
                        muted,
                    );
                    canvas.fill(
                        Rect::new(w - 182.0, y + 5.0, 40.0, ROW_HEIGHT - 10.0),
                        Color::rgb(car.license_color, 0.85 * alpha),
                    );
                    canvas.text(
                        car.license.as_str(),
                        Rect::new(w - 182.0, y, 40.0, ROW_HEIGHT),
                        12.0,
                        Align::Center,
                        Color::rgb(0x10_1418, alpha),
                    );
                    let last = if car.in_pit {
                        "PIT"
                    } else {
                        car.last_lap.as_str()
                    };
                    canvas.text(
                        last,
                        Rect::new(w - 138.0, y, 64.0, ROW_HEIGHT),
                        size,
                        Align::Right,
                        muted,
                    );
                    canvas.text(
                        car.gap.as_str(),
                        Rect::new(w - 70.0, y, 62.0, ROW_HEIGHT),
                        size,
                        Align::Right,
                        text,
                    );
                }
            }
        }
    }

    fn reset(&mut self) {
        self.recycle();
        std::mem::swap(&mut self.rows, &mut self.next);
        self.recycle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lap_times_are_minutes_and_seconds() {
        let mut out = FixedText::new(12);
        write_lap_time(&mut out, 105.34);
        assert_eq!(out.as_str(), "1:45.3");
        write_lap_time(&mut out, 59.96);
        assert_eq!(out.as_str(), "1:00.0");
        write_lap_time(&mut out, 61.05);
        assert_eq!(out.as_str(), "1:01.1");
        write_lap_time(&mut out, -1.0);
        assert_eq!(out.as_str(), "");
    }
}
