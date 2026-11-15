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

/// The background opacity slider's percent for the pointer at `x` over its track, which starts at `left` and is
/// `width` wide: 0 at the left end, 100 at the right, the nearest whole percent between, and the ends beyond them.
/// `None` while the track has no width (before it is laid out).
pub fn percent_at(x: f32, left: f32, width: f32) -> Option<u8> {
    if !width.is_finite() || width <= 0.0 || !x.is_finite() || !left.is_finite() {
        return None;
    }
    let max = f32::from(config::MAX_BACKGROUND_OPACITY);
    let fraction = ((x - left) / width).clamp(0.0, 1.0);
    Some((fraction * max).round() as u8)
}

/// The slider's percent after `key` while it has the focus: the arrow keys move it by 1 (by 10 with Shift), Page
/// Up and Page Down by 10, Home and End to 0 and 100. `None` for other keys.
pub fn step_percent(percent: u8, key: &str, shift: bool) -> Option<u8> {
    let max = config::MAX_BACKGROUND_OPACITY;
    let step = if shift { 10 } else { 1 };
    let percent = percent.min(max);
    Some(match key {
        "left" | "down" => percent.saturating_sub(step),
        "right" | "up" => percent.saturating_add(step).min(max),
        "pagedown" => percent.saturating_sub(10),
        "pageup" => percent.saturating_add(10).min(max),
        "home" => 0,
        "end" => max,
        _ => return None,
    })
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
