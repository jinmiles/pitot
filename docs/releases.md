# Releases

Releases are built and published by the `Release` workflow (`.github/workflows/release.yml`) when a version tag
is pushed. Nothing is built on a developer machine. Pitot ships as an installer made with
[Velopack](https://velopack.io) (MIT), which also gives installed copies their updates.

## Installing, updating, and uninstalling

- **Install**: download `Pitot-win-Setup.exe` from the [latest release](https://github.com/jinmiles/pitot/releases)
  and run it. It installs for your Windows user only, without asking for administrator rights, into
  `%LocalAppData%\Pitot`, adds *Pitot* to the Start menu and the desktop, and starts Pitot: the overlay, and the
  [Pitot window](control-window.md) unless `start_in_tray` is set. The installer is not signed yet, so Windows
  SmartScreen may warn the first time ("More info", then "Run anyway"). Pitot needs the Microsoft Visual C++
  Redistributable (x64), which the sim installs.
- **Update**: when the Pitot window opens, it looks for a newer release and downloads it in the background, then
  shows an *Update to vX.Y.Z* button at its top ([Updates](control-window.md#updates)). Pressing it closes the
  overlay, installs the new version, and starts the overlay and the window again. Nothing updates or restarts on
  its own, so an update never interrupts a session. `check_updates = false` in `[general]`
  ([configuration](configuration.md#updates)) stops the checks.
- **Uninstall**: *Settings > Apps > Installed apps > Pitot > Uninstall*. This stops Pitot and removes
  `%LocalAppData%\Pitot`, the shortcuts, and the entry in Settings.

The settings and everything Pitot writes for you stay in `%APPDATA%\Pitot`, outside the installation:
`config.toml`, `window.toml` (where the Pitot window opens), the logs, `captures`, `tracks`, and `pit.toml`.
Updates leave that folder alone, and so does uninstalling; delete it by hand to remove them too. Installed and
not installed copies of Pitot share it.

| Path | What it holds |
|---|---|
| `%LocalAppData%\Pitot\current\pitot.exe` | The overlay and, with `--window`, the Pitot window |
| `%LocalAppData%\Pitot\current\LICENSE` | Pitot's license (MIT) |
| `%LocalAppData%\Pitot\current\THIRD-PARTY-LICENSES.txt` | The licenses of the code compiled into `pitot.exe` |
| `%LocalAppData%\Pitot\Update.exe` | The installer's updater and uninstaller |
| `%LocalAppData%\Pitot\packages` | The installed version's package and a downloaded update until it is installed |
| `%APPDATA%\Pitot` | Settings, logs, recordings: untouched by installs, updates, and uninstalls |

A `pitot.exe` that does not run from an installation, such as `target\release\pitot.exe` from a build, runs as
before and has no updates: the window says that updates need the installed Pitot.

## Making a release

1. In a pull request titled `release: vX.Y.Z`, set `version` in `[workspace.package]` of `Cargo.toml` and run
   `cargo build` once, so `Cargo.lock` records the new version (the workflow builds with `--locked`).
2. Merge it, then tag the merge commit and push the tag:

   ```text
   git fetch origin
   git tag vX.Y.Z origin/main
   git push origin vX.Y.Z
   ```

The workflow then:

- checks that the tagged commit is on `main` and that the tag matches the workspace version,
- runs the tests, and builds `pitot` and `pitot-bench` in release mode,
- stages `pitot.exe`, its third-party license text, and the repository's `LICENSE` in a folder of their own, and
  zips `pitot-bench` (`.github/scripts/package.py stage`, which fails when `LICENSE` is missing),
- installs the Velopack packaging tool `vpk` (a .NET tool) at the version in `VPK_VERSION`, which must be the
  version of the `velopack` crate in `Cargo.lock` (the step checks it); Velopack asks for the two to match,
- downloads the latest published release's package (`vpk download github --pre`), so `vpk pack` can make a delta
  package from it; the first release with an installer has none, and only the full package is made,
- packs the installer and the update packages: `vpk pack` with package id `Pitot`, the tag's version, title
  (product name) `Pitot`, authors `Woojin Kim`, main executable `pitot.exe`, runtime `win-x64` (the installer
  refuses 32-bit Windows), and the staged folder, without a
  portable zip,
- collects the published files and writes `SHA256SUMS.txt` (`package.py collect`), and publishes a GitHub release
  named `Pitot X.Y.Z` with them, short notes on installing, and the pull requests merged since the previous
  release. Versions before 1.0 are marked as pre-releases.

## What a release holds

| File | Contents |
|---|---|
| `Pitot-win-Setup.exe` | The installer: the full package and the updater, which install Pitot for the current user |
| `Pitot-X.Y.Z-full.nupkg` | The whole installed folder of version X.Y.Z (`pitot.exe`, `LICENSE`, and `THIRD-PARTY-LICENSES.txt`), the package an update downloads |
| `Pitot-X.Y.Z-delta.nupkg` | The difference to the previous release's full package, when there was one to make it from; an update from that release downloads it instead |
| `releases.win.json` | The release feed: each package's version, type (full or delta), file name, size, SHA-1, and SHA-256 |
| `RELEASES` | The same list in the older format of Squirrel, the installer Velopack replaces, when `vpk` writes one |
| `pitot-THIRD-PARTY-LICENSES.txt` | The same text as the installed `THIRD-PARTY-LICENSES.txt`, on its own |
| `pitot-bench-X.Y.Z-windows-x64.zip` | `pitot-bench.exe` and its `THIRD-PARTY-LICENSES.txt`, a developer tool that is not installed |
| `SHA256SUMS.txt` | SHA-256 of every other file, with Unix line endings so `sha256sum -c` reads it |

The names stay the same from release to release, apart from the version in the package names, which the feed
gives. Releases up to 0.1.4 published `pitot.exe` on its own and in a zip instead of the installer and the
packages.

`THIRD-PARTY-LICENSES.txt` lists what is compiled into the binaries: the code ported from other projects (the
Third-Party Notices section of `README.md`) and every Rust crate linked in, with its license files.
`.github/scripts/package.py` finds the crates with `cargo metadata` for the Windows target: the binary's normal
dependencies, without proc macros and the crates only they use, since those run at build time and are not in the
binary. Each crate's license files are the ones at the top of its package (`LICENSE*`, `COPYING*`, `NOTICE*`,
`UNLICENSE`); a text that repeats an earlier one word for word is referred to instead of repeated. Packaging fails
when a crate has no license or no license file, so a new dependency without them is caught. The installer's
updater (`Update.exe`) and setup program come with `vpk` (Velopack, MIT) and are not in that list.

The binaries are not signed yet, and they use the Microsoft Visual C++ runtime (installed with the sim).

## How updates work

Only the Pitot window's process (`pitot.exe --window`) looks for, downloads, and installs updates, on threads of
its own; the overlay's process never uses the network. The window does it with the `velopack` crate (default
features off), whose HTTP client is `ureq` with `rustls` (`ring`) and Mozilla's root certificates, without an
async runtime.

1. When the window opens with `check_updates` on, it asks the GitHub API for the repository's latest ten releases,
   pre-releases included (every release before 1.0 is one), and reads `releases.win.json` from each that has it
   ([what the update check sends](#what-the-update-check-sends) lists the requests).
2. When the newest full release is newer than the installed version, compared as semantic versions (so
   `0.2.0-rc.1` is newer than `0.1.9`, and `0.1.5-rc.1` is older than `0.1.5`), it downloads that release's
   package from the release into `%LocalAppData%\Pitot\packages` (the delta packages instead, when they lead from
   the installed version). Each downloaded file must have the size and SHA-256 that the feed lists before it is
   used; one that does not stays a `.partial` file, which is never installed, and the window says it could not
   download the update. Delta packages are checked the same way before they are applied to the installed
   version's package; when that fails, the full package is downloaded instead.
3. The *Update to vX.Y.Z* button tells the overlay to quit as *Quit Pitot* does (it logs its counters), waits up
   to 15 s for its process to end, and starts `Update.exe`, which waits for the window's process to exit, stops
   anything still running from the installation, replaces `current`, and starts `pitot.exe` again. Started that
   way, the overlay opens the Pitot window also with `start_in_tray`, since the window was open. If the update
   cannot be applied, `Update.exe` starts the old version again.

`pitot.exe` runs the installer's start-up hook first in every mode (`velopack::VelopackApp`, in
`crates/app/src/main.rs`): the installer, `Update.exe`, and the uninstaller start it with arguments of their own
around each step, and the hook ends those starts at once. It never installs a downloaded update on a start (the
library's default would, and would stop a running overlay), so updates install only from the button. Run outside
an installation, the hook looks for `Update.exe` next to the executable and its folder, finds none, and returns.

## What the update check sends

This section is the privacy statement for Pitot's update check.

The overlay never uses the network. Only the Pitot window of an installed Pitot does, and only with
`check_updates` on in `[general]` (the default). It checks each time the window opens, and when its *Check for
updates* switch is turned on; never in the background otherwise, and never while the window is closed. The
window says so next to the switch at all times: "Checks GitHub for new Pitot versions when this window opens. No
personal data is sent."

What a check requests, all over HTTPS from GitHub, which hosts Pitot's releases:

1. `GET https://api.github.com/repos/jinmiles/pitot/releases?per_page=10&page=1`: the list of the latest ten
   releases.
2. For each of those that has one, `GET` its `releases.win.json`
   (`https://github.com/jinmiles/pitot/releases/download/<tag>/releases.win.json`, which GitHub redirects to its
   file storage, `release-assets.githubusercontent.com`): which packages the release has, with their sizes and
   checksums.
3. Only when a newer version exists: the release list once more, and the newer version's update packages from its
   release, the same way.

Pressing the *Update* button requests nothing more: it installs the package already downloaded.

Each request carries only the headers the HTTP client needs: `Host`, `Accept`, `Accept-Encoding`, and
`User-Agent: ureq/<version>` (the name and version of the HTTP library, the same for every program that uses it).
No account, installation id, Pitot version, setting, telemetry, file name, or anything else from the PC is sent,
and nothing is sent to anyone but GitHub. As with any web request, GitHub and the networks in between see the PC's
IP address and the time of the request; [GitHub's privacy statement](https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement)
covers what GitHub does with them. The Pitot project runs no server and receives nothing.

On the PC, the downloaded packages are kept in `%LocalAppData%\Pitot\packages`, where the installer's library
also writes a random id in `.betaId`, for staged roll-outs on other kinds of release servers; GitHub releases do
not use it, and it is never sent. Uninstalling removes the folder.

To turn the check off, switch *Check for updates* off in the Pitot window, or set `check_updates = false` in
`%APPDATA%\Pitot\config.toml`. The window then sends nothing at all. A `pitot.exe` that is not installed never
checks.

## Testing updates without publishing

`PITOT_UPDATE_SOURCE` is a developer option: when it is set in the window's environment, the window takes its
releases from it instead of GitHub. It is a folder holding a `releases.win.json` and the packages it lists (the
files a dry run's artifact holds), or the URL of a web server that serves such a folder (it then asks for
`releases.win.json?localVersion=...&id=Pitot&stagingId=...`, which a GitHub source never sends). The window is
usually started by the overlay, so set the variable before starting the overlay:

```text
set PITOT_UPDATE_SOURCE=D:\pitot-test\0.1.5-test.2
"%LocalAppData%\Pitot\current\pitot.exe"
```

To test an update from one build to another: run the workflow by hand on a branch twice, with *Package version*
set to an older and a newer test version (such as `0.1.5-test.1` and `0.1.5-test.2`), download both artifacts
(`gh run download <run id> -n release -D <folder>`), install the older one with its `Pitot-win-Setup.exe`, and
start it with `PITOT_UPDATE_SOURCE` set to the newer artifact's folder. Uninstall it from Settings afterwards.

## Dry runs

Pull requests that change the workflow, the packaging script, `Cargo.lock`, or `README.md` run the same build and
packaging, installer included, without publishing; the files a release would publish are kept as the run's
`release` artifact for seven days. The workflow can also be started by hand (`workflow_dispatch`) for a dry run of
any branch, optionally with another package version than the workspace's. Dry runs make only the full package:
their version may be older than the published one, which a delta needs.
