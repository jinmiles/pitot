//! Extraction of the session info sections the overlay uses from the sim's session YAML.
//!
//! The sim's text is not strict YAML: it is Windows-1252 unless `WeekendInfo` declares `Encoding: UTF8`, and
//! free-text fields such as driver and team names are written unquoted. [`parse`] therefore decodes the text,
//! keeps only the `WeekendInfo`, `SessionInfo`, `DriverInfo`, and `SplitTimeInfo` sections, quotes free-text
//! values, and then reads the result with a YAML parser. Missing or unexpected fields become `None` rather
//! than failing the whole extraction.
//!
//! The fields are taken from the parser's events as they come, without building a document tree. A tree of
//! the full text is about 9,000 short-lived allocations per update; built on every update, it fragments the
//! heap so that the process's committed memory keeps creeping up for hours.
//!
//! Call it off the data thread, only when `sessionInfoUpdate` changes. Errors carry a position only, never
//! session text, because the text contains other drivers' names and IDs.

use std::borrow::Cow;
use std::fmt;
use std::mem;

use saphyr::Scalar;
use saphyr_parser::{Event, Parser};

/// Top-level sections kept for parsing; everything else (setup, cameras, radio) is dropped first.
const SECTIONS: [&str; 4] = ["WeekendInfo", "SessionInfo", "DriverInfo", "SplitTimeInfo"];

/// Free-text keys the sim writes unquoted, so a name like `*Star` or `A: B` would break a strict parser.
const FREE_TEXT_KEYS: [&str; 5] = [
    "UserName",
    "TeamName",
    "AbbrevName",
    "Initials",
    "DriverSetupName",
];

/// Errors from [`parse`]. Positions refer to the text after section filtering and quoting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// The YAML could not be parsed.
    Syntax { line: usize, col: usize },
    /// The text holds no YAML document.
    Empty,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { line, col } => {
                write!(f, "session info syntax error at line {line}, column {col}")
            }
            Self::Empty => f.write_str("session info is empty"),
        }
    }
}

impl std::error::Error for SessionError {}

/// Extracted session info.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionInfo {
    pub weekend: WeekendInfo,
    pub driver_info: DriverInfo,
    pub sessions: Vec<Session>,
    pub sectors: Vec<Sector>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WeekendInfo {
    pub track_id: Option<i64>,
    pub track_name: Option<String>,
    pub track_display_name: Option<String>,
    pub track_length_km: Option<f64>,
    pub category: Option<String>,
    pub event_type: Option<String>,
    pub official: Option<bool>,
    pub num_car_classes: Option<i64>,
    pub series_id: Option<i64>,
    pub session_id: Option<i64>,
    pub sub_session_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DriverInfo {
    /// `CarIdx` of the player's car.
    pub player_car_idx: Option<usize>,
    pub pace_car_idx: Option<usize>,
    pub fuel_max_liters: Option<f64>,
    pub max_fuel_pct: Option<f64>,
    pub est_lap_time: Option<f64>,
    /// Indexed by `CarIdx`; the length is the `max_cars` passed to [`parse`].
    pub drivers: Vec<Option<Driver>>,
    /// Entries whose `CarIdx` was missing or not below `max_cars`.
    pub dropped: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Driver {
    pub car_idx: usize,
    pub user_name: Option<String>,
    pub abbrev_name: Option<String>,
    pub initials: Option<String>,
    pub user_id: Option<i64>,
    pub team_name: Option<String>,
    pub car_number: Option<String>,
    pub car_screen_name_short: Option<String>,
    pub car_class_id: Option<i64>,
    pub car_class_short_name: Option<String>,
    pub car_class_rel_speed: Option<i64>,
    pub car_class_est_lap_time: Option<f64>,
    /// `0xRRGGBB`.
    pub car_class_color: Option<u32>,
    pub irating: Option<i64>,
    pub license: Option<String>,
    /// `0xRRGGBB`.
    pub license_color: Option<u32>,
    pub incidents: Option<i64>,
    pub is_spectator: bool,
    pub is_pace_car: bool,
    pub is_ai: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Session {
    pub num: Option<i64>,
    pub session_type: Option<String>,
    pub name: Option<String>,
    /// `None` when unlimited or missing; see `unlimited_laps`.
    pub laps: Option<i64>,
    pub unlimited_laps: bool,
    /// `None` when unlimited or missing; see `unlimited_time`.
    pub time_seconds: Option<f64>,
    pub unlimited_time: bool,
    pub results: Vec<ResultPosition>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResultPosition {
    pub position: Option<i64>,
    pub class_position: Option<i64>,
    pub car_idx: Option<usize>,
    pub lap: Option<i64>,
    pub time: Option<f64>,
    pub fastest_lap: Option<i64>,
    pub fastest_time: Option<f64>,
    pub last_time: Option<f64>,
    pub laps_led: Option<i64>,
    pub laps_complete: Option<i64>,
    pub incidents: Option<i64>,
    pub reason_out: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sector {
    pub num: Option<i64>,
    pub start_pct: Option<f64>,
}

/// Decodes the raw session info bytes to text: UTF-8 when `WeekendInfo` declares `Encoding: UTF8`,
/// Windows-1252 otherwise. Bytes Windows-1252 leaves undefined become U+FFFD.
pub fn decode(raw: &[u8]) -> String {
    let head = &raw[..raw.len().min(256)];
    let utf8 = head
        .split(|&b| b == b'\n')
        .any(|line| line.trim_ascii() == b"Encoding: UTF8");
    if utf8 {
        return String::from_utf8_lossy(raw).into_owned();
    }
    if raw.is_ascii() {
        return raw.iter().map(|&b| char::from(b)).collect();
    }
    let mut text = String::with_capacity(raw.len() + raw.len() / 8);
    text.extend(raw.iter().map(|&b| windows_1252(b)));
    text
}

/// Parses raw session info bytes. `max_cars` sizes [`DriverInfo::drivers`]; pass the `CarIdx*` array length
/// of the live connection.
pub fn parse(raw: &[u8], max_cars: usize) -> Result<SessionInfo, SessionError> {
    let text = prepare(&decode(raw));
    let mut walk = Walk::new(max_cars);
    // Character input allocates about half as often as `Parser::new_from_str` (saphyr-parser 0.3).
    for event in Parser::new_from_iter(text.chars()) {
        let (event, _) = event.map_err(|e| SessionError::Syntax {
            line: e.marker().line(),
            col: e.marker().col(),
        })?;
        if walk.event(event) == Flow::Done {
            break;
        }
    }
    if walk.documents == 0 {
        return Err(SessionError::Empty);
    }
    Ok(walk.info)
}

/// Keeps the wanted top-level sections, drops control characters, and quotes free-text values and values
/// that start with a comma.
fn prepare(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut keep = false;
    for line in text.lines() {
        if let Some(name) = top_level_key(line) {
            keep = SECTIONS.contains(&name);
        }
        if !keep {
            continue;
        }
        // Allocate only for the rare lines that need changing.
        let is_stray = |c: char| c != '\t' && c.is_control();
        let cleaned;
        let line = if line.contains(is_stray) {
            cleaned = line.chars().filter(|&c| !is_stray(c)).collect::<String>();
            cleaned.as_str()
        } else {
            line
        };
        match quoted(line) {
            Some(fixed) => out.push_str(&fixed),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// Name of a top-level mapping key line such as `DriverInfo:`.
fn top_level_key(line: &str) -> Option<&str> {
    let name = line.strip_suffix(':')?;
    let first = name.chars().next()?;
    (first.is_ascii_alphabetic() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then_some(name)
}

/// Rewrites `key: value` as `key: "value"` for free-text keys and comma-led values.
fn quoted(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start_matches([' ', '-']).len();
    let (prefix, rest) = line.split_at(indent);
    let (key, value) = rest.split_once(": ")?;
    if value.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    if !(FREE_TEXT_KEYS.contains(&key) || value.starts_with(',')) {
        return None;
    }
    // A value the sim already wrapped in double quotes is taken as raw text and quoted again consistently.
    let raw = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
    Some(format!("{prefix}{key}: \"{escaped}\""))
}

/// Where a node sits in the document, as far as extraction cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
    /// The document's top-level mapping.
    Root,
    Weekend,
    DriverInfo,
    Drivers,
    Driver,
    SessionInfo,
    Sessions,
    Session,
    Results,
    Result,
    SplitTimeInfo,
    Sectors,
    Sector,
    /// Anything not extracted, and everything inside it.
    Skip,
}

impl Node {
    /// The node that a mapping (`map`) or a sequence starting under `parent` is. `key` is the parent mapping's
    /// key for it, or `None` for a sequence item.
    fn child(parent: Option<Self>, key: Option<&str>, map: bool) -> Self {
        use Node::*;
        match (parent, key, map) {
            (None, None, true) => Root,
            (Some(Root), Some("WeekendInfo"), true) => Weekend,
            (Some(Root), Some("DriverInfo"), true) => DriverInfo,
            (Some(Root), Some("SessionInfo"), true) => SessionInfo,
            (Some(Root), Some("SplitTimeInfo"), true) => SplitTimeInfo,
            (Some(DriverInfo), Some("Drivers"), false) => Drivers,
            (Some(Drivers), None, true) => Driver,
            (Some(SessionInfo), Some("Sessions"), false) => Sessions,
            (Some(Sessions), None, true) => Session,
            (Some(Session), Some("ResultsPositions"), false) => Results,
            (Some(Results), None, true) => Result,
            (Some(SplitTimeInfo), Some("Sectors"), false) => Sectors,
            (Some(Sectors), None, true) => Sector,
            _ => Skip,
        }
    }
}

struct Frame<'i> {
    node: Node,
    map: bool,
    /// In a mapping: the key whose value comes next, or `None` when a key comes next.
    key: Option<Cow<'i, str>>,
}

#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Done,
}

/// Extraction state while the parser's events are read. List entries are built here and moved into `info`
/// when their mapping ends.
struct Walk<'i> {
    stack: Vec<Frame<'i>>,
    info: SessionInfo,
    documents: usize,
    driver_idx: Option<usize>,
    driver: Driver,
    session: Session,
    result: ResultPosition,
    sector: Sector,
}

impl<'i> Walk<'i> {
    fn new(max_cars: usize) -> Self {
        Self {
            stack: Vec::with_capacity(8),
            info: SessionInfo {
                driver_info: DriverInfo {
                    drivers: vec![None; max_cars],
                    ..DriverInfo::default()
                },
                ..SessionInfo::default()
            },
            documents: 0,
            driver_idx: None,
            driver: Driver::default(),
            session: Session::default(),
            result: ResultPosition::default(),
            sector: Sector::default(),
        }
    }

    fn event(&mut self, event: Event<'i>) -> Flow {
        match event {
            Event::DocumentStart(_) => self.documents += 1,
            // Only the first document is read.
            Event::DocumentEnd => return Flow::Done,
            Event::MappingStart(..) => self.start(true),
            Event::SequenceStart(..) => self.start(false),
            Event::MappingEnd | Event::SequenceEnd => self.end(),
            Event::Scalar(value, style, _, tag) => {
                let Some(top) = self.stack.last_mut().filter(|top| top.map) else {
                    return Flow::Continue;
                };
                match top.key.take() {
                    None => top.key = Some(value),
                    Some(key) => {
                        let node = top.node;
                        if let Some(value) =
                            Scalar::parse_from_cow_and_metadata(value, style, tag.as_ref())
                        {
                            self.value(node, &key, value);
                        }
                    }
                }
            }
            // The sim writes no anchors. An alias in a mapping is a key or a value that is not extracted.
            Event::Alias(_) => {
                if let Some(top) = self.stack.last_mut().filter(|top| top.map) {
                    top.key = match top.key {
                        Some(_) => None,
                        None => Some(Cow::Borrowed("")),
                    };
                }
            }
            Event::StreamStart | Event::StreamEnd | Event::Nothing => {}
        }
        Flow::Continue
    }

    fn start(&mut self, map: bool) {
        let node = match self.stack.last() {
            None => Node::child(None, None, map),
            Some(top) if !top.map => Node::child(Some(top.node), None, map),
            Some(top) => match top.key.as_deref() {
                Some(key) => Node::child(Some(top.node), Some(key), map),
                // A container used as a mapping key, which nothing extracted has.
                None => Node::Skip,
            },
        };
        match node {
            Node::Driver => {
                self.driver_idx = None;
                self.driver = Driver::default();
            }
            Node::Session => self.session = Session::default(),
            Node::Result => self.result = ResultPosition::default(),
            Node::Sector => self.sector = Sector::default(),
            _ => {}
        }
        self.stack.push(Frame {
            node,
            map,
            key: None,
        });
    }

    fn end(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        match frame.node {
            Node::Driver => {
                let drivers = &mut self.info.driver_info.drivers;
                match self
                    .driver_idx
                    .and_then(|car_idx| Some((car_idx, drivers.get_mut(car_idx)?)))
                {
                    Some((car_idx, slot)) => {
                        *slot = Some(Driver {
                            car_idx,
                            ..mem::take(&mut self.driver)
                        });
                    }
                    None => self.info.driver_info.dropped += 1,
                }
            }
            Node::Session => self.info.sessions.push(mem::take(&mut self.session)),
            Node::Result => self.session.results.push(mem::take(&mut self.result)),
            Node::Sector => self.info.sectors.push(mem::take(&mut self.sector)),
            _ => {}
        }
        if let Some(parent) = self.stack.last_mut().filter(|parent| parent.map) {
            // The container was the value of the parent's key. If it was a key instead, its value comes next,
            // under a key that matches nothing.
            parent.key = match parent.key {
                Some(_) => None,
                None => Some(Cow::Borrowed("")),
            };
        }
    }

    fn value(&mut self, node: Node, key: &str, v: Scalar<'i>) {
        match node {
            Node::Weekend => {
                let w = &mut self.info.weekend;
                match key {
                    "TrackID" => w.track_id = int(&v),
                    "TrackName" => w.track_name = text(v),
                    "TrackDisplayName" => w.track_display_name = text(v),
                    "TrackLength" => w.track_length_km = float(&v),
                    "Category" => w.category = text(v),
                    "EventType" => w.event_type = text(v),
                    "Official" => w.official = int(&v).map(|v| v != 0),
                    "NumCarClasses" => w.num_car_classes = int(&v),
                    "SeriesID" => w.series_id = int(&v),
                    "SessionID" => w.session_id = int(&v),
                    "SubSessionID" => w.sub_session_id = int(&v),
                    _ => {}
                }
            }
            Node::DriverInfo => {
                let d = &mut self.info.driver_info;
                match key {
                    "DriverCarIdx" => d.player_car_idx = index(&v),
                    "PaceCarIdx" => d.pace_car_idx = index(&v),
                    "DriverCarFuelMaxLtr" => d.fuel_max_liters = float(&v),
                    "DriverCarMaxFuelPct" => d.max_fuel_pct = float(&v),
                    "DriverCarEstLapTime" => d.est_lap_time = float(&v),
                    _ => {}
                }
            }
            Node::Driver => {
                let d = &mut self.driver;
                match key {
                    "CarIdx" => self.driver_idx = index(&v),
                    "UserName" => d.user_name = text(v),
                    "AbbrevName" => d.abbrev_name = text(v),
                    "Initials" => d.initials = text(v),
                    "UserID" => d.user_id = int(&v),
                    "TeamName" => d.team_name = text(v),
                    "CarNumber" => d.car_number = text(v),
                    "CarScreenNameShort" => d.car_screen_name_short = text(v),
                    "CarClassID" => d.car_class_id = int(&v),
                    "CarClassShortName" => d.car_class_short_name = text(v),
                    "CarClassRelSpeed" => d.car_class_rel_speed = int(&v),
                    "CarClassEstLapTime" => d.car_class_est_lap_time = float(&v),
                    "CarClassColor" => d.car_class_color = color(&v),
                    "IRating" => d.irating = int(&v),
                    "LicString" => d.license = text(v),
                    "LicColor" => d.license_color = color(&v),
                    "CurDriverIncidentCount" => d.incidents = int(&v),
                    "IsSpectator" => d.is_spectator = flag(&v),
                    "CarIsPaceCar" => d.is_pace_car = flag(&v),
                    "CarIsAI" => d.is_ai = flag(&v),
                    _ => {}
                }
            }
            Node::Session => {
                let s = &mut self.session;
                match key {
                    "SessionNum" => s.num = int(&v),
                    "SessionType" => s.session_type = text(v),
                    "SessionName" => s.name = text(v),
                    "SessionLaps" => {
                        s.laps = int(&v);
                        s.unlimited_laps = is_unlimited(&v);
                    }
                    "SessionTime" => {
                        s.time_seconds = float(&v);
                        s.unlimited_time = is_unlimited(&v);
                    }
                    _ => {}
                }
            }
            Node::Result => {
                let r = &mut self.result;
                match key {
                    "Position" => r.position = int(&v),
                    "ClassPosition" => r.class_position = int(&v),
                    "CarIdx" => r.car_idx = index(&v),
                    "Lap" => r.lap = int(&v),
                    "Time" => r.time = float(&v),
                    "FastestLap" => r.fastest_lap = int(&v),
                    "FastestTime" => r.fastest_time = float(&v),
                    "LastTime" => r.last_time = float(&v),
                    "LapsLed" => r.laps_led = int(&v),
                    "LapsComplete" => r.laps_complete = int(&v),
                    "Incidents" => r.incidents = int(&v),
                    "ReasonOutStr" => r.reason_out = text(v),
                    _ => {}
                }
            }
            Node::Sector => match key {
                "SectorNum" => self.sector.num = int(&v),
                "SectorStartPct" => self.sector.start_pct = float(&v),
                _ => {}
            },
            _ => {}
        }
    }
}

fn text(v: Scalar) -> Option<String> {
    match v {
        // A copy, so the result does not keep the parser buffers' spare capacity.
        Scalar::String(s) => Some(String::from(&*s)),
        Scalar::Integer(i) => Some(i.to_string()),
        Scalar::FloatingPoint(f) => Some(f.0.to_string()),
        Scalar::Boolean(b) => Some(b.to_string()),
        Scalar::Null => None,
    }
}

fn is_unlimited(v: &Scalar) -> bool {
    matches!(v, Scalar::String(s) if s == "unlimited")
}

fn int(v: &Scalar) -> Option<i64> {
    match v {
        Scalar::Integer(i) => Some(*i),
        Scalar::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn index(v: &Scalar) -> Option<usize> {
    int(v).and_then(|i| usize::try_from(i).ok())
}

/// A number, or a string that starts with one, such as `5.93 km` or `600.0000 sec`.
fn float(v: &Scalar) -> Option<f64> {
    match v {
        Scalar::FloatingPoint(f) => Some(f.0),
        Scalar::Integer(i) => Some(*i as f64),
        Scalar::String(s) => s.split_whitespace().next()?.parse().ok(),
        _ => None,
    }
}

fn flag(v: &Scalar) -> bool {
    int(v).is_some_and(|v| v != 0)
}

/// `0xRRGGBB`, written by the sim as a plain scalar that YAML may already read as a hex integer.
fn color(v: &Scalar) -> Option<u32> {
    match v {
        Scalar::Integer(i) => u32::try_from(*i).ok(),
        Scalar::String(s) => {
            let s = s.trim();
            let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
            u32::from_str_radix(hex, 16).ok()
        }
        _ => None,
    }
}

/// Windows-1252 to Unicode. 0x80-0x9F differ from ISO 8859-1; the five undefined bytes map to U+FFFD.
fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}',
        '\u{017D}', '\u{FFFD}', '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
        '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
    ];
    match b {
        0x80..=0x9F => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---
WeekendInfo:
 Encoding: ISO_8859_1
 TrackName: synthetic oval
 TrackID: 999
 TrackLength: 2.50 km
 TrackDisplayName: Synthetic Speedway
 Category: Oval
 Official: 0
 NumCarClasses: 2
 EventType: Race
 SubSessionID: 0
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionLaps: unlimited
   SessionTime: 600.0000 sec
   SessionType: Practice
   ResultsPositions:
 - SessionNum: 1
   SessionLaps: 20
   SessionTime: unlimited
   SessionType: Race
   ResultsPositions:
   - Position: 1
     ClassPosition: 0
     CarIdx: 1
     Lap: 3
     Time: -1.0000
     FastestLap: 2
     FastestTime: 31.2500
     LapsComplete: 3
     ReasonOutStr: Running
CameraInfo:
 Groups:
 - GroupNum: 1
RadioInfo:
 Radios: [unbalanced
DriverInfo:
 DriverCarIdx: 1
 PaceCarIdx: 0
 DriverCarFuelMaxLtr: 50.000
 DriverCarMaxFuelPct: 1.000
 Drivers:
 - CarIdx: 0
   UserName: Pace Car
   CarIsPaceCar: 1
   CarClassColor: 0xffffff
 - CarIdx: 1
   UserName: *Ana: Test #1
   AbbrevName: , Ana
   Initials: \"AT\"
   UserID: 101
   TeamName: Team \\ Back
   CarNumber: \"07\"
   CarClassID: 11
   CarClassShortName: GT3
   CarClassColor: 0xffda59
   IRating: 1500
   LicString: B 3.21
   LicColor: 0x00c702
   CurDriverIncidentCount: 4
 - CarIdx: 90
   UserName: Out Of Range
 - UserName: No Index
SplitTimeInfo:
 Sectors:
 - SectorNum: 0
   SectorStartPct: 0.000000
 - SectorNum: 1
   SectorStartPct: 0.500000
CarSetup:
 Broken: [setup data
...
";

    fn sample() -> SessionInfo {
        parse(SAMPLE.as_bytes(), 72).expect("parses")
    }

    #[test]
    fn extracts_weekend_info() {
        let w = sample().weekend;
        assert_eq!(w.track_id, Some(999));
        assert_eq!(w.track_display_name.as_deref(), Some("Synthetic Speedway"));
        assert_eq!(w.track_length_km, Some(2.5));
        assert_eq!(w.official, Some(false));
        assert_eq!(w.num_car_classes, Some(2));
        assert_eq!(w.series_id, None);
    }

    #[test]
    fn extracts_drivers_by_car_idx() {
        let d = sample().driver_info;
        assert_eq!(d.drivers.len(), 72);
        assert_eq!(d.player_car_idx, Some(1));
        assert_eq!(d.fuel_max_liters, Some(50.0));
        assert_eq!(d.dropped, 2, "CarIdx 90 and the entry without CarIdx");
        let pace = d.drivers[0].as_ref().expect("pace car");
        assert!(pace.is_pace_car);
        assert_eq!(pace.car_class_color, Some(0xFF_FFFF));
        let ana = d.drivers[1].as_ref().expect("driver");
        assert_eq!(
            ana.user_name.as_deref(),
            Some("*Ana: Test #1"),
            "free text with YAML indicators"
        );
        assert_eq!(
            ana.abbrev_name.as_deref(),
            Some(", Ana"),
            "value starting with a comma"
        );
        assert_eq!(
            ana.initials.as_deref(),
            Some("AT"),
            "already quoted by the sim"
        );
        assert_eq!(
            ana.team_name.as_deref(),
            Some("Team \\ Back"),
            "backslash kept"
        );
        assert_eq!(ana.car_number.as_deref(), Some("07"), "leading zero kept");
        assert_eq!(ana.car_class_color, Some(0xFF_DA59));
        assert_eq!(ana.license_color, Some(0x00_C702));
        assert_eq!(ana.license.as_deref(), Some("B 3.21"));
        assert_eq!(
            (ana.irating, ana.user_id, ana.incidents),
            (Some(1500), Some(101), Some(4))
        );
        assert!(!ana.is_ai && !ana.is_pace_car);
        assert!(d.drivers[2].is_none());
    }

    #[test]
    fn extracts_sessions_and_results() {
        let s = sample().sessions;
        assert_eq!(s.len(), 2);
        assert!(s[0].unlimited_laps && s[0].laps.is_none());
        assert_eq!(s[0].time_seconds, Some(600.0));
        assert!(s[0].results.is_empty(), "empty results list");
        assert_eq!(s[1].laps, Some(20));
        assert!(s[1].unlimited_time && s[1].time_seconds.is_none());
        let p1 = &s[1].results[0];
        assert_eq!(
            (p1.position, p1.car_idx, p1.lap),
            (Some(1), Some(1), Some(3))
        );
        assert_eq!((p1.time, p1.fastest_time), (Some(-1.0), Some(31.25)));
        assert_eq!(p1.reason_out.as_deref(), Some("Running"));
        assert_eq!(p1.laps_led, None);
    }

    #[test]
    fn extracts_sectors_and_skips_unused_sections() {
        // CameraInfo, RadioInfo, and CarSetup hold invalid YAML; they are dropped before parsing.
        let sectors = sample().sectors;
        assert_eq!(sectors.len(), 2);
        assert_eq!((sectors[1].num, sectors[1].start_pct), (Some(1), Some(0.5)));
    }

    #[test]
    fn decodes_windows_1252_and_utf8() {
        let raw = b"WeekendInfo:\n Encoding: ISO_8859_1\n TrackName: Jos\xe9 \x80 \x81\n";
        assert_eq!(
            decode(raw),
            "WeekendInfo:\n Encoding: ISO_8859_1\n TrackName: Jos\u{e9} \u{20ac} \u{fffd}\n"
        );
        let utf8 = "---\nWeekendInfo:\n Encoding: UTF8\n TrackName: Jos\u{e9}\n";
        assert_eq!(decode(utf8.as_bytes()), utf8);
        let parsed = parse(b"WeekendInfo:\n TrackName: Caf\xe9 Ring\n", 0).expect("parses");
        assert_eq!(parsed.weekend.track_name.as_deref(), Some("Caf\u{e9} Ring"));
    }

    #[test]
    fn missing_sections_are_empty() {
        let info = parse(b"---\nWeekendInfo:\n TrackID: 5\n...\n", 4).expect("parses");
        assert_eq!(info.weekend.track_id, Some(5));
        assert_eq!(info.driver_info.drivers, vec![None; 4]);
        assert!(info.sessions.is_empty() && info.sectors.is_empty());
    }

    #[test]
    fn reports_position_without_text() {
        let err = parse(b"DriverInfo:\n Drivers:\n - CarIdx: [1\n", 4).expect_err("invalid");
        assert!(matches!(err, SessionError::Syntax { .. }));
        assert_eq!(parse(b"", 4).expect_err("empty"), SessionError::Empty);
    }

    #[test]
    fn ignores_values_nested_under_other_keys() {
        let text = b"DriverInfo:\n Drivers:\n - CarIdx: 0\n   Extra:\n    UserName: Nested\n    CarIdx: 3\n   \
                     Flags: [1, {CarIdx: 2}]\n   UserName: Outer\n";
        let d = parse(text, 4).expect("parses").driver_info;
        let outer = d.drivers[0].as_ref().expect("driver 0");
        assert_eq!(outer.user_name.as_deref(), Some("Outer"));
        assert!(d.drivers[2].is_none() && d.drivers[3].is_none());
    }

    #[test]
    fn reads_the_first_document() {
        let text = b"---\nWeekendInfo:\n TrackID: 5\n...\n---\nWeekendInfo:\n TrackID: 6\n...\n";
        assert_eq!(parse(text, 0).expect("parses").weekend.track_id, Some(5));
    }

    #[test]
    fn strips_control_characters() {
        let info = parse(b"WeekendInfo:\n TrackName: A\x07B\n", 0).expect("parses");
        assert_eq!(info.weekend.track_name.as_deref(), Some("AB"));
    }
}
