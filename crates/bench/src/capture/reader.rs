//! Reads captures in order, or from a chunk found through the index.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use pitot_irsdk::layout::VarHeader;

use super::format::*;
use crate::replay::{FrameSource, Item, ReplayError, SourceLayout};

/// One record with its time in clock units since the capture started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record {
    Frame {
        time: u64,
        tick: i32,
        buffer: i32,
    },
    /// `carried` marks the copy that opens a chunk; it repeats the revision in effect.
    SessionInfo {
        time: u64,
        update: i32,
        carried: bool,
    },
    Gap {
        time: u64,
        missed: u32,
    },
    Disconnect {
        time: u64,
    },
    End {
        time: u64,
    },
}

/// A chunk as listed in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkEntry {
    pub offset: u64,
    pub first_frame: u64,
    pub start_time: u64,
    pub frames: u32,
}

const DAMAGED: ReplayError = ReplayError::Format("damaged capture record");

fn read_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<(), ReplayError> {
    reader.read_exact(buf).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => ReplayError::Format("capture is truncated"),
        _ => ReplayError::Io(e),
    })
}

/// A capture read as a replay source.
pub struct CaptureSource<R> {
    reader: R,
    layout: SourceLayout,
    clock_hz: u64,
    sdk_version: i32,
    chunk_frames: u32,
    records_start: u64,
    file_len: u64,
    next_chunk: u64,
    decompressor: zstd::bulk::Decompressor<'static>,
    compressed: Vec<u8>,
    chunk: Vec<u8>,
    at: usize,
    time: u64,
    prev_frame: Vec<u8>,
    /// The session info revision last handed out, so carried copies are not replayed twice.
    session_update: Option<i32>,
    ended: bool,
}

impl<R> std::fmt::Debug for CaptureSource<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSource")
            .field("layout", &self.layout)
            .field("clock_hz", &self.clock_hz)
            .finish_non_exhaustive()
    }
}

impl CaptureSource<BufReader<File>> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        Self::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read + Seek> CaptureSource<R> {
    /// Reads and checks the file header and the variable table.
    pub fn new(mut reader: R) -> Result<Self, ReplayError> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut head = [0u8; HEADER_SIZE];
        read_exact(&mut reader, &mut head)?;
        if &head[0..8] != MAGIC {
            return Err(ReplayError::Format("not a Pitot capture"));
        }
        if u32_at(&head, 8) != VERSION || u32_at(&head, 12) as usize != HEADER_SIZE {
            return Err(ReplayError::Format("unsupported capture version"));
        }
        if crc32(&head[..60]) != u32_at(&head, 60) {
            return Err(ReplayError::Checksum { offset: 0 });
        }
        let var_count = u32_at(&head, 28);
        let buf_len = u32_at(&head, 24) as usize;
        let tick_rate = i32_at(&head, 20);
        let clock_hz = u64_at(&head, 40);
        if var_count == 0 || var_count > MAX_VARS || buf_len == 0 || tick_rate <= 0 || clock_hz == 0
        {
            return Err(ReplayError::Format("invalid capture header"));
        }
        let mut table = vec![0u8; var_count as usize * VAR_HEADER_SIZE];
        read_exact(&mut reader, &mut table)?;
        if crc32(&table) != u32_at(&head, 52) {
            return Err(ReplayError::Checksum {
                offset: HEADER_SIZE as u64,
            });
        }
        let vars = table
            .as_chunks::<VAR_HEADER_SIZE>()
            .0
            .iter()
            .map(|raw| VarHeader::read(raw))
            .collect::<Result<Vec<_>, _>>()?;
        let records_start = (HEADER_SIZE + table.len()) as u64;
        Ok(Self {
            reader,
            layout: SourceLayout {
                vars,
                buf_len,
                tick_rate,
                session_info_capacity: u32_at(&head, 32) as usize,
                player_car_only: u32_at(&head, 16) & FLAG_PLAYER_CAR_ONLY != 0,
            },
            clock_hz,
            sdk_version: i32_at(&head, 36),
            chunk_frames: u32_at(&head, 48),
            records_start,
            file_len,
            next_chunk: records_start,
            decompressor: zstd::bulk::Decompressor::new()?,
            compressed: Vec::new(),
            chunk: Vec::new(),
            at: 0,
            time: 0,
            prev_frame: vec![0; buf_len],
            session_update: None,
            ended: false,
        })
    }

    pub fn clock_hz(&self) -> u64 {
        self.clock_hz
    }

    pub fn sdk_version(&self) -> i32 {
        self.sdk_version
    }

    pub fn chunk_frames(&self) -> u32 {
        self.chunk_frames
    }

    /// Loads the next chunk. `false` at the index, or at the end of a capture whose last chunk is incomplete.
    fn load_chunk(&mut self) -> Result<bool, ReplayError> {
        let offset = self.next_chunk;
        if offset + CHUNK_HEADER_SIZE as u64 > self.file_len {
            return Ok(false);
        }
        self.reader.seek(SeekFrom::Start(offset))?;
        let mut head = [0u8; CHUNK_HEADER_SIZE];
        read_exact(&mut self.reader, &mut head)?;
        if &head[0..4] == INDEX_MAGIC {
            return Ok(false);
        }
        if &head[0..4] != CHUNK_MAGIC || crc32(&head[..36]) != u32_at(&head, 36) {
            return Err(ReplayError::Checksum { offset });
        }
        let (compressed_len, len) = (u32_at(&head, 4), u32_at(&head, 8));
        if compressed_len > MAX_CHUNK_BYTES || len > MAX_CHUNK_BYTES {
            return Err(ReplayError::Format("capture chunk too large"));
        }
        let end = offset + (CHUNK_HEADER_SIZE as u64) + u64::from(compressed_len);
        if end > self.file_len {
            // The recorder stopped while writing this chunk.
            return Ok(false);
        }
        self.compressed.resize(compressed_len as usize, 0);
        read_exact(&mut self.reader, &mut self.compressed)?;
        self.chunk.clear();
        self.chunk.reserve(len as usize);
        // Zstandard checks the frame's content checksum while decompressing.
        self.decompressor
            .decompress_to_buffer(&self.compressed[..], &mut self.chunk)
            .map_err(|_| ReplayError::Checksum { offset })?;
        if self.chunk.len() != len as usize {
            return Err(ReplayError::Checksum { offset });
        }
        self.time = u64_at(&head, 28);
        self.at = 0;
        self.prev_frame.fill(0);
        self.next_chunk = end;
        Ok(true)
    }

    fn take(&mut self, n: usize) -> Result<&[u8], ReplayError> {
        let bytes = self.chunk.get(self.at..self.at + n).ok_or(DAMAGED)?;
        self.at += n;
        Ok(bytes)
    }

    fn take_i32(&mut self) -> Result<i32, ReplayError> {
        Ok(i32_at(self.take(4)?, 0))
    }

    /// Reads the next record of any kind into `buf` (frames and session info text; other kinds clear it).
    /// `Ok(None)` after the end record or the last whole chunk.
    pub fn read(&mut self, buf: &mut Vec<u8>) -> Result<Option<Record>, ReplayError> {
        buf.clear();
        if self.ended {
            return Ok(None);
        }
        if self.at >= self.chunk.len() && !self.load_chunk()? {
            self.ended = true;
            return Ok(None);
        }
        let kind = self.take(1)?[0];
        let delta = get_varint(&self.chunk, &mut self.at).ok_or(DAMAGED)?;
        self.time = self.time.saturating_add(delta);
        let time = self.time;
        let record = match kind {
            KIND_FRAME => {
                let tick = self.take_i32()?;
                let buffer = self.take_i32()?;
                let len = self.layout.buf_len;
                let row = self.chunk.get(self.at..self.at + len).ok_or(DAMAGED)?;
                buf.extend(row.iter().zip(&self.prev_frame).map(|(a, b)| a ^ b));
                self.at += len;
                self.prev_frame.copy_from_slice(buf);
                Record::Frame { time, tick, buffer }
            }
            KIND_SESSION_INFO | KIND_SESSION_CARRY => {
                let update = self.take_i32()?;
                let len = self.take_i32()? as u32 as usize;
                buf.extend_from_slice(self.take(len)?);
                Record::SessionInfo {
                    time,
                    update,
                    carried: kind == KIND_SESSION_CARRY,
                }
            }
            KIND_GAP => {
                let missed = self.take_i32()? as u32;
                Record::Gap { time, missed }
            }
            KIND_DISCONNECT => Record::Disconnect { time },
            KIND_END => {
                self.ended = true;
                Record::End { time }
            }
            _ => return Err(DAMAGED),
        };
        Ok(Some(record))
    }

    /// The chunk index from the trailer; `None` for a capture without one, such as an interrupted recording.
    pub fn index(&mut self) -> Result<Option<Vec<ChunkEntry>>, ReplayError> {
        let Some((index_offset, _, _)) = self.trailer()? else {
            return Ok(None);
        };
        self.reader.seek(SeekFrom::Start(index_offset))?;
        let mut head = [0u8; 8];
        read_exact(&mut self.reader, &mut head)?;
        let count = u32_at(&head, 4) as u64;
        if &head[0..4] != INDEX_MAGIC
            || index_offset + 8 + count * INDEX_ENTRY_SIZE as u64 + 4 > self.file_len
        {
            return Err(ReplayError::Format("invalid capture index"));
        }
        let mut body = vec![0u8; count as usize * INDEX_ENTRY_SIZE + 4];
        read_exact(&mut self.reader, &mut body)?;
        let (entries, crc) = body.split_at(body.len() - 4);
        let mut whole = head.to_vec();
        whole.extend_from_slice(entries);
        if crc32(&whole) != u32_at(crc, 0) {
            return Err(ReplayError::Checksum {
                offset: index_offset,
            });
        }
        Ok(Some(
            entries
                .as_chunks::<INDEX_ENTRY_SIZE>()
                .0
                .iter()
                .map(|e| ChunkEntry {
                    offset: u64_at(e, 0),
                    first_frame: u64_at(e, 8),
                    start_time: u64_at(e, 16),
                    frames: u32_at(e, 24),
                })
                .collect(),
        ))
    }

    /// The metadata JSON from the trailer, as text.
    pub fn metadata(&mut self) -> Result<Option<String>, ReplayError> {
        let Some((_, offset, len)) = self.trailer()? else {
            return Ok(None);
        };
        self.reader.seek(SeekFrom::Start(offset))?;
        let mut text = vec![0u8; len as usize];
        read_exact(&mut self.reader, &mut text)?;
        String::from_utf8(text)
            .map(Some)
            .map_err(|_| ReplayError::Format("capture metadata is not UTF-8"))
    }

    /// Index offset, metadata offset, and metadata length, when the trailer is present and intact.
    fn trailer(&mut self) -> Result<Option<(u64, u64, u32)>, ReplayError> {
        if self.file_len < self.records_start + TRAILER_SIZE as u64 {
            return Ok(None);
        }
        self.reader
            .seek(SeekFrom::Start(self.file_len - TRAILER_SIZE as u64))?;
        let mut trailer = [0u8; TRAILER_SIZE];
        read_exact(&mut self.reader, &mut trailer)?;
        if &trailer[0..8] != TRAILER_MAGIC || crc32(&trailer[..28]) != u32_at(&trailer, 28) {
            return Ok(None);
        }
        let (index, metadata, len) = (
            u64_at(&trailer, 8),
            u64_at(&trailer, 16),
            u32_at(&trailer, 24),
        );
        if index >= self.file_len || metadata + u64::from(len) > self.file_len {
            return Err(ReplayError::Format("invalid capture trailer"));
        }
        Ok(Some((index, metadata, len)))
    }

    /// Continues from the start of `chunk`; its carried session info is handed out as a revision.
    pub fn seek_chunk(&mut self, chunk: &ChunkEntry) {
        self.next_chunk = chunk.offset;
        self.chunk.clear();
        self.at = 0;
        self.session_update = None;
        self.ended = false;
    }
}

impl<R: Read + Seek> FrameSource for CaptureSource<R> {
    fn layout(&self) -> &SourceLayout {
        &self.layout
    }

    fn next(&mut self, buf: &mut Vec<u8>) -> Result<Option<Item>, ReplayError> {
        loop {
            match self.read(buf)? {
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
        self.next_chunk = self.records_start;
        self.chunk.clear();
        self.at = 0;
        self.session_update = None;
        self.ended = false;
        Ok(())
    }
}
