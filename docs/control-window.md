# The Pitot window

`pitot-control.exe` is the window that manages the overlay: it turns the whole overlay and each widget on and
off. It ships next to `pitot-overlay.exe` and runs as its own process.

## What it shows

- **Overlay**: whether the overlay runs, with *Show widgets* (off is the tray menu's *Hide widgets*) and *Move
  widgets* (the tray menu's *Move widgets*). A change on either side shows on the other at once. When no
  overlay runs, a *Start the overlay* row starts `pitot-overlay.exe` from the same folder.
- **Widgets**: one switch per widget, for every widget of the overlay's [list](widgets.md) in its order, with
  the label the list gives it (the window reads the list from `pitot-overlay`, so a new widget needs no change
  here). A switch writes that widget's `enabled` into `%APPDATA%\Pitot\config.toml`
  in place, changing only that value, so comments and layout stay (a missing key goes under the widget's
  `[widgets.<name>]` header, a missing table at the end; see [Moving widgets](configuration.md#moving-widgets)
  for the same rules). The overlay reloads the file as it does after any edit. Edits made elsewhere show when
  the window is activated again, or when the overlay reloads the file.
- **Quit Pitot**: quits the overlay and closes the window.

A problem, such as a configuration file that does not parse, shows in red above *Quit Pitot*.

The look is plain for now. Colors and sizes are in `crates/control/src/style.rs`, and the layout is in
`crates/control/src/panel.rs`, so a redesign changes those two files.

## Opening and closing

| Action | Result |
|---|---|
| The overlay starts | The window opens, unless `start_in_tray = true` in `[general]` ([configuration](configuration.md)) |
| Left click (or double click) on the tray icon | Opens the window, or brings it to the front |
| *Open Pitot* in the tray icon's right-click menu | The same |
| Starting `pitot-overlay.exe` again while it runs | The same; the second overlay exits at once |
| Starting `pitot-control.exe` again while it runs | Brings the open window to the front; the second one exits at once |
| The window's close button (X) or Alt+F4 | Closes the window; the overlay keeps running in the tray |
| `WM_CLOSE` to the window, for example from another program | Closes the window |
| The overlay quits (*Quit Pitot*, the tray's Quit, or `WM_CLOSE` to its windows) | The window closes too |

Closing the window ends its process, so nothing of it runs while it is closed. The window opens where it was
last closed (kept in `%APPDATA%\Pitot\window.toml`), or centered on the primary display when that place is no
longer on a display. While the window is active it stays above the always-on-top widgets; when another window
takes the focus, it goes back behind that window.

## How it reaches the overlay

The two processes talk through window messages, so the overlay carries no part of the window's UI framework and
spends nothing on it per tick or frame.

- The overlay's tray icon belongs to a hidden top-level window of class `PitotTray`. The Pitot window finds it by
  class and posts it a message (`WM_APP + 2`) with a command and a value in its parameters: hello (answer with
  the state), hide or show the widgets, start or end moving, quit. The overlay handles it on its message-loop
  thread between frames, as it handles the tray menu.
- The Pitot window owns a message-only window of class `PitotControlLink` on a thread of its own. The overlay
  posts it the state (widgets hidden, moving) after every change and after a hello, a note after each
  configuration reload, a request to come to the front, and a close when the overlay quits.
- Turning widgets on and off goes through `config.toml`, which the overlay already reloads.
- Each process is single-instance through a named mutex (`Local\PitotOverlay`, `Local\PitotControl`).

The message-handling code of both ends is in `crates/overlay/src/platform/link.rs`, so the window's crate has no
`unsafe` of its own.

## Cost

The overlay's own work is unchanged: messages from the window arrive only when the user acts, and the overlay
answers with one posted message. The benchmark configurations set `start_in_tray = true`, so measurements of the
overlay do not include the window, which would otherwise run as a child process of the overlay.

The window is built with [GPUI](https://www.gpui.rs/) (Apache-2.0), the UI framework of the Zed editor, which
draws with Direct3D 11 and DirectWrite and runs its own task executors. That is why it is a separate process: the
overlay's process has no async runtime and no UI framework. While open, the window wakes with the display's
refresh to check whether anything needs drawing, and draws only when something changed.

Measured on the lab PC (i7-9700, Parsec virtual display, release build, `pitot-bench collect --name
pitot-control.exe`, 60 s while open and idle, the overlay running a replay): 7.0 ms/s CPU (0.087% of all
cores), 22.4 MB private bytes. The window shows about 0.3 s after the process starts (0.4 s the first time).
The binary is about 5 MB.
