//! Tire surface temperatures for replays, from the sim's own telemetry files.
//!
//! Live telemetry has no surface temperatures or hot pressures; the sim writes them only to `.ibt` files, which
//! it records while its telemetry recording is on (one file per stint in the car). While the sim plays a replay,
//! the session thread finds the files of the live session, reads five samples a second of each tire's surface
//! temperatures, tread, and pressure, and hands them to the render thread as immutable segments. A file the sim
//! is still writing is read on from where the last search left it.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use pitot_irsdk::capture::Moment;
use pitot_irsdk::ibt::IbtReader;
use pitot_irsdk::layout::Field;
use pitot_irsdk::session::SessionParser;

use super::replay::SessionIds;

/// Left front, right front, left rear, right rear.
pub const CORNERS: usize = 4;
/// Left, middle, and right of a tire.
pub const ZONES: usize = 3;
const NAMES: [&str; CORNERS] = ["LF", "RF", "LR", "RR"];
const ZONE_NAMES: [&str; ZONES] = ["L", "M", "R"];

/// Samples kept per second of a file.
const SAMPLE_HZ: i32 = 5;
/// A moment this long after a sample still shows it. Longer gaps are time out of the car between stints.
const MAX_GAP: f64 = 0.5;

const NO_TEMP: i16 = i16::MIN;
const NO_VALUE: u16 = u16::MAX;

/// One moment of the player's tires.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TireSample {
    session: i32,
    time: f64,
    /// Surface temperatures in tenths of °C, by corner and zone.
    temps: [i16; CORNERS * ZONES],
    /// Tread remaining in ten-thousandths, by corner and zone.
    wear: [u16; CORNERS * ZONES],
    /// Hot pressure in tenths of kPa, by corner.
    pressure: [u16; CORNERS],
}

impl TireSample {
    /// Surface temperature in °C of `zone` (left, middle, right) of `corner`.
    pub fn temp(&self, corner: usize, zone: usize) -> Option<f32> {
        let v = *self.temps.get(corner * ZONES + zone)?;
        (v != NO_TEMP).then(|| f32::from(v) / 10.0)
    }

    /// Tread remaining, 0 to 1.
    pub fn wear(&self, corner: usize, zone: usize) -> Option<f32> {
        let v = *self.wear.get(corner * ZONES + zone)?;
        (v != NO_VALUE).then(|| f32::from(v) / 10_000.0)
    }

    /// Hot pressure in kPa.
    pub fn pressure(&self, corner: usize) -> Option<f32> {
        let v = *self.pressure.get(corner)?;
        (v != NO_VALUE).then(|| f32::from(v) / 10.0)
    }

    fn at(&self) -> (i32, f64) {
        (self.session, self.time)
    }
}

fn quantize(value: Option<f32>, scale: f32, max: f32, none: u16) -> u16 {
    value
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map_or(none, |v| (v * scale).round().min(max) as u16)
}

/// Consecutive samples of one file, in time order. Never empty.
#[derive(Debug, PartialEq)]
pub struct Segment {
    samples: Vec<TireSample>,
}

impl Segment {
    fn first(&self) -> (i32, f64) {
        self.samples[0].at()
    }

    fn last(&self) -> (i32, f64) {
        self.samples[self.samples.len() - 1].at()
    }
}

/// The session's tire samples, as segments in time order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TireTrack {
    segments: Vec<Arc<Segment>>,
}

impl TireTrack {
    /// The sample shown at `at`: the last one at or before it, unless the car was out of it since.
    pub fn sample_at(&self, at: Moment) -> Option<&TireSample> {
        let key = (at.session, at.time);
        let not_after = |m: (i32, f64)| order(m, key) != Ordering::Greater;
        let n = self.segments.partition_point(|s| not_after(s.first()));
        let segment = self.segments.get(n.checked_sub(1)?)?;
        let i = segment.samples.partition_point(|s| not_after(s.at()));
        let sample = segment.samples.get(i.checked_sub(1)?)?;
        (sample.session == at.session && at.time - sample.time <= MAX_GAP).then_some(sample)
    }

    /// Whether `at` lies within what the files held when they were read; outside it, a new search may find more.
    pub fn covers(&self, at: Moment) -> bool {
        let (Some(first), Some(last)) = (self.segments.first(), self.segments.last()) else {
            return false;
        };
        let (start, end) = (first.first(), last.last());
        let key = (at.session, at.time);
        order(start, key) != Ordering::Greater
            && order(key, (end.0, end.1 + MAX_GAP)) != Ordering::Greater
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

/// What a search learned about one file, kept while the file is unchanged.
#[derive(Debug)]
struct Known {
    modified: SystemTime,
    len: u64,
    /// `None` for a file that cannot be used: unreadable, without session IDs, or without tire temperatures.
    usable: Option<Usable>,
}

#[derive(Debug)]
struct Usable {
    ids: SessionIds,
    first: (i32, f64),
    last: (i32, f64),
    /// Index of the next record to read, and the segments read so far.
    next: usize,
    segments: Vec<Arc<Segment>>,
}

/// The variables read from a file.
struct Columns {
    num: Field,
    time: Field,
    temps: [Option<Field>; CORNERS * ZONES],
    wear: [Option<Field>; CORNERS * ZONES],
    pressure: [Option<Field>; CORNERS],
}

impl Columns {
    fn of<R: std::io::Read + std::io::Seek>(reader: &IbtReader<R>) -> Option<Self> {
        let field = |name: &str| reader.field(name).ok().flatten();
        let mut temps = [None; CORNERS * ZONES];
        let mut wear = [None; CORNERS * ZONES];
        let mut pressure = [None; CORNERS];
        for (c, corner) in NAMES.iter().enumerate() {
            for (z, zone) in ZONE_NAMES.iter().enumerate() {
                temps[c * ZONES + z] = field(&format!("{corner}temp{zone}"));
                wear[c * ZONES + z] = field(&format!("{corner}wear{zone}"));
            }
            pressure[c] = field(&format!("{corner}pressure"));
        }
        if temps.iter().all(Option::is_none) {
            return None;
        }
        Some(Self {
            num: field("SessionNum")?,
            time: field("SessionTime")?,
            temps,
            wear,
            pressure,
        })
    }

    fn moment(&self, row: &[u8]) -> Option<(i32, f64)> {
        Some((self.num.i32(row, 0)?, self.time.f64(row, 0)?))
    }

    fn sample(&self, row: &[u8]) -> Option<TireSample> {
        let (session, time) = self.moment(row)?;
        let read = |f: &Option<Field>| f.as_ref().and_then(|f| f.f32(row, 0));
        Some(TireSample {
            session,
            time,
            temps: std::array::from_fn(|i| {
                read(&self.temps[i])
                    .filter(|t| t.is_finite())
                    .map_or(NO_TEMP, |t| {
                        (t * 10.0).round().clamp(-3276.0, 3276.0) as i16
                    })
            }),
            wear: std::array::from_fn(|i| {
                quantize(read(&self.wear[i]), 10_000.0, 10_000.0, NO_VALUE)
            }),
            pressure: std::array::from_fn(|i| {
                quantize(read(&self.pressure[i]), 10.0, 65_534.0, NO_VALUE)
            }),
        })
    }
}

/// Reads a file's session IDs and first and last moments.
fn inspect(path: &Path, parser: &mut SessionParser) -> Option<Usable> {
    let mut reader = IbtReader::open(path).ok()?;
    let columns = Columns::of(&reader)?;
    let count = reader.record_count();
    if count == 0 {
        return None;
    }
    // The file holds the player's car only; the session info still lists every driver.
    let info = parser.parse(reader.session_info(), 64).ok()?;
    let ids = SessionIds::of(&info);
    let mut record = reader.new_record();
    reader.read_record(0, &mut record).ok()?;
    let first = columns.moment(record.bytes())?;
    reader.read_record(count - 1, &mut record).ok()?;
    let last = columns.moment(record.bytes())?;
    Some(Usable {
        ids,
        first,
        last,
        next: 0,
        segments: Vec::new(),
    })
}

/// Orders moments by session, then time.
fn order(a: (i32, f64), b: (i32, f64)) -> Ordering {
    a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
}

/// Reads the records of `path` from `usable.next` on, [`SAMPLE_HZ`] a second, into a new segment.
fn read_on(path: &Path, usable: &mut Usable) {
    let Ok(mut reader) = IbtReader::open(path) else {
        return;
    };
    let Some(columns) = Columns::of(&reader) else {
        return;
    };
    let stride = (reader.header().tick_rate / SAMPLE_HZ).max(1) as usize;
    let count = reader.record_count();
    let mut record = reader.new_record();
    let mut samples: Vec<TireSample> =
        Vec::with_capacity(count.saturating_sub(usable.next) / stride + 1);
    let mut previous = usable.segments.last().map(|s| s.last());
    let mut i = usable.next;
    while i < count {
        if reader.read_record(i, &mut record).is_err() {
            break;
        }
        // Records are in time order; one that is not (a damaged tail) is left out.
        if let Some(sample) = columns.sample(record.bytes())
            && previous.is_none_or(|p| order(p, sample.at()) == Ordering::Less)
        {
            previous = Some(sample.at());
            samples.push(sample);
        }
        i += stride;
    }
    usable.next = i;
    if let Some(last) = samples.last()
        && order(last.at(), usable.last) == Ordering::Greater
    {
        usable.last = last.at();
    }
    if !samples.is_empty() {
        usable.segments.push(Arc::new(Segment { samples }));
    }
}

/// Finds and reads the telemetry files of a session. Files are read again only when they change.
#[derive(Debug, Default)]
pub struct TireFinder {
    files: HashMap<PathBuf, Known>,
}

impl TireFinder {
    /// The tire samples of the session `want` describes, from the `.ibt` files in `folders`. An online session
    /// takes every file with its `SubSessionID` (one per stint). An offline session, whose `SubSessionID` 0 all
    /// share, takes the newest file of its track that covers `at`. `None` when no file matches.
    pub fn find(
        &mut self,
        folders: &[PathBuf],
        want: SessionIds,
        at: Moment,
        parser: &mut SessionParser,
    ) -> Option<TireTrack> {
        let mut listed: Vec<(SystemTime, u64, PathBuf)> = folders
            .iter()
            .filter_map(|dir| fs::read_dir(dir).ok())
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ibt")))
            .filter_map(|p| {
                let meta = fs::metadata(&p).ok()?;
                Some((meta.modified().ok()?, meta.len(), p))
            })
            .collect();
        listed.sort_by_key(|f| std::cmp::Reverse(f.0));
        self.files
            .retain(|path, _| listed.iter().any(|(_, _, p)| p == path));

        let online = want.sub_session != 0;
        let key = (at.session, at.time);
        let mut chosen: Vec<PathBuf> = Vec::new();
        for (modified, len, path) in listed {
            let known = match self.files.entry(path.clone()) {
                Entry::Occupied(entry) => {
                    let known = entry.into_mut();
                    if known.modified != modified || known.len != len {
                        match known.usable.as_mut() {
                            // Being written: the session stays and the records read so far stay read.
                            Some(usable) if known.len < len => {
                                if let Some(fresh) = inspect(&path, parser) {
                                    usable.last = fresh.last;
                                }
                            }
                            _ => known.usable = inspect(&path, parser),
                        }
                        known.modified = modified;
                        known.len = len;
                    }
                    known
                }
                Entry::Vacant(entry) => entry.insert(Known {
                    modified,
                    len,
                    usable: inspect(&path, parser),
                }),
            };
            let Some(usable) = known.usable.as_ref() else {
                continue;
            };
            let matches = if online {
                usable.ids.sub_session == want.sub_session
            } else {
                usable.ids.sub_session == 0
                    && want.track.is_some()
                    && usable.ids.track == want.track
                    && order(usable.first, key) != Ordering::Greater
                    && order(key, (usable.last.0, usable.last.1 + MAX_GAP)) != Ordering::Greater
            };
            if matches {
                chosen.push(path);
                if !online {
                    break;
                }
            }
        }
        let mut segments = Vec::new();
        for path in &chosen {
            let Some(usable) = self.files.get_mut(path).and_then(|k| k.usable.as_mut()) else {
                continue;
            };
            read_on(path, usable);
            segments.extend(usable.segments.iter().cloned());
        }
        if segments.is_empty() {
            return None;
        }
        segments.sort_by(|a, b| order(a.first(), b.first()));
        Some(TireTrack { segments })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use pitot_irsdk::layout::{
        DiskSubHeader, Header, SDK_VERSION, STATUS_CONNECTED, VarHeader, VarType,
    };

    use super::*;

    const RECORD_LEN: usize = 128;

    /// A synthetic `.ibt` of the player's tires: SessionTime, SessionNum, surface temperatures, tread, and hot
    /// pressures. In each record (session, time, base), the left front's left zone is at `base` and every other
    /// zone a tenth of a degree more than the one before.
    pub(crate) fn ibt(sub_session: i64, track: i64, records: &[(i32, f64, f32)]) -> Vec<u8> {
        let mut vars = vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Int, 8, 1, "SessionNum").expect("valid"),
        ];
        let mut at = 12;
        for kind in ["temp", "wear"] {
            for corner in NAMES {
                for zone in ZONE_NAMES {
                    let name = format!("{corner}{kind}{zone}");
                    vars.push(VarHeader::new(VarType::Float, at, 1, &name).expect("valid"));
                    at += 4;
                }
            }
        }
        for corner in NAMES {
            let name = format!("{corner}pressure");
            vars.push(VarHeader::new(VarType::Float, at, 1, &name).expect("valid"));
            at += 4;
        }
        let yaml =
            format!("---\nWeekendInfo:\n TrackID: {track}\n SubSessionID: {sub_session}\n...\n");
        let table_at = Header::SIZE + DiskSubHeader::SIZE;
        let info_at = table_at + vars.len() * VarHeader::SIZE;
        let info_len = yaml.len() + 16;
        let data_at = info_at + info_len;
        let mut bytes = vec![0u8; data_at + records.len() * RECORD_LEN];
        let mut header = Header::default();
        header.ver = SDK_VERSION;
        header.status = STATUS_CONNECTED;
        header.tick_rate = 60;
        header.session_info_len = info_len as i32;
        header.session_info_offset = info_at as i32;
        header.num_vars = vars.len() as i32;
        header.var_header_offset = table_at as i32;
        header.num_buf = 1;
        header.buf_len = RECORD_LEN as i32;
        header.var_buf[0].buf_offset = data_at as i32;
        header.write(&mut bytes).expect("header fits");
        // A record count of zero, as while the sim is still writing the file.
        DiskSubHeader::default()
            .write(&mut bytes)
            .expect("sub-header fits");
        for (n, var) in vars.iter().enumerate() {
            var.write(&mut bytes[table_at + n * VarHeader::SIZE..])
                .expect("fits");
        }
        bytes[info_at..info_at + yaml.len()].copy_from_slice(yaml.as_bytes());
        for (n, &(session, time, base)) in records.iter().enumerate() {
            let row = &mut bytes[data_at + n * RECORD_LEN..][..RECORD_LEN];
            row[0..8].copy_from_slice(&time.to_le_bytes());
            row[8..12].copy_from_slice(&session.to_le_bytes());
            for i in 0..CORNERS * ZONES {
                let temp = base + i as f32 * 0.1;
                row[12 + i * 4..16 + i * 4].copy_from_slice(&temp.to_le_bytes());
                let wear = 0.95 - i as f32 * 0.001;
                row[60 + i * 4..64 + i * 4].copy_from_slice(&wear.to_le_bytes());
            }
            for c in 0..CORNERS {
                let kpa = 170.0 + c as f32;
                row[108 + c * 4..112 + c * 4].copy_from_slice(&kpa.to_le_bytes());
            }
        }
        bytes
    }

    /// `seconds` of 60 Hz records of `session` from `start`, the base temperature rising a degree a second.
    pub(crate) fn stint(session: i32, start: f64, seconds: f64) -> Vec<(i32, f64, f32)> {
        (0..(seconds * 60.0) as usize)
            .map(|n| {
                let t = n as f64 / 60.0;
                (session, start + t, 80.0 + t as f32)
            })
            .collect()
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pitot-ibt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("creates the folder");
        dir
    }

    fn write(dir: &Path, name: &str, bytes: &[u8], age_s: u64) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).expect("writes");
        let file = fs::File::options().write(true).open(&path).expect("opens");
        file.set_modified(SystemTime::now() - Duration::from_secs(age_s))
            .expect("sets the time");
        path
    }

    fn ids(sub_session: i64, track: i64) -> SessionIds {
        SessionIds {
            sub_session,
            track: Some(track),
        }
    }

    #[test]
    fn online_sessions_join_every_stint() {
        let dir = folder("online");
        write(&dir, "first.ibt", &ibt(77, 5, &stint(2, 100.0, 10.0)), 60);
        write(&dir, "second.ibt", &ibt(77, 5, &stint(2, 300.0, 10.0)), 30);
        write(&dir, "other.ibt", &ibt(78, 5, &stint(2, 200.0, 10.0)), 10);
        write(&dir, "notes.txt", b"not telemetry", 5);
        let mut finder = TireFinder::default();
        let mut parser = SessionParser::new();
        let folders = [dir.clone()];
        let track = finder
            .find(&folders, ids(77, 5), Moment::new(2, 105.0), &mut parser)
            .expect("found");
        assert_eq!(track.segments.len(), 2, "both stints");

        // Five samples a second; the one at or just before the moment shows.
        let sample = track
            .sample_at(Moment::new(2, 105.1))
            .expect("in the first stint");
        assert!((sample.time - 105.0).abs() < 1e-9, "{}", sample.time);
        assert_eq!(sample.temp(0, 0), Some(85.0));
        assert_eq!(sample.temp(3, 2), Some(86.1), "the last zone");
        assert_eq!(sample.wear(0, 0), Some(0.95));
        assert_eq!(sample.pressure(2), Some(172.0));
        assert!(
            track.sample_at(Moment::new(2, 305.0)).is_some(),
            "second stint"
        );
        assert_eq!(
            track.sample_at(Moment::new(2, 200.0)),
            None,
            "out of the car between stints"
        );
        assert_eq!(
            track.sample_at(Moment::new(3, 105.0)),
            None,
            "another session"
        );
        assert_eq!(
            track.sample_at(Moment::new(2, 50.0)),
            None,
            "before the first"
        );
        assert!(track.covers(Moment::new(2, 200.0)));
        assert!(!track.covers(Moment::new(2, 400.0)));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_sessions_take_the_newest_file_that_covers_the_moment() {
        let dir = folder("offline");
        write(&dir, "old.ibt", &ibt(0, 5, &stint(0, 100.0, 10.0)), 600);
        write(&dir, "new.ibt", &ibt(0, 5, &stint(0, 100.0, 5.0)), 60);
        write(&dir, "elsewhere.ibt", &ibt(0, 6, &stint(0, 100.0, 10.0)), 1);
        let mut finder = TireFinder::default();
        let mut parser = SessionParser::new();
        let folders = [dir.clone()];
        let track = finder
            .find(&folders, ids(0, 5), Moment::new(0, 102.0), &mut parser)
            .expect("found");
        assert_eq!(track.segments.len(), 1, "one file");
        assert!(
            !track.covers(Moment::new(0, 108.0)),
            "the newer, shorter file"
        );
        let track = finder
            .find(&folders, ids(0, 5), Moment::new(0, 108.0), &mut parser)
            .expect("the older file covers it");
        assert!(track.covers(Moment::new(0, 108.0)));
        assert_eq!(
            finder.find(&folders, ids(0, 7), Moment::new(0, 102.0), &mut parser),
            None,
            "no file of that track"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_growing_file_is_read_on_from_where_it_was_left() {
        let dir = folder("growing");
        let mut records = stint(1, 0.0, 4.0);
        let path = write(&dir, "live.ibt", &ibt(9, 5, &records), 0);
        let mut finder = TireFinder::default();
        let mut parser = SessionParser::new();
        let folders = [dir.clone()];
        let first = finder
            .find(&folders, ids(9, 5), Moment::new(1, 2.0), &mut parser)
            .expect("found");
        assert!(!first.covers(Moment::new(1, 6.0)));
        records.extend(stint(1, 4.0, 4.0));
        fs::write(&path, ibt(9, 5, &records)).expect("grows");
        let grown = finder
            .find(&folders, ids(9, 5), Moment::new(1, 6.0), &mut parser)
            .expect("found");
        assert_eq!(grown.segments.len(), 2, "the new records only");
        assert!(Arc::ptr_eq(&grown.segments[0], &first.segments[0]));
        let at = grown.sample_at(Moment::new(1, 6.0)).expect("covered now");
        assert!((at.time - 6.0).abs() < 1e-9, "{}", at.time);
        // Unchanged: nothing is read again.
        let again = finder
            .find(&folders, ids(9, 5), Moment::new(1, 6.0), &mut parser)
            .expect("found");
        assert_eq!(again.segments.len(), 2);
        let _ = fs::remove_dir_all(dir);
    }
}
