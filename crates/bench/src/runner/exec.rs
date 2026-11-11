//! Executes a plan on Windows.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use pitot_irsdk::live::{LiveReader, ReaderConfig, SdkNames, Snapshot, Step, VarSet};

use super::detect;
use super::files;
use super::plan::{self, AppPlan, BASELINE, Plan, ReplayPlan, Slot};
use super::record::{Outcome, RunInfo, RunRecord, SeriesRecord};
use crate::collect::{self, Target, tree};
use crate::paths;
use crate::sys::{self, Job, Process, SysError};

/// The replay runs this much longer than warm-up and measurement, so it outlasts the measurement.
const REPLAY_SLACK: Duration = Duration::from_secs(10);
/// How long the replay may take to publish its first frame.
const REPLAY_START: Duration = Duration::from_secs(30);
/// How long stopped processes may take to exit.
const EXIT_WAIT: Duration = Duration::from_secs(10);
/// How long an app may take to exit after its windows were asked to close.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum RunnerError {
    Io(io::Error),
    Json(serde_json::Error),
    Sys(SysError),
    Collect(collect::CollectError),
    /// A problem that stops the series before it starts, or that every later run would hit too.
    Setup(String),
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Json(e) => write!(f, "{e}"),
            Self::Sys(e) => write!(f, "{e}"),
            Self::Collect(e) => write!(f, "collection failed: {e}"),
            Self::Setup(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for RunnerError {}

impl From<io::Error> for RunnerError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for RunnerError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<SysError> for RunnerError {
    fn from(e: SysError) -> Self {
        Self::Sys(e)
    }
}

impl From<collect::CollectError> for RunnerError {
    fn from(e: collect::CollectError) -> Self {
        Self::Collect(e)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Recorded in the series file.
    pub plan_file: PathBuf,
    /// Check the plan and print the order without running anything.
    pub dry_run: bool,
}

fn env_path(text: &str) -> Result<PathBuf, RunnerError> {
    plan::expand(text, |name| std::env::var(name).ok())
        .map(PathBuf::from)
        .map_err(RunnerError::Setup)
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Executable names that show an app is already running: its `measure` names, or its command's file name.
fn running_names(app: &AppPlan) -> Result<Vec<String>, RunnerError> {
    if !app.measure.is_empty() {
        return Ok(app.measure.clone());
    }
    let command = env_path(&app.command)?;
    Ok(command
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .into_iter()
        .collect())
}

fn sdk_reader() -> LiveReader {
    let config = ReaderConfig {
        retry_interval: Duration::from_millis(100),
        idle_timeout: Duration::from_millis(100),
    };
    LiveReader::new(SdkNames::sim(), VarSet::new(), config)
}

/// Whether the sim, or a replay, publishes SDK shared memory right now.
fn sdk_in_use() -> bool {
    let mut snapshot = Snapshot::new();
    matches!(sdk_reader().step(&mut snapshot), Ok(Step::Connected))
}

/// Checks everything a series needs before it starts, so a dry run reports the same problems.
fn check(plan: &Plan) -> Result<Vec<detect::Found>, RunnerError> {
    let mut entries = Vec::new();
    sys::processes(&mut entries)?;
    for app in &plan.apps {
        let names = running_names(app)?;
        if let Some(entry) = entries
            .iter()
            .find(|e| names.iter().any(|n| tree::same_exe(n, &e.name)))
        {
            return Err(RunnerError::Setup(format!(
                "{} (pid {}) of app `{}` is already running; close it before the series",
                entry.name, entry.pid, app.name
            )));
        }
        let command = env_path(&app.command)?;
        if command.components().count() > 1 && !command.is_file() {
            return Err(RunnerError::Setup(format!(
                "app `{}`: {} does not exist",
                app.name,
                command.display()
            )));
        }
        for file in &app.files {
            let from = env_path(&file.from)?;
            if !from.is_file() {
                return Err(RunnerError::Setup(format!(
                    "app `{}`: {} does not exist",
                    app.name,
                    from.display()
                )));
            }
            env_path(&file.to)?;
        }
    }
    if let Some(replay) = &plan.replay {
        let file = replay.file().map(|(_, f)| f).unwrap_or_default();
        if !Path::new(file).is_file() {
            return Err(RunnerError::Setup(format!(
                "replay file {file} does not exist"
            )));
        }
        if sdk_in_use() {
            return Err(RunnerError::Setup(
                "the sim or another replay already publishes SDK shared memory".to_owned(),
            ));
        }
    }
    Ok(detect::find(&entries, &plan.detect))
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), RunnerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    fs::write(path, text)?;
    Ok(())
}

/// Runs the series and returns its record. With `dry_run`, only checks the plan and returns the order.
pub fn execute(plan: &Plan, options: &Options) -> Result<SeriesRecord, RunnerError> {
    let restore_dir = paths::restore_dir();
    for path in files::restore(&restore_dir)? {
        eprintln!("restored {} from an interrupted series", path.display());
    }
    let detected = check(plan)?;
    for found in &detected {
        eprintln!(
            "warning: {} is running (pid {}): {}",
            found.name, found.pid, found.what
        );
    }

    let seed = plan
        .seed
        .unwrap_or_else(|| sys::now_filetime() ^ (u64::from(std::process::id()) << 32));
    let names: Vec<&str> = plan.apps.iter().map(|a| a.name.as_str()).collect();
    let slots = plan::order(&names, plan.repeats, seed);
    let series_id = paths::local_run_id();
    let runs = slots
        .iter()
        .map(|slot| RunRecord {
            position: slot.position,
            app: slot.app.clone(),
            repeat: slot.repeat,
            dir: slash_path(&paths::run_dir(
                &plan.scenario,
                &slot.app,
                &paths::series_run_id(&series_id, slot.position),
            )),
            outcome: None,
        })
        .collect();
    let mut record = SeriesRecord {
        tool: "pitot-bench".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        series_id: series_id.clone(),
        plan_file: slash_path(&options.plan_file),
        started_local: paths::local_timestamp(),
        finished_local: None,
        seed,
        detected,
        plan: plan.clone(),
        runs,
    };
    let minutes = plan.estimated_seconds() / 60.0;
    eprintln!(
        "series {series_id}: {} runs, about {minutes:.0} min, seed {seed}",
        slots.len()
    );
    for slot in &slots {
        eprintln!("  {:>3}  {}", slot.position, slot.app);
    }
    if options.dry_run {
        return Ok(record);
    }

    let series_file = paths::series_file(&plan.scenario, &series_id);
    write_json(&series_file, &record)?;
    for (i, slot) in slots.iter().enumerate() {
        let dir = PathBuf::from(&record.runs[i].dir);
        eprint!(
            "[{}/{}] {} (repeat {}): ",
            slot.position,
            slots.len(),
            slot.app,
            slot.repeat + 1
        );
        let outcome = run_one(plan, slot, &series_id, &dir)?;
        match &outcome {
            Outcome::Measured { summary } => eprintln!(
                "{:.3}% CPU ({:.2} ms/s), private {:.1} MB",
                summary.cpu_pct_mean,
                summary.cpu_ms_per_s_mean,
                summary.private_bytes_last as f64 / (1024.0 * 1024.0)
            ),
            Outcome::NotRunning => eprintln!("not running after the warm-up; not measured"),
            Outcome::LaunchFailed { error } => eprintln!("launch failed: {error}"),
        }
        record.runs[i].outcome = Some(outcome);
        write_json(&series_file, &record)?;
        if i + 1 < slots.len() {
            thread::sleep(Duration::from_secs_f64(plan.cooldown_s));
        }
    }
    record.finished_local = Some(paths::local_timestamp());
    write_json(&series_file, &record)?;
    eprintln!("series written to {}", series_file.display());
    Ok(record)
}

/// Restores placed files when the run ends, also on errors.
struct Placed;

impl Placed {
    fn new(app: &AppPlan) -> Result<Self, RunnerError> {
        let pairs = app
            .files
            .iter()
            .map(|f| Ok((env_path(&f.from)?, env_path(&f.to)?)))
            .collect::<Result<Vec<_>, RunnerError>>()?;
        let dir = paths::restore_dir();
        if let Err(e) = files::place(&dir, &pairs) {
            let _ = files::restore(&dir);
            return Err(e.into());
        }
        Ok(Self)
    }
}

impl Drop for Placed {
    fn drop(&mut self) {
        let dir = paths::restore_dir();
        if let Err(e) = files::restore(&dir) {
            eprintln!(
                "error: could not restore files ({e}); the originals are kept in {}",
                dir.display()
            );
        }
    }
}

fn run_one(plan: &Plan, slot: &Slot, series_id: &str, dir: &Path) -> Result<Outcome, RunnerError> {
    fs::create_dir_all(dir)?;
    let app = (slot.app != BASELINE)
        .then(|| plan.app(&slot.app))
        .flatten();
    let started_local = paths::local_timestamp();
    // Closing a job stops whatever is left of this run, also when the run fails. The replay has its own job, so
    // stopping the app leaves it running until it ends on its own.
    let app_job = Job::new()?;
    let replay_job = Job::new()?;
    let placed = app
        .filter(|a| !a.files.is_empty())
        .map(Placed::new)
        .transpose()?;
    let replay = plan
        .replay
        .as_ref()
        .map(|r| start_replay(r, plan, dir, &replay_job))
        .transpose()?;

    let launched = Instant::now();
    let (outcome, child, pid) = match app {
        None => (None, None, None),
        Some(app) => match launch(app, &app_job) {
            Ok(child) => {
                let pid = child.id();
                (None, Some(child), Some(pid))
            }
            Err(e) => (
                Some(Outcome::LaunchFailed {
                    error: e.to_string(),
                }),
                None,
                None,
            ),
        },
    };
    let target = match (app, pid) {
        (Some(app), _) if !app.measure.is_empty() => Target::Names(app.measure.clone()),
        (Some(_), Some(pid)) => Target::Pid(pid),
        _ => Target::Names(Vec::new()),
    };

    let outcome = match outcome {
        Some(failed) => failed,
        None => {
            eprint!("warm-up {} s, ", plan.warmup_s);
            if let Some(wait) = (launched + Duration::from_secs_f64(plan.warmup_s))
                .checked_duration_since(Instant::now())
            {
                thread::sleep(wait);
            }
            if app.is_some() && members(&target)?.is_empty() {
                Outcome::NotRunning
            } else {
                eprint!("measuring {} s ... ", plan.measure_s);
                let options = collect::Options {
                    interval: Duration::from_millis(plan.interval_ms),
                    duration: Duration::from_secs_f64(plan.measure_s),
                    out_dir: dir.to_owned(),
                };
                let summary = collect::run(target.clone(), &options, |_| {})?;
                Outcome::Measured { summary }
            }
        }
    };

    stop(&target, child, &app_job)?;
    if let Some(replay) = replay {
        finish_replay(replay)?;
    }
    drop(placed);
    let info = RunInfo {
        series_id: series_id.to_owned(),
        position: slot.position,
        app: slot.app.clone(),
        repeat: slot.repeat,
        command: app.map(|a| a.command.clone()),
        args: app.map(|a| a.args.clone()).unwrap_or_default(),
        pid,
        started_local,
        outcome: outcome.clone(),
    };
    write_json(&dir.join("run.json"), &info)?;
    Ok(outcome)
}

fn launch(app: &AppPlan, job: &Job) -> Result<Child, RunnerError> {
    let child = Command::new(env_path(&app.command)?)
        .args(&app.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Err(e) = job.assign(&child) {
        eprint!("(not in the run's job: {e}) ");
    }
    Ok(child)
}

/// Starts `pitot-bench replay` in a child process, logging to `replay.log`, and waits until it publishes.
fn start_replay(
    replay: &ReplayPlan,
    plan: &Plan,
    dir: &Path,
    job: &Job,
) -> Result<Child, RunnerError> {
    let seconds = plan.warmup_s + plan.measure_s + REPLAY_SLACK.as_secs_f64();
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("replay");
    let Some((option, file)) = replay.file() else {
        return Err(RunnerError::Setup("[replay] has no file".to_owned()));
    };
    command.args([option, file]);
    let log = dir.join("replay.log");
    let mut child = command
        .args(["--loop", "--speed"])
        .arg(replay.speed.to_string())
        .arg("--seconds")
        .arg(seconds.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(&log)?)
        .spawn()?;
    if let Err(e) = job.assign(&child) {
        eprint!("(replay not in the run's job: {e}) ");
    }
    let mut reader = sdk_reader();
    let mut snapshot = Snapshot::new();
    let deadline = Instant::now() + REPLAY_START;
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(RunnerError::Setup(format!(
                "the replay exited ({status}) before publishing; see {}",
                log.display()
            )));
        }
        if matches!(
            reader.step(&mut snapshot),
            Ok(Step::Connected | Step::Tick { .. })
        ) {
            return Ok(child);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(RunnerError::Setup(format!(
                "the replay did not publish within {} s; see {}",
                REPLAY_START.as_secs(),
                log.display()
            )));
        }
    }
}

/// Waits for the replay to end on its own, so its log has the summary, and stops it if it does not.
fn finish_replay(mut child: Child) -> Result<(), RunnerError> {
    if wait_exit(&mut child, REPLAY_SLACK + EXIT_WAIT)?.is_none() {
        let _ = child.kill();
        child.wait()?;
    }
    Ok(())
}

fn wait_exit(child: &mut Child, timeout: Duration) -> io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// PIDs and creation times of the target's process tree right now.
fn members(target: &Target) -> Result<Vec<(u32, u64)>, RunnerError> {
    let mut entries = Vec::new();
    sys::processes(&mut entries)?;
    let mut created = HashMap::new();
    let mut pids = Vec::new();
    tree::select(
        &entries,
        target,
        |entry| {
            let time = Process::open(entry.pid)
                .and_then(|p| p.times())
                .ok()
                .map(|t| t.created);
            if let Some(time) = time {
                created.insert(entry.pid, time);
            }
            time
        },
        &mut pids,
    );
    Ok(pids
        .into_iter()
        .filter_map(|pid| created.get(&pid).map(|&time| (pid, time)))
        .collect())
}

/// Asks the measured tree to close, as closing its windows would, and gives it [`CLOSE_WAIT`] to exit. Then stops
/// what is left of the tree (which can hold processes outside the job) and of the job, and waits until the tree is
/// gone.
fn stop(target: &Target, child: Option<Child>, job: &Job) -> Result<(), RunnerError> {
    let pids: Vec<u32> = members(target)?.iter().map(|&(pid, _)| pid).collect();
    if sys::close_windows(&pids) > 0 {
        let deadline = Instant::now() + CLOSE_WAIT;
        while Instant::now() < deadline && !members(target)?.is_empty() {
            thread::sleep(Duration::from_millis(100));
        }
    }
    for (pid, created) in members(target)? {
        let _ = sys::terminate(pid, created);
    }
    let _ = job.terminate();
    if let Some(mut child) = child {
        let _ = wait_exit(&mut child, EXIT_WAIT)?;
    }
    let deadline = Instant::now() + EXIT_WAIT;
    while !members(target)?.is_empty() {
        if Instant::now() >= deadline {
            return Err(RunnerError::Setup(
                "the app's processes did not exit after being terminated".to_owned(),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
