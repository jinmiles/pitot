use std::process::ExitCode;

#[cfg(windows)]
use pitot_bench::replay::{self, FrameSource, IbtSource, IrdtSource, ReplayOptions};

const USAGE: &str = "usage:
  pitot-bench replay (--irdt <file> | --ibt <file>) [--loop] [--speed <factor>] [--seconds <n>]
  pitot-bench collect (--pid <n> | --name <exe>...) [--seconds <n>] [--interval-ms <n>] [--out <dir>]";

#[derive(Debug, Default)]
struct ReplayArgs {
    irdt: Option<String>,
    ibt: Option<String>,
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
    match (&parsed.irdt, &parsed.ibt) {
        (Some(_), None) | (None, Some(_)) => Ok(parsed),
        _ => Err("give exactly one of --irdt or --ibt".to_owned()),
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
    match (args.irdt, args.ibt) {
        (Some(path), _) => play(IrdtSource::open(path)?, &options),
        (_, Some(path)) => play(IbtSource::open(path)?, &options),
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

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("replay") => parse_replay(args).map_err(Into::into).and_then(replay),
        Some("collect") => parse_collect(args).map_err(Into::into).and_then(collect),
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
}
