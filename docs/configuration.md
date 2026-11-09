# Configuration

The overlay reads `%APPDATA%\Pitot\config.toml`. On first run it writes a commented default file there.

Changes apply when the file is saved, without restarting. The overlay watches the directory with a change
notification (no polling) and reloads 250 ms after the last write. A file that fails to parse or validate is
reported with its line and column, and the previous configuration stays in use. The tray menu's
*Reload configuration* forces a reload.

## Log file

The overlay logs to `%APPDATA%\Pitot\pitot.log` and keeps the previous run's log as `pitot.previous.log`. It
records start and exit, configuration problems and reloads, sim connect and disconnect, and render device
recovery, but never telemetry values, session info, or driver names. Release builds have no console window
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

Widgets: `status`, `inputs`, `relative`, `standings`, `fuel`. Any field left out keeps that widget's default,
so a table can set just `x` and `y`. Unknown widgets or fields are errors, so typos do not go unnoticed.

| Field | Range |
|---|---|
| `rate_hz` | 0.1 to 120 |
| `opacity` | 0 to 1 |
| `width`, `height` | 16 to 8192 |

Monitors are counted left to right, then top to bottom, by their top-left corner. That order does not change
when Windows renumbers displays.

A configuration change rebuilds the overlay windows, widgets, and composition layers. The GPU device, the
connection to the sim, and the data pipeline keep running, so reloading does not grow memory.
