# The Pitot window

The Pitot window manages the overlay: it turns the whole overlay and each widget on and off. It is part of
`pitot.exe`, the one executable a release ships, but runs as a process of its own: started with `--window`,
`pitot.exe` runs the window; without arguments it runs the overlay. The overlay opens the window by starting its
own executable again with `--window`.

## What it shows

- **Overlay**: whether the overlay runs, with *Show widgets* (off is the tray menu's *Hide widgets*) and *Move
  widgets* (the tray menu's *Move widgets*, which places and resizes widgets with the mouse). A change on either
  side shows on the other at once. When no overlay runs, a *Start the overlay* row starts the overlay
  (`pitot.exe` without arguments).
- **Widgets**: one switch per widget, for every widget of the overlay's [list](widgets.md) in its order, with
  the label the list gives it (the window reads the list from `pitot-overlay`, so a new widget needs no change
  here). A switch writes that widget's `enabled` into `%APPDATA%\Pitot\config.toml`
  in place, changing only that value, so comments and layout stay (a missing key goes under the widget's
  `[widgets.<name>]` header, a missing table at the end; see
  [Moving and resizing widgets](configuration.md#moving-and-resizing-widgets) for the same rules). The overlay reloads the file as it does after any edit. Edits made elsewhere show when
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
| Starting `pitot.exe` again while the overlay runs | The same; the second overlay exits at once |
| Starting `pitot.exe --window` while the window is open | Brings the open window to the front; the second one exits at once |
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
draws with Direct3D 11 and DirectWrite and runs its own task executors. That is why it runs in a process of its
own: the overlay's process never starts GPUI, its executors, or its windows. While open, the window wakes with the
display's refresh to check whether anything needs drawing, and draws only when something changed.

The overlay and the window are one executable, so the overlay's process maps the window's code without running
it. Compared with the overlay's own executable in 0.1.3, measured on the lab PC:

- DLLs: the system DLLs that only GPUI imports (`comctl32.dll` for `TaskDialogIndirect`, `dwmapi.dll`, and
  `icuuc.dll`, which also loads `icu.dll`) are delay-loaded (`crates/app/build.rs`), so they load on first use,
  in the window's process. The overlay's process loads the same 49 modules as before; without the
  delay-loading it loaded those four more and started about 1.4 ms later.
- Static initializers: GPUI registers its built-in actions at process start (a few pointers in a list). Nothing
  starts a thread: the overlay's process has the same 7 threads as before, and reaches its first log line in
  the same 14 ms after the process is created.
- Memory: 0.15 MiB more private bytes from the start, the size of the window code's writable data, which
  Windows commits when it maps the executable. Nearly all of it (128 KB) is one lookup table of the OpenEXR
  decoder, which GPUI links in through the `image` crate's default formats; GPUI 0.2.2 offers no way to leave
  them out. Thread-local storage grows by 0.3 KB per thread.
- CPU: the same within noise, 31.23 ms/s before and 31.41 ms/s after (means of six alternating 60 s runs of
  the four reference widgets in a replay; the differences of the pairs ranged from -0.32 to +1.04 ms/s).

GPUI's Windows manifest is in the executable: per-monitor DPI awareness (v2), which the overlay also sets at start,
and version 6 of the common controls, which the overlay does not use.

The window's process, measured on the lab PC when it was still `pitot-control.exe` (i7-9700, Parsec virtual
display, release build, `pitot-bench collect`, 60 s while open and idle, the overlay running a replay): 7.0 ms/s
CPU (0.087% of all cores), 22.4 MB private bytes. The window shows about 0.3 s after the process starts (0.4 s
the first time). The executable is about 6 MB; the overlay's alone was about 1.4 MB.
