//! File logging: one line per `tracing` event in `pitot.log` next to the configuration, the previous run's log
//! kept as `pitot.previous.log`. Debug builds also print to the console.
//!
//! Only events are written; spans are not used. Nothing is logged per tick or per frame, and `debug` and `trace`
//! events compile out of release builds (`release_max_level_info`).

use std::fmt::{self, Write as _};
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::PathBuf;

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::config;
use crate::platform;

/// Most verbose level written.
const MAX_LEVEL: Level = if cfg!(debug_assertions) {
    Level::DEBUG
} else {
    Level::INFO
};

/// Starts logging for this process: keeps the previous log, installs the subscriber, and logs panics. Returns
/// the log file's path when it could be created. Call once, before anything else logs.
pub fn init() -> Option<PathBuf> {
    let path = config::log_file();
    let file = path.as_deref().and_then(|path| {
        fs::create_dir_all(path.parent()?).ok()?;
        if let Some(previous) = config::previous_log_file() {
            // Fails harmlessly on the first run, when there is no log yet.
            let _ = fs::rename(path, previous);
        }
        File::create(path).ok()
    });
    let created = file.is_some();
    let log = FileLog {
        file,
        console: cfg!(debug_assertions),
    };
    tracing::subscriber::set_global_default(log).ok()?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{info}");
        default_hook(info);
    }));
    path.filter(|_| created)
}

/// Writes each event as one line; a single write per line keeps lines from different threads apart.
struct FileLog {
    file: Option<File>,
    console: bool,
}

impl Subscriber for FileLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= MAX_LEVEL
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::from_level(MAX_LEVEL))
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let line = format_line(platform::local_time(), event);
        if let Some(mut file) = self.file.as_ref() {
            let _ = file.write_all(line.as_bytes());
        }
        if self.console {
            let _ = io::stderr().write_all(line.as_bytes());
        }
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

/// `2026-11-09 14:03:27.512 INFO  pitot_overlay::app: message key=value`
fn format_line(time: platform::LocalTime, event: &Event<'_>) -> String {
    let metadata = event.metadata();
    let mut line = String::with_capacity(128);
    let _ = write!(
        line,
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {:<5} {}:",
        time.year,
        time.month,
        time.day,
        time.hour,
        time.minute,
        time.second,
        time.millis,
        metadata.level(),
        metadata.target(),
    );
    event.record(&mut Fields(&mut line));
    line.push('\n');
    line
}

/// Appends the message, then `name=value` for every other field.
struct Fields<'a>(&'a mut String);

impl Fields<'_> {
    fn push(&mut self, field: &Field, value: fmt::Arguments<'_>) {
        let _ = match field.name() {
            "message" => write!(self.0, " {value}"),
            name => write!(self.0, " {name}={value}"),
        };
    }
}

impl Visit for Fields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, format_args!("{value}"));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format_args!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_one_line_per_event() {
        let path = std::env::temp_dir().join(format!("pitot-log-test-{}.log", std::process::id()));
        let log = FileLog {
            file: Some(File::create(&path).expect("create log")),
            console: false,
        };
        tracing::subscriber::with_default(log, || {
            tracing::info!(monitors = 3, font = "Segoe UI", "overlay started");
            tracing::trace!("below the level");
        });
        let text = fs::read_to_string(&path).expect("read log");
        let _ = fs::remove_file(&path);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let (time, rest) = lines[0].split_at(23);
        assert_eq!(time.as_bytes()[4], b'-', "{time}");
        assert_eq!(time.as_bytes()[19], b'.', "{time}");
        assert_eq!(
            rest,
            " INFO  pitot_overlay::log::tests: overlay started monitors=3 font=Segoe UI"
        );
    }
}
