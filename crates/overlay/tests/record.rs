//! The overlay records a session while the player drives, and a later replay of it shows the recording.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use pitot_irsdk::capture::{CaptureReader, Record};
use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{ReaderConfig, SdkNames};
use pitot_irsdk::publish::{Publisher, PublisherConfig};
use pitot_overlay::pipeline::{Display, Pipeline, RecordingConfig, Source, View};

const SESSION_INFO: &[u8] = b"WeekendInfo:\n TrackID: 9\n SubSessionID: 777\n";
const ROW: usize = 40;

fn publisher(names: &SdkNames) -> Publisher {
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Double, 8, 1, "ReplaySessionTime").expect("valid"),
            VarHeader::new(VarType::Int, 16, 1, "SessionNum").expect("valid"),
            VarHeader::new(VarType::Int, 20, 1, "ReplaySessionNum").expect("valid"),
            VarHeader::new(VarType::Float, 24, 1, "ThrottleRaw").expect("valid"),
            VarHeader::new(VarType::Bool, 28, 1, "IsOnTrack").expect("valid"),
            VarHeader::new(VarType::Bool, 29, 1, "IsReplayPlaying").expect("valid"),
            VarHeader::new(VarType::Int, 32, 1, "ReplayPlaySpeed").expect("valid"),
        ],
        buf_len: ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher.set_session_info(SESSION_INFO).expect("fits");
    publisher
}

/// Driving at session time `time` with the throttle at `throttle`, or replaying `replay` with live values.
fn row(time: f64, throttle: f32, replay: Option<f64>) -> [u8; ROW] {
    let mut row = [0u8; ROW];
    row[0..8].copy_from_slice(&time.to_le_bytes());
    match replay {
        Some(at) => {
            row[8..16].copy_from_slice(&at.to_le_bytes());
            row[29] = 1;
            row[32..36].copy_from_slice(&1i32.to_le_bytes());
        }
        None => {
            row[20..24].copy_from_slice(&(-1i32).to_le_bytes());
            row[24..28].copy_from_slice(&throttle.to_le_bytes());
            row[28] = 1;
        }
    }
    row
}

fn names(tag: &str) -> SdkNames {
    let pid = std::process::id();
    SdkNames::new(
        &format!("Local\\PitotRecord{tag}Map-{pid}"),
        &format!("Local\\PitotRecord{tag}Event-{pid}"),
    )
}

fn reader_config() -> ReaderConfig {
    ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    }
}

fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn captures(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "ptc"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_recorded_session_plays_in_its_replay() {
    let dir = std::env::temp_dir().join(format!("pitot-record-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let recording = RecordingConfig {
        dir: dir.clone(),
        max_bytes: 1 << 30,
    };

    // Drive 300 ticks, watch a replay for 60 (not recorded), drive 100 more, leave.
    let names_a = names("A");
    let mut sim = publisher(&names_a);
    let (pipeline, mut view) =
        Pipeline::start(names_a, reader_config(), Vec::new(), Some(recording)).expect("starts");
    wait("connection", || {
        view.poll();
        view.is_connected()
    });
    let ticks = |view: &View| {
        view.stats()
            .ticks
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    let mut publish = |view: &mut View, row: [u8; ROW]| {
        let before = ticks(view);
        sim.publish(&row).expect("publishes");
        wait("the tick", || ticks(view) > before);
        view.poll();
        // At the sim's rate: the recorder drains once a second, and its queue holds two seconds.
        thread::sleep(Duration::from_millis(15));
    };
    for i in 0..300 {
        publish(
            &mut view,
            row(100.0 + f64::from(i) / 60.0, i as f32 / 1000.0, None),
        );
    }
    for i in 0..60 {
        publish(
            &mut view,
            row(105.0 + f64::from(i) / 60.0, 0.0, Some(101.0)),
        );
    }
    for i in 0..100 {
        publish(&mut view, row(106.0 + f64::from(i) / 60.0, 0.9, None));
    }
    sim.set_connected(false).expect("disconnects");
    wait("the recording to finish", || {
        // The file exists before its header is flushed, so an open can fail until then.
        captures(&dir).first().is_some_and(|path| {
            CaptureReader::open(path)
                .ok()
                .and_then(|mut reader| reader.is_finished().ok())
                .unwrap_or(false)
        })
    });
    drop(pipeline);

    let path = captures(&dir).remove(0);
    let mut reader = CaptureReader::open(&path).expect("opens");
    let (mut frames, mut missed, mut sessions, mut first) = (0, 0, 0, None);
    let mut buf = Vec::new();
    while let Some(record) = reader.read(&mut buf).expect("reads") {
        first.get_or_insert(record);
        match record {
            Record::Frame { .. } => frames += 1,
            Record::SessionInfo { carried: false, .. } => sessions += 1,
            Record::Gap { missed: n, .. } => missed += n,
            _ => {}
        }
    }
    // A slow machine may delay the recorder past its queue; the rows it drops are recorded as a gap.
    assert_eq!(frames + missed, 400, "driving ticks only");
    assert!(frames >= 350, "{frames} frames");
    assert_eq!(sessions, 1);
    assert!(
        matches!(first, Some(Record::SessionInfo { .. })),
        "session info opens the capture"
    );
    let metadata = reader.metadata().expect("reads").expect("finished");
    assert!(
        metadata.contains("\"tool\": \"pitot-overlay\""),
        "{metadata}"
    );

    // Later, the sim replays the session: the overlay finds the recording and shows it.
    let names_b = names("B");
    let mut sim = publisher(&names_b);
    let (_pipeline, mut view) =
        Pipeline::start(names_b, reader_config(), vec![dir.clone()], None).expect("starts");
    let mut t = 2000.0;
    wait("the recording in the replay", || {
        t += 1.0 / 60.0;
        sim.publish(&row(t, 0.0, Some(102.0))).expect("publishes");
        view.poll();
        view.source() == Some(Source::Capture { first: true })
    });
    let throttle = view
        .snapshot()
        .f32(view.vars().throttle, 0)
        .expect("throttle");
    assert!(
        (throttle - 0.12).abs() < 0.002,
        "the recorded throttle at 102 s: {throttle}"
    );
    assert_eq!(view.display(), Display::Driving);
    let _ = std::fs::remove_dir_all(dir);
}
