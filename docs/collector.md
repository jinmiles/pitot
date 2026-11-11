# Collector

`pitot-bench collect` samples an overlay once per interval and writes the raw samples. Every overlay goes through
the same code path, Pitot included.

```text
pitot-bench collect (--pid <n> | --name <exe>...) [--seconds <n>] [--interval-ms <n>] [--out <dir>]
```

- `--name` can repeat. Names match without case, with or without `.exe`.
- Defaults: 60 s, one sample per 1000 ms (at least 100 ms), output in
  `bench-results/adhoc/<target>/<YYYYMMDD-HHMMSS>/`.
- The target does not have to run at the start. Until it does, samples show zero processes.
- A summary is printed at the end; the files hold everything.

## What counts as the target

The target is a process tree: every process the PID or names match, plus all their descendants, found again at
every sample. This matters for multi-process overlays (Electron main, GPU, and renderer processes, helpers):
measuring only the main process is the most common way overlay comparisons go wrong. A child is accepted only if
it was created after its parent, so a parent PID reused by an unrelated process is not followed.

- The first sample is a baseline and is not written.
- A process already running at the start counts from the baseline. A process started during the run counts from
  its creation.
- A process that exits keeps its CPU time up to its exit: its handle stays open until its final totals are read.
- Processes that cannot be opened, typically elevated processes when the collector is not elevated, are listed in
  `collect.json` under `unreadable_processes` and are not measured. Run the collector elevated in that case.

## `samples.csv`

One row per sample. Empty fields mean the source was not available.

| Column | Unit | Meaning | Source |
|---|---|---|---|
| `t_s` | s | Time since the baseline sample | Monotonic clock |
| `dt_s` | s | Time this row covers | Monotonic clock |
| `processes` | count | Live processes in the tree | Process list (Toolhelp) |
| `cpu_ms` | ms | CPU time (kernel and user) of the tree in this row, in clock ticks | `GetProcessTimes` |
| `cpu_mcycles` | 10^6 cycles | CPU cycles of the tree in this row | `QueryProcessCycleTime` |
| `cpu_pct` | % | `cpu_ms` over `dt_s` and all logical processors | Derived |
| `private_bytes` | bytes | Private bytes of the live processes | `GetProcessMemoryInfo` (`PrivateUsage`) |
| `working_set_bytes` | bytes | Working set of the live processes | `GetProcessMemoryInfo` |
| `gpu_pct` | % | Busiest GPU engine type | PDH `GPU Engine` |
| `gpu_3d_pct` | % | 3D engine | PDH `GPU Engine` |
| `gpu_dedicated_bytes` | bytes | Dedicated GPU memory held for the tree | PDH `GPU Process Memory` |
| `gpu_shared_bytes` | bytes | Shared system memory held by the GPU for the tree | PDH `GPU Process Memory` |
| `dwm_cpu_ms` | ms | CPU time of the desktop compositor (`dwm.exe`) | PDH `Process(dwm)` |
| `dwm_gpu_pct` | % | Busiest GPU engine type of `dwm.exe` | PDH `GPU Engine` |
| `collector_cpu_ms` | ms | CPU time of the collector itself, in clock ticks | `GetProcessTimes` |
| `collector_mcycles` | 10^6 cycles | CPU cycles of the collector itself | `QueryProcessCycleTime` |

Notes on the sources:

- **CPU time comes from cycles.** `QueryProcessCycleTime` counts the cycles a process's threads run on a
  counter that has a fixed rate on current x64 CPUs (the invariant time-stamp counter, at the nominal clock speed),
  whatever the actual clock speed. At the start of every run the collector measures that rate: the time-stamp
  counter against the monotonic clock over 100 ms. Cycles over the rate are CPU time, exact per row. The rows keep
  the raw counts; the summary and the [report](report.md) do the conversion. On the lab PC (i7-9700, nominal
  3.0 GHz) the rate is 3,000.0 MHz. Runs before this measured the cycles of busy loops on the collector's own
  thread instead, which read 2,985 to 2,997 MHz on an idle machine and 2,661 MHz once under heavy load from other
  processes, because a thread's cycles leave out the time it is preempted or its core serves interrupts; CPU time
  from such a run is high by the same ratio, 0.1 to 0.5% on an idle machine.
- **Clock ticks** are how the OS counts CPU time (`GetProcessTimes`, the `cpu_ms` columns): in whole ticks of
  15.6 ms by default. A one-second row of a light process reads 0, 15.6, or 31.2 ms, and a process that runs for
  a moment at the same point of every tick is counted a whole tick every time, or never. On the lab PC, three
  120 s runs of the same replay gave 58.0 to 58.8 Mcycles/s but 18.4 to 23.7 ms/s in clock ticks; an overlay
  waking once per second showed no tick at all over 120 s; and the collector itself, which wakes once per second,
  read exactly one tick (15.6 ms) per row while its cycles came to 7.6 ms/s. The columns are kept for comparison.
- **Per-process values come from handles keyed by PID.** PDH `Process` instances are named by executable
  (`app`, `app#1`, ...), and those names shift as processes start and exit, so series of same-named processes can
  swap. Handles avoid that.
- **GPU engines** are summed per adapter and engine type over the tree, and the busiest is reported, as Task
  Manager shows a process's GPU use. PDH GPU counters need a WDDM 2 driver; without them the GPU columns stay
  empty. One engine of one process reads 0 to 100%, but PDH sometimes reports a wild value for one sample
  (1.8e14% for `dwm.exe` once in a 40-minute run). Values outside 0 to 101% are skipped, and up to 101% counts
  as 100%.
- **DWM** is read through PDH, which needs no handle, so it works without elevation. DWM's CPU and GPU belong to
  the whole desktop; compare them against runs without an overlay.
- Counter paths are English (`PdhAddEnglishCounterW`), so localized Windows works.

On the lab PC (i7-9700, Windows build 26300), a 30 s collection of the overlay during a replay gave
27.08 ms/s CPU and 14.1 to 15.0 MB private bytes; PDH `\Process(pitot-overlay)\% Processor Time` and
`Private Bytes` over the same 30 s gave 27.28 ms/s and 14.8 MB on average.

## `collect.json`

Written at the start and completed at the end:

- `tool`, `version`, `target` (`{"pid": n}` or `{"names": [...]}`), `started_local`, `interval_ms`,
  `requested_seconds`.
- `machine`: CPU name, logical processors, the measured cycle counter rate (`cycle_rate_mhz`), and the OS
  product name, display version, build, and update build revision from the registry. Windows 11 still reports a `Windows 10` product name there; build 22000 or later is
  Windows 11.
- `sources`: whether GPU and DWM counters were available.
- `unreadable_processes`: tree members that could not be opened.
- `summary`: sample count and duration, maximum process count, time-weighted means of CPU (percent and ms/s),
  the highest per-row CPU, private bytes at the first and last row and at most, GPU and DWM means, and the
  collector's own CPU. CPU comes from cycles at `cycle_rate_mhz`, also recorded in the summary; without a
  measured rate it comes from clock ticks and the summary has no rate.

## Collector overhead

The collector's own CPU is recorded in every row. It grows with the size of the tree and with the number of GPU
counter instances on the system: 7.6 ms/s (from cycles) for a single-process target on the lab PC, most of it
reading the per-process GPU engine counters. It runs in its own process, so it is not part of any app's
numbers.
