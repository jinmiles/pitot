//! Reader for irDashies `.irdt` v1 telemetry tapes: raw captures of the live SDK shared memory.
//!
//! Layout (little-endian): a 96-byte file header, the original SDK header, the variable header table, then
//! records of a 40-byte header and a payload. Frame payloads are raw data buffer rows; session info payloads
//! are the whole session info area. Every payload carries an FNV-1a checksum.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use pitot_irsdk::layout::{Header, VarHeader};

use super::{FrameSource, Item, ReplayError, SourceLayout};

const MAGIC: &[u8; 8] = b"IRDTRCE\0";
const FORMAT_VERSION: u32 = 1;
const ENDIAN_MARKER: u32 = 0x0102_0304;
const FILE_HEADER_SIZE: usize = 96;
const RECORD_HEADER_SIZE: usize = 40;
const MAX_VARS: u32 = 4096;
const MAX_PAYLOAD: u32 = 64 * 1024 * 1024;

const KIND_FRAME: u32 = 1;
const KIND_SESSION_INFO: u32 = 2;
const KIND_GAP: u32 = 3;
const KIND_DISCONNECT: u32 = 4;
const KIND_END: u32 = 5;

/// FNV-1a, 32-bit, as the tape format uses for schema and payload checksums.
pub(crate) fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811C_9DC5, |hash, &b| {
        (hash ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

/// An `.irdt` v1 tape read sequentially.
#[derive(Debug)]
pub struct IrdtSource<R> {
    reader: R,
    layout: SourceLayout,
    records_start: u64,
    position: u64,
    file_len: u64,
}

impl IrdtSource<BufReader<File>> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        Self::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read + Seek> IrdtSource<R> {
    /// Reads and checks the file header, SDK header, and variable table.
    pub fn new(mut reader: R) -> Result<Self, ReplayError> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut head = [0u8; FILE_HEADER_SIZE];
        read_exact(&mut reader, &mut head)?;
        if &head[0..8] != MAGIC {
            return Err(ReplayError::Format("not an .irdt tape"));
        }
        let sizes_ok = u32_at(&head, 16) as usize == FILE_HEADER_SIZE
            && u32_at(&head, 20) as usize == Header::SIZE
            && u32_at(&head, 24) as usize == VarHeader::SIZE;
        if u32_at(&head, 8) != FORMAT_VERSION || u32_at(&head, 12) != ENDIAN_MARKER || !sizes_ok {
            return Err(ReplayError::Format("unsupported .irdt version or layout"));
        }
        let var_count = u32_at(&head, 28);
        if var_count == 0 || var_count > MAX_VARS {
            return Err(ReplayError::Format("invalid .irdt variable count"));
        }
        let schema_checksum = u32_at(&head, 56);

        let mut sdk = [0u8; Header::SIZE];
        read_exact(&mut reader, &mut sdk)?;
        let header = Header::read(&sdk)?;
        let mut table = vec![0u8; var_count as usize * VarHeader::SIZE];
        read_exact(&mut reader, &mut table)?;
        if fnv1a(&table) != schema_checksum {
            return Err(ReplayError::Format(
                ".irdt variable table checksum mismatch",
            ));
        }
        let vars = table
            .as_chunks::<{ VarHeader::SIZE }>()
            .0
            .iter()
            .map(|chunk| VarHeader::read(chunk))
            .collect::<Result<Vec<_>, _>>()?;
        let buf_len = usize::try_from(header.buf_len)
            .ok()
            .filter(|&len| len > 0)
            .ok_or(ReplayError::Format("invalid .irdt buffer length"))?;
        let session_info_capacity = usize::try_from(header.session_info_len)
            .ok()
            .filter(|&len| len > 0)
            .unwrap_or(super::SIM_SESSION_INFO_CAPACITY);
        let records_start = (FILE_HEADER_SIZE + Header::SIZE + table.len()) as u64;
        Ok(Self {
            reader,
            layout: SourceLayout {
                vars,
                buf_len,
                tick_rate: header.tick_rate,
                session_info_capacity,
                player_car_only: false,
            },
            records_start,
            position: records_start,
            file_len,
        })
    }
}

fn read_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<(), ReplayError> {
    reader.read_exact(buf).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => ReplayError::Format(".irdt tape is truncated"),
        _ => ReplayError::Io(e),
    })
}

impl<R: Read + Seek> FrameSource for IrdtSource<R> {
    fn layout(&self) -> &SourceLayout {
        &self.layout
    }

    fn next(&mut self, buf: &mut Vec<u8>) -> Result<Option<Item>, ReplayError> {
        loop {
            // A tape cut off after a whole record still ends cleanly.
            if self.position == self.file_len {
                return Ok(None);
            }
            let offset = self.position;
            let mut head = [0u8; RECORD_HEADER_SIZE];
            read_exact(&mut self.reader, &mut head)?;
            let kind = u32_at(&head, 0);
            let payload_len = u32_at(&head, 8);
            if u32_at(&head, 4) as usize != RECORD_HEADER_SIZE || payload_len > MAX_PAYLOAD {
                return Err(ReplayError::Format("invalid .irdt record header"));
            }
            buf.clear();
            buf.resize(payload_len as usize, 0);
            read_exact(&mut self.reader, buf)?;
            self.position += (RECORD_HEADER_SIZE + buf.len()) as u64;
            if fnv1a(buf) != u32_at(&head, 32) {
                return Err(ReplayError::Checksum { offset });
            }
            match kind {
                KIND_FRAME if buf.len() == self.layout.buf_len => return Ok(Some(Item::Frame)),
                KIND_FRAME => {
                    return Err(ReplayError::Format(
                        ".irdt frame does not match the buffer length",
                    ));
                }
                KIND_SESSION_INFO => return Ok(Some(Item::SessionInfo)),
                // Ticks the recorder missed; the replay simply continues with the next frame.
                KIND_GAP => continue,
                KIND_DISCONNECT => return Ok(Some(Item::Disconnect)),
                KIND_END => return Ok(None),
                _ => return Err(ReplayError::Format("unknown .irdt record kind")),
            }
        }
    }

    fn rewind(&mut self) -> Result<(), ReplayError> {
        self.reader.seek(SeekFrom::Start(self.records_start))?;
        self.position = self.records_start;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pitot_irsdk::layout::{SDK_VERSION, STATUS_CONNECTED, VarType};
    use std::io::Cursor;

    pub(crate) const ROW: usize = 16;

    pub(crate) fn vars() -> Vec<VarHeader> {
        vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Float, 8, 1, "ThrottleRaw").expect("valid"),
            VarHeader::new(VarType::Int, 12, 1, "Gear").expect("valid"),
        ]
    }

    pub(crate) fn row(n: u32) -> Vec<u8> {
        let mut row = vec![0u8; ROW];
        row[0..8].copy_from_slice(&f64::from(n).to_le_bytes());
        row[8..12].copy_from_slice(&(n as f32 / 100.0).to_le_bytes());
        row[12..16].copy_from_slice(&3i32.to_le_bytes());
        row
    }

    fn record(out: &mut Vec<u8>, kind: u32, payload: &[u8]) {
        let mut head = [0u8; RECORD_HEADER_SIZE];
        head[0..4].copy_from_slice(&kind.to_le_bytes());
        head[4..8].copy_from_slice(&(RECORD_HEADER_SIZE as u32).to_le_bytes());
        head[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        head[32..36].copy_from_slice(&fnv1a(payload).to_le_bytes());
        out.extend_from_slice(&head);
        out.extend_from_slice(payload);
    }

    /// A tape: session info, `frames` frames with a gap and a session info revision in the middle, end.
    pub(crate) fn tape(frames: u32) -> Vec<u8> {
        let vars = vars();
        let mut header = Header::default();
        header.ver = SDK_VERSION;
        header.status = STATUS_CONNECTED;
        header.tick_rate = 60;
        header.session_info_len = 256;
        header.num_vars = vars.len() as i32;
        header.num_buf = 3;
        header.buf_len = ROW as i32;
        let mut sdk = [0u8; Header::SIZE];
        header.write(&mut sdk).expect("fits");
        let mut table = Vec::new();
        for var in &vars {
            let mut raw = [0u8; VarHeader::SIZE];
            var.write(&mut raw).expect("fits");
            table.extend_from_slice(&raw);
        }
        let mut out = vec![0u8; FILE_HEADER_SIZE];
        out[0..8].copy_from_slice(MAGIC);
        for (at, value) in [
            (8, FORMAT_VERSION),
            (12, ENDIAN_MARKER),
            (16, 96),
            (20, 112),
            (24, 144),
        ] {
            out[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        out[28..32].copy_from_slice(&(vars.len() as u32).to_le_bytes());
        out[56..60].copy_from_slice(&fnv1a(&table).to_le_bytes());
        out.extend_from_slice(&sdk);
        out.extend_from_slice(&table);
        let mut info = vec![0u8; 256];
        info[..12].copy_from_slice(b"WeekendInfo:");
        record(&mut out, KIND_SESSION_INFO, &info);
        for n in 0..frames {
            if n == frames / 2 {
                record(&mut out, KIND_GAP, &[]);
                record(&mut out, KIND_SESSION_INFO, b"DriverInfo:\0\0\0");
            }
            record(&mut out, KIND_FRAME, &row(n));
        }
        record(&mut out, KIND_END, &[]);
        out
    }

    fn items(source: &mut IrdtSource<Cursor<Vec<u8>>>) -> Vec<(Item, Vec<u8>)> {
        let mut buf = Vec::new();
        let mut out = Vec::new();
        while let Some(item) = source.next(&mut buf).expect("reads") {
            out.push((item, buf.clone()));
        }
        out
    }

    #[test]
    fn reads_layout_and_records_in_order() {
        let mut source = IrdtSource::new(Cursor::new(tape(4))).expect("opens");
        let layout = source.layout().clone();
        assert_eq!(
            (
                layout.buf_len,
                layout.tick_rate,
                layout.session_info_capacity
            ),
            (ROW, 60, 256)
        );
        assert_eq!(layout.vars, vars());
        assert!(!layout.player_car_only);
        let items = items(&mut source);
        let kinds: Vec<Item> = items.iter().map(|(item, _)| *item).collect();
        use Item::{Frame, SessionInfo};
        assert_eq!(
            kinds,
            [SessionInfo, Frame, Frame, SessionInfo, Frame, Frame],
            "gap skipped"
        );
        assert_eq!(items[1].1, row(0));
        assert_eq!(items[5].1, row(3));
        assert_eq!(&items[3].1[..11], b"DriverInfo:");
    }

    #[test]
    fn rewinds_to_the_first_record() {
        let mut source = IrdtSource::new(Cursor::new(tape(2))).expect("opens");
        let first = items(&mut source);
        source.rewind().expect("rewinds");
        assert_eq!(items(&mut source), first);
    }

    #[test]
    fn rejects_bad_files() {
        let mut bad_magic = tape(2);
        bad_magic[0] = b'X';
        assert!(matches!(
            IrdtSource::new(Cursor::new(bad_magic)),
            Err(ReplayError::Format("not an .irdt tape"))
        ));
        let mut bad_schema = tape(2);
        bad_schema[FILE_HEADER_SIZE + Header::SIZE + 20] ^= 1;
        assert!(matches!(
            IrdtSource::new(Cursor::new(bad_schema)),
            Err(ReplayError::Format(
                ".irdt variable table checksum mismatch"
            ))
        ));
        let mut bad_payload = tape(2);
        let last_frame_byte = bad_payload.len() - RECORD_HEADER_SIZE - 1;
        bad_payload[last_frame_byte] ^= 1;
        let mut source = IrdtSource::new(Cursor::new(bad_payload)).expect("opens");
        let mut buf = Vec::new();
        let result = (0..4).try_for_each(|_| source.next(&mut buf).map(|_| ()));
        assert!(matches!(result, Err(ReplayError::Checksum { .. })));
        let mut cut = tape(2);
        cut.truncate(cut.len() - 30);
        let mut source = IrdtSource::new(Cursor::new(cut)).expect("opens");
        let result = (0..5).try_for_each(|_| source.next(&mut buf).map(|_| ()));
        assert!(matches!(
            result,
            Err(ReplayError::Format(".irdt tape is truncated"))
        ));
    }

    #[test]
    fn checksum_matches_reference_values() {
        assert_eq!(fnv1a(b""), 0x811C_9DC5);
        assert_eq!(fnv1a(b"a"), 0xE40C_292C);
    }
}
