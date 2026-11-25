//! Sectors: the player's time in each sector of the lap, against the player's best, the class's best this
//! session, and the cars directly ahead and behind in class order.
//!
//! Sector times are timed by the overlay from lap distance (see [`super::timing`]). In a race the comparison with
//! a neighbour is the time gained or lost against it in that sector: the player's sector time minus the
//! neighbour's for passes of the sector next to each other in time (the car ahead passed just before the player,
//! the car behind just after). Outside a race the class order follows best laps, so the neighbours are compared by
//! their best time in that sector instead.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use pitot_irsdk::session::{Session, SessionInfo};

use crate::config::{NoOptions, WidgetConfig};
use crate::pipeline::View;
use crate::render::{Align, Canvas, Color, Rect};

use super::palette::{BEST, DIM, GAIN, LIFT_BAND, LOSS, SURFACE, TEXT};
use super::standings::write_lap_time;
use super::text::FixedText;
use super::timing::{MAX_SECTORS, SectorClock, SectorStarts};
use super::{Build, MAX_LAYERS, Redraw, Register, Widget};

const HEADER_HEIGHT: f32 = 22.0;
const MAX_ROW_HEIGHT: f32 = 24.0;
const MIN_ROW_HEIGHT: f32 = 16.0;
const TEXT_SIZE: f32 = 13.0;
const LABEL_SIZE: f32 = 11.0;
/// Four value columns (without the car behind), and the header with six sectors' rows at their smallest.
const MIN_SIZE: (u32, u32) = (260, 120);
/// From this width the fifth column shows, the car behind. A large text size makes the widget narrower in widget
/// pixels.
const FIVE_COLUMNS_WIDTH: f32 = 340.0;
/// The narrowest value column whose header has room for the car's number beside its name.
const NUMBERS_COLUMN: f32 = 64.0;
/// `CarIdxTrackSurface` value for a car that is not in the world.
const NOT_IN_WORLD: i32 = -1;
/// How far apart in time two passes of a sector may be to count as next to each other, while the player has no
/// lap time yet.
const FALLBACK_WINDOW: f64 = 600.0;
/// Times closer than this are the same time.
const SAME_TIME: f32 = 5e-4;
/// Gains and losses smaller than this are shown as even.
const EVEN: f32 = 0.005;


/// A sector time: seconds to the millisecond under a minute (`32.456`), else `m:ss.sss`.
fn write_sector_time(out: &mut FixedText, seconds: f32) {
    out.clear();
    if seconds <= 0.0 {
        return;
    }
    if seconds < 59.9995 {
        let _ = write!(out, "{seconds:.3}");
    } else {
        write_lap_time(out, seconds);
    }
}

/// A time difference without its sign: hundredths under ten seconds, then tenths, then whole seconds.
fn write_delta(out: &mut FixedText, seconds: f32) {
    out.clear();
    let d = seconds.abs();
    if d < 9.995 {
        let _ = write!(out, "{d:.2}");
    } else if d < 99.95 {
        let _ = write!(out, "{d:.1}");
    } else {
        let _ = write!(out, "{d:.0}");
    }
}

/// The player's time in sector `s` minus the rival's, for passes next to each other in time: the rival's pass
/// ended before the player's for the car ahead, after it for the car behind, less than `window` seconds apart.
fn adjacent_gap(
    player: &SectorClock,
    rival: &SectorClock,
    s: usize,
    ahead: bool,
    window: f64,
) -> Option<f32> {
    let (mine, theirs) = (player.last[s], rival.last[s]);
    let (p, r) = (player.done[s], rival.done[s]);
    if mine <= 0.0 || theirs <= 0.0 || p < 0.0 || r < 0.0 {
        return None;
    }
    let apart = if ahead { p - r } else { r - p };
    (0.0..window).contains(&apart).then_some(mine - theirs)
}

/// The cars directly ahead of and behind the player in class order: from the session's results (they include
/// laps driven before the player joined), else from `CarIdxClassPosition`.
fn neighbours(
    info: &SessionInfo,
    session: Option<&Session>,
    view: &View,
    player: usize,
    in_class: impl Fn(usize) -> bool,
) -> (Option<usize>, Option<usize>) {
    if let Some(results) = session.map(|s| &s.results)
        && let Some(p) = results
            .iter()
            .find(|r| r.car_idx == Some(player))
            .and_then(|r| r.class_position)
    {
        let at = |want: i64| {
            results
                .iter()
                .find(|r| r.class_position == Some(want) && r.car_idx.is_some_and(&in_class))
                .and_then(|r| r.car_idx)
        };
        return ((p > 0).then(|| at(p - 1)).flatten(), at(p + 1));
    }
    let (snap, vars) = (view.snapshot(), view.vars());
    let Some(p) = snap.i32(vars.car_class_position, player).filter(|&p| p > 0) else {
        return (None, None);
    };
    let cars = view.car_count().max(info.driver_info.drivers.len());
    let at = |want: i32| {
        (0..cars).find(|&c| {
            c != player && in_class(c) && snap.i32(vars.car_class_position, c) == Some(want)
        })
    };
    ((p > 1).then(|| at(p - 1)).flatten(), at(p + 1))
}

/// Gains and losses against one neighbour in class order, per sector.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rival {
    car: Option<usize>,
    /// The player's time minus the rival's, and when the player's pass it is about ended.
    gap: [Option<(f32, f64)>; MAX_SECTORS],
}

impl Default for Rival {
    fn default() -> Self {
        Self {
            car: None,
            gap: [None; MAX_SECTORS],
        }
    }
}

impl Rival {
    /// A different car starts over.
    fn set_car(&mut self, car: Option<usize>) {
        if self.car != car {
            *self = Self {
                car,
                ..Self::default()
            };
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    /// The player's best time in the sector.
    Personal,
    /// The class's best time in the sector.
    Class,
}

/// A comparison cell: the size of the difference, whether the player gained (negative) or lost (positive), and
/// whether it is about the player's latest pass of the sector.
#[derive(Debug, Clone, PartialEq)]
struct Gap {
    text: FixedText,
    sign: i8,
    fresh: bool,
}

impl Gap {
    fn new() -> Self {
        Self {
            text: FixedText::new(8),
            sign: 0,
            fresh: false,
        }
    }

    fn set(&mut self, gap: Option<(f32, f64)>, latest: f64) {
        self.text.clear();
        self.sign = 0;
        self.fresh = false;
        if let Some((d, at)) = gap {
            write_delta(&mut self.text, d);
            self.sign = if d >= EVEN {
                1
            } else if d <= -EVEN {
                -1
            } else {
                0
            };
            self.fresh = at == latest;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SectorRow {
    number: FixedText,
    /// The player is in this sector.
    current: bool,
    /// The time is from the current lap; otherwise it is the last lap's.
    fresh: bool,
    time: FixedText,
    mark: Mark,
    to_best: FixedText,
    to_class: FixedText,
    ahead: Gap,
    behind: Gap,
}

impl SectorRow {
    fn new() -> Self {
        Self {
            number: FixedText::new(4),
            current: false,
            fresh: false,
            time: FixedText::new(12),
            mark: Mark::None,
            to_best: FixedText::new(8),
            to_class: FixedText::new(8),
            ahead: Gap::new(),
            behind: Gap::new(),
        }
    }
}

/// The car numbers of the neighbours compared with.
#[derive(Debug, Clone, PartialEq)]
struct Header {
    ahead: FixedText,
    behind: FixedText,
}

impl Header {
    fn new() -> Self {
        Self {
            ahead: FixedText::new(8),
            behind: FixedText::new(8),
        }
    }
}

/// Where each column goes for a given width: the sector number, then five equal value columns.
#[derive(Debug, Clone, Copy)]
struct Columns {
    number: f32,
    first: f32,
    width: f32,
    /// Value columns shown: five, or four without the car behind in a narrow widget.
    count: usize,
}

impl Columns {
    fn for_width(w: f32) -> Self {
        let first = 30.0;
        let count = if w >= FIVE_COLUMNS_WIDTH { 5 } else { 4 };
        Self {
            number: 6.0,
            first,
            width: ((w - first - 6.0) / count as f32).max(0.0),
            count,
        }
    }

    /// The cell of value column `k`, inset so right-aligned values keep a margin.
    fn cell(&self, k: usize, y: f32, h: f32) -> Rect {
        Rect::new(
            self.first + k as f32 * self.width,
            y,
            (self.width - 6.0).max(0.0),
            h,
        )
    }
}

/// Row height for `count` sector rows in a widget of height `h`.
fn row_height(count: usize, h: f32) -> f32 {
    if count == 0 {
        return MAX_ROW_HEIGHT;
    }
    ((h - HEADER_HEIGHT) / count as f32).clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT)
}

/// The sectors widget.
#[derive(Debug)]
pub struct SectorsWidget {
    width: u32,
    height: u32,
    opacity: f32,
    clocks: Vec<SectorClock>,
    sectors: SectorStarts,
    /// The session the times belong to, once one was seen.
    session: Option<Option<i64>>,
    ahead: Rival,
    behind: Rival,
    header: Header,
    next_header: Header,
    rows: Vec<SectorRow>,
    next: Vec<SectorRow>,
    count: usize,
    next_count: usize,
}

impl SectorsWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        let width = config.width.unwrap_or(400).max(MIN_SIZE.0);
        let height = config.height.unwrap_or(200).max(MIN_SIZE.1);
        Self {
            width,
            height,
            opacity: config.opacity,
            clocks: Vec::with_capacity(72),
            sectors: SectorStarts::default(),
            session: None,
            ahead: Rival::default(),
            behind: Rival::default(),
            header: Header::new(),
            next_header: Header::new(),
            rows: (0..MAX_SECTORS).map(|_| SectorRow::new()).collect(),
            next: (0..MAX_SECTORS).map(|_| SectorRow::new()).collect(),
            count: 0,
            next_count: 0,
        }
    }

    fn fill(&mut self, view: &View) {
        let (snap, vars) = (view.snapshot(), view.vars());
        self.next_count = 0;
        self.next_header.ahead.clear();
        self.next_header.behind.clear();
        let Some(info) = view.session() else {
            return;
        };
        let cars = view.car_count().max(info.driver_info.drivers.len());
        if self.clocks.len() != cars {
            self.clocks.clear();
            self.clocks.resize(cars, SectorClock::default());
            self.ahead = Rival::default();
            self.behind = Rival::default();
        }
        let session_num = snap.i32(vars.session_num, 0).map(i64::from);
        let mut starts = SectorStarts::default();
        starts.load(info);
        if self.session != Some(session_num) || starts != self.sectors {
            // A new session or a new sector layout: the times start over.
            self.session = Some(session_num);
            self.sectors = starts;
            self.clocks
                .iter_mut()
                .for_each(|c| *c = SectorClock::default());
            self.ahead = Rival::default();
            self.behind = Rival::default();
        }

        let now = view.tick_time().unwrap_or(0.0);
        let starts = self.sectors.as_slice();
        for (car, clock) in self.clocks.iter_mut().enumerate() {
            let pct = snap.f32(vars.car_lap_dist_pct, car).unwrap_or(-1.0);
            let in_world = snap.i32(vars.car_track_surface, car) != Some(NOT_IN_WORLD);
            let pit = snap.bool(vars.car_on_pit_road, car).unwrap_or(false);
            // A hair below zero at the line is on the lap; -1 means not in the world.
            clock.observe(starts, pct, now, in_world && !pit && pct >= -0.01);
        }

        let Some(player) = snap
            .i32(vars.player_car_idx, 0)
            .and_then(|i| usize::try_from(i).ok())
            .or(info.driver_info.player_car_idx)
            .filter(|&p| p < self.clocks.len())
        else {
            return;
        };
        let drivers = &info.driver_info.drivers;
        let class = drivers
            .get(player)
            .and_then(Option::as_ref)
            .and_then(|d| d.car_class_id);
        let in_class = |car: usize| {
            drivers
                .get(car)
                .and_then(Option::as_ref)
                .is_some_and(|d| !d.is_pace_car && !d.is_spectator && d.car_class_id == class)
        };
        let session = info
            .sessions
            .iter()
            .find(|s| s.num == session_num)
            .or(info.sessions.last());
        let is_race = session
            .and_then(|s| s.session_type.as_deref())
            .is_some_and(|t| t.eq_ignore_ascii_case("race"));
        let (ahead, behind) = neighbours(info, session, view, player, in_class);
        self.ahead.set_car(ahead);
        self.behind.set_car(behind);
        for (car, out) in [
            (ahead, &mut self.next_header.ahead),
            (behind, &mut self.next_header.behind),
        ] {
            if let Some(number) = car
                .and_then(|c| drivers.get(c)?.as_ref())
                .and_then(|d| d.car_number.as_deref())
            {
                let _ = write!(out, "#{number}");
            }
        }

        let mine = &self.clocks[player];
        let current = mine.current();
        // Passes of a sector next to each other in time are less than a lap apart.
        let window = [vars.lap_best_time, vars.lap_last_time]
            .into_iter()
            .filter_map(|v| snap.f32(v, 0))
            .find(|&t| t > 0.0)
            .map_or(FALLBACK_WINDOW, f64::from);
        self.next_count = self.sectors.len().min(MAX_SECTORS);
        for s in 0..self.next_count {
            let row = &mut self.next[s];
            row.number.clear();
            let _ = write!(row.number, "{}", s + 1);
            row.current = current == Some(s);
            let t = mine.last[s];
            row.fresh = t > 0.0 && current.is_some_and(|c| s < c);
            write_sector_time(&mut row.time, t);

            let best = mine.best[s];
            let class_best = self
                .clocks
                .iter()
                .enumerate()
                .filter(|&(car, c)| c.best[s] > 0.0 && in_class(car))
                .map(|(_, c)| c.best[s])
                .fold(f32::MAX, f32::min);
            row.mark = if t <= 0.0 {
                Mark::None
            } else if (t - class_best).abs() < SAME_TIME {
                Mark::Class
            } else if (t - best).abs() < SAME_TIME {
                Mark::Personal
            } else {
                Mark::None
            };
            row.to_best.clear();
            if t > 0.0 && best > 0.0 {
                write_delta(&mut row.to_best, t - best);
            }
            row.to_class.clear();
            if t > 0.0 && class_best < f32::MAX {
                write_delta(&mut row.to_class, t - class_best);
            }

            for (rival, ahead, cell) in [
                (&mut self.ahead, true, &mut row.ahead),
                (&mut self.behind, false, &mut row.behind),
            ] {
                if let Some(theirs) = rival.car.and_then(|c| self.clocks.get(c)) {
                    if is_race {
                        if let Some(g) = adjacent_gap(mine, theirs, s, ahead, window) {
                            rival.gap[s] = Some((g, mine.done[s]));
                        }
                    } else {
                        rival.gap[s] = (t > 0.0 && theirs.best[s] > 0.0)
                            .then_some((t - theirs.best[s], mine.done[s]));
                    }
                }
                cell.set(rival.gap[s], mine.done[s]);
            }
        }
    }
}

impl Register for SectorsWidget {
    type Vars = ();
    type Options = NoOptions;

    fn build(cx: &Build<'_>, _: ()) -> Self {
        Self::new(cx.config)
    }
}

impl Widget for SectorsWidget {
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
        self.fill(view);
        let header_changed = self.header != self.next_header;
        if header_changed {
            std::mem::swap(&mut self.header, &mut self.next_header);
        }
        if self.next_count != self.count {
            std::mem::swap(&mut self.rows, &mut self.next);
            self.count = self.next_count;
            redraw[0] = Redraw::All;
            return;
        }
        // Only the rows that changed are drawn again.
        let changed = |s: &usize| self.rows[*s] != self.next[*s];
        let first = (0..self.count).find(changed);
        let last = (0..self.count).rev().find(changed);
        if first.is_some() {
            std::mem::swap(&mut self.rows, &mut self.next);
        }
        let row_h = row_height(self.count, self.height as f32);
        let top = if header_changed {
            Some(0.0)
        } else {
            first.map(|s| HEADER_HEIGHT + s as f32 * row_h)
        };
        let bottom = last.map_or(HEADER_HEIGHT, |s| HEADER_HEIGHT + (s + 1) as f32 * row_h);
        if let Some(top) = top {
            redraw[0] = Redraw::Area(Rect::new(0.0, top, self.width as f32, bottom - top));
        }
    }

    fn draw(&self, _layer: usize, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill_background(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(SURFACE, self.opacity),
        );
        let cols = Columns::for_width(w);
        let dim = Color::rgb(DIM, 1.0);
        let label = |canvas: &mut Canvas<'_>, k: usize, text: &str, align: Align| {
            canvas.text(
                text,
                cols.cell(k, 0.0, HEADER_HEIGHT),
                LABEL_SIZE,
                align,
                dim,
            );
        };
        canvas.text(
            "S",
            Rect::new(cols.number, 0.0, 20.0, HEADER_HEIGHT),
            LABEL_SIZE,
            Align::Right,
            dim,
        );
        label(canvas, 0, "Time", Align::Right);
        label(canvas, 1, "Best", Align::Right);
        label(canvas, 2, "Class", Align::Right);
        label(canvas, 3, "Ahead", Align::Left);
        let behind = cols.count > 4;
        if behind {
            label(canvas, 4, "Behind", Align::Left);
        }
        // The cars' numbers beside the names, where a column has room for both.
        if cols.width >= NUMBERS_COLUMN {
            label(canvas, 3, self.header.ahead.as_str(), Align::Right);
            if behind {
                label(canvas, 4, self.header.behind.as_str(), Align::Right);
            }
        }
        canvas.fill(
            Rect::new(0.0, HEADER_HEIGHT - 1.0, w, 1.0),
            Color::rgb(DIM, 0.3),
        );
        if self.count == 0 {
            canvas.text(
                "No sectors yet",
                Rect::new(cols.first, HEADER_HEIGHT, w - cols.first, MAX_ROW_HEIGHT),
                TEXT_SIZE,
                Align::Left,
                dim,
            );
            return;
        }

        let row_h = row_height(self.count, h);
        let area = canvas.area();
        for (s, row) in self.rows[..self.count].iter().enumerate() {
            let y = HEADER_HEIGHT + s as f32 * row_h;
            if y + row_h <= area.y || y >= area.y + area.h || y + row_h > h + 0.5 {
                continue;
            }
            // The sector the player is in: a band without a hue, as every row here is the player's.
            if row.current {
                canvas.fill(Rect::new(0.0, y, w, row_h), Color::rgb(TEXT, LIFT_BAND));
            }
            let alpha = if row.fresh { 1.0 } else { 0.55 };
            canvas.text(
                row.number.as_str(),
                Rect::new(cols.number, y, 20.0, row_h),
                TEXT_SIZE,
                Align::Right,
                dim,
            );
            let time_color = match row.mark {
                Mark::Class => BEST,
                Mark::Personal => GAIN,
                Mark::None => TEXT,
            };
            canvas.text(
                row.time.as_str(),
                cols.cell(0, y, row_h),
                TEXT_SIZE,
                Align::Right,
                Color::rgb(time_color, alpha),
            );
            for (k, text) in [(1, &row.to_best), (2, &row.to_class)] {
                canvas.text(
                    text.as_str(),
                    cols.cell(k, y, row_h),
                    TEXT_SIZE,
                    Align::Right,
                    Color::rgb(TEXT, alpha),
                );
            }
            for (k, gap) in [(3, &row.ahead), (4, &row.behind)].into_iter().take(cols.count - 3) {
                let color = match gap.sign {
                    -1 => GAIN,
                    1 => LOSS,
                    _ => DIM,
                };
                canvas.text(
                    gap.text.as_str(),
                    cols.cell(k, y, row_h),
                    TEXT_SIZE,
                    Align::Right,
                    Color::rgb(color, if gap.fresh { 1.0 } else { 0.55 }),
                );
            }
        }
    }

    fn reset(&mut self) {
        self.clocks
            .iter_mut()
            .for_each(|c| *c = SectorClock::default());
        self.session = None;
        self.ahead = Rival::default();
        self.behind = Rival::default();
        self.count = 0;
        self.next_count = 0;
        self.header.ahead.clear();
        self.header.behind.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_deltas() {
        let mut out = FixedText::new(12);
        write_sector_time(&mut out, 32.4561);
        assert_eq!(out.as_str(), "32.456");
        write_sector_time(&mut out, 59.9996);
        assert_eq!(out.as_str(), "1:00.000");
        write_sector_time(&mut out, 0.0);
        assert_eq!(out.as_str(), "");
        write_delta(&mut out, -0.234);
        assert_eq!(out.as_str(), "0.23");
        write_delta(&mut out, 12.34);
        assert_eq!(out.as_str(), "12.3");
        write_delta(&mut out, 123.4);
        assert_eq!(out.as_str(), "123");
    }

    #[test]
    fn neighbours_are_compared_on_adjacent_passes() {
        let clock = |last: f32, done: f64| {
            let mut c = SectorClock::default();
            c.last[0] = last;
            c.done[0] = done;
            c
        };
        let player = clock(30.0, 200.0);
        // The car ahead passed 2 s earlier, 0.5 s quicker: the player lost 0.5 s.
        let ahead = clock(29.5, 198.0);
        assert_eq!(adjacent_gap(&player, &ahead, 0, true, 100.0), Some(0.5));
        // Its next pass is after the player's: no longer the pass next to the player's.
        let ahead_again = clock(29.0, 297.0);
        assert_eq!(adjacent_gap(&player, &ahead_again, 0, true, 100.0), None);
        // The car behind has not passed since the player did.
        let behind = clock(31.0, 105.0);
        assert_eq!(adjacent_gap(&player, &behind, 0, false, 100.0), None);
        // Now it has, 1 s slower: the player gained 1 s.
        let behind = clock(31.0, 203.0);
        assert_eq!(adjacent_gap(&player, &behind, 0, false, 100.0), Some(-1.0));
        // More than a lap apart does not count.
        assert_eq!(adjacent_gap(&player, &ahead, 0, true, 1.5), None);
        // No time yet.
        assert_eq!(
            adjacent_gap(&player, &SectorClock::default(), 0, true, 100.0),
            None
        );
    }

    #[test]
    fn rows_fit_the_height() {
        assert_eq!(row_height(3, 200.0), MAX_ROW_HEIGHT);
        assert_eq!(row_height(8, 200.0), (200.0 - HEADER_HEIGHT) / 8.0);
        assert_eq!(row_height(12, 200.0), MIN_ROW_HEIGHT);
    }
}

/// The widget against a simulated sim: three cars of one class lap at steady sector times through the real
/// pipeline.
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
    const BASE: usize = 16;
    /// `SessionTick`, in the third of the four per-car blocks, which the arrays leave free.
    const TICK_AT: usize = BASE + 2 * CARS * 4;
    const ROW: usize = BASE + CARS * 4 * 4;

    const SESSION: &str = "---
WeekendInfo:
 TrackID: 7
 SubSessionID: 1
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionLaps: 20
   SessionType: Race
   ResultsPositions:
   - Position: 1
     ClassPosition: 0
     CarIdx: 1
   - Position: 2
     ClassPosition: 1
     CarIdx: 2
   - Position: 3
     ClassPosition: 2
     CarIdx: 3
DriverInfo:
 DriverCarIdx: 2
 Drivers:
 - CarIdx: 1
   CarNumber: 7
   CarClassID: 10
 - CarIdx: 2
   CarNumber: 12
   CarClassID: 10
 - CarIdx: 3
   CarNumber: 33
   CarClassID: 10
SplitTimeInfo:
 Sectors:
 - SectorNum: 0
   SectorStartPct: 0.000000
 - SectorNum: 1
   SectorStartPct: 0.500000
...
";

    /// Sector times and the time into the lap at the session's start, per car.
    const CARS_TIMING: [(usize, [f64; 2], f64); 3] = [
        (1, [50.0, 50.0], 10.0),
        (2, [49.0, 51.0], 5.0),
        (3, [50.5, 50.5], 0.5),
    ];

    /// Lap distance at session time `t` for a car that drives each half of the lap at a steady speed.
    fn pct_at(sectors: [f64; 2], offset: f64, t: f64) -> f32 {
        let lap = sectors[0] + sectors[1];
        let into = (t + offset) % lap;
        let pct = if into < sectors[0] {
            0.5 * into / sectors[0]
        } else {
            0.5 + 0.5 * (into - sectors[0]) / sectors[1]
        };
        pct as f32
    }

    fn row(t: f64) -> Vec<u8> {
        let mut row = vec![0u8; ROW];
        row[0..8].copy_from_slice(&t.to_le_bytes());
        row[TICK_AT..TICK_AT + 4].copy_from_slice(&((t * 60.0).round() as i32).to_le_bytes());
        row[12..16].copy_from_slice(&2i32.to_le_bytes());
        let put = |row: &mut Vec<u8>, k: usize, car: usize, bytes: [u8; 4]| {
            let at = BASE + k * CARS * 4 + car * 4;
            row[at..at + 4].copy_from_slice(&bytes);
        };
        for car in 0..CARS {
            put(&mut row, 0, car, (-1.0f32).to_le_bytes());
            put(&mut row, 1, car, (-1i32).to_le_bytes());
        }
        for (car, sectors, offset) in CARS_TIMING {
            put(&mut row, 0, car, pct_at(sectors, offset, t).to_le_bytes());
            put(&mut row, 1, car, 3i32.to_le_bytes());
        }
        row
    }

    #[test]
    fn gains_and_losses_against_the_neighbours() {
        let pid = std::process::id();
        let names = SdkNames::new(
            &format!("Local\\PitotSectorsMap-{pid}"),
            &format!("Local\\PitotSectorsEvent-{pid}"),
        );
        let array = |t, n, name| VarHeader::new(t, n, CARS as i32, name).expect("valid");
        let at = |k: usize| (BASE + k * CARS * 4) as i32;
        let config = PublisherConfig {
            vars: vec![
                VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
                VarHeader::new(VarType::Int, 8, 1, "SessionNum").expect("valid"),
                VarHeader::new(VarType::Int, 12, 1, "PlayerCarIdx").expect("valid"),
                array(VarType::Float, at(0), "CarIdxLapDistPct"),
                array(VarType::Int, at(1), "CarIdxTrackSurface"),
                VarHeader::new(VarType::Int, TICK_AT as i32, 1, "SessionTick").expect("valid"),
            ],
            buf_len: ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 8192,
        };
        let mut publisher = Publisher::create(&names, &config).expect("creates");
        publisher.set_connected(true).expect("connects");
        publisher
            .set_session_info(SESSION.as_bytes())
            .expect("fits");
        let reader = ReaderConfig {
            retry_interval: Duration::from_millis(5),
            idle_timeout: Duration::from_millis(50),
        };
        let (_pipeline, mut view) =
            Pipeline::start(names, reader, Vec::new(), None).expect("starts");
        let deadline = Instant::now() + Duration::from_secs(5);
        while view.session().is_none() {
            assert!(Instant::now() < deadline, "session info arrives");
            publisher.publish(&row(0.0)).expect("publishes");
            thread::sleep(Duration::from_millis(5));
            view.poll();
        }

        let config = WidgetConfig {
            enabled: true,
            monitor: None,
            x: 0,
            y: 0,
            width: Some(400),
            height: Some(200),
            rate_hz: None,
            opacity: 0.8,
            options: toml::Table::new(),
        };
        let mut widget = SectorsWidget::new(&config);
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        // Two and a half laps at 2 Hz, waiting for each tick to arrive.
        let session_time = view.vars().session_time;
        let mut t = 0.0;
        while t < 255.0 {
            publisher.publish(&row(t)).expect("publishes");
            let deadline = Instant::now() + Duration::from_secs(2);
            while view.snapshot().f64(session_time, 0) != Some(t) {
                assert!(Instant::now() < deadline, "tick {t} arrives");
                thread::sleep(Duration::from_millis(1));
                view.poll();
            }
            widget.update(&mut view, Instant::now(), &mut redraw);
            t += 0.5;
        }

        assert_eq!(widget.count, 2);
        assert_eq!(widget.header.ahead.as_str(), "#7");
        assert_eq!(widget.header.behind.as_str(), "#33");
        let (s1, s2) = (&widget.rows[0], &widget.rows[1]);
        assert_eq!(s1.number.as_str(), "1");
        assert_eq!(s1.time.as_str(), "49.000");
        assert_eq!(s1.mark, Mark::Class, "the class's best first sector");
        assert_eq!(s1.to_best.as_str(), "0.00");
        assert_eq!(s1.to_class.as_str(), "0.00");
        assert_eq!(s2.time.as_str(), "51.000");
        assert_eq!(s2.mark, Mark::Personal);
        assert_eq!(s2.to_class.as_str(), "1.00", "the car ahead's 50 s");
        // Against the car ahead: 1 s gained in the first sector, 1 s lost in the second.
        assert_eq!((s1.ahead.text.as_str(), s1.ahead.sign), ("1.00", -1));
        assert_eq!((s2.ahead.text.as_str(), s2.ahead.sign), ("1.00", 1));
        // Against the car behind: 1.5 s gained, then 0.5 s lost.
        assert_eq!((s1.behind.text.as_str(), s1.behind.sign), ("1.50", -1));
        assert_eq!((s2.behind.text.as_str(), s2.behind.sign), ("0.50", 1));
        // At 255 s the player is 11 s into the second sector of lap three.
        assert!(s2.current && !s1.current);
        assert!(
            s1.fresh && !s2.fresh,
            "only the first sector was driven this lap"
        );
    }
}
