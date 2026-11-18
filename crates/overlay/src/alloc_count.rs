//! A counting global allocator for measuring the overlay's process (`pitot` built with `--features alloc-count`):
//! heap allocations of Rust code in total, on GPUI's thread, during widget updates, and during widget paints,
//! logged at exit. A measurement, not a check: GPUI allocates while it draws, and native code (Direct3D,
//! DirectWrite, the driver) allocates outside Rust's heap and is not counted.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tracing::info;

/// Allocations by everything in the process.
static TOTAL: AtomicU64 = AtomicU64::new(0);
/// Allocations on GPUI's thread (the scheduler, GPUI's own work, updates, and paints).
static MAIN: AtomicU64 = AtomicU64::new(0);
static UPDATE: AtomicU64 = AtomicU64::new(0);
static PAINT: AtomicU64 = AtomicU64::new(0);
static UPDATES: AtomicU64 = AtomicU64::new(0);
static PAINTS: AtomicU64 = AtomicU64::new(0);
static STARTED: OnceLock<Instant> = OnceLock::new();

const OTHER: u8 = 0;
const MAIN_THREAD: u8 = 1;
const IN_UPDATE: u8 = 2;
const IN_PAINT: u8 = 3;

thread_local! {
    // Const-initialized, so reading it never allocates.
    static PHASE: Cell<u8> = const { Cell::new(OTHER) };
}

fn record() {
    TOTAL.fetch_add(1, Ordering::Relaxed);
    let phase = PHASE.try_with(Cell::get).unwrap_or(OTHER);
    if phase != OTHER {
        MAIN.fetch_add(1, Ordering::Relaxed);
    }
    match phase {
        IN_UPDATE => UPDATE.fetch_add(1, Ordering::Relaxed),
        IN_PAINT => PAINT.fetch_add(1, Ordering::Relaxed),
        _ => 0,
    };
}

struct Counting;

// SAFETY: every call is forwarded unchanged to the system allocator, which upholds the contract; counting only adds
// atomic increments and a thread-local read that does not allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: forwarded with the caller's layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: forwarded with the caller's layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        // SAFETY: forwarded with the caller's pointer, layout, and size.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded with the caller's pointer and layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Marks the calling thread as GPUI's and starts the clock of the rates.
pub fn start() {
    let _ = STARTED.set(Instant::now());
    PHASE.with(|p| p.set(MAIN_THREAD));
}

/// What GPUI's thread is doing, for counting.
#[derive(Debug, Clone, Copy)]
pub enum Phase {
    Update,
    Paint,
}

/// Restores the phase before [`Phase::enter`] when dropped.
#[must_use]
pub struct PhaseGuard {
    previous: u8,
}

impl Phase {
    /// Counts this thread's allocations as this phase until the guard drops.
    pub fn enter(self) -> PhaseGuard {
        let (counter, phase) = match self {
            Self::Update => (&UPDATES, IN_UPDATE),
            Self::Paint => (&PAINTS, IN_PAINT),
        };
        counter.fetch_add(1, Ordering::Relaxed);
        PhaseGuard {
            previous: PHASE.with(|p| p.replace(phase)),
        }
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        PHASE.with(|p| p.set(self.previous));
    }
}

/// Logs the counts since [`start`].
pub fn log_totals() {
    let seconds = STARTED
        .get()
        .map_or(0.0, |at| at.elapsed().as_secs_f64())
        .max(1e-9);
    let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
    let (total, main) = (load(&TOTAL), load(&MAIN));
    let (update, paint) = (load(&UPDATE), load(&PAINT));
    let (updates, paints) = (load(&UPDATES), load(&PAINTS));
    let per = |n: u64, d: u64| if d == 0 { 0.0 } else { n as f64 / d as f64 };
    let rate = |n: u64| (n as f64 / seconds * 10.0).round() / 10.0;
    info!(
        seconds = (seconds * 10.0).round() / 10.0,
        total,
        total_per_s = rate(total),
        main_thread_per_s = rate(main),
        updates,
        update_allocations = update,
        per_update = (per(update, updates) * 100.0).round() / 100.0,
        paints,
        paint_allocations = paint,
        per_paint = (per(paint, paints) * 10.0).round() / 10.0,
        "allocations at exit"
    );
}
