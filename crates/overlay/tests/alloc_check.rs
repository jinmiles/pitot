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
use pitot_overlay::config::Config;
use pitot_overlay::pipeline::{Pipeline, RecordingConfig, Source, View};
use pitot_overlay::widgets::{MAX_LAYERS, Redraw, TiresWidget, Widget};

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
fn tire_widget_updates_do_not_allocate() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocTiresMap-{pid}"),
        &format!("Local\\PitotOverlayAllocTiresEvent-{pid}"),
    );
    // Lap and compound, then per corner wear L/M/R, carcass L/M/R, and distance.
    let mut vars = vec![
        VarHeader::new(VarType::Int, 0, 1, "Lap").expect("valid"),
        VarHeader::new(VarType::Int, 4, 1, "PlayerTireCompound").expect("valid"),
    ];
    let mut at = 8;
    for corner in ["LF", "RF", "LR", "RR"] {
        for name in [
            "wearL", "wearM", "wearR", "tempCL", "tempCM", "tempCR", "odometer",
        ] {
            vars.push(
                VarHeader::new(VarType::Float, at, 1, &format!("{corner}{name}")).expect("valid"),
            );
            at += 4;
        }
    }
    let row_len = at as usize;
    let config = PublisherConfig {
        vars,
        buf_len: row_len,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher
        .set_session_info(
            b"DriverInfo:\n DriverTires:\n - TireIndex: 0\n   TireCompoundType: \"Hard\"\n",
        )
        .expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), None).expect("starts");
    let mut widget = TiresWidget::new(&Config::default().widgets.tires);
    let mut row = vec![0u8; row_len];
    // Each cycle the tires run on, and every hundredth one is a pit stop that changes wear and temperatures.
    let mut cycle = |n: u32, widget: &mut TiresWidget, view: &mut View| -> bool {
        row[0..4].copy_from_slice(&((n / 100) as i32).to_le_bytes());
        let stop = (n / 100) as f32;
        for c in 0..4 {
            let base = 8 + c * 28;
            for z in 0..3 {
                let wear = 1.0 - stop * 0.01 - z as f32 * 0.001;
                row[base + z * 4..base + z * 4 + 4].copy_from_slice(&wear.to_le_bytes());
                let temp = 70.0 + stop + z as f32;
                row[base + 12 + z * 4..base + 16 + z * 4].copy_from_slice(&temp.to_le_bytes());
            }
            let meters = n as f32 * 100.0;
            row[base + 24..base + 28].copy_from_slice(&meters.to_le_bytes());
        }
        publisher.publish(&row).expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        widget.update(view, Instant::now(), &mut redraw);
        redraw[0] != Redraw::Nothing
    };
    // Warm-up includes the session info, which names the compound.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut n = 0;
    while view.session().is_none() || n < 150 {
        assert!(Instant::now() < deadline, "session info parsed");
        cycle(n, &mut widget, &mut view);
        n += 1;
    }

    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut redraws = 0;
    for _ in 0..600 {
        redraws += usize::from(cycle(n, &mut widget, &mut view));
        n += 1;
    }
    COUNTING.store(false, Ordering::Relaxed);

    assert!(
        redraws >= 300,
        "the values kept changing: {redraws} redraws"
    );
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the pipeline and the tire widget's updates"
    );
    drop(pipeline);
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

const SECTORS_SESSION: &[u8] = b"---
WeekendInfo:
 TrackID: 1
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionType: Race
   ResultsPositions:
   - Position: 1
     ClassPosition: 0
     CarIdx: 0
   - Position: 2
     ClassPosition: 1
     CarIdx: 1
   - Position: 3
     ClassPosition: 2
     CarIdx: 2
DriverInfo:
 DriverCarIdx: 1
 Drivers:
 - CarIdx: 0
   CarNumber: 1
   CarClassID: 5
 - CarIdx: 1
   CarNumber: 2
   CarClassID: 5
 - CarIdx: 2
   CarNumber: 3
   CarClassID: 5
SplitTimeInfo:
 Sectors:
 - SectorNum: 0
   SectorStartPct: 0.000000
 - SectorNum: 1
   SectorStartPct: 0.300000
 - SectorNum: 2
   SectorStartPct: 0.700000
...
";
const SECTORS_ROW: usize = 16 + 72 * 4 * 2;

#[test]
fn sectors_widget_does_not_allocate_per_update() {
    use pitot_overlay::config::WidgetConfig;
    use pitot_overlay::widgets::{MAX_LAYERS, Redraw, SectorsWidget, Widget};

    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocSectorsMap-{pid}"),
        &format!("Local\\PitotOverlayAllocSectorsEvent-{pid}"),
    );
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Int, 8, 1, "PlayerCarIdx").expect("valid"),
            VarHeader::new(VarType::Float, 16, 72, "CarIdxLapDistPct").expect("valid"),
            VarHeader::new(VarType::Int, 16 + 288, 72, "CarIdxTrackSurface").expect("valid"),
        ],
        buf_len: SECTORS_ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher.set_session_info(SECTORS_SESSION).expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), None).expect("starts");
    let widget_config = WidgetConfig {
        enabled: true,
        monitor: None,
        x: 0,
        y: 0,
        width: Some(400),
        height: Some(200),
        rate_hz: None,
        opacity: 0.8,
    };
    let mut widget = SectorsWidget::new(&widget_config);
    let mut redraw = [Redraw::Nothing; MAX_LAYERS];

    let mut row = [0u8; SECTORS_ROW];
    row[8..12].copy_from_slice(&1i32.to_le_bytes());
    for car in 0..72 {
        let surface = if car < 3 { 3i32 } else { -1 };
        let at = 16 + 288 + car * 4;
        row[at..at + 4].copy_from_slice(&surface.to_le_bytes());
    }
    // Three cars a little apart on 10 s laps, updated every quarter of a second of session time: sectors are
    // crossed and compared all the time.
    let mut cycle = |n: u32, view: &mut View, widget: &mut SectorsWidget| {
        let t = f64::from(n) * 0.25;
        row[0..8].copy_from_slice(&t.to_le_bytes());
        for (car, lap, offset) in [(0usize, 9.8f64, 1.0f64), (1, 10.0, 0.5), (2, 10.3, 0.0)] {
            let pct = (((t + offset) % lap) / lap) as f32;
            let at = 16 + car * 4;
            row[at..at + 4].copy_from_slice(&pct.to_le_bytes());
        }
        publisher.publish(&row).expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
        view.drain_samples(|_| {});
        widget.update(view, Instant::now(), &mut redraw);
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut n = 0;
    // Warm-up: the session info, per-car clocks, and the first laps.
    while view.session().is_none() || n < 200 {
        assert!(Instant::now() < deadline, "session info parsed");
        cycle(n, &mut view, &mut widget);
        n += 1;
    }

    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    for _ in 0..400 {
        cycle(n, &mut view, &mut widget);
        n += 1;
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the pipeline and the sectors widget"
    );
    drop(pipeline);
}

const MAP_CARS: usize = 8;
const MAP_ROW: usize = 36 + MAP_CARS * 9;
/// A round track of radius 300 m: one lap at 50 m/s is about 2,262 ticks.
const MAP_RADIUS: f64 = 300.0;
const MAP_SPEED: f64 = 50.0;
const MAP_SESSION: &[u8] = b"WeekendInfo:
 TrackID: 77
 TrackLength: 1.88 km
DriverInfo:
 DriverCarIdx: 0
 Drivers:
 - CarIdx: 0
   CarClassColor: 0xffda59
 - CarIdx: 1
   CarClassColor: 0xffda59
 - CarIdx: 2
   CarClassColor: 0x33ceff
";

/// The track map maps a lap and then moves the cars' dots without allocating per update.
#[test]
fn track_map_does_not_allocate_per_update() {
    use pitot_overlay::config::WidgetConfig;
    use pitot_overlay::widgets::{MAX_LAYERS, Redraw, TrackMapWidget, Widget};

    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocMapMap-{pid}"),
        &format!("Local\\PitotOverlayAllocMapEvent-{pid}"),
    );
    let at = |k: usize| (36 + k * MAP_CARS * 4) as i32;
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Bool, 8, 1, "IsOnTrack").expect("valid"),
            VarHeader::new(VarType::Bool, 9, 1, "OnPitRoad").expect("valid"),
            VarHeader::new(VarType::Int, 12, 1, "PlayerTrackSurface").expect("valid"),
            VarHeader::new(VarType::Int, 16, 1, "PlayerCarIdx").expect("valid"),
            VarHeader::new(VarType::Float, 20, 1, "VelocityX").expect("valid"),
            VarHeader::new(VarType::Float, 24, 1, "VelocityY").expect("valid"),
            VarHeader::new(VarType::Float, 28, 1, "YawNorth").expect("valid"),
            VarHeader::new(VarType::Float, 32, 1, "LapDistPct").expect("valid"),
            VarHeader::new(VarType::Float, at(0), MAP_CARS as i32, "CarIdxLapDistPct")
                .expect("valid"),
            VarHeader::new(VarType::Int, at(1), MAP_CARS as i32, "CarIdxTrackSurface")
                .expect("valid"),
            VarHeader::new(VarType::Bool, at(2), MAP_CARS as i32, "CarIdxOnPitRoad")
                .expect("valid"),
        ],
        buf_len: MAP_ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher.set_session_info(MAP_SESSION).expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), None).expect("starts");
    let folder = std::env::temp_dir().join(format!("pitot-alloc-trackmap-{pid}"));
    let _ = std::fs::remove_dir_all(&folder);
    let widget_config = WidgetConfig {
        enabled: true,
        monitor: None,
        x: 0,
        y: 0,
        width: Some(440),
        height: Some(330),
        rate_hz: None,
        opacity: 0.8,
    };
    let mut widget = TrackMapWidget::with_folder(&widget_config, Some(folder.clone()));

    // The player drives the circle counterclockwise from half a lap; the other cars follow at fixed distances.
    let lap_ticks = 2.0 * std::f64::consts::PI * MAP_RADIUS / MAP_SPEED * 60.0;
    let mut row = vec![0u8; MAP_ROW];
    let mut cycle =
        |n: u32, widget: &mut TrackMapWidget, view: &mut View| -> [Redraw; MAX_LAYERS] {
            let pct = (0.5 + f64::from(n) / lap_ticks).fract();
            let heading = (std::f64::consts::FRAC_PI_2 - pct * std::f64::consts::TAU)
                .rem_euclid(std::f64::consts::TAU);
            row[0..8].copy_from_slice(&(f64::from(n) / 60.0).to_le_bytes());
            row[8] = 1;
            row[12..16].copy_from_slice(&3i32.to_le_bytes());
            row[20..24].copy_from_slice(&(MAP_SPEED as f32).to_le_bytes());
            row[28..32].copy_from_slice(&(heading as f32).to_le_bytes());
            row[32..36].copy_from_slice(&(pct as f32).to_le_bytes());
            for car in 0..MAP_CARS {
                let at = |k: usize| 36 + k * MAP_CARS * 4 + car * 4;
                let car_pct = (pct + car as f64 * 0.03).fract() as f32;
                row[at(0)..at(0) + 4].copy_from_slice(&car_pct.to_le_bytes());
                row[at(1)..at(1) + 4].copy_from_slice(&3i32.to_le_bytes());
                row[36 + 2 * MAP_CARS * 4 + car] = u8::from(car == 2);
            }
            publisher.publish(&row).expect("publishes");
            thread::sleep(Duration::from_millis(2));
            view.poll();
            view.drain_samples(|_| {});
            let mut redraw = [Redraw::Nothing; MAX_LAYERS];
            // The widget runs at 10 Hz: every sixth tick.
            if n.is_multiple_of(6) {
                widget.update(view, Instant::now(), &mut redraw);
            }
            redraw
        };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut n = 0;
    while view.session().is_none() || n < 120 {
        assert!(Instant::now() < deadline, "session info parsed");
        cycle(n, &mut widget, &mut view);
        n += 1;
    }

    // Mapping: the line comes at tick 1,131, and the lap closes about 2,262 ticks later.
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    while n < 3_000 {
        cycle(n, &mut widget, &mut view);
        n += 1;
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations while mapping"
    );

    // The lap closes; the outline is saved on its own thread.
    let file = folder.join("77.trk");
    while n < 3_600 || !file.exists() {
        assert!(n < 6_000, "the lap was mapped and saved");
        cycle(n, &mut widget, &mut view);
        n += 1;
    }
    thread::sleep(Duration::from_millis(100));

    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut dots_drawn = 0;
    for _ in 0..600 {
        if cycle(n, &mut widget, &mut view)[1] != Redraw::Nothing {
            dots_drawn += 1;
        }
        n += 1;
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert!(dots_drawn >= 90, "the dots move: {dots_drawn} redraws");
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations while moving the dots"
    );
    drop(pipeline);
    let _ = std::fs::remove_dir_all(folder);
}
