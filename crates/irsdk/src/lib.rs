//! Reader for the sim's telemetry SDK. No rendering, UI, or overlay logic lives here.
//!
//! Planned modules (M1):
//! - `session`: extraction of `WeekendInfo`, `DriverInfo`, `SessionInfo` results, and
//!   `SplitTimeInfo` from the session YAML.
//! - `broadcast`: broadcast messages for replay position and speed, camera, and telemetry recording.

pub mod ibt;
pub mod layout;
#[cfg(windows)]
pub mod live;
#[cfg(windows)]
pub mod publish;
#[cfg(windows)]
mod shm;
