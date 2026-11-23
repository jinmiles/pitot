//! Every car's completed laps, for widgets that list or compare laps: a lap log, a lap time graph, the spread of
//! lap times, stints.
//!
//! Each observation reads every car's completed laps (`CarIdxLapCompleted`), last lap time, pit road, and track
//! location, and the player's incident count. A lap ends when the completed laps go up. Its time is the sim's
//! `CarIdxLastLapTime`, which changes a moment after the line (2 to 63 ticks later in the public AI race capture),
//! so a lap joins the history once its time arrives, or without a time after [`TIMING_WAIT`]. The flags say what
//! the observations saw during the lap, so how much they catch depends on how often widgets observe: an excursion
//! off track between two observations goes unseen.
//!
//! Each car keeps its last [`LAPS`] laps. Storage is sized per connection from the car count; observing does not
//! allocate.

use std::cell::RefCell;
use std::rc::Rc;

use pitot_irsdk::flags::track_location;

use crate::pipeline::View;

use super::fuel::is_green;

/// Laps kept per car.
pub(crate) const LAPS: usize = 100;
/// Tick time after the line within which the sim gives the lap's time; later, the lap is kept without one.
pub(crate) const TIMING_WAIT: f64 = 3.0;
/// A longer gap between observations of a car marks its lap partial.
const MAX_OBSERVATION_GAP: f64 = 5.0;

/// One history for every widget that reads laps.
pub(crate) type SharedLapHistory = Rc<RefCell<LapHistory>>;

/// A completed lap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Lap {
    /// Laps the car had completed with this one (`CarIdxLapCompleted`).
    pub number: i32,
    /// Seconds, as the sim timed it; NaN when it gave no time.
    pub time: f32,
    /// Tick time ([`View::tick_time`]) of the first observation after the line.
    pub ended: f64,
    /// What the observations saw during the lap: [`Lap::PIT`] and the others.
    pub flags: u8,
}

impl Lap {
    /// On pit road at some moment of the lap: an in lap, an out lap, or a drive-through.
    pub const PIT: u8 = 1;
    /// Seen off track.
    pub const OFF_TRACK: u8 = 2;
    /// The player's incident count went up; other cars' incidents are not published.
    pub const INCIDENT: u8 = 4;
    /// Not under green: a full-course caution, pace laps, or before the start.
    pub const CAUTION: u8 = 8;
    /// Not observed from start to end: the first lap seen of a car, or the car left the world or went unobserved
    /// for a while, so the other flags may miss something.
    pub const PARTIAL: u8 = 16;

    pub(crate) fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// Timed, seen from start to end, and never on pit road, off track, or with an incident. A lap under caution
    /// can be clean; check [`Lap::CAUTION`] for pace.
    pub(crate) fn is_clean(&self) -> bool {
        self.time.is_finite()
            && !self.has(Self::PIT | Self::OFF_TRACK | Self::INCIDENT | Self::PARTIAL)
    }
}

/// What one observation saw of a car in the world.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CarSample {
    completed: i32,
    /// `CarIdxLastLapTime`; zero or less when there is none.
    last_time: f32,
    on_pit: bool,
    off_track: bool,
}

/// A car's lap in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Running {
    /// Completed laps as last seen; `None` until the car is seen.
    completed: Option<i32>,
    flags: u8,
    /// Tick time of the last observation.
    seen: f64,
    last_time: f32,
    /// A lap that ended and waits for its time, with the time shown before it.
    pending: Option<(Lap, f32)>,
}

impl Default for Running {
    fn default() -> Self {
        Self {
            completed: None,
            flags: 0,
            seen: f64::NEG_INFINITY,
            last_time: 0.0,
            pending: None,
        }
    }
}

/// Where a car's laps are in its ring.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Ring {
    /// The slot written next.
    head: usize,
    len: usize,
}

/// A ring of laps per car; see the module documentation.
#[derive(Debug, Default)]
pub(crate) struct LapHistory {
    running: Vec<Running>,
    rings: Vec<Ring>,
    /// `LAPS` laps per car.
    laps: Vec<Lap>,
    session_num: Option<i32>,
    last: Option<f64>,
    incidents: Option<i32>,
}

impl LapHistory {
    /// A history to share between widgets.
    pub(crate) fn shared() -> SharedLapHistory {
        Rc::new(RefCell::new(Self::default()))
    }

    /// Takes the latest tick unless it was taken already. Allocates only when the car count changes (once per
    /// connection).
    pub(crate) fn observe(&mut self, view: &View) {
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
        if cars != self.running.len() {
            self.resize(cars);
        }
        let flags = snap.bitfield(vars.session_flags, 0).unwrap_or(0);
        let caution = !is_green(snap.i32(vars.session_state, 0), flags);
        let player = snap
            .i32(vars.player_car_idx, 0)
            .and_then(|p| usize::try_from(p).ok());
        let incidents = snap.i32(vars.incidents, 0);
        let drivers = view.session().map(|s| s.driver_info.drivers.as_slice());
        let sample = |car: usize| {
            let pace_car = drivers
                .and_then(|d| d.get(car))
                .and_then(Option::as_ref)
                .is_some_and(|d| d.is_pace_car);
            let surface = snap.i32(vars.car_track_surface, car)?;
            if pace_car || surface == track_location::NOT_IN_WORLD {
                return None;
            }
            Some(CarSample {
                completed: snap.i32(vars.car_lap_completed, car)?,
                last_time: snap.f32(vars.car_last_lap_time, car).unwrap_or(0.0),
                on_pit: snap.bool(vars.car_on_pit_road, car).unwrap_or(false),
                off_track: surface == track_location::OFF_TRACK,
            })
        };
        self.record(now, caution, player, incidents, sample);
    }

    /// Sizes the storage for `cars` cars and forgets every lap.
    fn resize(&mut self, cars: usize) {
        self.running.clear();
        self.running.resize(cars, Running::default());
        self.rings.clear();
        self.rings.resize(cars, Ring::default());
        let empty = Lap {
            number: 0,
            time: f32::NAN,
            ended: 0.0,
            flags: 0,
        };
        self.laps.clear();
        self.laps.resize(cars * LAPS, empty);
        self.last = None;
        self.incidents = None;
    }

    /// Forgets every lap, keeping the storage.
    pub(crate) fn reset(&mut self) {
        self.running
            .iter_mut()
            .for_each(|r| *r = Running::default());
        self.rings.iter_mut().for_each(|r| *r = Ring::default());
        self.last = None;
        self.incidents = None;
    }

    /// Takes one observation at tick time `now`. `sample(car)` is what it saw of a car in the world, `None` for
    /// one that is not; `incidents` is the player's incident count.
    fn record(
        &mut self,
        now: f64,
        caution: bool,
        player: Option<usize>,
        incidents: Option<i32>,
        sample: impl Fn(usize) -> Option<CarSample>,
    ) {
        match self.last {
            // A rewound replay or a tick count that started over: the laps are of another time line.
            Some(last) if now < last => self.reset(),
            Some(last) if now == last => return,
            _ => {}
        }
        self.last = Some(now);
        let incident = matches!((incidents, self.incidents), (Some(a), Some(b)) if a > b);
        if incidents.is_some() {
            self.incidents = incidents;
        }
        let Self {
            running,
            rings,
            laps,
            ..
        } = self;
        for (car, r) in running.iter_mut().enumerate() {
            let mut keep = |lap: Lap| push(laps, &mut rings[car], car, lap);
            let Some(s) = sample(car) else {
                if r.completed.is_some() {
                    r.flags |= Lap::PARTIAL;
                }
                if let Some((lap, _)) = r.pending.filter(|(l, _)| now - l.ended > TIMING_WAIT) {
                    keep(lap);
                    r.pending = None;
                }
                continue;
            };
            let mut seen = 0;
            if s.on_pit {
                seen |= Lap::PIT;
            }
            if s.off_track {
                seen |= Lap::OFF_TRACK;
            }
            if caution {
                seen |= Lap::CAUTION;
            }
            if incident && player == Some(car) {
                seen |= Lap::INCIDENT;
            }
            match r.completed {
                None => {
                    // First seen: the lap under way started unobserved.
                    *r = Running {
                        completed: Some(s.completed),
                        flags: Lap::PARTIAL | seen,
                        seen: now,
                        last_time: s.last_time,
                        pending: None,
                    };
                    continue;
                }
                Some(done) => {
                    if now - r.seen > MAX_OBSERVATION_GAP {
                        r.flags |= Lap::PARTIAL;
                    }
                    if s.completed > done {
                        if let Some((untimed, _)) = r.pending.take() {
                            keep(untimed);
                        }
                        let mut flags = r.flags | seen;
                        if s.completed > done + 1 {
                            flags |= Lap::PARTIAL;
                        }
                        let lap = Lap {
                            number: s.completed,
                            time: f32::NAN,
                            ended: now,
                            flags,
                        };
                        r.pending = Some((lap, r.last_time));
                        // The new lap starts where the car is now; the incident belongs to the lap that ended.
                        r.flags = seen & !Lap::INCIDENT;
                    } else if s.completed < done {
                        // Counted back (a reset or a new start): what was under way is not a lap.
                        r.pending = None;
                        r.flags = Lap::PARTIAL | seen;
                    } else {
                        r.flags |= seen;
                    }
                }
            }
            r.completed = Some(s.completed);
            r.seen = now;
            if let Some((mut lap, before)) = r.pending {
                if s.last_time != before {
                    lap.time = if s.last_time > 0.0 {
                        s.last_time
                    } else {
                        f32::NAN
                    };
                    keep(lap);
                    r.pending = None;
                } else if now - lap.ended > TIMING_WAIT {
                    keep(lap);
                    r.pending = None;
                }
            }
            r.last_time = s.last_time;
        }
    }

    /// The laps of `car` in the history, oldest first.
    pub(crate) fn laps(&self, car: usize) -> impl DoubleEndedIterator<Item = &Lap> + '_ {
        let ring = self.rings.get(car).copied().unwrap_or_default();
        (0..ring.len)
            .map(move |i| &self.laps[car * LAPS + (ring.head + LAPS - ring.len + i) % LAPS])
    }

    /// The latest lap of `car` in the history.
    pub(crate) fn last(&self, car: usize) -> Option<&Lap> {
        self.laps(car).next_back()
    }

    /// Laps of `car` in the history, up to [`LAPS`].
    pub(crate) fn count(&self, car: usize) -> usize {
        self.rings.get(car).map_or(0, |r| r.len)
    }
}

fn push(laps: &mut [Lap], ring: &mut Ring, car: usize, lap: Lap) {
    if let Some(slot) = laps.get_mut(car * LAPS + ring.head) {
        *slot = lap;
        ring.head = (ring.head + 1) % LAPS;
        ring.len = (ring.len + 1).min(LAPS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn car(completed: i32, last_time: f32) -> CarSample {
        CarSample {
            completed,
            last_time,
            on_pit: false,
            off_track: false,
        }
    }

    #[test]
    fn a_lap_is_kept_once_the_sim_times_it() {
        let mut laps = LapHistory::default();
        laps.resize(2);
        // Car 0 is first seen on lap 3 (its last lap 91.2 s); car 1 is not in the world.
        let at = |completed, time| move |c: usize| (c == 0).then_some(car(completed, time));
        laps.record(10.0, false, Some(0), Some(0), at(3, 91.2));
        laps.record(10.1, false, Some(0), Some(0), at(4, 91.2));
        assert_eq!(laps.count(0), 0, "waiting for the time");
        laps.record(10.2, false, Some(0), Some(0), at(4, 90.5));
        let lap = *laps.last(0).expect("a lap");
        assert_eq!((lap.number, lap.time), (4, 90.5));
        assert!((lap.ended - 10.1).abs() < 1e-9);
        assert!(
            lap.has(Lap::PARTIAL) && !lap.is_clean(),
            "first seen during it"
        );
        // The next lap, seen whole: clean. The time arrives with the line in one observation.
        laps.record(100.6, false, Some(0), Some(0), at(5, 90.4));
        let lap = *laps.last(0).expect("a lap");
        assert_eq!((lap.number, lap.time), (5, 90.4));
        assert!(lap.has(Lap::PARTIAL), "observed 90 s apart");
        // Laps 6 to 8 observed every 0.9 s, the sim showing each lap's time from the line on.
        let mut t = 100.6;
        for completed in 6..9 {
            let previous = 89.0 + completed as f32;
            for _ in 0..100 {
                t += 0.9;
                laps.record(t, false, Some(0), Some(0), at(completed - 1, previous));
            }
            t += 0.5;
            laps.record(t, false, Some(0), Some(0), at(completed, previous + 1.0));
        }
        let numbers: Vec<i32> = laps.laps(0).map(|l| l.number).collect();
        assert_eq!(numbers, [4, 5, 6, 7, 8]);
        assert!(laps.last(0).is_some_and(Lap::is_clean));
        assert_eq!(laps.count(1), 0);
        assert_eq!(laps.count(9), 0, "no such car");
    }

    #[test]
    fn a_lap_the_sim_does_not_time_is_kept_without_a_time() {
        let mut laps = LapHistory::default();
        laps.resize(1);
        laps.record(0.0, false, None, None, |_| Some(car(1, 80.0)));
        laps.record(1.0, false, None, None, |_| Some(car(2, 80.0)));
        laps.record(3.5, false, None, None, |_| Some(car(2, 80.0)));
        assert_eq!(laps.count(0), 0);
        laps.record(4.5, false, None, None, |_| Some(car(2, 80.0)));
        assert!(
            laps.last(0)
                .is_some_and(|l| l.number == 2 && l.time.is_nan())
        );
        // A time of -1 is none either.
        laps.record(5.0, false, None, None, |_| Some(car(3, 80.0)));
        laps.record(5.5, false, None, None, |_| Some(car(3, -1.0)));
        assert!(
            laps.last(0)
                .is_some_and(|l| l.number == 3 && l.time.is_nan())
        );
    }

    #[test]
    fn flags_follow_what_was_seen() {
        let mut laps = LapHistory::default();
        laps.resize(2);
        // Ten observations a second apart during lap `lap + 1`, then one at the line, under green, with the
        // player's (car 1's) incident count at `incidents`.
        let mut t = 0.0;
        let mut run = |laps: &mut LapHistory,
                       lap: i32,
                       f: &dyn Fn(usize, f32) -> CarSample,
                       caution: bool,
                       incidents: i32| {
            for step in 0..10 {
                t += 1.0;
                laps.record(t, caution, Some(1), Some(incidents), |c| {
                    Some(f(c, step as f32))
                });
            }
            t += 1.0;
            let line = |c: usize| Some(car(lap + 1, 60.0 + lap as f32 + c as f32));
            laps.record(t, false, Some(1), Some(incidents), line);
        };
        let plain = |_: usize, _: f32| car(0, 0.0);
        // Lap 0 is the first seen (partial); laps 1 to 4 each with one thing seen.
        run(
            &mut laps,
            0,
            &|c, _| CarSample {
                completed: 0,
                ..plain(c, 0.0)
            },
            false,
            0,
        );
        run(
            &mut laps,
            1,
            &|c, s| CarSample {
                completed: 1,
                on_pit: s == 3.0,
                ..plain(c, 0.0)
            },
            false,
            0,
        );
        run(
            &mut laps,
            2,
            &|c, s| CarSample {
                completed: 2,
                off_track: s == 5.0,
                ..plain(c, 0.0)
            },
            false,
            0,
        );
        run(
            &mut laps,
            3,
            &|c, _| CarSample {
                completed: 3,
                ..plain(c, 0.0)
            },
            true,
            0,
        );
        run(
            &mut laps,
            4,
            &|c, _| CarSample {
                completed: 4,
                ..plain(c, 0.0)
            },
            false,
            2,
        );
        run(
            &mut laps,
            5,
            &|c, _| CarSample {
                completed: 5,
                ..plain(c, 0.0)
            },
            false,
            2,
        );
        let flags = |car: usize| laps.laps(car).map(|l| l.flags).collect::<Vec<u8>>();
        // Car 1 is the player: its incidents count; car 0's laps see the same but no incident.
        assert_eq!(
            flags(1),
            [
                Lap::PARTIAL,
                Lap::PIT,
                Lap::OFF_TRACK,
                Lap::CAUTION,
                Lap::INCIDENT,
                0
            ]
        );
        assert_eq!(
            flags(0),
            [Lap::PARTIAL, Lap::PIT, Lap::OFF_TRACK, Lap::CAUTION, 0, 0]
        );
        assert!(laps.laps(1).all(|l| l.time.is_finite()));
        let clean: Vec<bool> = laps.laps(1).map(Lap::is_clean).collect();
        assert_eq!(
            clean,
            [false, false, false, true, false, true],
            "caution laps can be clean"
        );
    }

    #[test]
    fn keeps_the_last_laps_and_starts_over() {
        let mut laps = LapHistory::default();
        laps.resize(1);
        for n in 0..250 {
            let t = f64::from(n) * 10.0;
            laps.record(t, false, None, None, |_| Some(car(n, 50.0 + n as f32)));
        }
        assert_eq!(laps.count(0), LAPS);
        let first = laps.laps(0).next().expect("oldest").number;
        assert_eq!((first, laps.last(0).expect("latest").number), (150, 249));
        // Two laps between observations: the second is kept, partly seen.
        laps.record(2500.0, false, None, None, |_| Some(car(251, 1.0)));
        assert!(
            laps.last(0)
                .is_some_and(|l| l.number == 251 && l.has(Lap::PARTIAL))
        );
        // Going back in time (a rewound replay) forgets every lap.
        laps.record(100.0, false, None, None, |_| Some(car(10, 50.0)));
        assert_eq!(laps.count(0), 0);
        // Out of the world, a lap under way is partial.
        laps.record(110.0, false, None, None, |_| Some(car(10, 50.0)));
        laps.record(120.0, false, None, None, |_| None);
        laps.record(130.0, false, None, None, |_| Some(car(11, 51.0)));
        assert!(
            laps.last(0)
                .is_some_and(|l| l.number == 11 && l.has(Lap::PARTIAL))
        );
        // As documented: 24 bytes per lap.
        assert_eq!(std::mem::size_of::<Lap>(), 24);
        assert_eq!(laps.laps.len(), LAPS);
    }
}
