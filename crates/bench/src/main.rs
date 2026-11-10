use std::process::ExitCode;

#[cfg(windows)]
use pitot_bench::replay::{self, FrameSource, IbtSource, IrdtSource, ReplayOptions};

const USAGE: &str = "usage:
  pitot-bench replay (--irdt <file> | --ibt <file> | --capture <file>) [--loop] [--speed <factor>] [--seconds <n>]
  pitot-bench convert (--irdt <file> | --ibt <file>) --out <file.ptc> [--level <n>]
  pitot-bench collect (--pid <n> | --name <exe>...) [--seconds <n>] [--interval-ms <n>] [--out <dir>]
  pitot-bench run <plan.toml> [--dry-run]
  pitot-bench report <series.json> [--force]";

#[derive(Debug, Default)]
struct ReplayArgs {
    irdt: Option<String>,
    ibt: Option<String>,
    capture: Option<String>,
    looped: bool,
    speed: Option<f64>,
    seconds: Option<f64>,
}

fn parse_replay(mut args: impl Iterator<Item = String>) -> Result<ReplayArgs, String> {
    let mut parsed = ReplayArgs::default();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--irdt" => parsed.irdt = Some(value("--irdt")?),
            "--ibt" => parsed.ibt = Some(value("--ibt")?),
            "--capture" => parsed.capture = Some(value("--capture")?),
            "--loop" => parsed.looped = true,
            "--speed" => {
                parsed.speed = Some(
                    value("--speed")?
                        .parse()
                        .map_err(|_| "--speed needs a number")?,
                )
            }
            "--seconds" => {
                parsed.seconds = Some(
                    value("--seconds")?
                        .parse()
                        .map_err(|_| "--seconds needs a number")?,
                )
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    let given = [&parsed.irdt, &parsed.ibt, &parsed.capture]
        .iter()
        .filter(|f| f.is_some())
        .count();
    if given == 1 {
        Ok(parsed)
    } else {
        Err("give exactly one of --irdt, --ibt, or --capture".to_owned())
    }
}

#[cfg(windows)]
fn replay(args: ReplayArgs) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    let options = ReplayOptions {
        speed: args.speed.unwrap_or(1.0),
        looped: args.looped,
        max_duration: args.seconds.map(Duration::from_secs_f64),
        ..ReplayOptions::default()
    };
    fn play<S: FrameSource>(
        mut source: S,
        options: &ReplayOptions,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let layout = source.layout();
        eprintln!(
            "replaying {} variables at {} Hz x{}{}{}",
            layout.vars.len(),
            layout.tick_rate,
            options.speed,
            if options.looped { ", looping" } else { "" },
            if layout.player_car_only {
                ", player car only"
            } else {
                ""
            },
        );
        let summary = replay::run(&mut source, options)?;
        eprintln!(
            "done: {} frames, {} session info revisions, {} loops, {} late frames in {:.1} s",
            summary.frames,
            summary.session_infos,
            summary.loops,
            summary.late_frames,
            summary.elapsed.as_secs_f64()
        );
        Ok(())
    }
    match (args.irdt, args.ibt, args.capture) {
        (Some(path), _, _) => play(IrdtSource::open(path)?, &options),
        (_, Some(path), _) => play(IbtSource::open(path)?, &options),
        (_, _, Some(path)) => play(pitot_bench::capture::CaptureSource::open(path)?, &options),
        _ => Err(USAGE.into()),
    }
}

#[cfg(not(windows))]
fn replay(_: ReplayArgs) -> Result<(), Box<dyn std::error::Error>> {
    Err("replay needs Windows shared memory".into())
}

#[derive(Debug, PartialEq)]
struct CollectArgs {
    target: pitot_bench::collect::Target,
    seconds: f64,
    interval_ms: u64,
    out: Option<String>,
}

fn parse_collect(mut args: impl Iterator<Item = String>) -> Result<CollectArgs, String> {
    use pitot_bench::collect::Target;
    let (mut pid, mut names, mut seconds, mut interval_ms, mut out) =
        (None, Vec::new(), 60.0_f64, 1000_u64, None);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--pid" => {
                pid = Some(
                    value("--pid")?
                        .parse()
                        .map_err(|_| "--pid needs a number")?,
                )
            }
            "--name" => names.push(value("--name")?),
            "--seconds" => {
                seconds = value("--seconds")?
                    .parse()
                    .map_err(|_| "--seconds needs a number")?
            }
            "--interval-ms" => {
                interval_ms = value("--interval-ms")?
                    .parse()
                    .map_err(|_| "--interval-ms needs a number")?
            }
            "--out" => out = Some(value("--out")?),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("--seconds must be positive".to_owned());
    }
    if interval_ms < 100 {
        return Err("--interval-ms must be at least 100".to_owned());
    }
    let target = match (pid, names.is_empty()) {
        (Some(pid), true) => Target::Pid(pid),
        (None, false) => Target::Names(names),
        _ => return Err("give either --pid or one or more --name".to_owned()),
    };
    Ok(CollectArgs {
        target,
        seconds,
        interval_ms,
        out,
    })
}

#[cfg(windows)]
fn collect(args: CollectArgs) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    use pitot_bench::{collect, paths};
    let out_dir = match args.out {
        Some(dir) => dir.into(),
        None => paths::run_dir(paths::ADHOC, &args.target.label(), &paths::local_run_id()),
    };
    let options = collect::Options {
        interval: Duration::from_millis(args.interval_ms),
        duration: Duration::from_secs_f64(args.seconds),
        out_dir: out_dir.clone(),
    };
    eprintln!(
        "collecting {:?} for {} s every {} ms into {}",
        args.target,
        args.seconds,
        args.interval_ms,
        out_dir.display()
    );
    let mut empty = 0u32;
    let summary = collect::run(args.target, &options, |sample| {
        if sample.processes == 0 {
            empty += 1;
        }
    })?;
    eprintln!("{summary}");
    if empty > 0 {
        eprintln!("note: the target was not running in {empty} samples");
    }
    Ok(())
}

#[cfg(not(windows))]
fn collect(_: CollectArgs) -> Result<(), Box<dyn std::error::Error>> {
    Err("collect needs Windows".into())
}

#[derive(Debug, PartialEq)]
struct ConvertArgs {
    irdt: Option<String>,
    ibt: Option<String>,
    out: String,
    level: i32,
}

fn parse_convert(mut args: impl Iterator<Item = String>) -> Result<ConvertArgs, String> {
    let (mut irdt, mut ibt, mut out, mut level) = (None, None, None, 19);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--irdt" => irdt = Some(value("--irdt")?),
            "--ibt" => ibt = Some(value("--ibt")?),
            "--out" => out = Some(value("--out")?),
            "--level" => {
                level = value("--level")?
                    .parse()
                    .map_err(|_| "--level needs a number")?
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if irdt.is_some() == ibt.is_some() {
        return Err("give exactly one of --irdt or --ibt".to_owned());
    }
    if !(1..=22).contains(&level) {
        return Err("--level must be 1 to 22".to_owned());
    }
    let out = out.ok_or("give --out <file.ptc>")?;
    Ok(ConvertArgs {
        irdt,
        ibt,
        out,
        level,
    })
}

fn convert(args: ConvertArgs) -> Result<(), Box<dyn std::error::Error>> {
    use pitot_bench::capture::{self, CaptureSource};
    use pitot_bench::replay::{IbtSource, IrdtSource};
    use std::path::Path;
    let out = Path::new(&args.out);
    let started = std::time::Instant::now();
    let (input, counts) = match (&args.irdt, &args.ibt) {
        (Some(input), _) => (
            input,
            capture::convert::irdt(input.as_ref(), out, args.level)?,
        ),
        (_, Some(input)) => (
            input,
            capture::convert::ibt(input.as_ref(), out, args.level)?,
        ),
        _ => unreachable!("checked by parse_convert"),
    };
    let (before, after) = (
        std::fs::metadata(input)?.len(),
        std::fs::metadata(out)?.len(),
    );
    eprintln!(
        "{} frames, {} session info revisions, {} gaps ({} missed ticks), {} disconnects in {} chunks",
        counts.frames,
        counts.session_info_revisions,
        counts.gaps,
        counts.missed_ticks,
        counts.disconnects,
        counts.chunks
    );
    eprintln!(
        "{before} -> {after} bytes ({:.1}%) in {:.1} s",
        after as f64 * 100.0 / before.max(1) as f64,
        started.elapsed().as_secs_f64()
    );
    let mut written = CaptureSource::open(out)?;
    let checked = match (&args.irdt, &args.ibt) {
        (Some(input), _) => capture::convert::check(&mut IrdtSource::open(input)?, &mut written)?,
        (_, Some(input)) => capture::convert::check(&mut IbtSource::open(input)?, &mut written)?,
        _ => unreachable!("checked by parse_convert"),
    };
    eprintln!(
        "checked against the source: {} frames and {} session info revisions identical",
        checked.frames, checked.session_infos
    );
    Ok(())
}

#[derive(Debug, PartialEq)]
struct RunArgs {
    plan: String,
    dry_run: bool,
}

fn parse_run(args: impl Iterator<Item = String>) -> Result<RunArgs, String> {
    let (mut plan, mut dry_run) = (None, false);
    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            other if other.starts_with("--") => return Err(format!("unknown argument `{other}`")),
            _ if plan.is_some() => return Err("give one plan file".to_owned()),
            _ => plan = Some(arg),
        }
    }
    let plan = plan.ok_or("give a plan file")?;
    Ok(RunArgs { plan, dry_run })
}

#[cfg(windows)]
fn run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    use pitot_bench::runner::{self, Outcome, Plan};
    let text = std::fs::read_to_string(&args.plan)
        .map_err(|e| format!("cannot read {}: {e}", args.plan))?;
    let plan = Plan::parse(&text).map_err(|e| format!("{}: {e}", args.plan))?;
    let options = runner::Options {
        plan_file: args.plan.into(),
        dry_run: args.dry_run,
    };
    let record = runner::execute(&plan, &options)?;
    if !args.dry_run {
        let measured = record
            .runs
            .iter()
            .filter(|r| matches!(r.outcome, Some(Outcome::Measured { .. })))
            .count();
        eprintln!("{measured} of {} runs measured", record.runs.len());
    }
    Ok(())
}

#[cfg(not(windows))]
fn run(_: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    Err("run needs Windows".into())
}

#[derive(Debug, PartialEq)]
struct ReportArgs {
    series: String,
    force: bool,
}

fn parse_report(args: impl Iterator<Item = String>) -> Result<ReportArgs, String> {
    let (mut series, mut force) = (None, false);
    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            other if other.starts_with("--") => return Err(format!("unknown argument `{other}`")),
            _ if series.is_some() => return Err("give one series file".to_owned()),
            _ => series = Some(arg),
        }
    }
    let series = series.ok_or("give a series file")?;
    Ok(ReportArgs { series, force })
}

fn report(args: ReportArgs) -> Result<(), Box<dyn std::error::Error>> {
    let path = pitot_bench::report::generate(args.series.as_ref(), args.force)?;
    eprintln!("report written to {}", path.display());
    Ok(())
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("replay") => parse_replay(args).map_err(Into::into).and_then(replay),
        Some("collect") => parse_collect(args).map_err(Into::into).and_then(collect),
        Some("convert") => parse_convert(args).map_err(Into::into).and_then(convert),
        Some("run") => parse_run(args).map_err(Into::into).and_then(run),
        Some("report") => parse_report(args).map_err(Into::into).and_then(report),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitot_bench::collect::Target;

    fn collect_args(args: &[&str]) -> Result<CollectArgs, String> {
        parse_collect(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn collect_arguments() {
        let parsed = collect_args(&["--name", "a.exe", "--name", "b", "--seconds", "5"]);
        assert_eq!(
            parsed,
            Ok(CollectArgs {
                target: Target::Names(vec!["a.exe".into(), "b".into()]),
                seconds: 5.0,
                interval_ms: 1000,
                out: None,
            })
        );
        assert_eq!(
            collect_args(&["--pid", "42"]).map(|a| a.target),
            Ok(Target::Pid(42))
        );
        assert!(collect_args(&[]).is_err(), "a target is required");
        assert!(collect_args(&["--pid", "1", "--name", "a"]).is_err());
        assert!(collect_args(&["--pid", "1", "--interval-ms", "10"]).is_err());
        assert!(collect_args(&["--pid", "1", "--seconds", "0"]).is_err());
    }

    #[test]
    fn run_arguments() {
        let run = |args: &[&str]| parse_run(args.iter().map(|a| (*a).to_owned()));
        assert_eq!(
            run(&["plan.toml", "--dry-run"]),
            Ok(RunArgs {
                plan: "plan.toml".into(),
                dry_run: true
            })
        );
        assert!(run(&[]).is_err());
        assert!(run(&["a.toml", "b.toml"]).is_err());
        assert!(run(&["a.toml", "--fast"]).is_err());
        let report = |args: &[&str]| parse_report(args.iter().map(|a| (*a).to_owned()));
        assert_eq!(
            report(&["--force", "s.json"]),
            Ok(ReportArgs {
                series: "s.json".into(),
                force: true
            })
        );
        assert!(report(&[]).is_err());
    }

    #[test]
    fn convert_arguments() {
        let convert = |args: &[&str]| parse_convert(args.iter().map(|a| (*a).to_owned()));
        assert_eq!(
            convert(&["--irdt", "a.irdt", "--out", "a.ptc"]),
            Ok(ConvertArgs {
                irdt: Some("a.irdt".into()),
                ibt: None,
                out: "a.ptc".into(),
                level: 19
            })
        );
        assert_eq!(
            convert(&["--ibt", "a.ibt", "--out", "a.ptc", "--level", "3"]).map(|a| a.level),
            Ok(3)
        );
        assert!(convert(&["--irdt", "a", "--ibt", "b", "--out", "c"]).is_err());
        assert!(convert(&["--irdt", "a"]).is_err(), "needs --out");
        assert!(convert(&["--irdt", "a", "--out", "b", "--level", "30"]).is_err());
    }
}
