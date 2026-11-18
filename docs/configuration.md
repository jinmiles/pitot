# Configuration

The overlay reads `%APPDATA%\Pitot\config.toml`. On first run it writes a commented default file there.

Changes apply when the file is saved, without restarting. The overlay watches the directory with a change
notification (no polling) and reloads 250 ms after the last write. The notification does not say which file
changed, so the overlay compares `config.toml`'s write time and size first: writes to other files there, such as
the log, do not reload. A file that fails to parse or validate is
reported with its line and column, and the previous configuration stays in use. The tray menu's
*Reload configuration* forces a reload.

## The Pitot window

The [Pitot window](control-window.md) (`pitot.exe --window`, which the overlay starts) turns the overlay and each
widget on and off. It opens when the overlay starts, and a left click on the tray icon opens it later. Its close
button closes only the window; the overlay keeps running in the tray.

```toml
[general]
start_in_tray = false
```

- `start_in_tray`: `true` starts the overlay with only its tray icon, without opening the window. The benchmark
  configurations set it, so measurements do not include the window.

Its widget switches write the widget's `enabled` into this file, changing only that value, by the same rules as
moving widgets below.

## Display

The display the widgets show on, for every widget at once. The Pitot window's *Display* section sets it; it can
also be edited here.

```toml
[general]
monitor = 1
```

- `monitor`: a monitor's index, counting from 0 left to right, then top to bottom, by each monitor's top-left
  corner (see [Format](#format)). Left out, which is the default and has no value of its own, the widgets show on
  the primary display, and follow Windows when another display becomes the primary one.
- A widget's own `monitor` (in its `[widgets.<name>]` table) puts that widget on another display and comes before
  `[general] monitor`. It is for setups that split widgets over displays; the Pitot window does not set it.
- A monitor that is not connected counts as left out: a widget whose own monitor is not connected shows on the
  `[general]` one, and `[general] monitor` that is not connected falls back to the primary display. The log says
  which monitors are missing once, when the widgets are laid out with a different set of missing ones.
- `x` and `y` stay relative to the top-left corner of the widget's display. A widget that would reach past the
  display's right or bottom edge, as when it was placed on a larger display, shows just inside it: moved left or
  up as far as needed, and to the display's top-left corner when it is larger than the display. The file keeps
  its numbers, so the widget goes back to its place on a display where it fits. *Move widgets* then moves it from
  where it shows, and saves its place only once it is moved.
- A change rebuilds the overlay windows and widgets on the new display, as other changes do (below).

Choosing a display in the Pitot window writes `[general] monitor`, or removes it for *Primary display*, and
removes every widget's own `monitor` key, so all widgets go to the chosen display. Only those lines change: a
value changes in place, a new `monitor` goes on the line after `[general]` (a missing `[general]` table is added
at the end), and a removed key's line goes with the comment on that line; every other line and comment stays. A
`monitor` key that is not on a line of its own in a table with its own header (an inline table or a dotted key)
cannot be removed this way; the window says so and changes nothing.

## Updates

```toml
[general]
check_updates = true
```

- `check_updates`: with `true` (the default), the Pitot window of an installed Pitot looks for a newer release on
  GitHub each time it opens and downloads it; it installs only when you press its *Update* button
  ([Updates](control-window.md#updates), [how updates work](releases.md#how-updates-work)). With `false`, the
  window sends nothing over the network. The overlay itself never reads this setting and never uses the
  network; a change of only this value reloads without drawing anything again. The window's *Check for updates*
  switch saves it here in place. The benchmark configurations set it to `false`.

## Background opacity

How opaque the widgets' backgrounds are, for every widget at once. The Pitot window's *Background opacity* slider
sets it; it can also be edited here.

```toml
# background_opacity sets how opaque the widgets' backgrounds are, from 0 (none) to 100 (opaque); text and
# graphs stay as they are. At 82 each widget uses its own opacity below. The Pitot window's slider saves it here.
[general]
background_opacity = 82
```

- `background_opacity`: a whole number from 0 to 100. Left out, it is 82, which draws every widget as before the
  setting existed.
- Only backgrounds change: each widget's panel, the standings' class header bands, and the tire widget's corner
  blocks. Text, numbers, graphs and their bar tracks, lines, badges and chips, class colour bars, the highlights
  that mark the player's row (relative, standings, pit stops) and the current sector, the track map's road and
  car dots, and the tire temperature swatches stay as they are.
- Each background keeps its own alpha as a base: the widget's `opacity` (0.82 by default) for its panel, and the
  band's own for a band drawn over the panel (the standings' header bands use the widget's `opacity`, at least
  0.6). At 82 every background is drawn at its base. Below 82 it fades in proportion, to nothing at 0; above 82
  it rises in proportion to opaque at 100. A panel at the default `opacity` therefore has the percent as its
  opacity: 50 draws it at 0.50, 100 at 1.0. A widget given its own `opacity` keeps its difference from the others
  between the ends.
- A change of the value redraws every widget once, without rebuilding windows or widgets. While it does not
  change, nothing more is drawn.

While the slider moves, the window sends each value to the overlay, which draws it at once; releasing the
slider saves the value here, changing only that number (a missing key goes on the line after `[general]`, a
missing `[general]` table is added at the end). The overlay's reload of that write finds the value it already
shows and does nothing. A value from the slider holds over the file's in a reload for two seconds after the
slider last moved, so the reload of one save cannot undo a newer value.

## Text size

The size of the widgets' text, for every widget at once. The Pitot window's *Text size* slider sets it; it can also
be edited here.

```toml
# text_size sets the widgets' text in percent, from 70 to 150, with the rows, columns, and spacing around it; the
# widgets keep their width and height, so larger text fits fewer rows. The Pitot window's slider saves it here.
[general]
text_size = 100
```

- `text_size`: a whole number from 70 to 150. Left out, it is 100, which draws every widget exactly as before the
  setting existed. The slider moves in steps of 5; the file takes any whole number in the range. Below 70 the
  smallest labels (11 px) would fall under 8 px; at 150 a widget at its default size still shows its main rows
  and columns (below).
- Text and what is laid out from it change together: font sizes, row heights, column widths, padding, badges and
  boxes, and the margins and strokes drawn at a fixed number of pixels. A widget is laid out in widget pixels,
  `text_size / 100` screen pixels each: at 150 the default 460 x 300 relative is laid out as a 307 x 200 one and
  drawn half again as large, in the same 460 x 300 box.
- The box does not change: `x`, `y`, `width`, and `height` stay, so the widgets keep their places and do not cover
  each other. Larger text fits fewer rows and columns; smaller text fits more.
- Shapes sized from the box still fill it: the wind dial, the track map's circle, outline, road, and car dots, the
  tire blocks, and the input trace. The input trace draws no text and does not change at all: its trace, pedal
  bars, and steering wheel follow its box at any text size, and the trace keeps 2 px per sample.
- The status line has no size in the default file: its box is made of its two lines and grows and shrinks with
  them. Resized in *Move widgets*, it gets a box of its own like the others.
- What does not fit is left out whole, not overlapped or cut through. Each widget leaves out rows at the bottom and
  its less important columns first when its box is narrow in widget pixels:

| Widget | Left out in a narrow or short box (sizes in widget pixels) | At 150 in its default box |
|---|---|---|
| `status` | (its box is made of its text, see above) | grows to 450 x 81 |
| `inputs` | (no text) | unchanged |
| `relative` | below 400 wide, the iRating and license boxes (the name keeps their room); below 380, the footer's time of day; cars a pair at a time by height | one car on each side, no iRating or license |
| `standings` | the optional columns by width (sectors first, as before), rows by height | 15 rows, the main columns |
| `fuel` | below 380 wide, the fourth column (pit sign, fuel at the end); below 120 tall, the last lap's line | three columns, the average's line |
| `pit` | below 380 wide, the rejoin place and gap; below 300, the pit lap; in the head line, the full stop below 350 and the refueling speed below 230; cars by height | stint, stop, stood, fuel |
| `sectors` | below 340 wide, the car behind; the neighbours' car numbers in the header when a column is under 64; the last sectors by height | four columns |
| `wind` | the dial shrinks beside the text column | a small dial |
| `tires` | below 200 wide, the sets and pit lap in the header; in a block under 54 tall, the distance (or pressure) line; a line of temperatures or wear whose numbers do not all fit (the temperature swatches' colors stay) | temperatures, no wear numbers |
| `trackmap` | (shapes follow the box) | the same map, a larger `PIT` label |

- At a text size other than 100, two rules catch what a widget's own layout does not: a text that would reach past
  the widget's edge is not drawn, and a number wider than its box is not drawn rather than shown with digits
  missing. A long name is still cut at the end of its column, as at 100. A widget's box smaller than its smallest
  size in widget pixels (a box made small at a smaller text size) lays the widget out at its smallest, and what
  reaches past the box's edge is cut there.
- A change lays every widget out again in its box, keeping what it follows of the session (fuel use, lap times,
  gaps), and makes the text formats and glyph atlas again at the new size: about 3 ms, and 7 to 14 ms with the
  redraw of every widget, on the lab PC. Windows, layers, and the GPU device stay. While the value does not change,
  nothing more is made or drawn.

While the slider moves, the window sends each value to the overlay, which lays the widgets out at it at once;
releasing the slider saves it here in place, as the background opacity slider does (above).

## Moving and resizing widgets

Choose *Move widgets* in the tray menu or the Pitot window to place and size widgets with the mouse. Every enabled
widget shows, also without the sim, under a tinted handle with a thin frame and a grip of dots in its bottom-right
corner.

- **Move**: drag the inside of a handle with the left button; the widget follows and stays on its monitor.
- **Resize**: drag an edge (6 px wide) or a corner (16 px along each edge, the grip among them); the cursor shows
  which. The widget takes the new size as it is dragged. It stays on its monitor and gets no smaller than its
  smallest size (below) at the current [text size](#text-size). The opposite edges stay where they are.

Releasing the button saves what changed to `config.toml`: `x` and `y` after a move, `width` and `height` after a
resize, and all four when a left or top edge moved. Only those numbers change in the file, so comments and layout
stay. A missing key goes on the line after the widget's `[widgets.<name>]` header, and a missing table is added at
the end. A widget written as an inline table without those keys cannot be saved this way; the log says so, and the
widget keeps its new place and size until the file is reloaded or the overlay exits. A click that changes nothing
writes nothing.

Resizing is the same as editing `width` and `height` in the file: the widget's box changes, not its text. Text and
shapes keep their pixel size, and each widget lays out within the new box. The smallest sizes are at text size 100;
at another text size they are in widget pixels, so a drag stops at the smallest size times `text_size / 100`:

| Widget | Smallest | In a larger box |
|---|---|---|
| `status` | 160 x 54 | the second line has more room; the rest stays empty |
| `inputs` | 240 x 60 | a wider trace shows a longer history (2 px per sample, 60 samples a second); a taller one, a taller trace and a wheel up to 72 px |
| `relative` | 300 x 167 | more cars, a pair at a time: one on each side of the player per 67 px of height (rows of 33⅓ px); a wider box widens the names, and from 400 px shows the iRating and license boxes |
| `standings` | 400 x 96 | more rows (24 px each): more cars around the player, then more leaders per class. A shorter box shows fewer cars around the player first, then fewer leaders, then only the player's class, so the player stays in view; a wider one gets back the columns dropped for room |
| `fuel` | 270 x 90 | the columns and lines spread out: four columns from 380 px, the last lap's line from 120 px; below the default height the text is a size smaller |
| `pit` | 260 x 110 | more cars (22 px rows); a wider box widens the names, and shows the pit lap from 300 px and where a car rejoins from 380 px |
| `sectors` | 260 x 120 | wider value columns, the car behind from 340 px; rows grow to 24 px, then the rest stays empty. Below 16 px a row, the last sectors are left out |
| `wind` | 130 x 110 | a larger dial; the text column keeps its width |
| `tires` | 140 x 110 | larger tire blocks: their third line from 54 px a block, the sets and pit lap in the header from 200 px |
| `trackmap` | 200 x 150 | a larger map, with the same dots and road (a box smaller than the default thins them) |

A size below the smallest in the file is raised to it too. While a widget is resized, its composition surfaces are
made again at each new size and drawn whole; outside *Move widgets* nothing of this runs.

Clicks outside the handles still reach the windows below. A right click on a handle opens the tray menu, and
choosing *Move widgets* again (or turning it off in the Pitot window) ends moving. The overlay's own write to the
file does not trigger a reload. Widgets move within their display; the [Display](#display) setting chooses it.

## Recording and replays

The overlay records each session while the player drives, into `%APPDATA%\Pitot\captures`, one
[capture](capture-format.md) per sim connection named by its local start time (`20261112-153000.ptc`). While the
sim plays a replay of that session, during the session or from a saved `.rpy` file later, widgets show what they
showed at the replayed moment (see [Sim replays](overlay-pipeline.md#sim-replays)). Nothing else needs to run.

```toml
[recording]
enabled = true
max_gb = 5.0
```

- `enabled`: record sessions. Off, replays show widgets only from captures already there.
- `max_gb`: once the recordings take more than this many gigabytes, the oldest are deleted (after each
  recording and at start). A full-grid session takes about 0.17 GB per hour.
- Changes apply at the next start.
- Ticks while the sim plays a replay are not recorded (they hold live values, not the session), and a
  connection with nothing recorded, such as watching a saved replay, leaves no file.
- Captures hold other drivers' names and IDs from the session info. They stay on this PC; anonymize them with
  [`pitot-bench anonymize`](anonymize.md) before sharing.

Captures are looked up in `%APPDATA%\Pitot\captures` and in `captures` next to `pitot.exe`, where
[`pitot-bench record`](recorder.md) writes them when it runs from that folder.

The log says whether a capture was found (`replay: playing the capture of this session`) and when the widgets
switch to it and back. Without a capture, widgets hide during replays, except the tire widget when the sim's
telemetry files of the session are there (below).

## Telemetry files

While the sim plays a replay, the [tire widget](tires.md) shows the surface temperatures of the replayed moment
from the sim's own telemetry files (`.ibt`). The sim writes them while its telemetry recording is on (in its
options, or with Alt+L in the car), into `Documents\iRacing\telemetry`.

```toml
[telemetry]
folder = ""
```

- `folder`: where the `.ibt` files are. Empty for the sim's own folder in the current Documents folder.

The log says whether files of the session were found (`replay: tire temperatures from the sim's telemetry
files`). The files hold other drivers' names in their session info; the overlay only reads them.

## Track outlines

With `shape = "outline"`, the [track map](trackmap.md) keeps the outline of each track it mapped in
`%APPDATA%\Pitot\tracks`, one `<TrackID>.trk` file per track (8 KB). Deleting a file maps that track again on the
next clean lap. The default `shape = "circle"` needs no outline and does not touch the folder.

## One instance

Only one overlay runs per Windows session. Launching it again while it runs opens the Pitot window (or brings it
to the front) and exits at once, leaving the running overlay and its log untouched. Only one Pitot window runs
too; starting `pitot.exe --window` again brings the open one to the front.

## Log file

The overlay logs to `%APPDATA%\Pitot\pitot.log` and keeps the previous run's log as `pitot.previous.log`. It
records start and exit, configuration problems and reloads, sim connect and disconnect, and render device
recovery, but never telemetry values, session info, or driver names.

At every sim disconnect and at exit, the line also carries the data path's counters since start:

```text
2026-11-11 10:00:00.000 INFO  pitot_overlay::app: sim disconnected ticks=36000 missed=0 torn=0 errors=0 dropped_samples=0
```

| Field | Meaning |
|---|---|
| `ticks` | Ticks read from the sim |
| `missed` | Ticks the sim wrote that were overwritten before they could be read |
| `torn` | Reads where the buffer changed during both copy attempts |
| `errors` | Shared memory read errors |
| `dropped_samples` | Input samples lost because rendering fell a full ring (512 samples, about 8 s) behind |

`missed` and `dropped_samples` are gaps in the input trace. Both are zero for a run without gaps. Release builds have no console window
(quit from the tray icon or the Pitot window); debug builds also print the log to the console.

## Format

```toml
[general]
font = "Segoe UI"
monitor = 1  # every widget's monitor, counted as below; leave out for the primary monitor
start_in_tray = false  # true: do not open the Pitot window at start
background_opacity = 82  # every widget's background, 0 (none) to 100 (opaque); 82 is each widget's opacity
text_size = 100  # every widget's text and the rows and columns around it, 70 to 150 percent
check_updates = true  # false: the Pitot window does not look for updates

[widgets.inputs]
enabled = true
monitor = 1     # count from the left, 0 is the leftmost; leave out for [general] monitor
x = 660         # physical pixels from the monitor's top-left corner
y = 900
width = 600
height = 120
rate_hz = 60.0  # updates per second, 10 to 120; leave out for the widget's own rate
opacity = 0.82  # background opacity, 0 to 1, at the default background_opacity
```

The widgets, their names, and their pages are listed in [widgets.md](widgets.md). Widgets added after the first ten
are off by default and are not written into the default file: turn one on in the Pitot window, or add its table
with `enabled = true`. Any field left out keeps that widget's default, so a table can set just `x` and `y`. Unknown
widgets or fields are errors, so typos do not go unnoticed. A widget can have fields of its own, such as the track
map's `shape`, described on its page; an error in one names the widget and the line of its table.

The pit widget keeps what it measures, the refueling speed by car and the pit lane loss by track, in
`%APPDATA%\Pitot\pit.toml`. Delete a line there to measure that value again.

| Field | Range |
|---|---|
| `monitor` | 0 or more, a whole number; one that is not connected counts as left out ([Display](#display)) |
| `background_opacity` | `[general]` only: 0 to 100, a whole number |
| `text_size` | `[general]` only: 70 to 150, a whole number |
| `rate_hz` | 10 to 120; from 0.1 to below 10 it reads and is raised to 10 ([Update rates](#update-rates)) |
| `opacity` | 0 to 1 |
| `width`, `height` | 16 to 8192 |
| `shape` | `trackmap` only: `"circle"` (default) or `"outline"` |

Monitors are counted left to right, then top to bottom, by their top-left corner. That order does not change
when Windows renumbers displays. The overlay and the Pitot window count them with the same function. The window
numbers them from 1, so its *Display 1* is `monitor = 0`; its numbers follow this order, not the numbers Windows'
display settings show.

A configuration change rebuilds the overlay windows, widgets, and composition layers. The GPU device, the
connection to the sim, and the data pipeline keep running, so reloading does not grow memory. A change of
`background_opacity` alone only redraws the widgets, and a change of `text_size` (with or without
`background_opacity`) lays the widgets out again in their windows and layers and makes the text again.

## Update rates

Every widget updates at least 10 times per second. By default the input trace updates 60 times per second, the
fuel widget 30, and every other widget 10; the status panel, which has no `rate_hz` in the default file, every
100 ms. An update reads the latest data and redraws only what changed, so a widget whose values stand still
draws nothing.

Earlier versions allowed rates down to 0.1, and their files can hold one, such as `rate_hz = 2.0` in the
standings' table. Such a file still reads, with its value kept as written, and the widget updates 10 times per
second instead; `pitot.log` says so once for each such widget each time the widgets are made (at start, and when a
change to the file makes them again). A value below 0.1 or above 120 is an error, as before.
