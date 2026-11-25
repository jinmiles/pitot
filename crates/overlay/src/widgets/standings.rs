//! Standings: the session's order grouped by class, fastest class first.
//!
//! Positions and lap times come from the session's results (`ResultsPositions`), so they include laps driven
//! before the player joined; telemetry adds what the results do not have yet (a lap just completed) and what they
//! never have (tires, sector times). In a race, from the green flag, the order and the gaps are live: the results
//! only change at the line, so the order follows each car's laps and lap distance, and the gaps are timed by a
//! [`RaceClock`] shared with the relative. Each class shows a header and its leaders; the player's class also shows
//! the cars around the player.

use std::fmt::Write as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use pitot_irsdk::flags::session_state;
use pitot_irsdk::session::SessionInfo;

use crate::config::{NoOptions, WidgetConfig};
use crate::pipeline::View;
use crate::render::{Align, Canvas, Color, Rect};

use super::classes::{ClassLabel, class_label};
use super::palette::{
    BEST, COMPOUND_HARD, COMPOUND_INTER, COMPOUND_MEDIUM, COMPOUND_SOFT, COMPOUND_WET, DIM, GAIN, LOSS, PLAYER,
    PLAYER_BAND, RAISED, SURFACE, TEXT, UNKNOWN_CLASS, UNKNOWN_LICENSE,
};
use super::text::FixedText;
use super::timing::{Gap, RaceClock, SectorClock, SectorStarts};
use super::trackclock::{SharedTrackClock, TrackClock};
use super::{Build, MAX_LAYERS, Redraw, Register, Widget};

const ROW_HEIGHT: f32 = 24.0;
const TEXT_SIZE: f32 = 13.0;
/// Leaders shown per class, and cars on each side of the player in the player's class, before the rows are filled
/// (see [`select_rows`]).
const LEADERS: usize = 3;
const AROUND: usize = 3;
/// The fixed columns with a short name, and a header with three cars.
const MIN_SIZE: (u32, u32) = (400, 96);
const MAX_CLASSES: usize = 8;
/// Sector columns drawn at most.
const MAX_SECTOR_COLUMNS: usize = 6;
/// `CarIdxTrackSurface` value for a car that is not in the world.
const NOT_IN_WORLD: i32 = -1;

/// `1600 / ln 2`, the scale of iRating differences in the rating model.
const RATING_SCALE: f32 = 2308.312;

/// Strength of field: `RATING_SCALE * ln(n / sum(exp(-r / RATING_SCALE)))`.
pub(crate) fn strength_of_field(ratings: impl Iterator<Item = f32>) -> Option<f32> {
    let (mut n, mut sum) = (0u32, 0.0f32);
    for r in ratings.filter(|r| *r > 0.0) {
        n += 1;
        sum += (-r / RATING_SCALE).exp();
    }
    (n > 0).then(|| RATING_SCALE * (n as f32 / sum).ln())
}

/// Estimated iRating change for every car of a class if the race ended in `positions` (1-based), with the
/// community's model of the sim's formula: cars actually beaten minus cars expected to be beaten, less a
/// small factor for the position, scaled by `200 / n`. An estimate; the sim's own numbers can differ.
pub(crate) fn irating_changes(ratings: &[f32], positions: &[u32], out: &mut [f32]) {
    let n = ratings.len().min(positions.len()).min(out.len());
    if n == 0 {
        return;
    }
    let e = |r: f32| (-r / RATING_SCALE).exp();
    for i in 0..n {
        let ei = e(ratings[i]);
        // Probability of beating each car, including itself (0.5).
        let expected: f32 = (0..n)
            .map(|j| {
                let ej = e(ratings[j]);
                ((1.0 - ei) * ej) / ((1.0 - ej) * ei + (1.0 - ei) * ej)
            })
            .sum::<f32>()
            - 0.5;
        let place = positions[i] as f32;
        let fudge = (n as f32 / 2.0 - place) / 100.0;
        out[i] = (n as f32 - place - expected - fudge) * 200.0 / n as f32;
    }
}

/// `D 2.83` as `D2.8`: the license class and the safety rating to one decimal, cut, not rounded.
pub(crate) fn short_license(license: &str, out: &mut FixedText) {
    out.clear();
    let mut parts = license.split_whitespace();
    let (Some(class), rating) = (parts.next(), parts.next()) else {
        return;
    };
    out.push(class);
    if let Some(sr) = rating.and_then(|r| r.parse::<f32>().ok()) {
        let tenths = (sr * 10.0 + 1e-3).floor() as i32;
        let _ = write!(out, "{}.{}", tenths / 10, tenths % 10);
    }
}

/// The first letter of the car's name as a capital, standing for its maker (`BMW M4 GT3` is B).
pub(crate) fn maker_letter(car_name: &str) -> Option<char> {
    car_name
        .chars()
        .find(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_uppercase())
}

/// A duration as `h:mm:ss`, or `m:ss` under an hour.
pub(crate) fn write_clock(out: &mut FixedText, seconds: f64) {
    let s = seconds.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        let _ = write!(out, "{h}:{m:02}:{s:02}");
    } else {
        let _ = write!(out, "{m}:{s:02}");
    }
}

/// A session length as `2h`, `22h13m`, or `45m`.
pub(crate) fn write_length(out: &mut FixedText, seconds: f64) {
    let minutes = (seconds.max(0.0) / 60.0).round() as u64;
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, m) => {
            let _ = write!(out, "{m}m");
        }
        (h, 0) => {
            let _ = write!(out, "{h}h");
        }
        (h, m) => {
            let _ = write!(out, "{h}h{m}m");
        }
    }
}

/// `m:ss.sss`, or nothing for a missing time.
pub(crate) fn write_lap_time(out: &mut FixedText, seconds: f32) {
    out.clear();
    if seconds > 0.0 {
        // Round to milliseconds first, so 59.9996 s shows as 1:00.000, not 0:60.000.
        let millis = (f64::from(seconds) * 1000.0).round() as u64;
        let (minutes, rest) = (millis / 60_000, millis % 60_000);
        let _ = write!(out, "{minutes}:{:02}.{:03}", rest / 1000, rest % 1000);
    }
}

/// A gap in seconds: tenths below a minute, then `m:ss`.
fn write_gap(out: &mut FixedText, seconds: f32) {
    out.clear();
    if seconds < 60.0 {
        let _ = write!(out, "{seconds:.1}");
    } else {
        let s = seconds as u32;
        let _ = write!(out, "{}:{:02}", s / 60, s % 60);
    }
}

/// A gap as seconds, or `+1L` for whole laps. F2Time is not maintained in every session state, so gaps that read
/// as zero are left empty.
fn write_race_gap(out: &mut FixedText, gap: Gap) {
    out.clear();
    match gap {
        Gap::Laps(laps) => {
            let _ = write!(out, "+{laps}L");
        }
        Gap::Time(seconds) if seconds >= 0.05 => write_gap(out, seconds as f32),
        Gap::Time(_) => {}
    }
}

/// A sector time difference without its sign: tenths below ten seconds, whole seconds above.
fn write_delta(out: &mut FixedText, seconds: f32) {
    out.clear();
    let d = seconds.abs();
    if d < 9.95 {
        let _ = write!(out, "{d:.1}");
    } else {
        let _ = write!(out, "{d:.0}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LapMark {
    None,
    /// The car's own best lap.
    Personal,
    /// The best lap in the class.
    Class,
}

#[derive(Debug, Clone, PartialEq)]
struct HeaderRow {
    color: u32,
    /// The session's letter and the session-wide values; only on the first header.
    first: bool,
    session: FixedText,
    name: FixedText,
    sof: FixedText,
    cars: FixedText,
    laps: FixedText,
    air: FixedText,
    track: FixedText,
    time: FixedText,
}

impl HeaderRow {
    fn new() -> Self {
        Self {
            color: 0,
            first: false,
            session: FixedText::new(2),
            name: FixedText::new(24),
            sof: FixedText::new(12),
            cars: FixedText::new(8),
            laps: FixedText::new(16),
            air: FixedText::new(16),
            track: FixedText::new(16),
            time: FixedText::new(24),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct CarRow {
    player: bool,
    leader: bool,
    /// A line above: the rows before are the class leaders, these are the cars around the player.
    break_before: bool,
    color: u32,
    position: FixedText,
    /// Places gained (positive) or lost since the start; 0 hides it.
    change: i32,
    change_text: FixedText,
    number: FixedText,
    name: FixedText,
    license: FixedText,
    license_color: u32,
    rating: FixedText,
    rating_change: i32,
    rating_change_text: FixedText,
    maker: FixedText,
    /// Tire color, or 0 for none.
    tire: u32,
    gap: FixedText,
    interval: FixedText,
    last_lap: FixedText,
    mark: LapMark,
    sectors: [FixedText; MAX_SECTOR_COLUMNS],
    /// Per sector: positive when the car was slower than the player, negative when faster, 0 for none.
    sector_sign: [i8; MAX_SECTOR_COLUMNS],
}

impl CarRow {
    fn new() -> Self {
        Self {
            player: false,
            leader: false,
            break_before: false,
            color: 0,
            position: FixedText::new(4),
            change: 0,
            change_text: FixedText::new(4),
            number: FixedText::new(8),
            name: FixedText::new(64),
            license: FixedText::new(8),
            license_color: 0,
            rating: FixedText::new(8),
            rating_change: 0,
            rating_change_text: FixedText::new(8),
            maker: FixedText::new(4),
            tire: 0,
            gap: FixedText::new(12),
            interval: FixedText::new(12),
            last_lap: FixedText::new(12),
            mark: LapMark::None,
            sectors: std::array::from_fn(|_| FixedText::new(8)),
            sector_sign: [0; MAX_SECTOR_COLUMNS],
        }
    }

    /// Equal but for the gap and interval, which in a race change on every update.
    fn same_but_gaps(&self, other: &Self) -> bool {
        let Self {
            player,
            leader,
            break_before,
            color,
            position,
            change,
            change_text,
            number,
            name,
            license,
            license_color,
            rating,
            rating_change,
            rating_change_text,
            maker,
            tire,
            gap: _,
            interval: _,
            last_lap,
            mark,
            sectors,
            sector_sign,
        } = self;
        *player == other.player
            && *leader == other.leader
            && *break_before == other.break_before
            && *color == other.color
            && *position == other.position
            && *change == other.change
            && *change_text == other.change_text
            && *number == other.number
            && *name == other.name
            && *license == other.license
            && *license_color == other.license_color
            && *rating == other.rating
            && *rating_change == other.rating_change
            && *rating_change_text == other.rating_change_text
            && *maker == other.maker
            && *tire == other.tire
            && *last_lap == other.last_lap
            && *mark == other.mark
            && *sectors == other.sectors
            && *sector_sign == other.sector_sign
    }
}

// Rows live in fixed vectors and move between them and the spare pools by value, so boxing the variants would
// only add a pointer chase.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
enum Row {
    Empty,
    Header(HeaderRow),
    Car(CarRow),
}

/// One car of the session, as sorted.
#[derive(Debug, Clone, Copy)]
struct Entry {
    class_rank: usize,
    /// Class position, 1-based; `i32::MAX` without one.
    position: i32,
    car: usize,
    fastest: f32,
    last: f32,
    laps: i32,
    f2: f32,
    rating: f32,
    rating_change: f32,
    /// Live race order, compared descending: progress, then the earlier finish.
    key: (f64, f64),
}

/// Values of one class, collected while sorting.
#[derive(Debug, Clone, Copy, Default)]
struct Class {
    id: i64,
    rel_speed: i64,
    best: f32,
    entries: u32,
    in_world: u32,
}

/// Tire compound colors by `TireCompoundType`.
pub(crate) fn tire_color(compound: &str) -> u32 {
    // Case-insensitive search without lowercasing into a new string.
    let has = |word: &str| {
        compound
            .as_bytes()
            .windows(word.len())
            .any(|w| w.eq_ignore_ascii_case(word.as_bytes()))
    };
    if has("wet") || has("rain") {
        COMPOUND_WET
    } else if has("inter") {
        COMPOUND_INTER
    } else if has("soft") {
        COMPOUND_SOFT
    } else if has("medium") {
        COMPOUND_MEDIUM
    } else {
        COMPOUND_HARD
    }
}

/// The standings widget.
#[derive(Debug)]
pub struct StandingsWidget {
    width: u32,
    height: u32,
    opacity: f32,
    rows: Vec<Row>,
    next: Vec<Row>,
    /// Spare rows, reused so updates do not allocate.
    spare_cars: Vec<CarRow>,
    spare_headers: Vec<HeaderRow>,
    entries: Vec<Entry>,
    /// Index into the current session's results, per car.
    result_of: Vec<Option<usize>>,
    clocks: Vec<SectorClock>,
    sectors: SectorStarts,
    /// Live race order and gaps, shared with the relative.
    clock: SharedTrackClock,
    /// Class position at the start of the race, per car; 0 when unknown.
    start_positions: Vec<i32>,
    start_session: Option<i64>,
    ratings: Vec<f32>,
    places: Vec<u32>,
    changes: Vec<f32>,
}

impl StandingsWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        Self::with_clock(config, TrackClock::shared())
    }

    /// A standings widget timing gaps with `clock`, which other widgets may observe too.
    pub(crate) fn with_clock(config: &WidgetConfig, clock: SharedTrackClock) -> Self {
        let mut widget = Self {
            width: 0,
            height: 0,
            opacity: config.opacity,
            rows: Vec::new(),
            next: Vec::new(),
            spare_cars: Vec::new(),
            spare_headers: Vec::new(),
            entries: Vec::with_capacity(72),
            result_of: Vec::with_capacity(72),
            clocks: Vec::with_capacity(72),
            sectors: SectorStarts::default(),
            clock,
            start_positions: Vec::with_capacity(72),
            start_session: None,
            ratings: Vec::with_capacity(72),
            places: Vec::with_capacity(72),
            changes: Vec::with_capacity(72),
        };
        widget.resize(config.width.unwrap_or(760), config.height.unwrap_or(560));
        widget
    }

    /// (car number, gap) of the car rows last filled.
    #[cfg(test)]
    pub(crate) fn filled_gaps(&self) -> Vec<(String, String)> {
        self.next
            .iter()
            .filter_map(|r| match r {
                Row::Car(car) => {
                    Some((car.number.as_str().to_owned(), car.gap.as_str().to_owned()))
                }
                _ => None,
            })
            .collect()
    }

    /// Moves rows from `next` back to the spare pools and blanks every row.
    fn recycle(&mut self) {
        for row in &mut self.next {
            match std::mem::replace(row, Row::Empty) {
                Row::Car(car) => self.spare_cars.push(car),
                Row::Header(header) => self.spare_headers.push(header),
                Row::Empty => {}
            }
        }
    }

    pub(crate) fn fill(&mut self, view: &View) {
        self.recycle();
        let (snap, vars) = (view.snapshot(), view.vars());
        let Some(info) = view.session() else {
            return;
        };
        let cars = view.car_count().max(info.driver_info.drivers.len());
        if self.clocks.len() != cars {
            self.clocks.clear();
            self.clocks.resize(cars, SectorClock::default());
            self.start_positions.clear();
            self.start_positions.resize(cars, 0);
            self.result_of.clear();
            self.result_of.resize(cars, None);
            self.start_session = None;
        }
        let player = snap
            .i32(vars.player_car_idx, 0)
            .and_then(|i| usize::try_from(i).ok())
            .or(info.driver_info.player_car_idx);
        let session_num = snap.i32(vars.session_num, 0).map(i64::from);
        let session = info
            .sessions
            .iter()
            .find(|s| s.num == session_num)
            .or(info.sessions.last());
        let is_race = session
            .and_then(|s| s.session_type.as_deref())
            .is_some_and(|t| t.eq_ignore_ascii_case("race"));
        let now = view.tick_time().unwrap_or(0.0);
        let state = snap.i32(vars.session_state, 0);

        // Every car's progress, kept from before the green flag (and in every session, for the relative) so the
        // first gaps are ready at the start. Only a race uses it here.
        if let Ok(mut clock) = self.clock.try_borrow_mut() {
            clock.observe(view);
        }
        let shared = Rc::clone(&self.clock);
        let Ok(clock) = shared.try_borrow() else {
            return;
        };
        let race = clock.clock();
        let live = is_race
            && match state {
                Some(session_state::RACING) => true,
                Some(session_state::CHECKERED | session_state::COOL_DOWN) => race.is_live(),
                _ => false,
            };

        // Sector timing for every car.
        self.sectors.load(info);
        let starts = self.sectors.as_slice();
        for (car, clock) in self.clocks.iter_mut().enumerate() {
            let pct = snap.f32(vars.car_lap_dist_pct, car).unwrap_or(-1.0);
            let in_world = snap.i32(vars.car_track_surface, car) != Some(NOT_IN_WORLD);
            let pit = snap.bool(vars.car_on_pit_road, car).unwrap_or(false);
            // A hair below zero at the line is on the lap; -1 means not in the world.
            clock.observe(starts, pct, now, in_world && !pit && pct >= -0.01);
        }

        // Classes present, fastest first.
        let mut classes = [Class::default(); MAX_CLASSES];
        let mut class_count = 0;
        for driver in info.driver_info.drivers.iter().flatten() {
            if driver.is_pace_car || driver.is_spectator {
                continue;
            }
            let id = driver.car_class_id.unwrap_or(0);
            if !classes[..class_count].iter().any(|c| c.id == id) && class_count < MAX_CLASSES {
                classes[class_count] = Class {
                    id,
                    rel_speed: driver.car_class_rel_speed.unwrap_or(0),
                    best: f32::MAX,
                    ..Class::default()
                };
                class_count += 1;
            }
        }
        classes[..class_count].sort_unstable_by_key(|c| std::cmp::Reverse(c.rel_speed));

        // Results of the current session, by car.
        self.result_of.iter_mut().for_each(|r| *r = None);
        if let Some(session) = session {
            for (i, result) in session.results.iter().enumerate() {
                if let Some(slot) = result.car_idx.and_then(|c| self.result_of.get_mut(c)) {
                    *slot = Some(i);
                }
            }
        }

        self.entries.clear();
        for (car, driver) in info.driver_info.drivers.iter().enumerate() {
            let Some(driver) = driver else { continue };
            if driver.is_pace_car || driver.is_spectator {
                continue;
            }
            let id = driver.car_class_id.unwrap_or(0);
            let class_rank = classes[..class_count]
                .iter()
                .position(|c| c.id == id)
                .unwrap_or(MAX_CLASSES);
            let result = self
                .result_of
                .get(car)
                .copied()
                .flatten()
                .and_then(|i| session?.results.get(i));
            let position = result
                .and_then(|r| r.class_position)
                .map(|p| p as i32 + 1)
                .or_else(|| snap.i32(vars.car_class_position, car).filter(|&p| p > 0))
                .unwrap_or(i32::MAX);
            let positive = |t: Option<f32>| t.filter(|&t| t > 0.0);
            let fastest = match (
                positive(result.and_then(|r| r.fastest_time).map(|t| t as f32)),
                positive(snap.f32(vars.car_best_lap_time, car)),
            ) {
                (Some(a), Some(b)) => a.min(b),
                (a, b) => a.or(b).unwrap_or(0.0),
            };
            let last = positive(snap.f32(vars.car_last_lap_time, car))
                .or_else(|| positive(result.and_then(|r| r.last_time).map(|t| t as f32)))
                .unwrap_or(0.0);
            let result_laps = result.and_then(|r| r.laps_complete).map(|l| l as i32);
            let live_laps = snap.i32(vars.car_lap_completed, car);
            // In a race the telemetry's count is newer; elsewhere the results include laps from before joining.
            let laps = match (result_laps, live_laps) {
                (Some(a), Some(b)) if is_race => a.max(b),
                (a, b) => a.or(b).unwrap_or(0),
            };
            let key = if live {
                race.standing(car)
                    .map(|s| s.key())
                    .or_else(|| result_laps.map(|l| (f64::from(l), f64::NEG_INFINITY)))
                    .unwrap_or((f64::NEG_INFINITY, f64::NEG_INFINITY))
            } else {
                (f64::NEG_INFINITY, f64::NEG_INFINITY)
            };
            let in_world = snap.i32(vars.car_track_surface, car) != Some(NOT_IN_WORLD);
            if let Some(class) = classes.get_mut(class_rank) {
                class.entries += 1;
                class.in_world += u32::from(in_world);
                if fastest > 0.0 {
                    class.best = class.best.min(fastest);
                }
            }
            self.entries.push(Entry {
                class_rank,
                position,
                car,
                fastest,
                last,
                laps,
                f2: snap.f32(vars.car_f2_time, car).unwrap_or(0.0),
                rating: driver.irating.unwrap_or(0) as f32,
                rating_change: 0.0,
                key,
            });
        }
        // Live, furthest along first and finishers by their finish; otherwise (all keys equal) and for ties, the
        // results' order.
        self.entries.sort_unstable_by(|a, b| {
            a.class_rank
                .cmp(&b.class_rank)
                .then(b.key.0.total_cmp(&a.key.0))
                .then(b.key.1.total_cmp(&a.key.1))
                .then(a.position.cmp(&b.position))
                .then(a.car.cmp(&b.car))
        });
        if live {
            let mut class = usize::MAX;
            let mut place = 0;
            for e in &mut self.entries {
                if e.class_rank != class {
                    class = e.class_rank;
                    place = 0;
                }
                if e.key.0 > f64::NEG_INFINITY {
                    place += 1;
                    e.position = place;
                } else {
                    e.position = i32::MAX;
                }
            }
        }

        // Start positions of a race: the qualifying grid, else the order first seen under green.
        if is_race {
            if self.start_session != session_num {
                self.start_session = session_num;
                self.start_positions.iter_mut().for_each(|p| *p = 0);
                for q in &info.qualify_results {
                    if let (Some(car), Some(pos)) = (q.car_idx, q.class_position)
                        && let Some(slot) = self.start_positions.get_mut(car)
                    {
                        *slot = pos as i32 + 1;
                    }
                }
            }
            if state == Some(session_state::RACING) {
                for e in &self.entries {
                    if let Some(slot) = self.start_positions.get_mut(e.car)
                        && *slot == 0
                        && e.position != i32::MAX
                    {
                        *slot = e.position;
                    }
                }
            }
        }

        let mut next_row = 0;
        let rows = self.next.len();
        let shown = select_rows(&self.entries, player, rows);
        let mut start = 0;
        let mut first_header = true;
        while start < self.entries.len() && next_row < rows {
            let rank = self.entries[start].class_rank;
            let end = start
                + self.entries[start..]
                    .iter()
                    .take_while(|e| e.class_rank == rank)
                    .count();
            let class = classes.get(rank).copied().unwrap_or_default();
            if shown.player_class_only && !self.entries[start..end].iter().any(|e| Some(e.car) == player) {
                start = end;
                continue;
            }

            // Estimated iRating changes, for a race with positions.
            if is_race {
                self.ratings.clear();
                self.places.clear();
                for e in &self.entries[start..end] {
                    if e.position != i32::MAX && e.rating > 0.0 {
                        self.ratings.push(e.rating);
                        self.places.push(e.position as u32);
                    }
                }
                // Places within the cars that have one, so gaps in the numbering do not count.
                self.changes.clear();
                self.changes.resize(self.ratings.len(), 0.0);
                for (n, place) in self.places.iter_mut().enumerate() {
                    *place = n as u32 + 1;
                }
                irating_changes(&self.ratings, &self.places, &mut self.changes);
                let mut k = 0;
                for e in &mut self.entries[start..end] {
                    if e.position != i32::MAX && e.rating > 0.0 {
                        e.rating_change = self.changes[k];
                        k += 1;
                    }
                }
            }

            let leader = self.entries[start];
            let mut header = self.spare_headers.pop().unwrap_or_else(HeaderRow::new);
            write_header(
                &mut header,
                info,
                view,
                session,
                &self.entries[start..end],
                class,
                first_header,
            );
            first_header = false;
            self.next[next_row] = Row::Header(header);
            next_row += 1;

            let player_index =
                player.and_then(|p| self.entries[start..end].iter().position(|e| e.car == p));
            let around = player_index.map(|p| (p.saturating_sub(shown.around), p + shown.around));
            let mut previous: Option<usize> = None;
            for offset in 0..end - start {
                if next_row >= rows {
                    break;
                }
                let shown =
                    offset < shown.leaders || around.is_some_and(|(a, b)| offset >= a && offset <= b);
                if !shown {
                    continue;
                }
                let entry = self.entries[start + offset];
                let ahead = (offset > 0).then(|| self.entries[start + offset - 1]);
                let mut row = self.spare_cars.pop().unwrap_or_else(CarRow::new);
                let context = RowContext {
                    info,
                    view,
                    is_race,
                    race: live.then_some(race),
                    leader,
                    ahead,
                    class_best: class.best,
                    player,
                    clocks: &self.clocks,
                    sector_count: self.sectors.len(),
                    start_position: self.start_positions.get(entry.car).copied().unwrap_or(0),
                };
                write_car(&mut row, &entry, &context);
                row.break_before = previous.is_some_and(|p| offset > p + 1);
                previous = Some(offset);
                self.next[next_row] = Row::Car(row);
                next_row += 1;
            }
            start = end;
        }
    }
}

fn write_header(
    header: &mut HeaderRow,
    info: &SessionInfo,
    view: &View,
    session: Option<&pitot_irsdk::session::Session>,
    class_entries: &[Entry],
    class: Class,
    first: bool,
) {
    let (snap, vars) = (view.snapshot(), view.vars());
    let leader = class_entries.first();
    let driver = leader.and_then(|l| info.driver_info.drivers.get(l.car)?.as_ref());
    header.color = driver.and_then(|d| d.car_class_color).unwrap_or(UNKNOWN_CLASS);
    header.first = first;
    header.name.clear();
    let label = driver.map_or(ClassLabel::Number(Some(class.id)), |d| {
        class_label(&info.driver_info.drivers, d)
    });
    let _ = write!(header.name, "{label}");
    header.sof.clear();
    if let Some(sof) = strength_of_field(class_entries.iter().map(|e| e.rating)) {
        let _ = write!(header.sof, "SOF {:.1}k", sof / 1000.0);
    }
    header.cars.clear();
    let _ = write!(header.cars, "{}/{}", class.in_world, class.entries);

    // Laps: the class leader's against the session's, or an estimate from the time left in a timed session.
    header.laps.clear();
    let done = class_entries
        .iter()
        .map(|e| e.laps)
        .max()
        .unwrap_or(0)
        .max(0);
    let elapsed = snap.f64(vars.session_time, 0).unwrap_or(0.0);
    let remaining = snap
        .f64(vars.session_time_remain, 0)
        .filter(|&t| (0.0..1.0e6).contains(&t));
    match session.and_then(|s| s.laps).filter(|&l| l > 0) {
        Some(total) => {
            let _ = write!(header.laps, "{done}/{total}");
        }
        None => {
            if let (Some(remaining), true) = (remaining, done > 0) {
                let per_lap = elapsed / f64::from(done);
                let _ = write!(
                    header.laps,
                    "{done}/\u{2248}{:.0}",
                    f64::from(done) + remaining / per_lap
                );
            } else if done > 0 {
                let _ = write!(header.laps, "{done}");
            }
        }
    }

    header.session.clear();
    header.air.clear();
    header.track.clear();
    header.time.clear();
    if !first {
        return;
    }
    let letter = session
        .and_then(|s| s.session_type.as_deref())
        .and_then(|t| t.chars().next())
        .unwrap_or('S');
    let mut buf = [0u8; 4];
    header.session.set(letter.encode_utf8(&mut buf));
    if let Some(air) = snap.f32(vars.air_temp, 0) {
        let _ = write!(header.air, "Air {air:.1}\u{b0}C");
    }
    if let Some(track) = snap.f32(vars.track_temp, 0) {
        let _ = write!(header.track, "Track {track:.1}\u{b0}C");
    }
    write_clock(&mut header.time, elapsed);
    let total = session
        .and_then(|s| s.time_seconds)
        .filter(|&t| t > 0.0 && t < 1.0e6);
    match (total, session.and_then(|s| s.laps).filter(|&l| l > 0)) {
        (Some(total), _) => {
            header.time.push("/");
            write_length(&mut header.time, total);
        }
        (None, Some(laps)) if done > 0 => {
            header.time.push("/\u{2248}");
            write_length(&mut header.time, elapsed / f64::from(done) * laps as f64);
        }
        _ => {}
    }
}

struct RowContext<'a> {
    info: &'a SessionInfo,
    view: &'a View,
    is_race: bool,
    /// Live race timing, from the green flag.
    race: Option<&'a RaceClock>,
    leader: Entry,
    ahead: Option<Entry>,
    class_best: f32,
    player: Option<usize>,
    clocks: &'a [SectorClock],
    sector_count: usize,
    start_position: i32,
}

fn write_car(row: &mut CarRow, entry: &Entry, cx: &RowContext<'_>) {
    let (snap, vars) = (cx.view.snapshot(), cx.view.vars());
    let car = entry.car;
    let driver = cx
        .info
        .driver_info
        .drivers
        .get(car)
        .and_then(Option::as_ref);
    row.player = Some(car) == cx.player;
    row.leader = car == cx.leader.car;
    row.color = driver.and_then(|d| d.car_class_color).unwrap_or(UNKNOWN_CLASS);
    row.position.clear();
    if entry.position != i32::MAX {
        let _ = write!(row.position, "{}", entry.position);
    }
    row.change = 0;
    row.change_text.clear();
    if cx.is_race && cx.start_position > 0 && entry.position != i32::MAX {
        row.change = cx.start_position - entry.position;
        if row.change != 0 {
            let _ = write!(row.change_text, "{}", row.change.abs());
        }
    }
    row.number.clear();
    let _ = write!(
        row.number,
        "{}",
        driver.and_then(|d| d.car_number.as_deref()).unwrap_or("?")
    );
    row.name
        .set(driver.and_then(|d| d.user_name.as_deref()).unwrap_or(""));
    short_license(
        driver.and_then(|d| d.license.as_deref()).unwrap_or(""),
        &mut row.license,
    );
    row.license_color = driver.and_then(|d| d.license_color).unwrap_or(UNKNOWN_LICENSE);
    row.rating.clear();
    if entry.rating > 0.0 {
        let _ = write!(row.rating, "{:.1}k", entry.rating / 1000.0);
    }
    row.rating_change = entry.rating_change.round() as i32;
    row.rating_change_text.clear();
    if cx.is_race && row.rating_change != 0 {
        let _ = write!(row.rating_change_text, "{}", row.rating_change.abs());
    } else {
        row.rating_change = 0;
    }
    row.maker.clear();
    if let Some(letter) = driver
        .and_then(|d| {
            d.car_screen_name
                .as_deref()
                .or(d.car_screen_name_short.as_deref())
        })
        .and_then(maker_letter)
    {
        let mut buf = [0u8; 4];
        row.maker.set(letter.encode_utf8(&mut buf));
    }
    row.tire = snap
        .i32(vars.car_tire_compound, car)
        .filter(|&t| t >= 0)
        .and_then(|t| {
            cx.info
                .driver_info
                .tires
                .iter()
                .find(|tire| tire.index == Some(i64::from(t)))
        })
        .and_then(|tire| tire.compound.as_deref())
        .map_or(0, tire_color);

    // Gap to the class leader and interval to the car ahead: in a race the live time gaps (or `CarIdxF2Time`, which
    // changes only at the line, until a gap is timed), best laps otherwise.
    row.gap.clear();
    row.interval.clear();
    if !row.leader {
        let timed = |ahead: &Entry| cx.race.and_then(|r| r.gap(ahead.car, car));
        let (gap, interval) = if cx.is_race {
            let gap = timed(&cx.leader).unwrap_or(Gap::Time(f64::from(entry.f2 - cx.leader.f2)));
            let interval = cx.ahead.map_or(Gap::Time(0.0), |a| {
                timed(&a).unwrap_or(Gap::Time(f64::from(entry.f2 - a.f2)))
            });
            (gap, interval)
        } else if entry.fastest > 0.0 {
            let gap = entry.fastest - cx.leader.fastest;
            let interval = cx
                .ahead
                .filter(|a| a.fastest > 0.0)
                .map_or(0.0, |a| entry.fastest - a.fastest);
            (Gap::Time(f64::from(gap)), Gap::Time(f64::from(interval)))
        } else {
            (Gap::Time(0.0), Gap::Time(0.0))
        };
        write_race_gap(&mut row.gap, gap);
        write_race_gap(&mut row.interval, interval);
    }

    write_lap_time(&mut row.last_lap, entry.last);
    row.mark = if entry.last <= 0.0 {
        LapMark::None
    } else if (entry.last - cx.class_best).abs() < 0.0005 {
        LapMark::Class
    } else if (entry.last - entry.fastest).abs() < 0.0005 {
        LapMark::Personal
    } else {
        LapMark::None
    };

    // Sector differences against the player's last time in each sector.
    let player_clock = cx.player.and_then(|p| cx.clocks.get(p));
    let clock = cx.clocks.get(car);
    for s in 0..MAX_SECTOR_COLUMNS {
        row.sectors[s].clear();
        row.sector_sign[s] = 0;
        if row.player || s >= cx.sector_count {
            continue;
        }
        let (Some(mine), Some(theirs)) = (player_clock, clock) else {
            continue;
        };
        let (a, b) = (mine.last[s], theirs.last[s]);
        if a > 0.0 && b > 0.0 {
            let d = b - a;
            write_delta(&mut row.sectors[s], d);
            row.sector_sign[s] = if d > 0.0 { 1 } else { -1 };
        }
    }
}

/// Where each column goes for a given width. Optional columns are left out, least important first, when the
/// widget is too narrow.
#[derive(Debug, Clone, Copy)]
struct Columns {
    position: f32,
    change: Option<f32>,
    number: f32,
    name: (f32, f32),
    license: f32,
    rating: f32,
    rating_change: Option<f32>,
    maker: Option<f32>,
    tire: Option<f32>,
    gap: f32,
    interval: Option<f32>,
    last: f32,
    sectors: (f32, usize),
}

const SECTOR_WIDTH: f32 = 34.0;
/// Room for the last lap, `1:45.345` at the text size with a margin before it.
const LAST_WIDTH: f32 = 68.0;

impl Columns {
    fn for_width(w: f32, sectors: usize) -> Self {
        const NAME_MIN: f32 = 110.0;
        // Fixed columns: position 24, number 36, license 40, rating 40, gap 46, the last lap, margins 20.
        let fixed = 24.0 + 36.0 + 40.0 + 40.0 + 46.0 + LAST_WIDTH + 20.0;
        let mut spare = (w - fixed - NAME_MIN).max(0.0);
        let mut take = |width: f32| {
            if spare >= width {
                spare -= width;
                true
            } else {
                false
            }
        };
        let tire = take(16.0);
        let maker = take(16.0);
        let change = take(34.0);
        let interval = take(46.0);
        let rating_change = take(40.0);
        let mut sector_columns = 0;
        while sector_columns < sectors.min(MAX_SECTOR_COLUMNS) && take(SECTOR_WIDTH) {
            sector_columns += 1;
        }
        // Lay out from the right edge.
        let mut x = w - 6.0;
        let sectors_x = x - SECTOR_WIDTH * sector_columns as f32;
        x = sectors_x;
        let last = x - LAST_WIDTH;
        x = last;
        let interval_x = interval.then(|| {
            x -= 46.0;
            x
        });
        let gap = x - 46.0;
        x = gap;
        let tire_x = tire.then(|| {
            x -= 16.0;
            x
        });
        let maker_x = maker.then(|| {
            x -= 16.0;
            x
        });
        let rating_change_x = rating_change.then(|| {
            x -= 40.0;
            x
        });
        let rating = x - 40.0;
        x = rating;
        let license = x - 42.0;
        x = license;
        // From the left edge.
        let position = 8.0;
        let mut left = position + 24.0;
        let change_x = change.then(|| {
            let at = left;
            left += 34.0;
            at
        });
        let number = left;
        left += 36.0;
        Self {
            position,
            change: change_x,
            number,
            name: (left + 4.0, (x - left - 8.0).max(0.0)),
            license,
            rating,
            rating_change: rating_change_x,
            maker: maker_x,
            tire: tire_x,
            gap,
            interval: interval_x,
            last,
            sectors: (sectors_x, sector_columns),
        }
    }
}

/// Draws text only when its cell reaches into the area being drawn, which is the whole widget: a cell outside it
/// would only be clipped.
fn text_in(canvas: &mut Canvas<'_>, text: &str, rect: Rect, size: f32, align: Align, color: Color) {
    let area = canvas.area();
    if rect.x < area.x + area.w && rect.x + rect.w > area.x {
        canvas.text(text, rect, size, align, color);
    }
}

/// The header's class name: its place, and the room kept clear before the next item.
const NAME_WIDTH: f32 = 96.0;
const NAME_GAP: f32 = 6.0;
/// Longest text, in bytes, [`text_cut`] shortens; longer text is cut to this first.
const MAX_CUT: usize = 64;

const ELLIPSIS: &str = "\u{2026}";

/// The first `chars` characters of `text` (at most [`MAX_CUT`] bytes) without trailing spaces, then an ellipsis,
/// written into `buf`.
fn shortened<'a>(text: &str, chars: usize, buf: &'a mut [u8; MAX_CUT + ELLIPSIS.len()]) -> &'a str {
    let end = text
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(i, _)| i)
        .min(MAX_CUT);
    let end = (0..=end)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    let prefix = text[..end].trim_end();
    let len = prefix.len() + ELLIPSIS.len();
    buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
    buf[prefix.len()..len].copy_from_slice(ELLIPSIS.as_bytes());
    std::str::from_utf8(&buf[..len]).unwrap_or(ELLIPSIS)
}

/// Draws `text` left-aligned in `rect`, or when it is wider, as much of it as fits followed by an ellipsis.
fn text_cut(canvas: &mut Canvas<'_>, text: &str, rect: Rect, color: Color) {
    if canvas.fits(text, rect, TEXT_SIZE) {
        text_in(canvas, text, rect, TEXT_SIZE, Align::Left, color);
        return;
    }
    let mut buf = [0u8; MAX_CUT + ELLIPSIS.len()];
    // The most characters that fit, found by halving: a few measurements, each kept by the text cache.
    let (mut fits, mut too_many) = (0, text.chars().count());
    while too_many - fits > 1 {
        let mid = (fits + too_many) / 2;
        if canvas.fits(shortened(text, mid, &mut buf), rect, TEXT_SIZE) {
            fits = mid;
        } else {
            too_many = mid;
        }
    }
    if fits > 0 {
        let cut = shortened(text, fits, &mut buf);
        text_in(canvas, cut, rect, TEXT_SIZE, Align::Left, color);
    }
}

/// Cars a class of `cars` shows: its first `leaders`, and with the player at `player` in it, the cars within
/// `around` places of the player.
fn shown_in_class(cars: usize, player: Option<usize>, leaders: usize, around: usize) -> usize {
    let lead = leaders.min(cars);
    let Some(p) = player.filter(|&p| p < cars) else {
        return lead;
    };
    let (from, to) = (p.saturating_sub(around), (p + around).min(cars - 1));
    let near = to + 1 - from;
    // The cars near the player that are not leaders already.
    lead + near - lead.saturating_sub(from).min(near)
}

/// Which cars the rows show: `leaders` per class, `around` cars on each side of the player in the player's class, and
/// with `player_class_only`, no other class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    leaders: usize,
    around: usize,
    player_class_only: bool,
}

/// Rows the classes of `entries` (sorted by class) take with `shown`: a header each, and the cars
/// [`shown_in_class`] shows.
fn rows_needed(entries: &[Entry], player: Option<usize>, shown: Selection) -> usize {
    let mut rows = 0;
    let mut start = 0;
    while start < entries.len() {
        let rank = entries[start].class_rank;
        let cars = entries[start..]
            .iter()
            .take_while(|e| e.class_rank == rank)
            .count();
        let player_at = player.and_then(|p| entries[start..start + cars].iter().position(|e| e.car == p));
        if player_at.is_some() || !shown.player_class_only {
            rows += 1 + shown_in_class(cars, player_at, shown.leaders, shown.around);
        }
        start += cars;
    }
    rows
}

/// The cars that fill `rows`. Three leaders per class and three cars on each side of the player when they fit; with
/// rows to spare, more cars around the player, then more leaders, until the rows or the cars run out. In a short
/// widget, fewer cars around the player first, then fewer leaders down to one, and when even that does not fit,
/// only the player's class, so the player stays in view.
fn select_rows(entries: &[Entry], player: Option<usize>, rows: usize) -> Selection {
    let fits = |shown: Selection| rows_needed(entries, player, shown) <= rows;
    let has_player = player.is_some_and(|p| entries.iter().any(|e| e.car == p));
    let mut shown = Selection {
        leaders: 1,
        around: 0,
        player_class_only: false,
    };
    shown.player_class_only = has_player && !fits(shown);
    (shown.leaders, shown.around) = (LEADERS, AROUND);
    while shown.around > 0 && !fits(shown) {
        shown.around -= 1;
    }
    while shown.leaders > 1 && !fits(shown) {
        shown.leaders -= 1;
    }
    // More than the largest class can show adds nothing.
    let most = entries.len();
    while shown.around < most && fits(Selection { around: shown.around + 1, ..shown }) {
        shown.around += 1;
    }
    while shown.leaders < most && fits(Selection { leaders: shown.leaders + 1, ..shown }) {
        shown.leaders += 1;
    }
    shown
}

/// Rows up to and including the last one in use.
fn used_rows(rows: &[Row]) -> usize {
    rows.iter()
        .rposition(|row| !matches!(row, Row::Empty))
        .map_or(0, |last| last + 1)
}

impl StandingsWidget {
    fn draw_header(&self, canvas: &mut Canvas<'_>, y: f32, header: &HeaderRow) {
        let w = self.width as f32;
        canvas.fill_background(
            Rect::new(0.0, y + 1.0, w, ROW_HEIGHT - 2.0),
            Color::rgb(RAISED, self.opacity.max(0.6)),
        );
        canvas.fill(
            Rect::new(0.0, y + 1.0, 4.0, ROW_HEIGHT - 2.0),
            Color::rgb(header.color, 1.0),
        );
        let white = Color::rgb(TEXT, 1.0);
        let dim = Color::rgb(DIM, 1.0);
        let mut x = 10.0;
        let mut segment = |canvas: &mut Canvas<'_>, text: &str, width: f32, color: Color| {
            if !text.is_empty() && x + width <= w {
                text_in(
                    canvas,
                    text,
                    Rect::new(x, y, width, ROW_HEIGHT),
                    TEXT_SIZE,
                    Align::Left,
                    color,
                );
                x += width;
            }
        };
        if header.first {
            segment(
                canvas,
                header.session.as_str(),
                18.0,
                Color::rgb(header.color, 1.0),
            );
        }
        // A long class name ends in an ellipsis a little before the next item instead of running into it.
        if x + NAME_WIDTH <= w {
            text_cut(
                canvas,
                header.name.as_str(),
                Rect::new(x, y, NAME_WIDTH - NAME_GAP, ROW_HEIGHT),
                Color::rgb(header.color, 1.0),
            );
            x += NAME_WIDTH;
        }
        let mut segment = |canvas: &mut Canvas<'_>, text: &str, width: f32, color: Color| {
            if !text.is_empty() && x + width <= w {
                text_in(
                    canvas,
                    text,
                    Rect::new(x, y, width, ROW_HEIGHT),
                    TEXT_SIZE,
                    Align::Left,
                    color,
                );
                x += width;
            }
        };
        segment(canvas, header.sof.as_str(), 74.0, white);
        segment(canvas, header.cars.as_str(), 52.0, dim);
        segment(canvas, header.laps.as_str(), 84.0, white);
        if header.first {
            // Room for one decimal and a minus sign (`Air -12.4°C`) before the next item.
            segment(canvas, header.air.as_str(), 84.0, dim);
            segment(canvas, header.track.as_str(), 98.0, dim);
            segment(canvas, header.time.as_str(), 140.0, white);
        }
    }

    fn draw_car(&self, canvas: &mut Canvas<'_>, y: f32, car: &CarRow, cols: &Columns) {
        let w = self.width as f32;
        if car.player {
            canvas.fill(
                Rect::new(0.0, y, w, ROW_HEIGHT),
                Color::rgb(PLAYER, PLAYER_BAND),
            );
        }
        if car.break_before {
            canvas.fill(Rect::new(0.0, y, w, 1.0), Color::rgb(DIM, 0.5));
        }
        let text = Color::rgb(TEXT, 1.0);
        let muted = Color::rgb(DIM, 1.0);
        // The player's position and name in the player's color; their times keep the colors of their meaning.
        let own = if car.player {
            Color::rgb(PLAYER, 1.0)
        } else {
            text
        };
        let cell = |x: f32, width: f32| Rect::new(x, y, width, ROW_HEIGHT);
        canvas.fill(
            Rect::new(0.0, y + 2.0, 4.0, ROW_HEIGHT - 4.0),
            Color::rgb(car.color, 1.0),
        );
        text_in(
            canvas,
            car.position.as_str(),
            cell(cols.position, 22.0),
            TEXT_SIZE,
            Align::Right,
            own,
        );
        if let Some(x) = cols.change
            && car.change != 0
        {
            let (arrow, color) = if car.change > 0 {
                ("\u{25B2}", GAIN)
            } else {
                ("\u{25BC}", LOSS)
            };
            text_in(
                canvas,
                arrow,
                cell(x + 2.0, 12.0),
                10.0,
                Align::Center,
                Color::rgb(color, 1.0),
            );
            text_in(
                canvas,
                car.change_text.as_str(),
                cell(x + 14.0, 18.0),
                TEXT_SIZE,
                Align::Left,
                Color::rgb(color, 1.0),
            );
        }
        text_in(
            canvas,
            car.number.as_str(),
            cell(cols.number, 32.0),
            TEXT_SIZE,
            Align::Right,
            muted,
        );
        text_in(
            canvas,
            car.name.as_str(),
            cell(cols.name.0, cols.name.1),
            TEXT_SIZE,
            Align::Left,
            own,
        );
        if !car.license.as_str().is_empty() {
            canvas.fill(
                Rect::new(cols.license, y + 4.0, 38.0, ROW_HEIGHT - 8.0),
                Color::rgb(car.license_color, 0.85),
            );
            text_in(
                canvas,
                car.license.as_str(),
                cell(cols.license, 38.0),
                12.0,
                Align::Center,
                Color::rgb(SURFACE, 1.0),
            );
        }
        text_in(
            canvas,
            car.rating.as_str(),
            cell(cols.rating, 38.0),
            TEXT_SIZE,
            Align::Right,
            muted,
        );
        if let Some(x) = cols.rating_change
            && car.rating_change != 0
        {
            let (arrow, color) = if car.rating_change > 0 {
                ("\u{25B2}", GAIN)
            } else {
                ("\u{25BC}", LOSS)
            };
            text_in(
                canvas,
                arrow,
                cell(x + 4.0, 10.0),
                9.0,
                Align::Center,
                Color::rgb(color, 1.0),
            );
            text_in(
                canvas,
                car.rating_change_text.as_str(),
                cell(x + 14.0, 26.0),
                TEXT_SIZE,
                Align::Left,
                Color::rgb(color, 1.0),
            );
        }
        if let Some(x) = cols.maker {
            text_in(
                canvas,
                car.maker.as_str(),
                cell(x, 16.0),
                TEXT_SIZE,
                Align::Center,
                muted,
            );
        }
        if let Some(x) = cols.tire
            && car.tire != 0
        {
            canvas.circle(
                (x + 8.0, y + ROW_HEIGHT / 2.0),
                5.0,
                2.5,
                Color::rgb(car.tire, 1.0),
            );
        }
        if car.leader {
            text_in(
                canvas,
                "GAP",
                cell(cols.gap, 44.0),
                11.0,
                Align::Right,
                muted,
            );
            if let Some(x) = cols.interval {
                text_in(canvas, "INT", cell(x, 44.0), 11.0, Align::Right, muted);
            }
        } else {
            text_in(
                canvas,
                car.gap.as_str(),
                cell(cols.gap, 44.0),
                TEXT_SIZE,
                Align::Right,
                text,
            );
            if let Some(x) = cols.interval {
                text_in(
                    canvas,
                    car.interval.as_str(),
                    cell(x, 44.0),
                    TEXT_SIZE,
                    Align::Right,
                    text,
                );
            }
        }
        text_in(
            canvas,
            car.last_lap.as_str(),
            cell(cols.last, LAST_WIDTH - 4.0),
            TEXT_SIZE,
            Align::Right,
            text,
        );
        let underline = match car.mark {
            LapMark::Class => Some(BEST),
            LapMark::Personal => Some(GAIN),
            LapMark::None => None,
        };
        if let Some(color) = underline {
            let base = y + ROW_HEIGHT - 4.0;
            canvas.line(
                (cols.last + 10.0, base),
                (cols.last + LAST_WIDTH - 4.0, base),
                2.0,
                Color::rgb(color, 1.0),
            );
        }
        let (sx, count) = cols.sectors;
        for s in 0..count {
            let color = match car.sector_sign[s] {
                1 => GAIN,
                -1 => LOSS,
                _ => DIM,
            };
            text_in(
                canvas,
                car.sectors[s].as_str(),
                cell(sx + s as f32 * SECTOR_WIDTH, SECTOR_WIDTH - 4.0),
                TEXT_SIZE,
                Align::Right,
                Color::rgb(color, 1.0),
            );
        }
    }
}

impl Register for StandingsWidget {
    type Vars = ();
    type Options = NoOptions;

    fn build(cx: &Build<'_>, _: ()) -> Self {
        Self::with_clock(cx.config, cx.shared.clock())
    }
}

impl Widget for StandingsWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn min_size(&self) -> (u32, u32) {
        MIN_SIZE
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(MIN_SIZE.0);
        self.height = height.max(MIN_SIZE.1);
        let rows = (self.height as f32 / ROW_HEIGHT) as usize;
        // Every row back to the pools; the next update fills the new rows.
        self.recycle();
        std::mem::swap(&mut self.rows, &mut self.next);
        self.recycle();
        for list in [&mut self.rows, &mut self.next] {
            list.truncate(rows);
            list.resize_with(rows, || Row::Empty);
        }
        // Rows for a full widget, and the most two filled lists hold, so the pools neither run dry nor keep the
        // rows of a larger size.
        let headers = MAX_CLASSES.min(rows);
        self.spare_cars.truncate(2 * rows);
        self.spare_headers.truncate(2 * headers);
        while self.spare_cars.len() < rows {
            self.spare_cars.push(CarRow::new());
        }
        while self.spare_headers.len() < headers {
            self.spare_headers.push(HeaderRow::new());
        }
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(100)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        self.fill(view);
        // Only the rows that changed are drawn again: whole rows, or just the gap columns of rows whose gaps alone
        // changed (in a race, every row on every update). The header's session clock changes every second.
        // First and last row of each kind of change.
        let mut whole: Option<(usize, usize)> = None;
        let mut gaps: Option<(usize, usize)> = None;
        for (n, (old, new)) in self.rows.iter().zip(&self.next).enumerate() {
            if old == new {
                continue;
            }
            let range = match (old, new) {
                (Row::Car(a), Row::Car(b)) if a.same_but_gaps(b) => &mut gaps,
                _ => &mut whole,
            };
            *range = Some(range.map_or((n, n), |(first, _)| (first, n)));
        }
        if whole.is_none() && gaps.is_none() {
            return;
        }
        std::mem::swap(&mut self.rows, &mut self.next);
        let w = self.width as f32;
        let cols = Columns::for_width(w, self.sectors.len());
        let rows = |(first, last): (usize, usize), x: f32, width: f32| {
            Rect::new(
                x,
                first as f32 * ROW_HEIGHT,
                width,
                (last - first + 1) as f32 * ROW_HEIGHT,
            )
        };
        let gap_columns = |range| rows(range, cols.gap, cols.last - cols.gap);
        redraw[0] = match (whole, gaps) {
            (Some(a), Some(g)) if g.0 >= a.0 && g.1 <= a.1 => Redraw::Area(rows(a, 0.0, w)),
            (Some(a), Some(g)) => Redraw::Areas(rows(a, 0.0, w), gap_columns(g)),
            (Some(a), None) => Redraw::Area(rows(a, 0.0, w)),
            (None, Some(g)) => Redraw::Area(gap_columns(g)),
            (None, None) => Redraw::Nothing,
        };
    }

    fn draw(&self, _layer: usize, canvas: &mut Canvas<'_>) {
        let w = self.width as f32;
        // The background ends at the last row in use; below it the widget stays transparent.
        canvas.fill_background(
            Rect::new(0.0, 0.0, w, used_rows(&self.rows) as f32 * ROW_HEIGHT),
            Color::rgb(SURFACE, self.opacity),
        );
        let cols = Columns::for_width(w, self.sectors.len());
        let area = canvas.area();
        for (n, row) in self.rows.iter().enumerate() {
            let y = n as f32 * ROW_HEIGHT;
            if y + ROW_HEIGHT <= area.y || y >= area.y + area.h {
                continue;
            }
            match row {
                Row::Empty => {}
                Row::Header(header) => self.draw_header(canvas, y, header),
                Row::Car(car) => self.draw_car(canvas, y, car, &cols),
            }
        }
    }

    fn reset(&mut self) {
        self.recycle();
        std::mem::swap(&mut self.rows, &mut self.next);
        self.recycle();
        self.clocks
            .iter_mut()
            .for_each(|c| *c = SectorClock::default());
        if let Ok(mut clock) = self.clock.try_borrow_mut() {
            clock.reset();
        }
        self.start_session = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_fill_with_cars_around_the_player_then_leaders() {
        // Two classes: 5 cars, then 20 with the player 13th.
        let entry = |class_rank, car| Entry {
            class_rank,
            position: 1,
            car,
            fastest: 0.0,
            last: 0.0,
            laps: 0,
            f2: 0.0,
            rating: 0.0,
            rating_change: 0.0,
            key: (0.0, 0.0),
        };
        let entries: Vec<Entry> = (0..25).map(|car| entry(usize::from(car >= 5), car)).collect();
        let player = Some(17);
        let pick = |rows| {
            let s = select_rows(&entries, player, rows);
            (s.leaders, s.around, s.player_class_only)
        };
        // Headers 2, leaders 3 + 3, the player and 3 on each side: 15 rows.
        assert_eq!(pick(15), (3, 3, false));
        assert_eq!(rows_needed(&entries, player, select_rows(&entries, player, 15)), 15);
        // More rows: more cars around the player, then more leaders.
        assert_eq!(pick(17), (3, 4, false));
        let roomy = select_rows(&entries, player, 23);
        assert_eq!((roomy.leaders, roomy.around), (3, 7));
        assert_eq!(rows_needed(&entries, player, roomy), 23, "{roomy:?}");
        // Every car: the leaders take what is left once the player's class is shown.
        let all = select_rows(&entries, player, 40);
        assert_eq!(rows_needed(&entries, player, all), 27, "{all:?}");
        // Fewer rows: fewer around the player, then fewer leaders, then only the player's class.
        assert_eq!(pick(11), (3, 1, false));
        assert_eq!(pick(6), (1, 0, false));
        assert_eq!(pick(5), (1, 0, false));
        assert_eq!(pick(4), (2, 0, true));
        assert_eq!(pick(3), (1, 0, true));
        // Without a player, the leaders fill the rows.
        assert_eq!(select_rows(&entries, None, 12).leaders, 5);
    }

    #[test]
    fn the_background_ends_at_the_last_row_in_use() {
        let mut rows = vec![Row::Empty; 5];
        assert_eq!(used_rows(&rows), 0);
        rows[2] = Row::Header(HeaderRow::new());
        assert_eq!(used_rows(&rows), 3);
        rows[0] = Row::Header(HeaderRow::new());
        assert_eq!(used_rows(&rows), 3);
    }

    #[test]
    fn lap_times_are_minutes_and_seconds() {
        let mut out = FixedText::new(12);
        write_lap_time(&mut out, 105.345);
        assert_eq!(out.as_str(), "1:45.345");
        write_lap_time(&mut out, 59.9996);
        assert_eq!(out.as_str(), "1:00.000");
        write_lap_time(&mut out, 61.05);
        assert_eq!(out.as_str(), "1:01.050");
        write_lap_time(&mut out, 605.1234);
        assert_eq!(out.as_str(), "10:05.123");
        write_lap_time(&mut out, -1.0);
        assert_eq!(out.as_str(), "");
    }

    #[test]
    fn long_names_are_shortened_with_an_ellipsis() {
        let mut buf = [0u8; MAX_CUT + ELLIPSIS.len()];
        assert_eq!(shortened("Dallara P217 LMP2", 7, &mut buf), "Dallara\u{2026}");
        assert_eq!(
            shortened("Dallara P217 LMP2", 8, &mut buf),
            "Dallara\u{2026}",
            "no space before the ellipsis"
        );
        assert_eq!(shortened("Ü-Wagen", 1, &mut buf), "Ü\u{2026}", "whole characters");
        assert_eq!(shortened("GT3", 0, &mut buf), "\u{2026}");
        let long = "x".repeat(100);
        assert_eq!(shortened(&long, 90, &mut buf).len(), MAX_CUT + ELLIPSIS.len());
    }

    #[test]
    fn the_default_width_keeps_every_column() {
        let cols = Columns::for_width(760.0, 6);
        assert_eq!(cols.sectors.1, 6);
        assert!(cols.interval.is_some() && cols.rating_change.is_some());
        assert!(cols.name.1 >= 110.0, "{:?}", cols.name);
    }

    #[test]
    fn strength_of_field_of_equal_ratings_is_that_rating() {
        let sof = strength_of_field([2000.0, 2000.0, 2000.0].into_iter()).expect("sof");
        assert!((sof - 2000.0).abs() < 0.5, "{sof}");
        let mixed = strength_of_field([1000.0, 3000.0].into_iter()).expect("sof");
        assert!(
            mixed > 1000.0 && mixed < 2000.0,
            "weighted toward the lower rating: {mixed}"
        );
        assert_eq!(strength_of_field([0.0].into_iter()), None);
    }

    #[test]
    fn irating_changes_reward_beating_the_expectation() {
        let mut out = [0.0; 2];
        irating_changes(&[1500.0, 1500.0], &[1, 2], &mut out);
        assert!((out[0] - 50.0).abs() < 0.01, "{out:?}");
        assert!((out[1] + 49.0).abs() < 0.01, "{out:?}");
        // Beating a much higher rated driver gains more than beating an equal one.
        irating_changes(&[1500.0, 3000.0], &[1, 2], &mut out);
        assert!(out[0] > 60.0 && out[1] < -60.0, "{out:?}");
        // Finishing behind a much higher rated driver costs little.
        irating_changes(&[3000.0, 1500.0], &[1, 2], &mut out);
        assert!(out[1] > -30.0 && out[0] < 30.0, "{out:?}");
    }

    #[test]
    fn licenses_maker_letters_and_durations() {
        let mut out = FixedText::new(8);
        short_license("D 2.83", &mut out);
        assert_eq!(out.as_str(), "D2.8");
        short_license("A 4.99", &mut out);
        assert_eq!(out.as_str(), "A4.9");
        short_license("R 1.00", &mut out);
        assert_eq!(out.as_str(), "R1.0");
        assert_eq!(maker_letter("BMW M4 GT3"), Some('B'));
        assert_eq!(maker_letter("porsche 911 GT3 R"), Some('P'));
        assert_eq!(maker_letter("911"), None);
        let mut t = FixedText::new(24);
        write_clock(&mut t, 40000.0);
        assert_eq!(t.as_str(), "11:06:40");
        t.clear();
        write_clock(&mut t, 3264.0);
        assert_eq!(t.as_str(), "54:24");
        for (s, want) in [(7200.0, "2h"), (80_000.0, "22h13m"), (2700.0, "45m")] {
            t.clear();
            write_length(&mut t, s);
            assert_eq!(t.as_str(), want);
        }
        write_delta(&mut t, -1.43);
        assert_eq!(t.as_str(), "1.4");
        write_delta(&mut t, 13.2);
        assert_eq!(t.as_str(), "13");
    }
}

/// The widget against a simulated sim: session results and telemetry through the real pipeline.
#[cfg(all(test, windows))]
mod pipeline_tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use pitot_irsdk::layout::{VarHeader, VarType};
    use pitot_irsdk::live::{ReaderConfig, SdkNames};
    use pitot_irsdk::publish::{Publisher, PublisherConfig};

    use super::*;
    use crate::pipeline::Pipeline;

    const CARS: usize = 4;
    const ROW: usize = 24 + CARS * 4 * 7;

    const SESSION: &str = "---
WeekendInfo:
 TrackID: 7
 SubSessionID: 1
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionLaps: unlimited
   SessionTime: 3600.0000 sec
   SessionType: Practice
   ResultsPositions:
   - Position: 1
     ClassPosition: 0
     CarIdx: 1
     FastestTime: 80.0000
     LastTime: 81.0000
     LapsComplete: 5
   - Position: 2
     ClassPosition: 1
     CarIdx: 2
     FastestTime: 80.5000
     LastTime: 80.5000
     LapsComplete: 4
DriverInfo:
 DriverCarIdx: 2
 DriverTires:
 - TireIndex: 0
   TireCompoundType: \"Hard\"
 Drivers:
 - CarIdx: 1
   UserName: Early Driver
   CarNumber: 7
   CarScreenName: BMW M4 GT3
   CarClassID: 10
   CarClassShortName: GT3
   CarClassColor: 0xffda59
   IRating: 2500
   LicString: A 3.41
   LicColor: 0x0153db
 - CarIdx: 2
   UserName: Player
   CarNumber: 12
   CarScreenName: Porsche 911 GT3 R (992)
   CarClassID: 10
   CarClassShortName: GT3
   CarClassColor: 0xffda59
   IRating: 1500
   LicString: D 2.83
   LicColor: 0xfc8a27
SplitTimeInfo:
 Sectors:
 - SectorNum: 0
   SectorStartPct: 0.000000
 - SectorNum: 1
   SectorStartPct: 0.500000
...
";

    fn publisher(names: &SdkNames) -> Publisher {
        let array = |t, n, name| VarHeader::new(t, n, CARS as i32, name).expect("valid");
        let base = 24;
        let at = |k: usize| (base + k * CARS * 4) as i32;
        let config = PublisherConfig {
            vars: vec![
                VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
                VarHeader::new(VarType::Int, 8, 1, "SessionNum").expect("valid"),
                VarHeader::new(VarType::Int, 12, 1, "PlayerCarIdx").expect("valid"),
                VarHeader::new(VarType::Float, 16, 1, "AirTemp").expect("valid"),
                VarHeader::new(VarType::Float, 20, 1, "TrackTempCrew").expect("valid"),
                array(VarType::Float, at(0), "CarIdxLapDistPct"),
                array(VarType::Float, at(1), "CarIdxBestLapTime"),
                array(VarType::Float, at(2), "CarIdxLastLapTime"),
                array(VarType::Int, at(3), "CarIdxTrackSurface"),
                array(VarType::Int, at(4), "CarIdxTireCompound"),
                array(VarType::Int, at(5), "CarIdxClassPosition"),
                array(VarType::Float, at(6), "CarIdxF2Time"),
            ],
            buf_len: ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 8192,
        };
        let mut publisher = Publisher::create(names, &config).expect("creates");
        publisher.set_connected(true).expect("connects");
        publisher
            .set_session_info(SESSION.as_bytes())
            .expect("fits");
        publisher
    }

    fn row() -> Vec<u8> {
        let mut row = vec![0u8; ROW];
        row[0..8].copy_from_slice(&1000.0f64.to_le_bytes());
        row[12..16].copy_from_slice(&2i32.to_le_bytes());
        row[16..20].copy_from_slice(&31.6f32.to_le_bytes());
        row[20..24].copy_from_slice(&26.2f32.to_le_bytes());
        let put = |row: &mut Vec<u8>, k: usize, car: usize, bytes: [u8; 4]| {
            let at = 24 + k * CARS * 4 + car * 4;
            row[at..at + 4].copy_from_slice(&bytes);
        };
        for car in 0..CARS {
            put(&mut row, 0, car, 0.25f32.to_le_bytes());
            put(&mut row, 3, car, 3i32.to_le_bytes());
        }
        // The early driver's laps are only in the session's results: telemetry has none for that car.
        put(&mut row, 1, 2, 80.5f32.to_le_bytes());
        row
    }

    #[test]
    fn results_from_before_joining_are_shown() {
        let pid = std::process::id();
        let names = SdkNames::new(
            &format!("Local\\PitotStandingsMap-{pid}"),
            &format!("Local\\PitotStandingsEvent-{pid}"),
        );
        let mut publisher = publisher(&names);
        let config = ReaderConfig {
            retry_interval: Duration::from_millis(5),
            idle_timeout: Duration::from_millis(50),
        };
        let (_pipeline, mut view) =
            Pipeline::start(names, config, Vec::new(), None).expect("starts");
        let deadline = Instant::now() + Duration::from_secs(5);
        while view.session().is_none() {
            assert!(Instant::now() < deadline, "session info arrives");
            publisher.publish(&row()).expect("publishes");
            thread::sleep(Duration::from_millis(5));
            view.poll();
        }

        let config = WidgetConfig {
            enabled: true,
            monitor: None,
            x: 0,
            y: 0,
            width: Some(760),
            height: Some(240),
            rate_hz: None,
            opacity: 0.8,
            options: toml::Table::new(),
        };
        let mut widget = StandingsWidget::new(&config);
        widget.fill(&view);
        let Row::Header(header) = &widget.next[0] else {
            panic!("a class header first: {:?}", widget.next[0]);
        };
        assert_eq!(header.name.as_str(), "GT3");
        assert_eq!(header.cars.as_str(), "2/2");
        assert!(header.first);
        assert_eq!(header.session.as_str(), "P");
        assert_eq!(header.air.as_str(), "Air 31.6\u{b0}C");
        assert_eq!(header.track.as_str(), "Track 26.2\u{b0}C");
        assert_eq!(header.time.as_str(), "16:40/1h");
        assert!(
            header.sof.as_str().starts_with("SOF 1."),
            "{}",
            header.sof.as_str()
        );

        let (Row::Car(first), Row::Car(second)) = (&widget.next[1], &widget.next[2]) else {
            panic!("two cars: {:?}", &widget.next[1..3]);
        };
        assert_eq!(first.position.as_str(), "1");
        assert_eq!(
            first.last_lap.as_str(),
            "1:21.000",
            "from the session's results"
        );
        assert_eq!(first.mark, LapMark::None);
        assert!(first.leader && !first.player);
        assert_eq!(first.maker.as_str(), "B");
        assert_eq!(first.license.as_str(), "A3.4");
        assert_eq!(first.tire, tire_color("Hard"));
        assert_eq!(second.position.as_str(), "2");
        assert!(second.player);
        assert_eq!(second.gap.as_str(), "0.5", "best laps outside a race");
        assert_eq!(second.interval.as_str(), "0.5");
        assert_eq!(second.last_lap.as_str(), "1:20.500");
        assert_eq!(second.mark, LapMark::Personal);
        assert_eq!(second.maker.as_str(), "P");
        assert_eq!(second.change, 0, "no position change outside a race");
    }

    const RACE_CARS: usize = 5;
    const RACE_TICK: usize = 20 + RACE_CARS * 4 * 4;
    const RACE_ROW: usize = RACE_TICK + 4;

    const RACE_SESSION: &str = "---
WeekendInfo:
 TrackID: 7
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionLaps: 20
   SessionTime: unlimited
   SessionType: Race
   ResultsPositions:
   - Position: 1
     ClassPosition: 0
     CarIdx: 1
     LapsComplete: 3
   - Position: 2
     ClassPosition: 1
     CarIdx: 2
     LapsComplete: 3
   - Position: 3
     ClassPosition: 2
     CarIdx: 3
     LapsComplete: 3
   - Position: 4
     ClassPosition: 3
     CarIdx: 4
     LapsComplete: 2
QualifyResultsInfo:
 Results:
 - Position: 0
   ClassPosition: 0
   CarIdx: 1
 - Position: 1
   ClassPosition: 1
   CarIdx: 2
 - Position: 2
   ClassPosition: 2
   CarIdx: 3
 - Position: 3
   ClassPosition: 3
   CarIdx: 4
DriverInfo:
 DriverCarIdx: 3
 Drivers:
 - CarIdx: 0
   CarNumber: 0
   CarIsPaceCar: 1
   CarClassID: 11
 - CarIdx: 1
   CarNumber: 1
   CarClassID: 10
 - CarIdx: 2
   CarNumber: 2
   CarClassID: 10
 - CarIdx: 3
   CarNumber: 3
   CarClassID: 10
 - CarIdx: 4
   CarNumber: 4
   CarClassID: 10
...
";

    /// A race on 100 s laps from session time 1000 to `t`: car 2 leads car 1 by 2 s on track although the results
    /// still have car 1 ahead. Car 3 (the player) starts 10 s behind car 2 and is slower, 125 s laps, so its gaps
    /// grow by 0.2 s every second. Car 4 is 1.05 laps behind car 2. The pace car is out ahead of everyone.
    fn race_row(t: f64, state: i32) -> Vec<u8> {
        let mut row = vec![0u8; RACE_ROW];
        row[0..8].copy_from_slice(&t.to_le_bytes());
        row[RACE_TICK..RACE_TICK + 4].copy_from_slice(&((t * 60.0).round() as i32).to_le_bytes());
        row[12..16].copy_from_slice(&3i32.to_le_bytes());
        row[16..20].copy_from_slice(&state.to_le_bytes());
        let put = |row: &mut Vec<u8>, k: usize, car: usize, bytes: [u8; 4]| {
            let at = 20 + k * RACE_CARS * 4 + car * 4;
            row[at..at + 4].copy_from_slice(&bytes);
        };
        let start_and_speed = [
            (3.90, 0.01),
            (3.38, 0.01),
            (3.40, 0.01),
            (3.30, 0.008),
            (2.35, 0.01),
        ];
        for (car, (start, speed)) in start_and_speed.into_iter().enumerate() {
            let progress = start + speed * (t - 1000.0);
            let laps = progress.floor();
            put(&mut row, 0, car, ((progress - laps) as f32).to_le_bytes());
            put(&mut row, 1, car, (laps as i32).to_le_bytes());
            put(&mut row, 2, car, 3i32.to_le_bytes());
        }
        row
    }

    fn race_publisher(names: &SdkNames) -> Publisher {
        let array = |t, k: usize, name| {
            VarHeader::new(t, (20 + k * RACE_CARS * 4) as i32, RACE_CARS as i32, name)
                .expect("valid")
        };
        let config = PublisherConfig {
            vars: vec![
                VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
                VarHeader::new(VarType::Int, 8, 1, "SessionNum").expect("valid"),
                VarHeader::new(VarType::Int, 12, 1, "PlayerCarIdx").expect("valid"),
                VarHeader::new(VarType::Int, 16, 1, "SessionState").expect("valid"),
                array(VarType::Float, 0, "CarIdxLapDistPct"),
                array(VarType::Int, 1, "CarIdxLapCompleted"),
                array(VarType::Int, 2, "CarIdxTrackSurface"),
                array(VarType::Float, 3, "CarIdxF2Time"),
                VarHeader::new(VarType::Int, RACE_TICK as i32, 1, "SessionTick").expect("valid"),
            ],
            buf_len: RACE_ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 8192,
        };
        let mut publisher = Publisher::create(names, &config).expect("creates");
        publisher.set_connected(true).expect("connects");
        publisher
            .set_session_info(RACE_SESSION.as_bytes())
            .expect("fits");
        publisher
    }

    fn cars(widget: &StandingsWidget) -> Vec<&CarRow> {
        widget
            .next
            .iter()
            .filter_map(|r| match r {
                Row::Car(car) => Some(car),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn race_order_and_gaps_are_live_from_the_green_flag() {
        let pid = std::process::id();
        let names = SdkNames::new(
            &format!("Local\\PitotStandingsRaceMap-{pid}"),
            &format!("Local\\PitotStandingsRaceEvent-{pid}"),
        );
        let mut publisher = race_publisher(&names);
        let config = ReaderConfig {
            retry_interval: Duration::from_millis(5),
            idle_timeout: Duration::from_millis(50),
        };
        let (_pipeline, mut view) =
            Pipeline::start(names, config, Vec::new(), None).expect("starts");
        // Publishes until the view shows this moment.
        let mut step = |view: &mut View, t: f64, state: i32| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "the view reaches {t}");
                publisher.publish(&race_row(t, state)).expect("publishes");
                thread::sleep(Duration::from_millis(2));
                view.poll();
                let shown = view.snapshot().f64(view.vars().session_time, 0);
                if view.session().is_some() && shown == Some(t) {
                    break;
                }
            }
        };
        let config = WidgetConfig {
            enabled: true,
            monitor: None,
            x: 0,
            y: 0,
            width: Some(760),
            height: Some(240),
            rate_hz: None,
            opacity: 0.8,
            options: toml::Table::new(),
        };
        let mut widget = StandingsWidget::new(&config);

        // Under the pace laps the results' order stands.
        step(&mut view, 999.5, session_state::PARADE_LAPS);
        widget.fill(&view);
        let numbers: Vec<&str> = cars(&widget).iter().map(|c| c.number.as_str()).collect();
        assert_eq!(numbers, ["1", "2", "3", "4"]);

        // From the green flag the order is the track's, and gaps are timed once the car ahead has passed.
        let mut t = 1000.0;
        while t <= 1110.0 {
            step(&mut view, t, session_state::RACING);
            widget.fill(&view);
            t += 0.5;
        }
        let rows = cars(&widget);
        let numbers: Vec<&str> = rows.iter().map(|c| c.number.as_str()).collect();
        assert_eq!(numbers, ["2", "1", "3", "4"], "the pace car is left out");
        let positions: Vec<&str> = rows.iter().map(|c| c.position.as_str()).collect();
        assert_eq!(positions, ["1", "2", "3", "4"]);
        assert!(rows[0].leader);
        assert_eq!(
            (rows[0].change, rows[1].change),
            (1, -1),
            "against the grid"
        );
        assert_eq!(
            (rows[1].gap.as_str(), rows[1].interval.as_str()),
            ("2.0", "2.0")
        );
        // 110 s in, car 3 is 10 + 0.2 * 110 s behind car 2 and 8 + 0.2 * 110 s behind car 1.
        assert_eq!(
            (rows[2].gap.as_str(), rows[2].interval.as_str()),
            ("32.0", "30.0")
        );
        assert!(rows[2].player);
        assert_eq!(
            rows[3].gap.as_str(),
            "+1L",
            "more than a lap behind the leader"
        );
        // Car 3 passed car 4's spot (3.45 laps) 18.75 s after the start.
        assert_eq!(rows[3].interval.as_str(), "1:31");

        // Half a second later only car 3's gaps change: only the gap columns of its row are drawn again.
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        widget.update(&mut view, Instant::now(), &mut redraw);
        assert_eq!(
            redraw[0],
            Redraw::Area(Rect::new(0.0, 0.0, 760.0, 5.0 * ROW_HEIGHT))
        );
        step(&mut view, 1110.5, session_state::RACING);
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        widget.update(&mut view, Instant::now(), &mut redraw);
        let cols = Columns::for_width(760.0, 0);
        assert_eq!(
            redraw[0],
            Redraw::Area(Rect::new(
                cols.gap,
                3.0 * ROW_HEIGHT,
                cols.last - cols.gap,
                ROW_HEIGHT
            ))
        );
        let Row::Car(car) = &widget.rows[3] else {
            panic!("car 3's row: {:?}", widget.rows[3]);
        };
        assert_eq!((car.gap.as_str(), car.interval.as_str()), ("32.1", "30.1"));
    }
}
