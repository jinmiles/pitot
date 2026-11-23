//! While the sim plays a replay, the view shows a capture of the session at the replayed moment.
#![cfg(windows)]

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use pitot_irsdk::capture::test_capture::{Item, build};
use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{ReaderConfig, SdkNames};
use pitot_irsdk::publish::{Publisher, PublisherConfig};
use pitot_overlay::pipeline::{Display, InputSample, Pipeline, Source, View};

const SESSION_INFO: &[u8] = b"WeekendInfo:\n TrackID: 77\n SubSessionID: 12345\n";
const START: f64 = 100.0;
const FRAMES: usize = 600;

/// The capture's `SessionTick` at its first frame.
const CAPTURE_TICK: i32 = 6000;

/// A capture of session 0 from `START`: 600 frames, the player on track, throttle and car 1's lap distance
/// rising from 0 to 1.
fn capture_folder(name: &str) -> PathBuf {
    let vars = vec![
        VarHeader::new(VarType::Int, 0, 1, "SessionNum").expect("valid"),
        VarHeader::new(VarType::Double, 8, 1, "SessionTime").expect("valid"),
        VarHeader::new(VarType::Float, 16, 1, "ThrottleRaw").expect("valid"),
        VarHeader::new(VarType::Bool, 20, 1, "IsOnTrack").expect("valid"),
        VarHeader::new(VarType::Bool, 21, 1, "IsReplayPlaying").expect("valid"),
        VarHeader::new(VarType::Float, 24, 4, "CarIdxLapDistPct").expect("valid"),
        VarHeader::new(VarType::Int, 40, 1, "SessionTick").expect("valid"),
    ];
    let mut items = vec![Item::Session(1, SESSION_INFO.to_vec())];
    for i in 0..FRAMES {
        let mut row = vec![0u8; 44];
        row[8..16].copy_from_slice(&(START + i as f64 / 60.0).to_le_bytes());
        row[16..20].copy_from_slice(&(i as f32 / FRAMES as f32).to_le_bytes());
        row[20] = 1;
        row[28..32].copy_from_slice(&(i as f32 / FRAMES as f32).to_le_bytes());
        row[40..44].copy_from_slice(&(CAPTURE_TICK + i as i32).to_le_bytes());
        items.push(Item::Frame(1000 + i as i32, row));
    }
    let dir = std::env::temp_dir().join(format!("pitot-replay-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the folder");
    std::fs::write(dir.join("race.ptc"), build(&vars, 44, 60, &items)).expect("writes");
    dir
}

/// Where the sim's row has `SessionTick`, after the per-car array.
const TICK_AT: usize = 32 + 72 * 4;
const ROW: usize = TICK_AT + 4;

/// The sim's row: other order and sizes than the capture's. `with_speed` adds `ReplayPlaySpeed` after it.
fn sim_publisher(names: &SdkNames, with_speed: bool) -> Publisher {
    let mut vars = vec![
        VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
        VarHeader::new(VarType::Double, 8, 1, "ReplaySessionTime").expect("valid"),
        VarHeader::new(VarType::Int, 16, 1, "SessionNum").expect("valid"),
        VarHeader::new(VarType::Int, 20, 1, "ReplaySessionNum").expect("valid"),
        VarHeader::new(VarType::Float, 24, 1, "ThrottleRaw").expect("valid"),
        VarHeader::new(VarType::Bool, 28, 1, "IsOnTrack").expect("valid"),
        VarHeader::new(VarType::Bool, 29, 1, "IsReplayPlaying").expect("valid"),
        VarHeader::new(VarType::Float, 32, 72, "CarIdxLapDistPct").expect("valid"),
        VarHeader::new(VarType::Int, TICK_AT as i32, 1, "SessionTick").expect("valid"),
    ];
    if with_speed {
        vars.push(VarHeader::new(VarType::Int, ROW as i32, 1, "ReplayPlaySpeed").expect("valid"));
    }
    let config = PublisherConfig {
        vars,
        buf_len: ROW + if with_speed { 4 } else { 0 },
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher.set_session_info(SESSION_INFO).expect("fits");
    publisher
}

/// A live tick: driving with the throttle at 0.9, or replaying `replay_time` with live values that are not
/// the replayed moment's. With `speed`, the row ends with `ReplayPlaySpeed`.
fn sim_row(live_time: f64, replay_time: Option<f64>, speed: Option<i32>) -> Vec<u8> {
    let mut row = vec![0u8; ROW];
    row[0..8].copy_from_slice(&live_time.to_le_bytes());
    row[TICK_AT..TICK_AT + 4].copy_from_slice(&((live_time * 60.0).round() as i32).to_le_bytes());
    match replay_time {
        Some(t) => {
            row[8..16].copy_from_slice(&t.to_le_bytes());
            row[29] = 1;
        }
        None => {
            row[20..24].copy_from_slice(&(-1i32).to_le_bytes());
            row[24..28].copy_from_slice(&0.9f32.to_le_bytes());
            row[28] = 1;
        }
    }
    row[32 + 4..32 + 8].copy_from_slice(&0.99f32.to_le_bytes());
    if let Some(speed) = speed {
        let playing = if replay_time.is_some() { speed } else { 0 };
        row.extend_from_slice(&playing.to_le_bytes());
    }
    row
}

struct Sim {
    publisher: Publisher,
    time: f64,
    /// `ReplayPlaySpeed` while replaying, if the sim publishes it.
    speed: Option<i32>,
}

impl Sim {
    fn tick(&mut self, replay_time: Option<f64>) {
        self.time += 1.0 / 60.0;
        self.publisher
            .publish(&sim_row(self.time, replay_time, self.speed))
            .expect("publishes");
        thread::sleep(Duration::from_millis(4));
    }
}

fn wait_for(
    view: &mut View,
    sim: &mut Sim,
    at: Option<f64>,
    what: &str,
    mut done: impl FnMut(&mut View) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        view.poll();
        if done(view) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sim.tick(at);
    }
}

fn throttle(view: &View) -> Option<f32> {
    view.snapshot().f32(view.vars().throttle, 0)
}

fn frame(i: usize) -> f64 {
    START + i as f64 / 60.0
}

fn drain(view: &mut View) -> Vec<InputSample> {
    let mut samples = Vec::new();
    view.drain_samples(|s| samples.push(s));
    samples
}

/// Without `ReplayPlaySpeed`, every recorded frame the replay passes becomes a sample.
#[test]
fn replays_show_the_capture_at_the_replayed_moment() {
    let folder = capture_folder("frames");
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotReplayMap-{pid}"),
        &format!("Local\\PitotReplayEvent-{pid}"),
    );
    let mut sim = Sim {
        publisher: sim_publisher(&names, false),
        time: 500.0,
        speed: None,
    };
    let config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (_pipeline, mut view) =
        Pipeline::start(names, config, vec![folder.clone()], None).expect("starts");

    wait_for(&mut view, &mut sim, None, "live driving", |v| {
        v.display() == Display::Driving && v.session().is_some()
    });
    assert_eq!(view.source(), Some(Source::Sim));
    assert_eq!(throttle(&view), Some(0.9));
    let live_tick = view.tick_time().expect("tick time");
    assert!(live_tick > 500.0, "the live tick count: {live_tick}");

    // The replay shows frame 300; the sim's own values say nothing about it.
    wait_for(&mut view, &mut sim, Some(frame(300)), "the capture", |v| {
        v.source() == Some(Source::Capture { first: true })
    });
    assert_eq!(
        view.display(),
        Display::Driving,
        "the player was on track then"
    );
    assert_eq!(throttle(&view), Some(0.5));
    assert_eq!(
        view.tick_time(),
        Some(f64::from(CAPTURE_TICK + 300) / 60.0),
        "the capture's tick count times durations"
    );
    let vars = *view.vars();
    assert_eq!(view.snapshot().f32(vars.car_lap_dist_pct, 1), Some(0.5));
    assert_eq!(
        view.snapshot().count(vars.car_lap_dist_pct),
        72,
        "live layout"
    );
    assert_eq!(view.car_count(), 72);
    let prefill = drain(&mut view);
    assert_eq!(prefill.len(), 257, "the frame and the 256 before it");
    assert_eq!(prefill.last().map(|s| s.throttle), Some(0.5));
    assert!(prefill.windows(2).all(|w| w[1].tick == w[0].tick + 1));
    wait_for(&mut view, &mut sim, Some(frame(300)), "session info", |v| {
        v.session().is_some()
    });
    assert_eq!(
        view.session().and_then(|s| s.weekend.sub_session_id),
        Some(12345)
    );

    // Playing on adds every frame; pausing adds none.
    for i in 301..=330 {
        sim.tick(Some(frame(i)));
    }
    wait_for(&mut view, &mut sim, Some(frame(330)), "frame 330", |v| {
        throttle(v) == Some(0.55)
    });
    let played = drain(&mut view);
    assert_eq!(played.len(), 30);
    assert_eq!(played.first().map(|s| s.tick), Some(1301));
    for _ in 0..10 {
        sim.tick(Some(frame(330)));
    }
    thread::sleep(Duration::from_millis(50));
    view.poll();
    assert!(drain(&mut view).is_empty(), "paused");

    // A jump back shows the frame there at once, and the trace restarts when the replay plays on.
    wait_for(&mut view, &mut sim, Some(frame(60)), "frame 60", |v| {
        throttle(v) == Some(0.1)
    });
    assert_eq!(view.source(), Some(Source::Capture { first: true }));
    assert!(drain(&mut view).is_empty(), "no samples going back");
    for i in 61..=65 {
        sim.tick(Some(frame(i)));
    }
    wait_for(&mut view, &mut sim, Some(frame(65)), "frame 65", |v| {
        throttle(v) == Some(65.0 / 600.0)
    });
    assert_eq!(view.source(), Some(Source::Capture { first: false }));
    let restarted = drain(&mut view);
    assert_eq!(restarted.len(), 66, "from the start of the capture");
    assert_eq!(restarted.first().map(|s| s.tick), Some(1000));

    // Outside the capture, the live values show, in the replay state: widgets without their own source for
    // replays hide.
    wait_for(&mut view, &mut sim, Some(START - 30.0), "outside", |v| {
        v.display() == Display::Replay
    });
    assert_eq!(view.source(), Some(Source::SimAgain));

    // After the replay, live values again.
    wait_for(&mut view, &mut sim, None, "live again", |v| {
        v.display() == Display::Driving && throttle(v) == Some(0.9)
    });
    let _ = std::fs::remove_dir_all(folder);
}

fn view_missed(view: &View) -> u64 {
    view.stats()
        .missed
        .load(std::sync::atomic::Ordering::Relaxed)
}

fn ticks(view: &View) -> u64 {
    view.stats()
        .ticks
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Publishes one tick at `replay_time` and waits for the data thread to handle it.
fn replay_tick(view: &mut View, sim: &mut Sim, replay_time: f64) {
    let before = ticks(view);
    sim.tick(Some(replay_time));
    let deadline = Instant::now() + Duration::from_secs(2);
    while ticks(view) == before {
        assert!(Instant::now() < deadline, "the data thread reads the tick");
        thread::sleep(Duration::from_micros(200));
    }
    thread::sleep(Duration::from_millis(2));
}

/// While the replay plays, the trace gets one sample per tick however the replay clock moves.
#[test]
fn playing_replays_add_one_sample_per_tick() {
    let folder = capture_folder("ticks");
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotReplaySpeedMap-{pid}"),
        &format!("Local\\PitotReplaySpeedEvent-{pid}"),
    );
    let mut sim = Sim {
        publisher: sim_publisher(&names, true),
        time: 500.0,
        speed: Some(1),
    };
    let config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (_pipeline, mut view) =
        Pipeline::start(names, config, vec![folder.clone()], None).expect("starts");
    wait_for(&mut view, &mut sim, None, "live driving", |v| {
        v.display() == Display::Driving && v.session().is_some()
    });
    wait_for(&mut view, &mut sim, Some(frame(300)), "the capture", |v| {
        v.source() == Some(Source::Capture { first: true })
    });
    view.poll();
    drain(&mut view);
    // The restart above may miss a tick in a debug build; the ticks below may not.
    let missed_before = view_missed(&view);

    // 1x with the sim's uneven clock: 0.8 to 1.4 frames per tick, sometimes 0 or 2.
    let steps = [0.8, 1.2, 1.4, 0.6, 1.0, 2.0, 0.0, 1.0];
    let mut t = 300.0;
    for i in 0..64 {
        t += steps[i % steps.len()];
        replay_tick(&mut view, &mut sim, START + t / 60.0);
    }
    view.poll();
    let played = drain(&mut view);
    assert_eq!(played.len(), 64, "one sample per tick");
    assert_eq!(
        played.last().map(|s| s.throttle),
        throttle(&view),
        "the last sample is the frame shown"
    );

    // Paused: none.
    sim.speed = Some(0);
    for _ in 0..10 {
        replay_tick(&mut view, &mut sim, START + t / 60.0);
    }
    view.poll();
    assert!(drain(&mut view).is_empty(), "paused");

    // Slow motion at a quarter: still one per tick, each frame four times.
    sim.speed = Some(4);
    for _ in 0..20 {
        t += 0.25;
        replay_tick(&mut view, &mut sim, START + t / 60.0);
    }
    view.poll();
    let slow = drain(&mut view);
    assert_eq!(slow.len(), 20);
    let distinct = slow
        .windows(2)
        .filter(|w| w[0].throttle != w[1].throttle)
        .count()
        + 1;
    assert!((5..=6).contains(&distinct), "{distinct} frames in 20 ticks");
    assert_eq!(view_missed(&view), missed_before, "every tick was read");
    let _ = std::fs::remove_dir_all(folder);
}
