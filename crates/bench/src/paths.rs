//! The single source of truth for where bench output goes: `bench-results/<scenario>/<app>/<run-id>/`, relative
//! to the working directory.

use std::path::PathBuf;

pub const RESULTS_ROOT: &str = "bench-results";

/// Scenario name for collections started by hand rather than by a scenario run.
pub const ADHOC: &str = "adhoc";

pub fn run_dir(scenario: &str, app: &str, run_id: &str) -> PathBuf {
    [RESULTS_ROOT, scenario, app, run_id].iter().collect()
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
    }
}
