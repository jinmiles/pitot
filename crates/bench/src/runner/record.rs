//! What a series writes about itself: `series/<series-id>.json` and each run's `run.json`.

use serde::{Deserialize, Serialize};

use super::detect::Found;
use super::plan::Plan;
use crate::collect::Summary;

/// `bench-results/<scenario>/series/<series-id>.json`, rewritten after every run so an interrupted series keeps
/// what it finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesRecord {
    pub tool: String,
    pub version: String,
    pub series_id: String,
    pub plan_file: String,
    pub started_local: String,
    #[serde(default)]
    pub finished_local: Option<String>,
    pub seed: u64,
    /// Overlays and sim processes that were running when the series started.
    pub detected: Vec<Found>,
    pub plan: Plan,
    pub runs: Vec<RunRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub position: u32,
    /// An app name, or `baseline`.
    pub app: String,
    pub repeat: u32,
    /// The run's directory, relative to the working directory, with `/` separators.
    pub dir: String,
    /// Not set until the run is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

/// `run.json` in a run's directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    pub series_id: String,
    pub position: u32,
    pub app: String,
    pub repeat: u32,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// The launched process; the measured tree may hold more.
    #[serde(default)]
    pub pid: Option<u32>,
    pub started_local: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Measured {
        summary: Summary,
    },
    /// No process of the app was running after the warm-up, so nothing was measured.
    NotRunning,
    LaunchFailed {
        error: String,
    },
}
