use std::process::ExitCode;

#[cfg(windows)]
use pitot_bench::replay::{self, FrameSource, IbtSource, IrdtSource, ReplayOptions};

const USAGE: &str = "usage: pitot-bench replay (--irdt <file> | --ibt <file>) [--loop] [--speed <factor>] [--seconds <n>]";

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

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("replay") => parse_replay(args).map_err(Into::into).and_then(replay),
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
