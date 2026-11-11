//! The overlay's render thread: windows, widgets, and the schedule that drives them.
//!
//! The loop sleeps on a high-resolution waitable timer until the next widget is due, the configuration
//! directory changes, or a window message arrives. It updates the due widgets, redraws only what they report
//! as changed, and commits only when something was drawn. A rendering error, such as device loss, rebuilds the
//! renderer and redraws everything. A configuration change rebuilds windows and widgets; the data pipeline
//! keeps running.

use std::time::{Duration, Instant};

use pitot_irsdk::live::{ReaderConfig, SdkNames};
use tracing::{debug, info, warn};

use crate::config::{self, Config, WidgetConfig};
use crate::pipeline::{Pipeline, View};
use crate::platform::{
    self, DirectoryWatch, Monitor, OverlayWindow, Timer, Tray, TrayCommand, Wake,
};
use crate::render::{Canvas, Layer, RenderError, Renderer, WindowTarget};
use crate::widgets::{
    FuelWidget, InputTraceWidget, MAX_LAYERS, Redraw, RelativeWidget, StandingsWidget,
    StatusWidget, Widget,
};

/// Saving a file often takes several writes; reload once they settle.
const RELOAD_DELAY: Duration = Duration::from_millis(250);

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
    widget: Box<dyn Widget>,
    /// Index into `Layout::windows`.
    window: usize,
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
}

/// Builds the enabled widgets that exist so far. Widgets that are configured but not implemented yet are
/// skipped.
fn create_widget(name: &str, config: &WidgetConfig) -> Option<Box<dyn Widget>> {
    match name {
        "status" => Some(Box::new(StatusWidget::new(config.opacity))),
        "inputs" => Some(Box::new(InputTraceWidget::new(config))),
        "relative" => Some(Box::new(RelativeWidget::new(config))),
        "standings" => Some(Box::new(StandingsWidget::new(config))),
        "fuel" => Some(Box::new(FuelWidget::new(config))),
        _ => None,
    }
}

impl Layout {
    fn build(config: &Config, monitors: &[Monitor]) -> Result<Self, platform::PlatformError> {
        let rects: Vec<(i32, i32)> = monitors.iter().map(|m| (m.rect.x, m.rect.y)).collect();
        let order = config::monitor_order(&rects);
        let primary = monitors.iter().position(|m| m.primary).unwrap_or(0);
        let mut windows: Vec<OverlayWindow> = Vec::new();
        let mut window_monitors: Vec<usize> = Vec::new();
        let mut slots = Vec::new();
        let now = Instant::now();
        for (name, widget_config) in config.widgets() {
            if !widget_config.enabled {
                continue;
            }
            let Some(widget) = create_widget(name, widget_config) else {
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
                widget,
                window,
                x: widget_config.x,
                y: widget_config.y,
                interval,
                next_due: now,
                stale: true,
            });
        }
        Ok(Self { windows, slots })
    }
}

/// Composition objects for one layout, rebuilt after a configuration change. The renderer (the GPU device)
/// survives configuration changes and is recreated only after device loss.
struct Scene {
    _targets: Vec<WindowTarget>,
    /// Per slot, the widget's layers.
    layers: Vec<Vec<Layer>>,
}

impl Scene {
    fn build(renderer: &mut Renderer, layout: &Layout) -> Result<Self, RenderError> {
        let mut targets = Vec::with_capacity(layout.windows.len());
        for window in &layout.windows {
            targets.push(renderer.attach(window)?);
        }
        let mut layers = Vec::with_capacity(layout.slots.len());
        for slot in &layout.slots {
            let mut widget_layers = Vec::new();
            for spec in slot.widget.layers().into_iter().take(MAX_LAYERS) {
                widget_layers.push(renderer.layer(
                    &targets[slot.window],
                    slot.x + spec.x,
                    slot.y + spec.y,
                    spec.width,
                    spec.height,
                    spec.scrolling,
                )?);
            }
            layers.push(widget_layers);
        }
        renderer.commit_now()?;
        Ok(Self {
            _targets: targets,
            layers,
        })
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
    let mut layout = Layout::build(&config, &monitors)?;
    let tray = Tray::create()?;
    let mut renderer = Some(Renderer::new()?);
    let mut scene = match renderer.as_mut() {
        Some(device) => Some(Scene::build(device, &layout)?),
        None => None,
    };
    let (pipeline, mut view) = Pipeline::start(SdkNames::sim(), ReaderConfig::default())?;
    let timer = Timer::new()?;
    let mut reload_at: Option<Instant> = None;
    // Chosen from the tray menu: no widget is updated or drawn.
    let mut hidden = false;

    loop {
        if !platform::pump_messages() {
            break;
        }
        match platform::take_tray_command() {
            Some(TrayCommand::Reload) => reload_at = Some(Instant::now()),
            Some(TrayCommand::ToggleHidden) => {
                hidden = !hidden;
                tray.set_hidden(hidden);
                info!(hidden, "widgets hidden from the tray menu");
                layout.slots.iter_mut().for_each(|s| s.stale = true);
            }
            None => {}
        }
        if reload_at.is_some_and(|at| Instant::now() >= at) {
            reload_at = None;
            if let Some(path) = config::config_file() {
                match config::load_or_create(&path) {
                    Ok(fresh) if fresh == config => {}
                    Ok(fresh) => {
                        // Release GPU objects before their windows go away.
                        scene = None;
                        layout = Layout::build(&fresh, &platform::monitors())?;
                        config = fresh;
                        info!("configuration reloaded");
                    }
                    Err(e) => warn!("keeping the previous configuration: {e}"),
                }
            }
        }
        let changes = view.poll();
        if changes.connection {
            if view.is_connected() {
                info!("sim connected");
            } else {
                log_counters(&view, "sim disconnected");
            }
            for slot in &mut layout.slots {
                slot.widget.reset();
                slot.stale = true;
            }
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
        let now = Instant::now();
        if let (Some(device), Some(current)) = (renderer.as_mut(), scene.as_mut())
            && let Err(e) = frame(device, current, &mut layout.slots, &mut view, now, hidden)
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
                .filter(|s| !hidden && s.widget.visible(display))
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

/// Updates and draws the widgets that are due, then commits once. `hidden` hides every widget.
fn frame(
    renderer: &mut Renderer,
    scene: &mut Scene,
    slots: &mut [Slot],
    view: &mut View,
    now: Instant,
    hidden: bool,
) -> Result<(), RenderError> {
    let display = view.display();
    for (slot, layers) in slots.iter_mut().zip(scene.layers.iter_mut()) {
        let visible = !hidden && slot.widget.visible(display);
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
}
