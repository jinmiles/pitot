//! Finds a capture's frames by session time, so a capture can play in step with the sim's own replay.

use std::io::{Read, Seek};
use std::mem;

use super::{CaptureError, CaptureLayout, CaptureReader, Record};
use crate::layout::{Field, VarType};

/// A moment of a session: the session number and the session time in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Moment {
    pub session: i32,
    pub time: f64,
}

impl Moment {
    pub const fn new(session: i32, time: f64) -> Self {
        Self { session, time }
    }

    /// Whether `self` comes no later than `other` plus `slack` seconds.
    fn at_or_before(self, other: Moment, slack: f64) -> bool {
        self.session < other.session
            || (self.session == other.session && self.time <= other.time + slack)
    }
}

/// The recorded span of one session.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Span {
    session: i32,
    first: f64,
    last: f64,
}

#[derive(Debug, Default)]
struct Frame {
    row: Vec<u8>,
    moment: Moment,
    tick: i32,
    valid: bool,
}

/// Plays a capture by session time.
///
/// Only frames recorded while the sim was not playing a replay (`IsReplayPlaying` clear) count as the
/// session: during its replays the sim publishes live values, so a capture made while watching a replay
/// covers nothing.
///
/// Opening reads the whole capture once to find where each chunk starts in session time; this takes about a
/// second per hour of capture. After that, moving to a moment reads from the chunk that holds it, and every
/// buffer is already sized for the largest chunk and session info text, so moving does not allocate.
pub struct Seeker<R> {
    reader: CaptureReader<R>,
    session_num: Field,
    session_time: Field,
    /// Chunk offsets with the moment of their first frame, in capture order.
    chunks: Vec<(u64, Moment)>,
    spans: Vec<Span>,
    half_tick: f64,
    current: Frame,
    next: Frame,
    /// Session info in effect at the current frame.
    session: Vec<u8>,
    session_update: Option<i32>,
    session_changed: bool,
    /// Session info read before the next frame, which applies from that frame on.
    next_session: Vec<u8>,
    next_session_update: Option<i32>,
    scratch: Vec<u8>,
}

impl<R> std::fmt::Debug for Seeker<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seeker")
            .field("chunks", &self.chunks.len())
            .field("spans", &self.spans)
            .finish_non_exhaustive()
    }
}

fn moment(row: &[u8], num: &Field, time: &Field) -> Moment {
    Moment {
        session: num.i32(row, 0).unwrap_or(-1),
        time: time.f64(row, 0).unwrap_or(f64::NAN),
    }
}

impl<R: Read + Seek> Seeker<R> {
    /// Reads the capture once to index it by session time.
    pub fn new(mut reader: CaptureReader<R>) -> Result<Self, CaptureError> {
        let layout = reader.layout();
        let field = |name: &str, var_type: VarType| {
            layout
                .vars
                .iter()
                .find(|v| v.name_bytes() == name.as_bytes())
                .and_then(|v| Field::new(v, layout.buf_len).ok())
                .filter(|f| f.var_type() == var_type)
                .ok_or(CaptureError::Format(
                    "capture has no session number or time",
                ))
        };
        let session_num = field("SessionNum", VarType::Int)?;
        let session_time = field("SessionTime", VarType::Double)?;
        let replaying = field("IsReplayPlaying", VarType::Bool).ok();
        let half_tick = 0.5 / f64::from(layout.tick_rate.max(1));
        let buf_len = layout.buf_len;

        let mut chunks: Vec<(u64, Moment)> = Vec::new();
        let mut spans: Vec<Span> = Vec::new();
        // A replay in the recording ends the span before it: the session time ran on, but those frames show
        // the sim's live values during its replay, not the session as driven.
        let mut after_replay = false;
        let mut longest_text = 0;
        let mut buf = Vec::new();
        reader.rewind();
        while let Some(record) = reader.read(&mut buf)? {
            match record {
                Record::Frame { .. } => {
                    let m = moment(&buf, &session_num, &session_time);
                    if !m.time.is_finite() {
                        continue;
                    }
                    let start = reader.chunk_start();
                    if chunks.last().is_none_or(|(offset, _)| *offset != start) {
                        chunks.push((start, m));
                    }
                    if replaying.is_some_and(|f| f.bool(&buf, 0) == Some(true)) {
                        after_replay = true;
                        continue;
                    }
                    match spans.last_mut() {
                        Some(span)
                            if !after_replay
                                && span.session == m.session
                                && m.time >= span.last =>
                        {
                            span.last = m.time;
                        }
                        _ => spans.push(Span {
                            session: m.session,
                            first: m.time,
                            last: m.time,
                        }),
                    }
                    after_replay = false;
                }
                Record::SessionInfo { .. } => longest_text = longest_text.max(buf.len()),
                _ => {}
            }
        }
        if spans.is_empty() {
            return Err(CaptureError::Format(
                "capture has no frames recorded outside a replay",
            ));
        }
        let rows = buf_len.max(longest_text);
        let sized = || Vec::with_capacity(rows);
        Ok(Self {
            reader,
            session_num,
            session_time,
            chunks,
            spans,
            half_tick,
            current: Frame {
                row: sized(),
                ..Frame::default()
            },
            next: Frame {
                row: sized(),
                ..Frame::default()
            },
            session: sized(),
            session_update: None,
            session_changed: false,
            next_session: sized(),
            next_session_update: None,
            scratch: sized(),
        })
    }

    /// The capture's variable table and row length.
    pub fn layout(&self) -> &CaptureLayout {
        self.reader.layout()
    }

    /// The session info in effect at the current frame.
    pub fn session(&self) -> Option<(i32, &[u8])> {
        self.session_update.map(|u| (u, &self.session[..]))
    }

    /// Whether the capture has frames of `at`'s session around `at`, recorded while the sim was not playing a
    /// replay.
    pub fn covers(&self, at: Moment) -> bool {
        self.spans.iter().any(|s| {
            s.session == at.session
                && at.time >= s.first - self.half_tick
                && at.time <= s.last + self.half_tick
        })
    }

    /// The last recorded moment.
    pub fn end(&self) -> Moment {
        self.spans.last().map_or(Moment::default(), |s| Moment {
            session: s.session,
            time: s.last,
        })
    }

    /// Positions the capture at the last frame at or before `at`, or at the first frame after it when the
    /// capture starts later. Returns whether there is a current frame.
    pub fn seek(&mut self, at: Moment) -> Result<bool, CaptureError> {
        let after = self
            .chunks
            .partition_point(|(_, first)| first.at_or_before(at, self.half_tick));
        let (offset, _) = self.chunks[after.saturating_sub(1)];
        self.reader.seek_chunk(offset);
        self.current.valid = false;
        self.next.valid = false;
        self.session_update = None;
        self.next_session_update = None;
        self.fill_next()?;
        self.promote()?;
        while self.next.valid && self.next.moment.at_or_before(at, self.half_tick) {
            self.promote()?;
        }
        Ok(self.current.valid)
    }

    /// Moves forward frame by frame up to the last frame at or before `at`, calling `each` with every frame it
    /// moves onto. Returns how many frames it moved. A moment before the current frame moves nothing; use
    /// [`Seeker::seek`] to go back.
    pub fn advance(
        &mut self,
        at: Moment,
        mut each: impl FnMut(&[u8], Moment, i32),
    ) -> Result<usize, CaptureError> {
        let mut moved = 0;
        while self.next.valid && self.next.moment.at_or_before(at, self.half_tick) {
            self.promote()?;
            each(&self.current.row, self.current.moment, self.current.tick);
            moved += 1;
        }
        Ok(moved)
    }

    /// The current frame's row, moment, and recorded tick count.
    pub fn current(&self) -> Option<(&[u8], Moment, i32)> {
        self.current.valid.then_some((
            &self.current.row[..],
            self.current.moment,
            self.current.tick,
        ))
    }

    /// The session info in effect at the current frame, if it changed since the last call.
    pub fn take_session_change(&mut self) -> Option<(i32, &[u8])> {
        if !mem::take(&mut self.session_changed) {
            return None;
        }
        self.session_update.map(|u| (u, &self.session[..]))
    }

    /// Makes the next frame current and reads the one after it.
    fn promote(&mut self) -> Result<(), CaptureError> {
        mem::swap(&mut self.current, &mut self.next);
        if let Some(update) = self.next_session_update.take()
            && self.current.valid
            && self.session_update != Some(update)
        {
            mem::swap(&mut self.session, &mut self.next_session);
            self.session_update = Some(update);
            self.session_changed = true;
        }
        self.fill_next()
    }

    /// Reads up to the next frame into `next`, keeping session info read on the way for it.
    fn fill_next(&mut self) -> Result<(), CaptureError> {
        self.next.valid = false;
        loop {
            match self.reader.read(&mut self.scratch)? {
                Some(Record::Frame { tick, .. }) => {
                    let m = moment(&self.scratch, &self.session_num, &self.session_time);
                    mem::swap(&mut self.scratch, &mut self.next.row);
                    self.next.moment = m;
                    self.next.tick = tick;
                    self.next.valid = true;
                    return Ok(());
                }
                Some(Record::SessionInfo { update, .. }) => {
                    mem::swap(&mut self.scratch, &mut self.next_session);
                    self.next_session_update = Some(update);
                }
                Some(Record::Gap { .. } | Record::Disconnect { .. }) => {}
                Some(Record::End { .. }) | None => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::capture::test_capture::{Item, build};
    use crate::layout::VarHeader;

    const START: f64 = 100.0;

    fn vars() -> Vec<VarHeader> {
        vec![
            VarHeader::new(VarType::Int, 0, 1, "SessionNum").expect("valid"),
            VarHeader::new(VarType::Double, 8, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Float, 16, 1, "ThrottleRaw").expect("valid"),
        ]
    }

    fn row(session: i32, i: usize) -> Vec<u8> {
        let mut row = vec![0u8; 20];
        row[0..4].copy_from_slice(&session.to_le_bytes());
        row[8..16].copy_from_slice(&(START + i as f64 / 60.0).to_le_bytes());
        row[16..20].copy_from_slice(&(i as f32).to_le_bytes());
        row
    }

    /// 300 frames of session 0 in chunks of 60, session info 1 at the start and 2 before frame 150.
    fn seeker() -> Seeker<Cursor<Vec<u8>>> {
        let mut items = vec![Item::Session(1, b"first".to_vec())];
        for i in 0..300 {
            if i == 150 {
                items.push(Item::Session(2, b"second".to_vec()));
            }
            items.push(Item::Frame(i as i32, row(0, i)));
        }
        let bytes = build(&vars(), 20, 60, &items);
        Seeker::new(CaptureReader::new(Cursor::new(bytes)).expect("opens")).expect("indexes")
    }

    fn throttle(s: &Seeker<Cursor<Vec<u8>>>) -> f32 {
        let (row, ..) = s.current().expect("a frame");
        f32::from_le_bytes(row[16..20].try_into().unwrap())
    }

    fn at(i: f64) -> Moment {
        Moment::new(0, START + i / 60.0)
    }

    #[test]
    fn seeking_finds_the_frame_and_its_session_info() {
        let mut s = seeker();
        assert!(s.seek(at(120.0)).expect("seeks"));
        assert_eq!(throttle(&s), 120.0);
        assert_eq!(s.take_session_change(), Some((1, &b"first"[..])));
        assert_eq!(s.take_session_change(), None, "reported once");

        assert!(s.seek(at(200.0)).expect("seeks"));
        assert_eq!(throttle(&s), 200.0);
        assert_eq!(s.take_session_change(), Some((2, &b"second"[..])));

        assert!(s.seek(at(10.3)).expect("seeks back"));
        assert_eq!(throttle(&s), 10.0);
        assert_eq!(s.take_session_change(), Some((1, &b"first"[..])));
    }

    #[test]
    fn advancing_follows_play_pause_and_slow_motion() {
        let mut s = seeker();
        s.seek(at(140.0)).expect("seeks");
        s.take_session_change();
        let mut seen = Vec::new();
        let moved = s
            .advance(at(150.0), |row, _, tick| {
                seen.push((tick, f32::from_le_bytes(row[16..20].try_into().unwrap())));
            })
            .expect("advances");
        assert_eq!(moved, 10);
        assert_eq!(seen.first(), Some(&(141, 141.0)));
        assert_eq!(seen.last(), Some(&(150, 150.0)));
        assert_eq!(s.take_session_change(), Some((2, &b"second"[..])));

        assert_eq!(s.advance(at(150.0), |_, _, _| {}).expect("paused"), 0);
        assert_eq!(s.advance(at(150.25), |_, _, _| {}).expect("slow"), 0);
        assert_eq!(s.advance(at(150.6), |_, _, _| {}).expect("slow"), 1);
        assert_eq!(throttle(&s), 151.0);
        assert_eq!(s.advance(at(100.0), |_, _, _| {}).expect("back"), 0);

        assert_eq!(
            s.advance(at(1000.0), |_, _, _| {}).expect("to the end"),
            148
        );
        assert_eq!(throttle(&s), 299.0);
    }

    fn replay_vars() -> Vec<VarHeader> {
        let mut vars = vars();
        vars.push(VarHeader::new(VarType::Bool, 20, 1, "IsReplayPlaying").expect("valid"));
        vars
    }

    fn replay_row(i: usize, replaying: bool) -> Vec<u8> {
        let mut row = row(0, i);
        row.extend_from_slice(&[u8::from(replaying), 0, 0, 0]);
        row
    }

    #[test]
    fn frames_recorded_during_a_replay_do_not_count() {
        // Driving for 120 frames, watching a replay for 60 (the session time runs on), then driving again.
        let mut items = vec![Item::Session(1, b"first".to_vec())];
        for i in 0..240 {
            items.push(Item::Frame(
                i as i32,
                replay_row(i, (120..180).contains(&i)),
            ));
        }
        let bytes = build(&replay_vars(), 24, 60, &items);
        let s =
            Seeker::new(CaptureReader::new(Cursor::new(bytes)).expect("opens")).expect("indexes");
        assert!(s.covers(at(119.0)));
        assert!(!s.covers(at(150.0)), "the sim's replay");
        assert!(s.covers(at(180.0)));
        assert_eq!(s.end(), at(239.0));

        let watched: Vec<Item> = (0..120)
            .map(|i| Item::Frame(i, replay_row(i as usize, true)))
            .collect();
        let bytes = build(&replay_vars(), 24, 60, &watched);
        let only_replay = Seeker::new(CaptureReader::new(Cursor::new(bytes)).expect("opens"));
        assert!(
            only_replay.is_err(),
            "a capture of watching a replay covers nothing"
        );
    }

    #[test]
    fn coverage_is_per_session() {
        let s = seeker();
        assert!(s.covers(at(0.0)));
        assert!(s.covers(at(299.0)));
        assert!(!s.covers(at(-60.0)));
        assert!(!s.covers(at(400.0)));
        assert!(!s.covers(Moment::new(1, START)));
        assert_eq!(s.end(), at(299.0));
    }

    #[test]
    fn moving_does_not_grow_buffers() {
        let mut s = seeker();
        s.seek(at(0.0)).expect("seeks");
        let capacities = |s: &Seeker<Cursor<Vec<u8>>>| {
            [
                s.current.row.capacity(),
                s.next.row.capacity(),
                s.session.capacity(),
                s.next_session.capacity(),
                s.scratch.capacity(),
            ]
        };
        let before = capacities(&s);
        s.advance(at(299.0), |_, _, _| {}).expect("advances");
        s.seek(at(30.0)).expect("seeks back");
        let mut after = capacities(&s);
        let mut before = before;
        before.sort_unstable();
        after.sort_unstable();
        assert_eq!(before, after);
    }
}
