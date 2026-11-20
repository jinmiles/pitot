//! A counting global allocator for measuring the overlay's process (`pitot` built with `--features alloc-count`):
//! heap allocations of Rust code in total, on GPUI's thread, during widget updates, and during widget paints,
//! logged at exit. A measurement, not a check: GPUI allocates while it draws, and native code (Direct3D,
//! DirectWrite, the driver) allocates outside Rust's heap and is not counted.
//!
//! Allocations during a paint are counted by where they happen: in the overlay's own drawing code, or inside the
//! GPUI calls it makes, by kind (text layout, paths, and other drawing such as quads and glyphs). The kinds say
//! which allocations the overlay's use of GPUI causes and which GPUI makes whatever the overlay does.

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
/// Allocations during paints, by [`PaintKind`].
static PAINT: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static UPDATES: AtomicU64 = AtomicU64::new(0);
static PAINTS: AtomicU64 = AtomicU64::new(0);
static STARTED: OnceLock<Instant> = OnceLock::new();

const OTHER: u8 = 0;
const MAIN_THREAD: u8 = 1;
const IN_UPDATE: u8 = 2;
/// A paint, in [`PaintKind`] `n` at `IN_PAINT + n`.
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
        IN_UPDATE => {
            UPDATE.fetch_add(1, Ordering::Relaxed);
        }
        p if p >= IN_PAINT => {
            if let Some(counter) = PAINT.get(usize::from(p - IN_PAINT)) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
        _ => {}
    }
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

/// Where an allocation during a paint happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintKind {
    /// The overlay's own drawing code.
    Own = 0,
    /// GPUI's text layout.
    Text = 1,
    /// Building and painting paths (lyon's tessellation and GPUI's paths).
    Path = 2,
    /// GPUI's other painting: quads, glyphs, and content masks.
    Draw = 3,
}

/// What GPUI's thread is doing, for counting.
#[derive(Debug, Clone, Copy)]
pub enum Phase {
    Update,
    /// A widget's paint, in its own drawing code.
    Paint,
    /// Within a paint, a call of this kind.
    In(PaintKind),
}

/// Restores the phase before [`Phase::enter`] when dropped.
#[must_use]
pub struct PhaseGuard {
    previous: u8,
}

impl Phase {
    /// Counts this thread's allocations as this phase until the guard drops. An update or a paint counts one.
    pub fn enter(self) -> PhaseGuard {
        let phase = match self {
            Self::Update => {
                UPDATES.fetch_add(1, Ordering::Relaxed);
                IN_UPDATE
            }
            Self::Paint => {
                PAINTS.fetch_add(1, Ordering::Relaxed);
                IN_PAINT + PaintKind::Own as u8
            }
            Self::In(kind) => IN_PAINT + kind as u8,
        };
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

/// Allocations during paints so far, of every kind together.
pub fn paint_allocations() -> u64 {
    PAINT.iter().map(|c| c.load(Ordering::Relaxed)).sum()
}

/// Logs the counts since [`start`].
pub fn log_totals() {
    let seconds = STARTED
        .get()
        .map_or(0.0, |at| at.elapsed().as_secs_f64())
        .max(1e-9);
    let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
    let (total, main) = (load(&TOTAL), load(&MAIN));
    let update = load(&UPDATE);
    let paint = paint_allocations();
    let (updates, paints) = (load(&UPDATES), load(&PAINTS));
    let per = |n: u64, d: u64| if d == 0 { 0.0 } else { n as f64 / d as f64 };
    let per_paint = |n: u64| (per(n, paints) * 10.0).round() / 10.0;
    let rate = |n: u64| (n as f64 / seconds * 10.0).round() / 10.0;
    let kind = |k: PaintKind| load(&PAINT[k as usize]);
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
        per_paint = per_paint(paint),
        per_paint_own = per_paint(kind(PaintKind::Own)),
        per_paint_text = per_paint(kind(PaintKind::Text)),
        per_paint_path = per_paint(kind(PaintKind::Path)),
        per_paint_draw = per_paint(kind(PaintKind::Draw)),
        "allocations at exit"
    );
}
