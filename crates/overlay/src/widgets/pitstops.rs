//! Pit stops of every car, and what a stop costs.
//!
//! [`PitStops`] follows each car at the caller's rate: when it enters and leaves pit road, and how long it stands
//! still in its pit stall. A visit counts as a stop when the car comes off the track onto pit road and leaves pit
//! road back onto the track. Appearing on pit road from the garage, or leaving the world from the stall, is not a
//! stop.
//!
//! [`PitModel`] turns stops into estimates: the most fuel a standing time allows, and the time a stop costs (pit
//! lane loss plus service). [`PitTracker`] bundles the stops with the refueling speed measured at the player's own
//! stops and the values remembered between sessions (`pit.toml` in the configuration directory), so a widget needs
//! one call per update.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use pitot_irsdk::live::Snapshot;
use pitot_irsdk::session::SessionInfo;
use serde::Deserialize;

use crate::pipeline::{Vars, View};

/// `CarIdxTrackSurface` value for a car standing in or entering its pit stall.
pub(crate) const IN_PIT_STALL: i32 = 1;
/// `CarIdxTrackSurface` value for a car that is not in the world.
const NOT_IN_WORLD: i32 = -1;

/// Standing time of a stop that is not refueling: from stopping until the fuel flows, and from the last liter until
/// the crew is done. A fuel-only stop in the home PC capture (FIA F4): 1.0 s before and 2.0 s after.
pub(crate) const SERVICE_OVERHEAD: f32 = 3.0;
/// Refueling speed in liters per second until a stop of the player's measures it: 26.7 L in 2.5 s at that stop.
pub(crate) const DEFAULT_REFUEL_RATE: f32 = 10.7;

/// Lap distance a standing car does not change by between observations.
const STILL_PCT: f32 = 1.0e-6;
/// A longer pause between observations (the widget was hidden) or time going back restarts the following.
const MAX_STEP: f64 = 2.0;
/// Pit lane losses kept for the median, over all cars.
const LOSSES: usize = 9;
/// Pit lane losses outside this range are not counted (a tow, a long repair, a timing gap).
const LOSS_RANGE: std::ops::RangeInclusive<f32> = 0.0..=120.0;

/// One finished pit stop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Stop {
    /// Laps the car had completed when it entered pit road.
    pub(crate) lap: i32,
    /// Session time at which it left pit road.
    pub(crate) left_at: f64,
    /// Time on pit road, from entry to exit.
    pub(crate) lane_time: f32,
    /// Time standing still in the pit stall.
    pub(crate) stationary: f32,
    /// This stop's pit lane loss: lane time, minus standing time, minus the car's time for the same distance at its
    /// pace. `None` when the pace was not known or the result was out of range.
    pub(crate) loss: Option<f32>,
}

/// A visit to pit road that started from the track.
#[derive(Debug, Clone, Copy)]
struct Visit {
    entered_at: f64,
    entered_pct: f32,
    lap: i32,
    /// Session time of the observation where the current standing run began, or NaN while moving.
    still_from: f64,
    /// Session time of the run's last standing observation.
    still_last: f64,
    stationary: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct Follow {
    seen: bool,
    on_pit: bool,
    surface: i32,
    pct: f32,
    visit: Option<Visit>,
    last: Option<Stop>,
    stops: u16,
}

/// Follows every car's pit stops. Call [`PitStops::observe`] regularly, a few times per second or more: the standing
/// time is measured to about one observation interval.
#[derive(Debug, Default)]
pub(crate) struct PitStops {
    cars: Vec<Follow>,
    last_time: Option<f64>,
    losses: [f32; LOSSES],
    loss_count: usize,
    loss_next: usize,
}

impl PitStops {
    /// Feeds one observation of every car: whether it is on pit road, its track surface, lap distance, completed
    /// laps, and its pace (lap time) for the lane loss. `time` is the session time.
    pub(crate) fn observe(
        &mut self,
        time: f64,
        cars: usize,
        mut car: impl FnMut(usize) -> CarSample,
    ) {
        if self.cars.len() != cars {
            self.cars.clear();
            self.cars.resize(cars, Follow::default());
        }
        let step = self.last_time.map(|last| time - last);
        self.last_time = Some(time);
        if step.is_none_or(|s| !(0.0..=MAX_STEP).contains(&s)) {
            // First observation, or a pause or jump: what happened in between is unknown.
            for f in &mut self.cars {
                f.seen = false;
                f.visit = None;
            }
        }
        let step = step.unwrap_or(0.0).clamp(0.0, MAX_STEP);
        for (i, f) in self.cars.iter_mut().enumerate() {
            let s = car(i);
            if !f.seen {
                // A car first seen on pit road (or in the garage) is not followed until it leaves.
                *f = Follow {
                    seen: true,
                    on_pit: s.on_pit,
                    surface: s.surface,
                    pct: s.pct,
                    visit: None,
                    ..*f
                };
                continue;
            }
            if s.on_pit && !f.on_pit {
                let from_track =
                    f.surface != NOT_IN_WORLD && f.pct >= 0.0 && s.surface != NOT_IN_WORLD;
                f.visit = from_track.then_some(Visit {
                    entered_at: time,
                    entered_pct: s.pct,
                    lap: s.laps,
                    still_from: f64::NAN,
                    still_last: f64::NAN,
                    stationary: 0.0,
                });
            }
            if s.on_pit
                && let Some(v) = &mut f.visit
            {
                let still =
                    s.surface == IN_PIT_STALL && s.pct >= 0.0 && (s.pct - f.pct).abs() < STILL_PCT;
                if still {
                    if v.still_from.is_nan() {
                        // It was standing at the previous observation already.
                        v.still_from = time - step;
                    }
                    v.still_last = time;
                } else {
                    close_run(v, step);
                }
            }
            if !s.on_pit
                && f.on_pit
                && let Some(mut v) = f.visit.take()
                && s.surface != NOT_IN_WORLD
                && s.pct >= 0.0
            {
                close_run(&mut v, step);
                let lane_time = (time - v.entered_at) as f32;
                let stationary = v.stationary as f32;
                let span = (s.pct - v.entered_pct).rem_euclid(1.0);
                let loss = s
                    .pace
                    .filter(|&p| p > 1.0)
                    .map(|pace| lane_time - stationary - span * pace)
                    .filter(|l| LOSS_RANGE.contains(l));
                f.last = Some(Stop {
                    lap: v.lap,
                    left_at: time,
                    lane_time,
                    stationary,
                    loss,
                });
                f.stops = f.stops.saturating_add(1);
                if let Some(loss) = loss {
                    self.losses[self.loss_next] = loss;
                    self.loss_next = (self.loss_next + 1) % LOSSES;
                    self.loss_count = (self.loss_count + 1).min(LOSSES);
                }
            }
            if s.surface == NOT_IN_WORLD {
                f.visit = None;
            }
            f.on_pit = s.on_pit;
            f.surface = s.surface;
            f.pct = s.pct;
        }
    }

    /// The car's last finished stop.
    pub(crate) fn last_stop(&self, car: usize) -> Option<Stop> {
        self.cars.get(car)?.last
    }

    /// Finished stops of the car since following began.
    pub(crate) fn stops(&self, car: usize) -> u16 {
        self.cars.get(car).map_or(0, |f| f.stops)
    }

    /// Median pit lane loss of the recent stops of all cars.
    pub(crate) fn measured_loss(&self) -> Option<f32> {
        if self.loss_count == 0 {
            return None;
        }
        let mut sorted = [0.0f32; LOSSES];
        let n = self.loss_count;
        sorted[..n].copy_from_slice(&self.losses[..n]);
        sorted[..n].sort_unstable_by(f32::total_cmp);
        Some(if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        })
    }

    /// Forgets every car's stops (a new session), keeping the lane losses (same track).
    pub(crate) fn clear_stops(&mut self) {
        for f in &mut self.cars {
            *f = Follow::default();
        }
        self.last_time = None;
    }

    /// Forgets everything (a new connection or track).
    pub(crate) fn clear(&mut self) {
        self.clear_stops();
        self.loss_count = 0;
        self.loss_next = 0;
    }
}

/// Adds an open standing run to the visit's total. A run seen from observation `a` to `b` stood from somewhere in
/// the step before `a` to somewhere in the step after `b`: on average one step longer.
fn close_run(v: &mut Visit, step: f64) {
    if !v.still_from.is_nan() {
        v.stationary += v.still_last - v.still_from + step;
        v.still_from = f64::NAN;
    }
}

/// One car at one observation, as [`PitStops::observe`] needs it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CarSample {
    pub(crate) on_pit: bool,
    pub(crate) surface: i32,
    pub(crate) pct: f32,
    pub(crate) laps: i32,
    /// The car's lap time at its pace, for the lane loss.
    pub(crate) pace: Option<f32>,
}

/// Measures the refueling speed at the player's stops: the fuel level's rise per second while in the pit stall.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RefuelMeter {
    prev: Option<f32>,
    first: Option<(f64, f32)>,
    /// The last two rising observations: the very last may have reached the end of the fill between observations.
    before_last: Option<(f64, f32)>,
    last: Option<(f64, f32)>,
    rises: u32,
}

impl RefuelMeter {
    /// Feeds one observation. Returns the speed in liters per second when a fill has just ended.
    pub(crate) fn observe(&mut self, in_stall: bool, time: f64, fuel: f32) -> Option<f32> {
        let rising = in_stall && self.prev.is_some_and(|p| fuel > p + 0.01);
        self.prev = in_stall.then_some(fuel);
        if rising {
            self.first.get_or_insert((time, fuel));
            self.before_last = self.last;
            self.last = Some((time, fuel));
            self.rises += 1;
            return None;
        }
        let measured = match (self.first, self.before_last) {
            (Some((t0, f0)), Some((t1, f1))) if self.rises >= 3 && t1 - t0 >= 0.3 => {
                Some((f1 - f0) / (t1 - t0) as f32).filter(|r| (0.1..=100.0).contains(r))
            }
            _ => None,
        };
        if self.rises > 0 {
            *self = Self {
                prev: self.prev,
                ..Self::default()
            };
        }
        measured
    }
}

/// What a stop costs and what a standing time allows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PitModel {
    /// Seconds lost by driving pit lane instead of the track, without standing still. `None` until measured at this
    /// track.
    pub(crate) lane_loss: Option<f32>,
    /// Refueling speed in liters per second.
    pub(crate) refuel_rate: f32,
    /// The speed was measured at a stop of the player's car, not the default.
    pub(crate) rate_measured: bool,
    /// Usable tank of the player's car in liters.
    pub(crate) tank: Option<f32>,
}

impl Default for PitModel {
    fn default() -> Self {
        Self {
            lane_loss: None,
            refuel_rate: DEFAULT_REFUEL_RATE,
            rate_measured: false,
            tank: None,
        }
    }
}

impl PitModel {
    /// Standing time of a stop that adds `fuel` liters (and does nothing slower than refueling).
    pub(crate) fn service_time(&self, fuel: f32) -> f32 {
        if fuel > 0.0 {
            SERVICE_OVERHEAD + fuel / self.refuel_rate
        } else {
            0.0
        }
    }

    /// Time a stop that adds `fuel` liters costs against staying on track: the pit lane loss plus the standing
    /// time. `None` while the lane loss is unknown.
    pub(crate) fn expected_loss(&self, fuel: f32) -> Option<f32> {
        Some(self.lane_loss? + self.service_time(fuel))
    }

    /// Most fuel a car can have taken in `stationary` seconds standing, capped at the tank. An upper bound: tire
    /// changes and repairs also take standing time.
    pub(crate) fn fuel_for(&self, stationary: f32) -> f32 {
        let fuel = (stationary - SERVICE_OVERHEAD).max(0.0) * self.refuel_rate;
        self.tank.map_or(fuel, |tank| fuel.min(tank))
    }
}

/// The learned values file: refueling speed by car, pit lane loss by track.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Learned {
    refuel: HashMap<String, f32>,
    lane_loss: HashMap<String, f32>,
}

impl Learned {
    fn load(path: Option<&PathBuf>) -> Self {
        path.and_then(|p| fs::read_to_string(p).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn to_toml(&self) -> String {
        let mut out = String::from(
            "# Learned by the overlay's pit widget: refueling speed by car in liters per second, and pit lane\n\
             # loss by track ID in seconds. Delete a line to measure it again.\n",
        );
        for (table, values) in [("refuel", &self.refuel), ("lane_loss", &self.lane_loss)] {
            let mut keys: Vec<&String> = values.keys().collect();
            keys.sort();
            let _ = write!(out, "\n[{table}]\n");
            for key in keys {
                out.push('"');
                for c in key.chars().filter(|c| !c.is_control()) {
                    if c == '"' || c == '\\' {
                        out.push('\\');
                    }
                    out.push(c);
                }
                let _ = writeln!(out, "\" = {:.2}", values[key]);
            }
        }
        out
    }
}

/// Stops, the refueling speed, and the learned values, kept up to date from the view.
#[derive(Debug)]
pub(crate) struct PitTracker {
    stops: PitStops,
    refuel: RefuelMeter,
    learned: Learned,
    /// Where the learned values are kept; `None` keeps them in memory only.
    store: Option<PathBuf>,
    /// Learned values changed since the last save.
    dirty: bool,
    /// The session the stops belong to: `SessionNum`, track ID, and the player's car.
    session_num: Option<i32>,
    track: Option<i64>,
    car: String,
    player_stops: u16,
    model: PitModel,
}

impl PitTracker {
    pub(crate) fn new(store: Option<PathBuf>) -> Self {
        Self {
            stops: PitStops::default(),
            refuel: RefuelMeter::default(),
            learned: Learned::load(store.as_ref()),
            store,
            dirty: false,
            session_num: None,
            track: None,
            car: String::with_capacity(64),
            player_stops: 0,
            model: PitModel::default(),
        }
    }

    /// Takes the latest tick. Allocates only when the session's track or car changes, or when a stop of the
    /// player's saves what was learned.
    pub(crate) fn observe(&mut self, view: &View) {
        let (snap, vars, session) = (view.snapshot(), view.vars(), view.session());
        let Some(time) = snap.f64(vars.session_time, 0) else {
            return;
        };
        self.follow_session(snap, vars, session);
        let player = player_index(snap, vars, session);
        let count = snap.count(vars.car_lap_dist_pct);
        self.stops
            .observe(time, count, |i| car_sample(snap, vars, session, i));
        if let Some(fuel) = snap.f32(vars.fuel_level, 0) {
            let in_stall = snap.bool(vars.player_in_pit_stall, 0).unwrap_or(false);
            if let Some(rate) = self.refuel.observe(in_stall, time, fuel) {
                self.model.refuel_rate = rate;
                self.model.rate_measured = true;
                if !self.car.is_empty() {
                    let key = self.car.clone();
                    self.learned.refuel.insert(key, rate);
                    self.dirty = true;
                }
            }
        }
        if let Some(loss) = self.stops.measured_loss()
            && self.model.lane_loss != Some(loss)
        {
            self.model.lane_loss = Some(loss);
            self.dirty |= self.track.is_some();
        }
        // A finished stop of the player's is a good moment to keep what was learned: it happens a few times a race.
        let player_stops = player.map_or(0, |p| self.stops.stops(p));
        if player_stops != self.player_stops {
            self.player_stops = player_stops;
            self.save();
        }
    }

    /// Resets per-session state when the session, track, or player's car changes.
    fn follow_session(&mut self, snap: &Snapshot, vars: &Vars, session: Option<&SessionInfo>) {
        let num = snap.i32(vars.session_num, 0);
        if num != self.session_num {
            self.session_num = num;
            self.stops.clear_stops();
            self.player_stops = 0;
        }
        let Some(info) = session else {
            return;
        };
        let track = info.weekend.track_id;
        if track != self.track {
            self.save();
            self.track = track;
            self.stops.clear();
            self.player_stops = 0;
            self.model.lane_loss =
                track.and_then(|t| self.learned.lane_loss.get(&t.to_string()).copied());
        }
        let d = &info.driver_info;
        self.model.tank = d
            .fuel_max_liters
            .filter(|&l| l > 0.0)
            .map(|l| (l * d.max_fuel_pct.filter(|&p| p > 0.0).unwrap_or(1.0)) as f32);
        let car = player_index(snap, vars, session)
            .and_then(|p| d.drivers.get(p)?.as_ref()?.car_screen_name.as_deref())
            .unwrap_or("");
        if car != self.car {
            self.car.clear();
            self.car.push_str(car);
            let learned = self.learned.refuel.get(car).copied();
            self.model.refuel_rate = learned.unwrap_or(DEFAULT_REFUEL_RATE);
            self.model.rate_measured = learned.is_some();
        }
    }

    /// Writes the learned values when they changed.
    fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if let (Some(track), Some(loss)) = (self.track, self.stops.measured_loss()) {
            self.learned.lane_loss.insert(track.to_string(), loss);
        }
        if let Some(path) = &self.store {
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(path, self.learned.to_toml());
        }
    }

    pub(crate) fn model(&self) -> PitModel {
        self.model
    }

    pub(crate) fn stops(&self) -> &PitStops {
        &self.stops
    }

    /// The connection changed: keeps what was learned and forgets the session.
    pub(crate) fn reset(&mut self) {
        self.save();
        self.stops.clear();
        self.refuel = RefuelMeter::default();
        self.session_num = None;
        self.track = None;
        self.car.clear();
        self.player_stops = 0;
        self.model = PitModel::default();
    }
}

impl Drop for PitTracker {
    fn drop(&mut self) {
        self.save();
    }
}

/// The player's car index, from telemetry or the session info.
pub(crate) fn player_index(
    snap: &Snapshot,
    vars: &Vars,
    session: Option<&SessionInfo>,
) -> Option<usize> {
    snap.i32(vars.player_car_idx, 0)
        .and_then(|i| usize::try_from(i).ok())
        .or_else(|| session.and_then(|s| s.driver_info.player_car_idx))
}

/// Car `i` as [`PitStops::observe`] needs it. The pace is the car's best lap, else its class's estimated lap.
fn car_sample(snap: &Snapshot, vars: &Vars, session: Option<&SessionInfo>, i: usize) -> CarSample {
    let pace = snap
        .f32(vars.car_best_lap_time, i)
        .filter(|&t| t > 1.0)
        .or_else(|| {
            session
                .and_then(|s| {
                    s.driver_info
                        .drivers
                        .get(i)?
                        .as_ref()?
                        .car_class_est_lap_time
                })
                .map(|t| t as f32)
        });
    CarSample {
        on_pit: snap.bool(vars.car_on_pit_road, i).unwrap_or(false),
        surface: snap.i32(vars.car_track_surface, i).unwrap_or(NOT_IN_WORLD),
        pct: snap.f32(vars.car_lap_dist_pct, i).unwrap_or(-1.0),
        laps: snap.i32(vars.car_lap_completed, i).unwrap_or(-1),
        pace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON_TRACK: i32 = 3;
    /// Lap time of the test car: 0.1 lap takes 4 s.
    const PACE: f32 = 40.0;

    fn sample(on_pit: bool, surface: i32, pct: f32) -> CarSample {
        CarSample {
            on_pit,
            surface,
            pct,
            laps: 11,
            pace: Some(PACE),
        }
    }

    /// Drives one car through `steps` at 10 Hz from `start`: each entry is (on pit road, surface, lap distance).
    fn drive(stops: &mut PitStops, start: f64, steps: &[(bool, i32, f32)]) -> f64 {
        let mut t = start;
        for &(on_pit, surface, pct) in steps {
            stops.observe(t, 1, |_| sample(on_pit, surface, pct));
            t += 0.1;
        }
        t
    }

    /// A stop: 10 s on pit road from lap distance 0.95 to 0.05, standing still for 5.0 s in the stall.
    fn stop_steps() -> Vec<(bool, i32, f32)> {
        let mut steps = vec![(false, ON_TRACK, 0.94)];
        // 2.5 s rolling in, 5 s standing (51 observations at the same place), 2.4 s rolling out.
        for k in 0..25 {
            steps.push((true, 2, 0.95 + 0.002 * k as f32));
        }
        for _ in 0..51 {
            steps.push((true, IN_PIT_STALL, 1.0 - 0.001));
        }
        for k in 0..24 {
            steps.push((true, 2, (0.999 + 0.002 * k as f32) % 1.0));
        }
        steps.push((false, ON_TRACK, 0.05));
        steps
    }

    #[test]
    fn times_a_stop_and_its_lane_loss() {
        let mut stops = PitStops::default();
        drive(&mut stops, 100.0, &stop_steps());
        let stop = stops.last_stop(0).expect("a stop");
        assert_eq!(stop.lap, 11);
        assert!((stop.lane_time - 10.0).abs() < 0.11, "{}", stop.lane_time);
        // 51 observations at one place span 5.0 s; the run is counted one step longer.
        assert!((stop.stationary - 5.1).abs() < 0.01, "{}", stop.stationary);
        // 0.1 lap takes 4 s on track: lane loss 10 - 5.1 - 4.
        let loss = stop.loss.expect("pace known");
        assert!((loss - 0.9).abs() < 0.12, "{loss}");
        assert_eq!(stops.stops(0), 1);
    }

    #[test]
    fn lane_loss_is_the_median_of_stops() {
        let mut stops = PitStops::default();
        // Each visit: driving pit lane without standing, from 0.95 to 0.05 (4 s on track): losses 26, 27, 36.
        let mut t = 0.0;
        for lane in [30.0f32, 31.0, 40.0] {
            let mut steps = vec![(false, ON_TRACK, 0.94)];
            let n = (lane * 10.0) as usize;
            for k in 0..n {
                steps.push((true, 2, (0.95 + 0.1 * k as f32 / n as f32) % 1.0));
            }
            steps.push((false, ON_TRACK, 0.05));
            t = drive(&mut stops, t, &steps) + 0.1;
        }
        let median = stops.measured_loss().expect("losses");
        assert!((median - 27.0).abs() < 0.2, "{median}");
    }

    #[test]
    fn garage_visits_are_not_stops() {
        let mut stops = PitStops::default();
        // Appears in its stall from the garage, then drives out: not a stop.
        let t = drive(
            &mut stops,
            0.0,
            &[
                (false, NOT_IN_WORLD, -1.0),
                (true, IN_PIT_STALL, 0.06),
                (true, IN_PIT_STALL, 0.06),
                (true, 2, 0.07),
                (false, ON_TRACK, 0.08),
            ],
        );
        assert_eq!(stops.last_stop(0), None);
        // Comes in from the track and leaves the world from the stall: not a stop either.
        drive(
            &mut stops,
            t,
            &[
                (false, ON_TRACK, 0.9),
                (true, 2, 0.95),
                (true, IN_PIT_STALL, 0.96),
                (true, IN_PIT_STALL, 0.96),
                (false, NOT_IN_WORLD, -1.0),
            ],
        );
        assert_eq!(stops.last_stop(0), None);
        assert_eq!(stops.measured_loss(), None);
    }

    #[test]
    fn a_pause_drops_the_visit_in_progress() {
        let mut stops = PitStops::default();
        let steps = stop_steps();
        let t = drive(&mut stops, 0.0, &steps[..40]);
        // Observations stop for 30 s (the widget was hidden), then resume after the stop.
        drive(&mut stops, t + 30.0, &steps[90..]);
        assert_eq!(stops.last_stop(0), None, "the stop was not seen whole");
    }

    #[test]
    fn measures_the_refueling_speed() {
        // Home PC capture, 4 Hz excerpt: 12.26 L standing, then 2.67 L per 0.25 s up to the 40 L tank.
        let mut meter = RefuelMeter::default();
        let mut t = 2405.8;
        let mut fuel = 12.26f32;
        let mut result = None;
        for k in 0..30 {
            if (3..14).contains(&k) {
                fuel = (fuel + 2.667).min(40.0);
            }
            if let Some(r) = meter.observe(true, t, fuel) {
                result = Some(r);
            }
            t += 0.25;
        }
        let rate = result.expect("measured");
        assert!((rate - 10.67).abs() < 0.05, "{rate}");
        // Outside the stall, fuel changes do not count.
        let mut meter = RefuelMeter::default();
        for k in 0..10 {
            assert_eq!(meter.observe(false, k as f64, k as f32), None);
        }
    }

    #[test]
    fn fuel_from_standing_time() {
        let model = PitModel {
            lane_loss: Some(20.0),
            refuel_rate: 10.0,
            rate_measured: true,
            tank: Some(40.0),
        };
        assert_eq!(model.fuel_for(2.0), 0.0, "shorter than the overhead");
        assert!((model.fuel_for(5.5) - 25.0).abs() < 1e-4);
        assert_eq!(model.fuel_for(60.0), 40.0, "capped at the tank");
        assert_eq!(model.service_time(0.0), 0.0);
        assert!((model.service_time(25.0) - 5.5).abs() < 1e-4);
        assert_eq!(model.expected_loss(25.0), Some(25.5));
        assert_eq!(PitModel::default().expected_loss(10.0), None);
    }

    #[test]
    fn learned_values_round_trip() {
        let mut learned = Learned::default();
        learned.refuel.insert("FIA \"F4\"".to_owned(), 10.666);
        learned.lane_loss.insert("204".to_owned(), 21.4);
        let text = learned.to_toml();
        let back: Learned = toml::from_str(&text).expect("parses");
        assert_eq!(back.refuel.get("FIA \"F4\""), Some(&10.67));
        assert_eq!(back.lane_loss.get("204"), Some(&21.4));
    }
}
