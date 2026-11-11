//! Collector: samples an overlay's whole process tree once per interval and writes the raw samples.
//!
//! CPU time, cycles, and memory come from process handles keyed by PID, so processes with the same name never
//! mix. CPU time is taken from cycles at the cycle counter's measured rate; the OS's clock-tick count is kept
//! alongside. GPU engines, GPU memory, and DWM come from PDH with English counter paths, so localized Windows works and
//! DWM needs no elevation. Output: `samples.csv` and `collect.json`. See `docs/collector.md`.

pub mod gpu;
#[cfg(windows)]
mod sampler;
pub mod tree;

use std::fmt;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};

#[cfg(windows)]
pub use sampler::Collector;
pub use tree::{ProcessEntry, Target};

/// One interval of measurement.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sample {
    /// Seconds since the baseline sample.
    pub t: f64,
    /// Seconds this sample covers.
    pub dt: f64,
    /// Live processes in the tree.
    pub processes: u32,
    /// CPU time of the tree in this interval as the OS counts it, per clock tick (15.6 ms by default).
    pub cpu_ms: f64,
    /// CPU cycles of the tree in this interval, in millions; precise even for short intervals.
    pub cpu_mcycles: f64,
    /// `cpu_ms` as a percentage of all logical processors.
    pub cpu_pct: f64,
    pub private_bytes: u64,
    pub working_set_bytes: u64,
    /// Busiest GPU engine type, in percent.
    pub gpu_pct: Option<f64>,
    pub gpu_3d_pct: Option<f64>,
    /// Dedicated (video) memory and shared system memory the GPU holds for the tree.
    pub gpu_dedicated_bytes: Option<u64>,
    pub gpu_shared_bytes: Option<u64>,
    pub dwm_cpu_ms: Option<f64>,
    pub dwm_gpu_pct: Option<f64>,
    /// CPU time of the collector itself in this interval, per clock tick.
    pub collector_cpu_ms: f64,
    /// CPU cycles of the collector itself in this interval, in millions.
    pub collector_mcycles: f64,
}

pub const CSV_HEADER: &str = "t_s,dt_s,processes,cpu_ms,cpu_mcycles,cpu_pct,private_bytes,working_set_bytes,\
gpu_pct,gpu_3d_pct,gpu_dedicated_bytes,gpu_shared_bytes,dwm_cpu_ms,dwm_gpu_pct,collector_cpu_ms,collector_mcycles";

struct Opt<T>(Option<T>);

impl fmt::Display for Opt<f64> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{v:.3}"),
            None => Ok(()),
        }
    }
}

impl fmt::Display for Opt<u64> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{v}"),
            None => Ok(()),
        }
    }
}

impl Sample {
    /// One CSV row matching [`CSV_HEADER`]; missing values are empty fields.
    pub fn write_csv(&self, out: &mut impl Write) -> io::Result<()> {
        writeln!(
            out,
            "{:.3},{:.3},{},{:.3},{:.3},{:.4},{},{},{},{},{},{},{},{},{:.3},{:.3}",
            self.t,
            self.dt,
            self.processes,
            self.cpu_ms,
            self.cpu_mcycles,
            self.cpu_pct,
            self.private_bytes,
            self.working_set_bytes,
            Opt(self.gpu_pct),
            Opt(self.gpu_3d_pct),
            Opt(self.gpu_dedicated_bytes),
            Opt(self.gpu_shared_bytes),
            Opt(self.dwm_cpu_ms),
            Opt(self.dwm_gpu_pct),
            self.collector_cpu_ms,
            self.collector_mcycles,
        )
    }
}

/// Turns cycles into CPU time. With the cycle counter's rate, time is cycles over the rate; without it (the rate
/// could not be measured), time is the OS's clock-tick count.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuClock {
    /// Cycle counter rate in MHz (millions of cycles per second).
    pub cycle_rate_mhz: Option<f64>,
}

impl CpuClock {
    /// CPU time in ms from a count in millions of cycles and the clock-tick time for the same stretch.
    pub fn ms(self, mcycles: f64, tick_ms: f64) -> f64 {
        match self.cycle_rate_mhz {
            Some(rate) if rate > 0.0 => mcycles / rate * 1000.0,
            _ => tick_ms,
        }
    }
}

/// Time-weighted means and extremes of a run. CPU time comes from cycles when `cycle_rate_mhz` is set, otherwise
/// from clock ticks.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Summary {
    pub samples: usize,
    pub seconds: f64,
    pub processes_max: u32,
    /// The cycle counter rate CPU time was computed with; missing in summaries from before it was measured.
    #[serde(default)]
    pub cycle_rate_mhz: Option<f64>,
    pub cpu_pct_mean: f64,
    pub cpu_ms_per_s_mean: f64,
    pub cpu_ms_per_s_max: f64,
    pub private_bytes_first: u64,
    pub private_bytes_last: u64,
    pub private_bytes_max: u64,
    pub gpu_pct_mean: Option<f64>,
    pub gpu_3d_pct_mean: Option<f64>,
    pub dwm_cpu_ms_per_s_mean: Option<f64>,
    pub dwm_gpu_pct_mean: Option<f64>,
    pub collector_cpu_ms_per_s_mean: f64,
}

/// Mean of `value` weighted by each sample's duration, over the samples that have it.
fn weighted(samples: &[Sample], value: impl Fn(&Sample) -> Option<f64>) -> Option<f64> {
    let (sum, time) = samples
        .iter()
        .filter_map(|s| value(s).map(|v| (v * s.dt, s.dt)))
        .fold((0.0, 0.0), |(a, b), (v, t)| (a + v, b + t));
    (time > 0.0).then(|| sum / time)
}

impl Summary {
    /// `cores` is the number of logical processors, for the percentage of all cores.
    pub fn of(samples: &[Sample], clock: CpuClock, cores: u32) -> Self {
        let rate = |v: Option<f64>, s: &Sample| v.map(|v| v / s.dt);
        let cpu_ms = |s: &Sample| clock.ms(s.cpu_mcycles, s.cpu_ms);
        let cpu_ms_per_s_mean = weighted(samples, |s| rate(Some(cpu_ms(s)), s)).unwrap_or(0.0);
        Self {
            samples: samples.len(),
            seconds: samples.iter().map(|s| s.dt).sum(),
            processes_max: samples.iter().map(|s| s.processes).max().unwrap_or(0),
            cycle_rate_mhz: clock.cycle_rate_mhz,
            cpu_pct_mean: cpu_ms_per_s_mean / (10.0 * f64::from(cores.max(1))),
            cpu_ms_per_s_mean,
            cpu_ms_per_s_max: samples.iter().map(|s| cpu_ms(s) / s.dt).fold(0.0, f64::max),
            private_bytes_first: samples.first().map_or(0, |s| s.private_bytes),
            private_bytes_last: samples.last().map_or(0, |s| s.private_bytes),
            private_bytes_max: samples.iter().map(|s| s.private_bytes).max().unwrap_or(0),
            gpu_pct_mean: weighted(samples, |s| s.gpu_pct),
            gpu_3d_pct_mean: weighted(samples, |s| s.gpu_3d_pct),
            dwm_cpu_ms_per_s_mean: weighted(samples, |s| rate(s.dwm_cpu_ms, s)),
            dwm_gpu_pct_mean: weighted(samples, |s| s.dwm_gpu_pct),
            collector_cpu_ms_per_s_mean: weighted(samples, |s| {
                rate(Some(clock.ms(s.collector_mcycles, s.collector_cpu_ms)), s)
            })
            .unwrap_or(0.0),
        }
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MB: f64 = 1024.0 * 1024.0;
        writeln!(
            f,
            "{} samples over {:.1} s, up to {} processes",
            self.samples, self.seconds, self.processes_max
        )?;
        writeln!(
            f,
            "  CPU      {:.3}% of all cores, {:.2} ms/s mean, {:.1} ms/s max",
            self.cpu_pct_mean, self.cpu_ms_per_s_mean, self.cpu_ms_per_s_max
        )?;
        writeln!(
            f,
            "  memory   private {:.1} -> {:.1} MB (max {:.1} MB)",
            self.private_bytes_first as f64 / MB,
            self.private_bytes_last as f64 / MB,
            self.private_bytes_max as f64 / MB
        )?;
        match (self.gpu_pct_mean, self.gpu_3d_pct_mean) {
            (Some(gpu), Some(d3)) => {
                writeln!(f, "  GPU      {gpu:.2}% busiest engine, {d3:.2}% 3D")?
            }
            _ => writeln!(f, "  GPU      not available")?,
        }
        match (self.dwm_cpu_ms_per_s_mean, self.dwm_gpu_pct_mean) {
            (Some(cpu), Some(gpu)) => writeln!(f, "  DWM      {cpu:.2} ms/s CPU, {gpu:.2}% GPU")?,
            (Some(cpu), None) => writeln!(f, "  DWM      {cpu:.2} ms/s CPU")?,
            _ => writeln!(f, "  DWM      not available")?,
        }
        write!(
            f,
            "  collector {:.2} ms/s CPU",
            self.collector_cpu_ms_per_s_mean
        )?;
        match self.cycle_rate_mhz {
            Some(rate) => write!(f, "\n  CPU time from cycles at {rate:.0} MHz"),
            None => write!(f, "\n  CPU time from clock ticks (cycle rate not measured)"),
        }
    }
}

#[cfg(windows)]
pub use run::{CollectError, Options, run};

#[cfg(windows)]
mod run {
    use std::fs::{self, File};
    use std::io::{self, BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use serde::Serialize;

    use super::{CSV_HEADER, Collector, CpuClock, Summary, Target};
    use crate::sys::{self, SysError};

    #[derive(Debug)]
    pub enum CollectError {
        Io(io::Error),
        Json(serde_json::Error),
        Sys(SysError),
    }

    impl std::fmt::Display for CollectError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Io(e) => write!(f, "{e}"),
                Self::Json(e) => write!(f, "{e}"),
                Self::Sys(e) => write!(f, "{e}"),
            }
        }
    }

    impl std::error::Error for CollectError {}

    impl From<io::Error> for CollectError {
        fn from(e: io::Error) -> Self {
            Self::Io(e)
        }
    }

    impl From<serde_json::Error> for CollectError {
        fn from(e: serde_json::Error) -> Self {
            Self::Json(e)
        }
    }

    impl From<SysError> for CollectError {
        fn from(e: SysError) -> Self {
            Self::Sys(e)
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct Options {
        pub interval: Duration,
        pub duration: Duration,
        pub out_dir: PathBuf,
    }

    #[derive(Debug, Serialize)]
    struct Machine {
        cpu: Option<String>,
        logical_processors: u32,
        /// Rate of the CPU cycle counter, measured at the start of the run.
        cycle_rate_mhz: Option<f64>,
        os_product: Option<String>,
        os_display_version: Option<String>,
        os_build: Option<String>,
        os_ubr: Option<u32>,
    }

    impl Machine {
        fn read(logical_processors: u32, cycle_rate_mhz: Option<f64>) -> Self {
            const CPU: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
            const OS: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
            Self {
                cpu: sys::registry_string(CPU, "ProcessorNameString"),
                logical_processors,
                cycle_rate_mhz,
                os_product: sys::registry_string(OS, "ProductName"),
                os_display_version: sys::registry_string(OS, "DisplayVersion"),
                os_build: sys::registry_string(OS, "CurrentBuild"),
                os_ubr: sys::registry_dword(OS, "UBR"),
            }
        }
    }

    #[derive(Debug, Serialize)]
    struct Sources {
        gpu: bool,
        dwm: bool,
    }

    /// `collect.json`: what was measured, where, and how; written at the start and completed at the end.
    #[derive(Debug, Serialize)]
    struct RunInfo {
        tool: &'static str,
        version: &'static str,
        target: Target,
        started_local: String,
        interval_ms: u128,
        requested_seconds: f64,
        machine: Machine,
        sources: Sources,
        unreadable_processes: Vec<String>,
        summary: Option<Summary>,
    }

    fn write_json(path: &Path, info: &RunInfo) -> Result<(), CollectError> {
        let mut file = BufWriter::new(File::create(path)?);
        serde_json::to_writer_pretty(&mut file, info)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    /// Collects `target` for `options.duration` and writes `samples.csv` and `collect.json` to `options.out_dir`.
    /// `on_sample` sees each sample as it is written.
    pub fn run(
        target: Target,
        options: &Options,
        mut on_sample: impl FnMut(&super::Sample),
    ) -> Result<Summary, CollectError> {
        fs::create_dir_all(&options.out_dir)?;
        let mut collector = Collector::new(target.clone());
        let mut info = RunInfo {
            tool: "pitot-bench",
            version: env!("CARGO_PKG_VERSION"),
            target,
            started_local: crate::paths::local_timestamp(),
            interval_ms: options.interval.as_millis(),
            requested_seconds: options.duration.as_secs_f64(),
            machine: Machine::read(collector.logical_processors(), collector.cycle_rate_mhz()),
            sources: Sources {
                gpu: collector.gpu_available(),
                dwm: collector.dwm_available(),
            },
            unreadable_processes: Vec::new(),
            summary: None,
        };
        let info_path = options.out_dir.join("collect.json");
        write_json(&info_path, &info)?;

        let mut csv = BufWriter::new(File::create(options.out_dir.join("samples.csv"))?);
        writeln!(csv, "{CSV_HEADER}")?;
        let count = (options.duration.as_secs_f64() / options.interval.as_secs_f64())
            .round()
            .max(1.0) as u64;
        let mut samples = Vec::with_capacity(count as usize);
        let start = Instant::now();
        collector.sample()?;
        for n in 1..=count {
            let due = start + options.interval * n as u32;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            if let Some(sample) = collector.sample()? {
                sample.write_csv(&mut csv)?;
                // Flushed per row so an interrupted long run keeps its samples.
                csv.flush()?;
                on_sample(&sample);
                samples.push(sample);
            }
        }

        let clock = CpuClock {
            cycle_rate_mhz: collector.cycle_rate_mhz(),
        };
        let summary = Summary::of(&samples, clock, collector.logical_processors());
        info.unreadable_processes = collector.unreadable().map(str::to_owned).collect();
        info.summary = Some(summary.clone());
        write_json(&info_path, &info)?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t: f64, cpu_ms: f64, private: u64, gpu: Option<f64>) -> Sample {
        Sample {
            t,
            dt: 1.0,
            processes: 2,
            cpu_ms,
            cpu_mcycles: cpu_ms * 3.0,
            cpu_pct: cpu_ms / 80.0,
            private_bytes: private,
            working_set_bytes: private * 2,
            gpu_pct: gpu,
            gpu_3d_pct: gpu,
            gpu_dedicated_bytes: None,
            gpu_shared_bytes: Some(4096),
            dwm_cpu_ms: Some(10.0),
            dwm_gpu_pct: None,
            collector_cpu_ms: 0.5,
            collector_mcycles: 1.25,
        }
    }

    #[test]
    fn csv_row_matches_the_header() {
        let mut out = Vec::new();
        sample(1.0, 15.625, 1000, None).write_csv(&mut out).unwrap();
        let row = String::from_utf8(out).unwrap();
        assert_eq!(
            row,
            "1.000,1.000,2,15.625,46.875,0.1953,1000,2000,,,,4096,10.000,,0.500,1.250\n"
        );
        assert_eq!(
            row.trim_end().split(',').count(),
            CSV_HEADER.split(',').count()
        );
    }

    #[test]
    fn summary_is_time_weighted() {
        let mut samples = vec![
            sample(1.0, 10.0, 100, Some(1.0)),
            sample(2.0, 30.0, 300, None),
        ];
        samples[1].dt = 3.0;
        let ticks = CpuClock {
            cycle_rate_mhz: None,
        };
        let s = Summary::of(&samples, ticks, 8);
        assert_eq!(s.samples, 2);
        assert_eq!(s.seconds, 4.0);
        assert_eq!(s.cpu_ms_per_s_mean, 10.0, "40 ms over 4 s");
        assert_eq!(s.cpu_ms_per_s_max, 10.0);
        assert_eq!(s.cpu_pct_mean, 10.0 / 80.0, "10 ms/s of 8 cores");
        assert_eq!(s.cycle_rate_mhz, None);
        assert_eq!((s.private_bytes_first, s.private_bytes_last), (100, 300));
        assert_eq!(s.gpu_pct_mean, Some(1.0), "only samples with GPU data");
        assert_eq!(s.dwm_gpu_pct_mean, None);
        let dwm = s.dwm_cpu_ms_per_s_mean.unwrap_or(0.0);
        assert!((dwm - 5.0).abs() < 1e-9, "20 ms over 4 s: {dwm}");

        // 30 and 90 Mcycles at 1500 MHz are 20 and 60 ms; the collector's 1.25 Mcycles per row are 0.833 ms.
        let cycles = CpuClock {
            cycle_rate_mhz: Some(1500.0),
        };
        let s = Summary::of(&samples, cycles, 8);
        assert!((s.cpu_ms_per_s_mean - 20.0).abs() < 1e-9, "80 ms over 4 s");
        assert!((s.cpu_ms_per_s_max - 20.0).abs() < 1e-9);
        assert!((s.cpu_pct_mean - 0.25).abs() < 1e-9);
        assert!((s.collector_cpu_ms_per_s_mean - 2.5 / 1.5 / 4.0).abs() < 1e-9);
        assert_eq!(s.cycle_rate_mhz, Some(1500.0));
    }
}
