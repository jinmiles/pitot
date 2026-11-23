//! The [`RaceClock`] shared by the standings and the relative, kept up to date from the view.
//!
//! Whichever of the two widgets updates first observes the latest tick for both, so the checkpoint history is kept
//! once and each widget gains the other's observations (both run at 10 Hz by default, each on its own timer).
//!
//! The clock is timed on the sim's tick count ([`View::tick_time`]), as the sim's own lap times and results are.

use std::cell::RefCell;
use std::rc::Rc;

use pitot_irsdk::flags::{session_state, track_location};

use crate::pipeline::View;

use super::timing::RaceClock;

/// Tick time between observations, at least: when both widgets update within this, the second reads the first's.
const MIN_SPACING: f64 = 0.04;

/// One clock for every widget that times gaps.
pub(crate) type SharedTrackClock = Rc<RefCell<TrackClock>>;

/// A race clock fed with every car's laps and lap distance, in every session.
#[derive(Debug, Default)]
pub(crate) struct TrackClock {
    clock: RaceClock,
    session_num: Option<i32>,
    /// Tick time of the last observation.
    last_time: Option<f64>,
}

impl TrackClock {
    pub(crate) fn shared() -> SharedTrackClock {
        Rc::new(RefCell::new(Self::default()))
    }

    pub(crate) fn clock(&self) -> &RaceClock {
        &self.clock
    }

    /// Takes the latest tick unless another widget took it moments ago. Allocates only when the car count
    /// changes (once per connection).
    pub(crate) fn observe(&mut self, view: &View) {
        let (snap, vars) = (view.snapshot(), view.vars());
        let Some(info) = view.session() else {
            return;
        };
        let Some(now) = view.tick_time() else {
            return;
        };
        // A time going back (a rewound replay, a tick count that started over) is observed at once: the clock
        // starts over.
        if self
            .last_time
            .is_some_and(|last| now >= last && now - last < MIN_SPACING)
        {
            return;
        }
        self.last_time = Some(now);
        let cars = view.car_count().max(info.driver_info.drivers.len());
        if self.clock.len() != cars {
            self.clock.resize(cars);
        }
        let session_num = snap.i32(vars.session_num, 0);
        if session_num != self.session_num {
            self.session_num = session_num;
            self.clock.reset();
        }
        let session = info
            .sessions
            .iter()
            .find(|s| s.num == session_num.map(i64::from))
            .or(info.sessions.last());
        let is_race = session
            .and_then(|s| s.session_type.as_deref())
            .is_some_and(|t| t.eq_ignore_ascii_case("race"));
        // Finishes only exist in a race; practice and qualifying end with a checkered flag too.
        let checkered = is_race
            && matches!(
                snap.i32(vars.session_state, 0),
                Some(session_state::CHECKERED | session_state::COOL_DOWN)
            );
        let drivers = &info.driver_info.drivers;
        self.clock.observe(now, checkered, |car| {
            let racing = drivers
                .get(car)
                .and_then(Option::as_ref)
                .is_some_and(|d| !d.is_pace_car && !d.is_spectator);
            let surface = snap.i32(vars.car_track_surface, car);
            if !racing || surface == Some(track_location::NOT_IN_WORLD) {
                return None;
            }
            // Lap profiles only keep laps driven on track throughout.
            let on_track = surface.is_none_or(|s| s == track_location::ON_TRACK)
                && snap.bool(vars.car_on_pit_road, car) != Some(true);
            Some((
                snap.i32(vars.car_lap_completed, car)?,
                snap.f32(vars.car_lap_dist_pct, car)?,
                on_track,
            ))
        });
    }

    /// Forgets everything, for a new connection.
    pub(crate) fn reset(&mut self) {
        self.clock.reset();
        self.session_num = None;
        self.last_time = None;
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use pitot_irsdk::layout::{VarHeader, VarType};
    use pitot_irsdk::live::{ReaderConfig, SdkNames};
    use pitot_irsdk::publish::{Publisher, PublisherConfig};

    use super::*;
    use crate::pipeline::Pipeline;

    const CARS: usize = 2;
    const ROW: usize = 16 + CARS * 4 * 3;
    /// The tick count at the start: only differences matter.
    const FIRST_TICK: i32 = 123_456;
    /// `SessionTime` repeats its value at this tick and stays a tick behind until it steps twice at `CATCH_UP`, as
    /// the sim's does now and then.
    const STALL: i32 = 900;
    const CATCH_UP: i32 = 1440;

    const SESSION: &str = "---
WeekendInfo:
 TrackID: 7
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionType: Race
DriverInfo:
 DriverCarIdx: 0
 Drivers:
 - CarIdx: 0
   CarNumber: 1
 - CarIdx: 1
   CarNumber: 2
...
";

    /// `SessionTime` `n` ticks from the start.
    fn session_time(n: i32) -> f64 {
        let lag = i32::from((STALL..CATCH_UP).contains(&n));
        f64::from(n - lag) / 60.0
    }

    /// Laps, lap distance, and on track for `car` `n` ticks from the start, on 10 s laps: car 0 from 1.05 laps, car
    /// 1 three seconds behind it.
    fn sample(car: usize, n: i32) -> (i32, f32, bool) {
        let start = if car == 0 { 1.05 } else { 0.75 };
        let p = start + 0.1 * f64::from(n) / 60.0;
        (p.floor() as i32, (p - p.floor()) as f32, true)
    }

    fn row(n: i32) -> Vec<u8> {
        let mut row = vec![0u8; ROW];
        row[0..8].copy_from_slice(&session_time(n).to_le_bytes());
        row[8..12].copy_from_slice(&(FIRST_TICK + n).to_le_bytes());
        for car in 0..CARS {
            let (laps, pct, _) = sample(car, n);
            let at = |k: usize| 16 + k * CARS * 4 + car * 4;
            row[at(0)..at(0) + 4].copy_from_slice(&pct.to_le_bytes());
            row[at(1)..at(1) + 4].copy_from_slice(&laps.to_le_bytes());
            row[at(2)..at(2) + 4].copy_from_slice(&track_location::ON_TRACK.to_le_bytes());
        }
        row
    }

    #[test]
    fn laps_and_gaps_follow_the_ticks_when_session_time_slips() {
        let pid = std::process::id();
        let names = SdkNames::new(
            &format!("Local\\PitotTrackClockMap-{pid}"),
            &format!("Local\\PitotTrackClockEvent-{pid}"),
        );
        let array = |t, k: usize, name| {
            VarHeader::new(t, (16 + k * CARS * 4) as i32, CARS as i32, name).expect("valid")
        };
        let config = PublisherConfig {
            vars: vec![
                VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
                VarHeader::new(VarType::Int, 8, 1, "SessionTick").expect("valid"),
                VarHeader::new(VarType::Int, 12, 1, "SessionNum").expect("valid"),
                array(VarType::Float, 0, "CarIdxLapDistPct"),
                array(VarType::Int, 1, "CarIdxLapCompleted"),
                array(VarType::Int, 2, "CarIdxTrackSurface"),
            ],
            buf_len: ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 4096,
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

        // The shared clock on the view, and for comparison the same clock fed `SessionTime`.
        let mut clock = TrackClock::default();
        let mut on_session_time = RaceClock::default();
        on_session_time.resize(CARS);
        let mut gaps = None;
        // At 10 Hz, waiting for each tick to arrive.
        for n in (0..=1380).step_by(6) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "tick {n} arrives");
                publisher.publish(&row(n)).expect("publishes");
                thread::sleep(Duration::from_millis(2));
                view.poll();
                let tick = view.snapshot().i32(view.vars().session_tick, 0);
                if view.session().is_some() && tick == Some(FIRST_TICK + n) {
                    break;
                }
            }
            clock.observe(&view);
            on_session_time.observe(session_time(n), false, |car| Some(sample(car, n)));
            // Car 0 passed car 1's spot at 13 s, before the stall; it is 16 s now, while `SessionTime` lags.
            if n == 960 {
                gaps = Some((
                    clock.clock().track_gap(0, 1),
                    on_session_time.track_gap(0, 1),
                ));
            }
        }

        let Some((Some(gap), Some(slipped))) = gaps else {
            panic!("gaps at 16 s: {gaps:?}");
        };
        assert!((gap - 3.0).abs() < 1e-4, "on the ticks: {gap}");
        assert!(
            (slipped - (3.0 - 1.0 / 60.0)).abs() < 1e-4,
            "on SessionTime: {slipped}"
        );
        // Car 0's lap from 9.5 s to 19.5 s: the line before the stall, the next one while `SessionTime` lags.
        let lap = clock.clock().lap_time_between(0, 0.0, 1.0);
        assert!(lap.is_some_and(|l| (l - 10.0).abs() < 1e-4), "{lap:?}");
        let slipped = on_session_time.lap_time_between(0, 0.0, 1.0);
        assert!(
            slipped.is_some_and(|l| (l - (10.0 - 1.0 / 60.0)).abs() < 1e-4),
            "{slipped:?}"
        );
    }
}
