//! Timing from lap distance, shared by the widgets that show sector times and race gaps.
//!
//! The sim publishes no sector times, only where sectors start (`SplitTimeInfo`) and each car's lap distance. A
//! [`SectorClock`] per car turns lap distance observed a few times per second into sector times by interpolating,
//! by distance, when the car crossed a sector start between two observations.
//!
//! A [`RaceClock`] does the same for checkpoints all around the lap, so the time gap between two cars can be read
//! at any moment: the time since the car ahead passed the spot where the car behind is now. In a race that is the
//! gap in the race (laps count); on track it is the gap by lap distance alone, whatever the laps. It also keeps
//! each car's last usable lap as checkpoint times, so the time a car takes between two spots can be estimated
//! when its latest pass was not seen.
//!
//! Times are tick time: seconds on the sim's tick count (`View::tick_time`), which the sim's own lap times and
//! results follow. `SessionTime` would put a whole tick into a time now and then, as it slips against the ticks.
//! Only differences are used.

use pitot_irsdk::session::SessionInfo;

/// Sectors tracked per car; tracks have up to about ten.
pub(crate) const MAX_SECTORS: usize = 12;
/// A gap in observations longer than this restarts sector timing for a car.
const MAX_OBSERVATION_GAP: f64 = 5.0;
/// Lap distances taken as on the lap. At the line the sim can report a lap distance a hair below zero (or above
/// one); -1 means the car is not in the world.
pub(crate) const LAP_DISTANCE: std::ops::RangeInclusive<f32> = -0.01..=1.01;

/// Where the sectors of the track start, as lap distance, in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SectorStarts {
    starts: [f32; MAX_SECTORS],
    count: usize,
}

impl Default for SectorStarts {
    fn default() -> Self {
        Self {
            starts: [0.0; MAX_SECTORS],
            count: 0,
        }
    }
}

impl SectorStarts {
    /// Reads the sector starts from the session's `SplitTimeInfo`, without allocating.
    pub(crate) fn load(&mut self, info: &SessionInfo) {
        self.count = 0;
        for sector in info.sectors.iter().take(MAX_SECTORS) {
            if let Some(start) = sector.start_pct {
                self.starts[self.count] = start as f32;
                self.count += 1;
            }
        }
        self.starts[..self.count].sort_unstable_by(f32::total_cmp);
    }

    pub(crate) fn as_slice(&self) -> &[f32] {
        &self.starts[..self.count]
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }
}

/// Times one car's sectors from its lap distance, observed a few times per second. Crossing times are
/// interpolated between observations by distance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SectorClock {
    pct: f32,
    time: f64,
    sector: usize,
    /// When the car entered its current sector, or negative when unknown.
    entered: f64,
    /// Last time of each sector, or 0 when not timed yet.
    pub last: [f32; MAX_SECTORS],
    /// Best time of each sector since the clock was reset, or 0 when not timed yet.
    pub best: [f32; MAX_SECTORS],
    /// Tick time at which each sector's last time ended, or negative when not timed yet.
    pub done: [f64; MAX_SECTORS],
}

impl Default for SectorClock {
    fn default() -> Self {
        Self {
            pct: -1.0,
            time: 0.0,
            sector: 0,
            entered: -1.0,
            last: [0.0; MAX_SECTORS],
            best: [0.0; MAX_SECTORS],
            done: [-1.0; MAX_SECTORS],
        }
    }
}

fn sector_of(starts: &[f32], pct: f32) -> usize {
    starts.iter().rposition(|&s| s <= pct).unwrap_or(0)
}

impl SectorClock {
    /// Feeds the car's lap distance at tick time `time`. `valid` is false off track, in the pits, or not in
    /// the world; timing then restarts. A lap distance a hair outside the lap (within [`LAP_DISTANCE`]) is the
    /// line.
    pub(crate) fn observe(&mut self, starts: &[f32], pct: f32, time: f64, valid: bool) {
        if !valid || starts.len() < 2 || !LAP_DISTANCE.contains(&pct) {
            self.pct = -1.0;
            self.entered = -1.0;
            return;
        }
        let pct = pct.clamp(0.0, 1.0);
        let sector = sector_of(starts, pct);
        let continued =
            self.pct >= 0.0 && time > self.time && time - self.time <= MAX_OBSERVATION_GAP;
        if !continued {
            self.pct = pct;
            self.time = time;
            self.sector = sector;
            self.entered = -1.0;
            return;
        }
        if sector != self.sector {
            let next = (self.sector + 1) % starts.len();
            if sector == next {
                // Distance travelled since the last observation, and to the boundary, across the line if needed.
                let wrap = |d: f32| if d < 0.0 { d + 1.0 } else { d };
                let boundary = starts[next];
                let travelled = wrap(pct - self.pct);
                let to_boundary = wrap(boundary - self.pct);
                let fraction = if travelled > 0.0 {
                    (to_boundary / travelled).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let crossed = self.time + (time - self.time) * f64::from(fraction);
                if self.entered >= 0.0 {
                    let s = self.sector;
                    let t = (crossed - self.entered) as f32;
                    self.last[s] = t;
                    self.done[s] = crossed;
                    if self.best[s] <= 0.0 || t < self.best[s] {
                        self.best[s] = t;
                    }
                }
                self.entered = crossed;
            } else {
                // Skipped a sector (or went backwards): the next crossing starts timing again.
                self.entered = -1.0;
            }
            self.sector = sector;
        }
        self.pct = pct;
        self.time = time;
    }

    /// The sector the car is in, while it is being timed.
    pub(crate) fn current(&self) -> Option<usize> {
        (self.pct >= 0.0).then_some(self.sector)
    }
}

/// Checkpoints per lap for race gaps: one every tenth of a percent of the lap (4 to 7 m on most tracks).
const CHECKPOINTS: usize = 1000;
/// Pass times are kept in this unit after [`RaceClock`]'s base, in an `i32`: rounded to 0.05 ms, whatever the
/// session's length.
const TIME_UNIT: f64 = 1.0e-4;
/// An `i32` of [`TIME_UNIT`] holds 59 hours; a clock running longer than this since its base starts over.
const MAX_SPAN: f64 = 48.0 * 3600.0;
/// No pass in a checkpoint slot.
const NO_PASS: i16 = i16::MIN;
/// Within this lap distance of the line, `CarIdxLapDistPct` can wrap a moment before or after
/// `CarIdxLapCompleted` counts the lap.
const NEAR_LINE: f32 = 0.1;
/// The two agree again within a moment; a disagreement that lasts longer than this is real (a car towed across the
/// line) and the sim's counters are taken as they are.
const LINE_SETTLE: f64 = 1.0;
/// Longest time between observations within a lap kept as a lap profile: longer stretches are interpolated
/// linearly and lose the lap's shape.
const MAX_PROFILE_GAP: f64 = 1.0;

/// A car's place in the race for ordering, highest first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Standing {
    /// Laps completed plus lap distance, as last seen.
    Running(f64),
    /// Took the checkered flag with this many laps at this tick time.
    Finished { laps: i32, at: f64 },
}

impl Standing {
    /// Progress, then for finishers the earlier finish first: compare these descending.
    pub(crate) fn key(self) -> (f64, f64) {
        match self {
            Self::Running(progress) => (progress, f64::NEG_INFINITY),
            Self::Finished { laps, at } => (f64::from(laps), -at),
        }
    }
}

/// A gap between two cars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Gap {
    /// Seconds since the car ahead was where the car behind is.
    Time(f64),
    /// The car ahead is this many whole laps ahead.
    Laps(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Progress {
    /// Laps completed plus lap distance, kept continuous across the line; NaN until seen.
    value: f64,
    /// Tick time of the last observation, or of the finish.
    time: f64,
    /// The last observation was valid, so the next one continues from it.
    tracking: bool,
    finished: bool,
    /// Tick time since which `value` has been a lap off the sim's counters, or NaN.
    corrected: f64,
    /// The last observation was on the racing surface.
    clean: bool,
    /// Progress at the last observation that ends a stretch a lap profile cannot use (off the racing surface,
    /// tracking restarted, or a long time since the previous observation), or NaN.
    unclean: f64,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            value: f64::NAN,
            time: 0.0,
            tracking: false,
            finished: false,
            corrected: f64::NAN,
            clean: false,
            unclean: f64::NAN,
        }
    }
}

impl Progress {
    /// A car that took the checkered flag at `line` laps at tick time `at`.
    fn finished(line: f64, at: f64) -> Self {
        Self {
            value: line,
            time: at,
            finished: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Finish {
    /// No checkered flag yet.
    Pending,
    /// The overall leader took the checkered flag at this tick time.
    At(f64),
    /// The checkered flag came out before the leader's finish could be timed.
    Unknown,
}

/// Every car's progress through a race and the times it passed checkpoints around the lap, observed a few times
/// per second. Gives the live order and time gaps that do not wait for the line.
///
/// Storage is sized once per connection by [`RaceClock::resize`]; observing does not allocate. A checkpoint pass
/// takes 6 bytes: its time as an `i32` of [`TIME_UNIT`] after `base`, and its lap as an `i16`.
#[derive(Debug)]
pub(crate) struct RaceClock {
    cars: Vec<Progress>,
    /// When each car last passed each checkpoint, `CHECKPOINTS` per car: [`TIME_UNIT`]s after `base`.
    times: Vec<i32>,
    /// The lap each of those passes belongs to (the pass is at `laps * CHECKPOINTS + checkpoint` checkpoints), or
    /// [`NO_PASS`].
    pass_laps: Vec<i16>,
    /// Each car's last usable lap: seconds from the line to each checkpoint and back to the line, `CHECKPOINTS + 1`
    /// per car.
    profiles: Vec<f32>,
    /// The lap each car's profile is of, or `i32::MIN` for none yet.
    profile_laps: Vec<i32>,
    /// Tick time the pass times count from: the first observation since the clock started over, or NaN before it.
    base: f64,
    last: f64,
    finish: Finish,
}

impl Default for RaceClock {
    fn default() -> Self {
        Self {
            cars: Vec::new(),
            times: Vec::new(),
            pass_laps: Vec::new(),
            profiles: Vec::new(),
            profile_laps: Vec::new(),
            base: f64::NAN,
            last: f64::NEG_INFINITY,
            finish: Finish::Pending,
        }
    }
}

impl RaceClock {
    /// Sizes the storage for `cars` cars and forgets everything.
    pub(crate) fn resize(&mut self, cars: usize) {
        self.cars.clear();
        self.cars.resize(cars, Progress::default());
        self.times.clear();
        self.times.resize(cars * CHECKPOINTS, 0);
        self.pass_laps.clear();
        self.pass_laps.resize(cars * CHECKPOINTS, NO_PASS);
        self.profiles.clear();
        self.profiles.resize(cars * (CHECKPOINTS + 1), 0.0);
        self.profile_laps.clear();
        self.profile_laps.resize(cars, i32::MIN);
        self.base = f64::NAN;
        self.last = f64::NEG_INFINITY;
        self.finish = Finish::Pending;
    }

    /// Bytes the clock keeps for its cars.
    #[cfg(test)]
    fn storage_bytes(&self) -> usize {
        use std::mem::size_of_val;
        size_of_val(self.cars.as_slice())
            + size_of_val(self.times.as_slice())
            + size_of_val(self.pass_laps.as_slice())
            + size_of_val(self.profiles.as_slice())
            + size_of_val(self.profile_laps.as_slice())
    }

    pub(crate) fn len(&self) -> usize {
        self.cars.len()
    }

    /// Tick time of the latest observation.
    #[cfg(test)]
    pub(crate) fn now(&self) -> f64 {
        self.last
    }

    /// Forgets everything, keeping the storage.
    pub(crate) fn reset(&mut self) {
        self.cars.iter_mut().for_each(|c| *c = Progress::default());
        self.pass_laps.iter_mut().for_each(|l| *l = NO_PASS);
        self.profile_laps.iter_mut().for_each(|l| *l = i32::MIN);
        self.base = f64::NAN;
        self.last = f64::NEG_INFINITY;
        self.finish = Finish::Pending;
    }

    /// Tick time `t` as kept in `times`.
    fn stamp(&self, t: f64) -> i32 {
        // Within `MAX_SPAN` of the base this fits; `as` saturates otherwise.
        ((t - self.base) / TIME_UNIT).round() as i32
    }

    /// The tick time a value of `times` stands for.
    fn time_of(&self, stamp: i32) -> f64 {
        self.base + f64::from(stamp) * TIME_UNIT
    }

    /// Feeds every car at tick time `now`. `sample(car)` gives `CarIdxLapCompleted`, `CarIdxLapDistPct`, and
    /// whether the car is on the racing surface (on track, not on pit road) for a car in the world, and `None` for
    /// a car that is not (its place is kept) or not racing (the pace car). `checkered` is true once the session
    /// state is checkered or cool-down.
    pub(crate) fn observe(
        &mut self,
        now: f64,
        checkered: bool,
        sample: impl Fn(usize) -> Option<(i32, f32, bool)>,
    ) {
        if now < self.last || now - self.base > MAX_SPAN {
            // Time went back (a replay was rewound): the history no longer applies. Or the pass times would no
            // longer fit their `i32`s (two days on).
            self.reset();
        } else if now == self.last {
            // The same moment again (two widgets reading one tick) would break continuity at the line.
            return;
        }
        if self.base.is_nan() {
            self.base = now;
        }
        self.last = now;
        let finish = self.finish;
        for car in 0..self.cars.len() {
            let c = self.cars[car];
            if c.finished {
                continue;
            }
            // At the line the sim can report a lap distance a hair below zero; -1 means not in the world.
            let Some((laps, pct, clean)) =
                sample(car).filter(|&(_, p, _)| LAP_DISTANCE.contains(&p))
            else {
                self.cars[car].tracking = false;
                continue;
            };
            let pct = pct.clamp(0.0, 1.0);
            let raw = f64::from(laps) + f64::from(pct);
            let continued = c.tracking && now > c.time && now - c.time <= MAX_OBSERVATION_GAP;
            let mut value = raw;
            let mut corrected = f64::NAN;
            if continued && !(NEAR_LINE..=1.0 - NEAR_LINE).contains(&pct) {
                // The lap count and the lap distance can disagree by a lap for a moment at the line.
                for candidate in [raw - 1.0, raw + 1.0] {
                    if (candidate - c.value).abs() < (value - c.value).abs() {
                        value = candidate;
                    }
                }
                if value != raw {
                    let since = if c.corrected.is_nan() {
                        now
                    } else {
                        c.corrected
                    };
                    if now - since <= LINE_SETTLE {
                        corrected = since;
                    } else {
                        value = raw;
                    }
                }
            }
            // The stretch since the previous observation spoils the lap profile when either end is off the racing
            // surface, when tracking starts again, or when the observations are far apart.
            let unclean = if continued && clean && c.clean && now - c.time <= MAX_PROFILE_GAP {
                c.unclean
            } else {
                value
            };
            let mut next = Progress {
                value,
                time: now,
                tracking: true,
                finished: false,
                corrected,
                clean,
                unclean,
            };
            if continued && value > c.value && value - c.value < 0.5 {
                self.pass(car, (c.value, c.time), (value, now), unclean);
                // After the leader's finish, the next time a car reaches the line is its finish.
                if let Finish::At(at) = finish {
                    let line = value.floor();
                    if line > c.value
                        && let Some(crossed) = self.crossing(car, line)
                        && crossed >= at
                    {
                        next = Progress::finished(line, crossed);
                    }
                }
            }
            self.cars[car] = next;
        }
        if checkered && self.finish == Finish::Pending {
            self.time_finish();
        } else if !checkered && self.finish != Finish::Pending {
            // Back before the checkered flag: the finishes no longer apply.
            self.reset();
        }
    }

    /// The checkered flag is out: the leader finished when it last reached the line, and so did every car that
    /// reached it since.
    fn time_finish(&mut self) {
        let leader = (0..self.cars.len())
            .filter(|&c| self.cars[c].value.is_finite())
            .max_by(|&a, &b| self.cars[a].value.total_cmp(&self.cars[b].value));
        let at = leader.and_then(|c| self.crossing(c, self.cars[c].value.floor()));
        let Some(at) = at else {
            self.finish = Finish::Unknown;
            return;
        };
        self.finish = Finish::At(at);
        for car in 0..self.cars.len() {
            let line = self.cars[car].value.floor();
            if let Some(crossed) = self.crossing(car, line).filter(|&t| t >= at) {
                self.cars[car] = Progress::finished(line, crossed);
            }
        }
    }

    /// Whether the order is known: false when the checkered flag came out before the clock could time the
    /// leader's finish (the overlay connected too late), so finishers cannot be told from cars still running.
    pub(crate) fn is_live(&self) -> bool {
        self.finish != Finish::Unknown
    }

    /// Records when a car passed each checkpoint between two observations, `(progress, time)` each, interpolated by
    /// distance. Reaching the line keeps the lap just completed as the car's profile when it is usable: every
    /// checkpoint of it passed, and nothing since `unclean` (see [`Progress::unclean`]) touches it.
    fn pass(&mut self, car: usize, (from, t0): (f64, f64), (to, t1): (f64, f64), unclean: f64) {
        let k = CHECKPOINTS as f64;
        let first = (from * k).floor() as i32 + 1;
        let last = (to * k).floor() as i32;
        for mark in first..=last {
            let at = mark as f64 / k;
            let time = t0 + (t1 - t0) * (at - from) / (to - from);
            let checkpoint = mark.rem_euclid(CHECKPOINTS as i32) as usize;
            let lap = mark.div_euclid(CHECKPOINTS as i32);
            if checkpoint == 0 && unclean < f64::from(lap - 1) {
                self.keep_profile(car, lap - 1, time);
            }
            let slot = car * CHECKPOINTS + checkpoint;
            let stamp = self.stamp(time);
            if let (Some(t), Some(l)) = (self.times.get_mut(slot), self.pass_laps.get_mut(slot)) {
                *t = stamp;
                *l = i16::try_from(lap).unwrap_or(NO_PASS);
            }
        }
    }

    /// Keeps `lap`, which `car` completed at tick time `end`, as its profile if every checkpoint of the lap was
    /// passed. Runs before the line's checkpoint is overwritten with the next lap.
    fn keep_profile(&mut self, car: usize, lap: i32, end: f64) {
        let first = car * CHECKPOINTS;
        let Ok(lap16) = i16::try_from(lap) else {
            return;
        };
        let (Some(laps), Some(times)) = (
            self.pass_laps.get(first..first + CHECKPOINTS),
            self.times.get(first..first + CHECKPOINTS),
        ) else {
            return;
        };
        let complete = lap16 != NO_PASS && laps.iter().all(|&l| l == lap16);
        let start = self.base + f64::from(times[0]) * TIME_UNIT;
        if !complete || end <= start {
            return;
        }
        let at = car * (CHECKPOINTS + 1);
        let Some(profile) = self.profiles.get_mut(at..at + CHECKPOINTS + 1) else {
            return;
        };
        for (out, &stamp) in profile.iter_mut().zip(times) {
            *out = (f64::from(stamp - times[0]) * TIME_UNIT) as f32;
        }
        profile[CHECKPOINTS] = (end - start) as f32;
        if let Some(l) = self.profile_laps.get_mut(car) {
            *l = lap;
        }
    }

    /// The lap `car`'s profile is of.
    #[cfg(test)]
    fn profile_lap(&self, car: usize) -> Option<i32> {
        self.profile_laps
            .get(car)
            .copied()
            .filter(|&l| l != i32::MIN)
    }

    /// Seconds `car` took on its last usable lap to drive from lap distance `from` forward to lap distance `to`,
    /// across the line when `to` is before `from`, interpolated between checkpoints. `None` until the car has
    /// completed a usable lap: every checkpoint passed, observed at most [`MAX_PROFILE_GAP`] apart, and on the racing
    /// surface throughout (no pit road, pit stall, pit approach, or off track).
    pub(crate) fn lap_time_between(&self, car: usize, from: f32, to: f32) -> Option<f64> {
        if *self.profile_laps.get(car)? == i32::MIN {
            return None;
        }
        let at = car * (CHECKPOINTS + 1);
        let profile = self.profiles.get(at..at + CHECKPOINTS + 1)?;
        let elapsed = |pct: f32| {
            let x = f64::from(pct.clamp(0.0, 1.0)) * CHECKPOINTS as f64;
            let i = (x.floor() as usize).min(CHECKPOINTS - 1);
            let (t0, t1) = (f64::from(profile[i]), f64::from(profile[i + 1]));
            t0 + (t1 - t0) * (x - i as f64)
        };
        let (start, end) = (elapsed(from), elapsed(to));
        Some(if to >= from {
            end - start
        } else {
            f64::from(profile[CHECKPOINTS]) - start + end
        })
    }

    /// When `car` last passed the checkpoint at `progress` (a multiple of a checkpoint), if that pass was seen.
    fn crossing(&self, car: usize, progress: f64) -> Option<f64> {
        let mark = (progress * CHECKPOINTS as f64).round() as i32;
        let slot = car * CHECKPOINTS + mark.rem_euclid(CHECKPOINTS as i32) as usize;
        let lap = i16::try_from(mark.div_euclid(CHECKPOINTS as i32))
            .ok()
            .filter(|&l| l != NO_PASS)?;
        (self.pass_laps.get(slot) == Some(&lap))
            .then(|| self.times.get(slot).map(|&s| self.time_of(s)))
            .flatten()
    }

    /// Tick time at which `car` was at `progress` (laps plus lap distance), interpolated between checkpoints.
    pub(crate) fn time_at(&self, car: usize, progress: f64) -> Option<f64> {
        let c = self.cars.get(car)?;
        if !c.value.is_finite() || !progress.is_finite() || progress > c.value {
            return None;
        }
        let k = CHECKPOINTS as f64;
        let mark = (progress * k).floor();
        let (p0, p1) = (mark / k, (mark + 1.0) / k);
        let t0 = self.crossing(car, p0)?;
        let (t1, p1) = if c.value >= p1 {
            (self.crossing(car, p1)?, p1)
        } else {
            (c.time, c.value)
        };
        if p1 <= p0 || t1 < t0 {
            return Some(t0);
        }
        Some(t0 + (t1 - t0) * ((progress - p0) / (p1 - p0)).clamp(0.0, 1.0))
    }

    pub(crate) fn standing(&self, car: usize) -> Option<Standing> {
        let c = self.cars.get(car)?;
        if !c.value.is_finite() {
            None
        } else if c.finished {
            Some(Standing::Finished {
                laps: c.value as i32,
                at: c.time,
            })
        } else {
            Some(Standing::Running(c.value))
        }
    }

    /// Race gap from `ahead` back to `behind` at the latest observation: the time since `ahead` was where
    /// `behind` is (where it finished, for a finisher), or whole laps when `ahead` is a lap or more ahead. `None`
    /// when a car is unseen, `ahead` is not ahead, or its pass of that spot was not seen.
    pub(crate) fn gap(&self, ahead: usize, behind: usize) -> Option<Gap> {
        let (a, b) = (self.cars.get(ahead)?, self.cars.get(behind)?);
        if !a.value.is_finite() || !b.value.is_finite() {
            return None;
        }
        let laps = a.value - b.value;
        if laps >= 1.0 {
            return Some(Gap::Laps(laps as u32));
        }
        let at = if b.finished { b.time } else { self.last };
        let passed = self.time_at(ahead, b.value)?;
        Some(Gap::Time((at - passed).max(0.0)))
    }

    /// Gap on track from `ahead` back to `behind`, whatever their laps: the time since `ahead` last passed the
    /// lap distance where `behind` is now. Both must be in the world at the latest observation; `None` when that
    /// pass was not seen.
    pub(crate) fn track_gap(&self, ahead: usize, behind: usize) -> Option<f64> {
        let (a, b) = (self.cars.get(ahead)?, self.cars.get(behind)?);
        if !a.tracking || !b.tracking || a.time != self.last || b.time != self.last {
            return None;
        }
        // The latest progress of `ahead` at the lap distance of `behind`.
        let spot = b.value - b.value.floor();
        let mut at = a.value.floor() + spot;
        if at > a.value {
            at -= 1.0;
        }
        let passed = self.time_at(ahead, at)?;
        Some((self.last - passed).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sectors_are_timed_between_crossings() {
        let starts = [0.0, 0.3, 0.7];
        let mut clock = SectorClock::default();
        // 100 s laps, observed every half second: 0.005 of a lap per observation.
        let mut t = 0.0;
        let mut pct = 0.01;
        for _ in 0..450 {
            clock.observe(&starts, pct, t, true);
            t += 0.5;
            pct = (pct + 0.005) % 1.0;
        }
        // Each sector takes its share of the 100 s lap.
        assert!((clock.last[0] - 30.0).abs() < 0.01, "{:?}", clock.last);
        assert!((clock.last[1] - 40.0).abs() < 0.01, "{:?}", clock.last);
        assert!((clock.last[2] - 30.0).abs() < 0.01, "{:?}", clock.last);
        assert_eq!(clock.best[..3], clock.last[..3]);
        // The lap started at 0.01 at 0 s: sector 0 last ended at 129 s, sector 2 at the line at 199 s.
        assert!((clock.done[0] - 129.0).abs() < 0.01, "{:?}", clock.done);
        assert!((clock.done[2] - 199.0).abs() < 0.01, "{:?}", clock.done);
        assert_eq!(clock.current(), Some(sector_of(&starts, pct)));

        // A long gap restarts timing: the next crossing gives no time.
        let mut clock = SectorClock::default();
        clock.observe(&starts, 0.25, 0.0, true);
        clock.observe(&starts, 0.29, 10.0, true);
        clock.observe(&starts, 0.31, 10.5, true);
        assert_eq!(clock.last, [0.0; MAX_SECTORS]);
        // In the pits nothing is timed.
        clock.observe(&starts, 0.71, 11.0, false);
        assert_eq!(clock.last, [0.0; MAX_SECTORS]);
        assert_eq!(clock.current(), None);
    }

    #[test]
    fn a_lap_distance_a_hair_below_zero_at_the_line_is_the_line() {
        // 100 s laps at a steady speed, sectors from 0 and 0.5, observed every second from 0.30 on and more often
        // around the line: 0.998, then -1e-6 where the car is at the line, then 0.004.
        let starts = [0.0, 0.5];
        let mut clock = SectorClock::default();
        let times = (30..100)
            .map(f64::from)
            .chain([99.8, 100.0, 100.4])
            .chain((101..=160).map(f64::from));
        for t in times {
            let pct = if t == 100.0 {
                -1.0e-6
            } else {
                (t / 100.0).fract() as f32
            };
            clock.observe(&starts, pct, t, LAP_DISTANCE.contains(&pct));
        }
        // Sector 1 from 50 s to the line at 100 s, then sector 0 from the line to 150 s.
        assert!((clock.last[1] - 50.0).abs() < 0.01, "{:?}", clock.last);
        assert!((clock.last[0] - 50.0).abs() < 0.01, "{:?}", clock.last);
        assert!((clock.done[1] - 100.0).abs() < 0.01, "{:?}", clock.done);
        // -1 still means not in the world: timing restarts.
        clock.observe(&starts, -1.0, 160.1, LAP_DISTANCE.contains(&-1.0));
        assert_eq!(clock.current(), None);
    }

    #[test]
    fn best_keeps_the_fastest_time() {
        let starts = [0.0, 0.5];
        let mut clock = SectorClock::default();
        // Half-lap sectors at constant speed within each, observed every 0.1 s: sector 0 in 40 s (not timed, as
        // tracking starts in it), then 42 s, then 45 s; sector 1 always in 50 s.
        let mut t = 0.0;
        for (sector, seconds) in [(0, 40), (1, 50), (0, 42), (1, 50), (0, 45), (1, 50)] {
            let steps = seconds * 10;
            let base = 0.5 * sector as f32;
            for k in 0..steps {
                clock.observe(&starts, base + 0.5 * k as f32 / steps as f32, t, true);
                t += 0.1;
            }
        }
        clock.observe(&starts, 0.0, t, true);
        assert!((clock.best[0] - 42.0).abs() < 0.01, "{:?}", clock.best);
        assert!((clock.last[0] - 45.0).abs() < 0.01, "{:?}", clock.last);
        assert!((clock.best[1] - 50.0).abs() < 0.01, "{:?}", clock.best);
    }

    /// Lap count and lap distance of a car at `progress` laps, on the racing surface.
    fn sample(progress: f64) -> Option<(i32, f32, bool)> {
        let laps = progress.floor();
        Some((laps as i32, (progress - laps) as f32, true))
    }

    /// Where a car is at time `t` when it reaches progress `p` at `p * 100 + 5 sin(2 pi p) + offset` seconds:
    /// 100 s laps with the speed changing around the lap.
    fn progress_at(t: f64, offset: f64) -> f64 {
        let time_of = |p: f64| p * 100.0 + 5.0 * (std::f64::consts::TAU * p).sin() + offset;
        let (mut lo, mut hi) = (-2.0, 20.0);
        for _ in 0..80 {
            let mid = (lo + hi) / 2.0;
            if time_of(mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    #[test]
    fn race_gaps_are_the_time_since_the_car_ahead_was_there() {
        // Car 0 leads car 1 by 2 s on the same varying-speed lap; car 2 is 1.5 laps behind car 0.
        let mut clock = RaceClock::default();
        clock.resize(3);
        let offsets = [0.0, 2.0, 150.0];
        let mut worst: f64 = 0.0;
        let mut t = 200.0;
        while t < 600.0 {
            clock.observe(t, false, |car| sample(progress_at(t, offsets[car])));
            if t > 210.0 {
                let Some(Gap::Time(gap)) = clock.gap(0, 1) else {
                    panic!("a time gap at {t}: {:?}", clock.gap(0, 1));
                };
                worst = worst.max((gap - 2.0).abs());
                assert_eq!(clock.gap(0, 2), Some(Gap::Laps(1)));
                assert_eq!(clock.gap(1, 0), None, "car 0 is not behind car 1");
            }
            t += 0.5;
        }
        // Linear interpolation between half-second observations on a lap whose speed changes by 30%.
        assert!(worst < 0.02, "worst gap error {worst}");
        let (Some(a), Some(b)) = (clock.standing(0), clock.standing(1)) else {
            panic!("both seen");
        };
        assert!(a.key() > b.key());
    }

    #[test]
    fn track_gaps_ignore_laps() {
        // On the varying-speed lap: car 1 reaches every spot 3 s before the player (car 0) but is a lap down; car 2
        // reaches every spot 5 s after the player but is a lap up.
        let mut clock = RaceClock::default();
        clock.resize(3);
        let place = |car: usize, t: f64| match car {
            0 => progress_at(t, 0.0),
            1 => progress_at(t, -3.0) - 1.0,
            _ => progress_at(t, 5.0) + 1.0,
        };
        let mut worst: f64 = 0.0;
        let mut t = 200.0;
        while t < 600.0 {
            clock.observe(t, false, |car| sample(place(car, t)));
            if t > 210.0 {
                let ahead = clock.track_gap(1, 0).unwrap_or(f64::NAN);
                let behind = clock.track_gap(0, 2).unwrap_or(f64::NAN);
                worst = worst.max((ahead - 3.0).abs()).max((behind - 5.0).abs());
            }
            // The relative's rate.
            t += 0.1;
        }
        assert!(worst < 0.005, "worst track gap error {worst}");
        // In the race the lapped car is 97 s behind the player, whatever the track says.
        let race_gap = time_gap(clock.gap(0, 1));
        assert!((race_gap - 97.0).abs() < 0.01, "{race_gap}");
        // Until the car ahead has passed the spot since the clock started, there is no gap.
        let mut fresh = RaceClock::default();
        fresh.resize(2);
        for (n, t) in [0.0, 0.1, 0.2].into_iter().enumerate() {
            fresh.observe(t, false, |car| {
                sample(0.30 + 0.001 * n as f64 + 0.05 * car as f64)
            });
        }
        assert_eq!(fresh.track_gap(1, 0), None);
    }

    #[test]
    fn lap_profiles_keep_the_last_usable_lap() {
        // Four cars together on the varying-speed lap, observed at 10 Hz from about 2.34 laps on; they reach laps 3,
        // 4, 5, and 6 at 263, 363, 463, and 563 s. Car 1 is on pit road for a moment every lap, car 2 runs off track
        // once on lap 4, and car 3 drops out of the world for two seconds on lap 3.
        let mut clock = RaceClock::default();
        clock.resize(4);
        for n in 2000..=5700 {
            let t = f64::from(n) * 0.1;
            let p = progress_at(t, -37.0);
            let (laps, pct) = (p.floor(), p - p.floor());
            clock.observe(t, false, |car| {
                let clean = match car {
                    1 => !(0.60..0.62).contains(&pct),
                    2 => !(4.30..4.31).contains(&p),
                    _ => true,
                };
                if car == 3 && (310.0..312.0).contains(&t) {
                    return None;
                }
                Some((laps as i32, pct as f32, clean))
            });
            let laps: [Option<i32>; 4] = std::array::from_fn(|car| clock.profile_lap(car));
            match n {
                // The first lap started before the clock did, and the second is not complete yet.
                2620 | 3000 => assert_eq!(laps, [None; 4], "{t}"),
                4000 => assert_eq!(laps, [Some(3), None, Some(3), None], "{t}"),
                5000 => assert_eq!(laps, [Some(4), None, Some(3), Some(4)], "{t}"),
                5700 => assert_eq!(laps, [Some(5), None, Some(5), Some(5)], "{t}"),
                _ => {}
            }
        }
        // Lap 5 of car 0, interpolated between checkpoints: from p to q takes (q - p) * 100 + 5 (sin 2 pi q - sin 2
        // pi p) seconds.
        let expected = |p: f64, q: f64| {
            (q - p) * 100.0
                + 5.0 * ((std::f64::consts::TAU * q).sin() - (std::f64::consts::TAU * p).sin())
        };
        let between = |from: f32, to: f32| clock.lap_time_between(0, from, to).unwrap_or(f64::NAN);
        assert!((between(0.25, 0.40) - expected(0.25, 0.40)).abs() < 0.005);
        // Across the line, from the end of the lap into its start.
        assert!((between(0.95, 0.05) - expected(0.95, 1.05)).abs() < 0.005);
        assert!((between(0.0, 1.0) - 100.0).abs() < 0.005);
        assert_eq!(between(0.5, 0.5), 0.0);
        assert_eq!(
            clock.lap_time_between(1, 0.25, 0.40),
            None,
            "never a usable lap"
        );
        assert_eq!(clock.lap_time_between(9, 0.25, 0.40), None, "no such car");
        // Long gaps between observations lose the lap's shape: no profile.
        let mut sparse = RaceClock::default();
        sparse.resize(1);
        for n in 130..=380 {
            let t = f64::from(n) * 1.5;
            sparse.observe(t, false, |_| sample(progress_at(t, -37.0)));
        }
        assert_eq!(sparse.profile_lap(0), None);
        // A reset forgets the profiles.
        clock.reset();
        assert_eq!(clock.lap_time_between(0, 0.25, 0.40), None);
    }

    #[test]
    fn storage_for_72_cars() {
        let mut clock = RaceClock::default();
        clock.resize(72);
        // Per car: a 4-byte time and a 2-byte lap per checkpoint, the lap profile, its lap, and the progress.
        let per_car =
            CHECKPOINTS * (4 + 2) + (CHECKPOINTS + 1) * 4 + 4 + std::mem::size_of::<Progress>();
        assert_eq!(clock.storage_bytes(), 72 * per_car);
        assert!(
            clock.storage_bytes() < 720 * 1024,
            "{}",
            clock.storage_bytes()
        );
    }

    #[test]
    fn pass_times_keep_their_precision_in_long_sessions() {
        // Two cars 2 s apart on 100 s laps, first seen at 1,000 s and followed again 40 hours later; then the same
        // 70 hours later, past the 59 hours an `i32` of pass time holds: the clock started over at two days.
        for hours in [40.0, 70.0] {
            let mut clock = RaceClock::default();
            clock.resize(2);
            clock.observe(1000.0, false, |car| sample(0.5 - 0.02 * car as f64));
            let start = 1000.0 + hours * 3600.0;
            for n in 0..=1500 {
                let t = start + f64::from(n) * 0.1;
                clock.observe(t, false, |car| {
                    sample(0.5 + (t - start) / 100.0 - 0.02 * car as f64)
                });
            }
            let gap = clock.track_gap(0, 1).unwrap_or(f64::NAN);
            assert!((gap - 2.0).abs() < 1e-4, "{hours} h: {gap}");
            let lap = clock.time_at(0, 1.9).unwrap_or(f64::NAN)
                - clock.time_at(0, 1.4).unwrap_or(f64::NAN);
            assert!((lap - 50.0).abs() < 1e-4, "{hours} h: {lap}");
        }
    }

    #[test]
    fn progress_stays_continuous_when_the_line_counters_disagree() {
        let mut clock = RaceClock::default();
        clock.resize(2);
        // Car 0 crosses the line: its lap distance wraps one observation before its lap count goes up. Car 1,
        // behind it, has its lap count go up one observation before its lap distance wraps.
        let steps = [
            ((4, 0.98), (4, 0.97)),
            ((4, 0.003), (5, 0.985)),
            ((5, 0.008), (5, 0.995)),
            ((5, 0.013), (5, 0.004)),
        ];
        for (n, (a, b)) in steps.into_iter().enumerate() {
            let t = 10.0 + n as f64 * 0.5;
            clock.observe(t, false, |car| {
                let (laps, pct) = if car == 0 { a } else { b };
                Some((laps, pct, true))
            });
            let (Some(Standing::Running(pa)), Some(Standing::Running(pb))) =
                (clock.standing(0), clock.standing(1))
            else {
                panic!("both running");
            };
            assert!(pa > pb, "step {n}: {pa} {pb}");
            assert!(pa - pb < 0.05, "step {n}: {pa} {pb}");
        }
        // A car moved across the line without the lap counting (towed into a pit stall just past it): after a
        // moment the sim's counters are taken as they are.
        let mut towed = RaceClock::default();
        towed.resize(1);
        towed.observe(0.0, false, |_| Some((5, 0.95, true)));
        let mut t = 0.5;
        while t < 10.0 {
            towed.observe(t, false, |_| Some((5, 0.05, true)));
            t += 0.5;
        }
        assert!(running(towed.standing(0)).is_some_and(|p| (p - 5.05).abs() < 1e-6));
        // Without a previous observation there is nothing to correct against.
        let mut fresh = RaceClock::default();
        fresh.resize(1);
        fresh.observe(0.0, false, |_| Some((4, 0.003, true)));
        assert!(running(fresh.standing(0)).is_some_and(|p| (p - 4.003).abs() < 1e-6));
    }

    fn running(standing: Option<Standing>) -> Option<f64> {
        match standing {
            Some(Standing::Running(p)) => Some(p),
            _ => None,
        }
    }

    fn finished(standing: Option<Standing>) -> Option<(i32, f64)> {
        match standing {
            Some(Standing::Finished { laps, at }) => Some((laps, at)),
            _ => None,
        }
    }

    fn time_gap(gap: Option<Gap>) -> f64 {
        match gap {
            Some(Gap::Time(t)) => t,
            other => panic!("a time gap: {other:?}"),
        }
    }

    #[test]
    fn cars_out_of_the_world_keep_their_place() {
        let mut clock = RaceClock::default();
        clock.resize(2);
        let mut t = 0.0;
        while t < 30.0 {
            clock.observe(t, false, |car| {
                sample(if car == 0 {
                    3.2 + t / 100.0
                } else {
                    3.1 + t / 100.0
                })
            });
            t += 0.5;
        }
        // Car 0 disconnects at 3.495: it stays there while car 1 drives past it.
        while t < 60.0 {
            clock.observe(t, false, |car| {
                (car == 1).then(|| sample(3.1 + t / 100.0)).flatten()
            });
            t += 0.5;
        }
        assert!(running(clock.standing(0)).is_some_and(|p| (p - 3.495).abs() < 1e-6));
        let (Some(a), Some(b)) = (clock.standing(0), clock.standing(1)) else {
            panic!("both seen");
        };
        assert!(b.key() > a.key(), "car 1 passed the stopped car");
        // Car 1 passed car 0's spot at 39.5 s: the stopped car's gap keeps growing.
        let gap = time_gap(clock.gap(1, 0));
        assert!((gap - (clock.now() - 39.5)).abs() < 0.01, "{gap}");
        assert_eq!(
            clock.track_gap(1, 0),
            None,
            "on track, only cars in the world"
        );
        // A time going back (a rewound replay) forgets everything.
        clock.observe(5.0, false, |_| None);
        assert_eq!(clock.standing(1), None);
    }

    #[test]
    fn finishers_keep_their_finishing_order() {
        let mut clock = RaceClock::default();
        clock.resize(3);
        // 100 s laps. Car 0 leads car 1 by 1 s; car 2 is a lap down, 30 s behind car 0 on track. The leader
        // reaches the end of lap 10 at 1000.2 s and takes the checkered flag.
        let start = [0.2, 1.2, 130.2];
        let mut t = 900.0;
        while t < 1200.0 {
            clock.observe(t, t >= 1000.2, |car| sample((t - start[car]) / 100.0));
            t += 0.5;
        }
        // Everyone kept driving after their finish; the finishes are where they first reached the line.
        let near = |got: Option<(i32, f64)>, laps: i32, at: f64| {
            got.is_some_and(|(l, a)| l == laps && (a - at).abs() < 1e-4)
        };
        assert!(
            near(finished(clock.standing(0)), 10, 1000.2),
            "{:?}",
            clock.standing(0)
        );
        assert!(
            near(finished(clock.standing(1)), 10, 1001.2),
            "{:?}",
            clock.standing(1)
        );
        assert!(
            near(finished(clock.standing(2)), 9, 1030.2),
            "{:?}",
            clock.standing(2)
        );
        let key = |car| clock.standing(car).map(Standing::key);
        assert!(key(0) > key(1) && key(1) > key(2));
        assert!((time_gap(clock.gap(0, 1)) - 1.0).abs() < 1e-4);
        assert_eq!(clock.gap(1, 2), Some(Gap::Laps(1)));
        assert!(clock.is_live());
    }

    #[test]
    fn a_finish_already_out_when_first_seen_is_unknown() {
        let mut clock = RaceClock::default();
        clock.resize(1);
        clock.observe(1000.5, true, |_| sample(10.005));
        assert!(!clock.is_live());
        clock.observe(1001.0, true, |_| sample(10.01));
        assert!(running(clock.standing(0)).is_some_and(|p| (p - 10.01).abs() < 1e-6));
    }

    #[test]
    fn starts_are_sorted() {
        let mut info = SessionInfo::default();
        for start in [0.7, 0.0, 0.3] {
            info.sectors.push(pitot_irsdk::session::Sector {
                num: None,
                start_pct: Some(start),
            });
        }
        let mut starts = SectorStarts::default();
        starts.load(&info);
        assert_eq!(starts.as_slice(), &[0.0, 0.3, 0.7]);
        assert_eq!(starts.len(), 3);
    }
}
