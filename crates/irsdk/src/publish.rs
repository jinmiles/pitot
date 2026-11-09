//! Writer for a simulated SDK shared memory, for tests and the Tier A replayer.
//!
//! The publisher creates the mapping and an auto-reset data-valid event, and refuses to start when either name
//! is already in use, so it never writes into the memory of a running sim.

use core::ops::Range;

use crate::layout::{Header, LayoutError, MAX_BUFS, SDK_VERSION, STATUS_CONNECTED, VarHeader};
use crate::live::{LiveError, SESSION_INFO_UPDATE_OFFSET, STATUS_OFFSET, SdkNames, tick_offset};
use crate::shm::{Event, Mapping};

/// Layout of a published mapping.
#[derive(Debug, Clone, PartialEq)]
pub struct PublisherConfig {
    pub vars: Vec<VarHeader>,
    /// Length of one data buffer row.
    pub buf_len: usize,
    /// Number of rotating data buffers, `1..=MAX_BUFS`. The sim uses 3.
    pub num_buf: usize,
    pub tick_rate: i32,
    /// Bytes reserved for the session info text, including its NUL terminator.
    pub session_info_capacity: usize,
}

/// Publishes rows into a simulated shared memory and signals the data-valid event.
#[derive(Debug)]
pub struct Publisher {
    mapping: Mapping,
    event: Event,
    header: Header,
    var_capacity: usize,
    buf_capacity: usize,
    buf_offsets: [usize; MAX_BUFS],
    num_buf: usize,
    session_info: Range<usize>,
    next_buf: usize,
    next_tick: i32,
}

const fn align16(n: usize) -> usize {
    n.div_ceil(16) * 16
}

fn out_of_range(field: &'static str) -> LiveError {
    LayoutError::OutOfRange { field }.into()
}

fn to_i32(value: usize, field: &'static str) -> Result<i32, LiveError> {
    i32::try_from(value).map_err(|_| out_of_range(field))
}

impl Publisher {
    /// Creates the mapping and event with the header and variable table written and the status disconnected.
    pub fn create(names: &SdkNames, config: &PublisherConfig) -> Result<Self, LiveError> {
        if !(1..=MAX_BUFS).contains(&config.num_buf) {
            return Err(LayoutError::InvalidBufferCount(to_i32(config.num_buf, "numBuf")?).into());
        }
        if config.buf_len == 0 || config.session_info_capacity == 0 {
            return Err(out_of_range("bufLen"));
        }
        let table_end = Header::SIZE + config.vars.len() * VarHeader::SIZE;
        let info_start = align16(table_end);
        let info_end = info_start + config.session_info_capacity;
        let stride = align16(config.buf_len);
        let data_start = align16(info_end);
        let mut buf_offsets = [0; MAX_BUFS];
        for (n, offset) in buf_offsets.iter_mut().enumerate().take(config.num_buf) {
            *offset = data_start + n * stride;
        }
        let size = data_start + config.num_buf * stride;

        // Refuse both names before touching either, so a running sim is never written to.
        let mapping = Mapping::create(&names.mapping, size)?;
        let event = Event::create(&names.event)?;

        let mut header = Header::default();
        header.ver = SDK_VERSION;
        header.tick_rate = config.tick_rate;
        header.session_info_len = to_i32(config.session_info_capacity, "sessionInfoLen")?;
        header.session_info_offset = to_i32(info_start, "sessionInfoOffset")?;
        header.num_buf = to_i32(config.num_buf, "numBuf")?;
        for (buf, &offset) in header
            .var_buf
            .iter_mut()
            .zip(&buf_offsets)
            .take(config.num_buf)
        {
            buf.buf_offset = to_i32(offset, "varBuf.bufOffset")?;
        }
        let mut publisher = Self {
            mapping,
            event,
            header,
            var_capacity: config.vars.len(),
            buf_capacity: config.buf_len,
            buf_offsets,
            num_buf: config.num_buf,
            session_info: info_start..info_end,
            next_buf: 0,
            next_tick: 1,
        };
        publisher.set_layout(&config.vars, config.buf_len)?;
        Ok(publisher)
    }

    /// Rewrites the variable table and buffer length, as the sim does when the car changes. Must fit within
    /// the variable count and buffer length given at creation.
    pub fn set_layout(&mut self, vars: &[VarHeader], buf_len: usize) -> Result<(), LiveError> {
        if vars.len() > self.var_capacity {
            return Err(out_of_range("numVars"));
        }
        if buf_len == 0 || buf_len > self.buf_capacity {
            return Err(out_of_range("bufLen"));
        }
        let mut raw = [0u8; VarHeader::SIZE];
        for (n, var) in vars.iter().enumerate() {
            var.write(&mut raw)?;
            self.write(Header::SIZE + n * VarHeader::SIZE, &raw)?;
        }
        self.header.num_vars = to_i32(vars.len(), "numVars")?;
        self.header.var_header_offset = to_i32(Header::SIZE, "varHeaderOffset")?;
        self.header.buf_len = to_i32(buf_len, "bufLen")?;
        self.write_header()
    }

    /// Sets the connected status bit. Disconnecting also signals the event so waiting readers notice.
    pub fn set_connected(&mut self, connected: bool) -> Result<(), LiveError> {
        self.header.status = if connected { STATUS_CONNECTED } else { 0 };
        if !self.mapping.store_i32(STATUS_OFFSET, self.header.status) {
            return Err(out_of_range("status"));
        }
        if connected { Ok(()) } else { self.event.set() }
    }

    /// Replaces the session info text and increments `sessionInfoUpdate`, which it returns.
    pub fn set_session_info(&mut self, text: &[u8]) -> Result<i32, LiveError> {
        if text.len() >= self.session_info.len() {
            return Err(out_of_range("sessionInfoLen"));
        }
        let mut padded = vec![0u8; self.session_info.len()];
        padded[..text.len()].copy_from_slice(text);
        self.write(self.session_info.start, &padded)?;
        self.header.session_info_update = self.header.session_info_update.wrapping_add(1);
        if !self
            .mapping
            .store_i32(SESSION_INFO_UPDATE_OFFSET, self.header.session_info_update)
        {
            return Err(out_of_range("sessionInfoUpdate"));
        }
        Ok(self.header.session_info_update)
    }

    /// Writes `row` into the next buffer, publishes its tick count, and signals the event. Returns the tick.
    ///
    /// Does not allocate.
    pub fn publish(&mut self, row: &[u8]) -> Result<i32, LiveError> {
        let expected = usize::try_from(self.header.buf_len).unwrap_or(0);
        if row.len() != expected {
            return Err(LiveError::RowSizeMismatch {
                expected,
                actual: row.len(),
            });
        }
        let buf = self.next_buf;
        let offset = self
            .buf_offsets
            .get(buf)
            .copied()
            .ok_or_else(|| out_of_range("varBuf"))?;
        self.write(offset, row)?;
        let tick = self.next_tick;
        if !self.mapping.store_i32(tick_offset(buf), tick) {
            return Err(out_of_range("varBuf.tickCount"));
        }
        if let Some(entry) = self.header.var_buf.get_mut(buf) {
            entry.tick_count = tick;
        }
        self.event.set()?;
        self.next_buf = (buf + 1) % self.num_buf;
        self.next_tick = tick.wrapping_add(1);
        Ok(tick)
    }

    /// Tick count the next [`Publisher::publish`] uses, for example to simulate a counter reset.
    pub fn set_next_tick(&mut self, tick: i32) {
        self.next_tick = tick;
    }

    fn write_header(&mut self) -> Result<(), LiveError> {
        let mut raw = [0u8; Header::SIZE];
        self.header.write(&mut raw)?;
        self.write(0, &raw)
    }

    fn write(&self, offset: usize, data: &[u8]) -> Result<(), LiveError> {
        if self.mapping.write(offset, data) {
            Ok(())
        } else {
            Err(out_of_range("mapping"))
        }
    }
}
