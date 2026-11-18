//! The data thread and the scheduler's view do not allocate per tick, live or while a capture plays during a
//! replay, and widgets do not allocate per update, also after being resized or laid out at another text size.
//! Drawing is GPUI's and allocates there; `pitot --features alloc-count` measures it in the running overlay.
//! Run with `--features alloc-check`.
#![cfg(all(windows, feature = "alloc-check"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant};

use pitot_irsdk::capture::test_capture::{Item, build};
use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{ReaderConfig, SdkNames};
use pitot_irsdk::publish::{Publisher, PublisherConfig};
use pitot_overlay::config::{self, Config};
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
    // The scheduler's side of the wake-up: a waiting task that asks for every tick, as it does while a widget is
    // due by the next tick. Waking it from the data thread must not allocate either.
    let woken = Arc::new(Woken(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&woken));
    let cycle =
        |publisher: &mut Publisher, view: &mut View, row: &mut [u8; ROW], n: u32| -> usize {
            let wake = Arc::clone(view.wake());
            let mut waiting = wake.wait();
            let _ = Pin::new(&mut waiting).poll(&mut Context::from_waker(&waker));
            wake.arm_ticks(true);
            row[0..8].copy_from_slice(&f64::from(n).to_le_bytes());
            publisher.publish(row).expect("publishes");
            // Give the data thread time to read the tick, as the sim's 60 Hz would.
            thread::sleep(Duration::from_millis(4));
            let _ = Pin::new(&mut waiting).poll(&mut Context::from_waker(&waker));
            view.poll();
            view.drain_samples(|_| {})
        };
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
    assert!(
        woken.0.load(Ordering::Relaxed) >= 590,
        "ticks woke the waiting task: {}",
        woken.0.load(Ordering::Relaxed)
    );
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the data thread, its wake-ups, and view"
    );
    drop(pipeline);
}

/// Counts wake-ups of a task; an `Arc` waker wakes and clones without allocating.
struct Woken(AtomicUsize);

impl std::task::Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
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
            VarHeader::new(VarType::Int, 28, 1, "SessionNum").expect("valid"),
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
    // The sim's telemetry file of the session, for the tire widget's surface temperatures.
    let telemetry = folder.join("telemetry");
    std::fs::create_dir_all(&telemetry).expect("creates the folder");
    std::fs::write(
        telemetry.join("stint.ibt"),
        telemetry_file(4242, 1, 100.0, 40.0),
    )
    .expect("writes");
    view.set_telemetry_folders(vec![telemetry]);
    let mut tires = TiresWidget::new(Config::default().widget("tires").expect("tires"));
    let mut update = |view: &mut View| -> bool {
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        tires.update(view, Instant::now(), &mut redraw);
        redraw[0] != Redraw::Nothing
    };

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
    // Warm-up: the live session info, the searches, the first restart, and its session info. The tire widget
    // asks for the telemetry files once the live session info has named the session.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut n = 0;
    let mut named = false;
    while !(view.source() == Some(Source::Capture { first: true }) && view.session().is_some())
        || n < 400
    {
        assert!(Instant::now() < deadline, "the capture plays");
        cycle(&mut publisher, &mut view, &mut row, n);
        named |= view.session().is_some();
        if named {
            update(&mut view);
        }
        n += 1;
    }

    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut samples = 0;
    let mut redraws = 0;
    for _ in 0..600 {
        samples += cycle(&mut publisher, &mut view, &mut row, n);
        redraws += usize::from(update(&mut view));
        n += 1;
    }
    COUNTING.store(false, Ordering::Relaxed);

    assert!(samples >= 590, "one sample per replayed tick: {samples}");
    // The file's temperatures rise a degree a second: ten seconds of replay change them about ten times.
    assert!(
        redraws >= 5,
        "the tire widget follows the replay: {redraws} redraws"
    );
    assert_eq!(
        view.source(),
        Some(Source::Capture { first: true }),
        "no restart"
    );
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the data thread, view, and tire widget"
    );
    drop(pipeline);
    let _ = std::fs::remove_dir_all(folder);
}

/// A minimal `.ibt` of session 0 at 60 Hz from `start` for `seconds`: session time and number, and surface
/// temperatures that rise a degree a second.
fn telemetry_file(sub_session: i64, track: i64, start: f64, seconds: f64) -> Vec<u8> {
    use pitot_irsdk::layout::{DiskSubHeader, Header, SDK_VERSION, STATUS_CONNECTED};
    let mut vars = vec![
        VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
        VarHeader::new(VarType::Int, 8, 1, "SessionNum").expect("valid"),
    ];
    for (i, corner) in ["LF", "RF", "LR", "RR"].iter().enumerate() {
        for (z, zone) in ["L", "M", "R"].iter().enumerate() {
            let at = 12 + (i * 3 + z) as i32 * 4;
            let name = format!("{corner}temp{zone}");
            vars.push(VarHeader::new(VarType::Float, at, 1, &name).expect("valid"));
        }
    }
    let record_len = 64;
    let yaml = format!("WeekendInfo:\n TrackID: {track}\n SubSessionID: {sub_session}\n");
    let table_at = Header::SIZE + DiskSubHeader::SIZE;
    let info_at = table_at + vars.len() * VarHeader::SIZE;
    let data_at = info_at + yaml.len() + 16;
    let records = (seconds * 60.0) as usize;
    let mut bytes = vec![0u8; data_at + records * record_len];
    let mut header = Header::default();
    header.ver = SDK_VERSION;
    header.status = STATUS_CONNECTED;
    header.tick_rate = 60;
    header.session_info_len = (yaml.len() + 16) as i32;
    header.session_info_offset = info_at as i32;
    header.num_vars = vars.len() as i32;
    header.var_header_offset = table_at as i32;
    header.num_buf = 1;
    header.buf_len = record_len as i32;
    header.var_buf[0].buf_offset = data_at as i32;
    header.write(&mut bytes).expect("header fits");
    DiskSubHeader::default()
        .write(&mut bytes)
        .expect("sub-header fits");
    for (n, var) in vars.iter().enumerate() {
        var.write(&mut bytes[table_at + n * VarHeader::SIZE..])
            .expect("fits");
    }
    bytes[info_at..info_at + yaml.len()].copy_from_slice(yaml.as_bytes());
    for n in 0..records {
        let row = &mut bytes[data_at + n * record_len..][..record_len];
        let t = n as f64 / 60.0;
        row[0..8].copy_from_slice(&(start + t).to_le_bytes());
        for i in 0..12 {
            let temp = 80.0 + t as f32;
            row[12 + i * 4..16 + i * 4].copy_from_slice(&temp.to_le_bytes());
        }
    }
    bytes
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
    let mut widget = TiresWidget::new(Config::default().widget("tires").expect("tires"));
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
    let config = WidgetConfig {
        enabled: true,
        monitor: None,
        x: 0,
        y: 0,
        width: None,
        height: None,
        rate_hz: None,
        opacity: 0.82,
        options: toml::Table::new(),
    };
    // The wind widget's own variables, as the registry resolves them.
    let mut widget = WindWidget::new(&config, view.resolve());
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
        options: toml::Table::new(),
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

const STANDINGS_SESSION: &[u8] = b"---
WeekendInfo:
 TrackID: 1
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionLaps: 20
   SessionType: Race
   ResultsPositions:
   - Position: 1
     ClassPosition: 0
     CarIdx: 0
     LapsComplete: 1
   - Position: 2
     ClassPosition: 1
     CarIdx: 1
     LapsComplete: 1
   - Position: 3
     ClassPosition: 2
     CarIdx: 2
     LapsComplete: 1
QualifyResultsInfo:
 Results:
 - Position: 0
   ClassPosition: 0
   CarIdx: 2
DriverInfo:
 DriverCarIdx: 1
 Drivers:
 - CarIdx: 0
   CarNumber: 1
   CarClassID: 5
   IRating: 2000
 - CarIdx: 1
   CarNumber: 2
   CarClassID: 5
   IRating: 1800
 - CarIdx: 2
   CarNumber: 3
   CarClassID: 5
   IRating: 1500
SplitTimeInfo:
 Sectors:
 - SectorNum: 0
   SectorStartPct: 0.000000
 - SectorNum: 1
   SectorStartPct: 0.500000
...
";
const STANDINGS_ROW: usize = 16 + 72 * 4 * 3;

/// The standings and the relative in a live race: the order changes as cars pass each other and gaps are timed on
/// every update.
#[test]
fn standings_and_relative_do_not_allocate_per_update() {
    use pitot_overlay::config::WidgetConfig;
    use pitot_overlay::widgets::{RelativeWidget, StandingsWidget};

    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocStandingsMap-{pid}"),
        &format!("Local\\PitotOverlayAllocStandingsEvent-{pid}"),
    );
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Int, 8, 1, "PlayerCarIdx").expect("valid"),
            VarHeader::new(VarType::Int, 12, 1, "SessionState").expect("valid"),
            VarHeader::new(VarType::Float, 16, 72, "CarIdxLapDistPct").expect("valid"),
            VarHeader::new(VarType::Int, 16 + 288, 72, "CarIdxLapCompleted").expect("valid"),
            VarHeader::new(VarType::Int, 16 + 576, 72, "CarIdxTrackSurface").expect("valid"),
        ],
        buf_len: STANDINGS_ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher.set_session_info(STANDINGS_SESSION).expect("fits");
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
        width: Some(760),
        height: Some(240),
        rate_hz: None,
        opacity: 0.8,
        options: toml::Table::new(),
    };
    let mut widgets = (
        StandingsWidget::new(&widget_config),
        RelativeWidget::new(&widget_config),
    );
    let mut redraw = [Redraw::Nothing; MAX_LAYERS];

    let mut row = [0u8; STANDINGS_ROW];
    row[8..12].copy_from_slice(&1i32.to_le_bytes());
    row[12..16].copy_from_slice(&4i32.to_le_bytes());
    for car in 0..72 {
        let surface = if car < 3 { 3i32 } else { -1 };
        let at = 16 + 576 + car * 4;
        row[at..at + 4].copy_from_slice(&surface.to_le_bytes());
    }
    // Three cars on slightly different 10 s laps, updated every half second of session time: they keep passing
    // each other and crossing the line.
    let mut cycle = |n: u32, view: &mut View, widgets: &mut (StandingsWidget, RelativeWidget)| {
        let t = f64::from(n) * 0.5;
        row[0..8].copy_from_slice(&t.to_le_bytes());
        for (car, lap, offset) in [(0usize, 10.4f64, 0.6f64), (1, 10.0, 0.3), (2, 9.7, 0.0)] {
            let progress = 1.0 + offset / lap + t / lap;
            let laps = progress.floor();
            let at = 16 + car * 4;
            row[at..at + 4].copy_from_slice(&((progress - laps) as f32).to_le_bytes());
            let at = 16 + 288 + car * 4;
            row[at..at + 4].copy_from_slice(&(laps as i32).to_le_bytes());
        }
        publisher.publish(&row).expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
        view.drain_samples(|_| {});
        widgets.0.update(view, Instant::now(), &mut redraw);
        widgets.1.update(view, Instant::now(), &mut redraw);
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut n = 0;
    // Warm-up: the session info, per-car storage, and the first laps.
    while view.session().is_none() || n < 100 {
        assert!(Instant::now() < deadline, "session info parsed");
        cycle(n, &mut view, &mut widgets);
        n += 1;
    }

    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    for _ in 0..400 {
        cycle(n, &mut view, &mut widgets);
        n += 1;
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the pipeline, the standings, and the relative"
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
 DriverCarEstLapTime: 37.7
 Drivers:
 - CarIdx: 0
   CarClassColor: 0xffda59
 - CarIdx: 1
   CarClassColor: 0xffda59
 - CarIdx: 2
   CarClassColor: 0x33ceff
";

/// The track map maps a lap and then moves the cars' dots and the pit exit without allocating per update.
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
        options: "shape = \"outline\"".parse().expect("a table"),
    };
    // A pit lane loss learned at this track before, so the pit exit shows.
    std::fs::create_dir_all(&folder).expect("creates the folder");
    let pit_store = folder.join("pit.toml");
    std::fs::write(&pit_store, "[lane_loss]\n\"77\" = 25.0\n").expect("writes");
    let mut widget = TrackMapWidget::with_pit_store(
        &widget_config,
        Some(folder.clone()),
        Some(pit_store.clone()),
    );

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

    // The circle, the default: no outline, the dots and the pit exit from the first update.
    let circle_config = WidgetConfig {
        options: toml::Table::new(),
        ..widget_config
    };
    let mut circle = TrackMapWidget::with_pit_store(&circle_config, None, Some(pit_store));
    for _ in 0..60 {
        cycle(n, &mut circle, &mut view);
        n += 1;
    }
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut circle_drawn = 0;
    for _ in 0..600 {
        if cycle(n, &mut circle, &mut view)[1] != Redraw::Nothing {
            circle_drawn += 1;
        }
        n += 1;
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert!(
        circle_drawn >= 90,
        "the dots move on the circle: {circle_drawn} redraws"
    );
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations while moving the dots on the circle"
    );
    drop(pipeline);
    let _ = std::fs::remove_dir_all(folder);
}

const PIT_CARS: usize = 8;
/// Single values first (time, session, player, fuel, laps), then seven per-car arrays and the pit road flags.
const PIT_ARRAYS: usize = 36;
const PIT_ON_PIT_ROAD: usize = PIT_ARRAYS + PIT_CARS * 4 * 7;
const PIT_ROW: usize = PIT_ON_PIT_ROAD + PIT_CARS;
const PIT_SESSION: &[u8] = b"WeekendInfo:
 TrackID: 7
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionLaps: 30
   SessionType: Race
DriverInfo:
 DriverCarIdx: 0
 DriverCarFuelMaxLtr: 40.000
 DriverCarMaxFuelPct: 1.000
 Drivers:
 - CarIdx: 0
   UserName: Driver A
   CarNumber: 1
   CarScreenName: Test Car
   CarClassID: 1
 - CarIdx: 1
   UserName: Driver B
   CarNumber: 2
   CarScreenName: Test Car
   CarClassID: 1
 - CarIdx: 2
   UserName: Driver C
   CarNumber: 3
   CarScreenName: Other Car
   CarClassID: 1
";

/// One 10 Hz tick of a race of three cars, a lap every 10 s. The player (car 0) uses 2 L per lap. Car `stopper`
/// comes onto pit road at tick `start`, stands in its stall from 1 s to 6 s later, and leaves pit road 9 s after
/// coming in, 0.02 lap further on.
fn pit_row(n: u32, stopper: usize, start: u32) -> [u8; PIT_ROW] {
    let mut row = [0u8; PIT_ROW];
    let put = |row: &mut [u8; PIT_ROW], k: usize, car: usize, bytes: [u8; 4]| {
        let at = PIT_ARRAYS + k * PIT_CARS * 4 + car * 4;
        row[at..at + 4].copy_from_slice(&bytes);
    };
    let player = f64::from(n) * 0.01;
    row[0..8].copy_from_slice(&(f64::from(n) * 0.1).to_le_bytes());
    row[20..24].copy_from_slice(&((40.0 - player * 2.0) as f32).to_le_bytes());
    row[24..28].copy_from_slice(&(player as i32).to_le_bytes());
    row[28..32].copy_from_slice(&(player.fract() as f32).to_le_bytes());
    row[32..36].copy_from_slice(&(30 - player as i32).to_le_bytes());
    for car in 0..3 {
        let offset = 0.3 * car as f64;
        let mut distance = offset + player;
        let mut surface = 3i32;
        let mut on_pit = false;
        if car == stopper && n >= start {
            let k = f64::from(n - start);
            let entry = offset + f64::from(start) * 0.01;
            distance = if k < 10.0 {
                entry + k * 0.0005
            } else if k < 60.0 {
                entry + 0.005
            } else if k < 90.0 {
                entry + 0.005 + (k - 60.0) * 0.0005
            } else {
                entry + 0.02 + (k - 90.0) * 0.01
            };
            if k < 90.0 {
                on_pit = true;
                surface = if (10.0..60.0).contains(&k) { 1 } else { 2 };
            }
        }
        put(&mut row, 0, car, (distance.fract() as f32).to_le_bytes());
        put(&mut row, 1, car, surface.to_le_bytes());
        put(&mut row, 2, car, (distance as i32).to_le_bytes());
        put(&mut row, 3, car, 10.0f32.to_le_bytes());
        put(&mut row, 4, car, 1i32.to_le_bytes());
        put(&mut row, 5, car, (car as i32 + 1).to_le_bytes());
        put(&mut row, 6, car, (2.0 * car as f32).to_le_bytes());
        row[PIT_ON_PIT_ROAD + car] = u8::from(on_pit);
    }
    row
}

#[test]
fn pit_widget_does_not_allocate_per_update() {
    use pitot_overlay::config::WidgetConfig;
    use pitot_overlay::widgets::{MAX_LAYERS, PitWidget, Redraw, Widget};

    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocPitMap-{pid}"),
        &format!("Local\\PitotOverlayAllocPitEvent-{pid}"),
    );
    let array = |t, k: usize, name| {
        VarHeader::new(
            t,
            (PIT_ARRAYS + k * PIT_CARS * 4) as i32,
            PIT_CARS as i32,
            name,
        )
        .expect("valid")
    };
    let config = PublisherConfig {
        vars: vec![
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Int, 8, 1, "SessionNum").expect("valid"),
            VarHeader::new(VarType::Int, 12, 1, "PlayerCarIdx").expect("valid"),
            VarHeader::new(VarType::Bool, 16, 1, "PlayerCarInPitStall").expect("valid"),
            VarHeader::new(VarType::Float, 20, 1, "FuelLevel").expect("valid"),
            VarHeader::new(VarType::Int, 24, 1, "LapCompleted").expect("valid"),
            VarHeader::new(VarType::Float, 28, 1, "LapDistPct").expect("valid"),
            VarHeader::new(VarType::Int, 32, 1, "SessionLapsRemainEx").expect("valid"),
            array(VarType::Float, 0, "CarIdxLapDistPct"),
            array(VarType::Int, 1, "CarIdxTrackSurface"),
            array(VarType::Int, 2, "CarIdxLapCompleted"),
            array(VarType::Float, 3, "CarIdxBestLapTime"),
            array(VarType::Int, 4, "CarIdxClass"),
            array(VarType::Int, 5, "CarIdxClassPosition"),
            array(VarType::Float, 6, "CarIdxF2Time"),
            VarHeader::new(
                VarType::Bool,
                PIT_ON_PIT_ROAD as i32,
                PIT_CARS as i32,
                "CarIdxOnPitRoad",
            )
            .expect("valid"),
        ],
        buf_len: PIT_ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher.set_session_info(PIT_SESSION).expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), None).expect("starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while view.session().is_none() {
        assert!(Instant::now() < deadline, "session info parsed");
        publisher
            .publish(&pit_row(0, 0, u32::MAX))
            .expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
    }
    let widget_config = WidgetConfig {
        enabled: true,
        monitor: None,
        x: 0,
        y: 0,
        width: Some(400),
        height: Some(280),
        rate_hz: None,
        opacity: 0.8,
        options: toml::Table::new(),
    };
    let mut widget = PitWidget::with_store(&widget_config, None);
    let mut redraws = 0usize;
    let mut cycle = |n: u32, stopper: usize, start: u32| {
        publisher
            .publish(&pit_row(n, stopper, start))
            .expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        widget.update(&mut view, Instant::now(), &mut redraw);
        redraws += usize::from(redraw[0] != Redraw::Nothing);
    };
    // Warm-up: three laps for the fuel use, and car 1 makes a stop that measures the pit lane loss.
    for n in 1..400 {
        cycle(n, 1, 320);
    }
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    // Car 2, another car model, makes a stop while counting, and the next stops and rejoins are estimated.
    for n in 400..800 {
        cycle(n, 2, 500);
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the data thread and the pit widget's updates"
    );
    assert!(redraws > 0, "the widget had something to draw");
    drop(widget);
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
        options: toml::Table::new(),
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

const EVERY_ROW: usize = 32 + 72 * 4 * 4 + 72;

/// Every widget of the list at its defaults, as the render loop makes them, and the shared gap and lap histories:
/// three cars race on 10 s laps, their lap times arrive a second after the line, the player has incidents and
/// pit laps, and another car runs wide. A widget added to the list is checked here too.
#[test]
fn every_widget_and_the_histories_do_not_allocate_per_update() {
    use pitot_overlay::widgets::AllWidgets;

    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotOverlayAllocEveryMap-{pid}"),
        &format!("Local\\PitotOverlayAllocEveryEvent-{pid}"),
    );
    let array = |k: usize| 32 + k * 288;
    let var =
        |kind, at: usize, count, name| VarHeader::new(kind, at as i32, count, name).expect("valid");
    let config = PublisherConfig {
        vars: vec![
            var(VarType::Double, 0, 1, "SessionTime"),
            var(VarType::Int, 8, 1, "SessionNum"),
            var(VarType::Int, 12, 1, "SessionState"),
            var(VarType::BitField, 16, 1, "SessionFlags"),
            var(VarType::Int, 20, 1, "PlayerCarIdx"),
            var(VarType::Bool, 24, 1, "IsOnTrack"),
            var(VarType::Int, 28, 1, "PlayerCarMyIncidentCount"),
            var(VarType::Float, array(0), 72, "CarIdxLapDistPct"),
            var(VarType::Int, array(1), 72, "CarIdxLapCompleted"),
            var(VarType::Float, array(2), 72, "CarIdxLastLapTime"),
            var(VarType::Int, array(3), 72, "CarIdxTrackSurface"),
            var(VarType::Bool, array(4), 72, "CarIdxOnPitRoad"),
        ],
        buf_len: EVERY_ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 4096,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher.set_session_info(STANDINGS_SESSION).expect("fits");
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(50),
    };
    let (pipeline, mut view) =
        Pipeline::start(names, reader_config, Vec::new(), None).expect("starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !view.is_connected() {
        assert!(Instant::now() < deadline, "connects");
        view.poll();
        thread::sleep(Duration::from_millis(2));
    }
    let mut widgets = AllWidgets::new(&view);
    assert!(widgets.len() >= 10, "every widget is made");

    let mut row = [0u8; EVERY_ROW];
    fn put(row: &mut [u8; EVERY_ROW], at: usize, bytes: [u8; 4]) {
        row[at..at + 4].copy_from_slice(&bytes);
    }
    put(&mut row, 12, 4i32.to_le_bytes());
    put(&mut row, 20, 1i32.to_le_bytes());
    row[24] = 1;
    for car in 3..72 {
        put(&mut row, array(3) + car * 4, (-1i32).to_le_bytes());
    }
    // Updated every half second of session time.
    let mut cycle = |n: u32, view: &mut View, widgets: &mut AllWidgets| -> (usize, usize) {
        let t = f64::from(n) * 0.5;
        row[0..8].copy_from_slice(&t.to_le_bytes());
        put(&mut row, 28, ((n / 50) as i32).to_le_bytes());
        for (car, lap, offset) in [(0usize, 10.4f64, 0.6f64), (1, 10.0, 0.3), (2, 9.7, 0.0)] {
            let progress = 1.0 + offset / lap + t / lap;
            let laps = progress.floor();
            let pct = progress - laps;
            put(&mut row, array(0) + car * 4, (pct as f32).to_le_bytes());
            put(&mut row, array(1) + car * 4, (laps as i32).to_le_bytes());
            // The lap's time a second after the line, a little different every lap.
            let timed = (progress - 1.0 / lap).floor();
            let time = (lap + 0.01 * (timed % 7.0)) as f32;
            put(&mut row, array(2) + car * 4, time.to_le_bytes());
            // The player drives through the pits every fourth lap; car 2 runs wide every third.
            let pit = car == 1 && laps % 4.0 == 0.0 && pct < 0.2;
            row[array(4) + car] = u8::from(pit);
            let wide = car == 2 && laps % 3.0 == 0.0 && (0.4..0.5).contains(&pct);
            let surface: i32 = if wide { 0 } else { 3 };
            put(&mut row, array(3) + car * 4, surface.to_le_bytes());
        }
        publisher.publish(&row).expect("publishes");
        thread::sleep(Duration::from_millis(4));
        view.poll();
        view.drain_samples(|_| {});
        widgets.update(view, Instant::now(), 1)
    };
    let mut n = 0;
    // Warm-up: the session info, per-car storage, and the first laps of every car.
    while view.session().is_none() || n < 200 {
        assert!(Instant::now() < deadline, "session info parsed");
        cycle(n, &mut view, &mut widgets);
        n += 1;
    }
    let (_, laps_before) = cycle(n, &mut view, &mut widgets);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let mut held = (0, 0);
    for _ in 0..400 {
        n += 1;
        held = cycle(n, &mut view, &mut widgets);
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert!(held.0 > 200, "gap samples taken: {}", held.0);
    assert!(
        held.1 >= laps_before + 15,
        "laps kept: {laps_before} then {}",
        held.1
    );
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "allocations across the pipeline, every widget, and the histories"
    );

    // Resized as in *Move widgets*, to their smallest size and to twice the size they were made with: after a
    // short warm-up at the new size, updates do not allocate.
    type Pick = fn((u32, u32), (u32, u32)) -> (u32, u32);
    let sizes: [(&str, Pick); 2] = [
        ("smallest", |min, _| min),
        ("twice", |_, made| (made.0 * 2, made.1 * 2)),
    ];
    for (name, pick) in sizes {
        widgets.resize(pick);
        for _ in 0..40 {
            n += 1;
            cycle(n, &mut view, &mut widgets);
        }
        ALLOCATIONS.store(0, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        for _ in 0..200 {
            n += 1;
            cycle(n, &mut view, &mut widgets);
        }
        COUNTING.store(false, Ordering::Relaxed);
        assert_eq!(
            ALLOCATIONS.load(Ordering::Relaxed),
            0,
            "allocations at the {name} size"
        );
    }

    // At either end of `[general] text_size`, as the slider sets it: the widgets laid out in their boxes as made, in
    // widget pixels. After a short warm-up, updates do not allocate.
    for percent in [config::MAX_TEXT_SIZE, config::MIN_TEXT_SIZE] {
        let scale = config::text_scale(percent);
        let inside = |side: u32| (side as f32 / scale).ceil() as u32;
        widgets.resize(|_, made| (inside(made.0), inside(made.1)));
        for _ in 0..40 {
            n += 1;
            cycle(n, &mut view, &mut widgets);
        }
        ALLOCATIONS.store(0, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        for _ in 0..200 {
            n += 1;
            cycle(n, &mut view, &mut widgets);
        }
        COUNTING.store(false, Ordering::Relaxed);
        assert_eq!(
            ALLOCATIONS.load(Ordering::Relaxed),
            0,
            "allocations at a text size of {percent}%"
        );
    }
    drop(pipeline);
}
