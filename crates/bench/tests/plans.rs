//! The committed benchmark plans parse, and the files they put in place exist.

use std::fs;
use std::path::{Path, PathBuf};

use pitot_bench::runner::Plan;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn committed_plans_are_valid() {
    let dir = repo().join("benchmarks/tier-a");
    let mut count = 0;
    for entry in fs::read_dir(&dir).expect("benchmarks/tier-a") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        // Machine-specific copies (`*.local.toml`) are not committed.
        if !name.ends_with(".toml") || name.ends_with(".local.toml") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("readable");
        let plan = Plan::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        for app in &plan.apps {
            for file in &app.files {
                assert!(
                    repo().join(&file.from).is_file(),
                    "{name}: {} is missing",
                    file.from
                );
            }
        }
        count += 1;
    }
    assert!(count >= 3, "found {count} plans");
}
