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

use crate::config::{self, Config};
use crate::pipeline::{Display, Pipeline, RecordingConfig, Source, View};
use crate::platform::{
    self, ControlEvent, DirectoryWatch, Monitor, MoveHandle, OverlayWindow, ScreenRect, Timer,
    Tray, TrayCommand, Wake, WidgetMove,
};
use crate::render::{Canvas, Layer, RenderError, Renderer, WindowTarget};
use crate::widgets::{self, Build, MAX_LAYERS, Redraw, Shared, Widget};

/// Saving a file often takes several writes; reload once they settle.
const RELOAD_DELAY: Duration = Duration::from_millis(250);

/// How long a value from the Pitot window's background opacity slider holds over the file's in a reload: the
/// reload of the slider's previous save must not undo a newer value, but a window that went away mid-drag must not
/// hold it for ever.
const PREVIEW_HOLD: Duration = Duration::from_secs(2);

/// The value of the background opacity slider while it moves, and when it last moved.
#[derive(Debug, Clone, Copy)]
struct Preview {
    percent: u8,
    at: Instant,
}

/// What a reload changes, before a configuration that differs from the current one is used.
#[derive(Debug, PartialEq, Eq)]
enum Reload {
    Nothing,
    /// Only `[general] background_opacity`: the widgets redraw, nothing is rebuilt.
    Background,
    Everything,
}

/// Compares the reloaded configuration with the current one. A recent slider value replaces the file's background
/// opacity in `fresh`, and an old one is forgotten.
fn compare(
    current: &Config,
    fresh: &mut Config,
    preview: &mut Option<Preview>,
    now: Instant,
) -> Reload {
    match *preview {
        Some(p) if now.saturating_duration_since(p.at) < PREVIEW_HOLD => {
            fresh.general.background_opacity = p.percent;
        }
        _ => *preview = None,
    }
    if fresh == current {
        return Reload::Nothing;
    }
    let mut rest = fresh.clone();
    rest.general.background_opacity = current.general.background_opacity;
    if rest == *current {
        Reload::Background
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
}

impl Layout {
    /// Makes every enabled widget of the registry, with its variables resolved in `vars` (the pipeline's set).
    fn build(
        config: &Config,
        monitors: &[Monitor],
        vars: &VarSet,
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
            let cx = Build {
                config: widget_config,
                shared: &shared,
            };
            let Some(widget) = widgets::make_widget(name, &cx, vars) else {
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
        })
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
            for spec in slot.widget.layers().into_iter().take(MAX_LAYERS) {
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
            let (width, height) = slot.widget.size();
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
    let mut layout = Layout::build(&config, &monitors, view.var_set())?;
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
    let mut renderer = Some(Renderer::new()?);
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
    // While the Pitot window's slider moves, the background opacity it shows.
    let mut preview: Option<Preview> = None;

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
                        preview = None;
                    } else {
                        preview = Some(Preview {
                            percent,
                            at: Instant::now(),
                        });
                    }
                    // Applied below, before the frame. The window needs no answer.
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
                    let change = compare(&config, &mut fresh, &mut preview, Instant::now());
                    (fresh, change)
                });
                match reloaded {
                    Ok((_, Reload::Nothing)) => {}
                    Ok((fresh, Reload::Background)) => {
                        config = fresh;
                        info!("configuration reloaded: background opacity");
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
                        layout = Layout::build(&fresh, &platform::monitors(), view.var_set())?;
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
        if renderer.is_none() {
            renderer = Renderer::new()
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
        let background = preview.map_or(config.general.background_opacity, |p| p.percent);
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
            match redraw {
                Redraw::Nothing => {}
                Redraw::All => renderer.draw(layer, None, draw)?,
                Redraw::Area(area) => renderer.draw(layer, Some(area), draw)?,
                Redraw::Areas(a, b) => {
                    renderer.draw(layer, Some(a), draw)?;
                    renderer.draw(layer, Some(b), draw)?;
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
        // The window saved 50 and the overlay took it; the reload of that write changes nothing.
        let mut preview = None;
        let mut fresh = with(50);
        assert_eq!(
            compare(&with(50), &mut fresh, &mut preview, now),
            Reload::Nothing
        );
        // The slider moved on to 30 before that reload: the file's 50 does not win over it.
        let mut preview = Some(Preview {
            percent: 30,
            at: now,
        });
        let mut fresh = with(50);
        assert_eq!(
            compare(&with(50), &mut fresh, &mut preview, now + RELOAD_DELAY),
            Reload::Background
        );
        assert_eq!(fresh.general.background_opacity, 30);
        assert!(preview.is_some());
        // A window that went away mid-drag: after a while the file's value is used again.
        let mut fresh = with(50);
        let later = now + PREVIEW_HOLD + RELOAD_DELAY;
        assert_eq!(
            compare(&with(50), &mut fresh, &mut preview, later),
            Reload::Nothing
        );
        assert!(preview.is_none());
        // A value edited by hand redraws without a rebuild; any other change rebuilds.
        let mut fresh = with(10);
        assert_eq!(
            compare(&with(50), &mut fresh, &mut None, now),
            Reload::Background
        );
        let mut fresh = with(10);
        fresh.general.start_in_tray = true;
        assert_eq!(
            compare(&with(50), &mut fresh, &mut None, now),
            Reload::Everything
        );
    }
}
