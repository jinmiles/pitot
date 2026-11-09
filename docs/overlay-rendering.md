# Overlay windows and rendering

## Running on a machine without the sim

```text
cargo run -p pitot-bench --release -- replay --irdt data/irdashies/ai-race-10min.irdt --loop
cargo run -p pitot-overlay --release
```

The overlay waits for the sim's shared memory and connects when the replay starts. Quit from the tray icon's
menu.

## Windows

- One window per monitor that has widgets, created over the monitor's bounds in physical pixels (the process is
  per-monitor DPI aware, so monitors left of or above the primary one, at negative coordinates, work).
- Extended styles `WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE |
  WS_EX_TOOLWINDOW`: no redirection surface, always on top, click-through (`WM_NCHITTEST` returns
  `HTTRANSPARENT`), never activated, and not shown in the taskbar.
- A tray icon owned by a hidden window offers Reload configuration and Quit.

## Rendering

- One D3D11 device (hardware, falling back to WARP), shared by Direct2D, DirectWrite, and a DirectComposition
  desktop device.
- Each window has a composition target and a root visual. Each widget has its own visual and surface (a
  `Layer`), positioned in the window by offset. Moving or hiding a widget changes the visual tree only.
- A redraw passes the smallest update rectangle to `IDCompositionSurface::BeginDraw`, clips to it, clears it to
  transparent, and draws in layer pixels.
- The device commits only when something was drawn or a visibility changed.
- Widgets draw through `Canvas` (rectangles, lines, text) and never touch COM directly. All rendering `unsafe` is
  in the `render` module and all windowing `unsafe` in `platform`, each block with a `// SAFETY:` comment.
- A rendering error, such as device loss, drops the renderer, waits 500 ms, rebuilds every window target and
  layer, and redraws all widgets.

## Schedule

The render thread sleeps in `MsgWaitForMultipleObjectsEx` on a high-resolution waitable timer
(`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`; `timeBeginPeriod` is never called). It wakes when the next widget is due
or a window message arrives. Each widget has its own interval, says whether it needs a full or partial redraw or
none, and is hidden outside its display states (most widgets show only while driving).

## Development measurement

Lab PC (i7-9700, RTX 3090), release build, replaying the public AI race capture at 60 Hz with the status widget
updating at 4 Hz. Measured over 15 s with the process CPU time counter, not with `pitot-bench`:

- CPU 31 ms over 15.0 s: 0.21% of one core, 0.026% of all eight cores
- Private bytes 12.1 MB
- 7 threads

These are development numbers; official numbers come from `pitot-bench` runs on the home PC.

## Not yet

- Text is drawn with `DrawText`; the glyph atlas and cached layouts follow in their own change.
- Widget placement is a built-in default until the configuration file exists.
- The binary is a console application for now, so errors are visible during development.
