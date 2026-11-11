//! Converts `.irdt` tapes and `.ibt` files to captures, and checks a capture against its source.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek};
use std::path::Path;

use super::reader::{CaptureSource, Record};
use crate::replay::{FrameSource, IbtSource, IrdtSource, Item, ReplayError};
use pitot_irsdk::capture::{CaptureWriter, Counts, SourceInfo, WriterOptions};

/// Clock of record times for sources without their own, such as `.ibt` files: 100 ns units.
const SYNTHETIC_CLOCK_HZ: u64 = 10_000_000;

/// Chunk length of written captures.
pub const CHUNK_SECONDS: u32 = 10;

fn source_info(kind: &str, path: &Path) -> SourceInfo {
    SourceInfo {
        kind: kind.to_owned(),
        file: path.file_name().map(|n| n.to_string_lossy().into_owned()),
        bytes: fs::metadata(path).ok().map(|m| m.len()),
    }
}

fn trim_nul(text: &[u8]) -> &[u8] {
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    &text[..end]
}

/// Converts an `.irdt` tape, keeping every record's time, tick, buffer index, update number, and gap.
pub fn irdt(input: &Path, output: &Path, level: i32) -> Result<Counts, ReplayError> {
    let mut source = IrdtSource::open(input)?;
    let options = WriterOptions {
        level,
        chunk_seconds: CHUNK_SECONDS,
        clock_hz: source.clock_hz(),
        sdk_version: source.sdk_version(),
        source: source_info("irdt", input),
        tool: "pitot-bench",
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    let mut writer = CaptureWriter::new(
        BufWriter::new(File::create(output)?),
        source.layout(),
        options,
    )?;
    let mut buf = Vec::new();
    let mut last = 0;
    use crate::replay::IrdtKind;
    while let Some(record) = source.read_record(&mut buf)? {
        last = record.elapsed;
        match record.kind {
            IrdtKind::Frame => writer.frame(record.elapsed, record.tick, record.value, &buf)?,
            IrdtKind::SessionInfo => {
                writer.session_info(record.elapsed, record.value, trim_nul(&buf))?
            }
            IrdtKind::Gap => writer.gap(record.elapsed, record.value.max(0) as u32)?,
            IrdtKind::Disconnect => writer.disconnect(record.elapsed)?,
            IrdtKind::End => break,
        }
    }
    Ok(writer.finish(last)?)
}

/// Converts an `.ibt` file: its session info snapshot, then every record at the tick rate. Ticks come from
/// `SessionTick` when the file has it.
pub fn ibt(input: &Path, output: &Path, level: i32) -> Result<Counts, ReplayError> {
    let mut source = IbtSource::open(input)?;
    let layout = source.layout().clone();
    let options = WriterOptions {
        level,
        chunk_seconds: CHUNK_SECONDS,
        clock_hz: SYNTHETIC_CLOCK_HZ,
        sdk_version: source.sdk_version(),
        source: source_info("ibt", input),
        tool: "pitot-bench",
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    let session_tick = layout
        .vars
        .iter()
        .find(|v| v.name_bytes() == b"SessionTick")
        .and_then(|v| usize::try_from(v.offset).ok())
        .filter(|&at| at + 4 <= layout.buf_len);
    let mut writer = CaptureWriter::new(BufWriter::new(File::create(output)?), &layout, options)?;
    let period = SYNTHETIC_CLOCK_HZ / layout.tick_rate.max(1) as u64;
    let (mut buf, mut frames) = (Vec::new(), 0u64);
    let time = |frames: u64| frames * period;
    while let Some(item) = source.next(&mut buf)? {
        match item {
            Item::Frame => {
                let tick = session_tick
                    .map(|at| i32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]))
                    .unwrap_or(frames as i32);
                writer.frame(time(frames), tick, 0, &buf)?;
                frames += 1;
            }
            Item::SessionInfo => writer.session_info(time(frames), 1, trim_nul(&buf))?,
            Item::Disconnect => writer.disconnect(time(frames))?,
        }
    }
    Ok(writer.finish(time(frames))?)
}

/// What a check compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Checked {
    pub frames: u64,
    pub session_infos: u64,
    pub disconnects: u64,
}

/// Replays `source` and `capture` side by side and fails at the first item that differs: kind, frame bytes, or
/// session info text (without NUL padding). Carried session info copies are skipped, as in a replay.
pub fn check<S: FrameSource, R: Read + Seek>(
    source: &mut S,
    capture: &mut CaptureSource<R>,
) -> Result<Checked, ReplayError> {
    if source.layout().vars != capture.layout().vars
        || source.layout().buf_len != capture.layout().buf_len
        || source.layout().tick_rate != capture.layout().tick_rate
    {
        return Err(ReplayError::Format("layouts differ"));
    }
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut checked = Checked::default();
    loop {
        let (left, right) = (source.next(&mut a)?, capture.next(&mut b)?);
        match (left, right) {
            (None, None) => return Ok(checked),
            (Some(Item::Frame), Some(Item::Frame)) if a == b => checked.frames += 1,
            (Some(Item::SessionInfo), Some(Item::SessionInfo)) if trim_nul(&a) == trim_nul(&b) => {
                checked.session_infos += 1
            }
            (Some(Item::Disconnect), Some(Item::Disconnect)) => checked.disconnects += 1,
            _ => return Err(ReplayError::Format("capture differs from its source")),
        }
    }
}

/// Every record of a capture in order, for tests and diagnostics.
pub fn records<R: Read + Seek>(capture: &mut CaptureSource<R>) -> Result<Vec<Record>, ReplayError> {
    let mut buf = Vec::new();
    let mut out = Vec::new();
    while let Some(record) = capture.read(&mut buf)? {
        out.push(record);
    }
    Ok(out)
}
