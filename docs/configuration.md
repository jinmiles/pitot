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

## Moving and resizing widgets

Choose *Move widgets* in the tray menu or the Pitot window to place and size widgets with the mouse. Every enabled
widget shows, also without the sim, under a tinted handle with a thin frame and a grip of dots in its bottom-right
corner.

- **Move**: drag the inside of a handle with the left button; the widget follows and stays on its monitor.
- **Resize**: drag an edge (6 px wide) or a corner (16 px along each edge, the grip among them); the cursor shows
  which. The widget takes the new size as it is dragged. It stays on its monitor and gets no smaller than its
  smallest size (below). The opposite edges stay where they are.

Releasing the button saves what changed to `config.toml`: `x` and `y` after a move, `width` and `height` after a
resize, and all four when a left or top edge moved. Only those numbers change in the file, so comments and layout
stay. A missing key goes on the line after the widget's `[widgets.<name>]` header, and a missing table is added at
the end. A widget written as an inline table without those keys cannot be saved this way; the log says so, and the
widget keeps its new place and size until the file is reloaded or the overlay exits. A click that changes nothing
writes nothing.

Resizing is the same as editing `width` and `height` in the file: the widget's box changes, not its text. Text and
shapes keep their pixel size, and each widget lays out within the new box:

| Widget | Smallest | In a larger box |
|---|---|---|
| `status` | 160 x 54 | the second line has more room; the rest stays empty |
| `inputs` | 240 x 60 | a wider trace shows a longer history (2 px per sample, 60 samples a second); a taller one, a taller trace and a wheel up to 72 px |
| `relative` | 400 x 167 | more cars, a pair at a time: one on each side of the player per 67 px of height (rows of 33⅓ px); a wider box widens the names |
| `standings` | 400 x 96 | more rows (24 px each): more cars around the player, then more leaders per class. A shorter box shows fewer cars around the player first, then fewer leaders, then only the player's class, so the player stays in view; a wider one gets back the columns dropped for room |
| `fuel` | 380 x 120 | the four columns and six lines spread out; below the default height the text is a size smaller |
| `pit` | 360 x 110 | more cars (22 px rows); a wider box widens the names |
| `sectors` | 340 x 120 | wider value columns; rows grow to 24 px, then the rest stays empty. Below 16 px a row, the last sectors are left out |
| `wind` | 160 x 140 | a larger dial; the text column keeps its width |
| `tires` | 200 x 150 | larger tire blocks |
| `trackmap` | 200 x 150 | a larger map, with the same dots and road (a box smaller than the default thins them) |

A size below the smallest in the file is raised to it too. While a widget is resized, its composition surfaces are
made again at each new size and drawn whole; outside *Move widgets* nothing of this runs.

Clicks outside the handles still reach the windows below. A right click on a handle opens the tray menu, and
choosing *Move widgets* again (or turning it off in the Pitot window) ends moving. The overlay's own write to the
file does not trigger a reload. A widget's monitor is set in the file.

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
start_in_tray = false  # true: do not open the Pitot window at start

[widgets.inputs]
enabled = true
monitor = 1     # count from the left, 0 is the leftmost; leave out for the primary monitor
x = 660         # physical pixels from the monitor's top-left corner
y = 900
width = 600
height = 120
rate_hz = 60.0  # update rate; leave out for the widget's own rate
opacity = 0.82  # background opacity, 0 to 1
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
| `rate_hz` | 0.1 to 120 |
| `opacity` | 0 to 1 |
| `width`, `height` | 16 to 8192 |
| `shape` | `trackmap` only: `"circle"` (default) or `"outline"` |

Monitors are counted left to right, then top to bottom, by their top-left corner. That order does not change
when Windows renumbers displays.

A configuration change rebuilds the overlay windows, widgets, and composition layers. The GPU device, the
connection to the sim, and the data pipeline keep running, so reloading does not grow memory.
