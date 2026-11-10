//! Report stage: processed metrics per run (`metrics.json`) and a report per series (`report-<id>.md` and
//! `.json`): means over runs with confidence intervals, differences against the interleaved baselines, and the
//! plan's budget. Platform independent. See `docs/report.md`.

pub mod metrics;
mod render;
pub mod stats;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use metrics::{MetricsError, RunMetrics};
pub use stats::Estimate;

use crate::paths;
use crate::runner::{BASELINE, Budget, Outcome, SeriesRecord};

/// How a metric relates to the app: its own process tree, or the whole desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Measured on the app's process tree; zero without an app.
    App,
    /// Desktop-wide (DWM); compared against the baselines.
    Desktop,
}

/// A metric the report shows, read from each run's metrics.
pub struct Metric {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub scope: Scope,
    pub value: fn(&RunMetrics) -> Option<f64>,
}

pub const METRICS: &[Metric] = &[
    Metric {
        key: "cpu_pct",
        label: "CPU, all cores",
        unit: "%",
        scope: Scope::App,
        value: |m| Some(m.cpu_pct),
    },
    Metric {
        key: "cpu_ms_per_s",
        label: "CPU time",
        unit: "ms/s",
        scope: Scope::App,
        value: |m| Some(m.cpu_ms_per_s),
    },
    Metric {
        key: "cpu_peak_ms_per_s",
        label: "CPU, busiest sample",
        unit: "ms/s",
        scope: Scope::App,
        value: |m| m.cpu_peak_ms_per_s,
    },
    Metric {
        key: "cpu_mcycles_per_s",
        label: "CPU cycles",
        unit: "Mcycles/s",
        scope: Scope::App,
        value: |m| Some(m.cpu_mcycles_per_s),
    },
    Metric {
        key: "private_mb_max",
        label: "Private bytes, highest",
        unit: "MB",
        scope: Scope::App,
        value: |m| Some(m.private_mb_max),
    },
    Metric {
        key: "private_mb_mean",
        label: "Private bytes, mean",
        unit: "MB",
        scope: Scope::App,
        value: |m| Some(m.private_mb_mean),
    },
    Metric {
        key: "private_growth_mb_per_h",
        label: "Private bytes, growth",
        unit: "MB/h",
        scope: Scope::App,
        value: |m| m.private_growth_mb_per_h,
    },
    Metric {
        key: "working_set_mb_mean",
        label: "Working set, mean",
        unit: "MB",
        scope: Scope::App,
        value: |m| Some(m.working_set_mb_mean),
    },
    Metric {
        key: "gpu_pct",
        label: "GPU, busiest engine",
        unit: "%",
        scope: Scope::App,
        value: |m| m.gpu_pct,
    },
    Metric {
        key: "gpu_dedicated_mb_mean",
        label: "GPU dedicated memory",
        unit: "MB",
        scope: Scope::App,
        value: |m| m.gpu_dedicated_mb_mean,
    },
    Metric {
        key: "gpu_shared_mb_mean",
        label: "GPU shared memory",
        unit: "MB",
        scope: Scope::App,
        value: |m| m.gpu_shared_mb_mean,
    },
    Metric {
        key: "dwm_cpu_ms_per_s",
        label: "DWM CPU time",
        unit: "ms/s",
        scope: Scope::Desktop,
        value: |m| m.dwm_cpu_ms_per_s,
    },
    Metric {
        key: "dwm_gpu_pct",
        label: "DWM GPU, busiest engine",
        unit: "%",
        scope: Scope::Desktop,
        value: |m| m.dwm_gpu_pct,
    },
    Metric {
        key: "collector_cpu_ms_per_s",
        label: "Collector CPU time",
        unit: "ms/s",
        scope: Scope::Desktop,
        value: |m| Some(m.collector_cpu_ms_per_s),
    },
];

/// The machine the series ran on, from the first measured run's `collect.json`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Machine {
    pub cpu: Option<String>,
    pub logical_processors: u32,
    pub os_product: Option<String>,
    pub os_display_version: Option<String>,
    pub os_build: Option<String>,
    pub os_ubr: Option<u32>,
}

impl Machine {
    /// The OS name. The registry still says "Windows 10" on Windows 11, which starts at build 22000.
    pub fn os(&self) -> String {
        let build: Option<u32> = self.os_build.as_deref().and_then(|b| b.parse().ok());
        let product = self
            .os_product
            .clone()
            .unwrap_or_else(|| "Windows".to_owned());
        let product = match build {
            Some(b) if b >= 22000 && product.starts_with("Windows 10") => {
                product.replacen("Windows 10", "Windows 11", 1)
            }
            _ => product,
        };
        let mut text = product;
        if let Some(version) = &self.os_display_version {
            text.push_str(&format!(" {version}"));
        }
        if let Some(build) = &self.os_build {
            text.push_str(&format!(", build {build}"));
            if let Some(ubr) = self.os_ubr {
                text.push_str(&format!(".{ubr}"));
            }
        }
        text
    }
}

#[derive(Debug, Deserialize)]
struct CollectInfo {
    machine: Machine,
}

/// Per-app results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppResult {
    pub app: String,
    pub runs: usize,
    pub measured: usize,
    /// Mean over measured runs, by metric key.
    pub metrics: BTreeMap<String, Estimate>,
    /// Desktop-wide metrics minus the baselines, by metric key; empty for the baseline itself.
    pub vs_baseline: BTreeMap<String, Estimate>,
}

impl AppResult {
    pub fn metric(&self, key: &str) -> Option<&Estimate> {
        self.metrics.get(key)
    }

    pub fn difference(&self, key: &str) -> Option<&Estimate> {
        self.vs_baseline.get(key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Within,
    Over,
    /// The confidence interval contains the limit.
    Inconclusive,
    /// No data for this item.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetCheck {
    pub app: String,
    pub item: String,
    pub limit: String,
    pub value: String,
    pub verdict: Verdict,
}

/// A run and what happened to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunLine {
    pub position: u32,
    pub app: String,
    pub status: String,
}

/// `report-<series-id>.json`; the Markdown report shows the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub tool: String,
    pub version: String,
    pub scenario: String,
    pub series: SeriesRecord,
    pub machine: Machine,
    pub runs: Vec<RunLine>,
    /// The baseline first, then the apps in plan order.
    pub results: Vec<AppResult>,
    pub budget: Vec<BudgetCheck>,
}

#[derive(Debug)]
pub enum ReportError {
    Io(io::Error),
    Json(serde_json::Error),
    Metrics {
        dir: String,
        error: MetricsError,
    },
    /// The series has no measured run.
    NothingMeasured,
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Json(e) => write!(f, "{e}"),
            Self::Metrics { dir, error } => write!(f, "{dir}: {error}"),
            Self::NothingMeasured => f.write_str("the series has no measured run"),
        }
    }
}

impl std::error::Error for ReportError {}

impl From<io::Error> for ReportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for ReportError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// Mean over runs that have a value.
fn estimate(runs: &[&RunMetrics], value: fn(&RunMetrics) -> Option<f64>) -> Option<Estimate> {
    let values: Vec<f64> = runs.iter().filter_map(|m| value(m)).collect();
    Estimate::of(&values)
}

fn values(runs: &[&RunMetrics], value: fn(&RunMetrics) -> Option<f64>) -> Vec<f64> {
    runs.iter().filter_map(|m| value(m)).collect()
}

/// Builds the report from a series and its runs' metrics, in the series' order.
pub fn build(series: SeriesRecord, machine: Machine, measured: &[(String, RunMetrics)]) -> Report {
    let mut apps = vec![BASELINE.to_owned()];
    apps.extend(series.plan.apps.iter().map(|a| a.name.clone()));
    let of = |app: &str| -> Vec<&RunMetrics> {
        measured
            .iter()
            .filter(|(a, _)| a == app)
            .map(|(_, m)| m)
            .collect()
    };
    let baseline = of(BASELINE);
    let results: Vec<AppResult> = apps
        .iter()
        .map(|app| {
            let runs = of(app);
            let metrics = METRICS
                .iter()
                .filter_map(|m| Some((m.key.to_owned(), estimate(&runs, m.value)?)))
                .collect();
            let vs_baseline = if app == BASELINE {
                BTreeMap::new()
            } else {
                METRICS
                    .iter()
                    .filter(|m| m.scope == Scope::Desktop)
                    .filter_map(|m| {
                        let d = Estimate::difference(
                            &values(&runs, m.value),
                            &values(&baseline, m.value),
                        )?;
                        Some((m.key.to_owned(), d))
                    })
                    .collect()
            };
            AppResult {
                app: app.clone(),
                runs: series.runs.iter().filter(|r| &r.app == app).count(),
                measured: runs.len(),
                metrics,
                vs_baseline,
            }
        })
        .collect();
    let budget = series
        .plan
        .budget
        .as_ref()
        .map(|budget| check_budget(budget, &results, measured))
        .unwrap_or_default();
    let runs = series
        .runs
        .iter()
        .map(|r| RunLine {
            position: r.position,
            app: r.app.clone(),
            status: match &r.outcome {
                Some(Outcome::Measured { .. }) => "measured".to_owned(),
                Some(Outcome::NotRunning) => "not running after warm-up".to_owned(),
                Some(Outcome::LaunchFailed { error }) => format!("launch failed: {error}"),
                None => "not run".to_owned(),
            },
        })
        .collect();
    Report {
        tool: "pitot-bench".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        scenario: series.plan.scenario.clone(),
        machine,
        runs,
        results,
        budget,
        series,
    }
}

/// A mean judged against an upper limit by its interval: within if the whole interval is at or below it, over if
/// the whole interval is above it.
fn judge_mean(estimate: Option<&Estimate>, limit: f64) -> Verdict {
    match estimate {
        None => Verdict::Missing,
        Some(e) => match (e.ci_low, e.ci_high) {
            (Some(_), Some(high)) if high <= limit => Verdict::Within,
            (Some(low), Some(_)) if low > limit => Verdict::Over,
            (Some(_), Some(_)) => Verdict::Inconclusive,
            _ if e.mean <= limit => Verdict::Within,
            _ => Verdict::Over,
        },
    }
}

fn format_estimate(e: Option<&Estimate>, digits: usize) -> String {
    match e {
        None => "n/a".to_owned(),
        Some(e) => match (e.ci_low, e.ci_high) {
            (Some(low), Some(high)) => {
                format!(
                    "{:.digits$} ({:.digits$} to {:.digits$})",
                    e.mean, low, high
                )
            }
            _ => format!("{:.digits$} (1 run)", e.mean),
        },
    }
}

fn check_budget(
    budget: &Budget,
    results: &[AppResult],
    measured: &[(String, RunMetrics)],
) -> Vec<BudgetCheck> {
    let mut checks = Vec::new();
    for result in results.iter().filter(|r| r.app != BASELINE) {
        let mut push = |item: &str, limit: String, value: String, verdict: Verdict| {
            checks.push(BudgetCheck {
                app: result.app.clone(),
                item: item.to_owned(),
                limit,
                value,
                verdict,
            });
        };
        // The worst run counts for limits on peaks and highs.
        let worst = |value: fn(&RunMetrics) -> Option<f64>| {
            measured
                .iter()
                .filter(|(a, _)| *a == result.app)
                .filter_map(|(_, m)| value(m))
                .reduce(f64::max)
        };
        let max_check = |worst: Option<f64>, limit: f64, digits: usize| match worst {
            None => ("n/a".to_owned(), Verdict::Missing),
            Some(v) => (
                format!("{v:.digits$} (worst run)"),
                if v <= limit {
                    Verdict::Within
                } else {
                    Verdict::Over
                },
            ),
        };
        if let Some(limit) = budget.cpu_pct {
            let e = result.metric("cpu_pct");
            push(
                "CPU, all cores, mean",
                format!("{limit} %"),
                format_estimate(e, 3),
                judge_mean(e, limit),
            );
        }
        if let Some(limit) = budget.cpu_peak_ms_per_s {
            let (value, verdict) = max_check(worst(|m| m.cpu_peak_ms_per_s), limit, 1);
            push(
                "CPU, busiest sample",
                format!("{limit} ms/s"),
                value,
                verdict,
            );
        }
        if let Some(limit) = budget.private_mb {
            let (value, verdict) = max_check(worst(|m| Some(m.private_mb_max)), limit, 1);
            push(
                "Private bytes, highest",
                format!("{limit} MB"),
                value,
                verdict,
            );
        }
        if let Some(limit) = budget.private_growth_mb_per_h {
            let e = result.metric("private_growth_mb_per_h");
            push(
                "Private bytes, growth",
                format!("{limit} MB/h"),
                format_estimate(e, 2),
                judge_mean(e, limit),
            );
        }
        if budget.dwm_no_increase {
            for (key, item) in [
                ("dwm_cpu_ms_per_s", "DWM CPU over baseline"),
                ("dwm_gpu_pct", "DWM GPU over baseline"),
            ] {
                let d = result.difference(key);
                // No measurable increase: the interval of the difference reaches zero or below.
                let verdict = match d.and_then(|d| d.ci_low) {
                    None => Verdict::Missing,
                    Some(low) if low > 0.0 => Verdict::Over,
                    Some(_) => Verdict::Within,
                };
                push(
                    item,
                    "no increase".to_owned(),
                    format_estimate(d, 2),
                    verdict,
                );
            }
        }
    }
    checks
}

/// Loads a series file and its runs' metrics (computing them when missing or with `force`), then writes
/// `report-<series-id>.md` and `.json` under the scenario. Returns the Markdown path.
pub fn generate(series_file: &Path, force: bool) -> Result<PathBuf, ReportError> {
    let series: SeriesRecord = serde_json::from_str(&fs::read_to_string(series_file)?)?;
    let mut measured = Vec::new();
    let mut machine = None;
    for run in &series.runs {
        if !matches!(run.outcome, Some(Outcome::Measured { .. })) {
            continue;
        }
        let dir = Path::new(&run.dir);
        let metrics =
            metrics::load_or_compute(dir, force).map_err(|error| ReportError::Metrics {
                dir: run.dir.clone(),
                error,
            })?;
        if machine.is_none() {
            let info: CollectInfo =
                serde_json::from_str(&fs::read_to_string(dir.join("collect.json"))?)?;
            machine = Some(info.machine);
        }
        measured.push((run.app.clone(), metrics));
    }
    let machine = machine.ok_or(ReportError::NothingMeasured)?;
    let report = build(series, machine, &measured);
    let (scenario, id) = (report.scenario.clone(), report.series.series_id.clone());
    let json_path = paths::report_file(&scenario, &id, "json");
    let mut json = serde_json::to_string_pretty(&report)?;
    json.push('\n');
    fs::write(&json_path, json)?;
    let md_path = paths::report_file(&scenario, &id, "md");
    fs::write(&md_path, render::markdown(&report))?;
    Ok(md_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Plan, RunRecord};

    fn run_metrics(cpu_pct: f64, private: f64, dwm: f64) -> RunMetrics {
        RunMetrics {
            samples: 180,
            seconds: 180.0,
            processes_max: 1,
            cpu_pct,
            cpu_ms_per_s: cpu_pct * 80.0,
            cpu_mcycles_per_s: cpu_pct * 300.0,
            cpu_peak_ms_per_s: Some(cpu_pct * 200.0),
            private_mb_first: private,
            private_mb_last: private,
            private_mb_max: private,
            private_mb_mean: private,
            private_growth_mb_per_h: Some(0.0),
            working_set_mb_mean: private * 2.0,
            gpu_pct: Some(1.0),
            gpu_3d_pct: Some(1.0),
            gpu_dedicated_mb_mean: None,
            gpu_shared_mb_mean: Some(2.0),
            dwm_cpu_ms_per_s: Some(dwm),
            dwm_gpu_pct: Some(dwm / 10.0),
            collector_cpu_ms_per_s: 3.0,
        }
    }

    pub(super) fn sample_report() -> Report {
        let plan = Plan::parse(
            r#"
scenario = "unit"
repeats = 3
[budget]
cpu_pct = 0.2
cpu_peak_ms_per_s = 10
private_mb = 30
dwm_no_increase = true
[[app]]
name = "light"
command = "light.exe"
[[app]]
name = "heavy"
command = "heavy.exe"
"#,
        )
        .expect("plan");
        let slots = crate::runner::plan::order(&["light", "heavy"], 3, 1);
        let runs = slots
            .iter()
            .map(|s| RunRecord {
                position: s.position,
                app: s.app.clone(),
                repeat: s.repeat,
                dir: format!("bench-results/unit/{}/{}", s.app, s.position),
                outcome: Some(if s.app == "heavy" && s.repeat == 2 {
                    Outcome::NotRunning
                } else {
                    Outcome::Measured {
                        summary: Default::default(),
                    }
                }),
            })
            .collect();
        let series = SeriesRecord {
            tool: "pitot-bench".into(),
            version: "0".into(),
            series_id: "20261111-000000".into(),
            plan_file: "plan.toml".into(),
            started_local: "2026-11-11T00:00:00".into(),
            finished_local: None,
            seed: 1,
            detected: Vec::new(),
            plan,
            runs,
        };
        let measured: Vec<(String, RunMetrics)> = [
            ("baseline", 0.0, 0.0, 10.0),
            ("baseline", 0.0, 0.0, 11.0),
            ("baseline", 0.0, 0.0, 10.5),
            ("baseline", 0.0, 0.0, 10.2),
            ("light", 0.020, 14.0, 10.4),
            ("light", 0.022, 14.2, 10.8),
            ("light", 0.021, 14.1, 10.1),
            ("heavy", 1.50, 120.0, 30.0),
            ("heavy", 1.70, 125.0, 31.0),
        ]
        .iter()
        .map(|&(app, cpu, private, dwm)| (app.to_owned(), run_metrics(cpu, private, dwm)))
        .collect();
        let machine = Machine {
            cpu: Some("Test CPU".into()),
            logical_processors: 8,
            os_product: Some("Windows 10 Pro".into()),
            os_display_version: Some("25H2".into()),
            os_build: Some("26300".into()),
            os_ubr: Some(1),
        };
        build(series, machine, &measured)
    }

    #[test]
    fn results_per_app_with_baseline_differences() {
        let report = sample_report();
        let apps: Vec<&str> = report.results.iter().map(|r| r.app.as_str()).collect();
        assert_eq!(apps, [BASELINE, "light", "heavy"]);
        let heavy = &report.results[2];
        assert_eq!((heavy.runs, heavy.measured), (3, 2));
        let cpu = heavy.metric("cpu_pct").expect("cpu");
        assert!((cpu.mean - 1.6).abs() < 1e-9);
        let dwm = heavy.difference("dwm_cpu_ms_per_s").expect("difference");
        assert!((dwm.mean - (30.5 - 10.425)).abs() < 1e-9, "{dwm:?}");
        assert!(report.results[0].vs_baseline.is_empty());
        assert_eq!(report.runs.len(), 10);
        assert!(
            report
                .runs
                .iter()
                .any(|r| r.status == "not running after warm-up")
        );
        assert_eq!(report.machine.os(), "Windows 11 Pro 25H2, build 26300.1");
    }

    #[test]
    fn budget_verdicts() {
        let report = sample_report();
        let verdict = |app: &str, item: &str| {
            report
                .budget
                .iter()
                .find(|c| c.app == app && c.item == item)
                .map(|c| c.verdict)
        };
        assert_eq!(
            verdict("light", "CPU, all cores, mean"),
            Some(Verdict::Within)
        );
        assert_eq!(
            verdict("heavy", "CPU, all cores, mean"),
            Some(Verdict::Over)
        );
        assert_eq!(
            verdict("light", "CPU, busiest sample"),
            Some(Verdict::Within)
        );
        assert_eq!(verdict("heavy", "CPU, busiest sample"), Some(Verdict::Over));
        assert_eq!(
            verdict("heavy", "Private bytes, highest"),
            Some(Verdict::Over)
        );
        assert_eq!(
            verdict("light", "DWM CPU over baseline"),
            Some(Verdict::Within)
        );
        assert_eq!(
            verdict("heavy", "DWM CPU over baseline"),
            Some(Verdict::Over)
        );
        assert_eq!(report.budget.len(), 2 * 5);
    }

    #[test]
    fn interval_on_the_limit_is_inconclusive() {
        let e = Estimate::of(&[0.15, 0.25, 0.2]).expect("values");
        assert_eq!(judge_mean(Some(&e), 0.2), Verdict::Inconclusive);
        assert_eq!(judge_mean(Some(&e), 1.0), Verdict::Within);
        assert_eq!(judge_mean(Some(&e), 0.01), Verdict::Over);
        assert_eq!(judge_mean(None, 0.2), Verdict::Missing);
    }
}
