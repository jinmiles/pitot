//! The live read path must not allocate after warm-up. Run with `--features alloc-check`.
#![cfg(all(windows, feature = "alloc-check"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Duration;

use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{LiveReader, ReaderConfig, SdkNames, Snapshot, Step, VarSet};
use pitot_irsdk::publish::{Publisher, PublisherConfig};

/// Counts allocations per thread while counting is switched on, so parallel tests do not interfere.
struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn record() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
    }
}

/// Runs `f` with counting on and returns the number of allocations it made on this thread.
fn count_allocations<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|n| n.set(0));
    COUNTING.with(|c| c.set(true));
    let result = f();
    COUNTING.with(|c| c.set(false));
    (result, ALLOCATIONS.with(Cell::get))
}

#[test]
fn counter_sees_allocations() {
    let (v, allocations) = count_allocations(|| vec![1u8; 64]);
    assert_eq!(v.len(), 64);
    assert_eq!(allocations, 1);
}

// SAFETY: every method forwards to the system allocator with the caller's arguments unchanged, so the
// allocator contract is upheld by `System`; counting touches only const-initialized thread locals.
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

const ROW: usize = 24 + 3 * 72 * 4;

#[test]
fn live_read_path_does_not_allocate() {
    let pid = std::process::id();
    let names = SdkNames::new(
        &format!("Local\\PitotAllocMap-{pid}"),
        &format!("Local\\PitotAllocEvent-{pid}"),
    );
    let vars = vec![
        VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
        VarHeader::new(VarType::Float, 8, 1, "ThrottleRaw").expect("valid"),
        VarHeader::new(VarType::Float, 12, 1, "BrakeRaw").expect("valid"),
        VarHeader::new(VarType::Bool, 16, 1, "BrakeABSactive").expect("valid"),
        VarHeader::new(VarType::Int, 20, 1, "Gear").expect("valid"),
        VarHeader::new(VarType::Float, 24, 72, "CarIdxLapDistPct").expect("valid"),
        VarHeader::new(VarType::Float, 24 + 288, 72, "CarIdxEstTime").expect("valid"),
        VarHeader::new(VarType::Int, 24 + 576, 72, "CarIdxPosition").expect("valid"),
    ];
    let config = PublisherConfig {
        vars,
        buf_len: ROW,
        num_buf: 3,
        tick_rate: 60,
        session_info_capacity: 64 * 1024,
    };
    let mut publisher = Publisher::create(&names, &config).expect("creates");
    publisher.set_connected(true).expect("connects");
    publisher
        .set_session_info(b"WeekendInfo:\n TrackName: synthetic\n")
        .expect("fits");

    let mut wanted = VarSet::new();
    for name in [
        "SessionTime",
        "ThrottleRaw",
        "BrakeRaw",
        "BrakeABSactive",
        "Gear",
        "CarIdxLapDistPct",
        "CarIdxEstTime",
        "CarIdxPosition",
        "Clutch",
    ] {
        wanted.add(name);
    }
    let reader_config = ReaderConfig {
        retry_interval: Duration::from_millis(1),
        idle_timeout: Duration::from_millis(5),
    };
    let mut reader = LiveReader::new(names, wanted, reader_config);
    let mut snapshot = Snapshot::new();
    assert_eq!(reader.step(&mut snapshot), Ok(Step::Connected));
    let mut session_info = Vec::with_capacity(reader.session_info_capacity().expect("connected"));

    let mut row = [0u8; ROW];
    let mut ticks = |range: core::ops::Range<u32>| {
        for n in range {
            row[0..8].copy_from_slice(&f64::from(n).to_le_bytes());
            publisher.publish(&row).expect("publishes");
            let step = reader.step(&mut snapshot);
            assert!(matches!(step, Ok(Step::Tick { missed: 0 })), "{step:?}");
        }
    };
    ticks(0..600);
    let ((), allocations) = count_allocations(|| ticks(600..6600));
    assert_eq!(allocations, 0, "allocations while reading 6000 ticks");

    let ((idle, copied), allocations) = count_allocations(|| {
        (
            reader.step(&mut snapshot),
            reader.copy_session_info(&mut session_info),
        )
    });
    assert!(matches!(idle, Ok(Step::Idle)));
    assert_eq!(copied, Ok(Some(1)));
    assert_eq!(
        allocations, 0,
        "allocations in an idle wait and a session info copy"
    );
}
