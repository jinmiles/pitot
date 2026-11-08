//! Binary layout of the SDK shared memory and `.ibt` files, mirroring `irsdk_defines.h`.
//!
//! The sim writes little-endian x86-64 data. Every structure is decoded from and encoded to byte slices
//! with explicit little-endian reads, so this module needs no `unsafe` and works on any platform.

use core::fmt;
use core::mem::offset_of;
use core::ops::Range;
use core::slice::Iter;

/// Header version this module understands (`IRSDK_VER`).
pub const SDK_VERSION: i32 = 2;
/// Maximum number of data buffers in the header (`IRSDK_MAX_BUFS`).
pub const MAX_BUFS: usize = 4;
/// Size of the name and unit fields (`IRSDK_MAX_STRING`).
pub const MAX_STRING: usize = 32;
/// Size of the description field (`IRSDK_MAX_DESC`).
pub const MAX_DESC: usize = 64;
/// `irsdk_stConnected` bit in [`Header::status`].
pub const STATUS_CONNECTED: i32 = 1;

/// Errors from decoding, encoding, or validating SDK structures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// The byte slice is shorter than the structure or range requires.
    Truncated { needed: usize, available: usize },
    /// The header reports a version other than [`SDK_VERSION`].
    UnsupportedVersion(i32),
    /// `numBuf` is outside `1..=MAX_BUFS`.
    InvalidBufferCount(i32),
    /// A variable header carries an unknown type code.
    InvalidVarType(i32),
    /// A field is negative, overflows, or points outside the available bytes.
    OutOfRange { field: &'static str },
    /// A text field is not valid UTF-8.
    NonUtf8Text,
    /// A text value does not fit its field with a NUL terminator.
    TextTooLong { field: &'static str },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, available } => {
                write!(f, "need {needed} bytes, only {available} available")
            }
            Self::UnsupportedVersion(v) => write!(f, "unsupported SDK header version {v}"),
            Self::InvalidBufferCount(n) => write!(f, "invalid data buffer count {n}"),
            Self::InvalidVarType(t) => write!(f, "unknown variable type code {t}"),
            Self::OutOfRange { field } => write!(f, "`{field}` is out of range"),
            Self::NonUtf8Text => f.write_str("text field is not valid UTF-8"),
            Self::TextTooLong { field } => write!(f, "text does not fit `{field}`"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Variable data type (`irsdk_VarType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VarType {
    Char,
    Bool,
    Int,
    BitField,
    Float,
    Double,
}

impl VarType {
    /// Converts a raw type code from a variable header.
    pub const fn from_raw(raw: i32) -> Result<Self, LayoutError> {
        match raw {
            0 => Ok(Self::Char),
            1 => Ok(Self::Bool),
            2 => Ok(Self::Int),
            3 => Ok(Self::BitField),
            4 => Ok(Self::Float),
            5 => Ok(Self::Double),
            other => Err(LayoutError::InvalidVarType(other)),
        }
    }

    /// Raw type code as stored in a variable header.
    pub const fn raw(self) -> i32 {
        match self {
            Self::Char => 0,
            Self::Bool => 1,
            Self::Int => 2,
            Self::BitField => 3,
            Self::Float => 4,
            Self::Double => 5,
        }
    }

    /// Size in bytes of one element (`irsdk_VarTypeBytes`).
    pub const fn size(self) -> usize {
        match self {
            Self::Char | Self::Bool => 1,
            Self::Int | Self::BitField | Self::Float => 4,
            Self::Double => 8,
        }
    }
}

impl TryFrom<i32> for VarType {
    type Error = LayoutError;

    fn try_from(raw: i32) -> Result<Self, Self::Error> {
        Self::from_raw(raw)
    }
}

/// Data buffer descriptor (`irsdk_varBuf`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VarBuf {
    /// Tick of the data in this buffer; used to pick the newest buffer and detect torn reads.
    pub tick_count: i32,
    /// Offset of the buffer from the start of the mapping or file.
    pub buf_offset: i32,
    pad: [i32; 2],
}

/// Main SDK header at offset 0 of the mapping and of an `.ibt` file (`irsdk_header`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub ver: i32,
    /// Bit field of `STATUS_*` flags.
    pub status: i32,
    pub tick_rate: i32,
    /// Incremented by the sim whenever the session info text changes.
    pub session_info_update: i32,
    pub session_info_len: i32,
    pub session_info_offset: i32,
    pub num_vars: i32,
    pub var_header_offset: i32,
    pub num_buf: i32,
    /// Length of one data buffer (one row of variables).
    pub buf_len: i32,
    pad1: [i32; 2],
    pub var_buf: [VarBuf; MAX_BUFS],
}

/// Variable descriptor (`irsdk_varHeader`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarHeader {
    /// Raw type code; see [`VarHeader::var_type`].
    pub raw_type: i32,
    /// Offset of the value from the start of a data buffer.
    pub offset: i32,
    /// Number of elements; greater than one for arrays such as `CarIdx*`.
    pub count: i32,
    pub count_as_time: bool,
    pad: [u8; 3],
    name: [u8; MAX_STRING],
    desc: [u8; MAX_DESC],
    unit: [u8; MAX_STRING],
}

/// `.ibt` sub-header that follows [`Header`] in a telemetry file (`irsdk_diskSubHeader`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DiskSubHeader {
    /// Unix time (`time_t`, 64-bit on the sim's platform).
    pub session_start_date: i64,
    pub session_start_time: f64,
    pub session_end_time: f64,
    pub session_lap_count: i32,
    pub session_record_count: i32,
}

// Sizes and offsets from irsdk_defines.h. Encoding below relies on these.
const _: () = {
    assert!(size_of::<VarBuf>() == 16);
    assert!(offset_of!(VarBuf, buf_offset) == 4);
    assert!(size_of::<Header>() == 112);
    assert!(offset_of!(Header, session_info_update) == 12);
    assert!(offset_of!(Header, num_vars) == 24);
    assert!(offset_of!(Header, buf_len) == 36);
    assert!(offset_of!(Header, var_buf) == 48);
    assert!(size_of::<VarHeader>() == 144);
    assert!(offset_of!(VarHeader, count_as_time) == 12);
    assert!(offset_of!(VarHeader, name) == 16);
    assert!(offset_of!(VarHeader, desc) == 48);
    assert!(offset_of!(VarHeader, unit) == 112);
    assert!(size_of::<DiskSubHeader>() == 32);
    assert!(offset_of!(DiskSubHeader, session_lap_count) == 24);
    assert!(offset_of!(DiskSubHeader, session_record_count) == 28);
};

impl VarBuf {
    pub const SIZE: usize = size_of::<Self>();

    fn read_at(bytes: &[u8], at: usize) -> Result<Self, LayoutError> {
        Ok(Self {
            tick_count: read_i32(bytes, at + offset_of!(Self, tick_count))?,
            buf_offset: read_i32(bytes, at + offset_of!(Self, buf_offset))?,
            pad: [0; 2],
        })
    }

    fn write_at(&self, out: &mut [u8], at: usize) -> Result<(), LayoutError> {
        write_bytes(
            out,
            at + offset_of!(Self, tick_count),
            &self.tick_count.to_le_bytes(),
        )?;
        write_bytes(
            out,
            at + offset_of!(Self, buf_offset),
            &self.buf_offset.to_le_bytes(),
        )?;
        write_bytes(out, at + offset_of!(Self, pad), &[0; 8])
    }
}

impl Header {
    pub const SIZE: usize = size_of::<Self>();

    /// Decodes the header from the start of `bytes`. Does not validate field values.
    pub fn read(bytes: &[u8]) -> Result<Self, LayoutError> {
        check_len(bytes, Self::SIZE)?;
        let i = |field: usize| read_i32(bytes, field);
        let mut var_buf = [VarBuf::default(); MAX_BUFS];
        for (n, buf) in var_buf.iter_mut().enumerate() {
            *buf = VarBuf::read_at(bytes, offset_of!(Self, var_buf) + n * VarBuf::SIZE)?;
        }
        Ok(Self {
            ver: i(offset_of!(Self, ver))?,
            status: i(offset_of!(Self, status))?,
            tick_rate: i(offset_of!(Self, tick_rate))?,
            session_info_update: i(offset_of!(Self, session_info_update))?,
            session_info_len: i(offset_of!(Self, session_info_len))?,
            session_info_offset: i(offset_of!(Self, session_info_offset))?,
            num_vars: i(offset_of!(Self, num_vars))?,
            var_header_offset: i(offset_of!(Self, var_header_offset))?,
            num_buf: i(offset_of!(Self, num_buf))?,
            buf_len: i(offset_of!(Self, buf_len))?,
            pad1: [0; 2],
            var_buf,
        })
    }

    /// Encodes the header into the start of `out`. Padding is written as zeros.
    pub fn write(&self, out: &mut [u8]) -> Result<(), LayoutError> {
        check_len(out, Self::SIZE)?;
        let fields = [
            (offset_of!(Self, ver), self.ver),
            (offset_of!(Self, status), self.status),
            (offset_of!(Self, tick_rate), self.tick_rate),
            (
                offset_of!(Self, session_info_update),
                self.session_info_update,
            ),
            (offset_of!(Self, session_info_len), self.session_info_len),
            (
                offset_of!(Self, session_info_offset),
                self.session_info_offset,
            ),
            (offset_of!(Self, num_vars), self.num_vars),
            (offset_of!(Self, var_header_offset), self.var_header_offset),
            (offset_of!(Self, num_buf), self.num_buf),
            (offset_of!(Self, buf_len), self.buf_len),
        ];
        for (at, value) in fields {
            write_bytes(out, at, &value.to_le_bytes())?;
        }
        write_bytes(out, offset_of!(Self, pad1), &[0; 8])?;
        for (n, buf) in self.var_buf.iter().enumerate() {
            buf.write_at(out, offset_of!(Self, var_buf) + n * VarBuf::SIZE)?;
        }
        Ok(())
    }

    /// Whether the sim reports itself as connected.
    pub const fn is_connected(&self) -> bool {
        self.status & STATUS_CONNECTED != 0
    }

    /// Number of data buffers in use, checked against `1..=MAX_BUFS`.
    pub fn buffer_count(&self) -> Result<usize, LayoutError> {
        match usize::try_from(self.num_buf) {
            Ok(n @ 1..=MAX_BUFS) => Ok(n),
            _ => Err(LayoutError::InvalidBufferCount(self.num_buf)),
        }
    }

    /// Byte range of the variable header table, checked against `available` bytes.
    pub fn var_table_range(&self, available: usize) -> Result<Range<usize>, LayoutError> {
        let count = usize::try_from(self.num_vars).map_err(|_| out_of_range("numVars"))?;
        let len = count
            .checked_mul(VarHeader::SIZE)
            .ok_or(out_of_range("numVars"))?;
        checked_range(self.var_header_offset, len, "varHeaderOffset", available)
    }

    /// Byte range of the session info text area, checked against `available` bytes.
    pub fn session_info_range(&self, available: usize) -> Result<Range<usize>, LayoutError> {
        let len =
            usize::try_from(self.session_info_len).map_err(|_| out_of_range("sessionInfoLen"))?;
        checked_range(
            self.session_info_offset,
            len,
            "sessionInfoOffset",
            available,
        )
    }

    /// Byte range of data buffer `index`, checked against `available` bytes.
    pub fn buffer_range(
        &self,
        index: usize,
        available: usize,
    ) -> Result<Range<usize>, LayoutError> {
        if index >= self.buffer_count()? {
            return Err(out_of_range("varBuf"));
        }
        let len = match usize::try_from(self.buf_len) {
            Ok(len) if len > 0 => len,
            _ => return Err(out_of_range("bufLen")),
        };
        let buf = self.var_buf.get(index).ok_or(out_of_range("varBuf"))?;
        checked_range(buf.buf_offset, len, "varBuf.bufOffset", available)
    }

    /// Checks the version, tick rate, buffer count, and that the variable table, session info, and every
    /// data buffer in use lie within `available` bytes.
    ///
    /// Variable headers are not checked here: readers check only the variables they use, so an unknown
    /// type in a future sim build does not reject the whole connection.
    pub fn validate(&self, available: usize) -> Result<(), LayoutError> {
        if self.ver != SDK_VERSION {
            return Err(LayoutError::UnsupportedVersion(self.ver));
        }
        if self.tick_rate <= 0 {
            return Err(out_of_range("tickRate"));
        }
        self.var_table_range(available)?;
        self.session_info_range(available)?;
        for index in 0..self.buffer_count()? {
            self.buffer_range(index, available)?;
        }
        Ok(())
    }

    /// Iterates over the variable headers in `bytes`, which starts at the same origin as the header.
    pub fn var_headers<'a>(&self, bytes: &'a [u8]) -> Result<VarHeaders<'a>, LayoutError> {
        let range = self.var_table_range(bytes.len())?;
        let table = bytes.get(range).ok_or(out_of_range("varHeaderOffset"))?;
        // The range length is a multiple of the header size, so there is no remainder.
        let (table, _) = table.as_chunks::<{ VarHeader::SIZE }>();
        Ok(VarHeaders {
            table: table.iter(),
        })
    }
}

/// Iterator over the variable header table; see [`Header::var_headers`].
#[derive(Debug, Clone)]
pub struct VarHeaders<'a> {
    table: Iter<'a, [u8; VarHeader::SIZE]>,
}

impl Iterator for VarHeaders<'_> {
    type Item = Result<VarHeader, LayoutError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.table.next().map(|chunk| VarHeader::read(chunk))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.table.size_hint()
    }
}

impl ExactSizeIterator for VarHeaders<'_> {}

impl VarHeader {
    pub const SIZE: usize = size_of::<Self>();

    /// Creates a header with empty description and unit. `name` must leave room for a NUL terminator.
    pub fn new(
        var_type: VarType,
        offset: i32,
        count: i32,
        name: &str,
    ) -> Result<Self, LayoutError> {
        let mut header = Self {
            raw_type: var_type.raw(),
            offset,
            count,
            count_as_time: false,
            pad: [0; 3],
            name: [0; MAX_STRING],
            desc: [0; MAX_DESC],
            unit: [0; MAX_STRING],
        };
        fill_text(&mut header.name, name, "name")?;
        Ok(header)
    }

    /// Returns the header with `desc` set. It must leave room for a NUL terminator.
    pub fn with_desc(mut self, desc: &str) -> Result<Self, LayoutError> {
        fill_text(&mut self.desc, desc, "desc")?;
        Ok(self)
    }

    /// Returns the header with `unit` set. It must leave room for a NUL terminator.
    pub fn with_unit(mut self, unit: &str) -> Result<Self, LayoutError> {
        fill_text(&mut self.unit, unit, "unit")?;
        Ok(self)
    }

    /// Decodes a variable header from the start of `bytes`.
    pub fn read(bytes: &[u8]) -> Result<Self, LayoutError> {
        check_len(bytes, Self::SIZE)?;
        Ok(Self {
            raw_type: read_i32(bytes, offset_of!(Self, raw_type))?,
            offset: read_i32(bytes, offset_of!(Self, offset))?,
            count: read_i32(bytes, offset_of!(Self, count))?,
            count_as_time: read_array::<1>(bytes, offset_of!(Self, count_as_time))? != [0],
            pad: [0; 3],
            name: read_array(bytes, offset_of!(Self, name))?,
            desc: read_array(bytes, offset_of!(Self, desc))?,
            unit: read_array(bytes, offset_of!(Self, unit))?,
        })
    }

    /// Encodes the header into the start of `out`. Padding is written as zeros.
    pub fn write(&self, out: &mut [u8]) -> Result<(), LayoutError> {
        check_len(out, Self::SIZE)?;
        write_bytes(
            out,
            offset_of!(Self, raw_type),
            &self.raw_type.to_le_bytes(),
        )?;
        write_bytes(out, offset_of!(Self, offset), &self.offset.to_le_bytes())?;
        write_bytes(out, offset_of!(Self, count), &self.count.to_le_bytes())?;
        write_bytes(
            out,
            offset_of!(Self, count_as_time),
            &[u8::from(self.count_as_time)],
        )?;
        write_bytes(out, offset_of!(Self, pad), &[0; 3])?;
        write_bytes(out, offset_of!(Self, name), &self.name)?;
        write_bytes(out, offset_of!(Self, desc), &self.desc)?;
        write_bytes(out, offset_of!(Self, unit), &self.unit)
    }

    /// Decoded type, or an error for a type code this module does not know.
    pub const fn var_type(&self) -> Result<VarType, LayoutError> {
        VarType::from_raw(self.raw_type)
    }

    /// Name bytes up to the first NUL, or the whole field when it has none.
    pub fn name_bytes(&self) -> &[u8] {
        until_nul(&self.name)
    }

    pub fn name(&self) -> Result<&str, LayoutError> {
        as_text(self.name_bytes())
    }

    pub fn desc_bytes(&self) -> &[u8] {
        until_nul(&self.desc)
    }

    pub fn desc(&self) -> Result<&str, LayoutError> {
        as_text(self.desc_bytes())
    }

    pub fn unit_bytes(&self) -> &[u8] {
        until_nul(&self.unit)
    }

    pub fn unit(&self) -> Result<&str, LayoutError> {
        as_text(self.unit_bytes())
    }

    /// Byte range of the value within a data buffer of `buf_len` bytes.
    pub fn value_range(&self, buf_len: usize) -> Result<Range<usize>, LayoutError> {
        let count = match usize::try_from(self.count) {
            Ok(count) if count > 0 => count,
            _ => return Err(out_of_range("count")),
        };
        let len = count
            .checked_mul(self.var_type()?.size())
            .ok_or(out_of_range("count"))?;
        checked_range(self.offset, len, "offset", buf_len)
    }
}

impl DiskSubHeader {
    pub const SIZE: usize = size_of::<Self>();
    /// The sub-header follows the main header in an `.ibt` file.
    pub const OFFSET: usize = Header::SIZE;

    /// Decodes the sub-header from `bytes`, which starts at the beginning of the file.
    pub fn read(bytes: &[u8]) -> Result<Self, LayoutError> {
        check_len(bytes, Self::OFFSET + Self::SIZE)?;
        let at = |field: usize| Self::OFFSET + field;
        Ok(Self {
            session_start_date: i64::from_le_bytes(read_array(
                bytes,
                at(offset_of!(Self, session_start_date)),
            )?),
            session_start_time: f64::from_le_bytes(read_array(
                bytes,
                at(offset_of!(Self, session_start_time)),
            )?),
            session_end_time: f64::from_le_bytes(read_array(
                bytes,
                at(offset_of!(Self, session_end_time)),
            )?),
            session_lap_count: read_i32(bytes, at(offset_of!(Self, session_lap_count)))?,
            session_record_count: read_i32(bytes, at(offset_of!(Self, session_record_count)))?,
        })
    }

    /// Encodes the sub-header into `out`, which starts at the beginning of the file.
    pub fn write(&self, out: &mut [u8]) -> Result<(), LayoutError> {
        check_len(out, Self::OFFSET + Self::SIZE)?;
        let at = |field: usize| Self::OFFSET + field;
        write_bytes(
            out,
            at(offset_of!(Self, session_start_date)),
            &self.session_start_date.to_le_bytes(),
        )?;
        write_bytes(
            out,
            at(offset_of!(Self, session_start_time)),
            &self.session_start_time.to_le_bytes(),
        )?;
        write_bytes(
            out,
            at(offset_of!(Self, session_end_time)),
            &self.session_end_time.to_le_bytes(),
        )?;
        write_bytes(
            out,
            at(offset_of!(Self, session_lap_count)),
            &self.session_lap_count.to_le_bytes(),
        )?;
        write_bytes(
            out,
            at(offset_of!(Self, session_record_count)),
            &self.session_record_count.to_le_bytes(),
        )
    }
}

const fn out_of_range(field: &'static str) -> LayoutError {
    LayoutError::OutOfRange { field }
}

fn check_len(bytes: &[u8], needed: usize) -> Result<(), LayoutError> {
    if bytes.len() < needed {
        return Err(LayoutError::Truncated {
            needed,
            available: bytes.len(),
        });
    }
    Ok(())
}

fn checked_range(
    offset: i32,
    len: usize,
    field: &'static str,
    available: usize,
) -> Result<Range<usize>, LayoutError> {
    let start = usize::try_from(offset).map_err(|_| out_of_range(field))?;
    let end = start.checked_add(len).ok_or(out_of_range(field))?;
    if end > available {
        return Err(out_of_range(field));
    }
    Ok(start..end)
}

fn read_array<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], LayoutError> {
    let truncated = LayoutError::Truncated {
        needed: at.saturating_add(N),
        available: bytes.len(),
    };
    let end = at.checked_add(N).ok_or(truncated)?;
    bytes
        .get(at..end)
        .and_then(|s| s.try_into().ok())
        .ok_or(truncated)
}

fn read_i32(bytes: &[u8], at: usize) -> Result<i32, LayoutError> {
    read_array(bytes, at).map(i32::from_le_bytes)
}

fn write_bytes(out: &mut [u8], at: usize, value: &[u8]) -> Result<(), LayoutError> {
    let available = out.len();
    let truncated = LayoutError::Truncated {
        needed: at.saturating_add(value.len()),
        available,
    };
    let end = at.checked_add(value.len()).ok_or(truncated)?;
    out.get_mut(at..end)
        .ok_or(truncated)?
        .copy_from_slice(value);
    Ok(())
}

fn until_nul(field: &[u8]) -> &[u8] {
    field.split(|&b| b == 0).next().unwrap_or(field)
}

fn as_text(bytes: &[u8]) -> Result<&str, LayoutError> {
    core::str::from_utf8(bytes).map_err(|_| LayoutError::NonUtf8Text)
}

fn fill_text(field: &mut [u8], text: &str, name: &'static str) -> Result<(), LayoutError> {
    let bytes = text.as_bytes();
    // Keep at least one NUL so C readers see a terminated string.
    if bytes.len() >= field.len() {
        return Err(LayoutError::TextTooLong { field: name });
    }
    field.fill(0);
    field
        .get_mut(..bytes.len())
        .ok_or(LayoutError::TextTooLong { field: name })?
        .copy_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> Header {
        let mut var_buf = [VarBuf::default(); MAX_BUFS];
        for (n, buf) in (0i32..).zip(var_buf.iter_mut().take(3)) {
            buf.tick_count = 100 + n;
            buf.buf_offset = 464 + 32 * n;
        }
        Header {
            ver: SDK_VERSION,
            status: STATUS_CONNECTED,
            tick_rate: 60,
            session_info_update: 7,
            session_info_len: 64,
            session_info_offset: 400,
            num_vars: 2,
            var_header_offset: 112,
            num_buf: 3,
            buf_len: 32,
            var_buf,
            ..Header::default()
        }
    }

    /// Header (112) + two variables (288) + session info (64) + three 32-byte buffers = 560 bytes.
    fn sample_bytes() -> Vec<u8> {
        let mut bytes = vec![0; 560];
        sample_header().write(&mut bytes).expect("header fits");
        let speed = VarHeader::new(VarType::Float, 0, 1, "Speed").and_then(|v| v.with_unit("m/s"));
        let laps = VarHeader::new(VarType::Double, 8, 3, "CarIdxLapDistPct")
            .and_then(|v| v.with_desc("Lap"));
        for (n, var) in [speed, laps].into_iter().enumerate() {
            let at = 112 + n * VarHeader::SIZE;
            var.expect("valid variable")
                .write(&mut bytes[at..])
                .expect("variable fits");
        }
        bytes
    }

    #[test]
    fn header_round_trips_at_sdk_offsets() {
        let bytes = sample_bytes();
        assert_eq!(Header::read(&bytes), Ok(sample_header()));
        assert_eq!(bytes[24..28], 2i32.to_le_bytes(), "numVars at 24");
        assert_eq!(bytes[36..40], 32i32.to_le_bytes(), "bufLen at 36");
        assert_eq!(
            bytes[48..52],
            100i32.to_le_bytes(),
            "varBuf[0].tickCount at 48"
        );
        assert_eq!(
            bytes[68..72],
            496i32.to_le_bytes(),
            "varBuf[1].bufOffset at 68"
        );
    }

    #[test]
    fn short_input_is_truncated() {
        let bytes = sample_bytes();
        let truncated = |needed, available| LayoutError::Truncated { needed, available };
        assert_eq!(Header::read(&bytes[..111]), Err(truncated(112, 111)));
        assert_eq!(VarHeader::read(&bytes[112..200]), Err(truncated(144, 88)));
        assert_eq!(DiskSubHeader::read(&bytes[..143]), Err(truncated(144, 143)));
        assert_eq!(
            sample_header().write(&mut [0u8; 100]),
            Err(truncated(112, 100))
        );
    }

    #[test]
    fn validate_accepts_consistent_layout() {
        let len = sample_bytes().len();
        let header = sample_header();
        assert_eq!(header.validate(len), Ok(()));
        assert!(header.is_connected());
        assert_eq!(header.var_table_range(len), Ok(112..400));
        assert_eq!(header.session_info_range(len), Ok(400..464));
        assert_eq!(header.buffer_range(2, len), Ok(528..560));
    }

    #[test]
    fn validate_rejects_bad_fields() {
        let len = sample_bytes().len();
        let check = |edit: fn(&mut Header), expected: LayoutError| {
            let mut header = sample_header();
            edit(&mut header);
            assert_eq!(header.validate(len), Err(expected));
        };
        check(|h| h.ver = 1, LayoutError::UnsupportedVersion(1));
        check(|h| h.num_buf = 0, LayoutError::InvalidBufferCount(0));
        check(|h| h.num_buf = 5, LayoutError::InvalidBufferCount(5));
        check(|h| h.tick_rate = 0, out_of_range("tickRate"));
        check(|h| h.num_vars = -1, out_of_range("numVars"));
        check(|h| h.num_vars = 4, out_of_range("varHeaderOffset"));
        check(
            |h| h.var_header_offset = -4,
            out_of_range("varHeaderOffset"),
        );
        check(
            |h| h.session_info_len = 200,
            out_of_range("sessionInfoOffset"),
        );
        check(|h| h.buf_len = 0, out_of_range("bufLen"));
        check(
            |h| h.var_buf[2].buf_offset = 540,
            out_of_range("varBuf.bufOffset"),
        );
        check(
            |h| h.var_buf[1].buf_offset = i32::MAX,
            out_of_range("varBuf.bufOffset"),
        );
    }

    #[test]
    fn buffer_range_rejects_unused_index() {
        assert_eq!(
            sample_header().buffer_range(3, 560),
            Err(out_of_range("varBuf"))
        );
    }

    #[test]
    fn var_headers_decode_in_order() {
        let bytes = sample_bytes();
        let header = sample_header();
        let vars: Vec<VarHeader> = header
            .var_headers(&bytes)
            .expect("table in range")
            .collect::<Result<_, _>>()
            .expect("headers decode");
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name(), Ok("Speed"));
        assert_eq!(vars[0].unit(), Ok("m/s"));
        assert_eq!(vars[0].var_type(), Ok(VarType::Float));
        assert_eq!(vars[1].name(), Ok("CarIdxLapDistPct"));
        assert_eq!(vars[1].desc(), Ok("Lap"));
        assert_eq!(vars[1].count, 3);
        assert_eq!(
            &bytes[112 + 144 + 16..112 + 144 + 32],
            b"CarIdxLapDistPct",
            "name at 16"
        );
        assert_eq!(
            header.var_headers(&bytes[..300]).map(|it| it.len()),
            Err(out_of_range("varHeaderOffset"))
        );
    }

    #[test]
    fn var_header_round_trips() {
        let mut var = VarHeader::new(VarType::BitField, 12, 1, "SessionFlags")
            .and_then(|v| v.with_unit("irsdk_Flags"))
            .expect("valid header");
        var.count_as_time = true;
        let mut bytes = [0xAAu8; VarHeader::SIZE];
        var.write(&mut bytes).expect("fits");
        assert_eq!(
            bytes[12..16],
            [1, 0, 0, 0],
            "countAsTime and zeroed padding"
        );
        assert_eq!(VarHeader::read(&bytes), Ok(var));
    }

    #[test]
    fn text_fields_without_nul_use_whole_field() {
        let mut bytes = [0u8; VarHeader::SIZE];
        bytes[16..48].fill(b'x');
        let var = VarHeader::read(&bytes).expect("decodes");
        assert_eq!(var.name_bytes().len(), MAX_STRING);
        bytes[16] = 0xFF;
        let var = VarHeader::read(&bytes).expect("decodes");
        assert_eq!(var.name(), Err(LayoutError::NonUtf8Text));
    }

    #[test]
    fn text_must_leave_room_for_nul() {
        let long = "x".repeat(MAX_STRING);
        assert_eq!(
            VarHeader::new(VarType::Int, 0, 1, &long),
            Err(LayoutError::TextTooLong { field: "name" })
        );
        assert!(VarHeader::new(VarType::Int, 0, 1, &long[..MAX_STRING - 1]).is_ok());
        let desc = "d".repeat(MAX_DESC);
        let result = VarHeader::new(VarType::Int, 0, 1, "Gear").and_then(|v| v.with_desc(&desc));
        assert_eq!(result, Err(LayoutError::TextTooLong { field: "desc" }));
    }

    #[test]
    fn var_types_map_codes_and_sizes() {
        let expected = [
            (0, VarType::Char, 1),
            (1, VarType::Bool, 1),
            (2, VarType::Int, 4),
            (3, VarType::BitField, 4),
            (4, VarType::Float, 4),
            (5, VarType::Double, 8),
        ];
        for (raw, ty, size) in expected {
            assert_eq!(VarType::from_raw(raw), Ok(ty));
            assert_eq!(ty.raw(), raw);
            assert_eq!(ty.size(), size);
        }
        assert_eq!(VarType::try_from(6), Err(LayoutError::InvalidVarType(6)));
        assert_eq!(VarType::from_raw(-1), Err(LayoutError::InvalidVarType(-1)));
    }

    #[test]
    fn value_range_checks_buffer_bounds() {
        let var = |ty, offset, count| VarHeader::new(ty, offset, count, "V").expect("valid header");
        assert_eq!(var(VarType::Double, 8, 3).value_range(32), Ok(8..32));
        assert_eq!(
            var(VarType::Double, 8, 3).value_range(31),
            Err(out_of_range("offset"))
        );
        assert_eq!(
            var(VarType::Float, 0, 0).value_range(32),
            Err(out_of_range("count"))
        );
        assert_eq!(
            var(VarType::Float, -4, 1).value_range(32),
            Err(out_of_range("offset"))
        );
        let mut unknown = var(VarType::Int, 0, 1);
        unknown.raw_type = 9;
        assert_eq!(unknown.value_range(32), Err(LayoutError::InvalidVarType(9)));
    }

    #[test]
    fn disk_sub_header_round_trips_after_header() {
        let sub = DiskSubHeader {
            session_start_date: 1_762_253_821,
            session_start_time: 9.25,
            session_end_time: 1645.5,
            session_lap_count: 12,
            session_record_count: 98_144,
        };
        let mut bytes = [0u8; 144];
        sub.write(&mut bytes).expect("fits");
        assert_eq!(
            bytes[112..120],
            1_762_253_821i64.to_le_bytes(),
            "sessionStartDate at 112"
        );
        assert_eq!(
            bytes[140..144],
            98_144i32.to_le_bytes(),
            "sessionRecordCount at 140"
        );
        assert_eq!(DiskSubHeader::read(&bytes), Ok(sub));
    }
}
