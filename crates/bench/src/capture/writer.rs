//! Writes captures: records are collected per chunk, compressed when the chunk is full, and the index, metadata,
//! and trailer are written at the end.

use std::io::{self, Write};

use serde::Serialize;

use super::format::*;
use crate::replay::SourceLayout;

/// Where a capture's data came from, for its metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceInfo {
    /// `live`, `irdt`, or `ibt`.
    pub kind: String,
    /// File name of a converted source, without its directory.
    pub file: Option<String>,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriterOptions {
    /// Zstandard level: 3 keeps live recording light, 19 makes the smallest files.
    pub level: i32,
    pub chunk_seconds: u32,
    /// Clock frequency of record times.
    pub clock_hz: u64,
    pub sdk_version: i32,
    pub source: SourceInfo,
}

/// Counts written into the metadata and returned by [`CaptureWriter::finish`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Counts {
    pub frames: u64,
    pub session_info_revisions: u64,
    pub gaps: u64,
    pub missed_ticks: u64,
    pub disconnects: u64,
    pub chunks: u64,
}

#[derive(Debug, Serialize)]
struct Metadata<'a> {
    format: &'static str,
    version: u32,
    tool: &'static str,
    tool_version: &'static str,
    source: &'a SourceInfo,
    compression: &'static str,
    level: i32,
    clock_hz: u64,
    tick_rate: i32,
    seconds: f64,
    #[serde(flatten)]
    counts: Counts,
}

struct IndexEntry {
    offset: u64,
    first_frame: u64,
    start_time: u64,
    frames: u32,
}

pub struct CaptureWriter<W: Write> {
    out: W,
    options: WriterOptions,
    tick_rate: i32,
    buf_len: usize,
    chunk_frames: u32,
    compressor: zstd::bulk::Compressor<'static>,
    /// Records of the chunk being collected.
    records: Vec<u8>,
    record_count: u32,
    frames_in_chunk: u32,
    chunk_first_frame: u64,
    chunk_start: u64,
    last_time: u64,
    prev_frame: Vec<u8>,
    /// The session info revision in effect, carried into every new chunk.
    session: Option<(i32, Vec<u8>)>,
    index: Vec<IndexEntry>,
    position: u64,
    counts: Counts,
    first_time: Option<u64>,
}

impl<W: Write> std::fmt::Debug for CaptureWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureWriter")
            .field("counts", &self.counts)
            .finish_non_exhaustive()
    }
}

impl<W: Write> CaptureWriter<W> {
    /// Writes the file header and variable table.
    pub fn new(mut out: W, layout: &SourceLayout, options: WriterOptions) -> io::Result<Self> {
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, what.to_owned());
        if layout.vars.is_empty() || layout.vars.len() > MAX_VARS as usize {
            return Err(invalid("variable count out of range"));
        }
        if layout.buf_len == 0 || layout.tick_rate <= 0 || options.clock_hz == 0 {
            return Err(invalid(
                "buffer length, tick rate, and clock must be positive",
            ));
        }
        let mut table = vec![0u8; layout.vars.len() * VAR_HEADER_SIZE];
        for (var, raw) in layout
            .vars
            .iter()
            .zip(table.as_chunks_mut::<VAR_HEADER_SIZE>().0)
        {
            var.write(raw).map_err(|e| invalid(&e.to_string()))?;
        }
        let chunk_frames = (options.chunk_seconds.max(1)).saturating_mul(layout.tick_rate as u32);
        let mut head = [0u8; HEADER_SIZE];
        head[0..8].copy_from_slice(MAGIC);
        put_u32(&mut head, 8, VERSION);
        put_u32(&mut head, 12, HEADER_SIZE as u32);
        put_u32(
            &mut head,
            16,
            if layout.player_car_only {
                FLAG_PLAYER_CAR_ONLY
            } else {
                0
            },
        );
        put_u32(&mut head, 20, layout.tick_rate as u32);
        put_u32(&mut head, 24, layout.buf_len as u32);
        put_u32(&mut head, 28, layout.vars.len() as u32);
        put_u32(&mut head, 32, layout.session_info_capacity as u32);
        put_u32(&mut head, 36, options.sdk_version as u32);
        put_u64(&mut head, 40, options.clock_hz);
        put_u32(&mut head, 48, chunk_frames);
        put_u32(&mut head, 52, crc32(&table));
        let crc = crc32(&head[..60]);
        put_u32(&mut head, 60, crc);
        out.write_all(&head)?;
        out.write_all(&table)?;

        let mut compressor = zstd::bulk::Compressor::new(options.level)?;
        compressor.include_checksum(true)?;
        Ok(Self {
            out,
            tick_rate: layout.tick_rate,
            buf_len: layout.buf_len,
            chunk_frames,
            compressor,
            records: Vec::with_capacity(chunk_frames as usize * (layout.buf_len + 16)),
            record_count: 0,
            frames_in_chunk: 0,
            chunk_first_frame: 0,
            chunk_start: 0,
            last_time: 0,
            prev_frame: vec![0; layout.buf_len],
            session: None,
            index: Vec::new(),
            position: (HEADER_SIZE + table.len()) as u64,
            counts: Counts::default(),
            first_time: None,
            options,
        })
    }

    /// Starts a record of `kind` at `time`, opening a chunk first when none is open.
    fn begin(&mut self, kind: u8, time: u64) {
        self.first_time.get_or_insert(time);
        if self.records.is_empty() {
            self.chunk_start = time;
            self.chunk_first_frame = self.counts.frames;
            self.last_time = time;
            self.record_count = 0;
            self.frames_in_chunk = 0;
            self.prev_frame.fill(0);
            if let Some((update, text)) = &self.session {
                self.records.push(KIND_SESSION_CARRY);
                put_varint(&mut self.records, 0);
                self.records.extend_from_slice(&update.to_le_bytes());
                self.records
                    .extend_from_slice(&(text.len() as u32).to_le_bytes());
                self.records.extend_from_slice(text);
                self.record_count += 1;
            }
        }
        self.records.push(kind);
        // Times never run backwards in a capture; a source that does is clamped.
        put_varint(&mut self.records, time.saturating_sub(self.last_time));
        self.last_time = self.last_time.max(time);
        self.record_count += 1;
    }

    /// A data buffer row: `tick` is the SDK `tickCount`, `buffer` the index of the buffer it was read from.
    pub fn frame(&mut self, time: u64, tick: i32, buffer: i32, row: &[u8]) -> io::Result<()> {
        if row.len() != self.buf_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame length does not match the buffer length",
            ));
        }
        self.begin(KIND_FRAME, time);
        self.records.extend_from_slice(&tick.to_le_bytes());
        self.records.extend_from_slice(&buffer.to_le_bytes());
        self.records
            .extend(row.iter().zip(&self.prev_frame).map(|(a, b)| a ^ b));
        self.prev_frame.copy_from_slice(row);
        self.counts.frames += 1;
        self.frames_in_chunk += 1;
        if self.frames_in_chunk >= self.chunk_frames {
            self.flush_chunk()?;
        }
        Ok(())
    }

    /// A session info revision; `text` without its NUL padding.
    pub fn session_info(&mut self, time: u64, update: i32, text: &[u8]) -> io::Result<()> {
        self.begin(KIND_SESSION_INFO, time);
        self.records.extend_from_slice(&update.to_le_bytes());
        self.records
            .extend_from_slice(&(text.len() as u32).to_le_bytes());
        self.records.extend_from_slice(text);
        self.session = Some((update, text.to_vec()));
        self.counts.session_info_revisions += 1;
        Ok(())
    }

    /// Ticks the recorder did not see.
    pub fn gap(&mut self, time: u64, missed: u32) -> io::Result<()> {
        self.begin(KIND_GAP, time);
        self.records.extend_from_slice(&missed.to_le_bytes());
        self.counts.gaps += 1;
        self.counts.missed_ticks += u64::from(missed);
        Ok(())
    }

    pub fn disconnect(&mut self, time: u64) -> io::Result<()> {
        self.begin(KIND_DISCONNECT, time);
        self.counts.disconnects += 1;
        Ok(())
    }

    fn flush_chunk(&mut self) -> io::Result<()> {
        if self.records.is_empty() {
            return Ok(());
        }
        let compressed = self.compressor.compress(&self.records)?;
        if compressed.len() > MAX_CHUNK_BYTES as usize
            || self.records.len() > MAX_CHUNK_BYTES as usize
        {
            return Err(io::Error::other("chunk too large"));
        }
        let mut head = [0u8; CHUNK_HEADER_SIZE];
        head[0..4].copy_from_slice(CHUNK_MAGIC);
        put_u32(&mut head, 4, compressed.len() as u32);
        put_u32(&mut head, 8, self.records.len() as u32);
        put_u32(&mut head, 12, self.record_count);
        put_u64(&mut head, 16, self.chunk_first_frame);
        put_u32(&mut head, 24, self.frames_in_chunk);
        put_u64(&mut head, 28, self.chunk_start);
        let crc = crc32(&head[..36]);
        put_u32(&mut head, 36, crc);
        self.out.write_all(&head)?;
        self.out.write_all(&compressed)?;
        self.index.push(IndexEntry {
            offset: self.position,
            first_frame: self.chunk_first_frame,
            start_time: self.chunk_start,
            frames: self.frames_in_chunk,
        });
        self.position += (CHUNK_HEADER_SIZE + compressed.len()) as u64;
        self.counts.chunks += 1;
        self.records.clear();
        Ok(())
    }

    /// Writes the end record, the last chunk, the index, the metadata, and the trailer. Returns the counts.
    pub fn finish(mut self, time: u64) -> io::Result<Counts> {
        self.begin(KIND_END, time);
        self.flush_chunk()?;

        let index_offset = self.position;
        let mut index = Vec::with_capacity(8 + self.index.len() * INDEX_ENTRY_SIZE + 4);
        index.extend_from_slice(INDEX_MAGIC);
        index.extend_from_slice(&(self.index.len() as u32).to_le_bytes());
        for entry in &self.index {
            index.extend_from_slice(&entry.offset.to_le_bytes());
            index.extend_from_slice(&entry.first_frame.to_le_bytes());
            index.extend_from_slice(&entry.start_time.to_le_bytes());
            index.extend_from_slice(&entry.frames.to_le_bytes());
        }
        let crc = crc32(&index);
        index.extend_from_slice(&crc.to_le_bytes());
        self.out.write_all(&index)?;
        self.position += index.len() as u64;

        let metadata_offset = self.position;
        let span = time.saturating_sub(self.first_time.unwrap_or(time));
        let metadata = Metadata {
            format: "pitot-capture",
            version: VERSION,
            tool: "pitot-bench",
            tool_version: env!("CARGO_PKG_VERSION"),
            source: &self.options.source,
            compression: "zstd",
            level: self.options.level,
            clock_hz: self.options.clock_hz,
            tick_rate: self.tick_rate,
            seconds: span as f64 / self.options.clock_hz as f64,
            counts: self.counts,
        };
        let mut json = serde_json::to_vec_pretty(&metadata).map_err(io::Error::other)?;
        json.push(b'\n');
        self.out.write_all(&json)?;
        self.position += json.len() as u64;

        let mut trailer = [0u8; TRAILER_SIZE];
        trailer[0..8].copy_from_slice(TRAILER_MAGIC);
        put_u64(&mut trailer, 8, index_offset);
        put_u64(&mut trailer, 16, metadata_offset);
        put_u32(&mut trailer, 24, json.len() as u32);
        let crc = crc32(&trailer[..28]);
        put_u32(&mut trailer, 28, crc);
        self.out.write_all(&trailer)?;
        self.out.flush()?;
        Ok(self.counts)
    }
}
