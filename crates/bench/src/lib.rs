//! Benchmark harness. Every overlay, including Pitot's, goes through the same launch, warm-up,
//! and collection path. This crate must never depend on `pitot-overlay`.
//!
//! - [`replay`]: replays recordings into simulated SDK shared memory at the recorded tick rate.
//! - [`collect`]: samples an overlay's whole process tree (CPU, memory, GPU, DWM) into raw CSV.
//! - [`runner`]: measurement series from a plan: replay, launch, warm-up, collection, shutdown, interleaved
//!   baselines, and a warning when known overlays run.
//! - [`report`]: processed metrics per run and a report per series with confidence intervals, baseline
//!   differences, and budget checks.
//! - [`paths`]: where bench output goes.
//!
//! Planned modules:
//! - `capture`: the project's own capture format, recorder, and converters (M2).
//! - `anonymize`: fixture anonymizer for driver names, IDs, and team names (M2).
//! - `tier_b`: replay controller over broadcast messages and PresentMon capture (M5).

pub mod collect;
pub mod paths;
pub mod replay;
pub mod report;
pub mod runner;
#[cfg(windows)]
mod sys;
