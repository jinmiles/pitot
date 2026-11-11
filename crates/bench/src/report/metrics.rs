//! Processed metrics of one run, computed from its raw `samples.csv`.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::stats;
use crate::collect::CpuClock;

/// 2^20 bytes, as the collector's summary uses.
const MB: f64 = 1024.0 * 1024.0;

/// One row of `samples.csv`, by column name. Empty fields are sources that were not available.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Row {
    pub t_s: f64,
    pub dt_s: f64,
    pub processes: u32,
    pub cpu_ms: f64,
    pub cpu_mcycles: f64,
    pub cpu_pct: f64,
    pub private_bytes: u64,
    pub working_set_bytes: u64,
    pub gpu_pct: Option<f64>,
    pub gpu_3d_pct: Option<f64>,
    pub gpu_dedicated_bytes: Option<u64>,
    pub gpu_shared_bytes: Option<u64>,
    pub dwm_cpu_ms: Option<f64>,
    pub dwm_gpu_pct: Option<f64>,
    pub collector_cpu_ms: f64,
    /// Missing in samples from before the collector counted its own cycles.
    #[serde(default)]
    pub collector_mcycles: Option<f64>,
}

/// Reads every row of a `samples.csv`.
pub fn read_samples(path: &Path) -> Result<Vec<Row>, csv::Error> {
    csv::Reader::from_path(path)?.deserialize().collect()
}

/// `metrics.json`: what the report needs from one run. Time means are weighted by each row's duration.
///
/// CPU time is the tree's cycles at the cycle counter rate the collector measured (`cycle_rate_mhz`). Runs
/// collected before the rate was measured have no rate; their CPU time is the OS's clock-tick count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunMetrics {
    pub samples: usize,
    pub seconds: f64,
    pub processes_max: u32,
    #[serde(default)]
    pub cycle_rate_mhz: Option<f64>,
    /// Mean CPU of the process tree as a percentage of all logical processors.
    pub cpu_pct: f64,
    pub cpu_ms_per_s: f64,
    /// CPU time as the OS counts it, in clock ticks (15.6 ms by default), for comparison.
    #[serde(default)]
    pub cpu_tick_ms_per_s: Option<f64>,
    pub cpu_mcycles_per_s: f64,
    /// The busiest row's CPU. Without a cycle rate it is estimated from the row's cycles at the run's average CPU
    /// time per cycle, since rows of clock-tick time are useless for spikes below a tick.
    pub cpu_peak_ms_per_s: Option<f64>,
    pub private_mb_first: f64,
    pub private_mb_last: f64,
    pub private_mb_max: f64,
    pub private_mb_mean: f64,
    /// Least squares slope of private bytes over the run.
    pub private_growth_mb_per_h: Option<f64>,
    pub working_set_mb_mean: f64,
    pub gpu_pct: Option<f64>,
    pub gpu_3d_pct: Option<f64>,
    pub gpu_dedicated_mb_mean: Option<f64>,
    pub gpu_shared_mb_mean: Option<f64>,
    pub dwm_cpu_ms_per_s: Option<f64>,
    pub dwm_gpu_pct: Option<f64>,
    pub collector_cpu_ms_per_s: f64,
}

/// Mean of `value` weighted by row duration, over the rows that have it.
fn weighted(rows: &[Row], value: impl Fn(&Row) -> Option<f64>) -> Option<f64> {
    let (sum, time) = rows
        .iter()
        .filter_map(|r| value(r).map(|v| (v * r.dt_s, r.dt_s)))
        .fold((0.0, 0.0), |(a, b), (v, t)| (a + v, b + t));
    (time > 0.0).then(|| sum / time)
}

impl RunMetrics {
    /// `None` for a run without rows. `cores` is the number of logical processors, for the percentage of all
    /// cores when CPU time comes from cycles.
    pub fn of(rows: &[Row], clock: CpuClock, cores: u32) -> Option<Self> {
        let (first, last) = (rows.first()?, rows.last()?);
        let seconds: f64 = rows.iter().map(|r| r.dt_s).sum();
        let rate = |v: f64, r: &Row| if r.dt_s > 0.0 { v / r.dt_s } else { 0.0 };
        let total_ms: f64 = rows.iter().map(|r| r.cpu_ms).sum();
        let total_mcycles: f64 = rows.iter().map(|r| r.cpu_mcycles).sum();
        let tick_ms_per_s = weighted(rows, |r| Some(rate(r.cpu_ms, r))).unwrap_or(0.0);
        let (cpu_ms_per_s, cpu_pct, cpu_peak_ms_per_s) = match clock.cycle_rate_mhz {
            Some(_) => {
                let ms_per_s = weighted(rows, |r| Some(rate(clock.ms(r.cpu_mcycles, r.cpu_ms), r)))
                    .unwrap_or(0.0);
                let peak = rows
                    .iter()
                    .map(|r| rate(clock.ms(r.cpu_mcycles, r.cpu_ms), r))
                    .fold(0.0, f64::max);
                (
                    ms_per_s,
                    ms_per_s / (10.0 * f64::from(cores.max(1))),
                    Some(peak),
                )
            }
            None => {
                let peak = (total_mcycles > 0.0).then(|| {
                    let ms_per_mcycle = total_ms / total_mcycles;
                    rows.iter()
                        .map(|r| rate(r.cpu_mcycles * ms_per_mcycle, r))
                        .fold(0.0, f64::max)
                });
                (
                    tick_ms_per_s,
                    weighted(rows, |r| Some(r.cpu_pct)).unwrap_or(0.0),
                    peak,
                )
            }
        };
        let collector_ms = |r: &Row| match r.collector_mcycles {
            Some(mcycles) => clock.ms(mcycles, r.collector_cpu_ms),
            None => r.collector_cpu_ms,
        };
        let growth = stats::slope(
            rows.iter()
                .map(|r| (r.t_s / 3600.0, r.private_bytes as f64 / MB)),
        );
        Some(Self {
            samples: rows.len(),
            seconds,
            processes_max: rows.iter().map(|r| r.processes).max().unwrap_or(0),
            cycle_rate_mhz: clock.cycle_rate_mhz,
            cpu_pct,
            cpu_ms_per_s,
            cpu_tick_ms_per_s: Some(tick_ms_per_s),
            cpu_mcycles_per_s: weighted(rows, |r| Some(rate(r.cpu_mcycles, r))).unwrap_or(0.0),
            cpu_peak_ms_per_s,
            private_mb_first: first.private_bytes as f64 / MB,
            private_mb_last: last.private_bytes as f64 / MB,
            private_mb_max: rows.iter().map(|r| r.private_bytes).max().unwrap_or(0) as f64 / MB,
            private_mb_mean: weighted(rows, |r| Some(r.private_bytes as f64 / MB)).unwrap_or(0.0),
            private_growth_mb_per_h: growth,
            working_set_mb_mean: weighted(rows, |r| Some(r.working_set_bytes as f64 / MB))
                .unwrap_or(0.0),
            gpu_pct: weighted(rows, |r| r.gpu_pct),
            gpu_3d_pct: weighted(rows, |r| r.gpu_3d_pct),
            gpu_dedicated_mb_mean: weighted(rows, |r| r.gpu_dedicated_bytes.map(|b| b as f64 / MB)),
            gpu_shared_mb_mean: weighted(rows, |r| r.gpu_shared_bytes.map(|b| b as f64 / MB)),
            dwm_cpu_ms_per_s: weighted(rows, |r| r.dwm_cpu_ms.map(|v| rate(v, r))),
            dwm_gpu_pct: weighted(rows, |r| r.dwm_gpu_pct),
            collector_cpu_ms_per_s: weighted(rows, |r| Some(rate(collector_ms(r), r)))
                .unwrap_or(0.0),
        })
    }
}

#[derive(Debug)]
pub enum MetricsError {
    Io(io::Error),
    Csv(csv::Error),
    Json(serde_json::Error),
    /// `samples.csv` has no rows.
    Empty,
}

impl std::fmt::Display for MetricsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Csv(e) => write!(f, "samples.csv: {e}"),
            Self::Json(e) => write!(f, "metrics.json: {e}"),
            Self::Empty => f.write_str("samples.csv has no samples"),
        }
    }
}

impl std::error::Error for MetricsError {}

#[derive(Debug, Default, Deserialize)]
struct Collected {
    #[serde(default)]
    machine: CollectedMachine,
}

#[derive(Debug, Default, Deserialize)]
struct CollectedMachine {
    #[serde(default)]
    logical_processors: u32,
    #[serde(default)]
    cycle_rate_mhz: Option<f64>,
}

/// The run's metrics: read from `metrics.json` when it exists, otherwise (or with `force`) computed from
/// `samples.csv` and written to `metrics.json`. The cycle counter rate and processor count come from the run's
/// `collect.json`.
pub fn load_or_compute(run_dir: &Path, force: bool) -> Result<RunMetrics, MetricsError> {
    let path = run_dir.join("metrics.json");
    if !force && let Ok(text) = fs::read_to_string(&path) {
        return serde_json::from_str(&text).map_err(MetricsError::Json);
    }
    let rows = read_samples(&run_dir.join("samples.csv")).map_err(MetricsError::Csv)?;
    let collected: Collected = match fs::read_to_string(run_dir.join("collect.json")) {
        Ok(text) => serde_json::from_str(&text).map_err(MetricsError::Json)?,
        Err(_) => Collected::default(),
    };
    let machine = collected.machine;
    // Without a processor count the percentage cannot be formed from cycles, so fall back to clock ticks.
    let clock = CpuClock {
        cycle_rate_mhz: machine
            .cycle_rate_mhz
            .filter(|_| machine.logical_processors > 0),
    };
    let metrics =
        RunMetrics::of(&rows, clock, machine.logical_processors).ok_or(MetricsError::Empty)?;
    let mut text = serde_json::to_string_pretty(&metrics).map_err(MetricsError::Json)?;
    text.push('\n');
    fs::write(&path, text).map_err(MetricsError::Io)?;
    Ok(metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "\
t_s,dt_s,processes,cpu_ms,cpu_mcycles,cpu_pct,private_bytes,working_set_bytes,gpu_pct,gpu_3d_pct,gpu_dedicated_bytes,gpu_shared_bytes,dwm_cpu_ms,dwm_gpu_pct,collector_cpu_ms
1.000,1.000,1,15.625,40.000,0.1953,10485760,20971520,1.000,1.000,1048576,,30.000,2.000,0.000
2.000,1.000,1,0.000,10.000,0.0000,10485760,20971520,3.000,2.000,1048576,,10.000,,15.625
3.000,1.000,1,15.625,12.500,0.1953,11534336,20971520,,,,,20.000,4.000,0.000
4.000,1.000,1,15.625,17.500,0.1953,11534336,20971520,2.000,0.000,1048576,,,,0.000
";

    fn rows() -> Vec<Row> {
        csv::Reader::from_reader(CSV.as_bytes())
            .deserialize()
            .collect::<Result<_, _>>()
            .expect("valid rows")
    }

    #[test]
    fn reads_rows_with_missing_sources() {
        let rows = rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].gpu_shared_bytes, None);
        assert_eq!(rows[1].dwm_gpu_pct, None);
        assert_eq!(rows[2].gpu_pct, None);
    }

    const TICKS: CpuClock = CpuClock {
        cycle_rate_mhz: None,
    };

    #[test]
    fn metrics_of_a_run() {
        let m = RunMetrics::of(&rows(), TICKS, 8).expect("rows");
        assert_eq!((m.samples, m.seconds, m.processes_max), (4, 4.0, 1));
        assert!((m.cpu_ms_per_s - 46.875 / 4.0).abs() < 1e-9);
        assert_eq!(m.cpu_tick_ms_per_s, Some(m.cpu_ms_per_s));
        assert_eq!(m.cycle_rate_mhz, None);
        assert!((m.cpu_mcycles_per_s - 20.0).abs() < 1e-9);
        // 46.875 ms over 80 Mcycles; the busiest row has 40 Mcycles.
        let peak = m.cpu_peak_ms_per_s.expect("cycles");
        assert!((peak - 40.0 * 46.875 / 80.0).abs() < 1e-9, "{peak}");
        assert_eq!(
            (m.private_mb_first, m.private_mb_last, m.private_mb_max),
            (10.0, 11.0, 11.0)
        );
        // Private bytes step up by 1 MB halfway through: a least squares slope of 0.4 MB/s.
        let growth = m.private_growth_mb_per_h.expect("slope");
        assert!((growth - 0.4 * 3600.0).abs() < 1e-6, "{growth}");
        assert_eq!(m.gpu_pct, Some(2.0), "mean of the rows that have it");
        assert_eq!(m.gpu_shared_mb_mean, None);
        assert_eq!(m.dwm_cpu_ms_per_s, Some(20.0));
        assert_eq!(m.dwm_gpu_pct, Some(3.0));
        assert!((m.collector_cpu_ms_per_s - 15.625 / 4.0).abs() < 1e-9);
        assert!(RunMetrics::of(&[], TICKS, 8).is_none());
    }

    #[test]
    fn cpu_time_from_cycles() {
        // 80 Mcycles over 4 s at 2000 MHz: 40 ms, 10 ms/s, 0.125% of 8 cores. The busiest row has 40 Mcycles.
        let clock = CpuClock {
            cycle_rate_mhz: Some(2000.0),
        };
        let m = RunMetrics::of(&rows(), clock, 8).expect("rows");
        assert!((m.cpu_ms_per_s - 10.0).abs() < 1e-9, "{m:?}");
        assert!((m.cpu_pct - 0.125).abs() < 1e-9, "{m:?}");
        assert_eq!(m.cpu_peak_ms_per_s, Some(20.0));
        assert!((m.cpu_tick_ms_per_s.unwrap() - 46.875 / 4.0).abs() < 1e-9);
        assert_eq!(m.cycle_rate_mhz, Some(2000.0));
        // Old samples have no collector cycles, so its clock-tick time stays.
        assert!((m.collector_cpu_ms_per_s - 15.625 / 4.0).abs() < 1e-9);

        let with_collector = format!(
            "{},collector_mcycles\n{}",
            CSV.lines().next().unwrap(),
            CSV.lines()
                .skip(1)
                .map(|l| format!("{l},5.000\n"))
                .collect::<String>()
        );
        let rows: Vec<Row> = csv::Reader::from_reader(with_collector.as_bytes())
            .deserialize()
            .collect::<Result<_, _>>()
            .expect("valid rows");
        let m = RunMetrics::of(&rows, clock, 8).expect("rows");
        assert!(
            (m.collector_cpu_ms_per_s - 2.5).abs() < 1e-9,
            "5 Mcycles per s at 2000 MHz"
        );
    }

    #[test]
    fn metrics_are_reused_until_forced() {
        let dir = std::env::temp_dir().join(format!("pitot-bench-metrics-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("samples.csv"), CSV).unwrap();
        fs::write(
            dir.join("collect.json"),
            r#"{"machine": {"logical_processors": 8, "cycle_rate_mhz": 2000.0}}"#,
        )
        .unwrap();
        let computed = load_or_compute(&dir, false).expect("computed");
        assert_eq!(computed.cycle_rate_mhz, Some(2000.0));
        assert!((computed.cpu_ms_per_s - 10.0).abs() < 1e-9);
        assert!(dir.join("metrics.json").is_file());
        let mut edited = computed.clone();
        edited.samples = 99;
        fs::write(
            dir.join("metrics.json"),
            serde_json::to_string(&edited).unwrap(),
        )
        .unwrap();
        assert_eq!(load_or_compute(&dir, false).expect("reused").samples, 99);
        assert_eq!(load_or_compute(&dir, true).expect("forced").samples, 4);
        let _ = fs::remove_dir_all(&dir);
    }
}
