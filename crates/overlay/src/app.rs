//! The overlay's render thread: windows, widgets, and the schedule that drives them.
//!
//! The loop sleeps on a high-resolution waitable timer until the next widget is due, the configuration
//! directory changes, or a window message arrives. It updates the due widgets, redraws only what they report
//! as changed, and commits only when something was drawn. A rendering error, such as device loss, rebuilds the
//! renderer and redraws everything. A configuration change rebuilds windows and widgets; the data pipeline
//! keeps running.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use pitot_irsdk::live::{ReaderConfig, SdkNames, VarSet};
use tracing::{debug, info, warn};

use crate::config::{self, Config, WidgetConfig};
use crate::pipeline::{Display, Pipeline, RecordingConfig, Source, View};
use crate::platform::{
    self, ControlEvent, DirectoryWatch, Monitor, MoveHandle, OverlayWindow, ScreenRect, Timer,
    Tray, TrayCommand, Wake, WidgetMove,
};
use crate::render::{Canvas, Layer, RenderError, Renderer, WindowTarget};
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
    /// Making the text formats and the glyph atlas.
    text: Duration,
}

/// What a reload changes, before a configuration that differs from the current one is used.
#[derive(Debug, PartialEq, Eq)]
enum Reload {
    Nothing,
    /// Only the look the Pitot window's sliders set, `[general] background_opacity` and `text_size`: the widgets
    /// are laid out at the text size and redraw; windows and layers stay.
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
    rest.general.background_opacity = current.general.background_opacity;
    rest.general.text_size = current.general.text_size;
    if rest == *current {
        Reload::Appearance
    } else {
        Reload::Everything
    }
}

/// How often the render loop looks at the pipeline while no widget is shown: once per second with the sim not
/// running, and often enough while it is connected that widgets appear promptly when the driver gets in the car.
const IDLE_WAKE: Duration = Duration::from_secs(1);
const HIDDEN_WAKE: Duration = Duration::from_millis(250);

/// When the render loop wakes next: the earliest due time of the shown widgets, or an idle wake when none is
/// shown. Hidden widgets keep their past due time, so they draw at once when they appear, but they must not set
/// the timer, or the loop would wake at every clock tick.
fn next_wake(shown_due: impl Iterator<Item = Instant>, connected: bool, now: Instant) -> Instant {
    shown_due
        .min()
        .unwrap_or_else(|| now + if connected { HIDDEN_WAKE } else { IDLE_WAKE })
}

struct Slot {
    /// The widget's name in the configuration.
    name: &'static str,
    widget: Box<dyn Widget>,
    /// The widget's box on screen, in pixels: its size at the default text size, which other text sizes keep.
    size: (u32, u32),
    /// Whether the widget is laid out and drawn at the layout's text scale (see [`Widget::scales_with_text`]).
    scaled: bool,
    /// Index into `Layout::windows`.
    window: usize,
    /// Screen position of the window's (the monitor's) top-left corner.
    origin: (i32, i32),
    /// Position in the window, as configured.
    x: i32,
    y: i32,
    interval: Duration,
    next_due: Instant,
    /// The widget must be fully redrawn, after creation, a rebuild, or a reset.
    stale: bool,
}

/// Windows and widgets built from one configuration.
struct Layout {
    windows: Vec<OverlayWindow>,
    slots: Vec<Slot>,
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

/// A widget made for a text scale.
struct Made {
    widget: Box<dyn Widget>,
    /// Its box on screen, in pixels.
    size: (u32, u32),
    /// Whether it is laid out and drawn at the text scale.
    scaled: bool,
}

/// Makes widget `name` laid out at text scale `scale`.
///
/// A widget is first made as configured, which gives its box: its size at the default text size. A widget that
/// scales with the text is then made again with that box in widget pixels. A widget whose table gives no size (the
/// status line) has a box made of its text, which grows with the text.
fn make_scaled(
    name: &str,
    config: &WidgetConfig,
    shared: &Shared,
    vars: &VarSet,
    scale: f32,
) -> Option<Made> {
    let widget = widgets::make_widget(name, &Build { config, shared }, vars)?;
    let size = widget.size();
    let scaled = widget.scales_with_text();
    if !scaled || scale == 1.0 {
        return Some(Made {
            widget,
            size,
            scaled,
        });
    }
    let mut in_widget_pixels = config.clone();
    in_widget_pixels.width = config.width.map(|_| widget_pixels(size.0, scale));
    in_widget_pixels.height = config.height.map(|_| widget_pixels(size.1, scale));
    let widget = widgets::make_widget(
        name,
        &Build {
            config: &in_widget_pixels,
            shared,
        },
        vars,
    )?;
    let (w, h) = widget.size();
    let on_screen = |configured: Option<u32>, at_default: u32, own: u32| {
        if configured.is_some() {
            at_default
        } else {
            (own as f32 * scale).round() as u32
        }
    };
    let size = (
        on_screen(config.width, size.0, w),
        on_screen(config.height, size.1, h),
    );
    Some(Made {
        widget,
        size,
        scaled,
    })
}

impl Layout {
    /// Makes every enabled widget of the registry, with its variables resolved in `vars` (the pipeline's set), laid
    /// out at text scale `scale`.
    fn build(
        config: &Config,
        monitors: &[Monitor],
        vars: &VarSet,
        scale: f32,
    ) -> Result<Self, platform::PlatformError> {
        let rects: Vec<(i32, i32)> = monitors.iter().map(|m| (m.rect.x, m.rect.y)).collect();
        let order = config::monitor_order(&rects);
        let primary = monitors.iter().position(|m| m.primary).unwrap_or(0);
        let mut windows: Vec<OverlayWindow> = Vec::new();
        let mut window_monitors: Vec<usize> = Vec::new();
        let mut slots = Vec::new();
        let now = Instant::now();
        let shared = Shared::new();
        for (name, widget_config) in config.widgets() {
            if !widget_config.enabled {
                continue;
            }
            let Some(Made {
                widget,
                size,
                scaled,
            }) = make_scaled(name, widget_config, &shared, vars, scale)
            else {
                continue;
            };
            let monitor = widget_config
                .monitor
                .and_then(|i| order.get(i).copied())
                .unwrap_or(primary);
            let window = match window_monitors.iter().position(|&m| m == monitor) {
                Some(index) => index,
                None => {
                    windows.push(OverlayWindow::create(monitors[monitor].rect)?);
                    window_monitors.push(monitor);
                    windows.len() - 1
                }
            };
            let interval = widget_config
                .rate_hz
                .map_or(widget.interval(), |hz| Duration::from_secs_f32(1.0 / hz));
            slots.push(Slot {
                name,
                widget,
                size,
                scaled,
                window,
                origin: (monitors[monitor].rect.x, monitors[monitor].rect.y),
                x: widget_config.x,
                y: widget_config.y,
                interval,
                next_due: now,
                stale: true,
            });
        }
        Ok(Self {
            windows,
            slots,
            shared,
            scale,
        })
    }

    /// Makes the widgets again laid out at text scale `scale`, in the same windows, with the same shared trackers.
    /// What each widget followed of the session on its own starts over. Allocates.
    fn rescale(&mut self, config: &Config, vars: &VarSet, scale: f32) {
        for slot in &mut self.slots {
            let Some(widget_config) = config.widget(slot.name) else {
                continue;
            };
            if let Some(made) = make_scaled(slot.name, widget_config, &self.shared, vars, scale) {
                slot.widget = made.widget;
                slot.size = made.size;
                slot.scaled = made.scaled;
            }
            slot.stale = true;
        }
        self.scale = scale;
    }

    /// Each slot's box on screen, to tell whether a scene's layers still fit.
    fn sizes(&self) -> Vec<(u32, u32)> {
        self.slots.iter().map(|s| s.size).collect()
    }
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
        scrolling: spec.scrolling,
    }
}

/// Composition objects for one layout, rebuilt after a configuration change. The renderer (the GPU device)
/// survives configuration changes and is recreated only after device loss.
struct Scene {
    _targets: Vec<WindowTarget>,
    /// Per slot, the widget's layers.
    layers: Vec<Vec<Layer>>,
    /// Per slot, each layer's position in the widget.
    offsets: Vec<Vec<(i32, i32)>>,
    /// Per slot, the box the layers were made for.
    sizes: Vec<(u32, u32)>,
}

impl Scene {
    fn build(renderer: &mut Renderer, layout: &Layout) -> Result<Self, RenderError> {
        let mut targets = Vec::with_capacity(layout.windows.len());
        for window in &layout.windows {
            targets.push(renderer.attach(window)?);
        }
        let mut layers = Vec::with_capacity(layout.slots.len());
        let mut offsets = Vec::with_capacity(layout.slots.len());
        for slot in &layout.slots {
            let mut widget_layers = Vec::new();
            let mut widget_offsets = Vec::new();
            let scale = if slot.scaled { layout.scale } else { 1.0 };
            for spec in slot.widget.layers().into_iter().take(MAX_LAYERS) {
                let spec = layer_in_box(spec, slot.widget.size(), slot.size, scale);
                widget_layers.push(renderer.layer(
                    &targets[slot.window],
                    slot.x + spec.x,
                    slot.y + spec.y,
                    spec.width,
                    spec.height,
                    spec.scrolling,
                )?);
                widget_offsets.push((spec.x, spec.y));
            }
            layers.push(widget_layers);
            offsets.push(widget_offsets);
        }
        renderer.commit_now()?;
        Ok(Self {
            _targets: targets,
            layers,
            offsets,
            sizes: layout.sizes(),
        })
    }

    /// Moves slot `id`'s layers to the slot's position, without redrawing them.
    fn place(
        &mut self,
        renderer: &mut Renderer,
        id: usize,
        slot: &Slot,
    ) -> Result<(), RenderError> {
        let (Some(layers), Some(offsets)) = (self.layers.get_mut(id), self.offsets.get(id)) else {
            return Ok(());
        };
        for (layer, (dx, dy)) in layers.iter_mut().zip(offsets) {
            renderer.move_layer(layer, slot.x + dx, slot.y + dy)?;
        }
        Ok(())
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

/// A move handle over each widget, for dragging them while *Move widgets* is chosen.
fn move_handles(slots: &[Slot]) -> Vec<MoveHandle> {
    slots
        .iter()
        .enumerate()
        .filter_map(|(id, slot)| {
            let (width, height) = slot.size;
            let rect = ScreenRect {
                x: slot.origin.0 + slot.x,
                y: slot.origin.1 + slot.y,
                width: i32::try_from(width).unwrap_or(i32::MAX),
                height: i32::try_from(height).unwrap_or(i32::MAX),
            };
            MoveHandle::create(id, rect)
                .inspect_err(|e| warn!("widgets.{} cannot be moved: {e}", slot.name))
                .ok()
        })
        .collect()
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

/// The overlay process, one per Windows session: logging, then [`run`]. A second start opens the Pitot window
/// instead and exits at once.
pub fn main() -> ExitCode {
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
    let code = match run() {
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

/// Runs the overlay until Quit is chosen from the tray icon.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    platform::enable_dpi_awareness();
    let mut config = load_config();
    let watch = config::config_dir().and_then(|dir| DirectoryWatch::new(&dir).ok());
    let config_stamp = || config::config_file().and_then(|path| config::stamp(&path));
    let mut stamp = config_stamp();
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
    let (pipeline, mut view) = Pipeline::start(
        SdkNames::sim(),
        ReaderConfig::default(),
        config::capture_dirs(),
        recording,
    )?;
    let documents = platform::documents_dir();
    view.set_telemetry_folders(config::telemetry_dirs(&config.telemetry, documents.clone()));
    let scale = config::text_scale(config.general.text_size);
    let mut layout = Layout::build(&config, &monitors, view.var_set(), scale)?;
    let tray = Tray::create()?;
    // A Pitot window open from before learns that the overlay runs now.
    platform::notify_control(ControlEvent::State {
        hidden: false,
        moving: false,
    });
    if !config.general.start_in_tray
        && let Err(e) = platform::open_control()
    {
        warn!("cannot open the Pitot window: {e}");
    }
    let mut renderer = Some(Renderer::new(layout.scale)?);
    let mut scene = match renderer.as_mut() {
        Some(device) => Some(Scene::build(device, &layout)?),
        None => None,
    };
    let timer = Timer::new()?;
    let mut reload_at: Option<Instant> = None;
    // Chosen from the tray menu: no widget is updated or drawn.
    let mut hidden = false;
    // While *Move widgets* is chosen: a handle over each widget.
    let mut handles: Option<Vec<MoveHandle>> = None;
    let mut moves: Vec<WidgetMove> = Vec::new();
    // While one of the Pitot window's sliders moves, the value it shows.
    let mut previews = Previews::default();
    // A text size change waiting for the frame that draws it.
    let mut rescaled: Option<Rescaled> = None;

    loop {
        if !platform::pump_messages() {
            break;
        }
        while let Some(command) = platform::take_tray_command() {
            match command {
                TrayCommand::Reload => reload_at = Some(Instant::now()),
                TrayCommand::SetHidden(on) if on != hidden => {
                    hidden = on;
                    tray.set_hidden(hidden);
                    info!(hidden, "widgets hidden or shown");
                    layout.slots.iter_mut().for_each(|s| s.stale = true);
                }
                TrayCommand::SetMoving(on) if on != handles.is_some() => {
                    handles = on.then(|| move_handles(&layout.slots));
                    tray.set_moving(on);
                    if on && hidden {
                        hidden = false;
                        tray.set_hidden(false);
                    }
                    info!(moving = on, "moving widgets started or ended");
                    layout.slots.iter_mut().for_each(|s| s.stale = true);
                }
                TrayCommand::SetHidden(_) | TrayCommand::SetMoving(_) => {}
                TrayCommand::BackgroundOpacity { percent, saved } => {
                    if saved {
                        // The reload of the window's write then finds the file as the overlay has it.
                        config.general.background_opacity = percent;
                        previews.background = None;
                    } else {
                        previews.background = Preview::now(percent);
                    }
                    // Applied below, before the frame. The window needs no answer.
                    continue;
                }
                TrayCommand::TextSize { percent, saved } => {
                    if saved {
                        config.general.text_size = percent;
                        previews.text_size = None;
                    } else {
                        previews.text_size = Preview::now(percent);
                    }
                    continue;
                }
            }
            // Also when nothing changed, so the window's switches settle on the overlay's state.
            platform::notify_control(ControlEvent::State {
                hidden,
                moving: handles.is_some(),
            });
        }
        platform::take_moves(&mut moves);
        for moved in moves.drain(..) {
            let Some(slot) = layout.slots.get_mut(moved.id) else {
                continue;
            };
            slot.x = moved.x - slot.origin.0;
            slot.y = moved.y - slot.origin.1;
            if let (Some(device), Some(current)) = (renderer.as_mut(), scene.as_mut())
                && let Err(e) = current.place(device, moved.id, slot)
            {
                debug!("widget not moved on screen, rebuilding the scene: {e}");
                scene = None;
            }
            if moved.done {
                if let Some(widget) = config.widget_mut(slot.name) {
                    (widget.x, widget.y) = (slot.x, slot.y);
                }
                let saved = config::config_file()
                    .ok_or_else(|| "no APPDATA directory".to_owned())
                    .and_then(|path| {
                        config::save_position(&path, slot.name, slot.x, slot.y)
                            .map_err(|e| e.to_string())
                    });
                match saved {
                    // The watch sees the write; the new stamp keeps it from reloading.
                    Ok(()) => {
                        stamp = config_stamp();
                        info!(widget = slot.name, x = slot.x, y = slot.y, "widget moved");
                    }
                    Err(e) => warn!(widget = slot.name, "widget moved but not saved: {e}"),
                }
            }
        }
        if reload_at.is_some_and(|at| Instant::now() >= at) {
            reload_at = None;
            if let Some(path) = config::config_file() {
                let reloaded = config::load_or_create(&path).map(|mut fresh| {
                    let change = compare(&config, &mut fresh, &mut previews, Instant::now());
                    (fresh, change)
                });
                match reloaded {
                    Ok((_, Reload::Nothing)) => {}
                    Ok((fresh, Reload::Appearance)) => {
                        // Applied below, before the frame: the background redraws, the text lays the widgets out.
                        config = fresh;
                        info!("configuration reloaded: background opacity or text size");
                        platform::notify_control(ControlEvent::ConfigChanged);
                    }
                    Ok((fresh, Reload::Everything)) => {
                        if fresh.telemetry != config.telemetry {
                            view.set_telemetry_folders(config::telemetry_dirs(
                                &fresh.telemetry,
                                documents.clone(),
                            ));
                        }
                        // Release GPU objects before their windows go away.
                        scene = None;
                        let moving = handles.take().is_some();
                        let scale = config::text_scale(previews.text_size(&fresh));
                        layout =
                            Layout::build(&fresh, &platform::monitors(), view.var_set(), scale)?;
                        config = fresh;
                        // New windows go above the old handles, so the handles are made again.
                        if moving {
                            handles = Some(move_handles(&layout.slots));
                        }
                        info!("configuration reloaded");
                        platform::notify_control(ControlEvent::ConfigChanged);
                    }
                    Err(e) => warn!("keeping the previous configuration: {e}"),
                }
            }
        }
        let changes = view.poll();
        if changes.connection {
            match view.source() {
                Some(Source::Sim) => info!("sim connected"),
                Some(Source::Capture { first: true }) => {
                    info!("replay: showing the session capture")
                }
                Some(Source::SimAgain) => info!("replay: showing live values again"),
                Some(Source::Capture { first: false }) => {}
                None => log_counters(&view, "sim disconnected"),
            }
            for slot in &mut layout.slots {
                slot.widget.reset();
                slot.stale = true;
            }
            layout.shared.reset();
        }
        // A changed text size lays the widgets out again in their boxes and makes the text formats and the atlas
        // again. The windows and layers stay unless a widget's box changes with its text (the status line).
        let text_size = previews.text_size(&config);
        let scale = config::text_scale(text_size);
        if layout.scale != scale {
            let started = Instant::now();
            layout.rescale(&config, view.var_set(), scale);
            if scene.as_ref().is_some_and(|s| s.sizes != layout.sizes()) {
                scene = None;
                if let Some(moving) = handles.as_mut() {
                    *moving = move_handles(&layout.slots);
                }
            }
            let laid_out = started.elapsed();
            if let Some(device) = renderer.as_mut()
                && let Err(e) = device.set_text_scale(scale)
            {
                warn!("text not made at the new size, rebuilding the device: {e}");
                scene = None;
                renderer = None;
            }
            rescaled = Some(Rescaled {
                text_size,
                started,
                laid_out,
                text: started.elapsed() - laid_out,
            });
        }
        if renderer.is_none() {
            renderer = Renderer::new(layout.scale)
                .inspect(|_| info!("render device rebuilt"))
                .inspect_err(|e| debug!("render device not created: {e}"))
                .ok();
        }
        if scene.is_none()
            && let Some(device) = renderer.as_mut()
        {
            scene = Scene::build(device, &layout)
                .inspect_err(|e| debug!("scene not built: {e}"))
                .ok();
            layout.slots.iter_mut().for_each(|s| s.stale = true);
        }
        // A changed background opacity redraws every widget once, as hidden widgets do when they show again.
        let background = previews.background(&config);
        if let Some(device) = renderer.as_mut()
            && device.set_background_opacity(background)
        {
            layout.slots.iter_mut().for_each(|s| s.stale = true);
        }
        let shown = if handles.is_some() {
            Shown::All
        } else if hidden {
            Shown::Hidden
        } else {
            Shown::Normal
        };
        let now = Instant::now();
        if let (Some(device), Some(current)) = (renderer.as_mut(), scene.as_mut())
            && let Err(e) = frame(device, current, &mut layout.slots, &mut view, now, shown)
        {
            // Device loss: rebuild the device and the scene on the next pass, after a short pause so a missing
            // device does not spin.
            warn!("render failed, rebuilding the device: {e}");
            scene = None;
            renderer = None;
            timer.set(Duration::from_millis(500))?;
            platform::wait(&timer, None, Duration::from_secs(1));
            continue;
        }
        // Once per text size the slider or the file sets: what laying out, making the text, and drawing took.
        if let Some(done) = rescaled.take() {
            let ms = |d: Duration| (d.as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
            info!(
                text_size = done.text_size,
                layout_ms = ms(done.laid_out),
                text_ms = ms(done.text),
                total_ms = ms(done.started.elapsed()),
                "text size applied"
            );
        }
        let display = view.display();
        let mut next = next_wake(
            layout
                .slots
                .iter()
                .filter(|s| shown.visible(s.widget.as_ref(), display))
                .map(|s| s.next_due),
            view.is_connected(),
            now,
        );
        if let Some(at) = reload_at {
            next = next.min(at);
        }
        timer.set(next.saturating_duration_since(Instant::now()))?;
        // The directory also holds the log, whose writes signal the watch too; only a changed file reloads.
        if platform::wait(&timer, watch.as_ref(), Duration::from_secs(1)) == Wake::Changed {
            let current = config_stamp();
            if current != stamp {
                stamp = current;
                reload_at = Some(Instant::now() + RELOAD_DELAY);
            }
        }
    }
    log_counters(&view, "data counters at exit");
    // Quitting Pitot closes its window too.
    platform::notify_control(ControlEvent::Close);
    drop(pipeline);
    Ok(())
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

/// Updates and draws the widgets that are due, then commits once.
fn frame(
    renderer: &mut Renderer,
    scene: &mut Scene,
    slots: &mut [Slot],
    view: &mut View,
    now: Instant,
    shown: Shown,
) -> Result<(), RenderError> {
    let display = view.display();
    for (slot, layers) in slots.iter_mut().zip(scene.layers.iter_mut()) {
        let visible = shown.visible(slot.widget.as_ref(), display);
        for layer in layers.iter_mut() {
            renderer.set_visible(layer, visible)?;
        }
        if !visible || (now < slot.next_due && !slot.stale) {
            continue;
        }
        slot.next_due = now + slot.interval;
        let mut redraws = [Redraw::Nothing; MAX_LAYERS];
        slot.widget.update(view, now, &mut redraws);
        if slot.stale {
            redraws = [Redraw::All; MAX_LAYERS];
            slot.stale = false;
        }
        let widget = &slot.widget;
        for (index, (layer, redraw)) in layers.iter_mut().zip(redraws).enumerate() {
            let draw = |canvas: &mut Canvas<'_>| widget.draw(index, canvas);
            let scaled = slot.scaled;
            match redraw {
                Redraw::Nothing => {}
                Redraw::All => renderer.draw(layer, None, scaled, draw)?,
                Redraw::Area(area) => renderer.draw(layer, Some(area), scaled, draw)?,
                Redraw::Areas(a, b) => {
                    renderer.draw(layer, Some(a), scaled, draw)?;
                    renderer.draw(layer, Some(b), scaled, draw)?;
                }
            }
            renderer.scroll(layer, widget.scroll(index))?;
        }
    }
    renderer.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_widgets_do_not_set_the_timer() {
        let now = Instant::now();
        let past = now - Duration::from_secs(5);
        let soon = now + Duration::from_millis(16);
        assert_eq!(
            next_wake([soon, now + IDLE_WAKE].into_iter(), true, now),
            soon
        );
        assert_eq!(
            next_wake([past].into_iter(), true, now),
            past,
            "a due widget draws at once"
        );
        assert_eq!(next_wake(std::iter::empty(), false, now), now + IDLE_WAKE);
        assert_eq!(next_wake(std::iter::empty(), true, now), now + HIDDEN_WAKE);
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
            scrolling: false,
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
            scrolling: true,
        };
        assert_eq!(layer_in_box(part, (200, 52), (200, 52), 1.0), part);
        // A layer inside the widget is placed and sized at the scale, and stays inside the box.
        let layer = layer_in_box(part, (200, 52), (300, 78), 1.5);
        assert_eq!(
            (layer.x, layer.y, layer.width, layer.height),
            (9, 9, 150, 60)
        );
        assert!(layer.scrolling);
        let edge = LayerSpec { x: 150, ..part };
        let layer = layer_in_box(edge, (200, 52), (300, 78), 1.5);
        assert_eq!((layer.x, layer.width), (225, 75), "to the box's edge");
    }
}
