//! The data thread and the render thread's view do not allocate per tick, live or while a capture plays during
//! a replay. Run with `--features alloc-check`.
#![cfg(all(windows, feature = "alloc-check"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use pitot_irsdk::capture::test_capture::{Item, build};
use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{ReaderConfig, SdkNames};
use pitot_irsdk::publish::{Publisher, PublisherConfig};
use pitot_overlay::pipeline::{Pipeline, RecordingConfig, Source, View};

/// Counts allocations on every thread while switched on: the data thread is internal to the pipeline.
struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
/// The count covers the whole process, so the tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

fn record() {
    if COUNTING.load(Ordering::Relaxed) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards to the system allocator with the caller's arguments unchanged, so the
// allocator contract is upheld by `System`; counting touches only atomics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract for `layout`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract for `layout`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        // SAFETY: `ptr` was allocated by this allocator with `layout`, as the caller guarantees.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by this allocator with `layout`, as the caller guarantees.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const ROW: usize = 16 + 72 * 4 * 3;

#[test]
fn pipeline_does_not_allocate_per_tick() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocMap-{pid}"),
        &format!("Local\\PitotOverlayAllocEvent-{pid}"),
    );
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Float, 8, 1, "ThrottleRaw").expect("valid"),
            VarHeader::new(VarType::Bool, 12, 1, "IsOnTrack").expect("valid"),
            VarHeader::new(VarType::Float, 16, 72, "CarIdxLapDistPct").expect("valid"),
            VarHeader::new(VarType::Float, 16 + 288, 72, "CarIdxEstTime").expect("valid"),
            VarHeader::new(VarType::Int, 16 + 576, 72, "CarIdxPosition").expect("valid"),
        ],
        buf_len: ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher
        .set_session_info(b"WeekendInfo:\n TrackID: 1\n")
        .expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), None).expect("starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !view.is_connected() {
        assert!(Instant::now() < deadline, "connects");
        view.poll();
        thread::sleep(Duration::from_millis(2));
    }

    let mut row = [0u8; ROW];
    let mut samples = 0usize;
    fn cycle(publisher: &mut Publisher, view: &mut View, row: &mut [u8; ROW], n: u32) -> usize {
        row[0..8].copy_from_slice(&f64::from(n).to_le_bytes());
        publisher.publish(row).expect("publishes");
        // Give the data thread time to read the tick, as the sim's 60 Hz would.
        thread::sleep(Duration::from_millis(4));
        view.poll();
        view.drain_samples(|_| {})
    }
    // Warm-up includes the first session info parse.
    for n in 0..120 {
        samples += cycle(&mut publisher, &mut view, &mut row, n);
    }
    while view.session().is_none() {
        assert!(Instant::now() < deadline, "session info parsed");
        samples += cycle(&mut publisher, &mut view, &mut row, 0);
    }

    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let before = samples;
    for n in 120..720 {
        samples += cycle(&mut publisher, &mut view, &mut row, n);
    }
    COUNTING.store(false, Ordering::Relaxed);

    assert!(
        samples - before >= 590,
        "the pipeline kept up: {} samples",
        samples - before
    );
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the data thread and view"
    );
    drop(pipeline);
}

const SESSION_INFO: &[u8] = b"WeekendInfo:\n TrackID: 1\n SubSessionID: 4242\n";
const REPLAY_ROW: usize = 32 + 72 * 4;

#[test]
fn playing_a_capture_does_not_allocate_per_tick() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    // A capture of 2,000 frames from session time 100 s, in ten-second chunks.
    let captured = vec![
        VarHeader::new(VarType::Int, 0, 1, "SessionNum").expect("valid"),
        VarHeader::new(VarType::Double, 8, 1, "SessionTime").expect("valid"),
        VarHeader::new(VarType::Float, 16, 1, "ThrottleRaw").expect("valid"),
        VarHeader::new(VarType::Bool, 20, 1, "IsOnTrack").expect("valid"),
        VarHeader::new(VarType::Float, 24, 72, "CarIdxLapDistPct").expect("valid"),
    ];
    let capture_row = 24 + 72 * 4;
    let mut items = vec![Item::Session(1, SESSION_INFO.to_vec())];
    for i in 0..2000 {
        let mut row = vec![0u8; capture_row];
        row[8..16].copy_from_slice(&(100.0 + f64::from(i) / 60.0).to_le_bytes());
        row[16..20].copy_from_slice(&(i as f32 / 2000.0).to_le_bytes());
        row[20] = 1;
        items.push(Item::Frame(i, row));
    }
    let folder = std::env::temp_dir().join(format!("pitot-alloc-replay-{pid}"));
    std::fs::create_dir_all(&folder).expect("creates the folder");
    std::fs::write(
        folder.join("race.ptc"),
        build(&captured, capture_row, 600, &items),
    )
    .expect("writes");

    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocReplayMap-{pid}"),
        &format!("Local\\PitotOverlayAllocReplayEvent-{pid}"),
    );
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Double, 8, 1, "ReplaySessionTime").expect("valid"),
            VarHeader::new(VarType::Int, 16, 1, "ReplaySessionNum").expect("valid"),
            VarHeader::new(VarType::Bool, 20, 1, "IsReplayPlaying").expect("valid"),
            VarHeader::new(VarType::Int, 24, 1, "ReplayPlaySpeed").expect("valid"),
            VarHeader::new(VarType::Float, 32, 72, "CarIdxLapDistPct").expect("valid"),
        ],
        buf_len: REPLAY_ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher.set_session_info(SESSION_INFO).expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, vec![folder.clone()], None).expect("starts");

    let mut row = [0u8; REPLAY_ROW];
    row[20] = 1;
    row[24..28].copy_from_slice(&1i32.to_le_bytes());
    fn cycle(
        publisher: &mut Publisher,
        view: &mut View,
        row: &mut [u8; REPLAY_ROW],
        n: u32,
    ) -> usize {
        // Live time runs on; the replay shows frame `n` of the capture.
        row[0..8].copy_from_slice(&(1000.0 + f64::from(n) / 60.0).to_le_bytes());
        row[8..16].copy_from_slice(&(100.0 + f64::from(n) / 60.0).to_le_bytes());
        publisher.publish(row).expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
        view.drain_samples(|_| {})
    }
    // Warm-up: the live session info, the search, the first restart, and its session info.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut n = 0;
    while !(view.source() == Some(Source::Capture { first: true }) && view.session().is_some())
        || n < 400
    {
        assert!(Instant::now() < deadline, "the capture plays");
        cycle(&mut publisher, &mut view, &mut row, n);
        n += 1;
    }

    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut samples = 0;
    for _ in 0..600 {
        samples += cycle(&mut publisher, &mut view, &mut row, n);
        n += 1;
    }
    COUNTING.store(false, Ordering::Relaxed);

    assert!(samples >= 590, "one sample per replayed tick: {samples}");
    assert_eq!(
        view.source(),
        Some(Source::Capture { first: true }),
        "no restart"
    );
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the data thread and view"
    );
    drop(pipeline);
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn recording_does_not_allocate_per_tick() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocRecordMap-{pid}"),
        &format!("Local\\PitotOverlayAllocRecordEvent-{pid}"),
    );
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Float, 8, 1, "ThrottleRaw").expect("valid"),
            VarHeader::new(VarType::Bool, 12, 1, "IsOnTrack").expect("valid"),
            VarHeader::new(VarType::Float, 16, 72, "CarIdxLapDistPct").expect("valid"),
            VarHeader::new(VarType::Float, 16 + 288, 72, "CarIdxEstTime").expect("valid"),
            VarHeader::new(VarType::Int, 16 + 576, 72, "CarIdxPosition").expect("valid"),
        ],
        buf_len: ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher
        .set_session_info(b"WeekendInfo:\n TrackID: 1\n")
        .expect("fits");
    let dir = std::env::temp_dir().join(format!("pitot-alloc-record-{pid}"));
    let recording = RecordingConfig {
        dir: dir.clone(),
        max_bytes: 1 << 30,
    };
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), Some(recording)).expect("starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !view.is_connected() {
        assert!(Instant::now() < deadline, "connects");
        view.poll();
        thread::sleep(Duration::from_millis(2));
    }
    let mut row = [0u8; ROW];
    // Slower than the other tests: the recorder drains once a second, and its queue holds two seconds of 60 Hz
    // ticks.
    let mut cycle = |n: u32| -> usize {
        row[0..8].copy_from_slice(&f64::from(n).to_le_bytes());
        row[12] = 1;
        publisher.publish(&row).expect("publishes");
        thread::sleep(Duration::from_millis(16));
        view.poll();
        view.drain_samples(|_| {})
    };
    // Warm-up: the buffer pool fills, and chunks are compressed and written.
    for n in 0..400 {
        cycle(n);
    }
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut samples = 0;
    for n in 400..1000 {
        samples += cycle(n);
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert!(samples >= 590, "the pipeline kept up: {samples}");
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the data, recorder, and render threads"
    );
    drop(pipeline);
    let _ = std::fs::remove_dir_all(dir);
}

const WIND_ROW: usize = 32;

#[test]
fn wind_widget_does_not_allocate_per_update() {
    use pitot_overlay::config::WidgetConfig;
    use pitot_overlay::widgets::{MAX_LAYERS, Redraw, Widget, WindWidget};

    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocWindMap-{pid}"),
        &format!("Local\\PitotOverlayAllocWindEvent-{pid}"),
    );
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Bool, 8, 1, "IsOnTrack").expect("valid"),
            VarHeader::new(VarType::Float, 12, 1, "WindVel").expect("valid"),
            VarHeader::new(VarType::Float, 16, 1, "WindDir").expect("valid"),
            VarHeader::new(VarType::Float, 20, 1, "YawNorth").expect("valid"),
        ],
        buf_len: WIND_ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher
        .set_session_info(b"WeekendInfo:\n TrackID: 1\n")
        .expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), None).expect("starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !view.is_connected() {
        assert!(Instant::now() < deadline, "connects");
        view.poll();
        thread::sleep(Duration::from_millis(2));
    }
    let mut widget = WindWidget::new(&WidgetConfig {
        enabled: true,
        monitor: None,
        x: 0,
        y: 0,
        width: None,
        height: None,
        rate_hz: None,
        opacity: 0.82,
    });
    let mut row = [0u8; WIND_ROW];
    // The car turns through every heading and the wind changes, so most updates have something new to show.
    let mut cycle = |n: u32| -> bool {
        row[0..8].copy_from_slice(&f64::from(n).to_le_bytes());
        row[8] = 1;
        row[12..16].copy_from_slice(&(3.0 + (n % 50) as f32 * 0.1).to_le_bytes());
        row[16..20].copy_from_slice(&(4.7f32 + (n % 7) as f32 * 0.01).to_le_bytes());
        row[20..24].copy_from_slice(&((n as f32 * 0.05) % std::f32::consts::TAU).to_le_bytes());
        publisher.publish(&row).expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
        view.drain_samples(|_| {});
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        widget.update(&mut view, Instant::now(), &mut redraw);
        redraw[0] != Redraw::Nothing
    };
    for n in 0..120 {
        cycle(n);
    }
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut redraws = 0;
    for n in 120..720 {
        redraws += usize::from(cycle(n));
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert!(
        redraws > 300,
        "the reading kept changing: {redraws} redraws"
    );
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the data thread, view, and wind widget"
    );
    drop(pipeline);
}

const FUEL_ROW: usize = 32;

#[test]
fn fuel_widget_does_not_allocate_per_update() {
    use pitot_overlay::config::WidgetConfig;
    use pitot_overlay::widgets::{FuelWidget, MAX_LAYERS, Redraw, Widget};

    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocFuelMap-{pid}"),
        &format!("Local\\PitotOverlayAllocFuelEvent-{pid}"),
    );
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Float, 8, 1, "FuelLevel").expect("valid"),
            VarHeader::new(VarType::Int, 12, 1, "LapCompleted").expect("valid"),
            VarHeader::new(VarType::Float, 16, 1, "LapDistPct").expect("valid"),
            VarHeader::new(VarType::Int, 20, 1, "SessionLapsRemainEx").expect("valid"),
            VarHeader::new(VarType::Bool, 24, 1, "IsOnTrack").expect("valid"),
        ],
        buf_len: FUEL_ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher
        .set_session_info(
            b"WeekendInfo:\n TrackID: 1\nDriverInfo:\n DriverCarIdx: 0\n DriverCarFuelMaxLtr: 100.000\n DriverCarMaxFuelPct: 0.500\n",
        )
        .expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), None).expect("starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !view.is_connected() {
        assert!(Instant::now() < deadline, "connects");
        view.poll();
        thread::sleep(Duration::from_millis(2));
    }
    let mut widget = FuelWidget::new(&WidgetConfig {
        enabled: true,
        monitor: None,
        x: 0,
        y: 0,
        width: None,
        height: None,
        rate_hz: None,
        opacity: 0.82,
    });
    let mut row = [0u8; FUEL_ROW];
    // A lap every 20 updates at 1 L per lap, 80 laps to go with 48 L and a 50 L tank: the pit window and the
    // estimates move every lap. Returns whether the widget redrew and whether the session info is in.
    let mut cycle = |n: u32| -> (bool, bool) {
        let lap = (n / 20) as i32;
        row[0..8].copy_from_slice(&f64::from(n).to_le_bytes());
        row[8..12].copy_from_slice(&(48.0 - n as f32 * 0.05).to_le_bytes());
        row[12..16].copy_from_slice(&lap.to_le_bytes());
        row[16..20].copy_from_slice(&((n % 20) as f32 / 20.0).to_le_bytes());
        row[20..24].copy_from_slice(&(80 - lap).to_le_bytes());
        row[24] = 1;
        publisher.publish(&row).expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
        view.drain_samples(|_| {});
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        widget.update(&mut view, Instant::now(), &mut redraw);
        (redraw[0] != Redraw::Nothing, view.session().is_some())
    };
    // Warm-up: the session info with the tank size, and two clean laps for the use per lap.
    let mut session = false;
    for n in 0..120 {
        session = cycle(n).1;
    }
    assert!(session, "session info parsed");
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut redraws = 0;
    for n in 120..720 {
        redraws += usize::from(cycle(n).0);
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert!(redraws > 300, "the fuel kept changing: {redraws} redraws");
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the data thread, view, and fuel widget"
    );
    drop(pipeline);
}
