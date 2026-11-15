//! Overlay configuration: a TOML file in the user's config directory, created with defaults on first run and
//! reloaded when it changes. A file that fails to parse or validate leaves the previous configuration in place.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::de::{self, DeserializeOwned, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

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

/// Where the Pitot window (`pitot.exe --window`) opens next, kept when it closes, inside [`config_dir`].
pub fn window_file() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("window.toml"))
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

/// Written on first run; kept in sync with [`Config::default`] by a test. Widgets missing here (every widget added
/// after the first ten, off by default) take their defaults from the registry.
pub const DEFAULT_TOML: &str = r#"# Pitot overlay configuration. Changes apply when the file is saved.
#
# Positions are in physical pixels from the top-left corner of the widget's monitor.
# `monitor` counts monitors from the left (0 is the leftmost); leave it out to use the primary monitor.
# *Move widgets* in the tray menu places and resizes widgets with the mouse and saves their x, y, width, and
# height here. The Pitot window turns widgets on and off and saves their `enabled` here.

# The Pitot window opens when the overlay starts. With start_in_tray = true, only the tray icon shows; clicking
# it opens the window.
# background_opacity sets how opaque the widgets' backgrounds are, from 0 (none) to 100 (opaque); text and
# graphs stay as they are. At 82 each widget uses its own opacity below. The Pitot window's slider saves it here.
[general]
font = "Segoe UI"
start_in_tray = false
background_opacity = 82

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
rate_hz = 30.0
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
    /// Start without opening the Pitot window; the tray icon opens it.
    pub start_in_tray: bool,
    /// How opaque the widgets' backgrounds are, in percent: see [`background_alpha`].
    pub background_opacity: u8,
}

impl Default for General {
    fn default() -> Self {
        Self {
            font: "Segoe UI".to_owned(),
            start_in_tray: false,
            background_opacity: DEFAULT_BACKGROUND_OPACITY,
        }
    }
}

/// `[general] background_opacity` when the file leaves it out: every widget's background at its own `opacity`.
pub const DEFAULT_BACKGROUND_OPACITY: u8 = 82;

/// The highest `[general] background_opacity`: opaque backgrounds.
pub const MAX_BACKGROUND_OPACITY: u8 = 100;

/// The alpha of a background fill whose own alpha is `base` (a widget's `opacity`, or a band drawn over its panel)
/// at `[general] background_opacity` = `percent`. At [`DEFAULT_BACKGROUND_OPACITY`] it is `base`; below, it falls
/// in proportion to 0 at 0; above, it rises in proportion to 1 at 100. A widget at the default `opacity` (0.82)
/// gets the percent itself as its panel's alpha. Percents above 100 count as 100.
pub fn background_alpha(base: f32, percent: u8) -> f32 {
    let base = base.clamp(0.0, 1.0);
    let percent = percent.min(MAX_BACKGROUND_OPACITY);
    // Exactly `base`, so the default draws as it did before the setting existed.
    if percent == DEFAULT_BACKGROUND_OPACITY {
        return base;
    }
    let (p, d) = (f32::from(percent), f32::from(DEFAULT_BACKGROUND_OPACITY));
    let max = f32::from(MAX_BACKGROUND_OPACITY);
    if p < d {
        base * p / d
    } else {
        base + (1.0 - base) * (p - d) / (max - d)
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

/// Every widget, in the order of the registry (`crate::widgets`, the list in `widgets/list.rs`). Without the
/// widgets (outside Windows) there are none.
pub fn widget_specs() -> &'static [Spec] {
    #[cfg(windows)]
    {
        crate::widgets::SPECS
    }
    #[cfg(not(windows))]
    {
        &[]
    }
}

/// The fields every widget's table has; a widget declares any others itself.
const COMMON_FIELDS: &str = "enabled, monitor, x, y, width, height, rate_hz, opacity";

/// A widget as the registry lists it: its name in the file, its label in the Pitot window, and its defaults.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    /// The name in `config.toml` (`[widgets.<name>]`), which is also the widget's module.
    pub name: &'static str,
    /// The name the Pitot window shows.
    pub label: &'static str,
    /// On by default. A widget is off unless its entry turns it on.
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `None` uses the widget's own rate.
    pub rate_hz: Option<f32>,
    pub opacity: f32,
    /// Checks the widget's own fields and returns them with every default filled in.
    options: fn(&toml::Table) -> Result<toml::Table, String>,
}

impl Spec {
    /// A widget at (`x`, `y`) on the primary monitor, with its own size and rate, off by default.
    pub const fn new(label: &'static str, x: i32, y: i32) -> Self {
        Self {
            name: "",
            label,
            enabled: false,
            x,
            y,
            width: None,
            height: None,
            rate_hz: None,
            opacity: 0.82,
            options: normalize_options::<NoOptions>,
        }
    }

    /// The default size in pixels.
    pub const fn size(self, width: u32, height: u32) -> Self {
        Self {
            width: Some(width),
            height: Some(height),
            ..self
        }
    }

    /// The default update rate.
    pub const fn rate(self, hz: f32) -> Self {
        Self {
            rate_hz: Some(hz),
            ..self
        }
    }

    /// On by default.
    pub const fn on(self) -> Self {
        Self {
            enabled: true,
            ..self
        }
    }

    /// The name, set by the registry from the widget's module.
    pub(crate) const fn named(self, name: &'static str) -> Self {
        Self { name, ..self }
    }

    /// The widget's own fields, set by the registry: see [`normalize_options`].
    pub(crate) const fn with_options(
        self,
        options: fn(&toml::Table) -> Result<toml::Table, String>,
    ) -> Self {
        Self { options, ..self }
    }

    /// The widget's configuration when the file does not mention it.
    pub fn defaults(&self) -> WidgetConfig {
        WidgetConfig {
            enabled: self.enabled,
            monitor: None,
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            rate_hz: self.rate_hz,
            opacity: self.opacity,
            options: (self.options)(&toml::Table::new()).unwrap_or_default(),
        }
    }
}

/// A widget without fields of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoOptions {}

/// Reads a widget's own fields as `T` (a struct with `#[serde(default, deny_unknown_fields)]`) and returns them
/// as written back from `T`, so every field is there and two tables are equal when their values are.
pub fn normalize_options<T: DeserializeOwned + Serialize + Default>(
    table: &toml::Table,
) -> Result<toml::Table, String> {
    let typed: T = table.clone().try_into().map_err(|e| {
        let message = e.to_string().trim_end().replace('\n', " ");
        if message.contains("there are no fields") {
            message.replace(
                "there are no fields",
                &format!("expected one of {COMMON_FIELDS}"),
            )
        } else if message.contains("unknown field") {
            format!("{message} (every widget also has {COMMON_FIELDS})")
        } else {
            message
        }
    })?;
    toml::Table::try_from(typed).map_err(|e| e.to_string())
}

/// Every widget's configuration, in the registry's order.
#[derive(Debug, Clone, PartialEq)]
pub struct Widgets(Vec<WidgetConfig>);

impl Default for Widgets {
    fn default() -> Self {
        Self(widget_specs().iter().map(Spec::defaults).collect())
    }
}

impl Widgets {
    /// Widget configurations by name, in the registry's order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &WidgetConfig)> {
        widget_specs().iter().map(|s| s.name).zip(&self.0)
    }

    pub fn get(&self, name: &str) -> Option<&WidgetConfig> {
        self.0.get(spec_index(name)?)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut WidgetConfig> {
        self.0.get_mut(spec_index(name)?)
    }
}

fn spec_index(name: &str) -> Option<usize> {
    widget_specs().iter().position(|s| s.name == name)
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
    /// The widget's own fields (such as the track map's `shape`), every one present: see [`normalize_options`].
    pub options: toml::Table,
}

impl WidgetConfig {
    /// The widget's own fields as `T`. They were checked when the file was read; a table that does not fit `T`
    /// gives its defaults.
    pub fn options_as<T: DeserializeOwned + Default>(&self) -> T {
        self.options.clone().try_into().unwrap_or_default()
    }
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

/// The `[widgets]` table: patches by the widget's index in the registry. A name the registry does not have is an
/// error at its place in the file.
#[derive(Debug, Default)]
struct FileWidgets(Vec<(usize, WidgetPatch)>);

impl<'de> Deserialize<'de> for FileWidgets {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Tables;

        impl<'de> Visitor<'de> for Tables {
            type Value = FileWidgets;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table of widgets")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<FileWidgets, A::Error> {
                let mut patches = Vec::new();
                while let Some(WidgetName(index)) = map.next_key()? {
                    patches.push((index, map.next_value()?));
                }
                Ok(FileWidgets(patches))
            }
        }

        deserializer.deserialize_map(Tables)
    }
}

/// A widget's name, as its index in the registry.
struct WidgetName(usize);

impl<'de> Deserialize<'de> for WidgetName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        spec_index(&name).map(WidgetName).ok_or_else(|| {
            let known: Vec<&str> = widget_specs().iter().map(|s| s.name).collect();
            de::Error::custom(format!(
                "unknown widget `{name}`, expected one of {}",
                known.join(", ")
            ))
        })
    }
}

/// A widget's table: the fields every widget has, and the widget's own, which its registry entry checks.
#[derive(Debug, Default, Deserialize)]
#[serde(default, expecting = "a widget's table")]
struct WidgetPatch {
    enabled: Option<bool>,
    monitor: Option<usize>,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
    rate_hz: Option<f32>,
    opacity: Option<f32>,
    #[serde(flatten)]
    options: toml::Table,
}

impl WidgetPatch {
    fn apply(self, config: &mut WidgetConfig, spec: &Spec) -> Result<(), String> {
        config.enabled = self.enabled.unwrap_or(config.enabled);
        config.monitor = self.monitor.or(config.monitor);
        config.x = self.x.unwrap_or(config.x);
        config.y = self.y.unwrap_or(config.y);
        config.width = self.width.or(config.width);
        config.height = self.height.or(config.height);
        config.rate_hz = self.rate_hz.or(config.rate_hz);
        config.opacity = self.opacity.unwrap_or(config.opacity);
        if !self.options.is_empty() {
            config.options = (spec.options)(&self.options)?;
        }
        Ok(())
    }
}

/// The line of widget `name`'s table in `text`, for errors found after parsing.
fn widget_line(text: &str, name: &str) -> Option<usize> {
    let document = toml::de::DeTable::parse(text).ok()?;
    let toml::de::DeValue::Table(widgets) = document.get_ref().get("widgets")?.get_ref() else {
        return None;
    };
    let (key, _) = widgets.get_key_value(name)?;
    Some(text[..key.span().start].matches('\n').count() + 1)
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
    /// A widget's position or `enabled`, or the background opacity, cannot be written into the file without changing
    /// more than that.
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
                "cannot change the file in place: write the table with its own header, such as \
                 [widgets.<name>] or [general]",
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
        let mut widgets = Widgets::default();
        for (index, patch) in file.widgets.0 {
            let (Some(spec), Some(widget)) = (widget_specs().get(index), widgets.0.get_mut(index))
            else {
                continue;
            };
            patch.apply(widget, spec).map_err(|message| {
                let place = widget_line(text, spec.name)
                    .map(|line| format!(" (line {line})"))
                    .unwrap_or_default();
                ConfigError::Parse(format!("widgets.{}{place}: {message}", spec.name))
            })?;
        }
        let config = Self {
            general: file.general,
            recording: file.recording,
            telemetry: file.telemetry,
            widgets,
        };
        config.validate()?;
        Ok(config)
    }

    /// Widget configurations by name, in the registry's order.
    pub fn widgets(&self) -> impl Iterator<Item = (&'static str, &WidgetConfig)> {
        self.widgets.iter()
    }

    /// The configuration of the widget called `name`.
    pub fn widget(&self, name: &str) -> Option<&WidgetConfig> {
        self.widgets.get(name)
    }

    /// The configuration of the widget called `name`, to change.
    pub fn widget_mut(&mut self, name: &str) -> Option<&mut WidgetConfig> {
        self.widgets.get_mut(name)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.general.background_opacity > MAX_BACKGROUND_OPACITY {
            return Err(ConfigError::OutOfRange {
                key: "general.background_opacity",
            });
        }
        if !(0.1..=10_000.0).contains(&self.recording.max_gb) {
            return Err(ConfigError::OutOfRange {
                key: "recording.max_gb",
            });
        }
        for (widget, c) in self.widgets() {
            let invalid = |field| Err(ConfigError::Invalid { widget, field });
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
/// comments and layout stay as they are. See [`with_fields`] for missing keys and what cannot be edited.
pub fn with_position(text: &str, name: &str, x: i32, y: i32) -> Result<String, ConfigError> {
    with_placement(text, name, Some((x, y)), None)
}

/// Returns `text` with widget `name` at `position` (`x`, `y`) and of `size` (`width`, `height`), each when given.
/// Only the numbers of those keys change, so comments and layout stay as they are. See [`with_fields`] for missing
/// keys and what cannot be edited.
pub fn with_placement(
    text: &str,
    name: &str,
    position: Option<(i32, i32)>,
    size: Option<(u32, u32)>,
) -> Result<String, ConfigError> {
    let mut fields = Vec::with_capacity(4);
    if let Some((x, y)) = position {
        fields.extend([("x", x.to_string()), ("y", y.to_string())]);
    }
    if let Some((width, height)) = size {
        fields.extend([("width", width.to_string()), ("height", height.to_string())]);
    }
    with_fields(text, &["widgets", name], &fields, |config| {
        let Some(widget) = config.widget_mut(name) else {
            return;
        };
        if let Some((x, y)) = position {
            (widget.x, widget.y) = (x, y);
        }
        if let Some((width, height)) = size {
            (widget.width, widget.height) = (Some(width), Some(height));
        }
    })
}

/// Returns `text` with widget `name` turned on or off. Only the value of its `enabled` key changes. See
/// [`with_fields`] for missing keys and what cannot be edited.
pub fn with_enabled(text: &str, name: &str, enabled: bool) -> Result<String, ConfigError> {
    let fields = [("enabled", enabled.to_string())];
    with_fields(text, &["widgets", name], &fields, |config| {
        if let Some(widget) = config.widget_mut(name) {
            widget.enabled = enabled;
        }
    })
}

/// Returns `text` with `[general] background_opacity` set to `percent`, at most 100. Only that value changes; a
/// missing key goes on the line after the `[general]` header, and a missing table is added at the end. See
/// [`with_fields`] for what cannot be edited.
pub fn with_background_opacity(text: &str, percent: u8) -> Result<String, ConfigError> {
    let percent = percent.min(MAX_BACKGROUND_OPACITY);
    let fields = [("background_opacity", percent.to_string())];
    with_fields(text, &["general"], &fields, |config| {
        config.general.background_opacity = percent;
    })
}

/// Returns `text` with the keys in `fields` of the table at `path` (`["widgets", name]`, `["general"]`) set to
/// their TOML values. Only those values change, so comments and layout stay as they are. A missing key goes on the
/// line after the table's header (`[widgets.<name>]`), and a missing table is added at the end. `expect` makes the
/// same change to the parsed configuration, and the edited text must parse to exactly that. Fails when the table
/// is written in a form this cannot edit in place (an inline table without the keys, or dotted keys) or when
/// `text` does not parse.
fn with_fields(
    text: &str,
    path: &[&str],
    fields: &[(&str, String)],
    expect: impl FnOnce(&mut Config),
) -> Result<String, ConfigError> {
    let before = Config::parse(text)?;
    let parse_error = |e: toml::de::Error| ConfigError::Parse(e.to_string());
    let document = toml::de::DeTable::parse(text).map_err(parse_error)?;
    let (name, parents) = path.split_last().ok_or(ConfigError::Unplaceable)?;
    let mut parent = Some(document.get_ref());
    for key in parents {
        parent = match parent.and_then(|t| t.get(*key)).map(|v| v.get_ref()) {
            Some(toml::de::DeValue::Table(table)) => Some(table),
            _ => None,
        };
    }
    let found = parent.and_then(|t| t.get_key_value(*name));
    // Edits as (byte range, replacement), applied from the end so earlier ranges stay valid.
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    match found {
        Some((key, value)) => {
            let toml::de::DeValue::Table(table) = value.get_ref() else {
                return Err(ConfigError::Unplaceable);
            };
            let mut missing = String::new();
            for (field, value) in fields {
                match table.get(*field) {
                    Some(old) => edits.push((old.span(), value.clone())),
                    None => missing.push_str(&format!("{field} = {value}\n")),
                }
            }
            if !missing.is_empty() {
                // Only a table's header line, such as `[widgets.<name>]`, can take new keys on the line below it.
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
            let mut table = format!("{gap}[{}]\n", path.join("."));
            for (field, value) in fields {
                table.push_str(&format!("{field} = {value}\n"));
            }
            edits.push((text.len()..text.len(), table));
        }
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_owned();
    for (range, replacement) in edits {
        out.replace_range(range, &replacement);
    }
    // The edit must make the change and nothing else.
    let after = Config::parse(&out)?;
    let mut expected = before;
    expect(&mut expected);
    if after != expected {
        return Err(ConfigError::Unplaceable);
    }
    Ok(out)
}

/// Saves widget `name` at (`x`, `y`) in the file at `path` (see [`with_position`]).
pub fn save_position(path: &Path, name: &str, x: i32, y: i32) -> Result<(), ConfigError> {
    save_edit(path, |text| with_position(text, name, x, y))
}

/// Saves widget `name`'s position and size, each when given, in the file at `path` (see [`with_placement`]).
pub fn save_placement(
    path: &Path,
    name: &str,
    position: Option<(i32, i32)>,
    size: Option<(u32, u32)>,
) -> Result<(), ConfigError> {
    save_edit(path, |text| with_placement(text, name, position, size))
}

/// Saves widget `name` turned on or off in the file at `path` (see [`with_enabled`]).
pub fn save_enabled(path: &Path, name: &str, enabled: bool) -> Result<(), ConfigError> {
    save_edit(path, |text| with_enabled(text, name, enabled))
}

/// Saves `[general] background_opacity` in the file at `path` (see [`with_background_opacity`]).
pub fn save_background_opacity(path: &Path, percent: u8) -> Result<(), ConfigError> {
    save_edit(path, |text| with_background_opacity(text, percent))
}

/// Rewrites the file at `path` with `edit`. Writes a temporary file next to it first and moves it over the old
/// one, so a failed write leaves the old file.
fn save_edit(
    path: &Path,
    edit: impl FnOnce(&str) -> Result<String, ConfigError>,
) -> Result<(), ConfigError> {
    let text = fs::read_to_string(path)?;
    let edited = edit(&text)?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, edited)?;
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

    fn widget<'a>(config: &'a Config, name: &str) -> &'a WidgetConfig {
        config.widget(name).expect("a widget")
    }

    #[test]
    fn default_file_matches_defaults() {
        assert_eq!(
            Config::parse(DEFAULT_TOML).expect("parses"),
            Config::default()
        );
    }

    /// The first ten widgets' defaults as they were before the registry: on, in this order, at these places,
    /// sizes, and rates.
    #[test]
    fn the_first_widgets_keep_their_defaults() {
        type Defaults = (&'static str, i32, i32, Option<(u32, u32)>, Option<f32>);
        let before: [Defaults; 10] = [
            ("status", 24, 24, None, None),
            ("inputs", 660, 900, Some((600, 120)), Some(60.0)),
            ("relative", 1460, 560, Some((460, 300)), Some(10.0)),
            ("standings", 24, 260, Some((760, 560)), Some(2.0)),
            ("fuel", 1460, 900, Some((420, 150)), Some(30.0)),
            ("pit", 800, 240, Some((400, 280)), Some(10.0)),
            ("sectors", 800, 24, Some((400, 200)), Some(5.0)),
            ("wind", 1460, 370, Some((200, 170)), Some(5.0)),
            ("tires", 1680, 370, Some((220, 170)), Some(5.0)),
            ("trackmap", 1460, 24, Some((440, 330)), Some(10.0)),
        ];
        let config = Config::default();
        let names: Vec<&str> = config.widgets().map(|(name, _)| name).take(10).collect();
        assert_eq!(
            names,
            before.map(|b| b.0),
            "first in the registry, in order"
        );
        for (name, x, y, size, rate_hz) in before {
            let w = widget(&config, name);
            assert!(w.enabled, "{name}");
            assert_eq!((w.x, w.y, w.monitor), (x, y, None), "{name}");
            assert_eq!((w.width, w.height), size.unzip(), "{name}");
            assert_eq!(w.rate_hz, rate_hz, "{name}");
            assert_eq!(w.opacity, 0.82, "{name}");
        }
        let shape: toml::Table = "shape = \"circle\"".parse().expect("table");
        assert_eq!(widget(&config, "trackmap").options, shape);
        assert!(widget(&config, "status").options.is_empty());
        // The Pitot window's labels, as its own list had them.
        let labels: Vec<&str> = widget_specs().iter().map(|s| s.label).take(10).collect();
        assert_eq!(
            labels,
            [
                "Status",
                "Input trace",
                "Relative",
                "Standings",
                "Fuel",
                "Pit stops",
                "Sectors",
                "Wind",
                "Tires",
                "Track map"
            ]
        );
    }

    /// Every widget's own fields have defaults: a table that leaves them out reads.
    #[test]
    fn every_widget_reads_a_table_without_its_own_fields() {
        for spec in widget_specs() {
            assert!(
                (spec.options)(&toml::Table::new()).is_ok(),
                "{}: its options need #[serde(default)]",
                spec.name
            );
        }
    }

    /// A file written by an earlier version, with its edits, reads as it did.
    #[test]
    fn an_older_file_reads_as_before() {
        let older = DEFAULT_TOML
            .replace(
                "[widgets.wind]\nenabled = true",
                "[widgets.wind]\nenabled = false",
            )
            .replace("shape = \"circle\"", "shape = \"outline\"")
            .replace("x = 1460\ny = 560", "x = 1300\ny = 500\nmonitor = 2");
        let config = Config::parse(&older).expect("parses");
        assert!(!widget(&config, "wind").enabled);
        assert_eq!(
            widget(&config, "trackmap").options.get("shape"),
            Some(&toml::Value::String("outline".into()))
        );
        let relative = widget(&config, "relative");
        assert_eq!(
            (relative.x, relative.y, relative.monitor),
            (1300, 500, Some(2))
        );
        let mut expected = Config::default();
        expected.widget_mut("wind").expect("wind").enabled = false;
        let relative = expected.widget_mut("relative").expect("relative");
        (relative.x, relative.y, relative.monitor) = (1300, 500, Some(2));
        expected.widget_mut("trackmap").expect("trackmap").options =
            "shape = \"outline\"".parse().expect("table");
        assert_eq!(config, expected, "nothing else changed");
        // Inline tables and dotted keys too.
        let inline = Config::parse("[widgets]\nfuel = { x = 1, y = 2 }\nwind.enabled = false\n")
            .expect("parses");
        assert_eq!(
            (widget(&inline, "fuel").x, widget(&inline, "fuel").y),
            (1, 2)
        );
        assert!(!widget(&inline, "wind").enabled);
    }

    #[test]
    fn benchmark_configurations_parse() {
        let four =
            Config::parse(include_str!("../../../benchmarks/pitot/four.toml")).expect("four");
        let enabled = |c: &Config| -> Vec<&'static str> {
            c.widgets()
                .filter(|(_, w)| w.enabled)
                .map(|(name, _)| name)
                .collect()
        };
        // A widget on by default would join the measured set: its entry would need `enabled = false` in both
        // files.
        assert_eq!(enabled(&four), ["inputs", "relative", "standings", "fuel"]);
        let defaults = Config::default();
        for name in ["inputs", "relative", "standings", "fuel"] {
            assert_eq!(
                widget(&four, name),
                widget(&defaults, name),
                "{name}: default positions and rates"
            );
        }
        let inputs =
            Config::parse(include_str!("../../../benchmarks/pitot/inputs.toml")).expect("inputs");
        assert_eq!(enabled(&inputs), ["inputs"]);
        assert_eq!(widget(&inputs, "inputs"), widget(&defaults, "inputs"));
        assert!(
            four.general.start_in_tray && inputs.general.start_in_tray,
            "measured without the Pitot window"
        );
        for general in [&four.general, &inputs.general] {
            assert_eq!(
                general.background_opacity, DEFAULT_BACKGROUND_OPACITY,
                "default backgrounds"
            );
        }
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
        let fuel = widget(&config, "fuel");
        assert_eq!((fuel.x, fuel.y), (10, 900), "y keeps the fuel default");
        assert_eq!(fuel.width, Some(420));
        assert_eq!(fuel.monitor, Some(2));
        assert!(fuel.enabled);
        assert_eq!(fuel.opacity, 0.82);
        assert_eq!(
            widget(&config, "relative"),
            widget(&Config::default(), "relative")
        );
        assert_eq!(Config::parse("").expect("empty file"), Config::default());
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        let unknown =
            Config::parse("[general]\n\n[widgets.status]\ncolour = 1\n").expect_err("field");
        assert!(matches!(unknown, ConfigError::Parse(_)));
        let message = unknown.to_string();
        assert!(
            message.contains("widgets.status (line 3)") && message.contains("colour"),
            "{message}"
        );
        let radar = Config::parse("[widgets.radar]\n").expect_err("widget");
        assert!(matches!(radar, ConfigError::Parse(_)));
        assert!(radar.to_string().contains("line 1"), "{radar}");
        assert!(
            radar.to_string().contains("unknown widget `radar`"),
            "{radar}"
        );
        // A widget's own field belongs to it alone, and its values are checked.
        let shape = Config::parse("[widgets.fuel]\nshape = \"circle\"\n").expect_err("shape");
        assert!(shape.to_string().contains("shape"), "{shape}");
        let square = Config::parse("[widgets.trackmap]\nshape = \"square\"\n").expect_err("value");
        assert!(square.to_string().contains("square"), "{square}");
        assert!(matches!(
            Config::parse("[widgets.trackmap]\nshape = 3\n"),
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
        let edited = load_or_create(&path).expect("reads");
        assert!(!widget(&edited, "status").enabled);
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
        let standings = widget(&config, "standings");
        assert_eq!((standings.x, standings.y), (1200, -40));
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
    fn saving_a_size_changes_only_its_numbers() {
        let sized =
            with_placement(DEFAULT_TOML, "relative", None, Some((520, 410))).expect("sized");
        let relative = Config::parse(&sized).expect("parses");
        let relative = widget(&relative, "relative");
        assert_eq!((relative.width, relative.height), (Some(520), Some(410)));
        assert_eq!((relative.x, relative.y), (1460, 560), "the place stays");
        let changed: Vec<(&str, &str)> = DEFAULT_TOML
            .lines()
            .zip(sized.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(
            changed,
            [
                ("width = 460", "width = 520"),
                ("height = 300", "height = 410")
            ]
        );
        assert_eq!(DEFAULT_TOML.lines().count(), sized.lines().count());

        // A top-left corner moves the place too; comments stay.
        let commented =
            "[widgets.fuel]\nx = 5 # left edge\ny = 6\nwidth = 420 # wide\nheight = 150\n";
        assert_eq!(
            with_placement(commented, "fuel", Some((1, 2)), Some((424, 154))).expect("placed"),
            "[widgets.fuel]\nx = 1 # left edge\ny = 2\nwidth = 424 # wide\nheight = 154\n"
        );
        let inline = "[widgets]\nfuel = { width = 100, height = 50 }\n";
        assert_eq!(
            with_placement(inline, "fuel", None, Some((300, 200))).expect("sized"),
            "[widgets]\nfuel = { width = 300, height = 200 }\n"
        );
        assert_eq!(
            with_placement(DEFAULT_TOML, "fuel", None, None).expect("nothing"),
            DEFAULT_TOML,
            "nothing to change"
        );
    }

    #[test]
    fn saving_a_size_adds_missing_keys_and_tables() {
        // The status widget has no size in the default file: the keys go under its header.
        let sized = with_placement(DEFAULT_TOML, "status", None, Some((340, 60))).expect("sized");
        assert!(
            sized.contains("[widgets.status]\nwidth = 340\nheight = 60\nenabled = true\n"),
            "{sized}"
        );
        let status = Config::parse(&sized).expect("parses");
        let status = widget(&status, "status");
        assert_eq!((status.width, status.height), (Some(340), Some(60)));
        assert_eq!(
            with_placement(
                "[widgets.fuel]\nwidth = 50\n",
                "fuel",
                None,
                Some((400, 160))
            )
            .expect("sized"),
            "[widgets.fuel]\nheight = 160\nwidth = 400\n"
        );
        assert_eq!(
            with_placement("[general]\n", "wind", Some((3, 4)), Some((210, 180))).expect("sized"),
            "[general]\n\n[widgets.wind]\nx = 3\ny = 4\nwidth = 210\nheight = 180\n"
        );
        assert!(matches!(
            with_placement(
                "[widgets]\nfuel = { x = 1 }\n",
                "fuel",
                None,
                Some((400, 160))
            ),
            Err(ConfigError::Unplaceable)
        ));
        // A size the configuration refuses is not written.
        assert!(matches!(
            with_placement(DEFAULT_TOML, "fuel", None, Some((9000, 160))),
            Err(ConfigError::Invalid {
                widget: "fuel",
                field: "width"
            })
        ));
    }

    #[test]
    fn saved_sizes_replace_the_file() {
        let dir = std::env::temp_dir().join(format!("pitot-size-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_TOML).expect("write");
        save_placement(&path, "inputs", Some((600, 880)), Some((800, 140))).expect("saved");
        let config = load_or_create(&path).expect("loads");
        let inputs = widget(&config, "inputs");
        assert_eq!(
            (inputs.x, inputs.y, inputs.width, inputs.height),
            (600, 880, Some(800), Some(140))
        );
        assert!(!path.with_extension("toml.tmp").exists());
        fs::remove_dir_all(&dir).expect("cleans up");
    }

    #[test]
    fn turning_a_widget_off_changes_only_its_value() {
        let off = with_enabled(DEFAULT_TOML, "wind", false).expect("edited");
        assert!(!widget(&Config::parse(&off).expect("parses"), "wind").enabled);
        let changed: Vec<(&str, &str)> = DEFAULT_TOML
            .lines()
            .zip(off.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(changed, [("enabled = true", "enabled = false")]);
        assert_eq!(DEFAULT_TOML.lines().count(), off.lines().count());
        let on = with_enabled(&off, "wind", true).expect("edited");
        assert_eq!(on, DEFAULT_TOML, "back on gives the same file");

        let commented = "[widgets.fuel]\nenabled = true # for races\nx = 5\n";
        assert_eq!(
            with_enabled(commented, "fuel", false).expect("edited"),
            "[widgets.fuel]\nenabled = false # for races\nx = 5\n"
        );
        let inline = "[widgets]\nfuel = { enabled = true, x = 1 }\n";
        assert_eq!(
            with_enabled(inline, "fuel", false).expect("edited"),
            "[widgets]\nfuel = { enabled = false, x = 1 }\n"
        );
    }

    #[test]
    fn turning_a_widget_off_adds_missing_keys_and_tables() {
        assert_eq!(
            with_enabled("[widgets.fuel]\nx = 5\n", "fuel", false).expect("edited"),
            "[widgets.fuel]\nenabled = false\nx = 5\n"
        );
        assert_eq!(
            with_enabled("[general]\nstart_in_tray = true\n", "trackmap", false).expect("edited"),
            "[general]\nstart_in_tray = true\n\n[widgets.trackmap]\nenabled = false\n"
        );
        assert!(matches!(
            with_enabled("[widgets]\nfuel = { x = 1 }\n", "fuel", false),
            Err(ConfigError::Unplaceable)
        ));
        assert!(matches!(
            with_enabled("[widgets.fuel]\nx = 1\n", "radar", false),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn background_opacity_defaults_to_the_widgets_own() {
        assert_eq!(
            Config::default().general.background_opacity,
            DEFAULT_BACKGROUND_OPACITY
        );
        assert_eq!(
            Config::parse("[general]\nfont = \"Arial\"\n")
                .expect("parses")
                .general
                .background_opacity,
            82,
            "left out"
        );
        let config = Config::parse("[general]\nbackground_opacity = 0\n").expect("parses");
        assert_eq!(config.general.background_opacity, 0);
        assert_eq!(
            config.general.font, "Segoe UI",
            "other fields keep defaults"
        );
        assert!(matches!(
            Config::parse("[general]\nbackground_opacity = 101\n"),
            Err(ConfigError::OutOfRange {
                key: "general.background_opacity"
            })
        ));
        // A percent is a whole number from 0 to 100.
        for bad in ["-1", "50.5", "\"50\"", "300"] {
            assert!(
                Config::parse(&format!("[general]\nbackground_opacity = {bad}\n")).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn background_opacity_maps_onto_each_widgets_own() {
        // At the default every background keeps its own alpha exactly, so nothing changes for existing files.
        for base in [0.0, 0.3, 0.6, 0.82, 1.0] {
            assert_eq!(background_alpha(base, DEFAULT_BACKGROUND_OPACITY), base);
            assert_eq!(background_alpha(base, 0), 0.0, "{base}: none at 0");
            assert!(
                (background_alpha(base, 100) - 1.0).abs() < 1e-6,
                "{base}: opaque at 100"
            );
        }
        // A panel at the default `opacity` gets the percent itself.
        for percent in [0u8, 10, 41, 82, 90, 100] {
            let alpha = background_alpha(0.82, percent);
            assert!(
                (alpha - f32::from(percent) / 100.0).abs() < 1e-6,
                "{percent}: {alpha}"
            );
        }
        // Halfway down halves a band's own alpha; halfway up closes half the gap to opaque.
        assert!((background_alpha(0.6, 41) - 0.3).abs() < 1e-6);
        assert!((background_alpha(0.6, 91) - 0.8).abs() < 1e-6);
        // Rising with the percent, and clamped at both ends.
        let alphas: Vec<f32> = (0..=100).map(|p| background_alpha(0.5, p)).collect();
        assert!(alphas.windows(2).all(|w| w[0] <= w[1]), "monotonic");
        assert_eq!(background_alpha(0.5, 200), background_alpha(0.5, 100));
        assert_eq!(background_alpha(1.5, 82), 1.0);
        assert_eq!(background_alpha(-0.5, 100), 1.0);
    }

    #[test]
    fn saving_background_opacity_changes_only_its_value() {
        let edited = with_background_opacity(DEFAULT_TOML, 40).expect("edited");
        assert_eq!(
            Config::parse(&edited)
                .expect("parses")
                .general
                .background_opacity,
            40
        );
        let changed: Vec<(&str, &str)> = DEFAULT_TOML
            .lines()
            .zip(edited.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(
            changed,
            [("background_opacity = 82", "background_opacity = 40")]
        );
        assert_eq!(DEFAULT_TOML.lines().count(), edited.lines().count());
        assert_eq!(
            with_background_opacity(&edited, 82).expect("edited"),
            DEFAULT_TOML,
            "back to the default gives the same file"
        );
        // Above 100 saves 100.
        assert_eq!(
            with_background_opacity("[general]\nbackground_opacity = 5 # dim\n", 250)
                .expect("edited"),
            "[general]\nbackground_opacity = 100 # dim\n"
        );
        // A file from before the setting: the key goes under the header, or the table at the end.
        assert_eq!(
            with_background_opacity("# mine\n[general]\nfont = \"Arial\"\n", 0).expect("edited"),
            "# mine\n[general]\nbackground_opacity = 0\nfont = \"Arial\"\n"
        );
        assert_eq!(
            with_background_opacity("[widgets.fuel]\nx = 1\n", 55).expect("edited"),
            "[widgets.fuel]\nx = 1\n\n[general]\nbackground_opacity = 55\n"
        );
        assert_eq!(
            with_background_opacity("", 55).expect("edited"),
            "[general]\nbackground_opacity = 55\n"
        );
        assert_eq!(
            with_background_opacity(
                "general = { font = \"Arial\", background_opacity = 9 }\n",
                10
            )
            .expect("edited"),
            "general = { font = \"Arial\", background_opacity = 10 }\n"
        );
        assert!(matches!(
            with_background_opacity("general = { font = \"Arial\" }\n", 10),
            Err(ConfigError::Unplaceable)
        ));
        assert!(matches!(
            with_background_opacity("[general\n", 10),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn saved_background_opacity_replaces_the_file() {
        let dir = std::env::temp_dir().join(format!("pitot-background-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_TOML).expect("write");
        save_background_opacity(&path, 25).expect("saved");
        let config = load_or_create(&path).expect("loads");
        assert_eq!(config.general.background_opacity, 25);
        assert_eq!(
            widget(&config, "fuel").opacity,
            0.82,
            "the widgets' own stay"
        );
        assert!(!path.with_extension("toml.tmp").exists());
        fs::remove_dir_all(&dir).expect("cleans up");
    }

    #[test]
    fn saved_enabled_replaces_the_file() {
        let dir = std::env::temp_dir().join(format!("pitot-enabled-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_TOML).expect("write");
        save_enabled(&path, "standings", false).expect("saved");
        assert!(!widget(&load_or_create(&path).expect("loads"), "standings").enabled);
        assert!(!path.with_extension("toml.tmp").exists());
        fs::remove_dir_all(&dir).expect("cleans up");
    }

    #[test]
    fn the_window_opens_at_start_unless_asked_not_to() {
        assert!(!Config::default().general.start_in_tray);
        let tray = Config::parse("[general]\nstart_in_tray = true\n").expect("parses");
        assert!(tray.general.start_in_tray);
        assert_eq!(tray.general.font, "Segoe UI", "other fields keep defaults");
    }

    #[test]
    fn saved_positions_replace_the_file() {
        let dir = std::env::temp_dir().join(format!("pitot-position-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_TOML).expect("write");
        save_position(&path, "fuel", 10, 20).expect("saved");
        let config = load_or_create(&path).expect("loads");
        assert_eq!(
            (widget(&config, "fuel").x, widget(&config, "fuel").y),
            (10, 20)
        );
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
