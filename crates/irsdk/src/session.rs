//! Extraction of the session info sections the overlay uses from the sim's session YAML.
//!
//! The sim's text is not strict YAML: it is Windows-1252 unless `WeekendInfo` declares `Encoding: UTF8`, and
//! free-text fields such as driver and team names are written unquoted. [`parse`] therefore decodes the text,
//! keeps only the `WeekendInfo`, `SessionInfo`, `DriverInfo`, and `SplitTimeInfo` sections, quotes free-text
//! values, and then parses the result with a YAML parser. Missing or unexpected fields become `None` rather
//! than failing the whole extraction.
//!
//! Call it off the data thread, only when `sessionInfoUpdate` changes. Errors carry a position only, never
//! session text, because the text contains other drivers' names and IDs.

use std::fmt;

use saphyr::{LoadableYamlNode, Yaml};

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
    let docs = Yaml::load_from_str(&text).map_err(|e| SessionError::Syntax {
        line: e.marker().line(),
        col: e.marker().col(),
    })?;
    let doc = docs.first().ok_or(SessionError::Empty)?;
    Ok(SessionInfo {
        weekend: doc
            .as_mapping_get("WeekendInfo")
            .map(weekend_info)
            .unwrap_or_default(),
        driver_info: driver_info(doc.as_mapping_get("DriverInfo"), max_cars),
        sessions: seq(doc.as_mapping_get("SessionInfo"), "Sessions")
            .map(session)
            .collect(),
        sectors: seq(doc.as_mapping_get("SplitTimeInfo"), "Sectors")
            .map(|s| Sector {
                num: int(s, "SectorNum"),
                start_pct: float(s, "SectorStartPct"),
            })
            .collect(),
    })
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

fn weekend_info(w: &Yaml) -> WeekendInfo {
    WeekendInfo {
        track_id: int(w, "TrackID"),
        track_name: text(w, "TrackName"),
        track_display_name: text(w, "TrackDisplayName"),
        track_length_km: float(w, "TrackLength"),
        category: text(w, "Category"),
        event_type: text(w, "EventType"),
        official: int(w, "Official").map(|v| v != 0),
        num_car_classes: int(w, "NumCarClasses"),
        series_id: int(w, "SeriesID"),
        session_id: int(w, "SessionID"),
        sub_session_id: int(w, "SubSessionID"),
    }
}

fn driver_info(d: Option<&Yaml>, max_cars: usize) -> DriverInfo {
    let mut info = DriverInfo {
        drivers: vec![None; max_cars],
        ..DriverInfo::default()
    };
    let Some(d) = d else {
        return info;
    };
    info.player_car_idx = index(d, "DriverCarIdx");
    info.pace_car_idx = index(d, "PaceCarIdx");
    info.fuel_max_liters = float(d, "DriverCarFuelMaxLtr");
    info.max_fuel_pct = float(d, "DriverCarMaxFuelPct");
    info.est_lap_time = float(d, "DriverCarEstLapTime");
    for entry in seq(Some(d), "Drivers") {
        let slot = index(entry, "CarIdx")
            .and_then(|car_idx| Some((car_idx, info.drivers.get_mut(car_idx)?)));
        let Some((car_idx, slot)) = slot else {
            info.dropped += 1;
            continue;
        };
        *slot = Some(Driver {
            car_idx,
            user_name: text(entry, "UserName"),
            abbrev_name: text(entry, "AbbrevName"),
            initials: text(entry, "Initials"),
            user_id: int(entry, "UserID"),
            team_name: text(entry, "TeamName"),
            car_number: text(entry, "CarNumber"),
            car_screen_name_short: text(entry, "CarScreenNameShort"),
            car_class_id: int(entry, "CarClassID"),
            car_class_short_name: text(entry, "CarClassShortName"),
            car_class_rel_speed: int(entry, "CarClassRelSpeed"),
            car_class_est_lap_time: float(entry, "CarClassEstLapTime"),
            car_class_color: color(entry, "CarClassColor"),
            irating: int(entry, "IRating"),
            license: text(entry, "LicString"),
            license_color: color(entry, "LicColor"),
            incidents: int(entry, "CurDriverIncidentCount"),
            is_spectator: flag(entry, "IsSpectator"),
            is_pace_car: flag(entry, "CarIsPaceCar"),
            is_ai: flag(entry, "CarIsAI"),
        });
    }
    info
}

fn session(s: &Yaml) -> Session {
    let laps = text(s, "SessionLaps");
    let time = text(s, "SessionTime");
    Session {
        num: int(s, "SessionNum"),
        session_type: text(s, "SessionType"),
        name: text(s, "SessionName"),
        laps: int(s, "SessionLaps"),
        unlimited_laps: laps.as_deref() == Some("unlimited"),
        time_seconds: float(s, "SessionTime"),
        unlimited_time: time.as_deref() == Some("unlimited"),
        results: seq(Some(s), "ResultsPositions")
            .map(|r| ResultPosition {
                position: int(r, "Position"),
                class_position: int(r, "ClassPosition"),
                car_idx: index(r, "CarIdx"),
                lap: int(r, "Lap"),
                time: float(r, "Time"),
                fastest_lap: int(r, "FastestLap"),
                fastest_time: float(r, "FastestTime"),
                last_time: float(r, "LastTime"),
                laps_led: int(r, "LapsLed"),
                laps_complete: int(r, "LapsComplete"),
                incidents: int(r, "Incidents"),
                reason_out: text(r, "ReasonOutStr"),
            })
            .collect(),
    }
}

fn seq<'a, 'i>(parent: Option<&'a Yaml<'i>>, key: &str) -> impl Iterator<Item = &'a Yaml<'i>> {
    parent
        .and_then(|p| p.as_mapping_get(key))
        .and_then(Yaml::as_sequence)
        .into_iter()
        .flatten()
}

fn text(y: &Yaml, key: &str) -> Option<String> {
    let v = y.as_mapping_get(key)?;
    if let Some(s) = v.as_str() {
        return Some(s.to_owned());
    }
    v.as_integer()
        .map(|i| i.to_string())
        .or_else(|| v.as_floating_point().map(|f| f.to_string()))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
}

fn int(y: &Yaml, key: &str) -> Option<i64> {
    let v = y.as_mapping_get(key)?;
    v.as_integer().or_else(|| v.as_str()?.trim().parse().ok())
}

fn index(y: &Yaml, key: &str) -> Option<usize> {
    int(y, key).and_then(|i| usize::try_from(i).ok())
}

/// A number, or a string that starts with one, such as `5.93 km` or `600.0000 sec`.
fn float(y: &Yaml, key: &str) -> Option<f64> {
    let v = y.as_mapping_get(key)?;
    v.as_floating_point()
        .or_else(|| v.as_integer().map(|i| i as f64))
        .or_else(|| v.as_str()?.split_whitespace().next()?.parse().ok())
}

fn flag(y: &Yaml, key: &str) -> bool {
    int(y, key).is_some_and(|v| v != 0)
}

/// `0xRRGGBB`, written by the sim as a plain scalar that YAML may already read as a hex integer.
fn color(y: &Yaml, key: &str) -> Option<u32> {
    let v = y.as_mapping_get(key)?;
    if let Some(i) = v.as_integer() {
        return u32::try_from(i).ok();
    }
    let s = v.as_str()?.trim();
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    u32::from_str_radix(hex, 16).ok()
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
    fn strips_control_characters() {
        let info = parse(b"WeekendInfo:\n TrackName: A\x07B\n", 0).expect("parses");
        assert_eq!(info.weekend.track_name.as_deref(), Some("AB"));
    }
}
