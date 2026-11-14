//! Sector timing from lap distance, shared by the widgets that show sector times.
//!
//! The sim publishes no sector times, only where sectors start (`SplitTimeInfo`) and each car's lap distance. A
//! [`SectorClock`] per car turns lap distance observed a few times per second into sector times by interpolating,
//! by distance, when the car crossed a sector start between two observations.

use pitot_irsdk::session::SessionInfo;

/// Sectors tracked per car; tracks have up to about ten.
pub(crate) const MAX_SECTORS: usize = 12;
/// A gap in observations longer than this restarts sector timing for a car.
const MAX_OBSERVATION_GAP: f64 = 5.0;

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
    /// Session time at which each sector's last time ended, or negative when not timed yet.
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
    /// Feeds the car's lap distance at session time `time`. `valid` is false off track, in the pits, or not in
    /// the world; timing then restarts.
    pub(crate) fn observe(&mut self, starts: &[f32], pct: f32, time: f64, valid: bool) {
        if !valid || starts.len() < 2 || !(0.0..=1.0).contains(&pct) {
            self.pct = -1.0;
            self.entered = -1.0;
            return;
        }
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
