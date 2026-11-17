# The Pitot window

The Pitot window manages the overlay: it turns the whole overlay and each widget on and off, and updates an
installed Pitot. It is part of
`pitot.exe`, the one executable a release ships, but runs as a process of its own: started with `--window`,
`pitot.exe` runs the window; without arguments it runs the overlay. The overlay opens the window by starting its
own executable again with `--window`.

## What it shows

- **Header**: *Pitot* and its version, small and dimmed beside it (the installed version when Pitot is installed,
  else the version it was built as), and whether the overlay runs. While a downloaded update waits, an
  *Update to vX.Y.Z* button sits below it ([Updates](#updates)).
- **Overlay**: whether the overlay runs, with *Show widgets* (off is the tray menu's *Hide widgets*) and *Move
  widgets* (the tray menu's *Move widgets*, which places and resizes widgets with the mouse). A change on either
  side shows on the other at once. When no overlay runs, a *Start the overlay* row starts the overlay
  (`pitot.exe` without arguments).
- **Appearance**: *Background opacity*, a slider from 0% (no backgrounds) to 100% (opaque backgrounds) with its
  percent shown, for the backgrounds of every widget at once; text and graphs stay as they are (see
  [Background opacity](configuration.md#background-opacity) for what counts as background). A click on the track
  sets it, and dragging the knob moves it; the widgets follow at once, also outside the window while the button
  is held. Releasing the button saves `[general] background_opacity` into `config.toml` in place. After a click,
  the slider has the focus (a ring around its knob), and the arrow keys move it by 1% (Shift: 10%), Page Up and
  Page Down by 10%, Home and End to 0% and 100%; each key's release saves. Without a running overlay the slider
  only saves, and the overlay starts with that value. When the window loses the focus mid-move, the value is
  saved then.

  *Text size* below it is the same kind of slider, from 70% to 150% in steps of 5 with its percent shown, for the
  text of every widget at once, with the rows, columns, and spacing laid out from it; the widgets keep their
  size, so larger text fits fewer rows (see [Text size](configuration.md#text-size)). While it moves, the widgets
  are laid out at each step at once; releasing it saves `[general] text_size` in place. With the focus, the arrow
  keys move it a step (Shift: two), Page Up and Page Down two steps, Home and End to 70% and 150%; a value between
  steps written in the file by hand moves to the next step in the key's direction.
- **Widgets**: one switch per widget, for every widget of the overlay's [list](widgets.md) in its order, with
  the label the list gives it (the window reads the list from `pitot-overlay`, so a new widget needs no change
  here). A switch writes that widget's `enabled` into `%APPDATA%\Pitot\config.toml`
  in place, changing only that value, so comments and layout stay (a missing key goes under the widget's
  `[widgets.<name>]` header, a missing table at the end; see
  [Moving and resizing widgets](configuration.md#moving-and-resizing-widgets) for the same rules). The overlay
  reloads the file as it does after any edit. Edits made elsewhere show when the window is activated again, or
  when the overlay reloads the file.
- **Updates**: a *Check for updates* switch for `[general] check_updates`, saved in place, which always says
  under its label what a check does ("Checks GitHub for new Pitot versions when this window opens. No personal
  data is sent."; [what the update check sends](releases.md#what-the-update-check-sends)), and one quiet line
  below it about the updates ([Updates](#updates)).
- **Quit Pitot**: quits the overlay and closes the window.

A problem, such as a configuration file that does not parse, shows in red above *Quit Pitot*.

The look is plain for now. Colors and sizes are in `crates/control/src/style.rs`, and the layout is in
`crates/control/src/panel.rs`, so a redesign changes those two files.

## Updates

Only a Pitot installed with its installer has updates ([installing and updating](releases.md#installing-updating-and-uninstalling)).
Each time the window opens with *Check for updates* on, it looks for a newer release on a thread of its own and
downloads it, then shows the button; the window stays usable meanwhile, and nothing restarts until the button is
pressed. The line below the switch says where it is:

| Line | When |
|---|---|
| *Checking for updates…* | The releases are being read |
| *Downloading v0.1.5… 40%* | A newer version is downloading |
| (the *Update to v0.1.5* button at the top) | It is downloaded and verified |
| *Up to date* | No release is newer than the installed version |
| *No updates found* | The releases list no installable version |
| *Couldn't check for updates* | The releases could not be read: offline, GitHub's limit of 60 requests an hour without an account, an error |
| *Couldn't download the update* | A download failed or did not match the size and SHA-256 its release lists |
| *Not checking for updates* | *Check for updates* is off: nothing is sent over the network |
| *Updates need the installed Pitot* | This `pitot.exe` does not run from an installation (a build, or a copied file) |
| *Closing the overlay…*, then *Installing v0.1.5…* | The button was pressed |
| *Couldn't install the update* | The installer could not be started; the overlay is started again |

A problem never opens a dialog, and the next opening of the window tries again. Turning the switch on checks at
once; turning it off hides a waiting update (its download stays and is used when the switch is on again).

The button quits the overlay as *Quit Pitot* does, so it logs its counters, and waits for its process to end (at
most 15 s). Then it starts the installer's updater, which waits for the window to close, installs the new version
without a dialog, and starts Pitot again: the overlay, which opens the window again, also with
`start_in_tray = true`. Until the window closes, it cannot be closed by hand, and the overlay's quit does not close
it. See [how updates work](releases.md#how-updates-work) for the requests, the checks of a download, and the test
option `PITOT_UPDATE_SOURCE`.

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
| The overlay quits (*Quit Pitot*, the tray's Quit, or `WM_CLOSE` to its windows) | The window closes too, except while it installs an update |

Closing the window ends its process, so nothing of it runs while it is closed. The window opens where it was
last closed (kept in `%APPDATA%\Pitot\window.toml`), or centered on the primary display when that place is no
longer on a display. While the window is active it stays above the always-on-top widgets; when another window
takes the focus, it goes back behind that window.

## How it reaches the overlay

The two processes talk through window messages, so the overlay carries no part of the window's UI framework and
spends nothing on it per tick or frame.

- The overlay's tray icon belongs to a hidden top-level window of class `PitotTray`. The Pitot window finds it by
  class and posts it a message (`WM_APP + 2`) with a command and a value in its parameters: hello (answer with
  the state), hide or show the widgets, start or end moving, quit, the background opacity or text size while its
  slider moves (a percent), and the background opacity or text size the window just saved. A percent out of a
  setting's range is brought into it. The overlay handles it on its message-loop thread between frames, as it
  handles the tray menu.
- The Pitot window owns a message-only window of class `PitotControlLink` on a thread of its own. The overlay
  posts it the state (widgets hidden, moving) after every change and after a hello, a note after each
  configuration reload, a request to come to the front, and a close when the overlay quits.
- Turning widgets on and off goes through `config.toml`, which the overlay already reloads.
- The background opacity slider sends each new percent while it moves (at most 101 messages for a drag across
  the track), and the overlay redraws every widget once per value, without a reload. When the slider stops, the
  window saves the value and then tells the overlay what the file now holds (the old value if the save failed),
  so the overlay's reload of that write changes nothing. A slider value holds over the file's in a reload for
  two seconds after it last moved: the reload of an earlier save cannot undo a newer value, and a window that
  closed mid-move does not hold it for ever.
- The text size slider works the same way (at most 17 messages for a drag across the track). For each new value
  the overlay lays every widget out again in its box through `Widget::resize`, which keeps what the widget follows
  of the session, makes the text formats and the glyph atlas again, and redraws every widget once: 7 to 14 ms in
  all on the lab PC, of which about 3 ms is the text. Cheap enough to follow the slider at every step, so the
  slider previews as it moves rather than only on release.
- Each process is single-instance through a named mutex (`Local\PitotOverlay`, `Local\PitotControl`).

The message-handling code of both ends is in `crates/overlay/src/platform/link.rs`, so the window's crate has no
`unsafe` of its own.

## Cost

The overlay's own work is unchanged: messages from the window arrive only when the user acts, and the overlay
answers with one posted message. Moving the background opacity slider redraws every shown widget once per new
value, as showing hidden widgets does, and moving the text size slider also lays the widgets out and makes the
text again (above); nothing is drawn or made while a slider does not move. The benchmark configurations set `start_in_tray = true`, so measurements of the
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
