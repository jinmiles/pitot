//! Plays a capture of the session in step with the sim's own replay.
//!
//! While the sim plays a replay, its telemetry stays live: only `ReplaySessionNum` and `ReplaySessionTime` say
//! which moment the replay shows. The data thread then publishes the frame a capture recorded at that moment,
//! mapped into the live layout, so the widgets show what they showed during the session.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use pitot_irsdk::capture::{CaptureError, CaptureReader, Moment, Record, Seeker};
use pitot_irsdk::live::{RowMap, Snapshot, VarSet};
use pitot_irsdk::session::SessionParser;

/// Seconds of input trace filled in after a jump: half the sample ring.
pub const PREFILL_SECONDS: f64 = (super::SAMPLE_CAPACITY / 2) as f64 / 60.0;

/// A forward move longer than this restarts the trace instead of adding every frame in between.
const MAX_STEP_SECONDS: f64 = 1.0;

/// Which session a capture belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionIds {
    /// `SubSessionID`; 0 for offline sessions, which all share it.
    pub sub_session: i64,
    pub track: Option<i64>,
}

impl SessionIds {
    pub fn of(info: &pitot_irsdk::session::SessionInfo) -> Self {
        Self {
            sub_session: info.weekend.sub_session_id.unwrap_or(0),
            track: info.weekend.track_id,
        }
    }
}

/// A capture found for a replay.
pub struct Found {
    /// File name, for the log.
    pub name: String,
    pub seeker: Seeker<BufReader<File>>,
    /// The recorder finished the file; otherwise it may still be growing.
    pub finished: bool,
}

/// Session IDs of captures already looked at, so a search reads each file once while it is unchanged.
#[derive(Debug, Default)]
pub struct CaptureCache {
    seen: HashMap<PathBuf, (SystemTime, u64, Option<SessionIds>)>,
}

/// The session IDs in a capture's first session info, which opens its first chunk.
fn ids_of(path: &Path, parser: &mut SessionParser) -> Option<SessionIds> {
    let mut reader = CaptureReader::open(path).ok()?;
    let cars = reader
        .layout()
        .vars
        .iter()
        .find(|v| v.name_bytes() == b"CarIdxLapDistPct")
        .map_or(64, |v| v.count.max(1) as usize);
    let mut text = Vec::new();
    let first_chunk = reader.chunk_start();
    while let Some(record) = reader.read(&mut text).ok()? {
        if reader.chunk_start() != first_chunk {
            return None;
        }
        if let Record::SessionInfo { .. } = record {
            return parser
                .parse(&text, cars)
                .ok()
                .map(|info| SessionIds::of(&info));
        }
    }
    None
}

/// Finds a capture of the session `want` describes in `folders`, newest first. An online session matches by
/// `SubSessionID`, preferring a capture that covers `at`. An offline session matches by track, and only a
/// capture that covers `at`.
pub fn find(
    folders: &[PathBuf],
    want: SessionIds,
    at: Moment,
    parser: &mut SessionParser,
    cache: &mut CaptureCache,
) -> Option<Found> {
    let mut files: Vec<(SystemTime, u64, PathBuf)> = folders
        .iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ptc")))
        .filter_map(|p| {
            let meta = fs::metadata(&p).ok()?;
            Some((meta.modified().ok()?, meta.len(), p))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let online = want.sub_session != 0;
    let mut fallback = None;
    for (modified, len, path) in files {
        let ids = match cache.seen.get(&path) {
            Some((m, l, ids)) if *m == modified && *l == len => *ids,
            _ => {
                let ids = ids_of(&path, parser);
                cache.seen.insert(path.clone(), (modified, len, ids));
                ids
            }
        };
        let Some(ids) = ids else {
            continue;
        };
        let matches = if online {
            ids.sub_session == want.sub_session
        } else {
            ids.sub_session == 0 && want.track.is_some() && ids.track == want.track
        };
        if !matches {
            continue;
        }
        let Ok(mut reader) = CaptureReader::open(&path) else {
            continue;
        };
        let finished = reader.is_finished().unwrap_or(false);
        let Ok(seeker) = Seeker::new(reader) else {
            continue;
        };
        let found = Found {
            name: path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            seeker,
            finished,
        };
        if found.seeker.covers(at) {
            return Some(found);
        }
        if online && fallback.is_none() {
            fallback = Some(found);
        }
    }
    fallback
}

/// What a replay moment asks of the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// The capture has no frame there.
    Outside,
    /// Still the current frame (paused, or slow motion between frames).
    Same,
    /// A short step forward: every frame passed becomes an input sample.
    Forward,
    /// Backwards: show the frame there, but leave the trace until playing forward restarts it.
    Back,
    /// A jump, a new session, or the first frame: restart the trace from the seconds before the moment.
    Restart,
}

/// A capture being played for the current replay.
pub struct Player {
    seeker: Seeker<BufReader<File>>,
    map: RowMap,
    snapshot: Snapshot,
    finished: bool,
    /// The trace holds samples up to the current frame.
    trace_current: bool,
    /// The capture's tick rate, which times its frames' `SessionTick`.
    ticks_per_second: f64,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("seeker", &self.seeker)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Plays `found` into snapshots laid out like `template`, which a reader of `set` filled.
    pub fn new(found: Found, set: &VarSet, template: &Snapshot) -> Self {
        let layout = found.seeker.layout();
        let map = RowMap::new(set, template, &layout.vars, layout.buf_len);
        let ticks_per_second = super::ticks_per_second(layout.tick_rate);
        Self {
            map,
            snapshot: template.clone(),
            seeker: found.seeker,
            finished: found.finished,
            trace_current: false,
            ticks_per_second,
        }
    }

    /// The capture's tick rate.
    pub fn ticks_per_second(&self) -> f64 {
        self.ticks_per_second
    }

    /// The latest frame, in the live layout.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The capture may still grow past what was indexed.
    pub fn growing(&self) -> bool {
        !self.finished
    }

    pub fn end(&self) -> Moment {
        self.seeker.end()
    }

    pub fn plan(&self, at: Moment) -> Plan {
        if !self.seeker.covers(at) {
            return Plan::Outside;
        }
        let Some((_, current, _)) = self.seeker.current() else {
            return Plan::Restart;
        };
        let tick = 1.0 / 60.0;
        if at.session != current.session || at.time > current.time + MAX_STEP_SECONDS {
            Plan::Restart
        } else if at.time < current.time - tick {
            Plan::Back
        } else if at.time < current.time + tick * 0.5 {
            Plan::Same
        } else if self.trace_current {
            Plan::Forward
        } else {
            Plan::Restart
        }
    }

    /// Moves forward to `at`, handing each frame passed to `sample`.
    pub fn forward(
        &mut self,
        at: Moment,
        mut sample: impl FnMut(&Snapshot),
    ) -> Result<(), CaptureError> {
        let (map, snapshot) = (&self.map, &mut self.snapshot);
        self.seeker.advance(at, |row, _, tick| {
            map.copy(row, tick, snapshot);
            sample(snapshot);
        })?;
        Ok(())
    }

    /// Moves back to `at` without samples.
    pub fn back(&mut self, at: Moment) -> Result<(), CaptureError> {
        self.seeker.seek(at)?;
        self.copy_current();
        self.trace_current = false;
        Ok(())
    }

    /// Moves to `at` from [`PREFILL_SECONDS`] before it, handing every frame on the way to `sample`.
    pub fn restart(
        &mut self,
        at: Moment,
        mut sample: impl FnMut(&Snapshot),
    ) -> Result<(), CaptureError> {
        self.seeker
            .seek(Moment::new(at.session, at.time - PREFILL_SECONDS))?;
        if self.copy_current() {
            sample(&self.snapshot);
        }
        self.forward(at, sample)?;
        self.trace_current = true;
        Ok(())
    }

    fn copy_current(&mut self) -> bool {
        match self.seeker.current() {
            Some((row, _, tick)) => {
                self.map.copy(row, tick, &mut self.snapshot);
                true
            }
            None => false,
        }
    }

    /// The session info in effect at the current frame, if it changed since the last call.
    pub fn take_session_change(&mut self) -> Option<&[u8]> {
        self.seeker.take_session_change().map(|(_, text)| text)
    }

    /// The session info in effect at the current frame.
    pub fn session(&self) -> Option<&[u8]> {
        self.seeker.session().map(|(_, text)| text)
    }
}
