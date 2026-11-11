//! Pitot captures (`.ptc`): what the sim published through SDK shared memory, in compressed ten-second chunks
//! with an index, checksums, and metadata. The format is specified in `docs/capture-format.md`.
//!
//! - [`CaptureReader`] reads a capture in order, or from a chunk found through the index.
//! - [`format`] holds the format's constants and primitives for writers.

pub mod format;
mod reader;

use std::fmt;
use std::io;

use crate::layout::{LayoutError, VarHeader};

pub use reader::{CaptureReader, ChunkEntry, Record};

/// What a capture's frames look like.
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
