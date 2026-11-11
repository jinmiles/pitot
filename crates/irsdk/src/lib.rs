//! Reader for the sim's telemetry SDK. No rendering, UI, or overlay logic lives here.
//!
//! - [`layout`]: shared-memory and `.ibt` structure layouts.
//! - [`capture`]: Pitot capture (`.ptc`) reader.
//! - [`flags`]: session state and flag values.
//! - [`ibt`]: `.ibt` telemetry file reader.
//! - [`live`]: live shared-memory reader (Windows).
//! - [`publish`]: simulated shared memory for tests and replays (Windows).
//! - [`session`]: session info extraction.
//! - [`broadcast`]: camera, replay, and telemetry recording commands.

pub mod broadcast;
pub mod capture;
pub mod flags;
pub mod ibt;
pub mod layout;
#[cfg(windows)]
pub mod live;
#[cfg(windows)]
pub mod publish;
pub mod session;
#[cfg(windows)]
mod shm;
