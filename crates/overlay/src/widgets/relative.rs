//! Relative: the cars nearest on track ahead of and behind the player, with time gaps, between a header (track
//! temperature, incidents, clock) and a footer (lap, time of day in the sim, session time).
//!
//! Order on track comes from lap distance, normalized to half a lap either way so cars across the start line are
//! placed correctly. The time gap is measured: for a car ahead, the time since it was where the player is now;
//! for a car behind, the time since the player was where that car is. The passes come from the [`RaceClock`]
//! shared with the standings, so laps and classes do not matter.
//!
//! Until a pass has been seen, or when either car is on pit road, the gap is estimated instead: the distance times
//! the player's lap time, or for cars of the player's class `CarIdxEstTime` (the sim's estimate of time into the
//! lap) when it agrees with the distance. Estimated times are not compared across classes, because each class's
//! estimate follows its own pace.

use std::fmt::Write as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use pitot_irsdk::flags::session_flags;

use crate::config::{NoOptions, WidgetConfig};
use crate::pipeline::View;
use crate::platform;
use crate::render::{Align, Canvas, Color, Rect};

use super::standings::{maker_letter, short_license, write_clock, write_length};
use super::text::FixedText;
use super::trackclock::{SharedTrackClock, TrackClock};
use super::{Build, MAX_LAYERS, Redraw, Register, Widget};

/// Rows per 100 px of height: a row is 33⅓ px, so the default 300 px holds the header, three cars on each side of
/// the player, and the footer. A taller widget shows more cars, a pair at a time; the rows share any height left.
const ROWS_PER_100_PX: u32 = 3;
/// Text sizes, the same at any height.
const FRAME_TEXT: f32 = 14.0;
const ROW_TEXT: f32 = 15.0;
/// Height of the badges and the rating and license boxes in a row.
const BADGE_HEIGHT: f32 = 56.0 / 3.0;
/// One car on each side of the player, and room for the name between the left and right columns once the rating
/// and license boxes are left out.
const MIN_SIZE: (u32, u32) = (300, 167);
/// From this width the rows show the rating and license boxes; narrower, the name keeps their room. A large text
/// size makes the widget narrower in widget pixels.
const BOXES_WIDTH: f32 = 400.0;
/// From this width the footer shows the time of day, which a narrower one has no room for beside the session.
const TIME_OF_DAY_WIDTH: f32 = 380.0;
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

/// Estimated gap in seconds from the player to a car, positive when the car is ahead on track.
///
/// Distance decides the side. Estimated times are only comparable within a class, so they refine the gap for
/// same-class cars and only when they agree with the distance.
pub(crate) fn car_gap(car: (f32, f32), player: (f32, f32), lap: f32, same_class: bool) -> f32 {
    let ((car_pct, car_est), (player_pct, player_est)) = (car, player);
    let by_distance = normalize_gap((car_pct - player_pct) * lap, lap);
    if same_class && car_est > 0.0 && player_est > 0.0 {
        let by_estimate = normalize_gap(car_est - player_est, lap);
        if by_estimate.signum() == by_distance.signum() {
            return by_estimate;
        }
    }
    by_distance
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

/// The gap to show, positive when the car is ahead on track: the measured gap on track when the clock saw the
/// pass, else the estimate. `measured` is the time since the car ahead of the two was where the other is; a pass
/// older than a lap is stale.
pub(crate) fn chosen_gap(ahead: bool, measured: Option<f64>, estimate: f32, lap: f32) -> f32 {
    match measured.filter(|&g| (0.0..=f64::from(lap)).contains(&g)) {
        Some(g) if ahead => g as f32,
        Some(g) => -(g as f32),
        None => estimate,
    }
}

/// A gap as its size in seconds to the tenth, without a sign: the row's place shows the side.
fn write_gap(out: &mut FixedText, gap: f32) {
    out.clear();
    let _ = write!(out, "{:.1}", gap.abs());
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
    out_lap: bool,
    black_flag: bool,
    laps: Laps,
    class_color: u32,
    position: FixedText,
    number: FixedText,
    name: FixedText,
    /// License class letter and safety rating, drawn apart in their own colors and places.
    license_class: FixedText,
    license_rating: FixedText,
    license_color: u32,
    rating: FixedText,
    maker: FixedText,
    gap: FixedText,
}

impl Row {
    fn new() -> Self {
        Self {
            used: false,
            player: false,
            in_pit: false,
            out_lap: false,
            black_flag: false,
            laps: Laps::Same,
            class_color: 0,
            position: FixedText::new(4),
            number: FixedText::new(8),
            name: FixedText::new(64),
            license_class: FixedText::new(4),
            license_rating: FixedText::new(8),
            license_color: 0,
            rating: FixedText::new(8),
            maker: FixedText::new(4),
            gap: FixedText::new(12),
        }
    }
}

/// The lines above and below the cars.
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    track: FixedText,
    incidents: FixedText,
    clock: FixedText,
    lap: FixedText,
    time_of_day: FixedText,
    session: FixedText,
    session_time: FixedText,
}

impl Frame {
    fn new() -> Self {
        Self {
            track: FixedText::new(8),
            incidents: FixedText::new(8),
            clock: FixedText::new(8),
            lap: FixedText::new(8),
            time_of_day: FixedText::new(8),
            session: FixedText::new(24),
            session_time: FixedText::new(24),
        }
    }
}

/// Splits `D 2.83` into the class letter and `2.8`, without allocating.
fn split_license(license: &str, class: &mut FixedText, rating: &mut FixedText) {
    short_license(license, rating);
    let mut digits = [0u8; 8];
    let mut chars = rating.as_str().chars();
    let letter = chars.next();
    let rest = chars.as_str().as_bytes();
    let n = rest.len().min(digits.len());
    digits[..n].copy_from_slice(&rest[..n]);
    class.clear();
    if let Some(letter) = letter {
        let mut buf = [0u8; 4];
        class.set(letter.encode_utf8(&mut buf));
    }
    rating.set(std::str::from_utf8(&digits[..n]).unwrap_or(""));
}

/// Follows one car's out lap: it starts when the car leaves pit road and ends when the car completes a lap or is
/// on pit road again. `out_lap_of` is the completed-lap count during the out lap, or -1.
fn track_out_lap(on_pit: &mut bool, out_lap_of: &mut i32, pit: bool, laps: i32) {
    if *on_pit && !pit {
        *out_lap_of = laps;
    } else if pit || *out_lap_of != laps {
        *out_lap_of = -1;
    }
    *on_pit = pit;
}

/// The relative widget.
#[derive(Debug)]
pub struct RelativeWidget {
    width: u32,
    height: u32,
    opacity: f32,
    /// Cars shown on each side of the player.
    side: usize,
    /// The cars, farthest ahead first: `2 * side + 1` rows.
    rows: Vec<Row>,
    next: Vec<Row>,
    /// (lap distance from the player, gap, car index) of each row's car this update.
    order: Vec<Option<(f32, f32, usize)>>,
    frame: Frame,
    next_frame: Frame,
    /// (lap distance from the player, gap, car index) of cars on track this update; capacity grows to the car
    /// count once.
    cars: Vec<(f32, f32, usize)>,
    /// Per car: on pit road at the last update, and the completed-lap count at its last pit exit (an out lap
    /// lasts while the count is unchanged), or -1.
    on_pit: Vec<bool>,
    out_lap_of: Vec<i32>,
    /// Each car's passes around the lap, shared with the standings.
    clock: SharedTrackClock,
}

/// Cars shown on each side of the player in a widget `height` pixels tall: as many as fit between the header and
/// the footer, the same number on both sides.
fn cars_each_side(height: u32) -> usize {
    let slots = (height * ROWS_PER_100_PX / 100) as usize;
    slots.saturating_sub(3) / 2
}

impl RelativeWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        Self::with_clock(config, TrackClock::shared())
    }

    /// A relative widget timing gaps with `clock`, which other widgets may observe too.
    pub(crate) fn with_clock(config: &WidgetConfig, clock: SharedTrackClock) -> Self {
        let mut widget = Self {
            width: 0,
            height: 0,
            opacity: config.opacity,
            side: 0,
            rows: Vec::new(),
            next: Vec::new(),
            order: Vec::new(),
            frame: Frame::new(),
            next_frame: Frame::new(),
            cars: Vec::with_capacity(72),
            on_pit: Vec::with_capacity(72),
            out_lap_of: Vec::with_capacity(72),
            clock,
        };
        widget.resize(config.width.unwrap_or(460), config.height.unwrap_or(300));
        widget
    }

    /// Rows: the header, the cars, and the footer.
    fn slots(&self) -> usize {
        self.rows.len() + 2
    }

    fn fill_frame(&mut self, view: &View) {
        let (snap, vars) = (view.snapshot(), view.vars());
        let f = &mut self.next_frame;
        for text in [
            &mut f.track,
            &mut f.incidents,
            &mut f.clock,
            &mut f.lap,
            &mut f.time_of_day,
            &mut f.session,
            &mut f.session_time,
        ] {
            text.clear();
        }
        if let Some(t) = snap.f32(vars.track_temp, 0) {
            let _ = write!(f.track, "{t:.1}");
        }
        if let Some(n) = snap.i32(vars.incidents, 0) {
            let _ = write!(f.incidents, "{n}");
        }
        let now = platform::local_time();
        let _ = write!(f.clock, "{}:{:02}", now.hour, now.minute);
        if let Some(lap) = snap.i32(vars.lap, 0) {
            let _ = write!(f.lap, "{}", lap.max(0));
        }
        if let Some(t) = snap.f32(vars.time_of_day, 0).filter(|t| *t >= 0.0) {
            let minutes = (t / 60.0) as u32;
            let _ = write!(f.time_of_day, "{}:{:02}", minutes / 60 % 24, minutes % 60);
        }
        let session_num = snap.i32(vars.session_num, 0).map(i64::from);
        let session = view
            .session()
            .and_then(|info| info.sessions.iter().find(|s| s.num == session_num));
        if let Some(name) = session.and_then(|s| s.session_type.as_deref()) {
            f.session.set(name);
        }
        let elapsed = snap.f64(vars.session_time, 0).unwrap_or(0.0);
        write_clock(&mut f.session_time, elapsed);
        if let Some(total) = session
            .and_then(|s| s.time_seconds)
            .filter(|&t| t > 0.0 && t < 1.0e6)
        {
            f.session_time.push("/");
            write_length(&mut f.session_time, total);
        }
    }

    fn fill(&mut self, view: &View) {
        for row in &mut self.next {
            row.used = false;
        }
        self.fill_frame(view);
        let snap = view.snapshot();
        let vars = view.vars();
        let session = view.session();
        let count = snap.count(vars.car_lap_dist_pct);
        if self.on_pit.len() != count {
            self.on_pit.clear();
            self.on_pit.resize(count, false);
            self.out_lap_of.clear();
            self.out_lap_of.resize(count, -1);
        }
        // Out laps: from leaving pit road until the next completed lap.
        for car in 0..count {
            track_out_lap(
                &mut self.on_pit[car],
                &mut self.out_lap_of[car],
                snap.bool(vars.car_on_pit_road, car).unwrap_or(false),
                snap.i32(vars.car_lap_completed, car).unwrap_or(-1),
            );
        }
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
        let on_pit = |i: usize| snap.bool(vars.car_on_pit_road, i).unwrap_or(false);
        let (player_pct, player_est) = (pct(player), est(player));
        if player_pct < 0.0 {
            return;
        }
        if let Ok(mut clock) = self.clock.try_borrow_mut() {
            clock.observe(view);
        }
        let shared = Rc::clone(&self.clock);
        let Ok(clock) = shared.try_borrow() else {
            return;
        };
        let clock = clock.clock();
        self.cars.clear();
        for i in 0..count {
            if i == player || pct(i) < 0.0 {
                continue;
            }
            if snap.i32(vars.car_track_surface, i) == Some(NOT_IN_WORLD) {
                continue;
            }
            if driver(i).is_some_and(|d| d.is_pace_car || d.is_spectator) {
                continue;
            }
            let class_of = |idx: usize| {
                snap.i32(vars.car_class, idx)
                    .map(i64::from)
                    .or_else(|| driver(idx).and_then(|d| d.car_class_id))
            };
            let same_class = class_of(i) == class_of(player);
            let estimate = car_gap((pct(i), est(i)), (player_pct, player_est), lap, same_class);
            let distance = normalize_gap(pct(i) - player_pct, 1.0);
            let ahead = distance > 0.0;
            // Pit road does not follow the track's speeds, so a pass there says little.
            let measured = if on_pit(i) || on_pit(player) {
                None
            } else if ahead {
                clock.track_gap(i, player)
            } else {
                clock.track_gap(player, i)
            };
            let gap = chosen_gap(ahead, measured, estimate, lap);
            self.cars.push((distance, gap, i));
        }
        // Nearest ahead (smallest positive distances) and nearest behind (largest negative distances).
        self.cars.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let split = self
            .cars
            .partition_point(|&(distance, _, _)| distance <= 0.0);
        let side = self.side;
        let behind = self.cars[..split].iter().rev().take(side);
        let ahead = self.cars[split..].iter().take(side);
        let lap_of = |i: usize| snap.i32(vars.car_lap, i).unwrap_or(0);
        // Rows: farthest ahead at the top, the player in the middle, farthest behind at the bottom.
        let slots = &mut self.order;
        slots.iter_mut().for_each(|slot| *slot = None);
        for (n, &car) in ahead.enumerate() {
            slots[side - 1 - n] = Some(car);
        }
        slots[side] = Some((0.0, 0.0, player));
        for (n, &car) in behind.enumerate() {
            slots[side + 1 + n] = Some(car);
        }
        for (row, &slot) in self.next.iter_mut().zip(slots.iter()) {
            let Some((distance, gap, i)) = slot else {
                continue;
            };
            let info = driver(i);
            row.used = true;
            row.player = i == player;
            row.in_pit = snap.bool(vars.car_on_pit_road, i).unwrap_or(false);
            row.out_lap = self.out_lap_of.get(i).is_some_and(|&l| l >= 0);
            row.black_flag = snap
                .bitfield(vars.car_session_flags, i)
                .is_some_and(|f| f & session_flags::BLACK != 0);
            row.laps = match lap_difference(lap_of(i), pct(i), lap_of(player), player_pct, distance)
            {
                d if d > 0 && !row.player => Laps::Ahead,
                d if d < 0 && !row.player => Laps::Behind,
                _ => Laps::Same,
            };
            row.class_color = info.and_then(|d| d.car_class_color).unwrap_or(0xFF_FFFF);
            row.position.clear();
            if let Some(p) = snap.i32(vars.car_class_position, i).filter(|&p| p > 0) {
                let _ = write!(row.position, "{p}");
            }
            row.number.clear();
            let _ = write!(
                row.number,
                "#{}",
                info.and_then(|d| d.car_number.as_deref()).unwrap_or("?")
            );
            row.name
                .set(info.and_then(|d| d.user_name.as_deref()).unwrap_or(""));
            split_license(
                info.and_then(|d| d.license.as_deref()).unwrap_or(""),
                &mut row.license_class,
                &mut row.license_rating,
            );
            row.license_color = info.and_then(|d| d.license_color).unwrap_or(0x80_8080);
            row.rating.clear();
            if let Some(rating) = info.and_then(|d| d.irating).filter(|&r| r > 0) {
                let _ = write!(row.rating, "{:.1}k", rating as f32 / 1000.0);
            }
            row.maker.clear();
            if let Some(letter) = info
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
            row.gap.clear();
            if !row.player {
                write_gap(&mut row.gap, gap);
            }
        }
    }

    fn row_height(&self) -> f32 {
        self.height as f32 / self.slots() as f32
    }

    fn draw_frame_line(&self, canvas: &mut Canvas<'_>, y: f32, top: bool) {
        let (w, row_h) = (self.width as f32, self.row_height());
        let size = FRAME_TEXT;
        let dim = Color::rgb(DIM, 1.0);
        let white = Color::rgb(WHITE, 1.0);
        let f = &self.frame;
        let cell = |x: f32, width: f32| Rect::new(x, y, width, row_h);
        if top {
            let mut x = 10.0;
            let mut pair = |canvas: &mut Canvas<'_>, label: &str, value: &str, unit: &str, width: f32| {
                if value.is_empty() {
                    return;
                }
                canvas.text(label, cell(x, 40.0), size * 0.85, Align::Left, dim);
                canvas.text(value, cell(x + 40.0, width), size, Align::Right, white);
                canvas.text(unit, cell(x + 41.0 + width, 20.0), size * 0.85, Align::Left, dim);
                x += 70.0 + width;
            };
            // Wide enough for a temperature with one decimal (`58.3`) at the largest text size.
            pair(canvas, "Track", f.track.as_str(), "\u{b0}C", 36.0);
            pair(canvas, "Inc", f.incidents.as_str(), "x", 26.0);
            canvas.text(
                f.clock.as_str(),
                cell(w - 70.0, 60.0),
                size,
                Align::Right,
                white,
            );
        } else {
            canvas.text("Lap", cell(10.0, 30.0), size * 0.85, Align::Left, dim);
            canvas.text(f.lap.as_str(), cell(40.0, 30.0), size, Align::Left, white);
            if w >= TIME_OF_DAY_WIDTH {
                canvas.text(
                    f.time_of_day.as_str(),
                    cell(w / 2.0 - 60.0, 60.0),
                    size,
                    Align::Right,
                    white,
                );
            }
            canvas.text(
                f.session.as_str(),
                cell(w - 220.0, 90.0),
                size * 0.85,
                Align::Right,
                dim,
            );
            canvas.text(
                f.session_time.as_str(),
                cell(w - 126.0, 116.0),
                size,
                Align::Right,
                white,
            );
        }
    }

    /// A row's iRating box at `rating_x` and license box at `license_x`.
    fn draw_boxes(
        &self,
        canvas: &mut Canvas<'_>,
        row: &Row,
        (rating_x, license_x): (f32, f32),
        y: f32,
        alpha: f32,
    ) {
        let row_h = self.row_height();
        let small = ROW_TEXT * 0.78;
        let badge_y = y + (row_h - BADGE_HEIGHT) / 2.0;
        let cell = |x: f32, width: f32| Rect::new(x, y, width, row_h);
        canvas.fill(
            Rect::new(rating_x + 2.0, badge_y, 40.0, BADGE_HEIGHT),
            Color::rgb(0xE8_EAED, 0.9 * alpha),
        );
        canvas.text(
            row.rating.as_str(),
            cell(rating_x + 2.0, 40.0),
            small,
            Align::Center,
            Color::rgb(BACKGROUND, alpha),
        );
        if !row.license_class.as_str().is_empty() {
            canvas.fill(
                Rect::new(license_x, badge_y, 40.0, BADGE_HEIGHT),
                Color::rgb(row.license_color, 0.9 * alpha),
            );
            let ink = Color::rgb(0xFF_FFFF, alpha);
            canvas.text(
                row.license_class.as_str(),
                cell(license_x + 3.0, 12.0),
                small,
                Align::Left,
                ink,
            );
            canvas.text(
                row.license_rating.as_str(),
                cell(license_x + 14.0, 24.0),
                small,
                Align::Left,
                ink,
            );
        }
    }

    fn draw_row(&self, canvas: &mut Canvas<'_>, y: f32, row: &Row) {
        let (w, row_h) = (self.width as f32, self.row_height());
        let size = ROW_TEXT;
        // Badges and boxes keep their height, centered in the row.
        let badge_y = y + (row_h - BADGE_HEIGHT) / 2.0;
        let small = size * 0.78;
        let cell = |x: f32, width: f32| Rect::new(x, y, width, row_h);
        if row.player {
            canvas.fill(Rect::new(0.0, y, w, row_h), Color::rgb(0xFF_FFFF, 0.10));
        }
        let base = match row.laps {
            Laps::Ahead => 0xFF_8A80,
            Laps::Behind => 0x82_B1FF,
            Laps::Same if row.player => 0xFF_B347,
            Laps::Same => WHITE,
        };
        let alpha = if row.in_pit { 0.45 } else { 1.0 };
        let text = Color::rgb(base, alpha);
        let dim = Color::rgb(DIM, alpha);
        canvas.text(
            row.position.as_str(),
            cell(6.0, 22.0),
            size,
            Align::Right,
            text,
        );
        canvas.text(
            row.number.as_str(),
            cell(32.0, 42.0),
            size,
            Align::Right,
            Color::rgb(row.class_color, alpha),
        );
        // Right side, from the edge: gap, maker, iRating, license, then the badges. A narrow widget leaves out the
        // iRating and the license.
        let gap_x = w - 58.0;
        let maker_x = gap_x - 18.0;
        let rating_x = maker_x - 44.0;
        let license_x = rating_x - 44.0;
        let boxes = w >= BOXES_WIDTH;
        canvas.text(
            row.gap.as_str(),
            cell(gap_x, 52.0),
            size,
            Align::Right,
            text,
        );
        canvas.text(
            row.maker.as_str(),
            cell(maker_x, 16.0),
            small,
            Align::Center,
            dim,
        );
        if boxes {
            self.draw_boxes(canvas, row, (rating_x, license_x), y, alpha);
        }
        let mut badge_x = if boxes { license_x } else { maker_x } - 4.0;
        let mut badge =
            |canvas: &mut Canvas<'_>, label: &str, fill: u32, ink: u32, border: Option<u32>| {
                let width = 12.0 + 8.0 * label.chars().count() as f32;
                badge_x -= width + 4.0;
                let rect = Rect::new(badge_x, badge_y, width, BADGE_HEIGHT);
                canvas.fill(rect, Color::rgb(fill, 1.0));
                if let Some(border) = border {
                    let (l, t, r, b) = (rect.x, rect.y, rect.x + rect.w, rect.y + rect.h);
                    let c = Color::rgb(border, 1.0);
                    canvas.line((l, t), (r, t), 1.0, c);
                    canvas.line((l, b), (r, b), 1.0, c);
                    canvas.line((l, t), (l, b), 1.0, c);
                    canvas.line((r, t), (r, b), 1.0, c);
                }
                canvas.text(
                    label,
                    cell(badge_x, width),
                    small,
                    Align::Center,
                    Color::rgb(ink, 1.0),
                );
            };
        if row.in_pit {
            badge(canvas, "PIT", 0x2D_6CDF, 0xFF_FFFF, None);
        } else if row.out_lap {
            badge(canvas, "OUT", 0xF2_C14E, BACKGROUND, None);
        }
        if row.black_flag {
            badge(canvas, "F", 0x00_0000, 0xFF_FFFF, Some(0xFF_FFFF));
        }
        canvas.text(
            row.name.as_str(),
            cell(82.0, (badge_x - 86.0).max(0.0)),
            size,
            Align::Left,
            text,
        );
    }
}

const WHITE: u32 = 0xF0_F2F5;
const DIM: u32 = 0xA0_A6AE;
const BACKGROUND: u32 = 0x10_1418;

impl Register for RelativeWidget {
    type Vars = ();
    type Options = NoOptions;

    fn build(cx: &Build<'_>, _: ()) -> Self {
        Self::with_clock(cx.config, cx.shared.clock())
    }
}

impl Widget for RelativeWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn min_size(&self) -> (u32, u32) {
        MIN_SIZE
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(MIN_SIZE.0);
        self.height = height.max(MIN_SIZE.1);
        self.side = cars_each_side(self.height);
        let rows = 2 * self.side + 1;
        for list in [&mut self.rows, &mut self.next] {
            list.truncate(rows);
            list.resize_with(rows, Row::new);
            // Filled again at the next update.
            list.iter_mut().for_each(|row| row.used = false);
        }
        self.order.clear();
        self.order.resize(rows, None);
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(100)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        self.fill(view);
        // Slots: the header, the cars, the footer. Only the span that changed is drawn again.
        let header = self.next_frame.track != self.frame.track
            || self.next_frame.incidents != self.frame.incidents
            || self.next_frame.clock != self.frame.clock;
        let footer = self.next_frame != self.frame && {
            let (a, b) = (&self.next_frame, &self.frame);
            a.lap != b.lap
                || a.time_of_day != b.time_of_day
                || a.session != b.session
                || a.session_time != b.session_time
        };
        let first_row = self.rows.iter().zip(&self.next).position(|(a, b)| a != b);
        let last_row = self.rows.iter().zip(&self.next).rposition(|(a, b)| a != b);
        let first = if header {
            Some(0)
        } else {
            first_row.map(|r| r + 1)
        };
        let last = if footer {
            Some(self.slots() - 1)
        } else {
            last_row.map(|r| r + 1)
        };
        let (Some(first), Some(last)) = (first.or(last), last.or(first)) else {
            return;
        };
        std::mem::swap(&mut self.rows, &mut self.next);
        std::mem::swap(&mut self.frame, &mut self.next_frame);
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
        canvas.fill_background(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(BACKGROUND, self.opacity),
        );
        let row_h = self.row_height();
        let area = canvas.area();
        let visible = |slot: usize| {
            let y = slot as f32 * row_h;
            y + row_h > area.y && y < area.y + area.h
        };
        if visible(0) {
            self.draw_frame_line(canvas, 0.0, true);
        }
        for (n, row) in self.rows.iter().enumerate() {
            if row.used && visible(n + 1) {
                self.draw_row(canvas, (n + 1) as f32 * row_h, row);
            }
        }
        let footer = self.slots() - 1;
        if visible(footer) {
            self.draw_frame_line(canvas, footer as f32 * row_h, false);
        }
    }

    fn reset(&mut self) {
        for row in self.rows.iter_mut().chain(self.next.iter_mut()) {
            row.used = false;
        }
        self.on_pit.iter_mut().for_each(|p| *p = false);
        self.out_lap_of.iter_mut().for_each(|l| *l = -1);
        if let Ok(mut clock) = self.clock.try_borrow_mut() {
            clock.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_laps_run_from_pit_exit_to_the_line() {
        let (mut pit, mut out) = (false, -1);
        track_out_lap(&mut pit, &mut out, false, 3);
        assert_eq!(out, -1, "driving");
        track_out_lap(&mut pit, &mut out, true, 3);
        assert_eq!(out, -1, "on pit road");
        track_out_lap(&mut pit, &mut out, false, 3);
        assert_eq!(out, 3, "left the pits");
        track_out_lap(&mut pit, &mut out, false, 3);
        assert_eq!(out, 3, "still the out lap");
        track_out_lap(&mut pit, &mut out, false, 4);
        assert_eq!(out, -1, "crossed the line");
    }

    #[test]
    fn licenses_split_into_class_and_rating() {
        let (mut class, mut rating) = (FixedText::new(4), FixedText::new(8));
        split_license("A 1.83", &mut class, &mut rating);
        assert_eq!((class.as_str(), rating.as_str()), ("A", "1.8"));
        split_license("", &mut class, &mut rating);
        assert_eq!((class.as_str(), rating.as_str()), ("", ""));
    }

    #[test]
    fn gaps_show_without_a_sign() {
        let mut out = FixedText::new(12);
        write_gap(&mut out, 1.43);
        assert_eq!(out.as_str(), "1.4", "ahead");
        write_gap(&mut out, -2.0);
        assert_eq!(out.as_str(), "2.0", "behind");
        write_gap(&mut out, -0.04);
        assert_eq!(out.as_str(), "0.0");
    }

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
    fn other_classes_are_placed_by_distance() {
        // Captured case: a faster-class car ahead on track has a lower estimated time than the player.
        let player = (0.2257, 26.29);
        let car = (0.2472, 26.01);
        let gap = car_gap(car, player, 140.0, false);
        assert!(gap > 2.9 && gap < 3.1, "ahead by distance: {gap}");
        assert_eq!(lap_difference(1, car.0, 1, player.0, gap), 0, "same lap");
        // The same numbers for a same-class car disagree with the distance, so distance wins there too.
        assert!(car_gap(car, player, 140.0, true) > 0.0);
    }

    #[test]
    fn same_class_cars_use_estimated_time() {
        let gap = car_gap((0.2510, 26.38), (0.2257, 26.29), 140.0, true);
        assert!((gap - 0.09).abs() < 0.001, "{gap}");
        // Across the line: car just behind the player who has just crossed.
        let gap = car_gap((0.99, 138.6), (0.01, 1.4), 140.0, true);
        assert!(gap < 0.0 && gap > -3.0, "{gap}");
    }

    #[test]
    fn measured_gaps_win_unless_missing_or_stale() {
        assert_eq!(chosen_gap(true, Some(1.25), 1.0, 90.0), 1.25);
        assert_eq!(chosen_gap(false, Some(2.5), -2.0, 90.0), -2.5);
        assert_eq!(chosen_gap(true, None, 1.0, 90.0), 1.0, "not seen yet");
        assert_eq!(
            chosen_gap(false, Some(95.0), -2.0, 90.0),
            -2.0,
            "older than a lap"
        );
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

/// The widget against a simulated sim: telemetry through the real pipeline.
#[cfg(all(test, windows))]
mod pipeline_tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use pitot_irsdk::layout::{VarHeader, VarType};
    use pitot_irsdk::live::{ReaderConfig, SdkNames};
    use pitot_irsdk::publish::{Publisher, PublisherConfig};

    use super::*;
    use crate::pipeline::Pipeline;
    use crate::widgets::StandingsWidget;

    const CARS: usize = 5;
    /// Per-car arrays of four bytes from byte 16, then the pit road flags (one byte each).
    const PIT_AT: usize = 16 + 5 * CARS * 4;
    const ROW: usize = PIT_AT + CARS;

    /// A practice session: the sim estimates the player's lap at 90 s, the cars do 100 s laps.
    const SESSION: &str = "---
WeekendInfo:
 TrackID: 7
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionLaps: unlimited
   SessionTime: 3600.0000 sec
   SessionType: Practice
DriverInfo:
 DriverCarIdx: 1
 DriverCarEstLapTime: 90.0000
 Drivers:
 - CarIdx: 0
   CarNumber: 0
   CarIsPaceCar: 1
 - CarIdx: 1
   CarNumber: 1
   CarClassID: 10
 - CarIdx: 2
   CarNumber: 2
   CarClassID: 10
 - CarIdx: 3
   CarNumber: 3
   CarClassID: 11
 - CarIdx: 4
   CarNumber: 4
   CarClassID: 10
...
";

    /// At session time `t` on 100 s laps: the player (car 1) at 2.30 laps at 0 s, car 2 3 s ahead, car 3 (another
    /// class) 5 s behind, car 4 10 s ahead on track but a lap down. Car 2 is on pit road when `car_2_in_pit`.
    fn row(t: f64, car_2_in_pit: bool) -> Vec<u8> {
        let mut row = vec![0u8; ROW];
        row[0..8].copy_from_slice(&t.to_le_bytes());
        row[12..16].copy_from_slice(&1i32.to_le_bytes());
        let put = |row: &mut Vec<u8>, k: usize, car: usize, bytes: [u8; 4]| {
            let at = 16 + k * CARS * 4 + car * 4;
            row[at..at + 4].copy_from_slice(&bytes);
        };
        let starts = [2.80, 2.30, 2.33, 2.25, 1.40];
        let best = [0.0f32, 100.0, 99.5, 100.4, 101.0];
        let class_position = [0i32, 2, 1, 1, 3];
        for (car, start) in starts.into_iter().enumerate() {
            let progress = start + t / 100.0;
            let laps = progress.floor();
            put(&mut row, 0, car, ((progress - laps) as f32).to_le_bytes());
            put(&mut row, 1, car, (laps as i32).to_le_bytes());
            put(&mut row, 2, car, 3i32.to_le_bytes());
            put(&mut row, 3, car, best[car].to_le_bytes());
            put(&mut row, 4, car, class_position[car].to_le_bytes());
        }
        row[PIT_AT + 2] = u8::from(car_2_in_pit);
        row
    }

    fn publisher(names: &SdkNames) -> Publisher {
        let array = |t, k: usize, name| {
            VarHeader::new(t, (16 + k * CARS * 4) as i32, CARS as i32, name).expect("valid")
        };
        let config = PublisherConfig {
            vars: vec![
                VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
                VarHeader::new(VarType::Int, 8, 1, "SessionNum").expect("valid"),
                VarHeader::new(VarType::Int, 12, 1, "PlayerCarIdx").expect("valid"),
                array(VarType::Float, 0, "CarIdxLapDistPct"),
                array(VarType::Int, 1, "CarIdxLapCompleted"),
                array(VarType::Int, 2, "CarIdxTrackSurface"),
                array(VarType::Float, 3, "CarIdxBestLapTime"),
                array(VarType::Int, 4, "CarIdxClassPosition"),
                VarHeader::new(VarType::Bool, PIT_AT as i32, CARS as i32, "CarIdxOnPitRoad")
                    .expect("valid"),
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

    /// "number gap" of the rows shown, top to bottom.
    fn shown(widget: &RelativeWidget) -> Vec<String> {
        widget
            .next
            .iter()
            .filter(|r| r.used)
            .map(|r| format!("{} {}", r.number.as_str(), r.gap.as_str()))
            .collect()
    }

    #[test]
    fn gaps_are_measured_on_track_once_the_passes_are_seen() {
        let pid = std::process::id();
        let names = SdkNames::new(
            &format!("Local\\PitotRelativeMap-{pid}"),
            &format!("Local\\PitotRelativeEvent-{pid}"),
        );
        let mut publisher = publisher(&names);
        let reader = ReaderConfig {
            retry_interval: Duration::from_millis(5),
            idle_timeout: Duration::from_millis(50),
        };
        let (_pipeline, mut view) =
            Pipeline::start(names, reader, Vec::new(), None).expect("starts");
        // Publishes until the view shows this moment.
        let mut step = |view: &mut View, t: f64, car_2_in_pit: bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "the view reaches {t}");
                publisher.publish(&row(t, car_2_in_pit)).expect("publishes");
                thread::sleep(Duration::from_millis(2));
                view.poll();
                let at = view.snapshot().f64(view.vars().session_time, 0);
                if view.session().is_some() && at == Some(t) {
                    break;
                }
            }
        };
        let config = |width, height| WidgetConfig {
            enabled: true,
            monitor: None,
            x: 0,
            y: 0,
            width: Some(width),
            height: Some(height),
            rate_hz: None,
            opacity: 0.8,
            options: toml::Table::new(),
        };
        // One clock for both widgets, as the overlay builds them.
        let clock = TrackClock::shared();
        let mut relative = RelativeWidget::with_clock(&config(460, 300), Rc::clone(&clock));
        let mut standings = StandingsWidget::with_clock(&config(760, 240), Rc::clone(&clock));

        // Nothing seen yet: the estimate, the distance times the 90 s lap.
        step(&mut view, 1000.0, false);
        relative.fill(&view);
        assert_eq!(shown(&relative), ["#4 9.0", "#2 2.7", "#1 ", "#3 4.5"]);

        // At 10 Hz, with the standings at 2 Hz on the same clock: after car 4 has passed the player's spot, every
        // gap is measured. Car 4 is a lap down, which does not matter on track; car 3 is in another class.
        let mut t = 1000.0;
        for n in 1..=120 {
            t = 1000.0 + f64::from(n) * 0.1;
            step(&mut view, t, false);
            relative.fill(&view);
            if n % 5 == 0 {
                standings.fill(&view);
            }
        }
        assert_eq!(shown(&relative), ["#4 10.0", "#2 3.0", "#1 ", "#3 5.0"]);
        // Practice standings keep the best-lap gaps on the shared clock: car 1 0.5 s and car 4 1.5 s off car 2's
        // best lap.
        let gaps = standings.filled_gaps();
        let gap_of = |number: &str| {
            gaps.iter()
                .find(|(n, _)| n == number)
                .map(|(_, g)| g.as_str())
        };
        assert_eq!(
            (gap_of("1"), gap_of("4")),
            (Some("0.5"), Some("1.5")),
            "{gaps:?}"
        );

        // On pit road the pass says little: back to the estimate for that car.
        step(&mut view, t + 0.1, true);
        relative.fill(&view);
        assert_eq!(shown(&relative)[1], "#2 2.7");
    }
}
