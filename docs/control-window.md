# The Pitot window

The Pitot window manages the overlay: it turns the whole overlay and each widget on and off, and updates an
installed Pitot. It is part of
`pitot.exe`, the one executable a release ships, but runs as a process of its own: started with `--window`,
`pitot.exe` runs the window; without arguments it runs the overlay. The overlay opens the window by starting its
own executable again with `--window`.

## What it shows

- **Header**: *Pitot* and its version, small and dimmed beside it (the installed version when Pitot is installed,
  else the version it was built as), whether the overlay runs, and *Quit Pitot* on the right. While a downloaded
  update waits, an *Update to vX.Y.Z* button sits below it ([Updates](#updates)). Both stay in place; the
  sections below them scroll when they do not fit.
- **Overlay**: whether the overlay runs, with *Show widgets* (off is the tray menu's *Hide widgets*) and *Move
  widgets* (the tray menu's *Move widgets*, which places and resizes widgets with the mouse and snaps them to the
  display's edges and to each other: see
  [Moving and resizing widgets](configuration.md#moving-and-resizing-widgets)). A change on either side shows on
  the other at once. When no overlay runs, a *Start the overlay* row starts the overlay (`pitot.exe` without
  arguments).
- **Display**: where the widgets show. One row per choice, with a round mark on the right that is filled, and a
  tinted row, for the current one:
  - *Primary display (follow Windows)*, with which display that is now (*Now Display 1 · DELL P2419H*). It leaves
    `[general] monitor` out, so the widgets follow Windows' primary display.
  - *Display 1*, *Display 2*, ...: every connected display in Pitot's order (left to right, then top to bottom;
    *Display 1* is `monitor = 0` in the file), with the monitor's name where Windows provides one (from the
    monitor's EDID, through the display configuration API), its resolution in physical pixels, and *Primary* on
    the primary one.
  - *Display N (not connected)*, *Using the primary display*: when the file names a display that is not connected.
    It is dimmed and cannot be clicked; choosing another row replaces it.

  A click writes `[general] monitor` (or removes it for the primary display) into `config.toml` in place, and
  removes every widget's own `monitor` key, so all widgets move to that display; the overlay reloads the file and
  rebuilds its windows there. While some widgets have a `monitor` of their own, a note above the rows says how
  many (*2 widgets have their own display. Choosing a display moves them too.*), and a click on the current choice
  still moves them. See [Display](configuration.md#display) for the setting, its precedence, and how widgets placed
  on a larger display are kept inside a smaller one. The list is read again when the window opens, each time it is
  activated, and when the overlay reloads the file, so a display connected or removed while the window is in the
  background shows when it comes back.
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
- **Quit Pitot** (in the header): quits the overlay and closes the window.

A problem, such as a configuration file that does not parse, shows in red below the header.

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

The two processes talk through window messages, so neither runs the other's code and the overlay spends nothing on
the window per tick or frame. Both draw with GPUI, each with its own app in its own process.

- The overlay's tray icon belongs to a hidden top-level window of class `PitotTray`. The Pitot window finds it by
  class and posts it a message (`WM_APP + 2`) with a command and a value in its parameters: hello (answer with
  the state), hide or show the widgets, start or end moving, quit, the background opacity or text size while its
  slider moves (a percent), and the background opacity or text size the window just saved. A percent out of a
  setting's range is brought into it. The overlay handles it on GPUI's thread, whose message loop dispatches the
  tray window's messages, as it handles the tray menu.
- The Pitot window owns a message-only window of class `PitotControlLink` on a thread of its own. The overlay
  posts it the state (widgets hidden, moving) after every change and after a hello, a note after each
  configuration reload, a request to come to the front, and a close when the overlay quits.
- Turning widgets on and off and choosing a display go through `config.toml`, which the overlay already reloads.
- The background opacity slider sends each new percent while it moves (at most 101 messages for a drag across
  the track), and the overlay redraws every widget once per value, without a reload. When the slider stops, the
  window saves the value and then tells the overlay what the file now holds (the old value if the save failed),
  so the overlay's reload of that write changes nothing. A slider value holds over the file's in a reload for
  two seconds after it last moved: the reload of an earlier save cannot undo a newer value, and a window that
  closed mid-move does not hold it for ever.
- The text size slider works the same way (at most 17 messages for a drag across the track). For each new value
  the overlay lays every widget out again in its box through `Widget::resize`, which keeps what the widget follows
  of the session, and redraws every widget once; GPUI lays out and rasterizes the text at the new size as it draws.
  Cheap enough to follow the slider at every step, so the slider previews as it moves rather than only on
  release.
- Each process is single-instance through a named mutex (`Local\PitotOverlay`, `Local\PitotControl`).

The message-handling code of both ends is in `crates/overlay/src/platform/link.rs`, so the window's crate has no
`unsafe` of its own.

## Cost

The overlay's own work is unchanged: messages from the window arrive only when the user acts, and the overlay
answers with one posted message. Moving the background opacity slider redraws every shown widget once per new
value, as showing hidden widgets does, and moving the text size slider also lays the widgets out again (above);
nothing is drawn or made while a slider does not move. The benchmark configurations set `start_in_tray = true`, so measurements of the
overlay do not include the window, which would otherwise run as a child process of the overlay.

The window is built with [GPUI](https://www.gpui.rs/) (Apache-2.0), the UI framework of the Zed editor, which
draws with Direct3D 11 and DirectWrite and runs its own task executors. It runs in a process of its own, so closing
it ends everything it started. The overlay draws its widgets with GPUI too, since #183
([rendering](overlay-rendering.md)); before that its process never started GPUI. While open, the window wakes with
the display's refresh to check whether anything needs drawing, and draws only when something changed.

GPUI comes from crates.io as `gpui-pre` 0.3.12 with `gpui-pre-platform` 0.3.12, pinned exactly (`=0.3.12`) as
gpui-kit pins them. They are snapshots of Zed's `gpui` crates (`zed-industries/zed` at `d2a4fb6`, Apache-2.0, with
their license file in the package), published on crates.io by the gpui-component maintainer (`huacnlee`), not by
Zed Industries. On Windows they bring no HTTP client, TLS, or async runtime: `gpui-pre-http-client` holds only
HTTP types, and the executors are GPUI's own (`async-task` and `flume` on Windows thread pool callbacks and the
window thread's messages). Its HLSL shaders are compiled at build time in release builds with the Windows SDK's
`fxc.exe` (found in `PATH`, the SDK, or `GPUI_FXC_PATH`), as GPUI 0.2.2 did, so release binaries carry their
bytecode; debug builds compile them at run time with `d3dcompiler_47.dll`, which Windows ships. The
`runtime_shaders` feature that gpui-kit enables only changes macOS.

The overlay and the window are one executable, so the overlay's process maps the window's code without running
it. Compared with the overlay's own executable in 0.1.3, measured on the lab PC (when the overlay still drew with
Direct2D; the overlay's GPUI is measured at the next release):

- DLLs: the system DLLs that only GPUI imports (`comctl32.dll` for `TaskDialogIndirect`, `dwmapi.dll`, and
  `icuuc.dll`, which also loads `icu.dll`) are delay-loaded (`crates/app/build.rs`), so they load on first use,
  in the window's process. The overlay's process loads the same 49 modules as before; without the
  delay-loading it loaded those four more and started about 1.4 ms later. GPUI 0.3.12 also imports
  `uiautomationcore.dll` (its accessibility tree, through AccessKit) and `winmm.dll` (`timeBeginPeriod`, only while
  it blocks with a timeout); both are delay-loaded too, and the overlay's process does not load them.
- Static initializers: GPUI registers its built-in actions at process start (a few pointers in a list). Nothing
  starts a thread: the overlay's process has the same 7 threads as before, and reaches its first log line in
  the same 14 ms after the process is created.
- Memory: 0.15 MiB more private bytes from the start, the size of the window code's writable data, which
  Windows commits when it maps the executable. Nearly all of it (128 KB) is one lookup table of the OpenEXR
  decoder, which GPUI links in through the `image` crate's default formats; GPUI 0.2.2 offers no way to leave
  them out. Thread-local storage grows by 0.3 KB per thread.
- CPU: the same within noise, 31.23 ms/s before and 31.41 ms/s after (means of six alternating 60 s runs of
  the four reference widgets in a replay; the differences of the pairs ranged from -0.32 to +1.04 ms/s).

The numbers above were measured with GPUI 0.2.2. The window moved to GPUI 0.3.12 after 0.1.5; its numbers, and the
overlay's with it, are measured again at the next release.

GPUI's Windows manifest is in the executable: per-monitor DPI awareness (v2), which the overlay also sets at start,
and version 6 of the common controls, which the overlay does not use.

The window's updates (`velopack` with its HTTP client `ureq`, `rustls` with `ring`, and Mozilla's roots) added
about 2.1 MiB to the executable (6.06 to 8.29 MB). For the overlay's process, compared with 0.1.4 on the lab PC
(three alternating 60 s runs of the four reference widgets in a replay of the AI race, `start_in_tray`):

- DLLs: `ws2_32.dll` (sockets) and `bcrypt.dll` (`BCryptGenRandom`) are delay-loaded like GPUI's, so they load in
  the window's process only. One module more loads at start, `cryptbase.dll`: the TLS library's random numbers
  import `SystemFunction036` from `advapi32.dll`, which the overlay imports itself, and Windows forwards that
  function to `cryptbase.dll` when it loads the executable (48 to 49 modules).
- The installer's start-up hook runs first in every process: outside an installation it checks two paths for
  `Update.exe` and returns. The first log line comes 18 to 23 ms after the process starts, as before.
- CPU 26.60 ms/s before and 26.72 ms/s after (pair differences -0.09 to +0.33 ms/s), private bytes at most 21.0,
  18.6, 18.0 MB before and 20.3, 17.6, 18.0 MB after: the same within noise. The same 6 to 7 threads.

The window's process, measured on the lab PC when it was still `pitot-control.exe` (i7-9700, Parsec virtual
display, release build, `pitot-bench collect`, 60 s while open and idle, the overlay running a replay): 7.0 ms/s
CPU (0.087% of all cores), 22.4 MB private bytes. The window shows about 0.3 s after the process starts (0.4 s
the first time). The executable is about 6 MB; the overlay's alone was about 1.4 MB.
