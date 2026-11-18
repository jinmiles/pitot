//! The overlay's process: the data pipeline, GPUI's app with a window per widget, and the scheduler that drives
//! them.
//!
//! GPUI owns the thread's message loop. The scheduler is a task on GPUI's thread that sleeps until something wakes
//! it ([`Wake`]): a tick from the data thread while a shown widget is due by the next tick, a message from the data
//! or session thread, its timer, a tray or Pitot window command, a drag of a move handle, or a change of the
//! configuration directory. It updates the due widgets and asks GPUI to redraw only the windows of widgets that
//! report a change; GPUI draws them at its next display refresh. Win32 calls on GPUI's windows run between GPUI's
//! updates, never inside one, since their window procedures run GPUI's handlers.
//!
//! Each widget has a window of its own, sized to its box, while any widget is shown. With none shown for a moment,
//! the windows close and GPUI goes headless, which stops its display-refresh thread, so the sim being off costs
//! about one wake-up per second. A configuration change rebuilds the widgets and their windows; the data pipeline
//! keeps running.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Bounds, Context, Entity, GraphicalEnvironment,
    IntoElement, QuitMode, Render, Styled as _, Window, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions, WindowingRequest, canvas, point, px, size,
};
use pitot_irsdk::live::{ReaderConfig, SdkNames, VarSet};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tracing::{debug, info, warn};

use crate::config::{self, Config};
use crate::pipeline::{Display, Pipeline, RecordingConfig, Source, View, Wake};
use crate::platform::{
    self, ControlEvent, DirectoryWatch, Monitor, MoveHandle, ScreenRect, Tray, TrayCommand,
    WidgetMove,
};
use crate::render::{self, LayerPlace, Style};
use crate::wake::reason;
use crate::widgets::{self, Build, LayerSpec, MAX_LAYERS, Redraw, Shared, Widget};

/// Saving a file often takes several writes; reload once they settle.
const RELOAD_DELAY: Duration = Duration::from_millis(250);

/// How long a value from one of the Pitot window's sliders holds over the file's in a reload: the reload of the
/// slider's previous save must not undo a newer value, but a window that went away mid-drag must not hold it for
/// ever.
const PREVIEW_HOLD: Duration = Duration::from_secs(2);

/// The value of one of the Pitot window's sliders while it moves, and when it last moved.
#[derive(Debug, Clone, Copy)]
struct Preview {
    percent: u8,
    at: Instant,
}

impl Preview {
    fn now(percent: u8) -> Option<Self> {
        Some(Self {
            percent,
            at: Instant::now(),
        })
    }
}

/// The Pitot window's sliders while they move: `[general] background_opacity` and `text_size`.
#[derive(Debug, Clone, Copy, Default)]
struct Previews {
    background: Option<Preview>,
    text_size: Option<Preview>,
}

impl Previews {
    /// The background opacity to draw with: the slider's while it moves, else the configuration's.
    fn background(&self, config: &Config) -> u8 {
        self.background
            .map_or(config.general.background_opacity, |p| p.percent)
    }

    /// The text size to lay the widgets out at: the slider's while it moves, else the configuration's.
    fn text_size(&self, config: &Config) -> u8 {
        self.text_size
            .map_or(config.general.text_size, |p| p.percent)
    }

    /// Puts recent slider values into `config` and forgets old ones.
    fn hold(&mut self, config: &mut Config, now: Instant) {
        let recent = |p: &Preview| now.saturating_duration_since(p.at) < PREVIEW_HOLD;
        self.background = self.background.filter(recent);
        self.text_size = self.text_size.filter(recent);
        if let Some(p) = self.background {
            config.general.background_opacity = p.percent;
        }
        if let Some(p) = self.text_size {
            config.general.text_size = p.percent;
        }
    }
}

/// A text size change, for its line in the log once it is drawn.
#[derive(Debug, Clone, Copy)]
struct Rescaled {
    text_size: u8,
    started: Instant,
    /// Making the widgets again in widget pixels.
    laid_out: Duration,
}

/// What a reload changes, before a configuration that differs from the current one is used.
#[derive(Debug, PartialEq, Eq)]
enum Reload {
    Nothing,
    /// Only settings the overlay does not use while it runs, such as `[general] check_updates`: the configuration
    /// is taken, and nothing is drawn again.
    Unused,
    /// Only the look the Pitot window's sliders set, `[general] background_opacity` and `text_size`: the widgets
    /// are laid out at the text size and redraw; their windows stay.
    Appearance,
    Everything,
}

/// Compares the reloaded configuration with the current one. Recent slider values replace the file's in `fresh`,
/// and old ones are forgotten.
fn compare(current: &Config, fresh: &mut Config, previews: &mut Previews, now: Instant) -> Reload {
    previews.hold(fresh, now);
    if fresh == current {
        return Reload::Nothing;
    }
    let mut rest = fresh.clone();
    rest.general.check_updates = current.general.check_updates;
    if rest == *current {
        return Reload::Unused;
    }
    rest.general.background_opacity = current.general.background_opacity;
    rest.general.text_size = current.general.text_size;
    if rest == *current {
        Reload::Appearance
    } else {
        Reload::Everything
    }
}

/// How often the scheduler looks at the pipeline while no widget is shown: once per second with the sim not
/// running, and often enough while it is connected that widgets appear promptly when the driver gets in the car.
const IDLE_WAKE: Duration = Duration::from_secs(1);
const HIDDEN_WAKE: Duration = Duration::from_millis(250);

/// The sim's tick, 60 Hz.
const TICK: Duration = Duration::from_micros(16_667);

/// A widget due this soon is updated now: the data thread's ticks, which wake the scheduler for due widgets, come
/// a little early or late.
const DUE_SLACK: Duration = Duration::from_millis(4);

/// While ticks wake the scheduler, its timer stands by this much after the next due time, in case ticks stop.
const TICK_FALLBACK: Duration = Duration::from_millis(50);

/// With no widget shown for this long, the widget windows close and GPUI goes headless. Short visits to the garage
/// keep them.
const HEADLESS_AFTER: Duration = Duration::from_secs(2);

/// While GPUI changes its windowing mode or windows are being made, the scheduler looks again this often.
const PENDING_WAKE: Duration = Duration::from_millis(100);

/// When the scheduler wakes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Next {
    /// The timer.
    at: Instant,
    /// Whether the next tick wakes it too.
    ticks: bool,
}

/// When the scheduler wakes next: at the next tick while the sim is connected and a shown widget is due by then,
/// else at the earliest due time of the shown widgets, or an idle wake when none is shown. Hidden widgets keep
/// their past due time, so they draw at once when they appear, but they must not set the timer, or the scheduler
/// would wake at every clock tick.
fn next_wake(shown_due: impl Iterator<Item = Instant>, connected: bool, now: Instant) -> Next {
    match shown_due.min() {
        Some(due) if connected && due <= now + TICK + TICK / 2 => Next {
            at: due.max(now) + TICK_FALLBACK,
            ticks: true,
        },
        Some(due) => Next {
            at: due,
            ticks: false,
        },
        None => Next {
            at: now + if connected { HIDDEN_WAKE } else { IDLE_WAKE },
            ticks: false,
        },
    }
}

/// What a widget window's paint reads: the widget, where its layers go in the window, and the shared style.
struct Painted {
    widget: RefCell<Box<dyn Widget>>,
    places: RefCell<Vec<LayerPlace>>,
    style: Rc<RefCell<Style>>,
}

impl Painted {
    /// Draws every layer of the widget, bottom to top.
    fn paint(&self, window: &mut Window) {
        let (Ok(widget), Ok(places), Ok(style)) = (
            self.widget.try_borrow(),
            self.places.try_borrow(),
            self.style.try_borrow(),
        ) else {
            return;
        };
        #[cfg(feature = "alloc-count")]
        let _paint = crate::alloc_count::Phase::Paint.enter();
        for (index, place) in places.iter().enumerate() {
            render::paint_layer(window, &style, *place, |canvas| widget.draw(index, canvas));
        }
    }
}

/// The root view of a widget's window: one canvas over the whole window that paints the widget.
struct WidgetView {
    painted: Rc<Painted>,
}

impl Render for WidgetView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let painted = Rc::clone(&self.painted);
        canvas(|_, _, _| {}, move |_, (), window, _| painted.paint(window)).size_full()
    }
}

/// A widget's GPUI window.
struct WidgetWindow {
    handle: AnyWindowHandle,
    view: Entity<WidgetView>,
    hwnd: isize,
    /// Styled as an overlay window and placed once.
    ready: bool,
    shown: bool,
    /// Where it is on screen.
    rect: ScreenRect,
}

struct Slot {
    /// The widget's name in the configuration.
    name: &'static str,
    painted: Rc<Painted>,
    /// The widget's box on screen, in pixels. The text size leaves it as it is, except for a box made of text.
    size: (u32, u32),
    /// Whether the widget is laid out and drawn at the layout's text scale (see [`Widget::scales_with_text`]).
    scaled: bool,
    /// The box is the widget's own size in widget pixels and grows with the text: its table gives no size (the
    /// status line) and it was not resized.
    text_box: bool,
    /// Screen position of the widget's monitor's top-left corner.
    origin: (i32, i32),
    /// The widget's monitor, in screen pixels, and its scale (DPI / 96): the window never shows outside it.
    monitor: ScreenRect,
    monitor_scale: f32,
    /// Position on the monitor: as configured, kept inside the monitor ([`config::clamp_position`]), then as moved.
    x: i32,
    y: i32,
    /// Where the widget was laid out or last saved, so a click in *Move widgets* that moves nothing saves nothing,
    /// also for a widget the layout moved inside its monitor.
    placed: (i32, i32),
    interval: Duration,
    next_due: Instant,
    /// The widget must be fully redrawn, after creation, a rebuild, or a reset.
    stale: bool,
    /// The drag in progress changed the widget's size, which is saved when the drag ends.
    resized: bool,
    window: Option<WidgetWindow>,
}

/// Widgets built from one configuration.
struct Layout {
    slots: Vec<Slot>,
    /// The monitors the configuration names that are not connected, in order: their widgets went elsewhere.
    missing: Vec<usize>,
    /// The trackers the widgets share, such as the clock that times gaps for the standings and the relative.
    shared: Shared,
    /// The text scale the widgets are laid out at ([`config::text_scale`]).
    scale: f32,
}

/// The size of a box of `size` screen pixels in widget pixels at text scale `scale`: rounded up, so the widget's
/// background still reaches the layer's edges.
fn widget_pixels(size: u32, scale: f32) -> u32 {
    (size as f32 / scale).ceil().max(1.0) as u32
}

/// `size` widget pixels on screen at text scale `scale`, to the nearest pixel.
fn screen_pixels(size: u32, scale: f32) -> u32 {
    (size as f32 * scale).round().max(1.0) as u32
}

/// Lays `widget` out at text scale `scale` and returns its box on screen. A box made of text (`text_box`) is the
/// widget's own size at the scale. Any other box is `size`: the widget takes it in widget pixels through
/// [`Widget::resize`], which keeps what it follows of the session. With `raise`, as for a configured or dragged
/// size, the box grows to where the widget raised its size; a new text size leaves the box as it is and the
/// widget's content that no longer fits is left out.
fn fit(
    widget: &mut dyn Widget,
    text_box: bool,
    size: (u32, u32),
    scale: f32,
    raise: bool,
) -> (u32, u32) {
    if text_box {
        let (w, h) = widget.size();
        return (screen_pixels(w, scale), screen_pixels(h, scale));
    }
    let wanted = (widget_pixels(size.0, scale), widget_pixels(size.1, scale));
    if widget.size() != wanted {
        widget.resize(wanted.0, wanted.1);
    }
    let (w, h) = widget.size();
    let raised = |side: u32, wanted: u32, got: u32| {
        if raise && got > wanted {
            screen_pixels(got, scale)
        } else {
            side
        }
    };
    (raised(size.0, wanted.0, w), raised(size.1, wanted.1, h))
}

/// A widget's layer in screen pixels within its box `size`, for a widget of `widget` widget pixels drawn at text
/// scale `scale`; `spec` is in widget pixels. A layer that reaches the widget's right or bottom edge reaches the
/// box's.
fn layer_in_box(spec: LayerSpec, widget: (u32, u32), size: (u32, u32), scale: f32) -> LayerSpec {
    if scale == 1.0 {
        return spec;
    }
    let place = |at: i64| (at as f32 * scale).round() as i64;
    let span = |at: i32, length: u32, whole: u32, room: u32| {
        let start = place(i64::from(at));
        let end_at = i64::from(at) + i64::from(length);
        let end = if end_at >= i64::from(whole) {
            i64::from(room)
        } else {
            place(end_at)
        };
        let length = (end - start).clamp(1, i64::from(room.max(1)));
        (start as i32, length as u32)
    };
    let (x, width) = span(spec.x, spec.width, widget.0, size.0);
    let (y, height) = span(spec.y, spec.height, widget.1, size.1);
    LayerSpec {
        x,
        y,
        width,
        height,
    }
}

impl Slot {
    /// Screen pixels per widget pixel for this widget in a layout at text scale `scale`.
    fn scale(&self, scale: f32) -> f32 {
        if self.scaled { scale } else { 1.0 }
    }

    /// The smallest box on screen a drag may give the widget: its smallest size, at the text scale.
    fn min_box(&self, scale: f32) -> (u32, u32) {
        let (w, h) = self.painted.widget.borrow().min_size();
        let scale = self.scale(scale);
        let up = |v: u32| (v as f32 * scale).ceil() as u32;
        (up(w), up(h))
    }

    /// Where the widget's layers go in its window, for a layout at text scale `scale`.
    fn place_layers(&self, scale: f32) {
        let widget = self.painted.widget.borrow();
        let at = self.scale(scale);
        let places = widget
            .layers()
            .into_iter()
            .take(MAX_LAYERS)
            .map(|spec| {
                let spec = layer_in_box(spec, widget.size(), self.size, at);
                LayerPlace {
                    x: spec.x as f32,
                    y: spec.y as f32,
                    width: spec.width as f32,
                    height: spec.height as f32,
                    scale: at,
                }
            })
            .collect();
        *self.painted.places.borrow_mut() = places;
    }

    /// Where the widget is on screen: its box.
    fn screen_rect(&self) -> ScreenRect {
        let (width, height) = self.size;
        ScreenRect {
            x: self.origin.0 + self.x,
            y: self.origin.1 + self.y,
            width: i32::try_from(width).unwrap_or(i32::MAX),
            height: i32::try_from(height).unwrap_or(i32::MAX),
        }
    }
}

impl Layout {
    /// Makes every enabled widget of the registry, with its variables resolved in `vars` (the pipeline's set), laid
    /// out at text scale `scale`, each on its monitor of `monitors` (in the order of [`platform::monitors`]) and
    /// inside it.
    fn build(
        config: &Config,
        monitors: &[Monitor],
        vars: &VarSet,
        scale: f32,
        style: &Rc<RefCell<Style>>,
    ) -> Self {
        let primary = monitors.iter().position(|m| m.primary).unwrap_or(0);
        let general = config.general.monitor;
        let mut missing: Vec<usize> = general
            .filter(|&i| i >= monitors.len())
            .into_iter()
            .collect();
        let mut slots = Vec::new();
        let now = Instant::now();
        let shared = Shared::new();
        for (name, widget_config) in config.widgets() {
            if !widget_config.enabled {
                continue;
            }
            let cx = Build {
                config: widget_config,
                shared: &shared,
            };
            let Some(mut widget) = widgets::make_widget(name, &cx, vars) else {
                continue;
            };
            // Made as configured, a widget has its box: the configured size raised to its smallest.
            let scaled = widget.scales_with_text();
            let text_box =
                scaled && widget_config.width.is_none() && widget_config.height.is_none();
            let made = widget.size();
            let size = fit(
                widget.as_mut(),
                text_box,
                made,
                if scaled { scale } else { 1.0 },
                false,
            );
            if let Some(index) = widget_config.monitor
                && index >= monitors.len()
                && !missing.contains(&index)
            {
                missing.push(index);
            }
            let monitor =
                config::resolve_monitor(widget_config.monitor, general, monitors.len(), primary);
            // Only without any monitor.
            let Some((rect, dpi)) = monitors.get(monitor).map(|m| (m.rect, m.dpi)) else {
                continue;
            };
            let interval = widget_config
                .rate_hz
                .map_or(widget.interval(), |hz| Duration::from_secs_f32(1.0 / hz));
            // Placed on a larger monitor, a widget would be cut off or out of sight; the file keeps its numbers.
            let (x, y) = config::clamp_position(
                widget_config.x,
                widget_config.y,
                size,
                (rect.width, rect.height),
            );
            if (x, y) != (widget_config.x, widget_config.y) {
                info!(
                    widget = name,
                    monitor,
                    x,
                    y,
                    "widget shown inside its monitor, away from its configured place"
                );
            }
            let slot = Slot {
                name,
                painted: Rc::new(Painted {
                    widget: RefCell::new(widget),
                    places: RefCell::new(Vec::new()),
                    style: Rc::clone(style),
                }),
                size,
                scaled,
                text_box,
                origin: (rect.x, rect.y),
                monitor: rect,
                monitor_scale: dpi.max(1) as f32 / 96.0,
                x,
                y,
                placed: (x, y),
                interval,
                next_due: now,
                stale: true,
                resized: false,
                window: None,
            };
            slot.place_layers(scale);
            slots.push(slot);
        }
        missing.sort_unstable();
        Self {
            slots,
            missing,
            shared,
            scale,
        }
    }

    /// Lays the widgets out again at text scale `scale`, each in its box: see [`fit`]. The widgets keep what they
    /// follow of the session; their windows take new sizes where a box changed (a box made of text). Allocates.
    fn rescale(&mut self, scale: f32) {
        for slot in &mut self.slots {
            let at = slot.scale(scale);
            let size = slot.size;
            slot.size = fit(
                slot.painted.widget.borrow_mut().as_mut(),
                slot.text_box,
                size,
                at,
                false,
            );
            slot.stale = true;
            slot.place_layers(scale);
        }
        self.scale = scale;
    }
}

/// Which widgets show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// Each widget decides from the display state.
    Normal,
    /// None: *Hide widgets* is chosen.
    Hidden,
    /// All of them, also without the sim: widgets are being moved.
    All,
}

impl Shown {
    fn visible(self, widget: &dyn Widget, display: Display) -> bool {
        match self {
            Self::Normal => widget.visible(display),
            Self::Hidden => false,
            Self::All => true,
        }
    }
}

/// A move handle over each widget, for dragging and resizing them while *Move widgets* is chosen, in a layout at
/// text scale `scale`.
fn move_handles(slots: &[Slot], scale: f32) -> Vec<MoveHandle> {
    slots
        .iter()
        .enumerate()
        .filter_map(|(id, slot)| {
            MoveHandle::create(id, slot.screen_rect(), slot.min_box(scale))
                .inspect_err(|e| warn!("widgets.{} cannot be moved: {e}", slot.name))
                .ok()
        })
        .collect()
}

/// Saves what the drag that ended changed of slot's widget: its place when it moved, its size when it was resized.
/// A click that changed nothing writes nothing. The configuration in memory follows, so a reload sees no change.
/// Returns whether the file was written.
fn save_drag(config: &mut Config, slot: &mut Slot) -> bool {
    let resized = std::mem::take(&mut slot.resized);
    let (width, height) = slot.size;
    let Some(widget) = config.widget_mut(slot.name) else {
        return false;
    };
    let position = ((slot.x, slot.y) != slot.placed).then_some((slot.x, slot.y));
    let size = resized.then_some((width, height));
    if position.is_none() && size.is_none() {
        return false;
    }
    if position.is_some() {
        (widget.x, widget.y) = (slot.x, slot.y);
        slot.placed = (slot.x, slot.y);
    }
    if resized {
        (widget.width, widget.height) = (Some(width), Some(height));
    }
    let saved = config::config_file()
        .ok_or_else(|| "no APPDATA directory".to_owned())
        .and_then(|path| {
            config::save_placement(&path, slot.name, position, size).map_err(|e| e.to_string())
        });
    match saved {
        Ok(()) => {
            info!(
                widget = slot.name,
                x = slot.x,
                y = slot.y,
                width,
                height,
                "widget moved or resized"
            );
            true
        }
        Err(e) => {
            warn!(
                widget = slot.name,
                "widget moved or resized but not saved: {e}"
            );
            false
        }
    }
}

/// Loads the configuration file, falling back to defaults (and saying why) when it cannot be used.
fn load_config() -> Config {
    let Some(path) = config::config_file() else {
        warn!("no APPDATA directory; using the default configuration");
        return Config::default();
    };
    config::load_or_create(&path).unwrap_or_else(|e| {
        warn!("using the default configuration: {e}");
        Config::default()
    })
}

/// How the overlay's process was started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Launch {
    /// Started again by the installer after the Pitot window installed an update: the window opens again, also
    /// with `[general] start_in_tray`, since it was open when the update was chosen.
    pub after_update: bool,
}

/// The overlay process, one per Windows session: logging, then [`run`]. A second start opens the Pitot window
/// instead and exits at once.
pub fn main(launch: Launch) -> ExitCode {
    // Checked before logging starts, so a second launch leaves the running overlay's log alone.
    let instance = platform::SingleInstance::acquire();
    if matches!(instance, Ok(None)) {
        let _ = platform::open_control();
        return ExitCode::SUCCESS;
    }
    crate::log::init();
    if let Err(e) = &instance {
        warn!("cannot check for a running overlay: {e}");
    }
    if launch.after_update {
        info!("started again after an update");
    }
    let code = match run(launch) {
        Ok(()) => {
            info!("exiting");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!("stopped: {e}");
            ExitCode::FAILURE
        }
    };
    // Held until the overlay is done.
    drop(instance);
    code
}

/// GPUI's windowing mode, as the scheduler asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Windowing {
    Headless,
    Windowed,
    /// A change was asked for and has not finished.
    Changing,
}

/// A Win32 call on a widget window, made between GPUI's updates.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// Style a new, hidden window as an overlay window and place it, still hidden; then show it when it is where it
    /// belongs on `monitor`.
    Ready {
        hwnd: isize,
        rect: ScreenRect,
        monitor: ScreenRect,
    },
    Place {
        hwnd: isize,
        rect: ScreenRect,
    },
    /// Show a window, only when it is where it belongs on `monitor`, or hide it.
    Show {
        hwnd: isize,
        show: bool,
        rect: ScreenRect,
        monitor: ScreenRect,
    },
}

/// What a scheduler step leaves for after GPUI's update.
#[derive(Default)]
struct Plan {
    actions: Vec<Action>,
    windowing: Option<WindowingRequest>,
    /// Move handles to make once the windows are placed.
    handles: bool,
}

/// The overlay's state on GPUI's thread.
struct Overlay {
    config: Config,
    stamp: Option<config::Stamp>,
    reload_at: Option<Instant>,
    layout: Layout,
    view: View,
    tray: Tray,
    _watch: Option<DirectoryWatch>,
    documents: Option<PathBuf>,
    style: Rc<RefCell<Style>>,
    /// Chosen from the tray menu: no widget is updated or drawn.
    hidden: bool,
    /// While *Move widgets* is chosen: a handle over each widget, once the windows are placed.
    moving: bool,
    handles: Option<Vec<MoveHandle>>,
    moves: Vec<WidgetMove>,
    /// While one of the Pitot window's sliders moves, the value it shows.
    previews: Previews,
    /// A text size change waiting for the frame that draws it.
    rescaled: Option<Rescaled>,
    windowing: Windowing,
    /// Since when no widget is shown, while windows are open.
    unseen_since: Option<Instant>,
    /// The actions of the last step, kept for their buffer.
    actions: Vec<Action>,
}

fn config_stamp() -> Option<config::Stamp> {
    config::config_file().and_then(|path| config::stamp(&path))
}

impl Overlay {
    fn shown(&self) -> Shown {
        if self.moving {
            Shown::All
        } else if self.hidden {
            Shown::Hidden
        } else {
            Shown::Normal
        }
    }

    /// Takes tray and Pitot window commands, and tells the Pitot window the state they leave.
    fn take_commands(&mut self) {
        while let Some(command) = platform::take_tray_command() {
            match command {
                TrayCommand::Reload => self.reload_at = Some(Instant::now()),
                TrayCommand::SetHidden(on) if on != self.hidden => {
                    self.hidden = on;
                    self.tray.set_hidden(on);
                    info!(hidden = on, "widgets hidden or shown");
                    self.layout.slots.iter_mut().for_each(|s| s.stale = true);
                }
                TrayCommand::SetMoving(on) if on != self.moving => {
                    self.moving = on;
                    if !on {
                        self.handles = None;
                    }
                    self.tray.set_moving(on);
                    if on && self.hidden {
                        self.hidden = false;
                        self.tray.set_hidden(false);
                    }
                    info!(moving = on, "moving widgets started or ended");
                    self.layout.slots.iter_mut().for_each(|s| s.stale = true);
                }
                TrayCommand::SetHidden(_) | TrayCommand::SetMoving(_) => {}
                TrayCommand::BackgroundOpacity { percent, saved } => {
                    if saved {
                        // The reload of the window's write then finds the file as the overlay has it.
                        self.config.general.background_opacity = percent;
                        self.previews.background = None;
                    } else {
                        self.previews.background = Preview::now(percent);
                    }
                    // Applied below. The window needs no answer.
                    continue;
                }
                TrayCommand::TextSize { percent, saved } => {
                    if saved {
                        self.config.general.text_size = percent;
                        self.previews.text_size = None;
                    } else {
                        self.previews.text_size = Preview::now(percent);
                    }
                    continue;
                }
            }
            // Also when nothing changed, so the window's switches settle on the overlay's state.
            platform::notify_control(ControlEvent::State {
                hidden: self.hidden,
                moving: self.moving,
            });
        }
    }

    /// Takes the drags of the move handles.
    fn take_moves(&mut self, plan: &mut Plan) {
        platform::take_moves(&mut self.moves);
        let scale = self.layout.scale;
        for moved in std::mem::take(&mut self.moves).drain(..) {
            let Some(slot) = self.layout.slots.get_mut(moved.id) else {
                continue;
            };
            slot.x = moved.x - slot.origin.0;
            slot.y = moved.y - slot.origin.1;
            let size = (
                u32::try_from(moved.width).unwrap_or(0),
                u32::try_from(moved.height).unwrap_or(0),
            );
            if size != slot.size {
                // A resized widget has a box of its own from now on, at any text size.
                slot.text_box = false;
                let at = slot.scale(scale);
                slot.size = fit(
                    slot.painted.widget.borrow_mut().as_mut(),
                    false,
                    size,
                    at,
                    true,
                );
                slot.resized = true;
                slot.stale = true;
                slot.place_layers(scale);
            }
            let rect = slot.screen_rect();
            if let Some(window) = slot.window.as_mut() {
                window.rect = rect;
                plan.actions.push(Action::Place {
                    hwnd: window.hwnd,
                    rect: window.rect,
                });
            }
            if moved.done {
                // The watch sees the write; the new stamp keeps it from reloading.
                if save_drag(&mut self.config, slot) {
                    self.stamp = config_stamp();
                }
                // The handle follows a widget that took another size than the one dragged to.
                let shown = slot.screen_rect();
                if let Some(handle) = self.handles.iter().flatten().find(|h| h.id() == moved.id)
                    && (shown.width, shown.height) != (moved.width, moved.height)
                {
                    handle.place(shown);
                }
            }
        }
    }

    /// Reloads the configuration file once its writes settled: see [`compare`] for what a change redoes.
    fn reload(&mut self, cx: &mut App) {
        if !self.reload_at.is_some_and(|at| Instant::now() >= at) {
            return;
        }
        self.reload_at = None;
        let Some(path) = config::config_file() else {
            return;
        };
        let reloaded = config::load_or_create(&path).map(|mut fresh| {
            let change = compare(&self.config, &mut fresh, &mut self.previews, Instant::now());
            (fresh, change)
        });
        match reloaded {
            Ok((_, Reload::Nothing)) => {}
            Ok((fresh, Reload::Unused)) => {
                self.config = fresh;
                info!("configuration reloaded: nothing the widgets use");
                platform::notify_control(ControlEvent::ConfigChanged);
            }
            Ok((fresh, Reload::Appearance)) => {
                // Applied after this: the background redraws, the text lays the widgets out.
                self.config = fresh;
                info!("configuration reloaded: background opacity or text size");
                platform::notify_control(ControlEvent::ConfigChanged);
            }
            Ok((fresh, Reload::Everything)) => {
                let monitors = platform::monitors();
                if monitors.is_empty() {
                    // Windows can list none while displays change; the next change of the file applies.
                    warn!("no monitors found: keeping the previous configuration");
                    return;
                }
                if fresh.telemetry != self.config.telemetry {
                    self.view.set_telemetry_folders(config::telemetry_dirs(
                        &fresh.telemetry,
                        self.documents.clone(),
                    ));
                }
                self.close_windows(cx);
                self.handles = None;
                let scale = config::text_scale(self.previews.text_size(&fresh));
                let missing = std::mem::take(&mut self.layout.missing);
                self.layout =
                    Layout::build(&fresh, &monitors, self.view.var_set(), scale, &self.style);
                log_missing(&missing, &self.layout.missing, monitors.len());
                self.config = fresh;
                info!("configuration reloaded");
                platform::notify_control(ControlEvent::ConfigChanged);
            }
            Err(e) => warn!("keeping the previous configuration: {e}"),
        }
    }

    /// Applies the background opacity and the text size the configuration or a slider sets.
    fn appearance(&mut self, plan: &mut Plan) {
        // A changed text size, from the slider or the file, lays the widgets out again in their boxes.
        let text_size = self.previews.text_size(&self.config);
        let scale = config::text_scale(text_size);
        let background = self.previews.background(&self.config);
        let mut style = self.style.borrow_mut();
        if self.layout.scale != scale || style.text_scale != render::valid_scale(scale) {
            let started = Instant::now();
            self.layout.rescale(scale);
            style.text_scale = render::valid_scale(scale);
            self.rescaled = Some(Rescaled {
                text_size,
                started,
                laid_out: started.elapsed(),
            });
            for slot in &mut self.layout.slots {
                let rect = slot.screen_rect();
                if let Some(window) = slot.window.as_mut()
                    && rect != window.rect
                {
                    window.rect = rect;
                    plan.actions.push(Action::Place {
                        hwnd: window.hwnd,
                        rect,
                    });
                }
            }
            // The smallest box a drag may give a widget changes with the text.
            if self.moving {
                self.handles = None;
                plan.handles = true;
            }
        }
        // A changed background opacity redraws every widget once, as hidden widgets do when they show again.
        if style.background != background {
            style.background = background;
            self.layout.slots.iter_mut().for_each(|s| s.stale = true);
        }
    }

    /// Closes every widget window.
    fn close_windows(&mut self, cx: &mut App) {
        for slot in &mut self.layout.slots {
            if let Some(window) = slot.window.take() {
                let _ = window
                    .handle
                    .update(cx, |_, window, _| window.remove_window());
            }
        }
    }

    /// Opens a window for each shown widget that has none, hidden; after the update it is styled and placed while
    /// still hidden, and shown once it is where it belongs. A widget gets its window when it first shows: GPUI keeps
    /// asking windows that never showed to paint, at every display refresh.
    fn open_windows(&mut self, cx: &mut App, plan: &mut Plan, shown: Shown, display: Display) {
        for slot in &mut self.layout.slots {
            if slot.window.is_some()
                || !shown.visible(slot.painted.widget.borrow().as_ref(), display)
            {
                continue;
            }
            let rect = slot.screen_rect();
            match open_widget_window(cx, &slot.painted, rect, slot.monitor_scale) {
                Ok(window) => {
                    slot.window = Some(window);
                    slot.stale = true;
                }
                Err(e) => warn!(widget = slot.name, "widget window not made: {e}"),
            }
        }
        for slot in &self.layout.slots {
            if let Some(window) = slot.window.as_ref().filter(|w| !w.ready) {
                plan.actions.push(Action::Ready {
                    hwnd: window.hwnd,
                    rect: window.rect,
                    monitor: slot.monitor,
                });
            }
        }
    }

    /// One pass of the scheduler, inside GPUI's update: takes what woke it, updates the due widgets, asks GPUI to
    /// redraw the windows of those that changed, and leaves Win32 calls on GPUI's windows for after the update.
    fn step(&mut self, reasons: u32, cx: &mut App) -> Plan {
        let mut plan = Plan {
            actions: std::mem::take(&mut self.actions),
            ..Plan::default()
        };
        plan.actions.clear();
        if reasons & reason::COMMAND != 0 {
            self.take_commands();
        }
        if reasons & reason::MOVE != 0 {
            self.take_moves(&mut plan);
        }
        if reasons & reason::CONFIG != 0 {
            // The directory also holds the log, whose writes signal the watch too; only a changed file reloads.
            let current = config_stamp();
            if current != self.stamp {
                self.stamp = current;
                self.reload_at = Some(Instant::now() + RELOAD_DELAY);
            }
        }
        self.reload(cx);
        self.appearance(&mut plan);
        let changes = self.view.poll();
        if changes.connection {
            match self.view.source() {
                Some(Source::Sim) => info!("sim connected"),
                Some(Source::Capture { first: true }) => {
                    info!("replay: showing the session capture")
                }
                Some(Source::SimAgain) => info!("replay: showing live values again"),
                Some(Source::Capture { first: false }) => {}
                None => log_counters(&self.view, "sim disconnected"),
            }
            for slot in &mut self.layout.slots {
                slot.painted.widget.borrow_mut().reset();
                slot.stale = true;
            }
            self.layout.shared.reset();
        }

        let display = self.view.display();
        let shown = self.shown();
        let any_shown = self
            .layout
            .slots
            .iter()
            .any(|s| shown.visible(s.painted.widget.borrow().as_ref(), display));
        let now = Instant::now();
        let mut pending = false;
        if any_shown {
            self.unseen_since = None;
            match self.windowing {
                Windowing::Headless => {
                    self.windowing = Windowing::Changing;
                    plan.windowing =
                        Some(WindowingRequest::Windowed(GraphicalEnvironment::detect()));
                    pending = true;
                }
                Windowing::Changing => pending = true,
                Windowing::Windowed => self.open_windows(cx, &mut plan, shown, display),
            }
        } else if self.windowing == Windowing::Windowed {
            let since = *self.unseen_since.get_or_insert(now);
            if !self.moving && now.saturating_duration_since(since) >= HEADLESS_AFTER {
                if self.layout.slots.iter().any(|s| s.window.is_some()) {
                    self.close_windows(cx);
                    pending = true;
                } else if cx.windows().is_empty() {
                    self.windowing = Windowing::Changing;
                    plan.windowing = Some(WindowingRequest::Headless);
                    pending = true;
                } else {
                    pending = true;
                }
            }
        }

        for slot in &mut self.layout.slots {
            let Some(window) = slot.window.as_mut() else {
                continue;
            };
            let visible = shown.visible(slot.painted.widget.borrow().as_ref(), display);
            if visible != window.shown && window.ready {
                window.shown = visible;
                plan.actions.push(Action::Show {
                    hwnd: window.hwnd,
                    show: visible,
                    rect: window.rect,
                    monitor: slot.monitor,
                });
                if visible {
                    slot.stale = true;
                }
            }
            if !visible || (now + DUE_SLACK < slot.next_due && !slot.stale) {
                continue;
            }
            slot.next_due = now + slot.interval;
            let mut redraws = [Redraw::Nothing; MAX_LAYERS];
            {
                #[cfg(feature = "alloc-count")]
                let _update = crate::alloc_count::Phase::Update.enter();
                slot.painted
                    .widget
                    .borrow_mut()
                    .update(&mut self.view, now, &mut redraws);
            }
            if std::mem::take(&mut slot.stale) || redraws.iter().any(|r| *r != Redraw::Nothing) {
                window.view.update(cx, |_, cx| cx.notify());
            }
        }
        if self.moving && self.handles.is_none() && self.windowing == Windowing::Windowed {
            plan.handles = true;
        }

        // Once per text size the slider or the file sets: what laying out took.
        if let Some(done) = self.rescaled.take() {
            let ms = |d: Duration| (d.as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
            info!(
                text_size = done.text_size,
                layout_ms = ms(done.laid_out),
                total_ms = ms(done.started.elapsed()),
                "text size applied"
            );
        }

        let mut next = next_wake(
            self.layout
                .slots
                .iter()
                .filter(|s| {
                    s.window.as_ref().is_some_and(|w| w.shown)
                        && shown.visible(s.painted.widget.borrow().as_ref(), display)
                })
                .map(|s| s.next_due),
            self.view.is_connected(),
            now,
        );
        if pending {
            next = Next {
                at: next.at.min(now + PENDING_WAKE),
                ticks: next.ticks,
            };
        }
        if let Some(since) = self.unseen_since
            && self.windowing == Windowing::Windowed
            && !self.moving
        {
            next.at = next.at.min(since + HEADLESS_AFTER);
        }
        if let Some(at) = self.reload_at {
            next.at = next.at.min(at);
        }
        self.view.wake().arm_ticks(next.ticks);
        self.tray
            .set_timer(next.at.saturating_duration_since(Instant::now()));
        plan
    }

    /// Makes the Win32 calls a step left, after GPUI's update.
    fn apply(&mut self, plan: &mut Plan) {
        for action in plan.actions.drain(..) {
            match action {
                Action::Ready {
                    hwnd,
                    rect,
                    monitor,
                } => {
                    // Still hidden: styled and placed before it ever shows.
                    if let Err(e) = platform::style_widget_window(hwnd) {
                        warn!("widget window not made an overlay window: {e}");
                        continue;
                    }
                    platform::place_widget_window(hwnd, rect, false);
                    let placed = platform::widget_window_rect(hwnd);
                    if placed != rect || !within(placed, monitor) {
                        warn!(
                            ?placed,
                            ?rect,
                            "widget window not where it belongs; not shown"
                        );
                        continue;
                    }
                    platform::show_widget_window(hwnd, true);
                    for slot in &mut self.layout.slots {
                        if let Some(window) = slot.window.as_mut().filter(|w| w.hwnd == hwnd) {
                            window.ready = true;
                            window.shown = true;
                        }
                    }
                }
                Action::Place { hwnd, rect } => platform::place_widget_window(hwnd, rect, false),
                Action::Show {
                    hwnd,
                    show: true,
                    rect,
                    monitor,
                } => {
                    // Shown only where it belongs: on its monitor, at its place.
                    if !within(platform::widget_window_rect(hwnd), monitor) {
                        platform::place_widget_window(hwnd, rect, false);
                    }
                    let placed = platform::widget_window_rect(hwnd);
                    if within(placed, monitor) {
                        platform::show_widget_window(hwnd, true);
                    } else {
                        warn!(
                            ?placed,
                            ?monitor,
                            "widget window outside its monitor; not shown"
                        );
                    }
                }
                Action::Show { hwnd, .. } => platform::show_widget_window(hwnd, false),
            }
        }
        if plan.handles
            && self.moving
            && self
                .layout
                .slots
                .iter()
                .all(|s| s.window.as_ref().is_some_and(|w| w.ready))
        {
            // Made after the windows, so they are above them.
            self.handles = Some(move_handles(&self.layout.slots, self.layout.scale));
        }
        self.actions = std::mem::take(&mut plan.actions);
    }
}

/// Whether `rect` lies within `monitor`.
fn within(rect: ScreenRect, monitor: ScreenRect) -> bool {
    rect.width > 0
        && rect.height > 0
        && rect.x >= monitor.x
        && rect.y >= monitor.y
        && rect.x + rect.width <= monitor.x + monitor.width
        && rect.y + rect.height <= monitor.y + monitor.height
}

/// Opens a widget's GPUI window, hidden: a transparent popup that is never focused, for `rect` on the monitor of
/// scale `scale` (DPI / 96). Its bounds and display are given in GPUI's logical pixels of that monitor, so a window
/// GPUI showed by itself would show there too; the scheduler shows it only after placing it in physical pixels.
fn open_widget_window(
    cx: &mut App,
    painted: &Rc<Painted>,
    rect: ScreenRect,
    scale: f32,
) -> Result<WidgetWindow, String> {
    let logical = |v: i32| px(v as f32 / scale);
    let bounds = Bounds::new(
        point(logical(rect.x), logical(rect.y)),
        size(logical(rect.width), logical(rect.height)),
    );
    let display_id = cx
        .displays()
        .into_iter()
        .find(|display| display.bounds().contains(&bounds.center()))
        .map(|display| display.id());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id,
        window_background: WindowBackgroundAppearance::Transparent,
        // Overlay windows are never active; GPUI would draw them at most 30 times a second otherwise.
        inactive_frame_interval: None,
        ..Default::default()
    };
    let painted = Rc::clone(painted);
    let mut hwnd = None;
    let handle = cx
        .open_window(options, |window, cx| {
            hwnd = window_hwnd(window);
            // Closing a widget window the usual way (the benchmark runner, End task) quits like the tray's Quit.
            window.on_window_should_close(cx, |_, _| {
                platform::quit();
                false
            });
            cx.new(|_| WidgetView { painted })
        })
        .map_err(|e| e.to_string())?;
    let view = handle
        .update(cx, |_, _, cx| cx.entity())
        .map_err(|e| e.to_string())?;
    let hwnd = hwnd.ok_or_else(|| "the window has no handle".to_owned())?;
    Ok(WidgetWindow {
        handle: handle.into(),
        view,
        hwnd,
        ready: false,
        shown: false,
        rect,
    })
}

fn window_hwnd(window: &Window) -> Option<isize> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// Runs the scheduler until the overlay quits.
async fn schedule(overlay: Rc<RefCell<Option<Overlay>>>, wake: Arc<Wake>, cx: &mut AsyncApp) {
    loop {
        let reasons = wake.wait().await;
        let plan = cx.update(|cx| {
            overlay
                .borrow_mut()
                .as_mut()
                .map(|overlay| overlay.step(reasons, cx))
        });
        let Some(mut plan) = plan else {
            return;
        };
        if let Some(request) = plan.windowing.take() {
            let windowed = matches!(request, WindowingRequest::Windowed(_));
            let task = cx.update(|cx| cx.request_windowing(request));
            let result = task.await;
            if let Some(overlay) = overlay.borrow_mut().as_mut() {
                overlay.windowing = match (&result, windowed) {
                    (Ok(()), true) => Windowing::Windowed,
                    (Ok(()), false) => Windowing::Headless,
                    // Still as it was: windowed if asked for headless, and the other way round.
                    (Err(_), true) => Windowing::Headless,
                    (Err(_), false) => Windowing::Windowed,
                };
                if let Err(e) = &result {
                    debug!(windowed, "windowing not changed: {e}");
                } else {
                    debug!(windowed, "windowing changed");
                }
            }
            wake.signal(reason::TIMER);
        }
        if let Some(overlay) = overlay.borrow_mut().as_mut() {
            overlay.apply(&mut plan);
        }
    }
}

/// Runs the overlay until Quit is chosen from the tray icon or the Pitot window, or a window is closed.
pub fn run(launch: Launch) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "alloc-count")]
    crate::alloc_count::start();
    platform::enable_dpi_awareness();
    let config = load_config();
    let monitors = platform::monitors();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        monitors = monitors.len(),
        "overlay started"
    );
    if monitors.is_empty() {
        return Err("no monitors found".into());
    }
    let recording = config
        .recording
        .enabled
        .then(config::recordings_dir)
        .flatten()
        .map(|dir| RecordingConfig {
            dir,
            max_bytes: (f64::from(config.recording.max_gb) * 1.0e9) as u64,
        });
    // First, so the widgets can resolve their variables in its set.
    let (pipeline, view) = Pipeline::start(
        SdkNames::sim(),
        ReaderConfig::default(),
        config::capture_dirs(),
        recording,
    )?;
    let wake = Arc::clone(view.wake());
    platform::set_wake(Arc::clone(&wake));
    let watch = config::config_dir().and_then(|dir| {
        let wake = Arc::clone(&wake);
        DirectoryWatch::start(&dir, move || wake.signal(reason::CONFIG)).ok()
    });
    let documents = platform::documents_dir();
    view.set_telemetry_folders(config::telemetry_dirs(&config.telemetry, documents.clone()));
    let scale = config::text_scale(config.general.text_size);
    let style = Rc::new(RefCell::new(Style {
        text_scale: render::valid_scale(scale),
        background: config.general.background_opacity,
        ..Style::default()
    }));
    let layout = Layout::build(&config, &monitors, view.var_set(), scale, &style);
    log_missing(&[], &layout.missing, monitors.len());
    let tray = Tray::create()?;
    // A Pitot window open from before learns that the overlay runs now.
    platform::notify_control(ControlEvent::State {
        hidden: false,
        moving: false,
    });
    if (launch.after_update || !config.general.start_in_tray)
        && let Err(e) = platform::open_control()
    {
        warn!("cannot open the Pitot window: {e}");
    }
    let overlay = Rc::new(RefCell::new(Some(Overlay {
        config,
        stamp: config_stamp(),
        reload_at: None,
        layout,
        view,
        tray,
        _watch: watch,
        documents,
        style,
        hidden: false,
        moving: false,
        handles: None,
        moves: Vec::new(),
        previews: Previews::default(),
        rescaled: None,
        windowing: Windowing::Headless,
        unseen_since: None,
        actions: Vec::new(),
    })));

    // Headless until a widget shows: no GPU device and no display-refresh thread while nothing is drawn.
    let scheduled = Rc::clone(&overlay);
    // The first pass lays the widgets out and opens the windows of those shown.
    wake.signal(reason::TIMER);
    gpui_platform::application()
        .with_quit_mode(QuitMode::Explicit)
        .with_windowing(WindowingRequest::Headless)
        .run(move |cx: &mut App| {
            cx.spawn(async move |cx| schedule(scheduled, wake, cx).await)
                .detach();
        });

    if let Some(overlay) = overlay.borrow_mut().take() {
        log_counters(&overlay.view, "data counters at exit");
        #[cfg(feature = "alloc-count")]
        crate::alloc_count::log_totals();
    }
    // Quitting Pitot closes its window too.
    platform::notify_control(ControlEvent::Close);
    drop(pipeline);
    Ok(())
}

/// Says once which monitors the configuration names that are not connected, when they differ from the previous
/// layout's: the widgets meant for them show on the `[general] monitor`, or on the primary monitor.
fn log_missing(before: &[usize], now: &[usize], connected: usize) {
    if now != before && !now.is_empty() {
        warn!(
            missing = ?now,
            connected,
            "monitor not connected: its widgets show on the general monitor or the primary one"
        );
    }
}

/// Logs the data path's counters since start: ticks read, ticks missed before they could be read, torn reads,
/// read errors, and input samples the render thread lost. Missed ticks and dropped samples are gaps in the input
/// trace.
fn log_counters(view: &View, event: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    let stats = view.stats();
    info!(
        ticks = stats.ticks.load(Relaxed),
        missed = stats.missed.load(Relaxed),
        torn = stats.torn.load(Relaxed),
        errors = stats.errors.load(Relaxed),
        dropped_samples = view.dropped_samples_total(),
        "{event}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_widgets_do_not_set_the_timer() {
        let now = Instant::now();
        let past = now - Duration::from_secs(5);
        let soon = now + Duration::from_millis(16);
        let later = now + Duration::from_millis(100);
        let ticks = |at| Next { at, ticks: true };
        let timer = |at| Next { at, ticks: false };
        // Connected: a widget due by the next tick waits for the tick, with the timer standing by.
        assert_eq!(
            next_wake([soon, now + IDLE_WAKE].into_iter(), true, now),
            ticks(soon + TICK_FALLBACK)
        );
        assert_eq!(
            next_wake([past].into_iter(), true, now),
            ticks(now + TICK_FALLBACK),
            "a due widget draws at the next tick"
        );
        // Later, or without the sim, the timer.
        assert_eq!(next_wake([later].into_iter(), true, now), timer(later));
        assert_eq!(next_wake([soon].into_iter(), false, now), timer(soon));
        assert_eq!(
            next_wake([past].into_iter(), false, now),
            timer(past),
            "without the sim, a due widget draws at once"
        );
        assert_eq!(
            next_wake(std::iter::empty(), false, now),
            timer(now + IDLE_WAKE)
        );
        assert_eq!(
            next_wake(std::iter::empty(), true, now),
            timer(now + HIDDEN_WAKE)
        );
    }

    #[test]
    fn a_reload_does_not_undo_the_slider() {
        let now = Instant::now();
        let with = |percent| {
            let mut config = Config::default();
            config.general.background_opacity = percent;
            config
        };
        let background = |percent| Previews {
            background: Some(Preview { percent, at: now }),
            text_size: None,
        };
        // The window saved 50 and the overlay took it; the reload of that write changes nothing.
        let mut previews = Previews::default();
        let mut fresh = with(50);
        assert_eq!(
            compare(&with(50), &mut fresh, &mut previews, now),
            Reload::Nothing
        );
        // The slider moved on to 30 before that reload: the file's 50 does not win over it.
        let mut previews = background(30);
        let mut fresh = with(50);
        assert_eq!(
            compare(&with(50), &mut fresh, &mut previews, now + RELOAD_DELAY),
            Reload::Appearance
        );
        assert_eq!(fresh.general.background_opacity, 30);
        assert!(previews.background.is_some());
        // A window that went away mid-drag: after a while the file's value is used again.
        let mut fresh = with(50);
        let later = now + PREVIEW_HOLD + RELOAD_DELAY;
        assert_eq!(
            compare(&with(50), &mut fresh, &mut previews, later),
            Reload::Nothing
        );
        assert!(previews.background.is_none());
        // A value edited by hand redraws without a rebuild; any other change rebuilds.
        let mut fresh = with(10);
        assert_eq!(
            compare(&with(50), &mut fresh, &mut Previews::default(), now),
            Reload::Appearance
        );
        let mut fresh = with(10);
        fresh.general.start_in_tray = true;
        assert_eq!(
            compare(&with(50), &mut fresh, &mut Previews::default(), now),
            Reload::Everything
        );
    }

    #[test]
    fn a_text_size_change_lays_out_again_without_a_rebuild() {
        let now = Instant::now();
        let with = |percent| {
            let mut config = Config::default();
            config.general.text_size = percent;
            config
        };
        // Edited by hand, alone or with the background: the widgets are laid out again in their windows.
        let mut fresh = with(130);
        assert_eq!(
            compare(&with(100), &mut fresh, &mut Previews::default(), now),
            Reload::Appearance
        );
        let mut fresh = with(80);
        fresh.general.background_opacity = 40;
        assert_eq!(
            compare(&with(100), &mut fresh, &mut Previews::default(), now),
            Reload::Appearance
        );
        // With anything else, everything is built again.
        let mut fresh = with(80);
        fresh.widget_mut("fuel").expect("fuel").enabled = false;
        assert_eq!(
            compare(&with(100), &mut fresh, &mut Previews::default(), now),
            Reload::Everything
        );
        // The slider's newer value holds over the file's previous save, and lays the widgets out.
        let mut previews = Previews {
            background: None,
            text_size: Some(Preview {
                percent: 140,
                at: now,
            }),
        };
        let mut fresh = with(120);
        assert_eq!(
            compare(&with(120), &mut fresh, &mut previews, now + RELOAD_DELAY),
            Reload::Appearance
        );
        assert_eq!(fresh.general.text_size, 140);
        assert_eq!(previews.text_size(&with(120)), 140);
        assert_eq!(previews.background(&with(120)), 82, "the file's");
        let later = now + PREVIEW_HOLD + RELOAD_DELAY;
        let mut fresh = with(120);
        assert_eq!(
            compare(&with(120), &mut fresh, &mut previews, later),
            Reload::Nothing
        );
        assert_eq!(previews.text_size(&with(120)), 120);
    }

    #[test]
    fn turning_update_checks_off_draws_nothing_again() {
        let now = Instant::now();
        let current = Config::default();
        let mut fresh = Config::default();
        fresh.general.check_updates = false;
        assert_eq!(
            compare(&current, &mut fresh, &mut Previews::default(), now),
            Reload::Unused
        );
        // With a change the widgets use, that change decides.
        let mut fresh = Config::default();
        fresh.general.check_updates = false;
        fresh.general.background_opacity = 40;
        assert_eq!(
            compare(&current, &mut fresh, &mut Previews::default(), now),
            Reload::Appearance
        );
        let mut fresh = Config::default();
        fresh.general.check_updates = false;
        fresh.widget_mut("fuel").expect("fuel").enabled = false;
        assert_eq!(
            compare(&current, &mut fresh, &mut Previews::default(), now),
            Reload::Everything
        );
    }

    /// A widget with a smallest size, for the box rules.
    struct Boxed {
        size: (u32, u32),
        min: (u32, u32),
        resized: usize,
    }

    impl Widget for Boxed {
        fn size(&self) -> (u32, u32) {
            self.size
        }

        fn min_size(&self) -> (u32, u32) {
            self.min
        }

        fn resize(&mut self, width: u32, height: u32) {
            self.size = (width.max(self.min.0), height.max(self.min.1));
            self.resized += 1;
        }

        fn interval(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn update(&mut self, _: &mut View, _: Instant, _: &mut [Redraw; MAX_LAYERS]) {}

        fn draw(&self, _: usize, _: &mut crate::render::Canvas<'_>) {}
    }

    fn boxed(size: (u32, u32)) -> Boxed {
        Boxed {
            size,
            min: (300, 167),
            resized: 0,
        }
    }

    #[test]
    fn a_text_size_keeps_the_box_and_lays_the_widget_out_inside() {
        // At the default size nothing changes: the widget keeps the size it was made at.
        let mut widget = boxed((460, 300));
        assert_eq!(fit(&mut widget, false, (460, 300), 1.0, false), (460, 300));
        assert_eq!(widget.resized, 0);
        // Larger text: the same box, fewer widget pixels; smaller text: more.
        assert_eq!(fit(&mut widget, false, (460, 300), 1.5, false), (460, 300));
        assert_eq!(widget.size, (307, 200));
        assert_eq!(fit(&mut widget, false, (460, 300), 0.7, false), (460, 300));
        assert_eq!(widget.size, (658, 429));
        // Back at the default, the widget fills its box again.
        assert_eq!(fit(&mut widget, false, (460, 300), 1.0, false), (460, 300));
        assert_eq!(widget.size, (460, 300));
        // A box too small for the widget at a large text size stays; the widget lays out at its smallest and what
        // does not fit is left out.
        let mut widget = boxed((300, 167));
        assert_eq!(fit(&mut widget, false, (300, 200), 1.5, false), (300, 200));
        assert_eq!(widget.size, (300, 167));
        // A drag to that size grows the box to the smallest that reads at the text size.
        assert_eq!(fit(&mut widget, false, (300, 200), 1.5, true), (450, 251));
        // A box made of text grows with it.
        let mut status = boxed((300, 54));
        assert_eq!(fit(&mut status, true, (300, 54), 1.5, false), (450, 81));
        assert_eq!(fit(&mut status, true, (450, 81), 0.7, false), (210, 38));
        assert_eq!(status.size, (300, 54), "its own size stays");
        assert_eq!(status.resized, 0);
    }

    #[test]
    fn boxes_in_widget_pixels_cover_the_screen_box() {
        // Rounded up, so a widget's background reaches the layer's last pixel.
        assert_eq!(widget_pixels(460, 1.0), 460);
        assert_eq!(widget_pixels(460, 1.5), 307);
        assert_eq!(widget_pixels(300, 1.5), 200);
        assert_eq!(widget_pixels(420, 0.7), 600);
        assert_eq!(widget_pixels(170, 1.15), 148);
        for (size, scale) in [(460u32, 1.5f32), (170, 1.15), (761, 0.75), (16, 1.45)] {
            let widget = widget_pixels(size, scale);
            assert!(
                widget as f32 * scale >= size as f32 - 1e-3,
                "{size} at {scale}"
            );
            assert!(
                (widget - 1) as f32 * scale < size as f32,
                "{size} at {scale}"
            );
        }
    }

    #[test]
    fn layers_fill_the_box_at_any_text_size() {
        let whole = LayerSpec {
            x: 0,
            y: 0,
            width: 307,
            height: 200,
        };
        // A layer over the whole widget covers the whole box, whatever the rounding.
        let layer = layer_in_box(whole, (307, 200), (460, 300), 1.5);
        assert_eq!(
            (layer.x, layer.y, layer.width, layer.height),
            (0, 0, 460, 300)
        );
        // At the default text size, widget pixels are screen pixels.
        let part = LayerSpec {
            x: 6,
            y: 6,
            width: 100,
            height: 40,
        };
        assert_eq!(layer_in_box(part, (200, 52), (200, 52), 1.0), part);
        // A layer inside the widget is placed and sized at the scale, and stays inside the box.
        let layer = layer_in_box(part, (200, 52), (300, 78), 1.5);
        assert_eq!(
            (layer.x, layer.y, layer.width, layer.height),
            (9, 9, 150, 60)
        );
        let edge = LayerSpec { x: 150, ..part };
        let layer = layer_in_box(edge, (200, 52), (300, 78), 1.5);
        assert_eq!((layer.x, layer.width), (225, 75), "to the box's edge");
    }
}
