//! Runner: executes a measurement plan. Every run puts the app's files in place, starts the replay, launches the
//! app, warms up, collects, and stops everything again; baselines without an overlay are interleaved. Every app,
//! Pitot included, goes through the same steps. See `docs/runner.md`.

pub mod detect;
#[cfg(windows)]
mod exec;
pub mod files;
pub mod plan;
pub mod record;

#[cfg(windows)]
pub use exec::{Options, RunnerError, execute};
pub use plan::{AppPlan, BASELINE, Budget, FilePlan, Plan, PlanError, ReplayPlan, Slot};
pub use record::{Outcome, RunInfo, RunRecord, SeriesRecord};
