//! Fixture anonymizer: replaces driver names, team names, customer and team IDs, and session IDs in session info
//! with consistent pseudonyms, and rewrites captures and `.ibt` files with the anonymized text. Telemetry values
//! are copied unchanged.
//!
//! The sim's session info is not strict YAML (free text is unquoted), so a YAML round trip would change it. The
//! text is rewritten line by line instead: values of the keys below are replaced, and every other byte is kept.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use pitot_irsdk::layout::{DiskSubHeader, Header, VarHeader};

use crate::capture::{CaptureSource, CaptureWriter, Counts, Record, SourceInfo, WriterOptions};
use crate::replay::{FrameSource, ReplayError};

/// Keys whose values name a person or a team.
const NAME_KEYS: &[&[u8]] = &[b"UserName", b"AbbrevName", b"Initials", b"TeamName"];
/// Keys whose values identify a customer or a team.
const ID_KEYS: &[&[u8]] = &[b"UserID", b"TeamID", b"DriverUserID", b"CustID"];
/// Keys whose values identify a session, through which results with names can be looked up.
const SESSION_KEYS: &[&[u8]] = &[b"SessionID", b"SubSessionID", b"LeagueID"];

/// Pseudonyms kept over every session info revision of a file, so the same driver gets the same name each time.
#[derive(Debug, Default)]
pub struct Pseudonyms {
    /// Original user name to driver number.
    drivers: HashMap<Vec<u8>, usize>,
    teams: HashMap<Vec<u8>, usize>,
    ids: HashMap<i64, i64>,
    sessions: HashMap<i64, i64>,
    /// Originals that must not appear in the output, longest first when replacing.
    originals: Vec<(Vec<u8>, Vec<u8>)>,
}

/// Counts of what was replaced, without the values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Replaced {
    pub drivers: usize,
    pub teams: usize,
    pub ids: usize,
    pub sessions: usize,
}

impl Pseudonyms {
    pub fn replaced(&self) -> Replaced {
        Replaced {
            drivers: self.drivers.len(),
            teams: self.teams.len(),
            ids: self.ids.len(),
            sessions: self.sessions.len(),
        }
    }

    fn driver(&mut self, name: &[u8]) -> usize {
        let next = self.drivers.len() + 1;
        let n = *self.drivers.entry(name.to_vec()).or_insert(next);
        if n == next {
            self.remember(name, format!("Driver {n:03}").into_bytes());
        }
        n
    }

    fn team(&mut self, name: &[u8]) -> usize {
        let next = self.teams.len() + 1;
        let n = *self.teams.entry(name.to_vec()).or_insert(next);
        if n == next {
            self.remember(name, format!("Team {n:03}").into_bytes());
        }
        n
    }

    fn remember(&mut self, original: &[u8], pseudonym: Vec<u8>) {
        // Short values such as initials are too common to replace inside other text.
        if original.len() >= 3 && !self.originals.iter().any(|(o, _)| o == original) {
            self.originals.push((original.to_vec(), pseudonym));
            self.originals
                .sort_by_key(|(original, _)| std::cmp::Reverse(original.len()));
        }
    }

    fn id(&mut self, id: i64) -> i64 {
        if id <= 0 {
            return id;
        }
        let next = 100_000 + self.ids.len() as i64 + 1;
        *self.ids.entry(id).or_insert(next)
    }

    fn session(&mut self, id: i64) -> i64 {
        if id <= 0 {
            return id;
        }
        let next = self.sessions.len() as i64 + 1;
        *self.sessions.entry(id).or_insert(next)
    }
}

/// A line split into indentation (with any `- ` list marker), key, and value.
struct Line<'a> {
    lead: &'a [u8],
    key: &'a [u8],
    value: &'a [u8],
    end: &'a [u8],
}

fn split_line(line: &[u8]) -> Option<Line<'_>> {
    let body_end = line
        .iter()
        .rposition(|&b| b != b'\n' && b != b'\r')
        .map_or(0, |i| i + 1);
    let (body, end) = line.split_at(body_end);
    let mut start = body.iter().take_while(|&&b| b == b' ').count();
    if body[start..].starts_with(b"- ") {
        start += 2;
    }
    let colon = start + body[start..].iter().position(|&b| b == b':')?;
    let key = &body[start..colon];
    if key.is_empty() || !key.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_') {
        return None;
    }
    let value = body.get(colon + 1..).unwrap_or_default();
    let value = value.strip_prefix(b" ").unwrap_or(value);
    Some(Line {
        lead: &body[..start],
        key,
        value,
        end,
    })
}

fn lines(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    text.split_inclusive(|&b| b == b'\n')
}

fn parse_id(value: &[u8]) -> Option<i64> {
    std::str::from_utf8(value).ok()?.trim().parse().ok()
}

/// Driver entries of `DriverInfo`, as the fields the pseudonyms need.
#[derive(Default)]
struct Entry {
    user_name: Option<Vec<u8>>,
    team_name: Option<Vec<u8>>,
    pace_car: bool,
}

fn entries(text: &[u8]) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in lines(text) {
        let Some(line) = split_line(line) else {
            continue;
        };
        if line.key == b"CarIdx" && line.lead.ends_with(b"- ") {
            out.push(Entry::default());
        }
        let Some(entry) = out.last_mut() else {
            continue;
        };
        match line.key {
            b"UserName" => entry.user_name = Some(line.value.to_vec()),
            b"TeamName" => entry.team_name = Some(line.value.to_vec()),
            b"CarIsPaceCar" => entry.pace_car = line.value == b"1",
            _ => {}
        }
    }
    out
}

/// Anonymizes one session info revision.
pub fn session_info(text: &[u8], names: &mut Pseudonyms) -> Vec<u8> {
    // Pace cars keep their name: it is not a person, and widgets recognize it.
    let mut keep: Vec<Vec<u8>> = Vec::new();
    for entry in entries(text) {
        if entry.pace_car {
            keep.extend(entry.user_name);
            keep.extend(entry.team_name);
            continue;
        }
        if let Some(name) = &entry.user_name {
            names.driver(name);
        }
        if let Some(team) = &entry.team_name {
            names.team(team);
        }
    }

    let mut out = Vec::with_capacity(text.len());
    let mut driver = None;
    for raw in lines(text) {
        let Some(line) = split_line(raw) else {
            out.extend_from_slice(raw);
            continue;
        };
        if line.key == b"CarIdx" && line.lead.ends_with(b"- ") {
            driver = None;
        }
        if line.key == b"UserName" {
            driver = (!keep.iter().any(|k| k == line.value)).then(|| names.driver(line.value));
        }
        let value: Option<Vec<u8>> = if keep.iter().any(|k| k == line.value) {
            None
        } else if line.key == b"UserName" {
            driver.map(|n| format!("Driver {n:03}").into_bytes())
        } else if line.key == b"TeamName" {
            Some(format!("Team {:03}", names.team(line.value)).into_bytes())
        } else if line.key == b"AbbrevName" {
            driver.map(|n| format!("D{n:03}").into_bytes())
        } else if line.key == b"Initials" {
            driver.map(|_| b"DR".to_vec())
        } else if NAME_KEYS.contains(&line.key) {
            Some(b"Anonymous".to_vec())
        } else if ID_KEYS.contains(&line.key) {
            parse_id(line.value).map(|id| names.id(id).to_string().into_bytes())
        } else if SESSION_KEYS.contains(&line.key) {
            parse_id(line.value).map(|id| names.session(id).to_string().into_bytes())
        } else if line.key == b"DriverSetupName" && !line.value.is_empty() {
            Some(b"setup.sto".to_vec())
        } else {
            None
        };
        match value {
            Some(value) => {
                out.extend_from_slice(line.lead);
                out.extend_from_slice(line.key);
                out.extend_from_slice(b": ");
                out.extend_from_slice(&value);
                out.extend_from_slice(line.end);
            }
            None => out.extend_from_slice(raw),
        }
    }
    // Names can also appear in free text, such as private radio channel names.
    for (original, pseudonym) in &names.originals {
        out = replace_all(&out, original, pseudonym);
    }
    out
}

fn replace_all(text: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.windows(from.len()).position(|w| w == from) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(to);
        rest = &rest[at + from.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// How many original names still appear in `text`, plus ID values that are not pseudonyms; zero after
/// anonymizing.
pub fn remaining(text: &[u8], names: &Pseudonyms) -> usize {
    let found = names
        .originals
        .iter()
        .filter(|(o, _)| text.windows(o.len()).any(|w| w == o.as_slice()))
        .count();
    let ids = lines(text)
        .filter_map(split_line)
        .filter(|line| {
            let Some(id) = parse_id(line.value) else {
                return false;
            };
            let mapped = |map: &HashMap<i64, i64>| id <= 0 || map.values().any(|&v| v == id);
            (ID_KEYS.contains(&line.key) && !mapped(&names.ids))
                || (SESSION_KEYS.contains(&line.key) && !mapped(&names.sessions))
        })
        .count();
    found + ids
}

#[derive(Debug)]
pub enum AnonymizeError {
    Io(io::Error),
    Replay(ReplayError),
    Format(&'static str),
}

impl std::fmt::Display for AnonymizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Replay(e) => write!(f, "{e}"),
            Self::Format(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for AnonymizeError {}

impl From<io::Error> for AnonymizeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ReplayError> for AnonymizeError {
    fn from(e: ReplayError) -> Self {
        Self::Replay(e)
    }
}

/// Rewrites a capture with anonymized session info; frames, gaps, and disconnects are copied with their times.
pub fn capture(
    input: &Path,
    output: &Path,
    level: i32,
) -> Result<(Counts, Replaced, usize), AnonymizeError> {
    let mut source = CaptureSource::open(input)?;
    // Chunks as long as the source's, so the output differs from it only in session info.
    let tick_rate = source.layout().tick_rate.max(1) as u32;
    let options = WriterOptions {
        level,
        chunk_seconds: (source.chunk_frames() / tick_rate).max(1),
        clock_hz: source.clock_hz(),
        sdk_version: source.sdk_version(),
        source: SourceInfo {
            kind: "anonymized".to_owned(),
            file: None,
            bytes: None,
        },
        tool: "pitot-bench",
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    let layout = source.layout().clone();
    let mut writer = CaptureWriter::new(BufWriter::new(File::create(output)?), &layout, options)?;
    let mut names = Pseudonyms::default();
    let mut buf = Vec::new();
    let mut end = 0;
    while let Some(record) = source.read(&mut buf)? {
        match record {
            Record::Frame { time, tick, buffer } => writer.frame(time, tick, buffer, &buf)?,
            // The writer carries the revision in effect into every chunk itself.
            Record::SessionInfo { carried: true, .. } => {}
            Record::SessionInfo { time, update, .. } => {
                let text = session_info(&buf, &mut names);
                writer.session_info(time, update, &text)?;
            }
            Record::Gap { time, missed } => writer.gap(time, missed)?,
            Record::Disconnect { time } => writer.disconnect(time)?,
            Record::End { time } => end = time,
        }
    }
    let counts = writer.finish(end)?;
    let revisions = verify(&mut CaptureSource::open(output)?, &names)?;
    Ok((counts, names.replaced(), revisions))
}

/// Rewrites an `.ibt` file with anonymized session info. Expects the sim's layout: headers, variable headers,
/// session info, then the records to the end of the file.
pub fn ibt(input: &Path, output: &Path) -> Result<(Replaced, usize), AnonymizeError> {
    let mut file = BufReader::new(File::open(input)?);
    let len = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;
    let mut head = [0u8; Header::SIZE + DiskSubHeader::SIZE];
    file.read_exact(&mut head)?;
    let mut header = Header::read(&head[..Header::SIZE])
        .map_err(|_| AnonymizeError::Format("not an .ibt file"))?;
    let vars_end =
        header.var_header_offset as i64 + i64::from(header.num_vars) * VarHeader::SIZE as i64;
    let info = (
        header.session_info_offset as i64,
        header.session_info_len as i64,
    );
    let data = header.var_buf[0].buf_offset as i64;
    let expected = header.var_header_offset as usize == head.len()
        && info.0 == vars_end
        && info.0 + info.1 == data
        && data as u64 <= len;
    if !expected {
        return Err(AnonymizeError::Format("unexpected .ibt layout"));
    }
    let mut prefix = vec![0u8; info.0 as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut prefix)?;
    let mut text = vec![0u8; info.1 as usize];
    file.read_exact(&mut text)?;
    let trimmed = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    let mut names = Pseudonyms::default();
    let mut fresh = session_info(&text[..trimmed], &mut names);
    if trimmed < text.len() {
        fresh.push(0);
    }
    header.session_info_len = fresh.len() as i32;
    header.var_buf[0].buf_offset = (info.0 as usize + fresh.len()) as i32;
    header
        .write(&mut prefix[..Header::SIZE])
        .map_err(|_| AnonymizeError::Format("cannot write the .ibt header"))?;
    let mut out = BufWriter::new(File::create(output)?);
    out.write_all(&prefix)?;
    out.write_all(&fresh)?;
    io::copy(&mut file, &mut out)?;
    out.flush()?;
    drop(out);
    let revisions = verify(&mut crate::replay::IbtSource::open(output)?, &names)?;
    Ok((names.replaced(), revisions))
}

/// Reads a written file the way a replay does and fails if a session info revision still holds an original.
fn verify(source: &mut impl FrameSource, names: &Pseudonyms) -> Result<usize, AnonymizeError> {
    let mut buf = Vec::new();
    let mut revisions = 0;
    while let Some(item) = source.next(&mut buf)? {
        if item == crate::replay::Item::SessionInfo {
            if remaining(&buf, names) > 0 {
                return Err(AnonymizeError::Format("an original name is still present"));
            }
            revisions += 1;
        }
    }
    Ok(revisions)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made-up session info in the sim's layout, with unquoted free text.
    const TEXT: &[u8] = b"---
WeekendInfo:
 TrackName: synthetic
 SeriesID: 228
 SessionID: 81234567
 SubSessionID: 70123456
 LeagueID: 0
DriverInfo:
 DriverCarIdx: 1
 DriverUserID: 424242
 DriverSetupName: Jane Q Racer baseline.sto
 Drivers:
 - CarIdx: 0
   UserName: Pace Car
   AbbrevName: 
   Initials: 
   UserID: -1
   TeamID: 0
   TeamName: Pace Car
   CarNumber: \"0\"
   CarIsPaceCar: 1
 - CarIdx: 1
   UserName: Jane Q Racer
   AbbrevName: Racer, J
   Initials: JR
   UserID: 424242
   TeamID: 0
   TeamName: Jane Q Racer
   CarNumber: \"7\"
   IRating: 2345
   CarIsPaceCar: 0
 - CarIdx: 2
   UserName: Max *Mustermann
   AbbrevName: Mustermann, M
   Initials: MM
   UserID: 515151
   TeamID: 0
   TeamName: Max *Mustermann
   CarNumber: \"12\"
   IRating: 1999
   CarIsPaceCar: 0
RadioInfo:
 Radios:
 - RadioNum: 0
   Frequencies:
   - FrequencyNum: 0
     FrequencyName: \"@ALLTEAMS\"
   - FrequencyNum: 1
     FrequencyName: \"@Max *Mustermann\"
...
";

    #[test]
    fn replaces_people_and_keeps_everything_else() {
        let mut names = Pseudonyms::default();
        let out = session_info(TEXT, &mut names);
        let text = String::from_utf8(out.clone()).expect("ASCII");
        for original in [
            "Jane Q Racer",
            "Mustermann",
            "424242",
            "515151",
            "81234567",
            "70123456",
            "Racer, J",
        ] {
            assert!(!text.contains(original), "{original} remains:\n{text}");
        }
        for kept in [
            "   UserName: Pace Car\n",
            "   TeamName: Pace Car\n",
            " SeriesID: 228\n",
            " LeagueID: 0\n",
            "   UserID: -1\n",
            "   IRating: 2345\n",
            "   CarNumber: \"12\"\n",
            "     FrequencyName: \"@ALLTEAMS\"\n",
        ] {
            assert!(text.contains(kept), "{kept:?} changed:\n{text}");
        }
        assert!(text.contains("   UserName: Driver 001\n   AbbrevName: D001\n   Initials: DR\n"));
        assert!(text.contains("   UserName: Driver 002\n"));
        assert!(
            text.contains(" DriverUserID: 100001\n"),
            "same ID as the driver's UserID"
        );
        assert!(text.contains("   UserID: 100001\n"));
        assert!(text.contains("   TeamName: Team 002\n"));
        assert!(text.contains(" SubSessionID: 2\n"));
        assert!(text.contains(" DriverSetupName: setup.sto\n"));
        assert!(
            text.contains("FrequencyName: \"@Driver 002\""),
            "names in free text"
        );
        assert_eq!(remaining(&out, &names), 0);
        assert_eq!(
            names.replaced(),
            Replaced {
                drivers: 2,
                teams: 2,
                ids: 2,
                sessions: 2
            }
        );
        // Line structure is kept: same number of lines.
        assert_eq!(
            text.lines().count(),
            TEXT.split(|&b| b == b'\n').count() - 1
        );
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pitot-anonymize-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn ibt_files_keep_their_records() {
        let dir = temp("ibt");
        let (input, output) = (dir.join("in.ibt"), dir.join("out.ibt"));
        std::fs::write(&input, crate::replay::ibt_file(TEXT)).expect("write");
        let (replaced, revisions) = ibt(&input, &output).expect("anonymizes");
        assert_eq!((replaced.drivers, replaced.ids, revisions), (2, 2, 1));
        let mut before = pitot_irsdk::ibt::IbtReader::open(&input).expect("opens");
        let mut after = pitot_irsdk::ibt::IbtReader::open(&output).expect("opens");
        let text = String::from_utf8_lossy(after.session_info()).into_owned();
        assert!(text.contains("UserName: Driver 001") && !text.contains("Jane"));
        assert_eq!(after.vars(), before.vars());
        assert_eq!(after.record_count(), before.record_count());
        let (mut a, mut b) = (before.new_record(), after.new_record());
        while before.next_record(&mut a).expect("reads") {
            assert!(after.next_record(&mut b).expect("reads"));
            assert_eq!(a.bytes(), b.bytes());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn captures_keep_their_frames_and_times() {
        use crate::replay::SourceLayout;
        use pitot_irsdk::layout::VarType;
        let dir = temp("ptc");
        let (input, output) = (dir.join("in.ptc"), dir.join("out.ptc"));
        let layout = SourceLayout {
            vars: vec![VarHeader::new(VarType::Int, 0, 1, "SessionTick").expect("valid")],
            buf_len: 4,
            tick_rate: 60,
            session_info_capacity: 4096,
            player_car_only: false,
        };
        let options = WriterOptions {
            level: 3,
            chunk_seconds: 1,
            clock_hz: 60,
            sdk_version: 2,
            source: SourceInfo {
                kind: "test".into(),
                file: None,
                bytes: None,
            },
            tool: "pitot-bench",
            tool_version: env!("CARGO_PKG_VERSION"),
        };
        let mut writer = CaptureWriter::new(
            BufWriter::new(File::create(&input).expect("create")),
            &layout,
            options,
        )
        .expect("header");
        writer.session_info(0, 1, TEXT).expect("info");
        for n in 0..150u32 {
            if n == 100 {
                writer.session_info(u64::from(n), 2, TEXT).expect("info");
            }
            writer
                .frame(u64::from(n), n as i32, 0, &n.to_le_bytes())
                .expect("frame");
        }
        writer.finish(150).expect("finish");

        let (counts, replaced, revisions) = capture(&input, &output, 3).expect("anonymizes");
        assert_eq!((counts.frames, counts.session_info_revisions), (150, 2));
        assert_eq!((replaced.drivers, revisions), (2, 2));
        let mut before = CaptureSource::open(&input).expect("opens");
        let mut after = CaptureSource::open(&output).expect("opens");
        let (mut a, mut b) = (Vec::new(), Vec::new());
        loop {
            let (left, right) = (
                before.read(&mut a).expect("reads"),
                after.read(&mut b).expect("reads"),
            );
            assert_eq!(left, right, "same records at the same times");
            match left {
                None => break,
                Some(Record::SessionInfo { .. }) => {
                    let text = String::from_utf8_lossy(&b);
                    assert!(text.contains("Driver 002"), "{text}");
                    for original in ["Jane", "Mustermann", "424242", "70123456"] {
                        assert!(!text.contains(original), "{original} remains");
                    }
                }
                Some(_) => assert_eq!(a, b),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pseudonyms_stay_the_same_over_revisions() {
        let mut names = Pseudonyms::default();
        let first = session_info(TEXT, &mut names);
        // A later revision lists the drivers the other way around.
        let swapped = String::from_utf8_lossy(TEXT)
            .replace("CarIdx: 1\n", "CarIdx: 9\n")
            .into_bytes();
        let second = session_info(&swapped, &mut names);
        let find = |text: &[u8], n: &str| String::from_utf8_lossy(text).contains(n);
        assert!(find(&first, "Driver 001") && find(&second, "Driver 001"));
        assert_eq!(names.replaced().drivers, 2, "no new pseudonyms");
        assert!(
            remaining(TEXT, &names) > 0,
            "the original still has the names"
        );
    }
}
