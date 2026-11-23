# Overlay windows and rendering

## Running on a machine without the sim

```text
cargo run -p pitot-bench --release -- replay --irdt data/irdashies/ai-race-10min.irdt --loop
cargo run -p pitot --release
```

The overlay waits for the sim's shared memory and connects when the replay starts. It opens the
[Pitot window](control-window.md) by starting its own executable again with `--window`
(`cargo run -p pitot --release -- --window` opens the window alone). Quit from the window or the tray icon's
menu. A build run this way is not installed, so it has no updates ([releases](releases.md)); the start-up hook of
the installer finds no installation and returns at once.

## GPUI

The widgets are drawn with [GPUI](https://www.gpui.rs/), the same UI framework as the Pitot window: `gpui-pre` and
`gpui-pre-platform` 0.3.12 ([control-window.md](control-window.md#cost) says where they come from). GPUI
owns the Direct3D 11 device, DirectWrite, the glyph atlas, and the window thread's message loop. The overlay's own
code calls none of Direct3D, Direct2D, DirectWrite, or DirectComposition: `render.rs` turns the widgets' drawing
into GPUI quads, paths, and glyphs, and `platform.rs` keeps the Win32 parts GPUI has no option for (window styles,
the tray icon, move handles).

The overlay and the Pitot window are still two processes of one executable, each with its own GPUI app; the link
between them is unchanged ([control-window.md](control-window.md)). Running both in one process is possible now
that both use GPUI, and left for a later change.

## Windows

- One GPUI window per widget, sized to the widget's box and placed at its position on its monitor in physical
  pixels (the process is per-monitor DPI aware, so monitors left of or above the primary one, at negative
  coordinates, work). A window is made when its widget first shows: hidden, with its bounds and display in GPUI's
  logical pixels of that monitor. Before it shows, the overlay styles it and places it in physical pixels, reads
  its rectangle back, and shows it only when it lies where it belongs on its monitor. GPUI keeps asking windows
  that never showed to paint at every display refresh (tens of thousands of `WM_PAINT` a second for one such
  window on the lab PC), so a widget that never shows has no window.
- GPUI makes the window a borderless popup (`WindowKind::PopUp`) with a transparent background: its composition
  swap chain has per-pixel alpha, and text is antialiased in grayscale, as on any transparent GPUI window. The
  overlay then sets, on the window's handle (`raw-window-handle`): the style `WS_POPUP`, without the caption GPUI's
  frameless windows keep (which gives them the desktop's shadow and rounded corners), square corners
  (`DWMWA_WINDOW_CORNER_PREFERENCE`), and the extended styles `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST |
  WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`: click-through, always on top, never activated, and not in the taskbar. Only a
  layered window with `WS_EX_TRANSPARENT` is left out of mouse hit testing, so clicks reach the windows below, also
  those of other processes. The layer is set to full opacity once, which a layered window needs before it shows;
  GPUI's swap chain still draws the content. GPUI is told the windows are never focused and never to slow their
  frames down for being inactive (`inactive_frame_interval: None`).
- Win32 calls on GPUI's windows (styles, placement, showing) run between GPUI's updates: their window procedures
  run GPUI's handlers, which borrow GPUI's app, and calling them inside an update panics.
- Moving a widget moves its window; resizing it resizes the window, and GPUI draws it at the new size. Hiding a
  widget hides its window. A widget shown once keeps its window while any widget shows.
- Each window costs what GPUI gives every window, whatever it draws (read from `gpui-pre-windows` 0.3.12):
  a composition swap chain of three buffers, a path texture, and a 4x multisampled path texture, all the window's
  size (32 bytes per pixel together, made when the window is made and resized with it); a glyph atlas of its own
  (a 1024x1024 texture per kind of sprite it draws); its own DirectComposition device, target, and visual; its own
  DirectManipulation manager; eight render pipelines with their shaders and instance buffers, which grow to the
  largest frame drawn; and a line layout cache. The four reference widgets' windows hold 698,600 pixels, about
  22 MB at 32 bytes each. Widgets do not share windows: a window covering several boxes has their gaps' pixels
  too and is drawn whole whenever any of them changes, so its widgets draw at the rate of the fastest and the
  compositor takes the whole window each time; in the default layout the only reference widgets close together,
  the relative (10 Hz) and the fuel widget (30 Hz), would make a window 12% larger than their boxes. *Move widgets*
  would also have to split and join windows while a widget is dragged.
- With no widget shown for 2 s (the sim off, in the garage, *Hide widgets*), every widget window closes and GPUI
  goes headless (`App::request_windowing`), which releases its GPU device and stops its display-refresh thread.
  The overlay starts headless and becomes windowed again when a widget is to show; making the device and the
  windows takes a frame or two.
- A tray icon owned by a hidden window (class `PitotTray`, which the Pitot window finds to send it commands; never
  shown). GPUI's message loop (`GetMessageW` for the whole thread) dispatches its messages, as it does the move
  handles'. A left click opens the [Pitot window](control-window.md); a right click opens the menu: *Open Pitot*,
  *Move widgets* (checked while moving), *Hide widgets* (checked while the widgets are hidden; hidden widgets are
  not updated or drawn), *Reload configuration*, and *Quit Pitot*. Windows 11 shows new tray icons in the
  hidden-icons area next to the clock until they are pinned.
- While moving widgets, each widget gets a move handle: a small layered top-level window over its window, tinted
  at 28% opacity with a thin frame and a grip of dots at the bottom right, that never takes the focus. Only the
  handles take the mouse, so clicks elsewhere still pass through. The handle answers `WM_SETCURSOR` with the move
  or resize cursor for the spot under the pointer, and captures the mouse while the button is held. Dragging the
  inside moves the handle and the widget's window. Dragging an edge or a corner resizes the handle, within its
  monitor and down to the widget's `min_size()`; the app calls the widget's `resize`, resizes its window, and GPUI
  draws it. Releasing the button saves what changed ([configuration](configuration.md#moving-and-resizing-widgets)).
  The widget windows themselves stay click-through. The handles are made after the widget windows, so they are
  above them.
- `WM_CLOSE` to the tray window or to a widget window quits like Quit, so tools that close apps the usual way (End
  task, the benchmark runner) end the overlay cleanly and its exit line with the data counters is written. The
  Pitot window, if open, closes with it.

## Drawing

- Each widget window's root view is one `canvas` element over the whole window. Its paint draws the widget's
  layers bottom to top, each clipped to its rectangle (`Window::with_content_mask`), through `Canvas`
  (rectangles, lines, circles, paths, and text). Widgets never touch GPUI directly.
- GPUI draws whole windows: when a widget's update reports any change, the scheduler notifies its view
  (`cx.notify()`), and GPUI draws that window again, every layer of it, at its next display refresh. Windows of
  widgets that did not change are not drawn. There are no partial updates; a `Redraw::Area` from a widget only
  says that something changed.
- `Canvas::fill` and `fill_background` are GPUI quads, as are level and upright lines (so they stay as sharp as
  fills). Other lines (`Canvas::line`, and `Canvas::lines` for several in one path) and polylines
  (`Canvas::stroke`, `polyline`) are GPUI paths with lyon's tessellation, flat ends for lines and round joins and
  ends for polylines. Circles are round quads: a border for a stroke, a fill for a disk.
- What a window keeps between paints (`PaintCache`) keeps a paint from redoing work GPUI's whole-window drawing
  would otherwise repeat: lines of text shaped in the last paint ([below](#text)), and one lyon stroke tessellator
  with its vertex buffers, which tessellates each stroke straight into a GPUI path (the triangles GPUI's own path
  builder makes from the same tessellation, without its intermediate path, fresh tessellator, and growing
  buffers). Points inside a level or upright run of a polyline are left out before tessellation: the line through
  the run's ends covers exactly the pixels the line through every point covers, joins included (a test compares
  the triangles' coverage), with far fewer triangles for pedals held at zero or full. A paint then allocates only
  in the GPUI calls that must: each path's vertices, which GPUI's scene takes and scales into a copy, and text not
  drawn in the last paint.
- `[general] text_size` ([configuration](configuration.md#text-size)) sets a text scale, `text_size / 100`. A widget
  that scales with the text (`Widget::scales_with_text`, every widget but the input trace) is given its box divided
  by the scale through `Widget::resize` and draws in those widget pixels: the canvas maps each coordinate to screen
  pixels so a whole widget pixel lands on a whole screen pixel and edges sharp at the default stay sharp, a one
  pixel line keeps a pixel, and then to GPUI's logical pixels at the window's DPI. Text sizes are asked for in
  widget pixels and pick the next of `TEXT_SIZES` (12, 14, 18, 26) up, times the scale. At a scale other than 1,
  text whose box reaches past the layer is not drawn, and a number (digits, signs, separators, units) wider than its
  box is not drawn rather than drawn with digits missing. A change of the scale resizes every widget and redraws.
- Backgrounds (a widget's panel, a band behind its content) are drawn with `Canvas::fill_background`, which
  scales the color's own alpha by `[general] background_opacity` ([configuration](configuration.md#background-opacity));
  everything else uses `Canvas::fill` and stays as it is. A change of the value redraws every widget once.
- GPUI handles device loss itself (its display-refresh thread checks the device and rebuilds what it draws with).
- A configuration change closes the widget windows and makes the widgets again; their windows open as they show.
- `pitot` built with `--features alloc-count` counts the process's heap allocations (Rust code only) and logs them
  at exit, also per widget update and per paint ([below](#development-measurement)).

## Text

- Text is in Segoe UI semibold, one line per call, painted glyph by glyph (`Window::paint_glyph`) from GPUI's
  glyph atlas, which rasterizes each glyph once per size.
- Numbers (lines made only of digits, signs, separators, `%`, `#`, `k`, `L`, and spaces, up to 24 characters) are
  put together from glyphs measured once per font size: each character is laid out alone by GPUI's text system the
  first time it is drawn at a size, and a line places each glyph where the advances of those before it end. In
  Segoe UI semibold these characters have no kerning or contextual forms between them: shaped whole by GPUI,
  every pair and triple of them at the font sizes of every text size gives the same glyphs, each exactly at that
  position, and the same width (checked when this was written, 233,184 lines). So numbers that change, such as
  gaps and times, draw without a DirectWrite layout or an allocation.
- Other text is laid out by GPUI's text system (DirectWrite underneath) and kept by the window, per font size,
  while it is drawn in every paint; text not drawn in a paint is let go at its end. GPUI would keep its own layouts
  for a frame too, but finding them again costs a copy of the string and lookups per line, every paint.
- Text is vertically centered in its box by the font's ascent and descent, as DirectWrite centers a paragraph, on a
  whole screen pixel; left, centered, or right in it; and clipped to it, so long names do not run into the next
  column. `Canvas::fits` measures with the same glyphs or layout.

## Schedule

The data and session threads are the overlay's own ([pipeline](overlay-pipeline.md)). On GPUI's thread one task,
the scheduler, sleeps until something wakes it (`wake.rs`): a reason bit is set and the task is woken, without a
lock or an allocation on the waking side; GPUI's executor then posts one message to its thread. Each widget has its
own interval, `config::update_interval`: the configured `rate_hz`, else the widget's own `interval()`, and never
longer than 100 ms, so every widget updates at least 10 times per second
([update rates](configuration.md#update-rates)). The scheduler wakes for:

- **ticks**: while the sim is connected and a shown widget is due by the next tick, the data thread wakes it once
  for the next tick it publishes, so updates follow the data (the 60 Hz input trace updates on every tick, a 10 Hz
  widget on every sixth). Each wake disarms the next, so a tick wakes the scheduler only when it asked for it.
- **its timer**: a window timer on the tray window (`SetTimer`, about 16 ms resolution, no thread of its own, never
  `timeBeginPeriod`): the next due widget when ticks do not drive it, 50 ms after the next tick as a fallback when
  they do, once per second with the sim off and no widget shown, four times per second while connected with none
  shown (so widgets appear promptly when the driver gets in the car), the reload of a changed file, and the 2 s
  before going headless.
- **messages**: connection changes, session info, and tire samples from the data and session threads.
- **commands**: the tray menu and the Pitot window; drags of move handles; changes in the configuration directory,
  which a thread of its own waits on (the directory also holds the log, so only a changed `config.toml` reloads).

Each pass takes those, polls the pipeline, updates each shown widget that is due (with 4 ms of slack, as ticks come
a little early or late), notifies GPUI of the widgets that changed, and leaves Win32 calls on windows for after
GPUI's update. Hidden widgets are not updated; they keep their last due time, so they draw at once when they
appear.

GPUI draws on its own schedule: while it is windowed, a display-refresh thread waits for each composition
(`DwmFlush`) and invalidates every GPUI window (`RedrawWindow`), and GPUI draws the windows that were notified and
presents them. A window that was not notified is not drawn or presented: its paint message runs GPUI's frame
callback, which finds the window clean and does nothing else. That callback still runs for every shown window at
every refresh, with a DirectManipulation update, two short updates of GPUI's app, and an IME check, so the
process wakes about once per window per refresh whenever any widget shows, whether or not anything changed. GPUI
has no option to stop that per window; fewer windows and going headless when nothing shows are what the overlay can
do about it.

A draw lays out and paints the window's one element, sorts and batches the scene, uploads every kind of primitive
to its instance buffer (`Map` with `WRITE_DISCARD`), draws, and presents the swap chain without dirty rectangles,
so the compositor takes the whole window. Paths take a pass of their own: the 4x multisampled path texture is
cleared and resolved over the whole window, and each path vertex goes to the GPU with its own copy of the color
(104 bytes a vertex).

## Widgets

Every widget is one line of `crates/overlay/src/widgets/list.rs` ([the list](widgets.md)):

```rust
wind: WindWidget = Spec::new("Wind", 1460, 370).size(200, 170).rate(10.0).on(),
```

`wind` is the module (`widgets/wind.rs`) and the widget's `[widgets.wind]` table; `WindWidget` is the type in that
module; the `Spec` is its label in the Pitot window, its default place (physical pixels from the top-left of the
primary monitor), size, and update rate, and `.on()` when it is on by default. From the list come the module
declarations, the defaults and the names the configuration accepts, the Pitot window's switches, the widgets the
scheduler makes for a configuration (`Layout::build`, in the list's order), and the variables the pipeline
registers. The `status` widget is a small panel with the connection state, tick, car count, and session, shown in
every display state and updated every 100 ms.

Each widget type implements `Register` next to its `Widget` implementation:

- `type Vars`: the variables it reads beyond `pipeline::Vars`, declared in its module with `var_group!` (the wind
  widget's `WindVars`), or `()`. The pipeline registers every widget's group at start and copies a variable once
  however many groups name it ([pipeline](overlay-pipeline.md#variables)). `view.vars()` still reads `Vars`.
- `type Options`: fields of its own in its table, a struct deriving `Deserialize`, `Serialize`, and `Default` with
  `#[serde(default, deny_unknown_fields)]` (the track map's `MapOptions { shape }`), or `NoOptions`. The
  configuration checks them when the file is read, and the widget reads them with
  `config.options_as::<MapOptions>()`.
- `fn build(cx, vars)`: makes the widget from `cx.config` (its `WidgetConfig`) and the shared trackers
  `cx.shared`.

Its `Widget` implementation gives, besides drawing, its smallest size (`min_size`) and `resize(width, height)`,
which *Move widgets* calls while the widget is resized and a change of the text size calls for every widget. A
resized widget must be the widget made at that size: the same `size()` and `layers()`, with what it follows of the
session kept. Text and shapes keep their pixel size; the widget lays out within the box (more rows, a longer
history). `resize` may allocate (buffers for more rows), so updates and draws at the new size do not. The usual
pattern is a constructor that makes the widget empty and calls `resize` with the configured size.

Sizes are in widget pixels, which are screen pixels at the default text size. At the largest text size (150) a
widget at its default size gets its box divided by 1.5, so its smallest size must be at most that (a test checks
it), and a narrow or short box leaves out its less important columns and rows rather than overlapping them. A
widget without text says `false` from `scales_with_text` and keeps screen pixels.

### Shared trackers

Trackers that several widgets read are made once per layout, when the first widget asks for one, so a tracker no
enabled widget uses costs nothing. They are made again with the layout after a configuration change. At a new
connection the clock and the histories forget the session; the pit tracker is reset by its widgets, which keep
what it learned. Each user observes a tracker at its own rate with `observe(view)`, and the first observation of a
moment does the work for all.

| Tracker | Read by | What |
|---|---|---|
| `shared.clock()` | standings, relative | every car's progress and the times it passed 200 checkpoints a lap: race gaps (`RaceClock::gap`) and gaps on track (`RaceClock::track_gap`) |
| `shared.pit()` | pit, track map | other cars' pit stops, the refueling speed, and the pit lane loss ([pit](pit.md)) |
| `shared.gaps()` | widgets to come: battle, faster class, rejoin, side by side | the gap history (below) |
| `shared.laps()` | widgets to come: lap log, lap times, lap time spread, stints | the lap history (below) |

**Gap history** (`widgets/gaps.rs`). Every 0.5 s of tick time (`View::tick_time`) it samples, for each car in the world, the gap on
track to the player and to the next cars ahead and behind it on track, as the clock measured them: the time since
the car in front passed the spot where the other one is now, whatever their laps. Pit road is included as
measured. A sample is a `GapSample { time, to_player, ahead, behind }`: `to_player` is positive when the car is
ahead of the player within half a lap, negative behind, 0 for the player, NaN when the pass was not measured;
`ahead` and `behind` are `Some((car, seconds))` with NaN seconds when not measured. `latest(car)`, `samples(car)`
(oldest first), and `ago(car, seconds)` (for how a gap changed) read it. 240 samples, two minutes, of 16 bytes
per car: 276 KB for 72 cars, allocated at the first observation and again only when the car count changes. A
replay going back in time, a tick count that starts over, or a new session starts it over.

**Lap history** (`widgets/laps.rs`). A car's lap ends when `CarIdxLapCompleted` goes up. Its time is the sim's
`CarIdxLastLapTime`, which changes 2 to 63 ticks after the line in the public AI race capture, so the lap joins the
history when the time changes, or untimed (NaN) after 3 s. A `Lap { number, time, ended, flags }` has the flags
`PIT` (on pit road during it), `OFF_TRACK` (seen off track), `INCIDENT` (the player's incident count went up; other
cars' are not published), `CAUTION` (not under green), and `PARTIAL` (not observed from start to end: the first lap
seen, or the car left the world or went unobserved for 5 s). `is_clean()` is timed and none of `PIT`, `OFF_TRACK`,
`INCIDENT`, `PARTIAL`; a caution lap can be clean. Flags see only what the observations see: an excursion off
track between two observations is missed. `laps(car)` (oldest first), `last(car)`, and `count(car)` read it. 100
laps of 24 bytes per car: 173 KB for 72 cars, allocated like the gap history. The pace car is left out.

Both are tested with synthetic data in their modules, and the allocation check observes both every update.

### Adding a widget

1. **The module**: `crates/overlay/src/widgets/<name>.rs` with a `pub struct` widget type, `impl Widget`, and
   `impl Register`. Variables not in `Vars` go in a `var_group!` there; fields of its own in an options struct
   there. Its unit tests go there too: they make the widget with its own constructor, its configuration from
   `Config::default().widget("<name>")`, and its variables from `view.resolve()`. The type and its variable group
   are `pub` (the list re-exports the type, and a private group in a public constructor fails the lint).

   ```rust
   use crate::config::{NoOptions, WidgetConfig};
   use crate::pipeline::{View, var_group};

   use super::{Build, MAX_LAYERS, Redraw, Register, Widget};

   var_group! {
       /// The variables only the blind spot widget reads.
       pub struct SpotterVars {
           car_left_right => "CarLeftRight",
       }
   }

   impl Register for SpotterWidget {
       type Vars = SpotterVars;
       type Options = NoOptions;

       fn build(cx: &Build<'_>, vars: SpotterVars) -> Self {
           Self::new(cx.config, vars)
       }
   }
   ```

2. **The list**: one line at the end of `crates/overlay/src/widgets/list.rs`. Leave out `.on()` to ship it off by
   default.
3. **The docs**: `docs/<name>.md` (what it shows, where the numbers come from, its own fields, and its measured
   cost), and one row at the end of the table in `docs/widgets.md`.

Nothing else changes: not `config.rs` or its `DEFAULT_TOML`, not `pipeline/vars.rs`, `app.rs`, or the Pitot window,
not `docs/configuration.md` or the benchmark configurations. `list.rs` and `docs/widgets.md` merge with
`merge=union` (`.gitattributes`), so branches that each add a widget merge there without conflicts.

What checks the new widget without more code:

- `cargo test -p pitot-overlay`: names and labels are unique, default rates are 10 to 120 Hz, every widget is made
  from its defaults with its own interval at most 100 ms, a resized widget equals the widget made at that size (and keeps its layers inside it), its default
  size lays out at every text size without being raised past its smallest, `docs/widgets.md`
  has its row in the list's order, the default file and the defaults agree, and the benchmark configurations still
  turn on exactly the four reference widgets.
- `cargo test -p pitot-overlay --features alloc-check --test alloc_check`:
  `every_widget_and_the_histories_do_not_allocate_per_update` makes every widget of the list and updates them
  through a light race of three cars, counting allocations, at their default size, their smallest, twice the
  default, and at the largest and smallest text sizes. Drawing is GPUI's and allocates there, so the test does not
  draw; `pitot --features alloc-count` measures what drawing allocates in the running overlay. A widget whose work
  needs other data adds its own test there.

A widget on by default joins the benchmark's measured set unless `benchmarks/pitot/four.toml` and `inputs.toml`
turn it off; the benchmark test fails until both do (then copy them to the `*.local.toml` files with `monitor` set).
Such a widget may also get a section in `DEFAULT_TOML`, at the end.

The rules for every widget stay: no allocation per update after warm-up (its own drawing code should not allocate
either; GPUI does, as it draws), a redraw only when something it shows changed, its own update rate, `visible()`
for the display states it shows in, its background through `Canvas::fill_background` (at the alpha of its
`opacity`), and no logging per update.

## Development measurement

Performance is measured with `pitot-bench` at each release, on the release build. For development:

- Every build logs render counters at exit, next to `data counters at exit` (never while it runs):
  `render counters at exit` with the scheduler's wake-ups, in total, per second, and per reason (ticks, timer,
  messages, commands, moves, configuration), and how often GPUI became windowed; then one `widget render counters
  at exit` line per widget made: updates, updates that asked for a redraw, paints (GPUI's draws of the widget's
  window, each presented once) and paints per second, the time the widget's paint took in total per second, on
  average, and at most (its drawing inside GPUI's frame, not GPUI's layout, upload, or present), windows made, and
  the `WM_PAINT` messages its windows received, in total and per second (the display-refresh thread's
  invalidations; a window subclass counts them on GPUI's thread). Paints well below paint messages are windows GPUI
  was asked to paint and found clean.
- `cargo build -p pitot --features alloc-count` builds the overlay with a counting allocator. At exit it logs
  `allocations at exit` with the total and the rate per second of the whole process, the rate on GPUI's thread,
  and the allocations during widget updates and paints with their counts and the allocations per paint, split by
  where they happen: the overlay's own drawing code (`per_paint_own`), GPUI's text layout (`per_paint_text`),
  paths (`per_paint_path`, lyon's tessellation and GPUI's paths), and GPUI's other painting (`per_paint_draw`:
  quads, glyphs). Each widget's line above then has `paint_allocations_mean`. It counts Rust's heap only:
  Direct3D, DirectWrite, and the driver allocate natively and are not counted.
- Wake-ups per thread can be read without ETW from the PDH counter `\Thread(pitot*)\Context Switches/sec`.
