//! What the Pitot window lists and knows, and where it opens. No UI here, so it is tested without a window.

use pitot_overlay::config::{self, Config, Spec};
use serde::Deserialize;

/// Every widget the window lists, one switch each: the overlay's registry, in its order. A widget's `name` is its
/// `[widgets.<name>]` table in `config.toml`, and its `label` the switch's text.
pub fn widgets() -> &'static [Spec] {
    config::widget_specs()
}

/// Whether each widget of [`widgets`] is enabled in `config`.
pub fn enabled(config: &Config) -> Vec<bool> {
    widgets()
        .iter()
        .map(|w| config.widget(w.name).is_some_and(|c| c.enabled))
        .collect()
}

/// What a percent slider covers and how it moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub min: u8,
    pub max: u8,
    /// The pointer and the arrow keys land on whole multiples of it.
    pub step: u8,
    /// How far Shift with an arrow key, Page Up, and Page Down move it.
    pub page: u8,
}

/// `[general] background_opacity`: 0 to 100 in steps of 1.
pub const OPACITY: Range = Range {
    min: 0,
    max: config::MAX_BACKGROUND_OPACITY,
    step: 1,
    page: 10,
};

/// `[general] text_size`: 70 to 150 in steps of 5.
pub const TEXT_SIZE: Range = Range {
    min: config::MIN_TEXT_SIZE,
    max: config::MAX_TEXT_SIZE,
    step: config::TEXT_SIZE_STEP,
    page: 2 * config::TEXT_SIZE_STEP,
};

impl Range {
    /// The slider's value for the pointer at `x` over its track, which starts at `left` and is `width` wide: `min`
    /// at the left end, `max` at the right, the nearest step between, and the ends beyond them. `None` while the
    /// track has no width (before it is laid out).
    pub fn value_at(self, x: f32, left: f32, width: f32) -> Option<u8> {
        if !width.is_finite() || width <= 0.0 || !x.is_finite() || !left.is_finite() {
            return None;
        }
        let fraction = ((x - left) / width).clamp(0.0, 1.0);
        let value = f32::from(self.min) + fraction * f32::from(self.max - self.min);
        let step = f32::from(self.step.max(1));
        let on_step = (value / step).round() * step;
        Some(on_step.clamp(f32::from(self.min), f32::from(self.max)) as u8)
    }

    /// Where `value` sits on the track, from 0 at `min` to 1 at `max`.
    pub fn fraction(self, value: u8) -> f32 {
        let span = f32::from(self.max.saturating_sub(self.min)).max(1.0);
        f32::from(value.clamp(self.min, self.max) - self.min) / span
    }

    /// The slider's value after `key` while it has the focus: the arrow keys move it to the next step (by `page`
    /// with Shift), Page Up and Page Down by `page`, Home and End to `min` and `max`. A value between steps (from a
    /// file edited by hand) moves to the nearest step in the key's direction. `None` for other keys.
    pub fn step(self, value: u8, key: &str, shift: bool) -> Option<u8> {
        let by = i32::from(if shift { self.page } else { self.step });
        let page = i32::from(self.page);
        let value = i32::from(value.clamp(self.min, self.max));
        let step = i32::from(self.step.max(1));
        // A move down rounds up to a step and a move up rounds down, so neither passes the next step.
        let moved = match key {
            "left" | "down" => ceil_to(value - by, step),
            "right" | "up" => floor_to(value + by, step),
            "pagedown" => ceil_to(value - page, step),
            "pageup" => floor_to(value + page, step),
            "home" => i32::from(self.min),
            "end" => i32::from(self.max),
            _ => return None,
        };
        Some(moved.clamp(i32::from(self.min), i32::from(self.max)) as u8)
    }
}

/// The smallest whole multiple of `step` at or above `value`.
fn ceil_to(value: i32, step: i32) -> i32 {
    -(-value).div_euclid(step) * step
}

/// The largest whole multiple of `step` at or below `value`.
fn floor_to(value: i32, step: i32) -> i32 {
    value.div_euclid(step) * step
}

/// The overlay as the window knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// No overlay answers.
    NotRunning,
    /// Asked to start; not answering yet.
    Starting,
    Running {
        hidden: bool,
        moving: bool,
    },
}

impl Overlay {
    /// One line for the window's header.
    pub fn status(self) -> &'static str {
        match self {
            Self::NotRunning => "Overlay not running",
            Self::Starting => "Starting the overlay",
            Self::Running { moving: true, .. } => "Moving widgets: drag them with the mouse",
            Self::Running { hidden: true, .. } => "Widgets hidden",
            Self::Running { .. } => "Overlay running",
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// Pitot's version as the window shows it: the installed version when Pitot is installed, else the version it was
/// built as.
pub fn version_text(installed: Option<&str>) -> String {
    format!("v{}", installed.unwrap_or(env!("CARGO_PKG_VERSION")))
}

/// Updates as the window knows them, from its start to the installer taking over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// Run from a copy that is not installed (a build, a copied executable): there is nothing to update.
    NotInstalled,
    /// `[general] check_updates = false`: nothing is asked.
    Off,
    Checking,
    /// A newer release, being downloaded: `percent` from 0 to 100.
    Downloading {
        version: String,
        percent: u8,
    },
    /// A newer release, downloaded and verified: the button installs it.
    Ready {
        version: String,
    },
    UpToDate,
    /// The releases list no installable version.
    NoneFound,
    CheckFailed,
    DownloadFailed,
    /// The button was pressed: the overlay is quitting.
    Closing {
        version: String,
    },
    /// The installer takes over once this window closes.
    Installing {
        version: String,
    },
    InstallFailed,
}

impl Update {
    /// One quiet line under the switch; `None` while the button says it all.
    pub fn line(&self) -> Option<String> {
        let line = match self {
            Self::NotInstalled => "Updates need the installed Pitot".to_owned(),
            Self::Off => "Not checking for updates".to_owned(),
            Self::Checking => "Checking for updates\u{2026}".to_owned(),
            Self::Downloading { version, percent } => {
                format!("Downloading v{version}\u{2026} {percent}%")
            }
            Self::Ready { .. } => return None,
            Self::UpToDate => "Up to date".to_owned(),
            Self::NoneFound => "No updates found".to_owned(),
            Self::CheckFailed => "Couldn't check for updates".to_owned(),
            Self::DownloadFailed => "Couldn't download the update".to_owned(),
            Self::Closing { .. } => "Closing the overlay\u{2026}".to_owned(),
            Self::Installing { version } => format!("Installing v{version}\u{2026}"),
            Self::InstallFailed => "Couldn't install the update".to_owned(),
        };
        Some(line)
    }

    /// The install button's label, while there is something to install.
    pub fn button(&self) -> Option<String> {
        match self {
            Self::Ready { version } => Some(format!("Update to v{version}")),
            _ => None,
        }
    }

    /// While the update is being installed: the overlay's quit must not close the window, and nothing else may
    /// start.
    pub fn installing(&self) -> bool {
        matches!(self, Self::Closing { .. } | Self::Installing { .. })
    }

    /// Whether a check may start now: not while one runs, nor once a release waits or installs.
    pub fn may_check(&self) -> bool {
        matches!(
            self,
            Self::Off
                | Self::UpToDate
                | Self::NoneFound
                | Self::CheckFailed
                | Self::DownloadFailed
                | Self::InstallFailed
        )
    }
}

/// Where the window opens: its top-left corner in screen coordinates, in the UI framework's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
}

impl Placement {
    /// Reads a placement file; anything unreadable means the default place.
    pub fn parse(text: &str) -> Option<Self> {
        toml::from_str::<Self>(text)
            .ok()
            .filter(|p| p.x.is_finite() && p.y.is_finite())
    }

    /// The file's text.
    pub fn to_text(self) -> String {
        format!(
            "# Where the Pitot window opens, in screen pixels. Written when it closes.\nx = {:.0}\ny = {:.0}\n",
            self.x, self.y
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_matches_the_configuration() {
        let config = Config::default();
        let names: Vec<&str> = config.widgets().map(|(name, _)| name).collect();
        let listed: Vec<&str> = widgets().iter().map(|w| w.name).collect();
        assert_eq!(listed, names);
        let labels: Vec<&str> = widgets().iter().map(|w| w.label).take(3).collect();
        assert_eq!(labels, ["Status", "Input trace", "Relative"]);
    }

    #[test]
    fn enabled_follows_the_file() {
        let config =
            Config::parse("[widgets.fuel]\nenabled = false\n[widgets.wind]\nenabled = false\n")
                .expect("parses");
        let on = enabled(&config);
        assert_eq!(on.len(), widgets().len());
        for (widget, on) in widgets().iter().zip(on) {
            let expected = widget.enabled && !matches!(widget.name, "fuel" | "wind");
            assert_eq!(on, expected, "{}", widget.name);
        }
    }

    #[test]
    fn placements_round_trip_and_bad_files_are_ignored() {
        let placement = Placement {
            x: 5000.0,
            y: -120.0,
        };
        assert_eq!(Placement::parse(&placement.to_text()), Some(placement));
        assert_eq!(Placement::parse("x = 1\n"), None, "y missing");
        assert_eq!(Placement::parse("x = 1\ny = 2\nz = 3\n"), None);
        assert_eq!(Placement::parse("x = nan\ny = 2\n"), None);
        assert_eq!(
            Placement::parse("x = 10\ny = 20\n"),
            Some(Placement { x: 10.0, y: 20.0 }),
            "integers"
        );
    }

    #[test]
    fn the_slider_turns_positions_and_keys_into_percents() {
        let percent_at = |x, left, width| OPACITY.value_at(x, left, width);
        assert_eq!(percent_at(100.0, 100.0, 200.0), Some(0));
        assert_eq!(percent_at(300.0, 100.0, 200.0), Some(100));
        assert_eq!(percent_at(182.0, 100.0, 200.0), Some(41));
        assert_eq!(percent_at(183.0, 100.0, 200.0), Some(42), "nearest");
        assert_eq!(
            percent_at(-50.0, 100.0, 200.0),
            Some(0),
            "beyond the left end"
        );
        assert_eq!(
            percent_at(900.0, 100.0, 200.0),
            Some(100),
            "beyond the right end"
        );
        assert_eq!(percent_at(150.0, 100.0, 0.0), None, "not laid out yet");
        assert_eq!(percent_at(f32::NAN, 100.0, 200.0), None);
        assert_eq!(OPACITY.fraction(82), 0.82);

        let step_percent = |percent, key, shift| OPACITY.step(percent, key, shift);
        assert_eq!(step_percent(82, "right", false), Some(83));
        assert_eq!(step_percent(82, "left", true), Some(72));
        assert_eq!(step_percent(95, "up", true), Some(100));
        assert_eq!(step_percent(3, "pagedown", false), Some(0));
        assert_eq!(step_percent(0, "down", false), Some(0));
        assert_eq!(step_percent(100, "pageup", false), Some(100));
        assert_eq!(step_percent(40, "home", false), Some(0));
        assert_eq!(step_percent(40, "end", false), Some(100));
        assert_eq!(step_percent(40, "a", false), None);
        assert_eq!(step_percent(200, "left", false), Some(99), "clamped first");
    }

    #[test]
    fn the_text_size_slider_moves_in_steps_of_five() {
        let at = |x| TEXT_SIZE.value_at(x, 0.0, 160.0);
        assert_eq!(at(0.0), Some(70));
        assert_eq!(at(160.0), Some(150));
        assert_eq!(at(60.0), Some(100), "the default, 30 of 80 points along");
        assert_eq!(at(64.0), Some(100), "the nearest step");
        assert_eq!(at(66.0), Some(105));
        assert_eq!(at(-10.0), Some(70));
        assert_eq!(at(999.0), Some(150));
        assert_eq!(TEXT_SIZE.fraction(70), 0.0);
        assert_eq!(TEXT_SIZE.fraction(110), 0.5);
        assert_eq!(TEXT_SIZE.fraction(150), 1.0);
        assert_eq!(TEXT_SIZE.fraction(20), 0.0, "below the range");

        let step = |value, key, shift| TEXT_SIZE.step(value, key, shift);
        assert_eq!(step(100, "right", false), Some(105));
        assert_eq!(step(100, "left", false), Some(95));
        assert_eq!(step(100, "up", true), Some(110));
        assert_eq!(step(100, "pagedown", false), Some(90));
        assert_eq!(step(150, "up", false), Some(150));
        assert_eq!(step(70, "down", true), Some(70));
        assert_eq!(step(120, "home", false), Some(70));
        assert_eq!(step(120, "end", false), Some(150));
        // A value between steps, written by hand, goes to the nearest step in the key's direction.
        assert_eq!(step(103, "right", false), Some(105));
        assert_eq!(step(103, "left", false), Some(100));
        assert_eq!(step(103, "pageup", false), Some(110));
        assert_eq!(step(103, "pagedown", false), Some(95));
        assert_eq!(step(100, "x", false), None);
        assert_eq!(step(200, "left", false), Some(145), "clamped first");
    }

    #[test]
    fn the_version_is_the_installed_one_when_there_is_one() {
        assert_eq!(version_text(Some("0.1.5-test.2")), "v0.1.5-test.2");
        assert_eq!(
            version_text(None),
            format!("v{}", env!("CARGO_PKG_VERSION")),
            "a copy that is not installed shows its build"
        );
    }

    #[test]
    fn each_update_state_says_one_quiet_thing() {
        let v = || "0.1.5".to_owned();
        assert_eq!(
            Update::NotInstalled.line().as_deref(),
            Some("Updates need the installed Pitot")
        );
        assert_eq!(Update::NotInstalled.button(), None);
        assert_eq!(
            Update::Downloading {
                version: v(),
                percent: 40
            }
            .line()
            .as_deref(),
            Some("Downloading v0.1.5\u{2026} 40%")
        );
        let ready = Update::Ready { version: v() };
        assert_eq!(ready.line(), None, "the button says it");
        assert_eq!(ready.button().as_deref(), Some("Update to v0.1.5"));
        assert_eq!(
            Update::CheckFailed.line().as_deref(),
            Some("Couldn't check for updates")
        );
        assert_eq!(
            Update::Installing { version: v() }.line().as_deref(),
            Some("Installing v0.1.5\u{2026}")
        );
        for state in [
            Update::Checking,
            Update::UpToDate,
            Update::Closing { version: v() },
            Update::InstallFailed,
        ] {
            assert!(state.line().is_some(), "{state:?}");
            assert_eq!(state.button(), None, "{state:?}");
        }
    }

    #[test]
    fn checks_and_installs_do_not_overlap() {
        let v = || "0.1.5".to_owned();
        assert!(Update::Closing { version: v() }.installing());
        assert!(Update::Installing { version: v() }.installing());
        assert!(!Update::Ready { version: v() }.installing());
        for idle in [
            Update::Off,
            Update::UpToDate,
            Update::NoneFound,
            Update::CheckFailed,
            Update::DownloadFailed,
            Update::InstallFailed,
        ] {
            assert!(idle.may_check(), "{idle:?}");
        }
        for busy in [
            Update::NotInstalled,
            Update::Checking,
            Update::Downloading {
                version: v(),
                percent: 0,
            },
            Update::Ready { version: v() },
            Update::Closing { version: v() },
            Update::Installing { version: v() },
        ] {
            assert!(!busy.may_check(), "{busy:?}");
        }
    }

    #[test]
    fn the_header_says_what_the_overlay_does() {
        assert_eq!(Overlay::NotRunning.status(), "Overlay not running");
        let running = |hidden, moving| Overlay::Running { hidden, moving };
        assert_eq!(running(false, false).status(), "Overlay running");
        assert_eq!(running(true, false).status(), "Widgets hidden");
        assert!(running(false, true).status().starts_with("Moving"));
        assert!(running(true, true).is_running());
        assert!(!Overlay::Starting.is_running());
    }
}
