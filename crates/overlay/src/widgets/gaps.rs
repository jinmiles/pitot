//! Every car's measured gaps on track over the last two minutes, for widgets that follow how gaps change: a battle
//! with the car ahead, a faster class closing in, a car approaching while the player rejoins, a car alongside.
//!
//! Every [`INTERVAL`] of tick time ([`View::tick_time`]) the history takes a sample: for each car in the world, the gap on track to
//! the player and to the next cars ahead and behind it on track, as the shared [`TrackClock`] measured them. A gap
//! is the time since the car in front passed the spot where the other one is now, whatever their laps; pit road
//! is included as measured. Storage is sized per connection from the car count; sampling does not allocate.
//!
//! The history follows the clock: widgets call [`GapHistory::observe`] at their own rate, which observes the clock
//! too (once for all its users) and samples when one is due.

use std::cell::RefCell;
use std::rc::Rc;

use pitot_irsdk::flags::track_location;

use crate::pipeline::View;

use super::trackclock::SharedTrackClock;

/// Tick time between samples.
pub(crate) const INTERVAL: f64 = 0.5;
/// Samples kept per car: two minutes.
pub(crate) const SAMPLES: usize = 240;
/// No car, in [`Entry`].
const NO_CAR: u16 = u16::MAX;

/// One history for every widget that follows gaps.
pub(crate) type SharedGapHistory = Rc<RefCell<GapHistory>>;

/// One car at one sample, as read back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GapSample {
    /// Tick time of the sample.
    pub time: f64,
    /// Seconds to the player on track: positive when the car is ahead of the player (within half a lap), negative
    /// when it is behind, 0 for the player. NaN when the pass was not measured or the car was not in the world.
    pub to_player: f32,
    /// The next car ahead on track, and the seconds since it passed this car's spot (NaN when not measured).
    pub ahead: Option<(usize, f32)>,
    /// The next car behind on track, and the seconds since this car passed its spot (NaN when not measured).
    pub behind: Option<(usize, f32)>,
}

/// One car at one sample, as stored: 16 bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    to_player: f32,
    ahead: f32,
    behind: f32,
    ahead_car: u16,
    behind_car: u16,
}

impl Entry {
    const UNKNOWN: Self = Self {
        to_player: f32::NAN,
        ahead: f32::NAN,
        behind: f32::NAN,
        ahead_car: NO_CAR,
        behind_car: NO_CAR,
    };
}

/// A ring of samples per car; see the module documentation.
#[derive(Debug)]
pub(crate) struct GapHistory {
    clock: SharedTrackClock,
    cars: usize,
    /// Tick time of each slot.
    times: Vec<f64>,
    /// `SAMPLES` entries per car.
    entries: Vec<Entry>,
    /// The slot written next.
    head: usize,
    /// Samples held, up to `SAMPLES`.
    len: usize,
    /// Cars in the world by lap distance, for one sample.
    order: Vec<(f32, u16)>,
    session_num: Option<i32>,
    last: Option<f64>,
}

impl GapHistory {
    pub(crate) fn new(clock: SharedTrackClock) -> Self {
        Self {
            clock,
            cars: 0,
            times: Vec::new(),
            entries: Vec::new(),
            head: 0,
            len: 0,
            order: Vec::new(),
            session_num: None,
            last: None,
        }
    }

    /// A history to share between widgets, on `clock`.
    pub(crate) fn shared(clock: SharedTrackClock) -> SharedGapHistory {
        Rc::new(RefCell::new(Self::new(clock)))
    }

    /// Observes the clock and takes a sample when one is due. Allocates only when the car count changes (once per
    /// connection).
    pub(crate) fn observe(&mut self, view: &View) {
        if let Ok(mut clock) = self.clock.try_borrow_mut() {
            clock.observe(view);
        }
        let (snap, vars) = (view.snapshot(), view.vars());
        let Some(now) = view.tick_time() else {
            return;
        };
        let session_num = snap.i32(vars.session_num, 0);
        if session_num != self.session_num {
            self.session_num = session_num;
            self.reset();
        }
        let cars = view.car_count();
        if cars != self.cars {
            self.resize(cars);
        }
        let player = snap
            .i32(vars.player_car_idx, 0)
            .and_then(|p| usize::try_from(p).ok())
            .filter(|&p| p < cars);
        let drivers = view.session().map(|s| s.driver_info.drivers.as_slice());
        // The cars the clock times: drivers of the session, not the pace car, in the world.
        let pct = |car: usize| {
            let racing = drivers
                .and_then(|d| d.get(car))
                .and_then(Option::as_ref)
                .is_some_and(|d| !d.is_pace_car && !d.is_spectator);
            let in_world =
                snap.i32(vars.car_track_surface, car) != Some(track_location::NOT_IN_WORLD);
            snap.f32(vars.car_lap_dist_pct, car)
                .filter(|p| (0.0..=1.0).contains(p) && racing && in_world)
        };
        let shared = Rc::clone(&self.clock);
        let Ok(clock) = shared.try_borrow() else {
            return;
        };
        let race = clock.clock();
        self.record(now, player, pct, |ahead, behind| {
            race.track_gap(ahead, behind)
        });
    }

    /// Sizes the storage for `cars` cars and forgets every sample.
    fn resize(&mut self, cars: usize) {
        self.cars = cars;
        self.times.clear();
        self.times.resize(SAMPLES, f64::NAN);
        self.entries.clear();
        self.entries.resize(cars * SAMPLES, Entry::UNKNOWN);
        self.order.clear();
        self.order.reserve_exact(cars);
        self.head = 0;
        self.len = 0;
        self.last = None;
    }

    /// Forgets every sample, keeping the storage.
    pub(crate) fn reset(&mut self) {
        self.head = 0;
        self.len = 0;
        self.last = None;
    }

    /// Takes a sample at tick time `now` when one is due. `pct(car)` is a car's lap distance while it is in the
    /// world, and `gap(ahead, behind)` the clock's gap on track from `ahead` back to `behind`.
    fn record(
        &mut self,
        now: f64,
        player: Option<usize>,
        pct: impl Fn(usize) -> Option<f32>,
        gap: impl Fn(usize, usize) -> Option<f64>,
    ) {
        if self.cars == 0 {
            return;
        }
        match self.last {
            // A rewound replay or a tick count that started over: the samples are of another time line.
            Some(last) if now < last => self.reset(),
            Some(last) if now - last < INTERVAL => return,
            _ => {}
        }
        self.last = Some(now);
        let slot = self.head;
        self.head = (self.head + 1) % SAMPLES;
        self.len = (self.len + 1).min(SAMPLES);
        self.times[slot] = now;
        self.order.clear();
        for car in 0..self.cars {
            self.entries[car * SAMPLES + slot] = Entry::UNKNOWN;
            if let (Some(p), Ok(index)) = (pct(car), u16::try_from(car)) {
                self.order.push((p, index));
            }
        }
        // Around the lap: the next car in lap distance is the one ahead on track.
        self.order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let seconds = |g: Option<f64>| g.map_or(f32::NAN, |g| g as f32);
        let player_pct = player.and_then(&pct);
        let n = self.order.len();
        for k in 0..n {
            let (car_pct, car) = self.order[k];
            let car = usize::from(car);
            let mut entry = Entry::UNKNOWN;
            if n > 1 {
                let (_, ahead) = self.order[(k + 1) % n];
                let (_, behind) = self.order[(k + n - 1) % n];
                entry.ahead_car = ahead;
                entry.ahead = seconds(gap(usize::from(ahead), car));
                entry.behind_car = behind;
                entry.behind = seconds(gap(car, usize::from(behind)));
            }
            if let (Some(player), Some(player_pct)) = (player, player_pct) {
                entry.to_player = if car == player {
                    0.0
                } else if ahead_of(car_pct, player_pct) {
                    seconds(gap(car, player))
                } else {
                    -seconds(gap(player, car))
                };
            }
            self.entries[car * SAMPLES + slot] = entry;
        }
    }

    /// Samples held; every car has one per sample.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The `index`-th sample of `car`, oldest first.
    fn sample(&self, car: usize, index: usize) -> Option<GapSample> {
        if car >= self.cars || index >= self.len {
            return None;
        }
        let slot = (self.head + SAMPLES - self.len + index) % SAMPLES;
        let e = self.entries[car * SAMPLES + slot];
        let neighbor = |c: u16, g: f32| (c != NO_CAR).then_some((usize::from(c), g));
        Some(GapSample {
            time: self.times[slot],
            to_player: e.to_player,
            ahead: neighbor(e.ahead_car, e.ahead),
            behind: neighbor(e.behind_car, e.behind),
        })
    }

    /// The samples of `car`, oldest first.
    pub(crate) fn samples(&self, car: usize) -> impl DoubleEndedIterator<Item = GapSample> + '_ {
        (0..self.len).filter_map(move |i| self.sample(car, i))
    }

    /// The latest sample of `car`.
    pub(crate) fn latest(&self, car: usize) -> Option<GapSample> {
        self.sample(car, self.len.checked_sub(1)?)
    }

    /// The newest sample of `car` taken at least `seconds` before the latest, for how a gap changed over that
    /// time. `None` when the history is shorter.
    pub(crate) fn ago(&self, car: usize, seconds: f64) -> Option<GapSample> {
        let latest = self.latest(car)?.time;
        self.samples(car).rev().find(|s| latest - s.time >= seconds)
    }
}

/// Whether a car at lap distance `car` is ahead of one at `other` on track: within half a lap in front.
fn ahead_of(car: f32, other: f32) -> bool {
    let d = (car - other).rem_euclid(1.0);
    d > 0.0 && d <= 0.5
}

#[cfg(test)]
mod tests {
    use super::super::trackclock::TrackClock;
    use super::*;

    /// Lap distance at time `t` of a car `offset` seconds behind car 0, on 100 s laps at constant speed.
    fn place(t: f64, offset: f64) -> f32 {
        (((t - offset) / 100.0).rem_euclid(1.0)) as f32
    }

    #[test]
    fn samples_every_interval_with_signed_gaps_and_neighbors() {
        let mut gaps = GapHistory::new(TrackClock::shared());
        gaps.resize(4);
        // Car 0 leads car 1 by 3 s and car 2 by 10 s; car 3 is not in the world. The player is car 1.
        let offsets = [0.0, 3.0, 10.0];
        let pct = |t: f64| move |car: usize| offsets.get(car).map(|&o| place(t, o));
        // Measured gaps as the clock gives them: the time since `ahead` passed where `behind` is.
        let gap = |ahead: usize, behind: usize| {
            let lead = (offsets[behind] - offsets[ahead]).rem_euclid(100.0);
            Some(lead)
        };
        let mut t = 50.0;
        while t < 60.0 {
            gaps.record(t, Some(1), pct(t), gap);
            t += 0.1;
        }
        // Every half second: 50.0, 50.5, ... 59.5 (the observation at 59.9 is too soon after 59.5).
        assert_eq!(gaps.len(), 20);
        let latest = gaps.latest(0).expect("a sample");
        assert!((latest.time - 59.5).abs() < 1e-6);
        assert!((latest.to_player - 3.0).abs() < 1e-6, "car 0 is 3 s ahead");
        let player = gaps.latest(1).expect("the player");
        assert_eq!(player.to_player, 0.0);
        let third = gaps.latest(2).expect("car 2");
        assert!((third.to_player + 7.0).abs() < 1e-6, "car 2 is 7 s behind");
        // On track: car 1 has car 0 ahead (3 s) and car 2 behind (7 s). Car 0, the furthest round the lap, has
        // car 2 ahead of it across the line, 90 s on.
        assert_eq!(player.ahead.map(|a| a.0), Some(0));
        assert!((player.ahead.expect("ahead").1 - 3.0).abs() < 1e-6);
        assert_eq!(player.behind.map(|b| b.0), Some(2));
        assert!((player.behind.expect("behind").1 - 7.0).abs() < 1e-6);
        assert_eq!(latest.ahead.map(|a| a.0), Some(2));
        assert!((latest.ahead.expect("ahead").1 - 90.0).abs() < 1e-6);
        assert_eq!(latest.behind.map(|b| b.0), Some(1));
        let outside = gaps.latest(3).expect("car 3");
        assert!(outside.to_player.is_nan() && outside.ahead.is_none() && outside.behind.is_none());
        // Five seconds back.
        let before = gaps.ago(0, 5.0).expect("ten samples back");
        assert!((before.time - 54.5).abs() < 1e-6);
        assert_eq!(gaps.samples(0).count(), 20);
        assert!(gaps.samples(0).map(|s| s.time).is_sorted());
        assert_eq!(gaps.ago(0, 60.0), None, "not that long yet");
        assert_eq!(gaps.latest(9), None, "no such car");
    }

    #[test]
    fn keeps_the_last_two_minutes_and_starts_over_when_time_goes_back() {
        let mut gaps = GapHistory::new(TrackClock::shared());
        gaps.resize(2);
        let pct = |t: f64| move |car: usize| Some(place(t, car as f64 * 5.0));
        // A gap that grows by 0.01 s per second: car 1 falls back.
        let gap = |t: f64| move |_: usize, _: usize| Some(5.0 + 0.01 * t);
        for n in 0..1000 {
            let t = f64::from(n) * INTERVAL;
            gaps.record(t, Some(0), pct(t), gap(t));
        }
        assert_eq!(gaps.len(), SAMPLES);
        // Samples 760 to 999.
        let oldest = gaps.samples(1).next().expect("oldest");
        assert_eq!(oldest.time, 380.0);
        let (now, then) = (
            gaps.latest(1).expect("now"),
            gaps.ago(1, 60.0).expect("then"),
        );
        let change = (now.to_player - then.to_player) / (now.time - then.time) as f32;
        assert!(
            (change + 0.01).abs() < 1e-4,
            "the gap behind grows: {change}"
        );
        // A replay rewound to before the samples.
        gaps.record(10.0, Some(0), pct(10.0), gap(10.0));
        assert_eq!(gaps.len(), 1);
        // Without a player, the gaps between cars are still kept.
        gaps.record(11.0, None, pct(11.0), gap(11.0));
        let sample = gaps.latest(0).expect("a sample");
        assert!(sample.to_player.is_nan());
        assert!(sample.ahead.is_some());
    }

    #[test]
    fn unmeasured_gaps_are_unknown_and_one_car_has_no_neighbors() {
        let mut gaps = GapHistory::new(TrackClock::shared());
        gaps.resize(3);
        gaps.record(
            1.0,
            Some(0),
            |car| (car == 0).then_some(0.5),
            |_, _| Some(1.0),
        );
        let alone = gaps.latest(0).expect("a sample");
        assert_eq!(alone.to_player, 0.0);
        assert!(alone.ahead.is_none() && alone.behind.is_none());
        gaps.record(2.0, Some(0), |car| Some(0.1 * car as f32), |_, _| None);
        let car = gaps.latest(2).expect("a sample");
        assert!(car.to_player.is_nan(), "a pass the clock did not see");
        assert!(car.ahead.is_some_and(|(_, g)| g.is_nan()));
        assert!(ahead_of(0.1, 0.9) && !ahead_of(0.9, 0.1) && !ahead_of(0.3, 0.3));
        // As documented: 16 bytes per car and sample.
        assert_eq!(std::mem::size_of::<Entry>(), 16);
        assert_eq!(gaps.entries.len(), 3 * SAMPLES);
    }
}
