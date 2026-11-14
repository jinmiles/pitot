//! Pit widget: the cars of the player's class with their last pit stop: the lap, the time standing still in the
//! pit stall, and the most fuel that standing time allows (an upper bound, since tire changes and repairs also take
//! standing time). Above them, the pit lane loss measured at this track and the refueling speed.
//!
//! The stops come from [`PitTracker`], which follows every car at this widget's rate.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::{self, WidgetConfig};
use crate::pipeline::View;
use crate::render::{Align, Canvas, Color, Rect};

use super::pitstops::{PitModel, PitTracker, player_index};
use super::text::FixedText;
use super::{MAX_LAYERS, Redraw, Widget};

/// Most car rows.
const MAX_ROWS: usize = 16;
/// Rows above the cars: the lane loss and refueling speed, then the column names.
const HEAD_ROWS: usize = 2;
/// Row height the default size is laid out for.
const ROW_HEIGHT: u32 = 22;

#[derive(Debug, Clone, PartialEq)]
struct Row {
    used: bool,
    player: bool,
    in_pit: bool,
    /// Another car model than the player's: its tank and refueling speed are assumed to be the player's.
    other_model: bool,
    class_color: u32,
    position: FixedText,
    number: FixedText,
    name: FixedText,
    stop: FixedText,
    stood: FixedText,
    fuel: FixedText,
}

impl Row {
    fn new() -> Self {
        Self {
            used: false,
            player: false,
            in_pit: false,
            other_model: false,
            class_color: 0,
            position: FixedText::new(4),
            number: FixedText::new(8),
            name: FixedText::new(64),
            stop: FixedText::new(8),
            stood: FixedText::new(8),
            fuel: FixedText::new(8),
        }
    }
}

/// The line above the column names.
#[derive(Debug, Clone, PartialEq)]
struct Head {
    loss: FixedText,
    rate: FixedText,
    rate_measured: bool,
    /// What a stop that fills the player's whole tank costs.
    full: FixedText,
}

impl Head {
    fn new() -> Self {
        Self {
            loss: FixedText::new(8),
            rate: FixedText::new(8),
            rate_measured: false,
            full: FixedText::new(8),
        }
    }

    fn fill(&mut self, model: &PitModel) {
        self.loss.clear();
        match model.lane_loss {
            Some(loss) => {
                let _ = write!(self.loss, "{loss:.1}");
            }
            None => self.loss.set("--.-"),
        }
        self.rate.clear();
        let _ = write!(self.rate, "{:.1}", model.refuel_rate);
        self.rate_measured = model.rate_measured;
        self.full.clear();
        match model.tank.and_then(|tank| model.expected_loss(tank)) {
            Some(loss) => {
                let _ = write!(self.full, "{loss:.1}");
            }
            None => self.full.set("--.-"),
        }
    }
}

/// The pit widget.
#[derive(Debug)]
pub struct PitWidget {
    width: u32,
    height: u32,
    opacity: f32,
    rows: Vec<Row>,
    next: Vec<Row>,
    head: Head,
    next_head: Head,
    tracker: PitTracker,
    /// (class position, car index) of the player's class this update; capacity grows to the car count once.
    cars: Vec<(i32, usize)>,
}

impl PitWidget {
    /// A pit widget that keeps what it learns in `pit.toml` in the configuration directory.
    pub fn new(config: &WidgetConfig) -> Self {
        Self::with_store(config, config::pit_file())
    }

    /// A pit widget that keeps what it learns in `store`, or only in memory when `None`.
    pub fn with_store(config: &WidgetConfig, store: Option<PathBuf>) -> Self {
        let height = config.height.unwrap_or(280).max(ROW_HEIGHT * 3);
        let rows = ((height / ROW_HEIGHT) as usize)
            .saturating_sub(HEAD_ROWS)
            .clamp(1, MAX_ROWS);
        Self {
            width: config.width.unwrap_or(400),
            height,
            opacity: config.opacity,
            rows: (0..rows).map(|_| Row::new()).collect(),
            next: (0..rows).map(|_| Row::new()).collect(),
            head: Head::new(),
            next_head: Head::new(),
            tracker: PitTracker::new(store),
            cars: Vec::with_capacity(72),
        }
    }

    fn fill(&mut self, view: &View) {
        for row in &mut self.next {
            row.used = false;
        }
        let model = self.tracker.model();
        self.next_head.fill(&model);
        let (snap, vars, session) = (view.snapshot(), view.vars(), view.session());
        let Some(player) = player_index(snap, vars, session) else {
            return;
        };
        let driver = |idx: usize| session.and_then(|s| s.driver_info.drivers.get(idx)?.as_ref());
        let class_of = |idx: usize| {
            snap.i32(vars.car_class, idx)
                .map(i64::from)
                .or_else(|| driver(idx).and_then(|d| d.car_class_id))
        };
        let player_class = class_of(player);
        let player_model = driver(player).and_then(|d| d.car_screen_name.as_deref());
        // The player's class in class order; unplaced cars (no lap yet) after the placed ones.
        self.cars.clear();
        for i in 0..snap.count(vars.car_lap_dist_pct) {
            if driver(i).is_some_and(|d| d.is_pace_car || d.is_spectator)
                || class_of(i) != player_class
            {
                continue;
            }
            let position = snap.i32(vars.car_class_position, i).unwrap_or(0);
            if position > 0 || i == player {
                self.cars
                    .push((if position > 0 { position } else { i32::MAX }, i));
            }
        }
        self.cars.sort_unstable();
        let rows = self.next.len();
        let at = self
            .cars
            .iter()
            .position(|&(_, i)| i == player)
            .unwrap_or(0);
        let start = at
            .saturating_sub(rows / 2)
            .min(self.cars.len().saturating_sub(rows));
        let stops = self.tracker.stops();
        for (row, &(position, i)) in self.next.iter_mut().zip(&self.cars[start..]) {
            let info = driver(i);
            row.used = true;
            row.player = i == player;
            row.in_pit = snap.bool(vars.car_on_pit_road, i).unwrap_or(false);
            row.other_model = info.and_then(|d| d.car_screen_name.as_deref()) != player_model;
            row.class_color = info.and_then(|d| d.car_class_color).unwrap_or(0xFF_FFFF);
            row.position.clear();
            if position != i32::MAX {
                let _ = write!(row.position, "{position}");
            }
            row.number.clear();
            let _ = write!(
                row.number,
                "#{}",
                info.and_then(|d| d.car_number.as_deref()).unwrap_or("?")
            );
            row.name
                .set(info.and_then(|d| d.user_name.as_deref()).unwrap_or(""));
            row.stop.clear();
            row.stood.clear();
            row.fuel.clear();
            if row.in_pit {
                row.stop.set("PIT");
            } else if let Some(stop) = stops.last_stop(i) {
                // The lap the car was on when it came in.
                let _ = write!(row.stop, "L{}", stop.lap.max(0) + 1);
            }
            if let Some(stop) = stops.last_stop(i) {
                let _ = write!(row.stood, "{:.1}", stop.stationary);
                let _ = write!(row.fuel, "{:.0}", model.fuel_for(stop.stationary));
            }
        }
    }

    fn row_height(&self) -> f32 {
        self.height as f32 / (self.rows.len() + HEAD_ROWS) as f32
    }

    /// Left edges of the stop, standing time, and fuel columns, from the right edge.
    fn columns(&self) -> (f32, f32, f32) {
        let w = self.width as f32;
        (w - 150.0, w - 102.0, w - 54.0)
    }

    fn draw_head(&self, canvas: &mut Canvas<'_>) {
        let row_h = self.row_height();
        let size = (row_h * 0.55).clamp(11.0, 15.0);
        let label = size * 0.85;
        let dim = Color::rgb(DIM, 1.0);
        let white = Color::rgb(WHITE, 1.0);
        let cell = |x: f32, width: f32, slot: f32| Rect::new(x, slot * row_h, width, row_h);
        let h = &self.head;
        canvas.text("Lane loss", cell(10.0, 62.0, 0.0), label, Align::Left, dim);
        canvas.text(
            h.loss.as_str(),
            cell(68.0, 36.0, 0.0),
            size,
            Align::Right,
            white,
        );
        canvas.text("s", cell(106.0, 10.0, 0.0), label, Align::Left, dim);
        canvas.text("Refuel", cell(124.0, 44.0, 0.0), label, Align::Left, dim);
        // Dim until measured at a stop of the player's: the default is from another car.
        let rate = if h.rate_measured { white } else { dim };
        canvas.text(
            h.rate.as_str(),
            cell(166.0, 32.0, 0.0),
            size,
            Align::Right,
            rate,
        );
        canvas.text("L/s", cell(200.0, 24.0, 0.0), label, Align::Left, dim);
        canvas.text("Full stop", cell(238.0, 60.0, 0.0), label, Align::Left, dim);
        canvas.text(
            h.full.as_str(),
            cell(296.0, 36.0, 0.0),
            size,
            Align::Right,
            white,
        );
        canvas.text("s", cell(334.0, 10.0, 0.0), label, Align::Left, dim);
        let (stop, stood, fuel) = self.columns();
        canvas.text("Stop", cell(stop, 44.0, 1.0), label, Align::Right, dim);
        canvas.text(
            "Stood s",
            cell(stood - 8.0, 52.0, 1.0),
            label,
            Align::Right,
            dim,
        );
        canvas.text(
            "Fuel \u{2264}L",
            cell(fuel - 10.0, 56.0, 1.0),
            label,
            Align::Right,
            dim,
        );
    }

    fn draw_row(&self, canvas: &mut Canvas<'_>, y: f32, row: &Row) {
        let (w, row_h) = (self.width as f32, self.row_height());
        let size = (row_h * 0.6).clamp(11.0, 16.0);
        let cell = |x: f32, width: f32| Rect::new(x, y, width, row_h);
        if row.player {
            canvas.fill(Rect::new(0.0, y, w, row_h), Color::rgb(0xFF_FFFF, 0.10));
        }
        let alpha = if row.in_pit { 0.55 } else { 1.0 };
        let base = if row.player { PLAYER } else { WHITE };
        let text = Color::rgb(base, alpha);
        canvas.text(
            row.position.as_str(),
            cell(6.0, 22.0),
            size,
            Align::Right,
            text,
        );
        canvas.text(
            row.number.as_str(),
            cell(32.0, 40.0),
            size,
            Align::Right,
            Color::rgb(row.class_color, alpha),
        );
        let (stop, stood, fuel) = self.columns();
        canvas.text(
            row.name.as_str(),
            cell(80.0, (stop - 84.0).max(0.0)),
            size,
            Align::Left,
            text,
        );
        let stop_color = if row.in_pit {
            Color::rgb(PIT, 1.0)
        } else {
            text
        };
        canvas.text(
            row.stop.as_str(),
            cell(stop, 44.0),
            size,
            Align::Right,
            stop_color,
        );
        canvas.text(
            row.stood.as_str(),
            cell(stood, 44.0),
            size,
            Align::Right,
            text,
        );
        let fuel_color = if row.other_model {
            Color::rgb(DIM, alpha)
        } else {
            Color::rgb(FUEL, alpha)
        };
        canvas.text(
            row.fuel.as_str(),
            cell(fuel, 46.0),
            size,
            Align::Right,
            fuel_color,
        );
    }
}

const WHITE: u32 = 0xF0_F2F5;
const DIM: u32 = 0xA0_A6AE;
const PLAYER: u32 = 0xFF_B347;
const PIT: u32 = 0x6C_9BFF;
const FUEL: u32 = 0x82_B1FF;
const BACKGROUND: u32 = 0x10_1418;

impl Widget for PitWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(100)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        self.tracker.observe(view);
        self.fill(view);
        // Slots: the head line, the column names (never change), then the cars. Only the changed span is drawn.
        let head = self.next_head != self.head;
        let first_row = self.rows.iter().zip(&self.next).position(|(a, b)| a != b);
        let last_row = self.rows.iter().zip(&self.next).rposition(|(a, b)| a != b);
        let first = if head {
            Some(0)
        } else {
            first_row.map(|r| r + HEAD_ROWS)
        };
        let last = last_row.map(|r| r + HEAD_ROWS).or(head.then_some(0));
        let (Some(first), Some(last)) = (first, last) else {
            return;
        };
        std::mem::swap(&mut self.rows, &mut self.next);
        std::mem::swap(&mut self.head, &mut self.next_head);
        let row_h = self.row_height();
        redraw[0] = Redraw::Area(Rect::new(
            0.0,
            first as f32 * row_h,
            self.width as f32,
            (last - first + 1) as f32 * row_h,
        ));
    }

    fn draw(&self, _layer: usize, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(BACKGROUND, self.opacity),
        );
        let row_h = self.row_height();
        let area = canvas.area();
        let visible = |slot: usize| {
            let y = slot as f32 * row_h;
            y + row_h > area.y && y < area.y + area.h
        };
        if visible(0) || visible(1) {
            self.draw_head(canvas);
        }
        for (n, row) in self.rows.iter().enumerate() {
            if row.used && visible(n + HEAD_ROWS) {
                self.draw_row(canvas, (n + HEAD_ROWS) as f32 * row_h, row);
            }
        }
    }

    fn reset(&mut self) {
        for row in self.rows.iter_mut().chain(self.next.iter_mut()) {
            row.used = false;
        }
        self.tracker.reset();
    }
}
