//! Overlay configuration: a TOML file in the user's config directory, created with defaults on first run and
//! reloaded when it changes. A file that fails to parse or validate leaves the previous configuration in place.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

/// The single source of truth for where configuration lives: `%APPDATA%\Pitot`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Pitot"))
}

/// `config.toml` inside [`config_dir`].
pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.toml"))
}

/// The current run's log, inside [`config_dir`].
pub fn log_file() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("pitot.log"))
}

/// The previous run's log, inside [`config_dir`].
pub fn previous_log_file() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("pitot.previous.log"))
}

/// What the pit widget learns (refueling speed by car, pit lane loss by track), inside [`config_dir`].
pub fn pit_file() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("pit.toml"))
}

/// Where the overlay records sessions: `captures` inside [`config_dir`].
pub fn recordings_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("captures"))
}

/// Folders searched for the sim's telemetry files (`.ibt`) of the session while it plays a replay: the configured
/// folder, or the sim's own `Documents\iRacing\telemetry`.
pub fn telemetry_dirs(telemetry: &Telemetry, documents: Option<PathBuf>) -> Vec<PathBuf> {
    if telemetry.folder.trim().is_empty() {
        documents
            .map(|dir| dir.join("iRacing").join("telemetry"))
            .into_iter()
            .collect()
    } else {
        vec![PathBuf::from(telemetry.folder.trim())]
    }
}

/// Where the track map keeps the outlines it recorded, one file per track: `tracks` inside [`config_dir`].
pub fn tracks_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("tracks"))
}

/// Folders searched for a capture of the session while the sim plays a replay: [`recordings_dir`], and
/// `captures` next to the overlay executable, where `pitot-bench record` run from that folder writes them.
pub fn capture_dirs() -> Vec<PathBuf> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("captures")));
    let in_config = recordings_dir();
    let mut dirs: Vec<PathBuf> = beside_exe.into_iter().chain(in_config).collect();
    dirs.dedup();
    dirs
}

/// Written on first run; kept in sync with [`Config::default`] by a test.
pub const DEFAULT_TOML: &str = r#"# Pitot overlay configuration. Changes apply when the file is saved.
#
# Positions are in physical pixels from the top-left corner of the widget's monitor.
# `monitor` counts monitors from the left (0 is the leftmost); leave it out to use the primary monitor.
# *Move widgets* in the tray menu places widgets with the mouse and saves their x and y here.

[general]
font = "Segoe UI"

# Records each session while you drive into %APPDATA%\Pitot\captures, so a replay of it (in the session or
# from a saved .rpy file) can show the widgets. The oldest recordings are deleted once the folder holds more
# than max_gb gigabytes (about 0.23 GB per hour). Changes apply at the next start.
[recording]
enabled = true
max_gb = 5.0

# While the sim plays a replay, the tire widget shows surface temperatures from the sim's own telemetry files
# (.ibt), which the sim writes while its telemetry recording is on. Leave folder empty for
# Documents\iRacing\telemetry.
[telemetry]
folder = ""

[widgets.status]
enabled = true
x = 24
y = 24
opacity = 0.82

[widgets.inputs]
enabled = true
x = 660
y = 900
width = 600
height = 120
rate_hz = 60.0
opacity = 0.82

[widgets.relative]
enabled = true
x = 1460
y = 560
width = 460
height = 300
rate_hz = 10.0
opacity = 0.82

[widgets.standings]
enabled = true
x = 24
y = 260
width = 760
height = 560
rate_hz = 2.0
opacity = 0.82

[widgets.fuel]
enabled = true
x = 1460
y = 900
width = 420
height = 150
rate_hz = 1.0
opacity = 0.82

# Other cars' pit stops: standing time, the fuel it allows, and the pit lane loss. See docs/pit.md.
[widgets.pit]
enabled = true
x = 800
y = 240
width = 400
height = 280
rate_hz = 10.0
opacity = 0.82

# The player's time in each sector against their best, the class's best, and the cars ahead and behind.
[widgets.sectors]
enabled = true
x = 800
y = 24
width = 400
height = 200
rate_hz = 5.0
opacity = 0.82

[widgets.wind]
enabled = true
x = 1460
y = 370
width = 200
height = 170
rate_hz = 5.0
opacity = 0.82

# Wear and carcass temperatures are from the last pit stall visit: the sim updates them only there. In replays,
# surface temperatures from the sim's telemetry files.
[widgets.tires]
enabled = true
x = 1680
y = 370
width = 220
height = 170
rate_hz = 5.0
opacity = 0.82

# The track map. shape = "circle" places the cars around a circle by lap distance, from the first moment of a
# session. shape = "outline" draws the track's outline once you have driven one clean lap of it, and keeps it in
# %APPDATA%\Pitot\tracks for later sessions.
[widgets.trackmap]
enabled = true
shape = "circle"
x = 1460
y = 24
width = 440
height = 330
rate_hz = 10.0
opacity = 0.82
"#;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub general: General,
    pub recording: Recording,
    pub telemetry: Telemetry,
    pub widgets: Widgets,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub font: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            font: "Segoe UI".to_owned(),
        }
    }
}

/// Recording sessions for replays.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recording {
    pub enabled: bool,
    /// Gigabytes the recordings may take before the oldest are deleted.
    pub max_gb: f32,
}

impl Default for Recording {
    fn default() -> Self {
        Self {
            enabled: true,
            max_gb: 5.0,
        }
    }
}

/// The sim's own telemetry files, for replays.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Telemetry {
    /// Folder of the `.ibt` files; empty for the sim's default.
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Widgets {
    pub status: WidgetConfig,
    pub inputs: WidgetConfig,
    pub relative: WidgetConfig,
    pub standings: WidgetConfig,
    pub fuel: WidgetConfig,
    pub pit: WidgetConfig,
    pub sectors: WidgetConfig,
    pub wind: WidgetConfig,
    pub tires: WidgetConfig,
    pub trackmap: WidgetConfig,
}

impl Default for Widgets {
    fn default() -> Self {
        let widget = |x, y, size: Option<(u32, u32)>, rate_hz: Option<f32>| WidgetConfig {
            enabled: true,
            monitor: None,
            x,
            y,
            width: size.map(|s| s.0),
            height: size.map(|s| s.1),
            rate_hz,
            opacity: 0.82,
            shape: None,
        };
        Self {
            status: widget(24, 24, None, None),
            inputs: widget(660, 900, Some((600, 120)), Some(60.0)),
            relative: widget(1460, 560, Some((460, 300)), Some(10.0)),
            standings: widget(24, 260, Some((760, 560)), Some(2.0)),
            fuel: widget(1460, 900, Some((420, 150)), Some(1.0)),
            pit: widget(800, 240, Some((400, 280)), Some(10.0)),
            sectors: widget(800, 24, Some((400, 200)), Some(5.0)),
            wind: widget(1460, 370, Some((200, 170)), Some(5.0)),
            tires: widget(1680, 370, Some((220, 170)), Some(5.0)),
            trackmap: WidgetConfig {
                shape: Some(MapShape::Circle),
                ..widget(1460, 24, Some((440, 330)), Some(10.0))
            },
        }
    }
}

/// One widget's placement and behavior.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetConfig {
    pub enabled: bool,
    /// Monitor index from the left; `None` is the primary monitor.
    pub monitor: Option<usize>,
    pub x: i32,
    pub y: i32,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Update rate; `None` uses the widget's own rate.
    pub rate_hz: Option<f32>,
    /// Background opacity, 0 to 1.
    pub opacity: f32,
    /// The track map's shape; other widgets have none.
    pub shape: Option<MapShape>,
}

/// How the track map draws the track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MapShape {
    /// Cars around a circle by lap distance; needs no lap.
    #[default]
    Circle,
    /// The track's outline, recorded from a clean lap of the player's.
    Outline,
}

/// The file's shape: every field optional, applied over the defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    general: General,
    recording: Recording,
    telemetry: Telemetry,
    widgets: FileWidgets,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileWidgets {
    status: WidgetPatch,
    inputs: WidgetPatch,
    relative: WidgetPatch,
    standings: WidgetPatch,
    fuel: WidgetPatch,
    pit: WidgetPatch,
    sectors: WidgetPatch,
    wind: WidgetPatch,
    tires: WidgetPatch,
    trackmap: WidgetPatch,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WidgetPatch {
    enabled: Option<bool>,
    monitor: Option<usize>,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
    rate_hz: Option<f32>,
    opacity: Option<f32>,
    shape: Option<MapShape>,
}

impl WidgetPatch {
    fn apply(self, mut config: WidgetConfig) -> WidgetConfig {
        config.enabled = self.enabled.unwrap_or(config.enabled);
        config.monitor = self.monitor.or(config.monitor);
        config.x = self.x.unwrap_or(config.x);
        config.y = self.y.unwrap_or(config.y);
        config.width = self.width.or(config.width);
        config.height = self.height.or(config.height);
        config.rate_hz = self.rate_hz.or(config.rate_hz);
        config.opacity = self.opacity.unwrap_or(config.opacity);
        config.shape = self.shape.or(config.shape);
        config
    }
}

/// Errors from loading the configuration.
#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    /// The file is not valid TOML or does not match the schema; the message includes the location.
    Parse(String),
    /// A value is outside its allowed range.
    Invalid {
        widget: &'static str,
        field: &'static str,
    },
    /// A widget's position cannot be written into the file without changing more than its position.
    Unplaceable,
    /// A setting outside the widgets is outside its allowed range.
    OutOfRange {
        key: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read the configuration: {e}"),
            Self::Parse(message) => write!(f, "invalid configuration: {message}"),
            Self::Invalid { widget, field } => write!(
                f,
                "invalid configuration: widgets.{widget}.{field} is out of range"
            ),
            Self::Unplaceable => f.write_str(
                "cannot save the position: write the widget as a [widgets.<name>] table with x and y",
            ),
            Self::OutOfRange { key } => write!(f, "invalid configuration: {key} is out of range"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl Config {
    /// Parses and validates configuration text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let file: FileConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let defaults = Widgets::default();
        let w = file.widgets;
        let config = Self {
            general: file.general,
            recording: file.recording,
            telemetry: file.telemetry,
            widgets: Widgets {
                status: w.status.apply(defaults.status),
                inputs: w.inputs.apply(defaults.inputs),
                relative: w.relative.apply(defaults.relative),
                standings: w.standings.apply(defaults.standings),
                fuel: w.fuel.apply(defaults.fuel),
                pit: w.pit.apply(defaults.pit),
                sectors: w.sectors.apply(defaults.sectors),
                wind: w.wind.apply(defaults.wind),
                tires: w.tires.apply(defaults.tires),
                trackmap: w.trackmap.apply(defaults.trackmap),
            },
        };
        config.validate()?;
        Ok(config)
    }

    /// Widget configurations by name, in a fixed order.
    pub fn widgets(&self) -> [(&'static str, &WidgetConfig); 10] {
        let w = &self.widgets;
        [
            ("status", &w.status),
            ("inputs", &w.inputs),
            ("relative", &w.relative),
            ("standings", &w.standings),
            ("fuel", &w.fuel),
            ("pit", &w.pit),
            ("sectors", &w.sectors),
            ("wind", &w.wind),
            ("tires", &w.tires),
            ("trackmap", &w.trackmap),
        ]
    }

    /// The configuration of the widget called `name`, as listed by [`Config::widgets`].
    pub fn widget_mut(&mut self, name: &str) -> Option<&mut WidgetConfig> {
        let w = &mut self.widgets;
        match name {
            "status" => Some(&mut w.status),
            "inputs" => Some(&mut w.inputs),
            "relative" => Some(&mut w.relative),
            "standings" => Some(&mut w.standings),
            "fuel" => Some(&mut w.fuel),
            "pit" => Some(&mut w.pit),
            "sectors" => Some(&mut w.sectors),
            "wind" => Some(&mut w.wind),
            "tires" => Some(&mut w.tires),
            "trackmap" => Some(&mut w.trackmap),
            _ => None,
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !(0.1..=10_000.0).contains(&self.recording.max_gb) {
            return Err(ConfigError::OutOfRange {
                key: "recording.max_gb",
            });
        }
        for (widget, c) in self.widgets() {
            let invalid = |field| Err(ConfigError::Invalid { widget, field });
            if c.shape.is_some() && widget != "trackmap" {
                return invalid("shape");
            }
            if c.rate_hz.is_some_and(|r| !(0.1..=120.0).contains(&r)) {
                return invalid("rate_hz");
            }
            if !(0.0..=1.0).contains(&c.opacity) {
                return invalid("opacity");
            }
            if c.width.is_some_and(|w| !(16..=8192).contains(&w)) {
                return invalid("width");
            }
            if c.height.is_some_and(|h| !(16..=8192).contains(&h)) {
                return invalid("height");
            }
        }
        Ok(())
    }
}

/// Reads `path`, writing the default file first when it does not exist.
pub fn load_or_create(path: &Path) -> Result<Config, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Config::parse(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(path, DEFAULT_TOML)?;
            Config::parse(DEFAULT_TOML)
        }
        Err(e) => Err(e.into()),
    }
}

/// Returns `text` with widget `name` placed at (`x`, `y`). Only the numbers of its `x` and `y` keys change, so
/// comments and layout stay as they are. A missing key goes on the line after the widget's `[widgets.<name>]`
/// header, and a missing table is added at the end. Fails when the widget is written in a form this cannot
/// edit in place (an inline table or dotted keys) or when `text` does not parse.
pub fn with_position(text: &str, name: &str, x: i32, y: i32) -> Result<String, ConfigError> {
    let before = Config::parse(text)?;
    let parse_error = |e: toml::de::Error| ConfigError::Parse(e.to_string());
    let document = toml::de::DeTable::parse(text).map_err(parse_error)?;
    let widget = match document.get_ref().get("widgets").map(|w| w.get_ref()) {
        Some(toml::de::DeValue::Table(widgets)) => widgets.get_key_value(name),
        _ => None,
    };
    // Edits as (byte range, replacement), applied from the end so earlier ranges stay valid.
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    match widget {
        Some((key, value)) => {
            let toml::de::DeValue::Table(table) = value.get_ref() else {
                return Err(ConfigError::Unplaceable);
            };
            let mut missing = String::new();
            for (field, number) in [("x", x), ("y", y)] {
                match table.get(field) {
                    Some(old) => edits.push((old.span(), number.to_string())),
                    None => missing.push_str(&format!("{field} = {number}\n")),
                }
            }
            if !missing.is_empty() {
                // Only a `[widgets.<name>]` header line can take new keys on the line below it.
                let line_start = text[..key.span().start].rfind('\n').map_or(0, |i| i + 1);
                if !text[line_start..].trim_start().starts_with('[') {
                    return Err(ConfigError::Unplaceable);
                }
                let line_end = text[key.span().end..]
                    .find('\n')
                    .map_or(text.len(), |i| key.span().end + i + 1);
                if line_end == text.len() && !text.ends_with('\n') {
                    missing.insert(0, '\n');
                }
                edits.push((line_end..line_end, missing));
            }
        }
        None => {
            let gap = if text.is_empty() || text.ends_with("\n\n") {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            edits.push((
                text.len()..text.len(),
                format!("{gap}[widgets.{name}]\nx = {x}\ny = {y}\n"),
            ));
        }
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_owned();
    for (range, replacement) in edits {
        out.replace_range(range, &replacement);
    }
    // The edit must place the widget and change nothing else.
    let after = Config::parse(&out)?;
    let mut expected = before;
    if let Some(widget) = expected.widget_mut(name) {
        widget.x = x;
        widget.y = y;
    }
    if after != expected {
        return Err(ConfigError::Unplaceable);
    }
    Ok(out)
}

/// Saves widget `name` at (`x`, `y`) in the file at `path` (see [`with_position`]). Writes a temporary file
/// next to it first and moves it over the old one, so a failed write leaves the old file.
pub fn save_position(path: &Path, name: &str, x: i32, y: i32) -> Result<(), ConfigError> {
    let text = fs::read_to_string(path)?;
    let placed = with_position(&text, name, x, y)?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, placed)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

/// When a file was last written and how long it is. The configuration directory also holds the log, and a change
/// notification on the directory does not say which file changed; comparing stamps tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// The file's stamp, or `None` if it cannot be read (for example, it does not exist).
pub fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    Some(Stamp {
        modified: meta.modified().ok(),
        len: meta.len(),
    })
}

/// Orders monitors for the `monitor` setting: left to right, then top to bottom. Returns indices into `rects`,
/// where each rect is `(x, y)` of the monitor's top-left corner.
pub fn monitor_order(rects: &[(i32, i32)]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by_key(|&i| (rects[i].0, rects[i].1));
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_matches_defaults() {
        assert_eq!(
            Config::parse(DEFAULT_TOML).expect("parses"),
            Config::default()
        );
    }

    #[test]
    fn benchmark_configurations_parse() {
        let four =
            Config::parse(include_str!("../../../benchmarks/pitot/four.toml")).expect("four");
        let enabled = |c: &Config| c.widgets().iter().filter(|(_, w)| w.enabled).count();
        assert_eq!(enabled(&four), 4);
        assert!(!four.widgets.status.enabled);
        let defaults = Widgets::default();
        assert_eq!(
            four.widgets.inputs, defaults.inputs,
            "default positions and rates"
        );
        assert_eq!(four.widgets.standings, defaults.standings);
        let inputs =
            Config::parse(include_str!("../../../benchmarks/pitot/inputs.toml")).expect("inputs");
        assert_eq!(enabled(&inputs), 1);
        assert_eq!(inputs.widgets.inputs, defaults.inputs);
    }

    #[test]
    fn recording_is_on_by_default_and_checked() {
        let config = Config::parse("").expect("parses");
        assert!(config.recording.enabled);
        assert_eq!(config.recording.max_gb, 5.0);
        let off = Config::parse("[recording]\nenabled = false\n").expect("parses");
        assert!(!off.recording.enabled);
        assert!(matches!(
            Config::parse("[recording]\nmax_gb = 0.0\n"),
            Err(ConfigError::OutOfRange { .. })
        ));
    }

    #[test]
    fn missing_fields_keep_defaults() {
        let config = Config::parse("[widgets.fuel]\nx = 10\nmonitor = 2\n").expect("parses");
        assert_eq!(
            (config.widgets.fuel.x, config.widgets.fuel.y),
            (10, 900),
            "y keeps the fuel default"
        );
        assert_eq!(config.widgets.fuel.width, Some(420));
        assert_eq!(config.widgets.fuel.monitor, Some(2));
        assert!(config.widgets.fuel.enabled);
        assert_eq!(config.widgets.fuel.opacity, 0.82);
        assert_eq!(config.widgets.relative, Widgets::default().relative);
        assert_eq!(Config::parse("").expect("empty file"), Config::default());
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        assert!(matches!(
            Config::parse("[widgets.status]\ncolour = 1\n"),
            Err(ConfigError::Parse(_))
        ));
        assert!(matches!(
            Config::parse("[widgets.radar]\n"),
            Err(ConfigError::Parse(_))
        ));
        assert!(matches!(
            Config::parse("[widgets.inputs]\nrate_hz = 500.0\n"),
            Err(ConfigError::Invalid {
                widget: "inputs",
                field: "rate_hz"
            })
        ));
        assert!(matches!(
            Config::parse("[widgets.fuel]\nopacity = 2.0\n"),
            Err(ConfigError::Invalid {
                widget: "fuel",
                field: "opacity"
            })
        ));
        let error = Config::parse("[widgets.status]\nx = \"left\"\n").expect_err("wrong type");
        assert!(
            error.to_string().contains("line"),
            "parse errors carry a location: {error}"
        );
    }

    #[test]
    fn creates_the_default_file_on_first_run() {
        let dir = std::env::temp_dir().join(format!("pitot-config-test-{}", std::process::id()));
        let path = dir.join("nested").join("config.toml");
        assert_eq!(load_or_create(&path).expect("creates"), Config::default());
        assert_eq!(fs::read_to_string(&path).expect("written"), DEFAULT_TOML);
        fs::write(&path, "[widgets.status]\nenabled = false\n").expect("edits");
        assert!(!load_or_create(&path).expect("reads").widgets.status.enabled);
        fs::remove_dir_all(&dir).expect("cleans up");
    }

    #[test]
    fn stamp_changes_only_with_its_own_file() {
        let dir = std::env::temp_dir().join(format!("pitot-stamp-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("creates");
        let (path, log) = (dir.join("config.toml"), dir.join("pitot.log"));
        assert_eq!(stamp(&path), None, "missing file");
        fs::write(&path, DEFAULT_TOML).expect("writes");
        let first = stamp(&path).expect("stamp");
        fs::write(&log, "a log line\n").expect("writes the log");
        assert_eq!(stamp(&path), Some(first), "another file changed");
        fs::write(&path, "[widgets.status]\nenabled = false\n").expect("edits");
        assert_ne!(stamp(&path), Some(first), "the file itself changed");
        fs::remove_dir_all(&dir).expect("cleans up");
    }

    #[test]
    fn saving_a_position_changes_only_its_numbers() {
        let placed = with_position(DEFAULT_TOML, "standings", 1200, -40).expect("placed");
        let config = Config::parse(&placed).expect("parses");
        assert_eq!(
            (config.widgets.standings.x, config.widgets.standings.y),
            (1200, -40)
        );
        let changed: Vec<(&str, &str)> = DEFAULT_TOML
            .lines()
            .zip(placed.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(changed, [("x = 24", "x = 1200"), ("y = 260", "y = -40")]);
        assert_eq!(DEFAULT_TOML.lines().count(), placed.lines().count());

        let commented = "[widgets.fuel]\nx = 5 # left edge\ny = 6\n";
        assert_eq!(
            with_position(commented, "fuel", 50, 60).expect("placed"),
            "[widgets.fuel]\nx = 50 # left edge\ny = 60\n"
        );
        let inline = "[widgets]\nfuel = { x = 1, y = 2 }\n";
        assert_eq!(
            with_position(inline, "fuel", 3, 4).expect("placed"),
            "[widgets]\nfuel = { x = 3, y = 4 }\n"
        );
    }

    #[test]
    fn saving_a_position_adds_missing_keys_and_tables() {
        let place = |text: &str, name: &str| with_position(text, name, 7, 8);
        assert_eq!(
            place("[widgets.fuel]\nenabled = true\n", "fuel").expect("placed"),
            "[widgets.fuel]\nx = 7\ny = 8\nenabled = true\n"
        );
        assert_eq!(
            place("[widgets.fuel]\ny = 2\n", "fuel").expect("placed"),
            "[widgets.fuel]\nx = 7\ny = 8\n"
        );
        assert_eq!(
            place("[widgets.fuel]", "fuel").expect("placed"),
            "[widgets.fuel]\nx = 7\ny = 8\n"
        );
        assert_eq!(
            place("[general]\nfont = \"Arial\"", "relative").expect("placed"),
            "[general]\nfont = \"Arial\"\n\n[widgets.relative]\nx = 7\ny = 8\n"
        );
        assert_eq!(
            place("", "relative").expect("placed"),
            "[widgets.relative]\nx = 7\ny = 8\n"
        );
        assert!(matches!(
            place("[widgets]\nfuel = { enabled = true }\n", "fuel"),
            Err(ConfigError::Unplaceable)
        ));
        assert!(matches!(
            place("[widgets.fuel\n", "fuel"),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn saved_positions_replace_the_file() {
        let dir = std::env::temp_dir().join(format!("pitot-position-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_TOML).expect("write");
        save_position(&path, "fuel", 10, 20).expect("saved");
        let config = load_or_create(&path).expect("loads");
        assert_eq!((config.widgets.fuel.x, config.widgets.fuel.y), (10, 20));
        assert!(!path.with_extension("toml.tmp").exists());
        fs::remove_dir_all(&dir).expect("cleans up");
    }

    #[test]
    fn monitors_are_ordered_left_to_right() {
        // Lab PC layout: primary at 0, one to its right, a portrait one further right and higher.
        assert_eq!(monitor_order(&[(1920, 0), (3840, -383), (0, 0)]), [2, 0, 1]);
        assert_eq!(monitor_order(&[(-1920, 0), (0, 0)]), [0, 1]);
    }
}
