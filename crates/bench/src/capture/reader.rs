//! Captures as replay sources, on top of `pitot_irsdk::capture::CaptureReader`.

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use pitot_irsdk::capture::{CaptureError, CaptureReader};
pub use pitot_irsdk::capture::{ChunkEntry, Record};

use crate::replay::{FrameSource, Item, ReplayError, SourceLayout};

impl From<CaptureError> for ReplayError {
    fn from(e: CaptureError) -> Self {
        match e {
            CaptureError::Io(e) => Self::Io(e),
            CaptureError::Layout(e) => Self::Layout(e),
            CaptureError::Format(what) => Self::Format(what),
            CaptureError::Checksum { offset } => Self::Checksum { offset },
        }
    }
}

/// A capture read as a replay source.
#[derive(Debug)]
pub struct CaptureSource<R> {
    reader: CaptureReader<R>,
    layout: SourceLayout,
    /// The session info revision last handed out, so carried copies are not replayed twice.
    session_update: Option<i32>,
}

impl CaptureSource<BufReader<File>> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        Ok(Self::wrap(CaptureReader::open(path)?))
    }
}

impl<R: Read + Seek> CaptureSource<R> {
    /// Reads and checks the file header and the variable table.
    pub fn new(reader: R) -> Result<Self, ReplayError> {
        Ok(Self::wrap(CaptureReader::new(reader)?))
    }

    fn wrap(reader: CaptureReader<R>) -> Self {
        let l = reader.layout();
        let layout = SourceLayout {
            vars: l.vars.clone(),
            buf_len: l.buf_len,
            tick_rate: l.tick_rate,
            session_info_capacity: l.session_info_capacity,
            player_car_only: l.player_car_only,
        };
        Self {
            reader,
            layout,
            session_update: None,
        }
    }

    pub fn clock_hz(&self) -> u64 {
        self.reader.clock_hz()
    }

    pub fn sdk_version(&self) -> i32 {
        self.reader.sdk_version()
    }

    pub fn chunk_frames(&self) -> u32 {
        self.reader.chunk_frames()
    }

    /// Reads the next record of any kind into `buf` (frames and session info text; other kinds clear it).
    /// `Ok(None)` after the end record or the last whole chunk.
    pub fn read(&mut self, buf: &mut Vec<u8>) -> Result<Option<Record>, ReplayError> {
        Ok(self.reader.read(buf)?)
    }

    /// The chunk index from the trailer; `None` for a capture without one, such as an interrupted recording.
    pub fn index(&mut self) -> Result<Option<Vec<ChunkEntry>>, ReplayError> {
        Ok(self.reader.index()?)
    }

    /// The metadata JSON from the trailer, as text.
    pub fn metadata(&mut self) -> Result<Option<String>, ReplayError> {
        Ok(self.reader.metadata()?)
    }

    /// Continues from the start of `chunk`; its carried session info is handed out as a revision.
    pub fn seek_chunk(&mut self, chunk: &ChunkEntry) {
        self.reader.seek_chunk(chunk.offset);
        self.session_update = None;
    }
}

impl<R: Read + Seek> FrameSource for CaptureSource<R> {
    fn layout(&self) -> &SourceLayout {
        &self.layout
    }

    fn next(&mut self, buf: &mut Vec<u8>) -> Result<Option<Item>, ReplayError> {
        loop {
            match self.reader.read(buf)? {
                None | Some(Record::End { .. }) => return Ok(None),
                Some(Record::Frame { .. }) => return Ok(Some(Item::Frame)),
                Some(Record::SessionInfo {
                    update, carried, ..
                }) => {
                    if carried && self.session_update == Some(update) {
                        continue;
                    }
                    self.session_update = Some(update);
                    return Ok(Some(Item::SessionInfo));
                }
                // Ticks the recorder missed; the replay simply continues with the next frame.
                Some(Record::Gap { .. }) => continue,
                Some(Record::Disconnect { .. }) => return Ok(Some(Item::Disconnect)),
            }
        }
    }

    fn rewind(&mut self) -> Result<(), ReplayError> {
        self.reader.rewind();
        self.session_update = None;
        Ok(())
    }
}
