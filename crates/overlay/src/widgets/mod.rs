//! Widgets: each owns its state, decides when and what to redraw, and draws through a [`Canvas`].
//!
//! A widget has one or more layers (its own composition surfaces), for example a static background and a
//! scrolling trace. Each update reports what to redraw per layer.
//!
//! Every widget is one line of `list.rs`: its module, which is also its `[widgets.<name>]` table, its type, and
//! its [`Spec`] (label and defaults). The list declares the modules and gives the defaults, the names the
//! configuration accepts, the Pitot window's switches, and the widgets the render loop makes. A widget type
//! implements [`Register`]: the variables it reads beyond `Vars`, the fields of its own in its table, and how it is
//! made from its configuration and the trackers the layout's widgets share ([`Shared`]). See "Adding a widget" in
//! `docs/overlay-rendering.md`.

#[allow(dead_code, reason = "read by the widgets of later batches")]
mod gaps;
#[allow(dead_code, reason = "read by the widgets of later batches")]
mod laps;
mod pitstops;
mod text;
mod timing;
mod trackclock;

use std::cell::OnceCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use pitot_irsdk::live::VarSet;
use serde::Serialize;
use serde::de::DeserializeOwned;

#[allow(unused_imports, reason = "read by the widgets of later batches")]
pub(crate) use gaps::{GapHistory, GapSample, SharedGapHistory};
#[allow(unused_imports, reason = "read by the widgets of later batches")]
pub(crate) use laps::{Lap, LapHistory, SharedLapHistory};
pub(crate) use pitstops::{PitTracker, SharedPitTracker};
pub(crate) use trackclock::{SharedTrackClock, TrackClock};

use crate::config::{self, Spec, WidgetConfig};
use crate::pipeline::{Display, VarGroup, View};
use crate::render::{Canvas, Rect};

/// Declares the widgets of `list.rs`: `module: Type = spec,` per widget.
macro_rules! widgets {
    ($($name:ident: $widget:ident = $spec:expr,)*) => {
        $(mod $name;)*
        $(pub use $name::$widget;)*

        /// Every widget, in the list's order.
        pub(crate) static SPECS: &[Spec] = &[$(
            $spec
                .named(stringify!($name))
                .with_options(config::normalize_options::<<$widget as Register>::Options>),
        )*];

        /// Makes the widgets of [`SPECS`], by index.
        static MAKERS: &[fn(&Build<'_>, &VarSet) -> Box<dyn Widget>] = &[$(make::<$widget>,)*];

        /// The names of every widget's own variables, which the pipeline registers at start.
        pub(crate) fn widget_var_names() -> impl Iterator<Item = &'static str> {
            [$(<<$widget as Register>::Vars as VarGroup>::NAMES,)*]
                .into_iter()
                .flatten()
                .copied()
        }
    };
}

include!("list.rs");

/// How the registry makes a widget. Every type in `list.rs` implements it, in its own module.
pub(crate) trait Register: Widget + Sized + 'static {
    /// The variables it reads beyond `Vars`: a struct declared with `crate::pipeline::var_group!`, or `()`.
    type Vars: VarGroup;
    /// Fields of its own in its `[widgets.<name>]` table: a struct with `#[serde(default, deny_unknown_fields)]`,
    /// or [`NoOptions`](crate::config::NoOptions). Read them with [`WidgetConfig::options_as`].
    type Options: DeserializeOwned + Serialize + Default;

    /// Makes the widget from its configuration, the layout's shared trackers, and its variables.
    fn build(cx: &Build<'_>, vars: Self::Vars) -> Self;
}

/// What a widget is made from.
pub(crate) struct Build<'a> {
    pub(crate) config: &'a WidgetConfig,
    pub(crate) shared: &'a Shared,
}

fn make<W: Register>(cx: &Build<'_>, vars: &VarSet) -> Box<dyn Widget> {
    Box::new(W::build(cx, W::Vars::resolve(vars)))
}

/// Makes the widget called `name`, with its variables resolved in `vars` (the pipeline's set, see
/// [`View::var_set`]). `None` for a name the list does not have.
pub(crate) fn make_widget(name: &str, cx: &Build<'_>, vars: &VarSet) -> Option<Box<dyn Widget>> {
    let index = SPECS.iter().position(|s| s.name == name)?;
    MAKERS.get(index).map(|make| make(cx, vars))
}

/// Trackers the widgets of one layout share. Each is made when the first widget asks for it, so one that no
/// enabled widget uses costs nothing.
pub(crate) struct Shared {
    /// Where the pit tracker keeps what it learns.
    pit_store: Option<PathBuf>,
    pit: OnceCell<SharedPitTracker>,
    clock: OnceCell<SharedTrackClock>,
    gaps: OnceCell<SharedGapHistory>,
    laps: OnceCell<SharedLapHistory>,
}

impl Shared {
    /// Trackers that keep what they learn in the configuration directory.
    pub(crate) fn new() -> Self {
        Self::with_pit_store(config::pit_file())
    }

    /// Trackers whose pit tracker keeps what it learns in `pit_store`, or nowhere.
    pub(crate) fn with_pit_store(pit_store: Option<PathBuf>) -> Self {
        Self {
            pit_store,
            pit: OnceCell::new(),
            clock: OnceCell::new(),
            gaps: OnceCell::new(),
            laps: OnceCell::new(),
        }
    }

    /// Other cars' pit stops and what a stop costs (the pit widget and the track map).
    pub(crate) fn pit(&self) -> SharedPitTracker {
        Rc::clone(
            self.pit
                .get_or_init(|| PitTracker::shared(self.pit_store.clone())),
        )
    }

    /// The race clock that times gaps (the standings and the relative).
    pub(crate) fn clock(&self) -> SharedTrackClock {
        Rc::clone(self.clock.get_or_init(TrackClock::shared))
    }

    /// Every car's gaps on track over the last minutes, on [`Shared::clock`].
    #[allow(dead_code, reason = "read by the widgets of later batches")]
    pub(crate) fn gaps(&self) -> SharedGapHistory {
        Rc::clone(self.gaps.get_or_init(|| GapHistory::shared(self.clock())))
    }

    /// Every car's completed laps.
    #[allow(dead_code, reason = "read by the widgets of later batches")]
    pub(crate) fn laps(&self) -> SharedLapHistory {
        Rc::clone(self.laps.get_or_init(LapHistory::shared))
    }

    /// Forgets the session, for a new connection: the clock and the histories. The pit tracker is reset by its
    /// widgets, which keep what it learned.
    pub(crate) fn reset(&self) {
        if let Some(clock) = self.clock.get()
            && let Ok(mut clock) = clock.try_borrow_mut()
        {
            clock.reset();
        }
        if let Some(gaps) = self.gaps.get()
            && let Ok(mut gaps) = gaps.try_borrow_mut()
        {
            gaps.reset();
        }
        if let Some(laps) = self.laps.get()
            && let Ok(mut laps) = laps.try_borrow_mut()
        {
            laps.reset();
        }
    }
}

/// Most layers a widget may have.
pub const MAX_LAYERS: usize = 2;

/// What a layer needs redrawn after an update.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Redraw {
    Nothing,
    All,
    /// Only this area, in layer pixels.
    Area(Rect),
    /// Two areas, for example a range of columns that wraps around a scrolling layer.
    Areas(Rect, Rect),
}

/// A widget layer, positioned in widget pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerSpec {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// The surface is a ring of columns shown from [`Widget::scroll`].
    pub scrolling: bool,
}

pub trait Widget {
    /// Size in pixels.
    fn size(&self) -> (u32, u32);

    /// The smallest size its layout still reads at. A configured or dragged size below it is raised to it.
    fn min_size(&self) -> (u32, u32);

    /// Takes the size `width` by `height`, raised to [`Widget::min_size`], as if it had been configured: text and
    /// shapes keep their pixel size, and the widget lays out within the new box (more rows, a longer history).
    /// What it follows of the session stays. Only *Move widgets* calls it, so it may allocate.
    fn resize(&mut self, width: u32, height: u32);

    /// The widget's layers, bottom to top. Called when the widget is placed.
    fn layers(&self) -> Vec<LayerSpec> {
        let (width, height) = self.size();
        vec![LayerSpec {
            x: 0,
            y: 0,
            width,
            height,
            scrolling: false,
        }]
    }

    /// How often [`Widget::update`] runs unless the configuration sets a rate.
    fn interval(&self) -> Duration;

    /// Takes the latest data and fills in what each layer needs redrawn. Must not allocate after warm-up.
    fn update(&mut self, view: &mut View, now: Instant, redraw: &mut [Redraw; MAX_LAYERS]);

    /// Draws `layer`. The canvas is clipped to the area being redrawn. Must not allocate after warm-up.
    fn draw(&self, layer: usize, canvas: &mut Canvas<'_>);

    /// First column to show for a scrolling layer.
    fn scroll(&self, _layer: usize) -> u32 {
        0
    }

    /// Whether the widget shows in this display state. Hidden widgets cost no drawing.
    fn visible(&self, display: Display) -> bool {
        display == Display::Driving
    }

    /// Called when the connection changes, so per-session state can reset.
    fn reset(&mut self) {}
}

/// For the allocation check outside the crate: every widget of the list at its defaults, made as the render loop
/// makes them, and the shared histories observed as widgets observe them. The pit tracker keeps nothing on disk.
#[cfg(feature = "alloc-check")]
pub struct AllWidgets {
    shared: Shared,
    widgets: Vec<Box<dyn Widget>>,
    /// Each widget's size as made.
    defaults: Vec<(u32, u32)>,
}

#[cfg(feature = "alloc-check")]
impl AllWidgets {
    /// Every widget of the list, on or off by default, with its variables resolved in `view`'s set.
    pub fn new(view: &View) -> Self {
        let shared = Shared::with_pit_store(None);
        let widgets: Vec<Box<dyn Widget>> = SPECS
            .iter()
            .filter_map(|spec| {
                let config = spec.defaults();
                let cx = Build {
                    config: &config,
                    shared: &shared,
                };
                make_widget(spec.name, &cx, view.var_set())
            })
            .collect();
        let defaults = widgets.iter().map(|w| w.size()).collect();
        Self {
            shared,
            widgets,
            defaults,
        }
    }

    /// Resizes every widget, as *Move widgets* does, to what `size` picks from its smallest size and the size it
    /// was made with.
    pub fn resize(&mut self, size: impl Fn((u32, u32), (u32, u32)) -> (u32, u32)) {
        for (widget, &made) in self.widgets.iter_mut().zip(&self.defaults) {
            let (width, height) = size(widget.min_size(), made);
            widget.resize(width, height);
        }
    }

    /// The widgets, in the list's order.
    pub fn widgets(&self) -> impl Iterator<Item = &dyn Widget> {
        self.widgets.iter().map(|w| w.as_ref())
    }

    /// How many widgets there are.
    pub fn len(&self) -> usize {
        self.widgets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.widgets.is_empty()
    }

    /// Updates every widget and observes the gap and lap histories. Returns the gap samples held and the laps
    /// held of `car`, so a test sees that they ran.
    pub fn update(&mut self, view: &mut View, now: Instant, car: usize) -> (usize, usize) {
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        for widget in &mut self.widgets {
            widget.update(view, now, &mut redraw);
        }
        let (gaps, laps) = (self.shared.gaps(), self.shared.laps());
        gaps.borrow_mut().observe(view);
        laps.borrow_mut().observe(view);
        (gaps.borrow().len(), laps.borrow().count(car))
    }
}

#[cfg(test)]
mod tests {
    use pitot_irsdk::live::VarSet;

    use super::*;
    use crate::pipeline::Vars;

    #[test]
    fn the_list_names_each_widget_once() {
        let names: Vec<&str> = SPECS.iter().map(|s| s.name).collect();
        let labels: Vec<&str> = SPECS.iter().map(|s| s.label).collect();
        for (i, spec) in SPECS.iter().enumerate() {
            assert!(!names[..i].contains(&spec.name), "{} twice", spec.name);
            assert!(!labels[..i].contains(&spec.label), "{} twice", spec.label);
            assert!(!spec.label.is_empty(), "{} has a label", spec.name);
            assert!(
                spec.rate_hz.is_none_or(|r| (0.1..=120.0).contains(&r)),
                "{}: a rate the configuration accepts",
                spec.name
            );
        }
        assert_eq!(MAKERS.len(), SPECS.len());
    }

    #[test]
    fn the_docs_list_every_widget_once() {
        let docs = include_str!("../../../../docs/widgets.md");
        let rows: Vec<&str> = docs.lines().filter(|l| l.starts_with("| `")).collect();
        assert_eq!(
            rows.len(),
            SPECS.len(),
            "one row per widget in docs/widgets.md"
        );
        for (row, spec) in rows.iter().zip(SPECS) {
            let on = if spec.enabled { "yes" } else { "no" };
            let expected = format!("| `{}` | {} | {on} | ", spec.name, spec.label);
            assert!(
                row.starts_with(&expected),
                "docs/widgets.md, in the list's order: {expected}"
            );
        }
    }

    #[test]
    fn every_widget_is_made_from_its_defaults() {
        let mut set = VarSet::new();
        Vars::register(&mut set);
        let core = set.len();
        for name in widget_var_names() {
            set.add(name);
        }
        // The wind widget's own variables: two new ones, and `YawNorth` from `Vars` copied once.
        assert_eq!(set.len(), core + 2);
        assert!(set.find("WindVel").is_some());
        let shared = Shared::with_pit_store(None);
        for spec in SPECS {
            let config = spec.defaults();
            let cx = Build {
                config: &config,
                shared: &shared,
            };
            let widget = make_widget(spec.name, &cx, &set).expect("made");
            if let (Some(width), Some(height)) = (spec.width, spec.height) {
                assert_eq!(widget.size(), (width, height), "{}", spec.name);
            }
        }
        let config = SPECS[0].defaults();
        let cx = Build {
            config: &config,
            shared: &shared,
        };
        assert!(make_widget("radar", &cx, &set).is_none());
    }

    #[test]
    fn a_resized_widget_is_the_widget_made_at_that_size() {
        let mut set = VarSet::new();
        Vars::register(&mut set);
        for name in widget_var_names() {
            set.add(name);
        }
        let shared = Shared::with_pit_store(None);
        let make = |name: &str, size: Option<(u32, u32)>| {
            let spec = SPECS.iter().find(|s| s.name == name).expect("spec");
            let mut config = spec.defaults();
            if let Some((width, height)) = size {
                (config.width, config.height) = (Some(width), Some(height));
            }
            let cx = Build {
                config: &config,
                shared: &shared,
            };
            make_widget(name, &cx, &set).expect("made")
        };
        for spec in SPECS {
            let mut widget = make(spec.name, None);
            let (min_w, min_h) = widget.min_size();
            let (w, h) = widget.size();
            assert!(w >= min_w && h >= min_h, "{}: the default fits", spec.name);
            assert!(
                min_w >= 16 && min_h >= 16,
                "{}: a size the file accepts",
                spec.name
            );
            for (width, height) in [
                (w * 2, h * 3),
                (min_w, min_h),
                (w + 37, h + 23),
                (1, 1),
                (w, h),
            ] {
                widget.resize(width, height);
                let at = format!("{} at {width}x{height}", spec.name);
                let made = make(spec.name, Some((width, height)));
                assert_eq!(widget.size(), made.size(), "{at}");
                assert_eq!(widget.layers(), made.layers(), "{at}");
                let (sw, sh) = widget.size();
                if width >= min_w && height >= min_h {
                    assert_eq!((sw, sh), (width, height), "{at}: takes the size");
                } else {
                    assert_eq!(
                        (sw, sh),
                        (width.max(min_w), height.max(min_h)),
                        "{at}: the smallest"
                    );
                }
                for layer in widget.layers() {
                    let (x, y) = (layer.x as u32, layer.y as u32);
                    assert!(
                        layer.x >= 0
                            && layer.y >= 0
                            && x + layer.width <= sw
                            && y + layer.height <= sh,
                        "{at}: {layer:?} in the widget"
                    );
                }
            }
        }
    }

    #[test]
    fn shared_trackers_are_made_once_and_only_when_asked_for() {
        let shared = Shared::with_pit_store(None);
        assert!(shared.clock.get().is_none() && shared.gaps.get().is_none());
        // The gap history runs on the same clock as the relative and the standings: the clock is held by the
        // layout, the history, and the handle asked for here.
        let gaps = shared.gaps();
        assert_eq!(Rc::strong_count(&shared.clock()), 3);
        assert!(Rc::ptr_eq(&shared.gaps(), &gaps));
        assert!(Rc::ptr_eq(&shared.laps(), &shared.laps()));
        assert!(
            shared.pit.get().is_none(),
            "no widget asked for the pit tracker"
        );
        shared.reset();
    }
}
