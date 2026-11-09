//! The data thread and the render thread's view do not allocate per tick. Run with `--features alloc-check`.
#![cfg(all(windows, feature = "alloc-check"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{ReaderConfig, SdkNames};
use pitot_irsdk::publish::{Publisher, PublisherConfig};
use pitot_overlay::pipeline::{Pipeline, View};

/// Counts allocations on every thread while switched on: the data thread is internal to the pipeline.
struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
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
    let (pipeline, mut view) = Pipeline::start(names, reader_config).expect("starts");
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
