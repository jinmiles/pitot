//! Benchmark harness. Every overlay, including Pitot's, goes through the same launch, warm-up,
//! and collection path. This crate must never depend on `pitot-overlay`.
//!
//! - [`replay`]: replays recordings into simulated SDK shared memory at the recorded tick rate.
//! - [`collect`]: samples an overlay's whole process tree (CPU, memory, GPU, DWM) into raw CSV.
//! - [`paths`]: where bench output goes.
//!
//! Planned modules:
//! - `capture`: the project's own capture format, recorder, and converters (M2).
//! - `run`: runner for launch, warm-up, measurement, shutdown, and interleaved baselines (M2).
//! - `report`: processed metrics and reports with confidence intervals (M2).
//! - `anonymize`: fixture anonymizer for driver names, IDs, and team names (M2).
//! - `tier_b`: replay controller over broadcast messages and PresentMon capture (M5).
//! - `detect`: warning when known overlays are running before a measurement (M5).

pub mod collect;
pub mod paths;
pub mod replay;
#[cfg(windows)]
mod sys;
