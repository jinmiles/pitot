//! What the Pitot window lists and knows, and where it opens. No UI here, so it is tested without a window.

use serde::Deserialize;

/// A widget as the window lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetInfo {
    /// The name in `config.toml` (`[widgets.<name>]`).
    pub name: &'static str,
    pub label: &'static str,
}

/// Every widget, in the order of [`pitot_overlay::config::Config::widgets`].
pub const WIDGETS: [WidgetInfo; 10] = [
    info("status", "Status"),
    info("inputs", "Input trace"),
    info("relative", "Relative"),
    info("standings", "Standings"),
    info("fuel", "Fuel"),
    info("pit", "Pit stops"),
    info("sectors", "Sectors"),
    info("wind", "Wind"),
    info("tires", "Tires"),
    info("trackmap", "Track map"),
];

const fn info(name: &'static str, label: &'static str) -> WidgetInfo {
    WidgetInfo { name, label }
}

/// Whether each widget in [`WIDGETS`] is enabled in `config`.
pub fn enabled(config: &pitot_overlay::config::Config) -> [bool; WIDGETS.len()] {
    let widgets = config.widgets();
    std::array::from_fn(|i| {
        widgets
            .iter()
            .find(|(name, _)| *name == WIDGETS[i].name)
            .is_some_and(|(_, widget)| widget.enabled)
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
    use pitot_overlay::config::Config;

    use super::*;

    #[test]
    fn the_list_matches_the_configuration() {
        let config = Config::default();
        let names: Vec<&str> = config.widgets().iter().map(|(name, _)| *name).collect();
        let listed: Vec<&str> = WIDGETS.iter().map(|w| w.name).collect();
        assert_eq!(listed, names);
    }

    #[test]
    fn enabled_follows_the_file() {
        let config =
            Config::parse("[widgets.fuel]\nenabled = false\n[widgets.wind]\nenabled = false\n")
                .expect("parses");
        let on = enabled(&config);
        for (widget, on) in WIDGETS.iter().zip(on) {
            assert_eq!(
                on,
                !matches!(widget.name, "fuel" | "wind"),
                "{}",
                widget.name
            );
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
