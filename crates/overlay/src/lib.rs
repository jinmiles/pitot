//! Overlay process: one process, no IPC between data and rendering, no WebView or garbage-collected runtime.
//! The widgets are drawn with GPUI, whose executor runs on the overlay's main thread. `pitot.exe` runs it through
//! [`app::main`] when started without arguments; with `--window` it runs the Pitot window instead, in a process of
//! its own.
//!
//! - [`pipeline`]: data and session threads, and the scheduler's view of the latest telemetry.
//! - [`wake`]: how those threads, the windows, and the timer wake the scheduler.
//! - [`platform`]: monitors, the widget windows' styles and placement, the tray icon, move handles, and the link to
//!   the Pitot window (`pitot-control`) (Windows).
//! - [`render`]: the drawing API widgets use, on GPUI's painting (Windows).
//! - [`app`]: the process start, the GPUI app with a window per widget, and the scheduler (Windows).
//! - [`widgets`]: the widget trait and widgets.
//! - [`config`]: the hot-reloaded TOML configuration.
//! - [`log`]: the log file (Windows).
//!
//! Planned modules:
//! - `output`: Spout2 and optional local web output, active only when enabled (M6).

#[cfg(all(windows, feature = "alloc-count"))]
pub mod alloc_count;
#[cfg(windows)]
pub mod app;
pub mod config;
#[cfg(windows)]
pub mod log;
pub mod pipeline;
#[cfg(windows)]
pub mod platform;
#[cfg(windows)]
pub mod render;
pub mod wake;
#[cfg(windows)]
pub mod widgets;
