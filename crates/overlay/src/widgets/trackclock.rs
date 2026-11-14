//! The [`RaceClock`] shared by the standings and the relative, kept up to date from the view.
//!
//! Whichever of the two widgets updates first observes the latest tick for both, so the checkpoint history is kept
//! once and each widget gains the other's observations (the standings run at 2 Hz, the relative at 10 Hz).

use std::cell::RefCell;
use std::rc::Rc;

use pitot_irsdk::flags::session_state;

use crate::pipeline::View;

use super::timing::RaceClock;

/// `CarIdxTrackSurface` value for a car that is not in the world.
const NOT_IN_WORLD: i32 = -1;
/// Session time between observations, at least: when both widgets update within this, the second reads the first's.
const MIN_SPACING: f64 = 0.04;

/// One clock for every widget that times gaps.
pub(crate) type SharedTrackClock = Rc<RefCell<TrackClock>>;

/// A race clock fed with every car's laps and lap distance, in every session.
#[derive(Debug, Default)]
pub(crate) struct TrackClock {
    clock: RaceClock,
    session_num: Option<i32>,
    /// Session time of the last observation.
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
        let Some(now) = snap.f64(vars.session_time, 0) else {
            return;
        };
        // A time going back (a rewound replay) is observed at once: the clock starts over.
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
            let in_world = snap.i32(vars.car_track_surface, car) != Some(NOT_IN_WORLD);
            if !racing || !in_world {
                return None;
            }
            Some((
                snap.i32(vars.car_lap_completed, car)?,
                snap.f32(vars.car_lap_dist_pct, car)?,
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
