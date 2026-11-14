# Configuration

The overlay reads `%APPDATA%\Pitot\config.toml`. On first run it writes a commented default file there.

Changes apply when the file is saved, without restarting. The overlay watches the directory with a change
notification (no polling) and reloads 250 ms after the last write. The notification does not say which file
changed, so the overlay compares `config.toml`'s write time and size first: writes to other files there, such as
the log, do not reload. A file that fails to parse or validate is
reported with its line and column, and the previous configuration stays in use. The tray menu's
*Reload configuration* forces a reload.

## Moving widgets

Choose *Move widgets* in the tray menu to place widgets with the mouse. Every enabled widget shows, also without
the sim, under a tinted handle. Drag a handle with the left button; the widget follows and stays on its monitor.
Releasing the button saves the widget's `x` and `y` to `config.toml`. Only those two numbers change in the file,
so comments and layout stay. A missing `x` or `y` goes on the line after the widget's `[widgets.<name>]` header,
and a missing table is added at the end. A widget written as an inline table without `x` and `y` cannot be saved
this way; the log says so, and the widget keeps its new place until the file is reloaded or the overlay exits.

Clicks outside the handles still reach the windows below. A right click on a handle opens the tray menu, and
choosing *Move widgets* again ends moving. The overlay's own write to the file does not trigger a reload. A
widget's monitor and size are set in the file.

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

Captures are looked up in `%APPDATA%\Pitot\captures` and in `captures` next to `pitot-overlay.exe`, where
[`pitot-bench record`](recorder.md) writes them when it runs from that folder.

The log says whether a capture was found (`replay: playing the capture of this session`) and when the widgets
switch to it and back. Without a capture, widgets hide during replays.

## One instance

Only one overlay runs per Windows session. Launching it again while it runs exits at once and leaves the running
overlay, its windows, and its log untouched.

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
(quit from the tray icon); debug builds also print the log to the console.

## Format

```toml
[general]
font = "Segoe UI"

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

Widgets: `status`, `inputs`, `relative`, `standings`, `fuel`, `sectors` ([sectors.md](sectors.md)), `wind`
([wind.md](wind.md)), `tires` ([tires.md](tires.md)), `pit` ([pit.md](pit.md)). Any field left out keeps that
widget's default, so a table can set just `x` and `y`. Unknown widgets or fields are errors, so typos do not go
unnoticed.

The pit widget keeps what it measures, the refueling speed by car and the pit lane loss by track, in
`%APPDATA%\Pitot\pit.toml`. Delete a line there to measure that value again.

| Field | Range |
|---|---|
| `rate_hz` | 0.1 to 120 |
| `opacity` | 0 to 1 |
| `width`, `height` | 16 to 8192 |

Monitors are counted left to right, then top to bottom, by their top-left corner. That order does not change
when Windows renumbers displays.

A configuration change rebuilds the overlay windows, widgets, and composition layers. The GPU device, the
connection to the sim, and the data pipeline keep running, so reloading does not grow memory.
