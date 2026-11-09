//! Overlay process: one process, no IPC between data and rendering. No async runtimes,
//! WebView, or garbage-collected runtimes.
//!
//! - [`pipeline`]: data and session threads, and the render thread's view of the latest telemetry.
//! - [`platform`]: monitors, overlay windows, message loop, timers, and the tray icon (Windows).
//! - [`render`]: D3D11, Direct2D, DirectWrite, and DirectComposition layers per widget (Windows).
//! - [`app`]: the render thread's schedule and main loop (Windows).
//! - [`widgets`]: the widget trait and widgets.
//!
//! Planned modules:
//! - `text`: glyph atlas for digits and symbols, cached DirectWrite layouts (M3).
//! - `config`: hot-reloaded TOML configuration (M3).
//! - more widgets: input trace (M3); relative, standings, and fuel (M4).
//! - `output`: Spout2 and optional local web output, active only when enabled (M6).

#[cfg(windows)]
pub mod app;
pub mod pipeline;
#[cfg(windows)]
pub mod platform;
#[cfg(windows)]
pub mod render;
#[cfg(windows)]
pub mod widgets;
