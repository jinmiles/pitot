//! Live telemetry reader over the sim's shared memory.
//!
//! Call [`LiveReader::step`] in a loop on a dedicated data thread. It connects when the sim appears (trying at
//! most once per `retry_interval`), resolves the requested variables by name once per connection, and copies
//! only those variables into a preallocated [`Snapshot`]. After warm-up a step does not allocate.
//!
//! While ticks arrive, the reader waits on the data-valid event with a timeout of two tick periods, so a
//! missed signal loses no data: ticks still held in the sim's rotating buffers are returned in tick order.
//! Without ticks for `idle_timeout`, each step waits `idle_timeout` instead.

use core::mem::offset_of;
use core::ops::Range;
use core::sync::atomic::{Ordering, fence};
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::HSTRING;

use crate::layout::{Field, Header, LayoutError, MAX_BUFS, VarBuf, VarHeader};
use crate::shm::{Event, Mapping};

/// Shared-memory mapping name used by the sim.
pub const SIM_MAPPING_NAME: &str = "Local\\IRSDKMemMapFileName";
/// Data-valid event name used by the sim.
pub const SIM_EVENT_NAME: &str = "Local\\IRSDKDataValidEvent";

pub(crate) const STATUS_OFFSET: usize = offset_of!(Header, status);
pub(crate) const SESSION_INFO_UPDATE_OFFSET: usize = offset_of!(Header, session_info_update);

/// Offset of `varBuf[buf].tickCount` from the start of the mapping.
pub(crate) const fn tick_offset(buf: usize) -> usize {
    offset_of!(Header, var_buf) + buf * VarBuf::SIZE + offset_of!(VarBuf, tick_count)
}

/// Errors from the live reader and the publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveError {
    /// A Win32 call failed with this HRESULT.
    Os { call: &'static str, code: i32 },
    /// The mapping or event name is already in use.
    AlreadyExists,
    /// The mapped header or variable table is inconsistent.
    Layout(LayoutError),
    /// A published row does not match the buffer length.
    RowSizeMismatch { expected: usize, actual: usize },
}

impl fmt::Display for LiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os { call, code } => write!(f, "{call} failed with HRESULT {code:#010x}"),
            Self::AlreadyExists => f.write_str("shared-memory or event name is already in use"),
            Self::Layout(e) => write!(f, "invalid shared-memory layout: {e}"),
            Self::RowSizeMismatch { expected, actual } => {
                write!(f, "row is {actual} bytes, expected {expected}")
            }
        }
    }
}

impl std::error::Error for LiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Layout(e) => Some(e),
            _ => None,
        }
    }
}

impl From<LayoutError> for LiveError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// Names of the shared-memory mapping and data-valid event.
#[derive(Debug, Clone)]
pub struct SdkNames {
    pub(crate) mapping: HSTRING,
    pub(crate) event: HSTRING,
}

impl SdkNames {
    /// The names the sim uses.
    pub fn sim() -> Self {
        Self::new(SIM_MAPPING_NAME, SIM_EVENT_NAME)
    }

    /// Custom names, for tests and isolated replays.
    pub fn new(mapping: &str, event: &str) -> Self {
        Self {
            mapping: HSTRING::from(mapping),
            event: HSTRING::from(event),
        }
    }
}

/// Handle to a variable requested through [`VarSet::add`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarId(usize);

/// The variables a reader copies each tick.
#[derive(Debug, Clone, Default)]
pub struct VarSet {
    names: Vec<String>,
    whole_row: bool,
}

impl VarSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every byte of each data buffer row, as the sim wrote it, for recorders. The snapshot then has no named
    /// variables; [`Snapshot::bytes`] is the row and [`LiveReader::layout`] describes it.
    pub fn whole_row() -> Self {
        Self {
            names: Vec::new(),
            whole_row: true,
        }
    }

    pub fn add(&mut self, name: &str) -> VarId {
        self.names.push(name.to_owned());
        VarId(self.names.len() - 1)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// The requested variables of the latest tick, stored back to back.
///
/// Sized when the reader connects. Variables the sim does not provide, or provides with an unknown type,
/// are unavailable and read as `None`.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    bytes: Vec<u8>,
    fields: Vec<Option<Field>>,
    tick: i32,
    buffer: usize,
}

impl Snapshot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sim tick count of the copied data.
    pub fn tick(&self) -> i32 {
        self.tick
    }

    /// Index of the sim's rotating buffer the latest tick was copied from.
    pub fn buffer(&self) -> usize {
        self.buffer
    }

    /// The copied variables, back to back. With [`Snapshot::bytes_mut`] and [`Snapshot::set_tick`], this lets
    /// a clone of the snapshot on another thread receive ticks without the reader.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    pub fn set_tick(&mut self, tick: i32) {
        self.tick = tick;
    }

    pub fn available(&self, var: VarId) -> bool {
        self.field(var).is_some()
    }

    /// Number of elements, for example the per-car array length; zero when unavailable.
    pub fn count(&self, var: VarId) -> usize {
        self.field(var).map_or(0, Field::count)
    }

    pub fn f32(&self, var: VarId, index: usize) -> Option<f32> {
        self.field(var)?.f32(&self.bytes, index)
    }

    pub fn f64(&self, var: VarId, index: usize) -> Option<f64> {
        self.field(var)?.f64(&self.bytes, index)
    }

    pub fn i32(&self, var: VarId, index: usize) -> Option<i32> {
        self.field(var)?.i32(&self.bytes, index)
    }

    pub fn bitfield(&self, var: VarId, index: usize) -> Option<u32> {
        self.field(var)?.bitfield(&self.bytes, index)
    }

    pub fn bool(&self, var: VarId, index: usize) -> Option<bool> {
        self.field(var)?.bool(&self.bytes, index)
    }

    fn field(&self, var: VarId) -> Option<&Field> {
        self.fields.get(var.0)?.as_ref()
    }
}

/// Copies the requested variables out of rows laid out by another variable table, such as a capture's, into a
/// snapshot laid out by a live connection, so the same [`VarId`]s read both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowMap {
    spans: Vec<Span>,
}

impl RowMap {
    /// Maps rows of `row_len` bytes described by `vars` into the layout of `snapshot`, which a reader of `set`
    /// filled. A variable missing from `vars`, or there with another type, reads as zero; of an array, the
    /// elements both have are copied.
    pub fn new(set: &VarSet, snapshot: &Snapshot, vars: &[VarHeader], row_len: usize) -> Self {
        let spans = set
            .names
            .iter()
            .zip(&snapshot.fields)
            .filter_map(|(name, dst)| {
                let dst = dst.as_ref()?;
                let src = vars
                    .iter()
                    .find(|var| var.name_bytes() == name.as_bytes())
                    .and_then(|var| Field::new(var, row_len).ok())
                    .filter(|src| src.var_type() == dst.var_type())?;
                Some(Span {
                    src: src.offset(),
                    dst: dst.offset(),
                    len: src.byte_len().min(dst.byte_len()),
                })
            })
            .collect();
        Self { spans }
    }

    /// Fills `snapshot` from `row` and sets its tick count. Variables without a source in the row are zero.
    pub fn copy(&self, row: &[u8], tick: i32, snapshot: &mut Snapshot) {
        snapshot.bytes.fill(0);
        for span in &self.spans {
            if let (Some(src), Some(dst)) = (
                row.get(span.src..span.src + span.len),
                snapshot.bytes.get_mut(span.dst..span.dst + span.len),
            ) {
                dst.copy_from_slice(src);
            }
        }
        snapshot.tick = tick;
    }
}

/// Timing policy of a [`LiveReader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderConfig {
    /// Minimum time between connection attempts while the sim is not running.
    pub retry_interval: Duration,
    /// Wait timeout when no tick arrived recently, and how long "recently" is.
    pub idle_timeout: Duration,
}

impl Default for ReaderConfig {
    fn default() -> Self {
        Self {
            retry_interval: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(1),
        }
    }
}

/// Result of one [`LiveReader::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A new tick was copied into the snapshot. `missed` counts ticks overwritten before they could be read.
    Tick { missed: u32 },
    /// A buffer was overwritten during both copy attempts; the snapshot holds partial data and must be ignored.
    Torn,
    /// No new tick within the wait timeout.
    Idle,
    /// Connected, or the layout changed: variables were resolved again and the snapshot was resized.
    Connected,
    /// The sim stopped reporting a connection; the reader released the mapping.
    Disconnected,
    /// The sim is not running or not ready.
    NotRunning,
}

/// Reads the requested variables tick by tick; see the module documentation.
#[derive(Debug)]
pub struct LiveReader {
    names: SdkNames,
    vars: VarSet,
    config: ReaderConfig,
    conn: Option<Live>,
    last_attempt: Option<Instant>,
}

impl LiveReader {
    pub fn new(names: SdkNames, vars: VarSet, config: ReaderConfig) -> Self {
        Self {
            names,
            vars,
            config,
            conn: None,
            last_attempt: None,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    /// The variables this reader copies.
    pub fn var_set(&self) -> &VarSet {
        &self.vars
    }

    /// Advances the reader by one event: a connection change, a tick, or a timeout.
    ///
    /// Blocks for at most `retry_interval` while disconnected and `idle_timeout` while connected. Errors leave
    /// the reader disconnected, and the next step tries to connect again.
    pub fn step(&mut self, snapshot: &mut Snapshot) -> Result<Step, LiveError> {
        let Self {
            conn, vars, config, ..
        } = self;
        let Some(live) = conn.as_mut() else {
            return self.connect(snapshot);
        };
        let result = live.step(vars, config, snapshot);
        if matches!(result, Ok(Step::Disconnected) | Err(_)) {
            *conn = None;
        }
        result
    }

    /// Current `sessionInfoUpdate`, or `None` while disconnected.
    pub fn session_info_update(&self) -> Option<i32> {
        self.conn
            .as_ref()?
            .mapping
            .load_i32(SESSION_INFO_UPDATE_OFFSET)
    }

    /// The connection's layout: variable headers, row length, tick rate, session info area size, and SDK
    /// version. `None` while disconnected.
    pub fn layout(&self) -> Option<&LiveLayout> {
        Some(&self.conn.as_ref()?.plan.layout)
    }

    /// Size of the session info area. Reserve this much in the buffer passed to
    /// [`LiveReader::copy_session_info`] so the copy does not allocate.
    pub fn session_info_capacity(&self) -> Option<usize> {
        Some(self.conn.as_ref()?.plan.session_info.len())
    }

    /// Copies the session info text without its NUL padding into `out` and returns the `sessionInfoUpdate` it
    /// belongs to. `Ok(None)` while disconnected or when the text changed during both copy attempts.
    pub fn copy_session_info(&self, out: &mut Vec<u8>) -> Result<Option<i32>, LiveError> {
        let Some(live) = self.conn.as_ref() else {
            return Ok(None);
        };
        let range = live.plan.session_info.clone();
        for _ in 0..2 {
            let Some(before) = live.mapping.load_i32(SESSION_INFO_UPDATE_OFFSET) else {
                return Ok(None);
            };
            out.clear();
            out.resize(range.len(), 0);
            if !live.mapping.read(range.start, out) {
                return Err(LayoutError::OutOfRange {
                    field: "sessionInfoOffset",
                }
                .into());
            }
            fence(Ordering::Acquire);
            if live.mapping.load_i32(SESSION_INFO_UPDATE_OFFSET) == Some(before) {
                if let Some(end) = out.iter().position(|&b| b == 0) {
                    out.truncate(end);
                }
                return Ok(Some(before));
            }
        }
        Ok(None)
    }

    fn connect(&mut self, snapshot: &mut Snapshot) -> Result<Step, LiveError> {
        if let Some(at) = self.last_attempt {
            let elapsed = at.elapsed();
            if elapsed < self.config.retry_interval {
                thread::sleep(self.config.retry_interval - elapsed);
            }
        }
        self.last_attempt = Some(Instant::now());
        let Some(mapping) = Mapping::open_read(&self.names.mapping)? else {
            return Ok(Step::NotRunning);
        };
        let Some(event) = Event::open(&self.names.event)? else {
            return Ok(Step::NotRunning);
        };
        let header = read_header(&mapping)?;
        if !header.is_connected() {
            return Ok(Step::NotRunning);
        }
        let plan = Plan::resolve(&mapping, &header, &self.vars, snapshot)?;
        // Data already in the buffers is not reported as new; the first tick is the next one the sim writes.
        let last_tick = pick(&mapping, &plan, None).map(|(_, tick)| tick);
        self.conn = Some(Live {
            mapping,
            event,
            plan,
            last_tick,
            last_tick_at: None,
            empty_wake: false,
        });
        Ok(Step::Connected)
    }
}

/// State of one connection.
#[derive(Debug)]
struct Live {
    mapping: Mapping,
    event: Event,
    plan: Plan,
    last_tick: Option<i32>,
    last_tick_at: Option<Instant>,
    /// The last wait was signaled but found no data; sleep one tick before waiting again so an event that
    /// stays signaled cannot cause a busy loop.
    empty_wake: bool,
}

impl Live {
    fn step(
        &mut self,
        vars: &VarSet,
        config: &ReaderConfig,
        snapshot: &mut Snapshot,
    ) -> Result<Step, LiveError> {
        if let Some(step) = self.take_tick(snapshot) {
            return Ok(step);
        }
        let header = read_header(&self.mapping)?;
        if !header.is_connected() {
            return Ok(Step::Disconnected);
        }
        if LayoutKey::of(&header) != self.plan.key {
            self.plan = Plan::resolve(&self.mapping, &header, vars, snapshot)?;
            self.last_tick = pick(&self.mapping, &self.plan, None).map(|(_, tick)| tick);
            return Ok(Step::Connected);
        }
        if self.empty_wake {
            self.empty_wake = false;
            thread::sleep(self.plan.tick_period);
        }
        let timeout = match self.last_tick_at {
            Some(at) if at.elapsed() < config.idle_timeout => {
                (self.plan.tick_period * 2).min(config.idle_timeout)
            }
            _ => config.idle_timeout,
        };
        let signaled = self.event.wait(timeout)?;
        if let Some(step) = self.take_tick(snapshot) {
            return Ok(step);
        }
        self.empty_wake = signaled;
        Ok(Step::Idle)
    }

    fn take_tick(&mut self, snapshot: &mut Snapshot) -> Option<Step> {
        match read_next(
            &self.mapping,
            &self.plan,
            self.last_tick,
            &mut snapshot.bytes,
        ) {
            ReadOutcome::New {
                tick,
                missed,
                buffer,
            } => {
                self.last_tick = Some(tick);
                self.last_tick_at = Some(Instant::now());
                snapshot.tick = tick;
                snapshot.buffer = buffer;
                Some(Step::Tick { missed })
            }
            ReadOutcome::Torn => Some(Step::Torn),
            ReadOutcome::Empty => None,
        }
    }
}

fn read_header(mapping: &Mapping) -> Result<Header, LiveError> {
    let mut raw = [0u8; Header::SIZE];
    if !mapping.read(0, &mut raw) {
        return Err(LayoutError::Truncated {
            needed: Header::SIZE,
            available: mapping.len(),
        }
        .into());
    }
    Ok(Header::read(&raw)?)
}

/// Header fields that, when changed, require resolving the variables again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayoutKey {
    num_vars: i32,
    var_header_offset: i32,
    num_buf: i32,
    buf_len: i32,
    buf_offsets: [i32; MAX_BUFS],
    session_info: (i32, i32),
}

impl LayoutKey {
    fn of(header: &Header) -> Self {
        Self {
            num_vars: header.num_vars,
            var_header_offset: header.var_header_offset,
            num_buf: header.num_buf,
            buf_len: header.buf_len,
            buf_offsets: header.var_buf.map(|b| b.buf_offset),
            session_info: (header.session_info_offset, header.session_info_len),
        }
    }
}

/// A requested variable copied from `src` within a sim buffer to `dst` within the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    src: usize,
    dst: usize,
    len: usize,
}

/// What a connection publishes, as recorders need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLayout {
    pub vars: Vec<VarHeader>,
    /// Bytes per data buffer row.
    pub buf_len: usize,
    pub tick_rate: i32,
    /// Size of the session info area.
    pub session_info_capacity: usize,
    pub sdk_version: i32,
}

/// Per-connection copy plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    layout: LiveLayout,
    spans: Vec<Span>,
    buf_offsets: [usize; MAX_BUFS],
    num_buf: usize,
    key: LayoutKey,
    session_info: Range<usize>,
    tick_period: Duration,
}

impl Plan {
    fn resolve(
        mapping: &Mapping,
        header: &Header,
        vars: &VarSet,
        snapshot: &mut Snapshot,
    ) -> Result<Self, LiveError> {
        let available = mapping.len();
        header.validate(available)?;
        let table_range = header.var_table_range(available)?;
        let mut table = vec![0u8; table_range.len()];
        if !mapping.read(table_range.start, &mut table) {
            return Err(LayoutError::OutOfRange {
                field: "varHeaderOffset",
            }
            .into());
        }
        let headers = table
            .as_chunks::<{ VarHeader::SIZE }>()
            .0
            .iter()
            .map(|chunk| VarHeader::read(chunk))
            .collect::<Result<Vec<_>, _>>()?;
        let buf_len = usize::try_from(header.buf_len)
            .map_err(|_| LayoutError::OutOfRange { field: "bufLen" })?;

        snapshot.fields.clear();
        let mut spans = Vec::with_capacity(vars.len().max(1));
        let mut total = 0;
        if vars.whole_row {
            spans.push(Span {
                src: 0,
                dst: 0,
                len: buf_len,
            });
            total = buf_len;
        }
        for name in &vars.names {
            // A variable with an unknown type or a bad range is unavailable rather than fatal.
            let field = headers
                .iter()
                .find(|var| var.name_bytes() == name.as_bytes())
                .and_then(|var| Field::new(var, buf_len).ok());
            match field {
                Some(field) => {
                    spans.push(Span {
                        src: field.offset(),
                        dst: total,
                        len: field.byte_len(),
                    });
                    snapshot.fields.push(Some(field.with_offset(total)));
                    total += field.byte_len();
                }
                None => snapshot.fields.push(None),
            }
        }
        snapshot.bytes.clear();
        snapshot.bytes.resize(total, 0);
        snapshot.tick = 0;

        let num_buf = header.buffer_count()?;
        let mut buf_offsets = [0; MAX_BUFS];
        for (index, offset) in buf_offsets.iter_mut().enumerate().take(num_buf) {
            *offset = header.buffer_range(index, available)?.start;
        }
        let tick_rate = u64::try_from(header.tick_rate)
            .map_err(|_| LayoutError::OutOfRange { field: "tickRate" })?;
        let session_info = header.session_info_range(available)?;
        Ok(Self {
            layout: LiveLayout {
                vars: headers,
                buf_len,
                tick_rate: header.tick_rate,
                session_info_capacity: session_info.len(),
                sdk_version: header.ver,
            },
            spans,
            buf_offsets,
            num_buf,
            key: LayoutKey::of(header),
            session_info,
            tick_period: Duration::from_nanos(1_000_000_000 / tick_rate.max(1)),
        })
    }
}

/// Where the reader takes buffer tick counts and bytes from; a seam for testing torn reads.
pub(crate) trait Source {
    fn tick(&self, buf: usize) -> Option<i32>;
    fn copy(&self, offset: usize, out: &mut [u8]) -> bool;
}

impl Source for Mapping {
    fn tick(&self, buf: usize) -> Option<i32> {
        self.load_i32(tick_offset(buf))
    }

    fn copy(&self, offset: usize, out: &mut [u8]) -> bool {
        self.read(offset, out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadOutcome {
    New {
        tick: i32,
        missed: u32,
        buffer: usize,
    },
    Empty,
    Torn,
}

/// Picks the buffer to read: the oldest unseen tick, the newest tick when nothing was seen yet, or the
/// newest tick when the counter went backwards (a sim restart).
fn pick<S: Source>(src: &S, plan: &Plan, last: Option<i32>) -> Option<(usize, i32)> {
    let mut newest: Option<(usize, i32)> = None;
    let mut oldest_unseen: Option<(usize, i32)> = None;
    for buf in 0..plan.num_buf {
        let tick = src.tick(buf)?;
        if newest.is_none_or(|(_, t)| tick > t) {
            newest = Some((buf, tick));
        }
        if last.is_some_and(|l| tick > l) && oldest_unseen.is_none_or(|(_, t)| tick < t) {
            oldest_unseen = Some((buf, tick));
        }
    }
    match last {
        None => newest,
        Some(l) => oldest_unseen.or_else(|| newest.filter(|&(_, t)| t < l)),
    }
}

/// Copies the planned variables of the next tick into `out`, retrying once when the buffer is overwritten
/// during the copy.
fn read_next<S: Source>(src: &S, plan: &Plan, last: Option<i32>, out: &mut [u8]) -> ReadOutcome {
    for _ in 0..2 {
        let Some((buf, tick)) = pick(src, plan, last) else {
            return ReadOutcome::Empty;
        };
        let Some(&base) = plan.buf_offsets.get(buf) else {
            return ReadOutcome::Torn;
        };
        let mut copied = true;
        for span in &plan.spans {
            match out.get_mut(span.dst..span.dst + span.len) {
                Some(dst) => copied &= src.copy(base + span.src, dst),
                None => copied = false,
            }
        }
        // Keep the second tick load after the data copies.
        fence(Ordering::Acquire);
        if copied && src.tick(buf) == Some(tick) {
            let missed = match last {
                Some(prev) if tick > prev => {
                    u32::try_from(i64::from(tick) - i64::from(prev) - 1).unwrap_or(u32::MAX)
                }
                _ => 0,
            };
            return ReadOutcome::New {
                tick,
                missed,
                buffer: buf,
            };
        }
    }
    ReadOutcome::Torn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::VarType;
    use crate::publish::{Publisher, PublisherConfig};
    use core::cell::Cell;
    use std::sync::atomic::AtomicUsize;

    const ROW: usize = 16 + 72 * 4;

    fn names() -> SdkNames {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        SdkNames::new(
            &format!("Local\\PitotTestMap-{pid}-{n}"),
            &format!("Local\\PitotTestEvent-{pid}-{n}"),
        )
    }

    fn sim_vars() -> Vec<VarHeader> {
        vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Float, 8, 1, "ThrottleRaw").expect("valid"),
            VarHeader::new(VarType::Int, 12, 1, "Gear").expect("valid"),
            VarHeader::new(VarType::Float, 16, 72, "CarIdxLapDistPct").expect("valid"),
        ]
    }

    fn row(time: f64, throttle: f32) -> [u8; ROW] {
        let mut row = [0u8; ROW];
        row[0..8].copy_from_slice(&time.to_le_bytes());
        row[8..12].copy_from_slice(&throttle.to_le_bytes());
        row[12..16].copy_from_slice(&3i32.to_le_bytes());
        row[16 + 71 * 4..].copy_from_slice(&0.5f32.to_le_bytes());
        row
    }

    fn publisher(names: &SdkNames) -> Publisher {
        let config = PublisherConfig {
            vars: sim_vars(),
            buf_len: ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 4096,
        };
        let mut publisher = Publisher::create(names, &config).expect("creates");
        publisher.set_connected(true).expect("connects");
        publisher
    }

    struct Reader {
        reader: LiveReader,
        snapshot: Snapshot,
        time: VarId,
        throttle: VarId,
        laps: VarId,
        lat: VarId,
    }

    fn reader(names: &SdkNames) -> Reader {
        let mut vars = VarSet::new();
        let time = vars.add("SessionTime");
        let throttle = vars.add("ThrottleRaw");
        let laps = vars.add("CarIdxLapDistPct");
        let lat = vars.add("Lat");
        let config = ReaderConfig {
            retry_interval: Duration::from_millis(1),
            idle_timeout: Duration::from_millis(20),
        };
        Reader {
            reader: LiveReader::new(names.clone(), vars, config),
            snapshot: Snapshot::new(),
            time,
            throttle,
            laps,
            lat,
        }
    }

    impl Reader {
        fn step(&mut self) -> Step {
            self.reader.step(&mut self.snapshot).expect("steps")
        }
    }

    #[test]
    fn row_maps_fill_live_layouts_from_other_tables() {
        let names = names();
        let mut publisher = publisher(&names);
        let mut r = reader(&names);
        assert_eq!(r.step(), Step::Connected);
        publisher.publish(&row(1.0, 0.25)).expect("publishes");
        assert!(matches!(r.step(), Step::Tick { .. }));

        // A recording's table: other order, ThrottleRaw stored as an Int, a shorter per-car array.
        let recorded = vec![
            VarHeader::new(VarType::Float, 0, 4, "CarIdxLapDistPct").expect("valid"),
            VarHeader::new(VarType::Int, 16, 1, "ThrottleRaw").expect("valid"),
            VarHeader::new(VarType::Double, 20, 1, "SessionTime").expect("valid"),
        ];
        let mut rec = vec![0u8; 28];
        for car in 0..4 {
            rec[car * 4..car * 4 + 4].copy_from_slice(&(car as f32 / 10.0).to_le_bytes());
        }
        rec[16..20].copy_from_slice(&7i32.to_le_bytes());
        rec[20..28].copy_from_slice(&42.5f64.to_le_bytes());

        let map = RowMap::new(r.reader.var_set(), &r.snapshot, &recorded, rec.len());
        let mut snap = r.snapshot.clone();
        map.copy(&rec, 99, &mut snap);
        assert_eq!(snap.tick(), 99);
        assert_eq!(snap.f64(r.time, 0), Some(42.5));
        assert_eq!(
            snap.f32(r.throttle, 0),
            Some(0.0),
            "another type reads as zero"
        );
        assert_eq!(snap.count(r.laps), 72, "the live layout is kept");
        assert_eq!(snap.f32(r.laps, 3), Some(0.3));
        assert_eq!(
            snap.f32(r.laps, 71),
            Some(0.0),
            "elements the row lacks are zero"
        );
        assert!(!snap.available(r.lat));
        // The live snapshot is untouched.
        assert_eq!(r.snapshot.f32(r.throttle, 0), Some(0.25));
    }

    #[test]
    fn not_running_without_mapping() {
        let mut r = reader(&names());
        assert_eq!(r.step(), Step::NotRunning);
        assert_eq!(r.step(), Step::NotRunning);
        assert!(!r.reader.is_connected());
    }

    #[test]
    fn not_running_until_publisher_connects() {
        let names = names();
        let config = PublisherConfig {
            vars: sim_vars(),
            buf_len: ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 64,
        };
        let mut publisher = Publisher::create(&names, &config).expect("creates");
        let mut r = reader(&names);
        assert_eq!(r.step(), Step::NotRunning);
        publisher.set_connected(true).expect("connects");
        assert_eq!(r.step(), Step::Connected);
    }

    #[test]
    fn reads_requested_fields() {
        let names = names();
        let mut publisher = publisher(&names);
        let mut r = reader(&names);
        assert_eq!(r.step(), Step::Connected);
        assert!(!r.snapshot.available(r.lat));
        assert_eq!(r.snapshot.f32(r.lat, 0), None);
        assert_eq!(r.snapshot.count(r.laps), 72);
        let tick = publisher.publish(&row(12.5, 0.25)).expect("publishes");
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.snapshot.tick(), tick);
        assert_eq!(r.snapshot.f64(r.time, 0), Some(12.5));
        assert_eq!(r.snapshot.f32(r.throttle, 0), Some(0.25));
        assert_eq!(r.snapshot.f32(r.laps, 71), Some(0.5));
        assert_eq!(r.snapshot.f32(r.laps, 72), None);
        assert_eq!(r.snapshot.f64(r.throttle, 0), None, "type mismatch");
        assert_eq!(r.step(), Step::Idle);
    }

    #[test]
    fn whole_rows_and_layout_for_recorders() {
        let names = names();
        let mut publisher = publisher(&names);
        let config = ReaderConfig {
            retry_interval: Duration::from_millis(1),
            idle_timeout: Duration::from_millis(20),
        };
        let mut reader = LiveReader::new(names.clone(), VarSet::whole_row(), config);
        let mut snapshot = Snapshot::new();
        assert_eq!(reader.layout(), None);
        assert_eq!(reader.step(&mut snapshot).expect("steps"), Step::Connected);
        let layout = reader.layout().expect("connected").clone();
        assert_eq!(layout.vars, sim_vars());
        assert_eq!(
            (
                layout.buf_len,
                layout.tick_rate,
                layout.session_info_capacity
            ),
            (ROW, 60, 4096)
        );
        assert_eq!(layout.sdk_version, crate::layout::SDK_VERSION);
        let published = row(3.0, 0.75);
        let tick = publisher.publish(&published).expect("publishes");
        assert_eq!(
            reader.step(&mut snapshot).expect("steps"),
            Step::Tick { missed: 0 }
        );
        assert_eq!(snapshot.bytes(), &published[..], "the whole row");
        assert_eq!(snapshot.tick(), tick);
        let first = snapshot.buffer();
        assert!(first < 3);
        let second = publisher.publish(&row(4.0, 0.5)).expect("publishes");
        reader.step(&mut snapshot).expect("steps");
        assert_eq!(snapshot.tick(), second);
        assert_ne!(snapshot.buffer(), first, "the sim rotates its buffers");
    }

    #[test]
    fn catches_up_in_tick_order_and_counts_missed() {
        let names = names();
        let mut publisher = publisher(&names);
        let mut r = reader(&names);
        assert_eq!(r.step(), Step::Connected);
        for n in 0..2 {
            publisher
                .publish(&row(f64::from(n), 0.0))
                .expect("publishes");
        }
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.snapshot.f64(r.time, 0), Some(0.0));
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.snapshot.f64(r.time, 0), Some(1.0));
        // Five ticks into three buffers: the two oldest are lost and reported.
        for n in 2..7 {
            publisher
                .publish(&row(f64::from(n), 0.0))
                .expect("publishes");
        }
        assert_eq!(r.step(), Step::Tick { missed: 2 });
        assert_eq!(r.snapshot.f64(r.time, 0), Some(4.0));
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.snapshot.f64(r.time, 0), Some(6.0));
    }

    #[test]
    fn disconnects_and_reconnects() {
        let names = names();
        let mut publisher = publisher(&names);
        let mut r = reader(&names);
        assert_eq!(r.step(), Step::Connected);
        publisher.set_connected(false).expect("disconnects");
        assert_eq!(r.step(), Step::Disconnected);
        assert!(!r.reader.is_connected());
        drop(publisher);
        assert_eq!(r.step(), Step::NotRunning);
        let mut publisher = self::publisher(&names);
        assert_eq!(r.step(), Step::Connected);
        publisher.publish(&row(1.0, 0.75)).expect("publishes");
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.snapshot.f32(r.throttle, 0), Some(0.75));
    }

    #[test]
    fn layout_change_resolves_again() {
        let names = names();
        let mut publisher = publisher(&names);
        let mut r = reader(&names);
        assert_eq!(r.step(), Step::Connected);
        // A new car: ThrottleRaw moves and the per-car array disappears.
        let vars = [
            VarHeader::new(VarType::Float, 4, 1, "ThrottleRaw").expect("valid"),
            VarHeader::new(VarType::Double, 8, 1, "SessionTime").expect("valid"),
        ];
        publisher.set_layout(&vars, 16).expect("fits");
        assert_eq!(r.step(), Step::Connected);
        assert!(!r.snapshot.available(r.laps));
        let mut small = [0u8; 16];
        small[4..8].copy_from_slice(&0.5f32.to_le_bytes());
        small[8..16].copy_from_slice(&3.0f64.to_le_bytes());
        publisher.publish(&small).expect("publishes");
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.snapshot.f32(r.throttle, 0), Some(0.5));
        assert_eq!(r.snapshot.f64(r.time, 0), Some(3.0));
    }

    #[test]
    fn tick_counter_reset_restarts_from_newest() {
        let names = names();
        let mut publisher = publisher(&names);
        publisher.set_next_tick(1000);
        for _ in 0..3 {
            publisher.publish(&row(1.0, 0.0)).expect("publishes");
        }
        let mut r = reader(&names);
        assert_eq!(r.step(), Step::Connected);
        // The counter restarts; stale buffers keep higher ticks until all of them are rewritten.
        publisher.set_next_tick(5);
        for n in 0..3 {
            publisher
                .publish(&row(10.0 + f64::from(n), 0.0))
                .expect("publishes");
        }
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.snapshot.tick(), 7);
        assert_eq!(r.snapshot.f64(r.time, 0), Some(12.0));
        publisher.publish(&row(13.0, 0.0)).expect("publishes");
        assert_eq!(r.step(), Step::Tick { missed: 0 });
        assert_eq!(r.snapshot.tick(), 8);
    }

    #[test]
    fn copies_session_info() {
        let names = names();
        let mut publisher = publisher(&names);
        let mut r = reader(&names);
        assert_eq!(r.reader.session_info_update(), None);
        assert_eq!(r.step(), Step::Connected);
        let update = publisher
            .set_session_info(b"WeekendInfo:\n TrackName: synthetic\n")
            .expect("fits");
        assert_eq!(r.reader.session_info_update(), Some(update));
        let mut out = Vec::with_capacity(r.reader.session_info_capacity().expect("connected"));
        assert_eq!(r.reader.copy_session_info(&mut out), Ok(Some(update)));
        assert_eq!(out, b"WeekendInfo:\n TrackName: synthetic\n");
    }

    #[test]
    fn publisher_refuses_names_in_use() {
        let names = names();
        let _first = publisher(&names);
        let config = PublisherConfig {
            vars: sim_vars(),
            buf_len: ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 64,
        };
        assert_eq!(
            Publisher::create(&names, &config).err(),
            Some(LiveError::AlreadyExists)
        );
    }

    /// Buffers whose tick changes after a configurable number of tick loads, to simulate the sim writing
    /// during a copy.
    struct FakeSource {
        ticks: [i32; 3],
        loads: Cell<usize>,
        bump_after: Vec<usize>,
    }

    impl Source for FakeSource {
        fn tick(&self, buf: usize) -> Option<i32> {
            let n = self.loads.get();
            self.loads.set(n + 1);
            let bumped = self.bump_after.iter().filter(|&&after| n >= after).count() as i32;
            Some(self.ticks[buf] + if buf == 2 { bumped } else { 0 })
        }

        fn copy(&self, _offset: usize, out: &mut [u8]) -> bool {
            out.fill(1);
            true
        }
    }

    fn fake_plan() -> Plan {
        Plan {
            layout: LiveLayout {
                vars: Vec::new(),
                buf_len: 4,
                tick_rate: 60,
                session_info_capacity: 0,
                sdk_version: 2,
            },
            spans: vec![Span {
                src: 0,
                dst: 0,
                len: 4,
            }],
            buf_offsets: [0, 16, 32, 0],
            num_buf: 3,
            key: LayoutKey::of(&Header::default()),
            session_info: 0..0,
            tick_period: Duration::from_millis(16),
        }
    }

    #[test]
    fn torn_copy_retries_once() {
        let mut out = [0u8; 4];
        // Loads 0-2 pick buffer 2 (tick 12); load 3 re-checks it after the copy and sees 13.
        let source = FakeSource {
            ticks: [10, 11, 12],
            loads: Cell::new(0),
            bump_after: vec![3],
        };
        assert_eq!(
            read_next(&source, &fake_plan(), Some(9), &mut out),
            ReadOutcome::New {
                tick: 10,
                missed: 0,
                buffer: 0
            },
            "oldest unseen buffer is read first and was not touched"
        );
        let source = FakeSource {
            ticks: [10, 11, 12],
            loads: Cell::new(0),
            bump_after: vec![3],
        };
        assert_eq!(
            read_next(&source, &fake_plan(), Some(11), &mut out),
            ReadOutcome::New {
                tick: 13,
                missed: 1,
                buffer: 2
            },
            "second attempt reads the rewritten buffer"
        );
        let source = FakeSource {
            ticks: [10, 11, 12],
            loads: Cell::new(0),
            bump_after: vec![3, 7],
        };
        assert_eq!(
            read_next(&source, &fake_plan(), Some(11), &mut out),
            ReadOutcome::Torn
        );
        assert_eq!(out, [1; 4]);
    }

    #[test]
    fn pick_handles_first_read_and_reset() {
        let source = FakeSource {
            ticks: [7, 8, 6],
            loads: Cell::new(0),
            bump_after: vec![],
        };
        let plan = fake_plan();
        assert_eq!(pick(&source, &plan, None), Some((1, 8)));
        assert_eq!(pick(&source, &plan, Some(6)), Some((0, 7)));
        assert_eq!(pick(&source, &plan, Some(8)), None);
        assert_eq!(
            pick(&source, &plan, Some(100)),
            Some((1, 8)),
            "counter went backwards"
        );
    }
}
