//! Pit widget: the cars of the player's class with their pit stops and the next one.
//!
//! For the last stop: the lap, the time standing still in the pit stall, and the most fuel that standing time allows
//! (an upper bound, since tire changes and repairs also take standing time). For the next stop, in a race: laps on
//! the current stint, when the car must pit compared with the player (laps earlier or later), and where it rejoins
//! after that stop. Above the cars, the pit lane loss measured at this track, the refueling speed, and what a stop
//! for a whole tank costs.
//!
//! The stops come from [`PitTracker`], which follows every car at this widget's rate. Other cars' fuel is not
//! published, so each car's stints are followed with the player's fuel per lap: a full tank at the race start, then
//! what was left at a stop plus the fuel its standing time allows.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use pitot_irsdk::flags::session_flags;

use crate::config::{NoOptions, WidgetConfig};
use crate::pipeline::View;
use crate::render::{Align, Canvas, Color, Rect};

use super::fuel::{FuelTracker, is_green, laps_to_go};
use super::pitstops::{PitModel, PitTracker, SharedPitTracker, Stop, player_index};
use super::text::FixedText;
use super::{Build, MAX_LAYERS, Redraw, Register, Widget};

/// Most car rows: a full grid.
const MAX_ROWS: usize = 64;
/// Rows above the cars: the lane loss and refueling speed, then the column names.
const HEAD_ROWS: usize = 2;
/// Row height the default size is laid out for.
const ROW_HEIGHT: u32 = 22;
/// The head line, the columns with room for a short name, and three cars.
const MIN_SIZE: (u32, u32) = (380, 110);

/// One car's current stint, followed with the player's fuel per lap.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Stint {
    /// Finished stops already taken into account.
    stops: u16,
    /// Laps completed when the stint started: 0 at the race start, else when the car left pit road.
    start_lap: i32,
    /// Fuel at the stint start, or `None` for the first stint, which starts with the opening fuel.
    start_fuel: Option<f32>,
}

impl Stint {
    /// Starts the next stint after `stop`: the fuel left when the car came in, plus `added`, within the tank.
    pub(crate) fn after_stop(&mut self, stop: &Stop, added: f32, per_lap: f32, fuel: Fuel) {
        let used = (stop.lap - self.start_lap).max(0) as f32 * per_lap;
        let left = (self.start_fuel.unwrap_or(fuel.opening) - used).max(0.0);
        self.start_fuel = Some((left + added).min(fuel.tank));
        self.start_lap = stop.left_lap;
    }

    /// Fuel left after `laps` completed laps.
    pub(crate) fn fuel_left(&self, laps: i32, per_lap: f32, opening: f32) -> f32 {
        self.start_fuel.unwrap_or(opening) - (laps - self.start_lap).max(0) as f32 * per_lap
    }
}

/// The fuel stints start from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Fuel {
    /// The usable tank.
    pub(crate) tank: f32,
    /// Fuel at the start line of the first lap.
    pub(crate) opening: f32,
}

/// Completed laps at which a car with `fuel` left at `distance` (completed laps plus lap distance) runs dry: it must
/// pit at the end of that lap.
pub(crate) fn pit_lap(distance: f32, fuel: f32, per_lap: f32) -> i32 {
    (distance + fuel / per_lap).floor() as i32
}

/// Where a car `gap` seconds ahead of the player (negative: behind) rejoins after losing `loss` seconds, among
/// cars at `others` (their gaps to the player, the player at 0): its class position and its new gap.
pub(crate) fn rejoin(gap: f32, loss: f32, others: impl Iterator<Item = f32>) -> (usize, f32) {
    let after = gap - loss;
    (1 + others.filter(|&g| g > after).count(), after)
}

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
    stint: FixedText,
    stop: FixedText,
    stood: FixedText,
    fuel: FixedText,
    /// Laps until the car must pit, minus the player's (`+2`: two laps later); the player's own pit lap.
    pit: FixedText,
    rejoin_place: FixedText,
    rejoin_gap: FixedText,
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
            stint: FixedText::new(8),
            stop: FixedText::new(8),
            stood: FixedText::new(8),
            fuel: FixedText::new(8),
            pit: FixedText::new(8),
            rejoin_place: FixedText::new(4),
            rejoin_gap: FixedText::new(8),
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

/// Left edges of the columns right of the name.
struct Columns {
    stint: f32,
    stop: f32,
    stood: f32,
    fuel: f32,
    pit: f32,
    place: f32,
    gap: f32,
}

/// What the next stops are estimated from, for one update.
struct Race {
    per_lap: f32,
    fuel: Fuel,
    /// Laps the player still has to drive.
    to_go: f32,
    /// Laps until the player must pit.
    player_laps: i32,
    player_f2: f32,
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
    /// Shared with the track map, which draws where the player would rejoin.
    tracker: SharedPitTracker,
    /// The player's fuel per lap.
    fuel: FuelTracker,
    /// The player's fuel at the start line of the first lap, worked back from the current fuel until the player's
    /// first stop. Other cars are assumed to have started with the same (same car, same pace laps).
    opening: Option<f32>,
    /// Per car: the current stint.
    stints: Vec<Stint>,
    /// (class position, car index) of the player's class this update; capacity grows to the car count once.
    cars: Vec<(i32, usize)>,
    /// (car index, race gap to the player) of the cars in `cars`, the player at 0.
    gaps: Vec<(usize, f32)>,
}

impl PitWidget {
    /// A pit widget following the stops with `tracker`, which other widgets may share.
    pub(crate) fn new(config: &WidgetConfig, tracker: SharedPitTracker) -> Self {
        let mut widget = Self {
            width: 0,
            height: 0,
            opacity: config.opacity,
            rows: Vec::new(),
            next: Vec::new(),
            head: Head::new(),
            next_head: Head::new(),
            tracker,
            fuel: FuelTracker::default(),
            opening: None,
            stints: Vec::with_capacity(72),
            cars: Vec::with_capacity(72),
            gaps: Vec::with_capacity(72),
        };
        widget.resize(config.width.unwrap_or(400), config.height.unwrap_or(280));
        widget
    }

    /// A pit widget with its own tracker, keeping what it learns in `store`, or only in memory when `None`.
    pub fn with_store(config: &WidgetConfig, store: Option<PathBuf>) -> Self {
        Self::new(config, PitTracker::shared(store))
    }

    /// Brings every car's stint up to date with its stops.
    fn follow(&mut self, view: &View, per_lap: Option<f32>, fuel: Option<Fuel>) {
        let tracker = self.tracker.borrow();
        let model = tracker.model();
        let stops = tracker.stops();
        let count = view.snapshot().count(view.vars().car_lap_dist_pct);
        if self.stints.len() != count {
            self.stints.clear();
            self.stints.resize(count, Stint::default());
        }
        for (car, stint) in self.stints.iter_mut().enumerate() {
            let n = stops.stops(car);
            if n < stint.stops {
                // The stops were forgotten (a new session): start over.
                *stint = Stint::default();
            }
            if n == stint.stops {
                continue;
            }
            stint.stops = n;
            let Some(stop) = stops.last_stop(car) else {
                continue;
            };
            let added = model.fuel_for(stop.stationary);
            match (per_lap, fuel) {
                (Some(per_lap), Some(fuel)) => stint.after_stop(&stop, added, per_lap, fuel),
                (None, Some(fuel)) => {
                    // The fuel use is not known yet: count the stint from what the stop added alone.
                    stint.start_lap = stop.left_lap;
                    stint.start_fuel = Some(added.min(fuel.tank));
                }
                (_, None) => {
                    stint.start_lap = stop.left_lap;
                    stint.start_fuel = None;
                }
            }
        }
    }

    fn fill(&mut self, view: &View) {
        for row in &mut self.next {
            row.used = false;
        }
        let model = self.tracker.borrow().model();
        self.next_head.fill(&model);
        let (snap, vars, session) = (view.snapshot(), view.vars(), view.session());

        // The player's fuel use, as the fuel widget counts it.
        let fuel_now = snap.f32(vars.fuel_level, 0);
        if let Some(fuel) = fuel_now {
            let laps = snap.i32(vars.lap_completed, 0).unwrap_or(0);
            let on_pit = snap.bool(vars.on_pit_road, 0).unwrap_or(false);
            let flags = snap.bitfield(vars.session_flags, 0).unwrap_or(0);
            let green = !on_pit && is_green(snap.i32(vars.session_state, 0), flags);
            self.fuel.observe(laps, fuel, green);
        }
        let per_lap = self.fuel.per_lap().filter(|&p| p > 0.0);
        let player = player_index(snap, vars, session);
        let laps_of = |i: usize| snap.i32(vars.car_lap_completed, i).unwrap_or(-1);
        let distance = player.map_or(0.0, |p| {
            laps_of(p).max(0) as f32 + snap.f32(vars.lap_dist_pct, 0).unwrap_or(0.0)
        });
        if let (Some(p), Some(fuel), Some(per_lap)) = (player, fuel_now, per_lap)
            && self.tracker.borrow().stops().stops(p) == 0
        {
            self.opening = Some(fuel + distance * per_lap);
        }
        let fuel = model.tank.map(|tank| Fuel {
            tank,
            opening: self.opening.map_or(tank, |o| o.min(tank)),
        });
        self.follow(view, per_lap, fuel);

        let Some(player) = player else {
            return;
        };
        let session_num = snap.i32(vars.session_num, 0).map(i64::from);
        let is_race = session
            .and_then(|s| s.sessions.iter().find(|x| x.num == session_num))
            .and_then(|s| s.session_type.as_deref())
            .is_some_and(|t| t.eq_ignore_ascii_case("race"));
        let checkered = snap
            .bitfield(vars.session_flags, 0)
            .is_some_and(|f| f & session_flags::CHECKERED != 0);
        let lap_time = snap
            .f32(vars.lap_last_time, 0)
            .filter(|&t| t > 0.0)
            .or_else(|| session?.driver_info.est_lap_time.map(|t| t as f32));
        let to_go = laps_to_go(
            snap.i32(vars.session_laps_remain, 0),
            snap.f64(vars.session_time_remain, 0),
            lap_time,
        );
        let race = match (is_race && !checkered, per_lap, fuel, to_go, fuel_now) {
            (true, Some(per_lap), Some(start), Some(to_go), Some(fuel)) => Some(Race {
                per_lap,
                fuel: start,
                to_go,
                player_laps: pit_lap(distance, fuel, per_lap) - laps_of(player).max(0),
                player_f2: snap.f32(vars.car_f2_time, player).unwrap_or(0.0),
            }),
            _ => None,
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
        self.gaps.clear();
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
                if let Some(race) = &race {
                    // Race time behind the leader: the player's minus the car's is how far the car is ahead.
                    let f2 = snap.f32(vars.car_f2_time, i).unwrap_or(0.0);
                    self.gaps.push((i, race.player_f2 - f2));
                }
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
        let tracker = self.tracker.borrow();
        let stops = tracker.stops();
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
            for text in [
                &mut row.stint,
                &mut row.stop,
                &mut row.stood,
                &mut row.fuel,
                &mut row.pit,
                &mut row.rejoin_place,
                &mut row.rejoin_gap,
            ] {
                text.clear();
            }
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
            let stint = self.stints.get(i).copied().unwrap_or_default();
            let laps = laps_of(i);
            if laps >= 0 && !row.in_pit {
                let _ = write!(row.stint, "{}", (laps - stint.start_lap).max(0));
            }
            let Some(race) = &race else {
                continue;
            };
            if row.in_pit || laps < 0 {
                continue;
            }
            // Laps until the car must pit. The player's own fuel is known; other cars follow their stint.
            let (laps_left, fuel_left) = if row.player {
                let fuel = fuel_now.unwrap_or(0.0);
                (race.player_laps, fuel)
            } else {
                let fuel = stint.fuel_left(laps, race.per_lap, race.fuel.opening);
                (pit_lap(laps as f32, fuel, race.per_lap) - laps, fuel)
            };
            if fuel_left >= race.to_go * race.per_lap {
                // The fuel lasts to the end: no stop to predict.
                continue;
            }
            if row.player {
                let _ = write!(row.pit, "L{}", laps.max(0) + race.player_laps.max(0));
            } else {
                let _ = write!(row.pit, "{:+}", laps_left - race.player_laps);
            }
            // At that stop: fuel for the rest of the race, within the tank, less what is still in it.
            let left_then = (fuel_left - laps_left.max(0) as f32 * race.per_lap).max(0.0);
            let after = (race.to_go - laps_left.max(0) as f32).max(0.0);
            let needed = (after * race.per_lap).min(race.fuel.tank) - left_then;
            let Some(loss) = model.expected_loss(needed.max(0.0)) else {
                continue;
            };
            let gap = if row.player {
                0.0
            } else {
                race.player_f2 - snap.f32(vars.car_f2_time, i).unwrap_or(0.0)
            };
            let others = self.gaps.iter().filter(|&&(c, _)| c != i).map(|&(_, g)| g);
            let (place, after) = rejoin(gap, loss, others);
            let _ = write!(row.rejoin_place, "{place}");
            let _ = write!(row.rejoin_gap, "{after:+.1}");
        }
    }

    fn row_height(&self) -> f32 {
        self.height as f32 / (self.rows.len() + HEAD_ROWS) as f32
    }

    fn columns(&self) -> Columns {
        let gap = self.width as f32 - 50.0;
        let place = gap - 24.0;
        let pit = place - 30.0;
        let fuel = pit - 32.0;
        let stood = fuel - 38.0;
        let stop = stood - 38.0;
        let stint = stop - 30.0;
        Columns {
            stint,
            stop,
            stood,
            fuel,
            pit,
            place,
            gap,
        }
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
        let c = self.columns();
        for (x, width, name) in [
            (c.stint, 28.0, "Stint"),
            (c.stop, 36.0, "Stop"),
            (c.stood, 36.0, "Stood"),
            (c.fuel, 30.0, "Fuel"),
            (c.pit, 28.0, "Pit"),
            (c.place, 70.0, "Rejoin"),
        ] {
            canvas.text(name, cell(x, width, 1.0), label, Align::Right, dim);
        }
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
        let dim = Color::rgb(DIM, alpha);
        canvas.text(
            row.position.as_str(),
            cell(4.0, 20.0),
            size,
            Align::Right,
            text,
        );
        canvas.text(
            row.number.as_str(),
            cell(26.0, 38.0),
            size,
            Align::Right,
            Color::rgb(row.class_color, alpha),
        );
        let c = self.columns();
        canvas.text(
            row.name.as_str(),
            cell(70.0, (c.stint - 72.0).max(0.0)),
            size,
            Align::Left,
            text,
        );
        let stop_color = if row.in_pit {
            Color::rgb(PIT, 1.0)
        } else {
            text
        };
        let fuel_color = if row.other_model {
            dim
        } else {
            Color::rgb(FUEL, alpha)
        };
        for (x, width, value, color) in [
            (c.stint, 28.0, &row.stint, dim),
            (c.stop, 36.0, &row.stop, stop_color),
            (c.stood, 36.0, &row.stood, text),
            (c.fuel, 30.0, &row.fuel, fuel_color),
            (c.pit, 28.0, &row.pit, text),
            (c.place, 22.0, &row.rejoin_place, text),
            (c.gap, 46.0, &row.rejoin_gap, text),
        ] {
            canvas.text(value.as_str(), cell(x, width), size, Align::Right, color);
        }
    }
}

const WHITE: u32 = 0xF0_F2F5;
const DIM: u32 = 0xA0_A6AE;
const PLAYER: u32 = 0xFF_B347;
const PIT: u32 = 0x6C_9BFF;
const FUEL: u32 = 0x82_B1FF;
const BACKGROUND: u32 = 0x10_1418;

impl Register for PitWidget {
    type Vars = ();
    type Options = NoOptions;

    fn build(cx: &Build<'_>, _: ()) -> Self {
        Self::new(cx.config, cx.shared.pit())
    }
}

impl Widget for PitWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn min_size(&self) -> (u32, u32) {
        MIN_SIZE
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(MIN_SIZE.0);
        self.height = height.max(MIN_SIZE.1);
        let rows = ((self.height / ROW_HEIGHT) as usize)
            .saturating_sub(HEAD_ROWS)
            .clamp(1, MAX_ROWS);
        for list in [&mut self.rows, &mut self.next] {
            list.truncate(rows);
            list.resize_with(rows, Row::new);
            // Filled again at the next update.
            list.iter_mut().for_each(|row| row.used = false);
        }
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(100)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        self.tracker.borrow_mut().observe(view);
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
        self.tracker.borrow_mut().reset();
        self.fuel = FuelTracker::default();
        self.opening = None;
        self.stints.iter_mut().for_each(|s| *s = Stint::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(lap: i32, left_lap: i32, stationary: f32) -> Stop {
        Stop {
            lap,
            left_lap,
            left_at: 0.0,
            lane_time: 25.0,
            stationary,
            loss: None,
        }
    }

    #[test]
    fn stints_follow_stops() {
        // 40 L tank, 2.5 L per lap: 16 laps on a full tank.
        let fuel = Fuel {
            tank: 40.0,
            opening: 40.0,
        };
        let mut stint = Stint::default();
        assert_eq!(pit_lap(0.0, stint.fuel_left(0, 2.5, fuel.opening), 2.5), 16);
        assert_eq!(stint.fuel_left(10, 2.5, fuel.opening), 15.0);
        // In at the end of lap 14 (14 laps done, 5 L left), 20 L added, out on lap 15.
        stint.after_stop(&stop(14, 15, 5.0), 20.0, 2.5, fuel);
        assert_eq!(stint.start_lap, 15);
        assert_eq!(stint.start_fuel, Some(25.0));
        // 25 L last 10 laps: must pit when 25 laps are done.
        assert_eq!(
            pit_lap(15.0, stint.fuel_left(15, 2.5, fuel.opening), 2.5),
            25
        );
        // A full fill is capped at the tank.
        stint.after_stop(&stop(20, 21, 30.0), 300.0, 2.5, fuel);
        assert_eq!(stint.start_fuel, Some(40.0));
        // A first stint starts with the opening fuel (the pace laps took some of the tank).
        let first = Stint::default();
        assert_eq!(first.fuel_left(4, 2.5, 37.0), 27.0);
    }

    #[test]
    fn the_player_pits_by_the_lap_the_fuel_reaches() {
        // Lap 10 half done with 5.75 L at 2.5 L per lap: dry at 12.8 laps, so in at the end of lap 12.
        assert_eq!(pit_lap(10.5, 5.75, 2.5), 12);
        assert_eq!(pit_lap(10.5, 0.5, 2.5), 10, "not enough to finish this lap");
    }

    #[test]
    fn rejoin_places_among_the_class() {
        // A car 10 s ahead of the player loses 25 s: it rejoins 15 s behind the player, behind a car 5 s back.
        let others = [30.0f32, 0.0, -5.0, -20.0];
        let (place, gap) = rejoin(10.0, 25.0, others.iter().copied());
        assert_eq!((place, gap), (4, -15.0));
        // The player, losing 25 s, rejoins behind the car 20 s back: fifth.
        let others = [30.0f32, 10.0, -5.0, -20.0];
        assert_eq!(rejoin(0.0, 25.0, others.iter().copied()), (5, -25.0));
    }
}
