//! Reader for the sim's telemetry SDK. No rendering, UI, or overlay logic lives here.
//!
//! Planned modules (M1):
//! - `live`: live reader with a per-connection offset cache, torn-read check on `tickCount`,
//!   and reconnect handling. Zero heap allocations per tick.
//! - `ibt`: `.ibt` parser for the header, variable definitions, session YAML, sample iteration,
//!   and seeking by `SessionNum`/`SessionTime`. Platform independent.
//! - `session`: extraction of `WeekendInfo`, `DriverInfo`, `SessionInfo` results, and
//!   `SplitTimeInfo` from the session YAML.
//! - `broadcast`: broadcast messages for replay position and speed, camera, and telemetry recording.

pub mod layout;
