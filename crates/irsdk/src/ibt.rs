//! Reader for `.ibt` telemetry files written by the sim.
//!
//! A file holds the SDK header, the disk sub-header, the variable table, one session info snapshot taken
//! when recording started, and fixed-size records of the player car's variables. Records are read on demand
//! into a reusable [`Record`], so iteration does not allocate and large files are never loaded whole.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::layout::{DiskSubHeader, Field, Header, LayoutError, VarHeader};

/// Errors from reading an `.ibt` file.
#[derive(Debug)]
pub enum IbtError {
    Io(io::Error),
    Layout(LayoutError),
    /// The disk sub-header claims more records than the file holds.
    Truncated {
        expected: usize,
        available: usize,
    },
    /// A variable needed for the operation is not in the file.
    MissingVariable(&'static str),
    /// The record index is past the last record.
    RecordOutOfRange {
        index: usize,
        count: usize,
    },
    /// The record buffer was not created by this reader.
    RecordSizeMismatch {
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for IbtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Layout(e) => write!(f, "invalid layout: {e}"),
            Self::Truncated {
                expected,
                available,
            } => write!(f, "file claims {expected} records but holds {available}"),
            Self::MissingVariable(name) => write!(f, "variable `{name}` is not in the file"),
            Self::RecordOutOfRange { index, count } => {
                write!(f, "record {index} is out of range ({count} records)")
            }
            Self::RecordSizeMismatch { expected, actual } => {
                write!(f, "record buffer is {actual} bytes, expected {expected}")
            }
        }
    }
}

impl std::error::Error for IbtError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Layout(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for IbtError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<LayoutError> for IbtError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// One record (a row of variables). Create it with [`IbtReader::new_record`] and reuse it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    bytes: Vec<u8>,
}

impl Record {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Reader over any seekable byte source.
#[derive(Debug)]
pub struct IbtReader<R> {
    source: R,
    header: Header,
    disk: DiskSubHeader,
    vars: Vec<VarHeader>,
    session_info: Vec<u8>,
    data_offset: u64,
    record_len: usize,
    record_count: usize,
    /// Index of the record the next sequential read returns.
    next: usize,
    /// Stream offset after the last read, to skip redundant seeks during sequential reads.
    stream_at: Option<u64>,
}

impl IbtReader<BufReader<File>> {
    /// Opens a file with a buffered reader.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IbtError> {
        Self::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read + Seek> IbtReader<R> {
    /// Reads the headers, variable table, and session info, and checks them against the file length.
    ///
    /// The record count comes from the disk sub-header. A count of zero, which a recording that was not
    /// closed cleanly can leave behind, falls back to the number of complete records in the file.
    pub fn new(mut source: R) -> Result<Self, IbtError> {
        let file_len = source.seek(SeekFrom::End(0))?;
        let available = usize::try_from(file_len).map_err(|_| LayoutError::OutOfRange {
            field: "file length",
        })?;
        let mut head = [0u8; Header::SIZE + DiskSubHeader::SIZE];
        if available < head.len() {
            return Err(LayoutError::Truncated {
                needed: head.len(),
                available,
            }
            .into());
        }
        source.seek(SeekFrom::Start(0))?;
        source.read_exact(&mut head)?;
        let header = Header::read(&head)?;
        let disk = DiskSubHeader::read(&head)?;
        header.validate_tables(available)?;

        let table = read_range(&mut source, header.var_table_range(available)?)?;
        let vars = table
            .as_chunks::<{ VarHeader::SIZE }>()
            .0
            .iter()
            .map(|chunk| VarHeader::read(chunk))
            .collect::<Result<Vec<_>, _>>()?;
        let mut session_info = read_range(&mut source, header.session_info_range(available)?)?;
        if let Some(end) = session_info.iter().position(|&b| b == 0) {
            session_info.truncate(end);
        }

        // validate_tables checked that bufLen is positive.
        let record_len = usize::try_from(header.buf_len)
            .map_err(|_| LayoutError::OutOfRange { field: "bufLen" })?;
        let data_start =
            usize::try_from(header.var_buf[0].buf_offset).map_err(|_| LayoutError::OutOfRange {
                field: "varBuf.bufOffset",
            })?;
        if data_start > available {
            return Err(LayoutError::OutOfRange {
                field: "varBuf.bufOffset",
            }
            .into());
        }
        let in_file = (available - data_start) / record_len;
        let claimed =
            usize::try_from(disk.session_record_count).map_err(|_| LayoutError::OutOfRange {
                field: "sessionRecordCount",
            })?;
        let record_count = match claimed {
            0 => in_file,
            n if n <= in_file => n,
            n => {
                return Err(IbtError::Truncated {
                    expected: n,
                    available: in_file,
                });
            }
        };

        Ok(Self {
            source,
            header,
            disk,
            vars,
            session_info,
            data_offset: data_start as u64,
            record_len,
            record_count,
            next: 0,
            stream_at: None,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn disk_header(&self) -> &DiskSubHeader {
        &self.disk
    }

    pub fn vars(&self) -> &[VarHeader] {
        &self.vars
    }

    /// Session info text as stored (Windows-1252 encoded YAML), without the NUL padding.
    pub fn session_info(&self) -> &[u8] {
        &self.session_info
    }

    pub fn record_count(&self) -> usize {
        self.record_count
    }

    pub fn record_len(&self) -> usize {
        self.record_len
    }

    /// Resolves a variable by name. `Ok(None)` means the file does not contain it.
    pub fn field(&self, name: &str) -> Result<Option<Field>, LayoutError> {
        self.vars
            .iter()
            .find(|var| var.name_bytes() == name.as_bytes())
            .map(|var| Field::new(var, self.record_len))
            .transpose()
    }

    /// Allocates a record buffer sized for this file.
    pub fn new_record(&self) -> Record {
        Record {
            bytes: vec![0; self.record_len],
        }
    }

    /// Reads record `index` and moves the sequential position after it.
    pub fn read_record(&mut self, index: usize, record: &mut Record) -> Result<(), IbtError> {
        if index >= self.record_count {
            return Err(IbtError::RecordOutOfRange {
                index,
                count: self.record_count,
            });
        }
        if record.bytes.len() != self.record_len {
            return Err(IbtError::RecordSizeMismatch {
                expected: self.record_len,
                actual: record.bytes.len(),
            });
        }
        let at = self.data_offset + (index as u64) * (self.record_len as u64);
        if self.stream_at != Some(at) {
            self.stream_at = None;
            self.source.seek(SeekFrom::Start(at))?;
        }
        self.source.read_exact(&mut record.bytes)?;
        self.stream_at = Some(at + self.record_len as u64);
        self.next = index + 1;
        Ok(())
    }

    /// Reads the next record in order. Returns `false` after the last record.
    pub fn next_record(&mut self, record: &mut Record) -> Result<bool, IbtError> {
        if self.next >= self.record_count {
            return Ok(false);
        }
        self.read_record(self.next, record)?;
        Ok(true)
    }

    /// Index of the record the next sequential read returns.
    pub fn position(&self) -> usize {
        self.next
    }

    /// Moves the sequential position back to the first record.
    pub fn rewind(&mut self) {
        self.next = 0;
    }

    /// Moves the sequential position to the first record at or after (`session_num`, `session_time`) and
    /// returns its index, or [`IbtReader::record_count`] when every record is earlier.
    ///
    /// Records are assumed to be ordered by `SessionNum` and then `SessionTime`. `SessionTime` steps are
    /// not assumed to be uniform; the sim occasionally advances it by two ticks.
    pub fn seek_session_time(
        &mut self,
        session_num: i32,
        session_time: f64,
    ) -> Result<usize, IbtError> {
        let num = self
            .field("SessionNum")?
            .ok_or(IbtError::MissingVariable("SessionNum"))?;
        let time = self
            .field("SessionTime")?
            .ok_or(IbtError::MissingVariable("SessionTime"))?;
        let mut record = self.new_record();
        let (mut lo, mut hi) = (0, self.record_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            self.read_record(mid, &mut record)?;
            let num_at = num
                .i32(record.bytes(), 0)
                .ok_or(IbtError::MissingVariable("SessionNum"))?;
            let time_at = time
                .f64(record.bytes(), 0)
                .ok_or(IbtError::MissingVariable("SessionTime"))?;
            if (num_at, time_at) < (session_num, session_time) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        self.next = lo;
        Ok(lo)
    }
}

fn read_range<R: Read + Seek>(
    source: &mut R,
    range: std::ops::Range<usize>,
) -> Result<Vec<u8>, IbtError> {
    let mut bytes = vec![0; range.len()];
    source.seek(SeekFrom::Start(range.start as u64))?;
    source.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{SDK_VERSION, STATUS_CONNECTED, VarType};
    use std::io::Cursor;

    const RECORD_LEN: usize = 32;
    const TABLE_AT: usize = Header::SIZE + DiskSubHeader::SIZE;
    const YAML: &[u8] = b"---\nWeekendInfo:\n TrackName: synthetic\n...\n";

    /// Synthetic file: SessionTime, SessionNum, ThrottleRaw, and a 4-car CarIdxLapDistPct array.
    fn synthetic(samples: &[(i32, f64)], claimed: i32, extra: &[u8]) -> Vec<u8> {
        let vars = [
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").and_then(|v| v.with_unit("s")),
            VarHeader::new(VarType::Int, 8, 1, "SessionNum"),
            VarHeader::new(VarType::Float, 12, 1, "ThrottleRaw").and_then(|v| v.with_unit("%")),
            VarHeader::new(VarType::Float, 16, 4, "CarIdxLapDistPct"),
        ];
        let info_at = TABLE_AT + vars.len() * VarHeader::SIZE;
        let info_len = 64;
        let data_at = info_at + info_len;
        let mut bytes = vec![0u8; data_at + samples.len() * RECORD_LEN];

        let mut header = Header::default();
        header.ver = SDK_VERSION;
        header.status = STATUS_CONNECTED;
        header.tick_rate = 60;
        header.session_info_len = info_len as i32;
        header.session_info_offset = info_at as i32;
        header.num_vars = vars.len() as i32;
        header.var_header_offset = TABLE_AT as i32;
        header.num_buf = 1;
        header.buf_len = RECORD_LEN as i32;
        header.var_buf[0].buf_offset = data_at as i32;
        header.write(&mut bytes).expect("header fits");
        let disk = DiskSubHeader {
            session_record_count: claimed,
            session_lap_count: 2,
            ..DiskSubHeader::default()
        };
        disk.write(&mut bytes).expect("sub-header fits");
        for (n, var) in vars.into_iter().enumerate() {
            let at = TABLE_AT + n * VarHeader::SIZE;
            var.expect("valid").write(&mut bytes[at..]).expect("fits");
        }
        bytes[info_at..info_at + YAML.len()].copy_from_slice(YAML);
        for (n, &(num, time)) in samples.iter().enumerate() {
            let row = &mut bytes[data_at + n * RECORD_LEN..][..RECORD_LEN];
            row[0..8].copy_from_slice(&time.to_le_bytes());
            row[8..12].copy_from_slice(&num.to_le_bytes());
            row[12..16].copy_from_slice(&(n as f32 / 10.0).to_le_bytes());
            for car in 0..4 {
                row[16 + 4 * car..20 + 4 * car].copy_from_slice(&(car as f32 * 0.25).to_le_bytes());
            }
        }
        bytes.extend_from_slice(extra);
        bytes
    }

    fn samples() -> Vec<(i32, f64)> {
        // Session 0 with one two-tick step, then session 1 restarting its clock.
        let step = 1.0 / 60.0;
        let mut out: Vec<(i32, f64)> = (0..5).map(|n| (0, 10.0 + n as f64 * step)).collect();
        out.push((0, 10.0 + 6.0 * step));
        out.extend((0..4).map(|n| (1, n as f64 * step)));
        out
    }

    fn open(bytes: Vec<u8>) -> IbtReader<Cursor<Vec<u8>>> {
        IbtReader::new(Cursor::new(bytes)).expect("opens")
    }

    #[test]
    fn reads_metadata_and_session_info() {
        let reader = open(synthetic(&samples(), 10, &[]));
        assert_eq!(reader.record_count(), 10);
        assert_eq!(reader.record_len(), RECORD_LEN);
        assert_eq!(reader.vars().len(), 4);
        assert_eq!(reader.disk_header().session_lap_count, 2);
        assert_eq!(reader.header().tick_rate, 60);
        assert_eq!(reader.session_info(), YAML, "NUL padding trimmed");
    }

    #[test]
    fn iterates_records_in_order() {
        let mut reader = open(synthetic(&samples(), 10, &[]));
        let throttle = reader
            .field("ThrottleRaw")
            .expect("valid")
            .expect("present");
        let laps = reader
            .field("CarIdxLapDistPct")
            .expect("valid")
            .expect("present");
        assert_eq!(laps.count(), 4);
        assert_eq!(reader.field("Lat"), Ok(None));
        let mut record = reader.new_record();
        let mut seen = Vec::new();
        while reader.next_record(&mut record).expect("reads") {
            seen.push(throttle.f32(record.bytes(), 0).expect("float"));
        }
        assert_eq!(seen.len(), 10);
        assert_eq!(seen[3], 0.3);
        assert_eq!(laps.f32(record.bytes(), 3), Some(0.75));
        assert!(!reader.next_record(&mut record).expect("end"));
    }

    #[test]
    fn random_access_checks_index_and_buffer() {
        let mut reader = open(synthetic(&samples(), 10, &[]));
        let mut record = reader.new_record();
        reader.read_record(7, &mut record).expect("reads");
        assert_eq!(reader.position(), 8);
        assert!(matches!(
            reader.read_record(10, &mut record),
            Err(IbtError::RecordOutOfRange {
                index: 10,
                count: 10
            })
        ));
        let mut wrong = Record { bytes: vec![0; 8] };
        assert!(matches!(
            reader.read_record(0, &mut wrong),
            Err(IbtError::RecordSizeMismatch {
                expected: 32,
                actual: 8
            })
        ));
    }

    #[test]
    fn seeks_by_session_and_time() {
        let mut reader = open(synthetic(&samples(), 10, &[]));
        let step = 1.0 / 60.0;
        assert_eq!(reader.seek_session_time(0, 0.0).expect("seeks"), 0);
        assert_eq!(
            reader
                .seek_session_time(0, 10.0 + 2.0 * step)
                .expect("seeks"),
            2
        );
        // Inside the two-tick step: the first record at or after the target is the one after the gap.
        assert_eq!(
            reader
                .seek_session_time(0, 10.0 + 5.0 * step)
                .expect("seeks"),
            5
        );
        assert_eq!(reader.seek_session_time(1, 0.0).expect("seeks"), 6);
        assert_eq!(reader.seek_session_time(1, 2.5 * step).expect("seeks"), 9);
        assert_eq!(reader.seek_session_time(2, 0.0).expect("seeks"), 10);
        let mut record = reader.new_record();
        assert_eq!(reader.seek_session_time(1, step).expect("seeks"), 7);
        assert!(reader.next_record(&mut record).expect("reads"));
        let num = reader.field("SessionNum").expect("valid").expect("present");
        assert_eq!(num.i32(record.bytes(), 0), Some(1));
    }

    #[test]
    fn record_count_follows_sub_header() {
        // Zero means the recording was not closed: use the complete records in the file.
        assert_eq!(
            open(synthetic(&samples(), 0, &[1, 2, 3])).record_count(),
            10
        );
        // A smaller claim wins, and a trailing partial record is ignored.
        assert_eq!(open(synthetic(&samples(), 4, &[1, 2, 3])).record_count(), 4);
        let err = IbtReader::new(Cursor::new(synthetic(&samples(), 12, &[])))
            .expect_err("claims too many");
        assert!(matches!(
            err,
            IbtError::Truncated {
                expected: 12,
                available: 10
            }
        ));
    }

    #[test]
    fn empty_recording_has_no_records() {
        let mut reader = open(synthetic(&[], 0, &[]));
        assert_eq!(reader.record_count(), 0);
        let mut record = reader.new_record();
        assert!(!reader.next_record(&mut record).expect("end"));
        assert_eq!(reader.seek_session_time(0, 0.0).expect("seeks"), 0);
    }

    #[test]
    fn rejects_short_or_inconsistent_files() {
        let short = IbtReader::new(Cursor::new(vec![0u8; 100])).expect_err("too short");
        assert!(matches!(
            short,
            IbtError::Layout(LayoutError::Truncated {
                needed: 144,
                available: 100
            })
        ));
        let mut bad_version = synthetic(&samples(), 10, &[]);
        bad_version[0..4].copy_from_slice(&1i32.to_le_bytes());
        assert!(matches!(
            IbtReader::new(Cursor::new(bad_version)),
            Err(IbtError::Layout(LayoutError::UnsupportedVersion(1)))
        ));
        let mut cut = synthetic(&samples(), 0, &[]);
        cut.truncate(TABLE_AT + 100);
        assert!(matches!(
            IbtReader::new(Cursor::new(cut)),
            Err(IbtError::Layout(LayoutError::OutOfRange { .. }))
        ));
    }

    #[test]
    fn seek_requires_session_variables() {
        let mut bytes = synthetic(&samples(), 10, &[]);
        // Rename SessionNum so the file no longer has it.
        let name_at = TABLE_AT + VarHeader::SIZE + 16;
        bytes[name_at..name_at + 10].copy_from_slice(b"SessionNuX");
        let mut reader = open(bytes);
        assert!(matches!(
            reader.seek_session_time(0, 0.0),
            Err(IbtError::MissingVariable("SessionNum"))
        ));
    }
}
