//! Records the session into a capture while the player drives, so a later replay of it can show the widgets
//! (see `replay`).
//!
//! The data thread already reads every tick. It copies the tick's whole row into a buffer from a recycled pool
//! and hands it over a bounded channel; a recorder thread wakes once a second, writes what arrived into the
//! capture (compressing every ten seconds of frames), and returns the buffers. So recording adds no wakeups per
//! tick and no allocation once the pool is filled. Ticks while the sim plays a replay are not recorded: they hold
//! live values, not the session.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pitot_irsdk::capture::{CaptureLayout, CaptureWriter, SourceInfo, WriterOptions};
use tracing::{info, warn};

use crate::platform;

/// Rows buffered between the data and recorder threads: two seconds at 60 Hz, twice the time between wake-ups.
const QUEUE: usize = 120;
/// Clock of record times: 100 ns units.
const CLOCK_HZ: u64 = 10_000_000;
/// Zstandard level of live recording.
const LEVEL: i32 = 3;
/// Two-second chunks: the writer holds a chunk's rows and its compressed form in memory (about 1 MB each for a
/// full-grid session), and a replay decodes one chunk at a time. Ten-second chunks took over 20 MB while
/// recording.
const CHUNK_SECONDS: u32 = 2;

/// Where and how much to record.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingConfig {
    pub dir: PathBuf,
    /// Older captures are deleted once the folder holds more than this.
    pub max_bytes: u64,
}

pub(super) enum Event {
    /// A sim connection with this layout began.
    Start(CaptureLayout, i32),
    Frame {
        at: Instant,
        tick: i32,
        buffer: i32,
        row: Vec<u8>,
    },
    SessionInfo {
        at: Instant,
        update: i32,
        text: Vec<u8>,
    },
    Gap {
        at: Instant,
        missed: u32,
    },
    /// The connection ended; finish the file.
    End,
}

/// The data thread's end: hands rows to the recorder and takes their buffers back.
pub(super) struct Recorder {
    events: SyncSender<Event>,
    spare: Receiver<Vec<u8>>,
    /// Fills the pool when a connection starts.
    refill: SyncSender<Vec<u8>>,
    /// Rows the queue had no room for since the last one sent, recorded as a gap.
    dropped: u32,
    first_drop: Option<Instant>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub(super) fn start(config: RecordingConfig) -> std::io::Result<Self> {
        let (events, received) = mpsc::sync_channel::<Event>(QUEUE);
        let (give_back, spare) = mpsc::sync_channel::<Vec<u8>>(QUEUE + 8);
        let thread = thread::Builder::new().name("pitot-record".into()).spawn({
            let give_back = give_back.clone();
            move || record_loop(&config, &received, &give_back)
        })?;
        Ok(Self {
            events,
            spare,
            refill: give_back,
            dropped: 0,
            first_drop: None,
            thread: Some(thread),
        })
    }

    /// A buffer from the pool, or a new one while the pool fills.
    fn buffer(&self, len: usize) -> Vec<u8> {
        let mut buffer = self.spare.try_recv().unwrap_or_default();
        buffer.clear();
        buffer.reserve(len);
        buffer
    }

    fn send(&mut self, event: Event) {
        match self.events.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(Event::Frame { at, .. })) => {
                self.dropped += 1;
                self.first_drop.get_or_insert(at);
            }
            Err(_) => {}
        }
    }

    pub(super) fn connected(&mut self, layout: CaptureLayout, sdk_version: i32) {
        self.dropped = 0;
        self.first_drop = None;
        // As many buffers as the queue holds, so rows never need a new one once recording runs.
        while self
            .refill
            .try_send(Vec::with_capacity(layout.buf_len))
            .is_ok()
        {}
        // The start must arrive; the queue drains within a second.
        let _ = self.events.send(Event::Start(layout, sdk_version));
    }

    pub(super) fn frame(&mut self, tick: i32, buffer: i32, row: &[u8], missed: u32) {
        let at = Instant::now();
        let missed = missed + std::mem::take(&mut self.dropped);
        if missed > 0 {
            let gap_at = self.first_drop.take().unwrap_or(at);
            self.send(Event::Gap { at: gap_at, missed });
        }
        let mut copy = self.buffer(row.len());
        copy.extend_from_slice(row);
        self.send(Event::Frame {
            at,
            tick,
            buffer,
            row: copy,
        });
    }

    pub(super) fn session_info(&mut self, update: i32, text: &[u8]) {
        let mut copy = self.buffer(text.len());
        copy.extend_from_slice(text);
        // Session info must not be lost; wait for room if needed (rare, and the queue drains every second).
        let _ = self.events.send(Event::SessionInfo {
            at: Instant::now(),
            update,
            text: copy,
        });
    }

    pub(super) fn ended(&mut self) {
        let _ = self.events.send(Event::End);
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.events.send(Event::End);
        // Closing the channel stops the thread after it finishes the file.
        let (closed, _) = mpsc::sync_channel(0);
        drop(std::mem::replace(&mut self.events, closed));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Open {
    path: PathBuf,
    writer: CaptureWriter<BufWriter<File>>,
    start: Instant,
    frames: u64,
}

impl Open {
    fn clock(&self, at: Instant) -> u64 {
        (at.saturating_duration_since(self.start).as_nanos() / 100) as u64
    }
}

/// A new capture's path: `<dir>/<local date and time>.ptc`, numbered if taken.
fn new_path(dir: &Path) -> PathBuf {
    let t = platform::local_time();
    let stem = format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    let mut path = dir.join(format!("{stem}.ptc"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.ptc"));
        n += 1;
    }
    path
}

fn open(dir: &Path, layout: &CaptureLayout, sdk_version: i32) -> std::io::Result<Open> {
    fs::create_dir_all(dir)?;
    let path = new_path(dir);
    let options = WriterOptions {
        level: LEVEL,
        chunk_seconds: CHUNK_SECONDS,
        clock_hz: CLOCK_HZ,
        sdk_version,
        source: SourceInfo {
            kind: "live".into(),
            file: None,
            bytes: None,
        },
        tool: "pitot-overlay",
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    let writer = CaptureWriter::new(BufWriter::new(File::create(&path)?), layout, options)?;
    Ok(Open {
        path,
        writer,
        start: Instant::now(),
        frames: 0,
    })
}

/// Finishes the file, or deletes it when it holds no frames (the whole connection was a replay).
fn finish(open: Open, config: &RecordingConfig) {
    let end = open.clock(Instant::now());
    let (path, frames) = (open.path.clone(), open.frames);
    match open.writer.finish(end) {
        Ok(_) if frames == 0 => {
            let _ = fs::remove_file(&path);
        }
        Ok(counts) => {
            info!(
                capture = %path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                frames = counts.frames,
                missed = counts.missed_ticks,
                "recording finished"
            );
            prune(&config.dir, config.max_bytes, None);
        }
        Err(e) => warn!("recording could not be finished: {e}"),
    }
}

/// Deletes the oldest captures in `dir` until they take at most `max_bytes`, never `keep`.
pub(super) fn prune(dir: &Path, max_bytes: u64, keep: Option<&Path>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ptc")))
        .filter_map(|p| {
            let meta = fs::metadata(&p).ok()?;
            Some((meta.modified().ok()?, meta.len(), p))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= max_bytes {
            break;
        }
        if Some(path.as_path()) == keep {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

fn record_loop(
    config: &RecordingConfig,
    events: &Receiver<Event>,
    give_back: &SyncSender<Vec<u8>>,
) {
    prune(&config.dir, config.max_bytes, None);
    let mut current: Option<Open> = None;
    let mut failed = false;
    let mut closed = false;
    while !closed {
        // One wake-up a second: everything that arrived meanwhile is written in one go.
        thread::sleep(Duration::from_secs(1));
        let drained = std::iter::from_fn(|| match events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                closed = true;
                None
            }
        });
        for event in drained {
            match event {
                Event::Start(layout, sdk_version) => {
                    if let Some(open) = current.take() {
                        finish(open, config);
                    }
                    match open(&config.dir, &layout, sdk_version) {
                        Ok(open) => {
                            failed = false;
                            current = Some(open);
                        }
                        Err(e) => {
                            if !failed {
                                warn!("recording is off for this connection: {e}");
                            }
                            failed = true;
                        }
                    }
                }
                Event::Frame {
                    at,
                    tick,
                    buffer,
                    row,
                } => {
                    if let Some(open) = current.as_mut() {
                        let time = open.clock(at);
                        if open.writer.frame(time, tick, buffer, &row).is_ok() {
                            open.frames += 1;
                        }
                    }
                    let _ = give_back.try_send(row);
                }
                Event::SessionInfo { at, update, text } => {
                    if let Some(open) = current.as_mut() {
                        let time = open.clock(at);
                        let _ = open.writer.session_info(time, update, &text);
                    }
                    let _ = give_back.try_send(text);
                }
                Event::Gap { at, missed } => {
                    if let Some(open) = current.as_mut() {
                        let time = open.clock(at);
                        let _ = open.writer.gap(time, missed);
                    }
                }
                Event::End => {
                    if let Some(open) = current.take() {
                        finish(open, config);
                    }
                }
            }
        }
    }
    if let Some(open) = current.take() {
        finish(open, config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pruning_deletes_the_oldest_first() {
        let dir = std::env::temp_dir().join(format!("pitot-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("creates");
        for (n, name) in ["a.ptc", "b.ptc", "c.ptc"].iter().enumerate() {
            fs::write(dir.join(name), vec![0u8; 100]).expect("writes");
            // Distinct modification times, oldest first.
            let file = File::options()
                .write(true)
                .open(dir.join(name))
                .expect("opens");
            let when =
                std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + n as u64 * 60);
            file.set_modified(when).expect("sets the time");
        }
        fs::write(dir.join("notes.txt"), vec![0u8; 1000]).expect("writes");
        prune(&dir, 250, None);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .expect("lists")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["b.ptc", "c.ptc", "notes.txt"]);
        prune(&dir, 0, Some(&dir.join("c.ptc")));
        assert!(
            dir.join("c.ptc").exists(),
            "the capture being written stays"
        );
        assert!(!dir.join("b.ptc").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
