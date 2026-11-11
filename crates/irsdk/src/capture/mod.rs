//! Pitot captures (`.ptc`): what the sim published through SDK shared memory, in compressed ten-second chunks
//! with an index, checksums, and metadata. The format is specified in `docs/capture-format.md`.
//!
//! - [`CaptureWriter`] writes one.
//! - [`CaptureReader`] reads a capture in order, or from a chunk found through the index.
//! - [`Seeker`] finds frames by session time, to play a capture in step with the sim's replay.
//! - [`format`] holds the format's constants and primitives for writers.

pub mod format;
mod reader;
mod seek;
mod writer;

use std::fmt;
use std::io;

use crate::layout::{LayoutError, VarHeader};

pub use reader::{CaptureReader, ChunkEntry, Record};
pub use seek::{Moment, Seeker};
pub use writer::{CaptureWriter, Counts, SourceInfo, WriterOptions};

/// What a capture's frames look like, and the shared-memory layout a replay of it publishes.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureLayout {
    pub vars: Vec<VarHeader>,
    /// Bytes per frame (`bufLen` of the sim).
    pub buf_len: usize,
    pub tick_rate: i32,
    /// Size of the sim's session info area.
    pub session_info_capacity: usize,
    /// The capture holds the player's car only (no `CarIdx*` data), as converted from `.ibt`.
    pub player_car_only: bool,
}

/// Errors from reading a capture.
#[derive(Debug)]
pub enum CaptureError {
    Io(io::Error),
    Layout(LayoutError),
    /// The file is not a capture this reader supports, or a record is damaged.
    Format(&'static str),
    /// A part of the file does not match its checksum.
    Checksum {
        offset: u64,
    },
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Layout(e) => write!(f, "invalid layout: {e}"),
            Self::Format(what) => write!(f, "unsupported capture: {what}"),
            Self::Checksum { offset } => write!(f, "record at byte {offset} fails its checksum"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<io::Error> for CaptureError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<LayoutError> for CaptureError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// Builds small captures for tests, without index or trailer, as a recorder leaves them when stopped. Other
/// crates' tests get it through the `test-capture` feature.
#[cfg(any(test, feature = "test-capture"))]
#[doc(hidden)]
pub mod test_capture {
    use super::format::*;
    use crate::layout::VarHeader;

    pub enum Item {
        /// Tick count and row.
        Frame(i32, Vec<u8>),
        /// Update number and text.
        Session(i32, Vec<u8>),
    }

    fn compress(records: &[u8]) -> Vec<u8> {
        let mut compressor = zstd::bulk::Compressor::new(3).expect("compressor");
        compressor.include_checksum(true).expect("checksum");
        compressor.compress(records).expect("compresses")
    }

    /// One frame per tick at 60 Hz, `chunk_frames` frames per chunk, each chunk opening with the session info
    /// in effect, as the writer does.
    pub fn build(vars: &[VarHeader], buf_len: usize, chunk_frames: u32, items: &[Item]) -> Vec<u8> {
        let mut table = vec![0u8; vars.len() * VAR_HEADER_SIZE];
        for (var, raw) in vars.iter().zip(table.as_chunks_mut::<VAR_HEADER_SIZE>().0) {
            var.write(raw).expect("writes");
        }
        let mut out = vec![0u8; HEADER_SIZE];
        out[0..8].copy_from_slice(MAGIC);
        put_u32(&mut out, 8, VERSION);
        put_u32(&mut out, 12, HEADER_SIZE as u32);
        put_u32(&mut out, 20, 60);
        put_u32(&mut out, 24, buf_len as u32);
        put_u32(&mut out, 28, vars.len() as u32);
        put_u32(&mut out, 32, 4096);
        put_u64(&mut out, 40, 60);
        put_u32(&mut out, 48, chunk_frames);
        put_u32(&mut out, 52, crc32(&table));
        let crc = crc32(&out[..60]);
        put_u32(&mut out, 60, crc);
        out.extend_from_slice(&table);

        let mut session: Option<(i32, Vec<u8>)> = None;
        let mut records = Vec::new();
        let mut frames = 0u32;
        let mut prev = vec![0u8; buf_len];
        let flush = |records: &mut Vec<u8>, frames: &mut u32, out: &mut Vec<u8>| {
            if records.is_empty() {
                return;
            }
            let compressed = compress(records);
            let mut head = [0u8; CHUNK_HEADER_SIZE];
            head[0..4].copy_from_slice(CHUNK_MAGIC);
            put_u32(&mut head, 4, compressed.len() as u32);
            put_u32(&mut head, 8, records.len() as u32);
            put_u32(&mut head, 24, *frames);
            let crc = crc32(&head[..36]);
            put_u32(&mut head, 36, crc);
            out.extend_from_slice(&head);
            out.extend_from_slice(&compressed);
            records.clear();
            *frames = 0;
        };
        for item in items {
            if records.is_empty() {
                prev.fill(0);
                if let Some((update, text)) = &session {
                    records.push(KIND_SESSION_CARRY);
                    put_varint(&mut records, 0);
                    records.extend_from_slice(&update.to_le_bytes());
                    records.extend_from_slice(&(text.len() as u32).to_le_bytes());
                    records.extend_from_slice(text);
                }
            }
            match item {
                Item::Session(update, text) => {
                    records.push(KIND_SESSION_INFO);
                    put_varint(&mut records, 0);
                    records.extend_from_slice(&update.to_le_bytes());
                    records.extend_from_slice(&(text.len() as u32).to_le_bytes());
                    records.extend_from_slice(text);
                    session = Some((*update, text.clone()));
                }
                Item::Frame(tick, row) => {
                    records.push(KIND_FRAME);
                    put_varint(&mut records, 1);
                    records.extend_from_slice(&tick.to_le_bytes());
                    records.extend_from_slice(&0i32.to_le_bytes());
                    records.extend(row.iter().zip(&prev).map(|(a, b)| a ^ b));
                    prev.copy_from_slice(row);
                    frames += 1;
                    if frames >= chunk_frames {
                        flush(&mut records, &mut frames, &mut out);
                    }
                }
            }
        }
        flush(&mut records, &mut frames, &mut out);
        out
    }
}
