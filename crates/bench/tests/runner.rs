//! A whole series without a replay: an app that keeps running, one that exits at once, and one that cannot start,
//! with baselines in between and a configuration file that is put back afterwards.
#![cfg(windows)]

use std::fs;
use std::path::PathBuf;

use pitot_bench::runner::{self, BASELINE, Outcome, Plan};

#[test]
fn runs_a_series_and_restores_files() {
    let root = std::env::temp_dir().join(format!("pitot-bench-runner-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("temp dir");
    // Output goes to `bench-results/` under the working directory; this is the only test in this binary.
    std::env::set_current_dir(&root).expect("enter temp dir");

    let config = root.join("app-config.toml");
    fs::write(&config, "user settings").expect("write config");
    fs::write(root.join("bench-config.toml"), "bench settings").expect("write bench config");
    let quiet_out = root.join("quiet-collect");
    // The bench binary itself stands in for an app: `collect` idles for its duration.
    let bench = PathBuf::from(env!("CARGO_BIN_EXE_pitot-bench"));
    let slash = |p: &std::path::Path| p.to_string_lossy().replace('\\', "/");
    let text = format!(
        r#"
scenario = "runner-test"
repeats = 1
warmup_s = 0.5
measure_s = 1.0
cooldown_s = 0
interval_ms = 250
seed = 3

[[app]]
name = "idle"
command = "{bench}"
args = ["collect", "--name", "no-such-process", "--seconds", "30", "--out", "{quiet}"]
files = [{{ from = "bench-config.toml", to = "{config}" }}]

[[app]]
name = "exits"
command = "{bench}"

[[app]]
name = "missing"
command = "no-such-program-for-pitot-bench"
"#,
        bench = slash(&bench),
        quiet = slash(&quiet_out),
        config = slash(&config),
    );
    let plan = Plan::parse(&text).expect("valid plan");
    let options = runner::Options {
        plan_file: "plan.toml".into(),
        dry_run: false,
    };
    let record = runner::execute(&plan, &options).expect("series runs");

    let apps: Vec<&str> = record.runs.iter().map(|r| r.app.as_str()).collect();
    assert_eq!(apps.len(), 5, "{apps:?}");
    assert_eq!((apps[0], apps[4]), (BASELINE, BASELINE));
    for run in &record.runs {
        let outcome = run.outcome.as_ref().expect("every run finished");
        match (run.app.as_str(), outcome) {
            (BASELINE, Outcome::Measured { summary }) => {
                assert_eq!(summary.processes_max, 0, "nothing to measure in a baseline");
            }
            ("idle", Outcome::Measured { summary }) => {
                assert!(summary.processes_max >= 1, "{summary:?}");
                assert!(summary.private_bytes_max > 0);
            }
            ("exits", Outcome::NotRunning) | ("missing", Outcome::LaunchFailed { .. }) => {}
            other => panic!("unexpected outcome {other:?}"),
        }
        let dir = root.join(&run.dir);
        assert!(dir.join("run.json").is_file(), "{}", dir.display());
        if matches!(outcome, Outcome::Measured { .. }) {
            assert!(dir.join("samples.csv").is_file());
            assert!(dir.join("collect.json").is_file());
        }
    }
    assert!(record.finished_local.is_some());

    let series = root
        .join("bench-results/runner-test/series")
        .join(format!("{}.json", record.series_id));
    let saved: runner::SeriesRecord =
        serde_json::from_str(&fs::read_to_string(series).expect("series file")).expect("parses");
    // Floats can differ in the last digit after a JSON round trip, so compare what identifies the runs.
    let key = |r: &runner::SeriesRecord| {
        let runs: Vec<_> = r
            .runs
            .iter()
            .map(|run| {
                (
                    run.position,
                    run.app.clone(),
                    run.dir.clone(),
                    run.outcome.is_some(),
                )
            })
            .collect();
        (r.series_id.clone(), r.seed, r.plan.apps.len(), runs)
    };
    assert_eq!(key(&saved), key(&record));
    assert_eq!(
        fs::read_to_string(&config).expect("config"),
        "user settings"
    );
    assert!(!root.join("bench-results/.restore/manifest.json").exists());

    std::env::set_current_dir(std::env::temp_dir()).expect("leave temp dir");
    let _ = fs::remove_dir_all(&root);
}
