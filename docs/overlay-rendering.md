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
- Extended styles `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST |
  WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`: click-through, no redirection surface, always on top, never activated,
  and not shown in the taskbar. Only a layered window with `WS_EX_TRANSPARENT` is left out of mouse hit
  testing, so clicks reach the windows below, also those of other processes (answering `WM_NCHITTEST` with
  `HTTRANSPARENT` passes them only to windows of the same thread). The layer is set to full opacity once,
  which a layered window needs before it shows; DirectComposition still draws the content.
- A tray icon owned by a hidden window opens its menu on a left or right click: *Move widgets* (checked while
  moving), *Hide widgets* (checked while the widgets are hidden; hidden widgets are not updated or drawn),
  *Reload configuration*, and *Quit Pitot*. Windows 11 shows new tray icons in the hidden-icons area next to the
  clock until they are pinned.
- While moving widgets, each widget gets a move handle: a small layered top-level window over it, tinted at
  28% opacity, that never takes the focus. Only the handles take the mouse, so clicks elsewhere still pass
  through. Dragging moves the handle and the widget's composition visuals (no redraw), and releasing the
  button saves the position ([configuration](configuration.md#moving-widgets)). The overlay windows themselves
  stay click-through. Outside move mode no handle exists.
- `WM_CLOSE` to any of these windows quits like Quit, so tools that close apps the usual way (End task, the
  benchmark runner) end the overlay cleanly and its exit line with the data counters is written.

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
- A rendering error, such as device loss, drops the renderer, waits 500 ms, rebuilds the device, every window
  target, and every layer, and redraws all widgets.
- A configuration change keeps the device and rebuilds only window targets and layers. Recreating the D3D11
  device each time leaked about 4.6 MB of driver memory per reload.

## Text

- Numbers, signs, separators, and units (`0-9 . , : + - / # % k L` and space) come from a glyph atlas built per
  text size when the device is created: each glyph is rendered once in white, and drawing it is a colored
  `FillOpacityMask` from the atlas, with no DirectWrite work per frame.
- Stable text without digits (names, labels) uses an `IDWriteTextLayout` cached per string, size, alignment, and
  box, created on first use and drawn with `DrawTextLayout`. The cache is cleared past 512 entries.
- Text that mixes digits with letters (a tick counter, "46.0 laps") changes too often to cache and is drawn with
  `DrawText` directly. Caching it filled the cache every couple of minutes and made private bytes swing between
  16 and 23.5 MB; drawn directly, they stay flat.
- Text is clipped to its box, so long names do not run into the next column.

## Schedule

The render thread sleeps in `MsgWaitForMultipleObjectsEx` on a high-resolution waitable timer
(`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`; `timeBeginPeriod` is never called). It wakes when the next widget is due
or a window message arrives. Each widget has its own interval, says whether it needs a full or partial redraw or
none, and is hidden outside its display states (most widgets show only while driving).

Only shown widgets set the timer. A hidden widget keeps its last due time, so it draws at once when it appears.
With no widget shown, the thread wakes once per second while the sim is not running, and four times per second
while it is connected (in the garage, for example), so widgets appear within a quarter second of getting in the
car.

## Development measurement

Lab PC (i7-9700, RTX 3090), release build, replaying the public AI race capture at 60 Hz with the status widget
updating at 4 Hz. Measured over 15 s with the process CPU time counter, not with `pitot-bench`:

- CPU 31 ms over 15.0 s: 0.21% of one core, 0.026% of all eight cores
- Private bytes 12.1 MB
- 7 threads

These are development numbers; official numbers come from `pitot-bench` runs on the home PC.

## Not yet

- Widget placement is a built-in default until the configuration file exists.
- The binary is a console application for now, so errors are visible during development.
