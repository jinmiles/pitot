//! Overlay configuration: a TOML file in the user's config directory, created with defaults on first run and
//! reloaded when it changes. A file that fails to parse or validate leaves the previous configuration in place.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The single source of truth for where configuration lives: `%APPDATA%\Pitot`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Pitot"))
}

/// `config.toml` inside [`config_dir`].
pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.toml"))
}

/// Written on first run; kept in sync with [`Config::default`] by a test.
pub const DEFAULT_TOML: &str = r#"# Pitot overlay configuration. Changes apply when the file is saved.
#
# Positions are in physical pixels from the top-left corner of the widget's monitor.
# `monitor` counts monitors from the left (0 is the leftmost); leave it out to use the primary monitor.

[general]
font = "Segoe UI"

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
width = 430
height = 280
rate_hz = 15.0
opacity = 0.82

[widgets.standings]
enabled = true
x = 24
y = 260
width = 480
height = 560
rate_hz = 4.0
opacity = 0.82

[widgets.fuel]
enabled = true
x = 1460
y = 900
width = 300
height = 120
rate_hz = 1.0
opacity = 0.82
"#;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub general: General,
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

#[derive(Debug, Clone, PartialEq)]
pub struct Widgets {
    pub status: WidgetConfig,
    pub inputs: WidgetConfig,
    pub relative: WidgetConfig,
    pub standings: WidgetConfig,
    pub fuel: WidgetConfig,
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
        };
        Self {
            status: widget(24, 24, None, None),
            inputs: widget(660, 900, Some((600, 120)), Some(60.0)),
            relative: widget(1460, 560, Some((430, 280)), Some(15.0)),
            standings: widget(24, 260, Some((480, 560)), Some(4.0)),
            fuel: widget(1460, 900, Some((300, 120)), Some(1.0)),
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
}

/// The file's shape: every field optional, applied over the defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    general: General,
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
            widgets: Widgets {
                status: w.status.apply(defaults.status),
                inputs: w.inputs.apply(defaults.inputs),
                relative: w.relative.apply(defaults.relative),
                standings: w.standings.apply(defaults.standings),
                fuel: w.fuel.apply(defaults.fuel),
            },
        };
        config.validate()?;
        Ok(config)
    }

    /// Widget configurations by name, in a fixed order.
    pub fn widgets(&self) -> [(&'static str, &WidgetConfig); 5] {
        let w = &self.widgets;
        [
            ("status", &w.status),
            ("inputs", &w.inputs),
            ("relative", &w.relative),
            ("standings", &w.standings),
            ("fuel", &w.fuel),
        ]
    }

    fn validate(&self) -> Result<(), ConfigError> {
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
    fn missing_fields_keep_defaults() {
        let config = Config::parse("[widgets.fuel]\nx = 10\nmonitor = 2\n").expect("parses");
        assert_eq!(
            (config.widgets.fuel.x, config.widgets.fuel.y),
            (10, 900),
            "y keeps the fuel default"
        );
        assert_eq!(config.widgets.fuel.width, Some(300));
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
    fn monitors_are_ordered_left_to_right() {
        // Lab PC layout: primary at 0, one to its right, a portrait one further right and higher.
        assert_eq!(monitor_order(&[(1920, 0), (3840, -383), (0, 0)]), [2, 0, 1]);
        assert_eq!(monitor_order(&[(-1920, 0), (0, 0)]), [0, 1]);
    }
}
