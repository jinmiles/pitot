//! Benchmark harness. Every overlay, including Pitot's, goes through the same launch, warm-up,
//! and collection path. This crate must never depend on `pitot-overlay`.
//!
//! Planned modules:
//! - `replay`: Tier A `.ibt` replayer that recreates the SDK shared memory and data-valid event
//!   at 60 Hz without the sim installed (M2).
//! - `collect`: PDH collectors summed over the overlay's process tree (M2).
//! - `run`: runner for launch, warm-up, measurement, shutdown, and interleaved baselines (M2).
//! - `report`: processed metrics and reports with confidence intervals (M2).
//! - `anonymize`: fixture anonymizer for driver names, IDs, and team names (M2).
//! - `tier_b`: replay controller over broadcast messages and PresentMon capture (M5).
//! - `detect`: warning when known overlays are running before a measurement (M5).
