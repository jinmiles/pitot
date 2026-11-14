//! The Pitot window (`pitot-control.exe`): turns the overlay and each widget on and off.
//!
//! It runs as its own process, opened from the overlay's tray icon and ended with its close button, so the
//! overlay's process carries none of its UI framework and nothing runs while it is closed. It reaches the
//! overlay through [`pitot_overlay::platform::ControlLink`] and edits `config.toml` in place, which the overlay
//! reloads.
//!
//! - [`model`]: what the window lists and knows, and where it opens; no UI.
//! - `app`: process start, the window, and the link to the overlay (Windows).
//! - `panel`: what the window shows and what clicks do (Windows).
//! - `style`: colors and sizes, in one place for the coming design (Windows).

#[cfg(windows)]
pub mod app;
pub mod model;
#[cfg(windows)]
mod panel;
#[cfg(windows)]
mod style;
