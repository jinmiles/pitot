//! Live recorder: copies everything the sim publishes through SDK shared memory into a capture. Every data
//! buffer row is stored whole, session info whenever its update number changes, ticks the recorder missed as
//! gaps, and disconnects. A layout change (another car or session type) starts a new capture file.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pitot_irsdk::live::{
    LiveError, LiveLayout, LiveReader, ReaderConfig, SdkNames, Snapshot, Step, VarSet,
};

use crate::capture::{CaptureWriter, Counts, SourceInfo, WriterOptions};
use crate::replay::SourceLayout;

/// Record times are kept in 100 ns units, as `.irdt` tapes do.
pub const CLOCK_HZ: u64 = 10_000_000;

#[derive(Debug)]
pub enum RecordError {
    Io(io::Error),
    Live(LiveError),
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Live(e) => write!(f, "shared memory: {e}"),
        }
    }
}

impl std::error::Error for RecordError {}

impl From<io::Error> for RecordError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<LiveError> for RecordError {
    fn from(e: LiveError) -> Self {
        Self::Live(e)
    }
}

#[derive(Debug, Clone)]
pub struct RecordOptions {
    pub names: SdkNames,
    /// The first capture's path; later ones (after a layout change) get `-2`, `-3`, ... before the extension.
    pub out: PathBuf,
    /// Zstandard level; 3 keeps the recorder light next to the sim.
    pub level: i32,
    /// Stop after this much wall time.
    pub max_duration: Option<Duration>,
    /// How often to look for the sim, and how long to wait for a tick.
    pub reader: ReaderConfig,
}

/// One capture file the recorder wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: PathBuf,
    pub counts: Counts,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordSummary {
    pub files: Vec<Written>,
    /// Reads that found the buffer overwritten during both copy attempts; the tick then counts as missed.
    pub torn: u64,
    pub elapsed: Duration,
}

/// The path of the `n`th capture (1-based).
pub fn numbered(out: &Path, n: usize) -> PathBuf {
    if n <= 1 {
        return out.to_owned();
    }
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match out.extension() {
        Some(ext) => format!("{stem}-{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    out.with_file_name(name)
}

struct Open {
    writer: CaptureWriter<BufWriter<File>>,
    layout: LiveLayout,
    path: PathBuf,
}

fn open(options: &RecordOptions, layout: &LiveLayout, n: usize) -> Result<Open, RecordError> {
    let path = numbered(&options.out, n);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let source = SourceLayout {
        vars: layout.vars.clone(),
        buf_len: layout.buf_len,
        tick_rate: layout.tick_rate,
        session_info_capacity: layout.session_info_capacity,
        player_car_only: false,
    };
    let writer = CaptureWriter::new(
        BufWriter::new(File::create(&path)?),
        &source,
        WriterOptions {
            level: options.level,
            chunk_seconds: crate::capture::convert::CHUNK_SECONDS,
            clock_hz: CLOCK_HZ,
            sdk_version: layout.sdk_version,
            source: SourceInfo {
                kind: "live".to_owned(),
                file: None,
                bytes: None,
            },
        },
    )?;
    Ok(Open {
        writer,
        layout: layout.clone(),
        path,
    })
}

/// Ctrl+C and Ctrl+Break set the returned flag instead of ending the process, so [`record`] finishes its file.
pub fn stop_on_ctrl_c() -> Result<&'static AtomicBool, crate::sys::SysError> {
    crate::sys::stop_on_ctrl_c()
}

/// Records until `stop` is set or `max_duration` passes. Waits for the sim when it is not running.
pub fn record(options: &RecordOptions, stop: &AtomicBool) -> Result<RecordSummary, RecordError> {
    let mut reader = LiveReader::new(options.names.clone(), VarSet::whole_row(), options.reader);
    let mut snapshot = Snapshot::new();
    let mut text = Vec::new();
    let start = Instant::now();
    let clock = |at: Instant| (at.duration_since(start).as_nanos() / 100) as u64;
    let mut summary = RecordSummary::default();
    let mut current: Option<Open> = None;
    let mut session_update = None;
    loop {
        if stop.load(Ordering::Relaxed)
            || options
                .max_duration
                .is_some_and(|max| start.elapsed() >= max)
        {
            break;
        }
        let step = reader.step(&mut snapshot)?;
        let now = clock(Instant::now());
        match step {
            Step::Connected => {
                let Some(layout) = reader.layout().cloned() else {
                    continue;
                };
                if current.as_ref().is_none_or(|c| c.layout != layout) {
                    if let Some(done) = current.take() {
                        let counts = done.writer.finish(now)?;
                        summary.files.push(Written {
                            path: done.path,
                            counts,
                        });
                    }
                    current = Some(open(options, &layout, summary.files.len() + 1)?);
                }
                session_update = None;
            }
            Step::Tick { missed } => {
                let Some(open) = current.as_mut() else {
                    continue;
                };
                if missed > 0 {
                    open.writer.gap(now, missed)?;
                }
                // Session info goes before the frame it was seen with, so a replay publishes both together.
                if reader.session_info_update() != session_update
                    && let Some(update) = reader.copy_session_info(&mut text)?
                {
                    open.writer.session_info(now, update, &text)?;
                    session_update = Some(update);
                }
                open.writer.frame(
                    now,
                    snapshot.tick(),
                    snapshot.buffer() as i32,
                    snapshot.bytes(),
                )?;
            }
            Step::Torn => summary.torn += 1,
            Step::Disconnected => {
                if let Some(open) = current.as_mut() {
                    open.writer.disconnect(now)?;
                }
            }
            Step::Idle | Step::NotRunning => {}
        }
    }
    if let Some(done) = current.take() {
        let counts = done.writer.finish(clock(Instant::now()))?;
        summary.files.push(Written {
            path: done.path,
            counts,
        });
    }
    summary.elapsed = start.elapsed();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_paths() {
        let out = Path::new("captures/race.ptc");
        assert_eq!(numbered(out, 1), PathBuf::from("captures/race.ptc"));
        assert_eq!(numbered(out, 3), PathBuf::from("captures/race-3.ptc"));
        assert_eq!(numbered(Path::new("race"), 2), PathBuf::from("race-2"));
    }
}
