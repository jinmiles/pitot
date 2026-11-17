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

## Windows

- One window per monitor that has widgets, created over the monitor's bounds in physical pixels (the process is
  per-monitor DPI aware, so monitors left of or above the primary one, at negative coordinates, work).
- Extended styles `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST |
  WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`: click-through, no redirection surface, always on top, never activated,
  and not shown in the taskbar. Only a layered window with `WS_EX_TRANSPARENT` is left out of mouse hit
  testing, so clicks reach the windows below, also those of other processes (answering `WM_NCHITTEST` with
  `HTTRANSPARENT` passes them only to windows of the same thread). The layer is set to full opacity once,
  which a layered window needs before it shows; DirectComposition still draws the content.
- A tray icon owned by a hidden window (class `PitotTray`, which the Pitot window finds to send it commands). A
  left click opens the [Pitot window](control-window.md); a right click opens the menu: *Open Pitot*, *Move
  widgets* (checked while moving), *Hide widgets* (checked while the widgets are hidden; hidden widgets are not
  updated or drawn), *Reload configuration*, and *Quit Pitot*. Windows 11 shows new tray icons in the
  hidden-icons area next to the clock until they are pinned.
- While moving widgets, each widget gets a move handle: a small layered top-level window over it, tinted at
  28% opacity with a thin frame and a grip of dots at the bottom right, that never takes the focus. Only the
  handles take the mouse, so clicks elsewhere still pass through. The handle answers `WM_SETCURSOR` with the
  move or resize cursor for the spot under the pointer, and captures the mouse while the button is held.
  Dragging the inside moves the handle and the widget's composition visuals (no redraw). Dragging an edge or a
  corner resizes the handle, within its monitor and down to the widget's `min_size()`; the app calls the
  widget's `resize`, gives its layers new composition surfaces at the new size (`Renderer::resize_layer`, once
  per batch of mouse messages, since a surface cannot change its size), and draws them whole. Releasing the button
  saves what changed ([configuration](configuration.md#moving-and-resizing-widgets)). The overlay windows
  themselves stay click-through. Outside move mode no handle exists, and widgets keep the surfaces they were made
  with.
- `WM_CLOSE` to any of these windows quits like Quit, so tools that close apps the usual way (End task, the
  benchmark runner) end the overlay cleanly and its exit line with the data counters is written. The Pitot
  window, if open, closes with it.

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
- `[general] text_size` ([configuration](configuration.md#text-size)) sets a text scale, `text_size / 100`. The
  text formats and the glyph atlas are made at `TEXT_SIZES` times the scale, and a widget that scales with the text
  (`Widget::scales_with_text`, every widget but the input trace) is given its box divided by the scale through
  `Widget::resize` and draws in those widget pixels: the canvas maps each coordinate to layer pixels, so a whole
  widget pixel lands on a whole layer pixel and edges sharp at the default stay sharp, and a one pixel line keeps a
  pixel. Text sizes are asked for in widget pixels and pick the same of `TEXT_SIZES` as before. At a scale other
  than 1, text whose box reaches past the layer is not drawn, and a number from the atlas wider than its box is not
  drawn rather than drawn with glyphs missing; at 1 nothing of this runs, so the default draws exactly as before.
  A change of the scale resizes every widget, makes the formats and atlas again, empties the layout cache, and
  redraws; nothing is made per frame while it stays.
- Backgrounds (a widget's panel, a band behind its content) are drawn with `Canvas::fill_background`, which
  scales the color's own alpha by `[general] background_opacity` ([configuration](configuration.md#background-opacity));
  everything else uses `Canvas::fill` and stays as it is. A change of the value redraws every widget once.
- A rendering error, such as device loss, drops the renderer, waits 500 ms, rebuilds the device, every window
  target, and every layer, and redraws all widgets.
- A configuration change keeps the device and rebuilds only window targets and layers. Recreating the D3D11
  device each time leaked about 4.6 MB of driver memory per reload.

## Text

- Numbers, signs, separators, and units (`0-9 . , : + - / # % k L` and space) come from a glyph atlas built per
  text size when the device is created: each glyph is rendered once in white, and drawing it is a colored
  `FillOpacityMask` from the atlas, with no DirectWrite work per frame.
- Stable text without digits (names, labels) uses an `IDWriteTextLayout` cached per string, size, alignment, and
  box, created on first use and drawn with `DrawTextLayout`. A string drawn in more than one box (an arrow in two
  columns, `PIT` in two widgets) keeps a layout for each. The cache is cleared past 512 layouts.
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

## Widgets

Every widget is one line of `crates/overlay/src/widgets/list.rs` ([the list](widgets.md)):

```rust
wind: WindWidget = Spec::new("Wind", 1460, 370).size(200, 170).rate(5.0).on(),
```

`wind` is the module (`widgets/wind.rs`) and the widget's `[widgets.wind]` table; `WindWidget` is the type in that
module; the `Spec` is its label in the Pitot window, its default place (physical pixels from the top-left of the
primary monitor), size, and update rate, and `.on()` when it is on by default. From the list come the module
declarations, the defaults and the names the configuration accepts, the Pitot window's switches, the widgets the
render thread makes for a configuration (`Layout::build`, in the list's order), and the variables the pipeline
registers. The `status` widget is a small panel with the connection state, tick, car count, and session, shown in
every display state.

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

**Gap history** (`widgets/gaps.rs`). Every 0.5 s of session time it samples, for each car in the world, the gap on
track to the player and to the next cars ahead and behind it on track, as the clock measured them: the time since
the car in front passed the spot where the other one is now, whatever their laps. Pit road is included as
measured. A sample is a `GapSample { time, to_player, ahead, behind }`: `to_player` is positive when the car is
ahead of the player within half a lap, negative behind, 0 for the player, NaN when the pass was not measured;
`ahead` and `behind` are `Some((car, seconds))` with NaN seconds when not measured. `latest(car)`, `samples(car)`
(oldest first), and `ago(car, seconds)` (for how a gap changed) read it. 240 samples, two minutes, of 16 bytes
per car: 276 KB for 72 cars, allocated at the first observation and again only when the car count changes. A
replay going back in time or a new session starts it over.

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

- `cargo test -p pitot-overlay`: names and labels are unique, rates are in range, every widget is made from its
  defaults, a resized widget equals the widget made at that size (and keeps its layers inside it), its default
  size lays out at every text size without being raised past its smallest, `docs/widgets.md`
  has its row in the list's order, the default file and the defaults agree, and the benchmark configurations still
  turn on exactly the four reference widgets.
- `cargo test -p pitot-overlay --features alloc-check --test alloc_check`:
  `every_widget_and_the_histories_do_not_allocate_per_update` makes every widget of the list and updates them
  through a light race of three cars, counting allocations; with a render device it draws them too, at their
  default size, their smallest, twice the default, and at the largest and smallest text sizes. A widget whose work
  needs other data adds its own test there.

A widget on by default joins the benchmark's measured set unless `benchmarks/pitot/four.toml` and `inputs.toml`
turn it off; the benchmark test fails until both do (then copy them to the `*.local.toml` files with `monitor` set).
Such a widget may also get a section in `DEFAULT_TOML`, at the end.

The rules for every widget stay: no allocation per update or draw after warm-up, a redraw only of what changed,
its own update rate, `visible()` for the display states it shows in, its background through
`Canvas::fill_background` (at the alpha of its `opacity`), and no logging per update.

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
