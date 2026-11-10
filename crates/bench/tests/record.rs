//! A replay recorded back: every frame and session info revision the replay published is in the capture.
#![cfg(windows)]

use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

use pitot_bench::capture::{CaptureSource, Record};
use pitot_bench::record::{RecordOptions, record};
use pitot_bench::replay::{FrameSource, Item, ReplayError, ReplayOptions, SourceLayout, run};
use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{ReaderConfig, SdkNames};

const FRAMES: u32 = 180;
const ROW: usize = 16 + 64 * 4;

/// Session info, then frames whose bytes depend on their index, a second revision at frame 90.
struct Synthetic {
    layout: SourceLayout,
    step: u32,
}

impl Synthetic {
    fn new() -> Self {
        Self {
            layout: SourceLayout {
                vars: vec![
                    VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
                    VarHeader::new(VarType::Int, 8, 1, "Lap").expect("valid"),
                    VarHeader::new(VarType::Float, 16, 64, "CarIdxLapDistPct").expect("valid"),
                ],
                buf_len: ROW,
                tick_rate: 60,
                session_info_capacity: 4096,
                player_car_only: false,
            },
            step: 0,
        }
    }
}

fn row(n: u32) -> Vec<u8> {
    let mut row = vec![0u8; ROW];
    row[0..8].copy_from_slice(&(f64::from(n) / 60.0).to_le_bytes());
    row[8..12].copy_from_slice(&((n / 60) as i32).to_le_bytes());
    for car in 0..64u32 {
        let at = 16 + car as usize * 4;
        row[at..at + 4].copy_from_slice(&(((n + car) % 100) as f32 / 100.0).to_le_bytes());
    }
    row
}

impl FrameSource for Synthetic {
    fn layout(&self) -> &SourceLayout {
        &self.layout
    }

    fn next(&mut self, buf: &mut Vec<u8>) -> Result<Option<Item>, ReplayError> {
        buf.clear();
        let step = self.step;
        self.step += 1;
        if step == 0 || step == 91 {
            buf.extend_from_slice(format!("WeekendInfo:\n TrackID: {step}\n").as_bytes());
            return Ok(Some(Item::SessionInfo));
        }
        let frame = if step > 91 { step - 2 } else { step - 1 };
        if frame >= FRAMES {
            return Ok(None);
        }
        buf.extend_from_slice(&row(frame));
        Ok(Some(Item::Frame))
    }

    fn rewind(&mut self) -> Result<(), ReplayError> {
        self.step = 0;
        Ok(())
    }
}

#[test]
fn records_what_a_replay_publishes() {
    let pid = std::process::id();
    let (mapping, event) = (
        format!("Local\\PitotRecordMap-{pid}"),
        format!("Local\\PitotRecordEvent-{pid}"),
    );
    let dir = std::env::temp_dir().join(format!("pitot-record-{pid}"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = dir.join("recorded.ptc");

    let stop = AtomicBool::new(false);
    let options = RecordOptions {
        names: SdkNames::new(&mapping, &event),
        out: out.clone(),
        level: 3,
        max_duration: Some(Duration::from_secs(10)),
        reader: ReaderConfig {
            retry_interval: Duration::from_millis(5),
            idle_timeout: Duration::from_millis(100),
        },
    };
    let summary = thread::scope(|scope| {
        let recorder = scope.spawn(|| record(&options, &stop));
        // Give the recorder a moment to wait for the mapping, as it would for the sim.
        thread::sleep(Duration::from_millis(300));
        let replay = ReplayOptions {
            names: SdkNames::new(&mapping, &event),
            linger: Duration::from_millis(500),
            ..ReplayOptions::default()
        };
        run(&mut Synthetic::new(), &replay).expect("replays");
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        recorder.join().expect("recorder thread").expect("records")
    });

    assert_eq!(summary.files.len(), 1, "one layout, one file");
    let counts = summary.files[0].counts;
    let mut capture = CaptureSource::open(&out).expect("opens");
    assert_eq!(capture.layout().vars, Synthetic::new().layout.vars);
    let mut buf = Vec::new();
    let (mut frames, mut texts, mut missed) = (Vec::new(), Vec::new(), 0u64);
    while let Some(record) = capture.read(&mut buf).expect("reads") {
        match record {
            Record::Frame { .. } => frames.push(buf.clone()),
            Record::SessionInfo { carried: false, .. } => texts.push(buf.clone()),
            Record::Gap { missed: m, .. } => missed += u64::from(m),
            _ => {}
        }
    }
    assert_eq!(counts.frames as usize, frames.len());
    // The recorder connects at the first frame and may miss it; everything after is recorded or counted.
    let first = (0..FRAMES)
        .find(|&n| frames.first() == Some(&row(n)))
        .expect("first frame is a replayed one");
    assert!(first <= 1, "starts at frame {first}");
    assert_eq!(frames.len() as u64 + missed, u64::from(FRAMES - first));
    let mut expected = first;
    for frame in &frames {
        // Skip what the gaps counted.
        while *frame != row(expected) {
            expected += 1;
            assert!(expected < FRAMES, "recorded frame not in the replay");
        }
        expected += 1;
    }
    // A busy machine can delay the recorder past the spare buffers; such ticks are counted as gaps.
    assert!(missed <= 3, "{missed} ticks missed");
    assert_eq!(
        texts.last().map(|t| t.as_slice()),
        Some(&b"WeekendInfo:\n TrackID: 91\n"[..])
    );
    assert!(
        summary.files[0].counts.disconnects >= 1,
        "the replay ends with a disconnect"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
