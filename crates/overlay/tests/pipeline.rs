//! Telemetry published into shared memory reaches the render thread's view with every input sample.
#![cfg(windows)]

use std::thread;
use std::time::{Duration, Instant};

use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{ReaderConfig, SdkNames};
use pitot_irsdk::publish::{Publisher, PublisherConfig};
use pitot_overlay::pipeline::{Display, InputSample, Pipeline, SAMPLE_CAPACITY, View};

const ROW: usize = 28 + 72 * 4;
const TICKS: u32 = 120;

fn row(n: u32) -> [u8; ROW] {
    let mut row = [0u8; ROW];
    row[0..8].copy_from_slice(&(f64::from(n) / 60.0).to_le_bytes());
    row[8..12].copy_from_slice(&(n as f32 / TICKS as f32).to_le_bytes());
    row[12..16].copy_from_slice(&0.25f32.to_le_bytes());
    row[16..20].copy_from_slice(&1.0f32.to_le_bytes());
    row[20..24].copy_from_slice(&(-0.5f32).to_le_bytes());
    row[24] = u8::from(n.is_multiple_of(10));
    row[25] = 1;
    row[28 + 4 * 5..28 + 4 * 6].copy_from_slice(&0.5f32.to_le_bytes());
    row
}

fn publisher(names: &SdkNames) -> Publisher {
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Float, 8, 1, "ThrottleRaw").expect("valid"),
            VarHeader::new(VarType::Float, 12, 1, "BrakeRaw").expect("valid"),
            VarHeader::new(VarType::Float, 16, 1, "Clutch").expect("valid"),
            VarHeader::new(VarType::Float, 20, 1, "SteeringWheelAngle").expect("valid"),
            VarHeader::new(VarType::Bool, 24, 1, "BrakeABSactive").expect("valid"),
            VarHeader::new(VarType::Bool, 25, 1, "IsOnTrack").expect("valid"),
            VarHeader::new(VarType::Float, 28, 72, "CarIdxLapDistPct").expect("valid"),
        ],
        buf_len: ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher
}

fn wait_for(view: &mut View, what: &str, mut done: impl FnMut(&mut View) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(view) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn dropped_samples_are_kept_across_connections() {
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotPipelineDropMap-{pid}"),
        &format!("Local\\PitotPipelineDropEvent-{pid}"),
    );
    let mut publisher = publisher(&names);
    let config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (_pipeline, mut view) = Pipeline::start(names, config, Vec::new(), None).expect("starts");
    wait_for(&mut view, "connection", |v| {
        v.poll().connection && v.is_connected()
    });
    // More ticks than the ring holds, without draining: the oldest samples are dropped.
    let extra = 40;
    for n in 0..SAMPLE_CAPACITY as u32 + extra {
        publisher.publish(&row(n)).expect("publishes");
        thread::sleep(Duration::from_millis(1));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while view_ticks(&view) < SAMPLE_CAPACITY as u64 + 1 {
        assert!(
            Instant::now() < deadline,
            "data thread did not read enough ticks"
        );
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(50));
    let read = view_ticks(&view);
    let dropped = view.dropped_samples();
    assert_eq!(dropped, read - SAMPLE_CAPACITY as u64, "read {read}");
    assert_eq!(view.dropped_samples_total(), dropped);

    publisher.set_connected(false).expect("disconnects");
    wait_for(&mut view, "disconnect", |v| {
        v.poll().connection && !v.is_connected()
    });
    assert_eq!(view.dropped_samples(), 0, "no connection");
    assert_eq!(
        view.dropped_samples_total(),
        dropped,
        "kept after disconnect"
    );
    publisher.set_connected(true).expect("reconnects");
    wait_for(&mut view, "reconnection", |v| {
        v.poll().connection && v.is_connected()
    });
    assert_eq!(
        view.dropped_samples_total(),
        dropped,
        "kept after reconnect"
    );
}

fn view_ticks(view: &View) -> u64 {
    view.stats()
        .ticks
        .load(std::sync::atomic::Ordering::Relaxed)
}

#[test]
fn telemetry_and_every_sample_reach_the_view() {
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotPipelineMap-{pid}"),
        &format!("Local\\PitotPipelineEvent-{pid}"),
    );
    let mut publisher = publisher(&names);
    let config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) = Pipeline::start(names, config, Vec::new(), None).expect("starts");
    wait_for(&mut view, "connection", |v| {
        v.poll().connection && v.is_connected()
    });
    assert_eq!(view.car_count(), 72);
    assert_eq!(view.display(), Display::Disconnected, "no tick yet");

    publisher
        .set_session_info(b"WeekendInfo:\n TrackID: 77\n")
        .expect("fits");
    // Session info is picked up on the next tick.
    publisher.publish(&row(0)).expect("publishes");
    wait_for(&mut view, "session info", |v| {
        v.poll();
        v.session().is_some()
    });
    assert_eq!(view.session().and_then(|s| s.weekend.track_id), Some(77));

    let mut samples: Vec<InputSample> = Vec::new();
    view.drain_samples(|s| samples.push(s));
    for n in 1..TICKS {
        publisher.publish(&row(n)).expect("publishes");
        thread::sleep(Duration::from_millis(16));
        // Poll on every other tick, as a slower frame would, so samples must queue.
        if n % 2 == 0 {
            view.poll();
            view.drain_samples(|s| samples.push(s));
        }
    }
    wait_for(&mut view, "last tick", |v| {
        v.poll();
        v.drain_samples(|s| samples.push(s));
        samples.len() == TICKS as usize
    });
    assert!(
        samples.windows(2).all(|w| w[1].tick == w[0].tick + 1),
        "every sample in order"
    );
    assert!(samples.iter().all(|s| s.missed_before == 0));
    let last = samples.last().copied().expect("samples");
    assert_eq!(last.throttle, (TICKS - 1) as f32 / TICKS as f32);
    assert_eq!((last.brake, last.clutch, last.steering), (0.25, 1.0, -0.5));
    assert_eq!(samples.iter().filter(|s| s.abs_active).count(), 12);
    assert_eq!(view.dropped_samples(), 0);

    let vars = *view.vars();
    let snapshot = view.snapshot();
    assert_eq!(
        snapshot.f64(vars.session_time, 0),
        Some(f64::from(TICKS - 1) / 60.0)
    );
    assert_eq!(snapshot.f32(vars.car_lap_dist_pct, 5), Some(0.5));
    assert_eq!(snapshot.f32(vars.speed, 0), None, "not published");
    assert_eq!(view.display(), Display::Driving);
    assert_eq!(
        view.stats()
            .session_parses
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );

    publisher.set_connected(false).expect("disconnects");
    wait_for(&mut view, "disconnect", |v| {
        v.poll().connection && !v.is_connected()
    });
    assert_eq!(view.display(), Display::Disconnected);

    let stopping = Instant::now();
    drop(pipeline);
    assert!(
        stopping.elapsed() < Duration::from_secs(2),
        "threads stop promptly"
    );
}
