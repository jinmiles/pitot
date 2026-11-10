# Runner

`pitot-bench run` executes a measurement series from a plan file. Every app, Pitot included, goes through the same
steps: files in place, replay started, app launched, warm-up, collection, everything stopped, files restored.
Runs without an overlay (baselines) are interleaved with the app runs.

```text
pitot-bench run <plan.toml> [--dry-run]
```

`--dry-run` checks the plan, reports running overlays, and prints the run order without running anything.
Paths in the plan are relative to the working directory, like the `bench-results/` output.

## Plan

```toml
scenario = "tier-a-race"   # output under bench-results/<scenario>/
repeats = 5                # runs per app; baselines run repeats + 1 times
warmup_s = 60
measure_s = 180
cooldown_s = 10            # pause between runs
interval_ms = 1000         # sample interval, at least 100
# seed = 1234              # run order seed; a random one is chosen and recorded when left out
# detect = ["Other.exe"]   # more executable names to warn about

[replay]                   # leave out to measure with the sim not running
irdt = "data/irdashies/ai-race-10min.irdt"   # or ibt = "..."
speed = 1.0

[[app]]
name = "pitot"             # output directory name; not "baseline" or "series"
command = "target/release/pitot-overlay.exe"
args = []
# measure = ["other.exe"]  # executable names to measure; default: the launched process tree
files = [{ from = "benchmarks/pitot/full.toml", to = "%APPDATA%/Pitot/config.toml" }]

[budget]                   # optional limits the report checks; see report.md
cpu_pct = 0.2
```

- `command`, `files.from`, and `files.to` expand `%NAME%` to environment variables.
- Use `measure` for an app whose launcher exits or starts the real app outside its own process tree. Every
  process with those names, and their descendants, is measured.
- Unknown keys are errors, so typos do not go unnoticed.

## Order

Each repeat is a baseline followed by every app in a random order, and the series ends with a baseline. With two
apps and three repeats:

```text
baseline, B, A, baseline, A, B, baseline, B, A, baseline
```

Drift over a series, such as a warming CPU or background activity, then affects apps and baselines alike. The
order follows from the seed, which is recorded, so a series can be repeated in the same order.

## One run

1. **Files.** The app's `files` are copied into place. Existing destinations are backed up first.
2. **Replay.** With a `[replay]` section, `pitot-bench replay --loop` starts in a child process, logging to the
   run's `replay.log`. The run waits until the replay publishes its first frame. The replay runs 10 s longer than
   warm-up and measurement, so the measurement never sees it stop.
3. **Launch.** The app starts with its arguments, and with standard input and output closed. Baselines launch
   nothing.
4. **Warm-up.** `warmup_s` from the launch. If no process of the app is running afterwards, the run is recorded as
   `not_running` and nothing is measured.
5. **Collection.** The [collector](collector.md) samples the app's process tree for `measure_s`. Baselines collect
   an empty tree, which records DWM and the GPU for comparison.
6. **Stop.** The measured tree and every process the run started are terminated, without asking the app to close,
   the same for every app. The replay is given time to finish and write its summary.
7. **Restore.** Backed-up files are put back and added files are removed.
8. **Cool-down.** `cooldown_s` before the next run.

Processes the runner starts are in a job object that terminates them when the run ends, also if the runner
itself exits early. Backups are listed in `bench-results/.restore/` before any file is replaced, and an
interrupted series is restored at the start of the next one.

## Checks before a series

- An app of the plan that is already running stops the series: a single-instance app would exit at launch and be
  measured as not running.
- A replay plan stops if the sim or another replay already publishes SDK shared memory.
- Missing commands (given as paths), files, or recordings stop the series.
- Known overlays and the sim are reported as warnings and recorded: RivaTuner Statistics Server and MSI
  Afterburner, Discord, the NVIDIA and AMD overlays, Steam, Xbox Game Bar, CapFrameX, SimHub, and the Pitot
  overlay, plus the plan's `detect` names. Close them before an official measurement.

## Output

```text
bench-results/<scenario>/series/<series-id>.json
bench-results/<scenario>/<app>/<series-id>-<position>/collect.json
                                                     /samples.csv
                                                     /run.json
                                                     /replay.log
```

- The series ID is the local start time, `YYYYMMDD-HHMMSS`; the position is the run's place in the series.
- The series file holds the plan as run, the seed, the overlays found at the start, and each run's directory and
  outcome. It is rewritten after every run, so an interrupted series keeps what it finished.
- `run.json` holds the app, command, launched PID, start time, and outcome: `measured` with the collector's
  summary, `not_running`, or `launch_failed` with the error.

## Limits

- Whether an app shows the replayed data, rather than only running, is not checked. Check each app by eye once
  before its first series.
- Apps that need the sim's process or window cannot be measured in Tier A and are not made to run with a fake
  one.
- Terminating an app skips its own shutdown, so settings it saves on exit are not saved. Measurement ends before
  that.
