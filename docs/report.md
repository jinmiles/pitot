# Report

`pitot-bench report` turns a [runner](runner.md) series into numbers with their uncertainty.

```text
pitot-bench report <series.json> [--force]
```

It reads `bench-results/<scenario>/series/<series-id>.json` and writes, under the scenario:

- `report-<series-id>.md`: the report to read.
- `report-<series-id>.json`: the same content for scripts, including the series record.

## Stages

1. **Raw**: each run's `samples.csv`, written by the [collector](collector.md).
2. **Processed**: each measured run's `metrics.json`, computed from `samples.csv` on the first report. Later
   reports reuse it; `--force` computes it again.
3. **Report**: per app, the mean of the runs' metrics, differences against the baselines, and the budget.

## Metrics per run

Means are weighted by each sample's duration.

| Metric | Meaning |
|---|---|
| `cpu_pct` | CPU of the process tree as a percentage of all logical processors |
| `cpu_ms_per_s` | CPU time per second of wall time (10 ms/s is 1% of one core) |
| `cpu_peak_ms_per_s` | The busiest sample's CPU, estimated from its cycles (see below) |
| `cpu_mcycles_per_s` | CPU cycles per second, in millions |
| `private_mb_first`, `_last`, `_max`, `_mean` | Private bytes, in MB of 2^20 bytes |
| `private_growth_mb_per_h` | Least squares slope of private bytes over the run |
| `working_set_mb_mean` | Working set |
| `gpu_pct`, `gpu_3d_pct` | Busiest GPU engine and the 3D engine of the process tree |
| `gpu_dedicated_mb_mean`, `gpu_shared_mb_mean` | GPU memory held for the process tree |
| `dwm_cpu_ms_per_s`, `dwm_gpu_pct` | The desktop compositor |
| `collector_cpu_ms_per_s` | The collector's own CPU time |

**The busiest sample.** Windows counts CPU time in clock ticks of 15.6 ms, so a one-second sample of a light
process reads 0, 15.6, or 31.2 ms, and no sample can show a spike smaller than a tick. Cycle counts are exact per
sample. The report converts each sample's cycles to time at the run's average CPU time per cycle
(`total cpu_ms / total cpu_mcycles`). Clock speed changes within a run make this an estimate.

**Growth.** A slope over a three-minute run mostly shows start-up allocations and caches filling. Memory growth
budgets need long runs, such as the 60-minute scenario.

## Across runs

- **Mean and interval.** The mean of a metric over an app's measured runs, with a 95% confidence interval from
  Student's t distribution (n - 1 degrees of freedom). One run gives a mean without an interval.
- **Against the baselines.** DWM, the GPU, and the collector belong to the whole desktop. For these, the report
  gives each app's runs minus the interleaved baseline runs, with Welch's interval, which does not assume equal
  variances. An interval that contains zero means no measurable difference.
- Baselines have no app, so the app's own values are shown as `-` for them.
- Runs that were not measured (app not running after the warm-up, launch failed) are listed and left out of the
  means.

The t quantiles come from the regularized incomplete beta function and are checked against tables in the unit
tests.

## Budget

A plan can state limits in a `[budget]` table; the report checks every app against them:

```toml
[budget]
cpu_pct = 0.2               # mean CPU, % of all cores
cpu_peak_ms_per_s = 10      # busiest sample, 10 ms/s = 1% of one core
private_mb = 30             # highest private bytes
private_growth_mb_per_h = 0.1
dwm_no_increase = true      # DWM CPU and GPU no higher than the baselines
```

| Verdict | Means | Highs and peaks | DWM |
|---|---|---|---|
| within | whole interval at or below the limit | worst run at or below the limit | interval of the difference reaches zero or below |
| over | whole interval above the limit | worst run above the limit | whole interval of the difference above zero |
| inconclusive | interval contains the limit | - | - |

With a single run, a mean is judged by its value alone.

## Limits

- One series per report. Combining series, or comparing machines, is left to the reader; the report states the
  machine, OS build, and plan so that results are not mixed by accident.
- The machine is read from the first measured run. All runs of a series run on the same machine.
