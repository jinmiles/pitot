//! The single source of truth for where bench output goes: `bench-results/<scenario>/<app>/<run-id>/` for runs
//! and `bench-results/<scenario>/series/<series-id>.json` for series, relative to the working directory.

use std::path::PathBuf;

pub const RESULTS_ROOT: &str = "bench-results";

/// Scenario name for collections started by hand rather than by a scenario run.
pub const ADHOC: &str = "adhoc";

/// Directory under a scenario that holds one file per series.
pub const SERIES: &str = "series";

pub fn run_dir(scenario: &str, app: &str, run_id: &str) -> PathBuf {
    [RESULTS_ROOT, scenario, app, run_id].iter().collect()
}

/// A run's ID within a series: the series ID and the run's position.
pub fn series_run_id(series_id: &str, position: u32) -> String {
    format!("{series_id}-{position:02}")
}

/// The record of a series: its plan, order, and the outcome of each run.
pub fn series_file(scenario: &str, series_id: &str) -> PathBuf {
    [RESULTS_ROOT, scenario, SERIES, &format!("{series_id}.json")]
        .iter()
        .collect()
}

/// A series' report, `report-<series-id>.<extension>` next to the scenario's app directories.
pub fn report_file(scenario: &str, series_id: &str, extension: &str) -> PathBuf {
    [
        RESULTS_ROOT,
        scenario,
        &format!("report-{series_id}.{extension}"),
    ]
    .iter()
    .collect()
}

/// Captures the recorder writes by default, relative to the working directory.
pub const CAPTURES_ROOT: &str = "captures";

/// A recording's default path: `captures/<run-id>.ptc`.
pub fn capture_file(run_id: &str) -> PathBuf {
    [CAPTURES_ROOT, &format!("{run_id}.ptc")].iter().collect()
}

/// Backups of files a runner put in place, kept until they are restored.
pub fn restore_dir() -> PathBuf {
    [RESULTS_ROOT, ".restore"].iter().collect()
}

/// A run ID from local time: `YYYYMMDD-HHMMSS`.
pub fn run_id([year, month, day, hour, minute, second]: [u16; 6]) -> String {
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

/// A run ID from the current local time.
#[cfg(windows)]
pub fn local_run_id() -> String {
    run_id(crate::sys::local_time())
}

/// Local time for records: `YYYY-MM-DDTHH:MM:SS`.
pub fn timestamp([year, month, day, hour, minute, second]: [u16; 6]) -> String {
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}")
}

#[cfg(windows)]
pub fn local_timestamp() -> String {
    timestamp(crate::sys::local_time())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        assert_eq!(run_id([2026, 11, 9, 7, 5, 3]), "20261109-070503");
        assert_eq!(
            run_dir(ADHOC, "pitot-overlay", "20261109-070503"),
            PathBuf::from("bench-results/adhoc/pitot-overlay/20261109-070503")
        );
        assert_eq!(series_run_id("20261109-070503", 3), "20261109-070503-03");
        assert_eq!(timestamp([2026, 11, 9, 7, 5, 3]), "2026-11-09T07:05:03");
        assert_eq!(
            capture_file("20261109-070503"),
            PathBuf::from("captures/20261109-070503.ptc")
        );
        assert_eq!(
            report_file("tier-a", "20261109-070503", "md"),
            PathBuf::from("bench-results/tier-a/report-20261109-070503.md")
        );
        assert_eq!(
            series_file("tier-a", "20261109-070503"),
            PathBuf::from("bench-results/tier-a/series/20261109-070503.json")
        );
    }
}
