//! Overlay process: one process, no IPC between data and rendering. No async runtimes,
//! WebView, or garbage-collected runtimes.
//!
//! Planned modules:
//! - `data`: data thread woken by the data-valid event, feeding lock-free SPSC buffers (M3).
//! - `session`: session thread that re-parses session info when `sessionInfoUpdate` changes (M3).
//! - `render`: per-monitor windows, DirectComposition visual tree, and Direct2D/DirectWrite on a
//!   shared D3D11 device, with partial updates through update rects (M3).
//! - `text`: glyph atlas for digits and symbols, cached DirectWrite layouts (M3).
//! - `schedule`: per-widget update rates on high-resolution waitable timers (M3).
//! - `config`: hot-reloaded TOML configuration (M3).
//! - `widgets`: input trace (M3); relative, standings, and fuel (M4).
//! - `output`: Spout2 and optional local web output, active only when enabled (M6).
