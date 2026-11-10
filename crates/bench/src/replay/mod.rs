//! Replays recordings into simulated SDK shared memory, so overlays run as they would against the sim.
//!
//! A [`FrameSource`] yields frames (raw data buffer rows) and session info revisions in recorded order.
//! [`run`] publishes them through `pitot_irsdk::publish::Publisher` on an absolute schedule at the recorded
//! tick rate, so a late frame does not delay the ones after it.

mod ibt;
mod irdt;

use std::fmt;
use std::io;

use pitot_irsdk::ibt::IbtError;
use pitot_irsdk::layout::{LayoutError, VarHeader};

pub use ibt::IbtSource;
#[cfg(test)]
pub(crate) use ibt::tests::ibt_with as ibt_file;
#[cfg(test)]
pub(crate) use irdt::tests::{CLOCK_HZ as IRDT_TEST_CLOCK_HZ, tape as irdt_tape};
pub use irdt::{IrdtKind, IrdtRecord, IrdtSource};
#[cfg(windows)]
pub use runner::{ReplayOptions, ReplaySummary, run};

/// Session info area size the sim uses, used when a source does not record its own.
pub const SIM_SESSION_INFO_CAPACITY: usize = 512 * 1024;

/// Errors from reading a recording or publishing it.
#[derive(Debug)]
pub enum ReplayError {
    Io(io::Error),
    Layout(LayoutError),
    Ibt(IbtError),
    #[cfg(windows)]
    Live(pitot_irsdk::live::LiveError),
    /// The file is not a supported recording.
    Format(&'static str),
    /// A record's payload does not match its checksum.
    Checksum {
        offset: u64,
    },
    /// The sim, or another replay, already owns the shared-memory names.
    NamesInUse,
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Layout(e) => write!(f, "invalid layout: {e}"),
            Self::Ibt(e) => write!(f, "invalid .ibt file: {e}"),
            #[cfg(windows)]
            Self::Live(e) => write!(f, "shared memory: {e}"),
            Self::Format(what) => write!(f, "unsupported recording: {what}"),
            Self::Checksum { offset } => write!(f, "record at byte {offset} fails its checksum"),
            Self::NamesInUse => {
                f.write_str("the sim or another replay already owns the shared-memory names")
            }
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<io::Error> for ReplayError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<LayoutError> for ReplayError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

impl From<IbtError> for ReplayError {
    fn from(e: IbtError) -> Self {
        Self::Ibt(e)
    }
}

#[cfg(windows)]
impl From<pitot_irsdk::live::LiveError> for ReplayError {
    fn from(e: pitot_irsdk::live::LiveError) -> Self {
        match e {
            pitot_irsdk::live::LiveError::AlreadyExists => Self::NamesInUse,
            other => Self::Live(other),
        }
    }
}

/// Shared-memory layout a source replays into.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceLayout {
    pub vars: Vec<VarHeader>,
    pub buf_len: usize,
    pub tick_rate: i32,
    pub session_info_capacity: usize,
    /// The recording holds the player's car only (no `CarIdx*` data), as `.ibt` files do.
    pub player_car_only: bool,
}

/// What [`FrameSource::next`] put into the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// One data buffer row of `buf_len` bytes.
    Frame,
    /// A session info revision (text, possibly NUL padded).
    SessionInfo,
    /// The sim disconnected in the recording.
    Disconnect,
}

/// A recording read in order.
pub trait FrameSource {
    fn layout(&self) -> &SourceLayout;

    /// Reads the next item into `buf`, replacing its contents. `Ok(None)` at the end of the recording.
    fn next(&mut self, buf: &mut Vec<u8>) -> Result<Option<Item>, ReplayError>;

    /// Restarts from the first item.
    fn rewind(&mut self) -> Result<(), ReplayError>;
}

#[cfg(windows)]
mod runner {
    use std::thread;
    use std::time::{Duration, Instant};

    use pitot_irsdk::live::SdkNames;
    use pitot_irsdk::publish::{Publisher, PublisherConfig};

    use super::{FrameSource, Item, ReplayError};

    /// How to replay a source.
    #[derive(Debug, Clone)]
    pub struct ReplayOptions {
        pub names: SdkNames,
        /// Playback speed factor; 1.0 is the recorded rate.
        pub speed: f64,
        /// Start over at the end, with a disconnect and reconnect so readers reset their state.
        pub looped: bool,
        /// Stop after this much wall time.
        pub max_duration: Option<Duration>,
        /// Stop after this many frames.
        pub max_frames: Option<u64>,
        /// How long the mapping stays up after the final disconnect, so readers can notice it.
        pub linger: Duration,
    }

    impl Default for ReplayOptions {
        fn default() -> Self {
            Self {
                names: SdkNames::sim(),
                speed: 1.0,
                looped: false,
                max_duration: None,
                max_frames: None,
                linger: Duration::from_secs(1),
            }
        }
    }

    /// What a replay did.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct ReplaySummary {
        pub frames: u64,
        pub session_infos: u64,
        pub loops: u64,
        /// Frames published more than one period after their scheduled time.
        pub late_frames: u64,
        pub elapsed: Duration,
    }

    /// Publishes `source` until it ends (or forever with `looped`), `max_duration`, or `max_frames`.
    pub fn run<S: FrameSource>(
        source: &mut S,
        options: &ReplayOptions,
    ) -> Result<ReplaySummary, ReplayError> {
        let layout = source.layout().clone();
        if !(options.speed.is_finite() && options.speed > 0.0) {
            return Err(ReplayError::Format("speed must be a positive number"));
        }
        let config = PublisherConfig {
            vars: layout.vars.clone(),
            buf_len: layout.buf_len,
            num_buf: 3,
            tick_rate: layout.tick_rate,
            session_info_capacity: layout.session_info_capacity,
        };
        let mut publisher = Publisher::create(&options.names, &config)?;
        let period = 1.0 / (f64::from(layout.tick_rate.max(1)) * options.speed);
        let mut buf = Vec::with_capacity(layout.buf_len.max(layout.session_info_capacity));
        let mut summary = ReplaySummary::default();
        let mut connected = false;
        let start = Instant::now();
        // Frames scheduled since `epoch`; the schedule restarts after a loop pause.
        let mut epoch = start;
        let mut scheduled = 0u64;

        loop {
            if options
                .max_duration
                .is_some_and(|max| start.elapsed() >= max)
                || options.max_frames.is_some_and(|max| summary.frames >= max)
            {
                break;
            }
            match source.next(&mut buf)? {
                Some(Item::SessionInfo) => {
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    publisher.set_session_info(&buf[..end])?;
                    summary.session_infos += 1;
                }
                Some(Item::Frame) => {
                    if !connected {
                        publisher.set_connected(true)?;
                        connected = true;
                    }
                    let due = epoch + Duration::from_secs_f64(scheduled as f64 * period);
                    let now = Instant::now();
                    if let Some(wait) = due.checked_duration_since(now) {
                        thread::sleep(wait);
                    } else if now.duration_since(due).as_secs_f64() > period {
                        summary.late_frames += 1;
                    }
                    publisher.publish(&buf)?;
                    scheduled += 1;
                    summary.frames += 1;
                }
                Some(Item::Disconnect) => {
                    if connected {
                        publisher.set_connected(false)?;
                        connected = false;
                    }
                }
                None if options.looped => {
                    if connected {
                        publisher.set_connected(false)?;
                        connected = false;
                    }
                    // Give readers a couple of ticks to see the disconnect before data starts over.
                    thread::sleep(Duration::from_secs_f64(period * 2.0));
                    source.rewind()?;
                    summary.loops += 1;
                    epoch = Instant::now();
                    scheduled = 0;
                }
                None => break,
            }
        }
        if connected {
            publisher.set_connected(false)?;
        }
        summary.elapsed = start.elapsed();
        thread::sleep(options.linger);
        Ok(summary)
    }
}
