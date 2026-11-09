//! The overlay's render thread: windows, widgets, and the schedule that drives them.
//!
//! The loop sleeps on a high-resolution waitable timer until the next widget is due (or a window message
//! arrives), updates the due widgets, redraws only what they report as changed, and commits only when
//! something was drawn. A rendering error, such as device loss, rebuilds the renderer and redraws everything.

use std::time::{Duration, Instant};

use pitot_irsdk::live::{ReaderConfig, SdkNames};

use crate::pipeline::{Pipeline, View};
use crate::platform::{self, Monitor, OverlayWindow, ScreenRect, Timer, Tray, TrayCommand};
use crate::render::{Layer, RenderError, Renderer, WindowTarget};
use crate::widgets::{Redraw, StatusWidget, Widget};

/// Where a widget goes: a monitor index and a position in that monitor's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub monitor: usize,
    pub x: i32,
    pub y: i32,
}

struct Slot {
    widget: Box<dyn Widget>,
    placement: Placement,
    next_due: Instant,
    /// The widget must be fully redrawn, after creation, a rebuild, or a reset.
    stale: bool,
}

/// GPU objects that are rebuilt after device loss.
struct Scene {
    renderer: Renderer,
    _targets: Vec<WindowTarget>,
    layers: Vec<Layer>,
}

impl Scene {
    fn build(windows: &[(usize, OverlayWindow)], slots: &[Slot]) -> Result<Self, RenderError> {
        let mut renderer = Renderer::new()?;
        let mut targets = Vec::with_capacity(windows.len());
        for (_, window) in windows {
            targets.push(renderer.attach(window)?);
        }
        let mut layers = Vec::with_capacity(slots.len());
        for slot in slots {
            let target = windows
                .iter()
                .position(|(monitor, _)| *monitor == slot.placement.monitor)
                .unwrap_or(0);
            let (w, h) = slot.widget.size();
            layers.push(renderer.layer(
                &targets[target],
                slot.placement.x,
                slot.placement.y,
                w,
                h,
            )?);
        }
        renderer.commit_now()?;
        Ok(Self {
            renderer,
            _targets: targets,
            layers,
        })
    }
}

/// Picks the monitor for a placement: the requested index, else the primary monitor.
fn monitor_index(monitors: &[Monitor], wanted: usize) -> usize {
    if wanted < monitors.len() {
        wanted
    } else {
        monitors.iter().position(|m| m.primary).unwrap_or(0)
    }
}

/// Default layout until configuration exists: the status panel near the primary monitor's top-left corner.
fn default_slots(monitors: &[Monitor]) -> Vec<Slot> {
    let primary = monitors.iter().position(|m| m.primary).unwrap_or(0);
    let now = Instant::now();
    vec![Slot {
        widget: Box::new(StatusWidget::new()),
        placement: Placement {
            monitor: primary,
            x: 24,
            y: 24,
        },
        next_due: now,
        stale: true,
    }]
}

/// Runs the overlay until Quit is chosen from the tray icon.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    platform::enable_dpi_awareness();
    let monitors = platform::monitors();
    if monitors.is_empty() {
        return Err("no monitors found".into());
    }
    let mut slots = default_slots(&monitors);
    for slot in &mut slots {
        slot.placement.monitor = monitor_index(&monitors, slot.placement.monitor);
    }
    // One window per monitor that has widgets.
    let mut windows: Vec<(usize, OverlayWindow)> = Vec::new();
    for slot in &slots {
        let monitor = slot.placement.monitor;
        if !windows.iter().any(|(m, _)| *m == monitor) {
            let rect: ScreenRect = monitors[monitor].rect;
            windows.push((monitor, OverlayWindow::create(rect)?));
        }
    }
    let _tray = Tray::create()?;
    let mut scene = Some(Scene::build(&windows, &slots)?);
    let (pipeline, mut view) = Pipeline::start(SdkNames::sim(), ReaderConfig::default())?;
    let timer = Timer::new()?;

    loop {
        if !platform::pump_messages() {
            break;
        }
        if platform::take_tray_command() == Some(TrayCommand::Reload) {
            // Configuration arrives with the config module; until then a reload redraws everything.
            slots.iter_mut().for_each(|s| s.stale = true);
        }
        let changes = view.poll();
        if changes.connection {
            for slot in &mut slots {
                slot.widget.reset();
                slot.stale = true;
            }
        }
        let now = Instant::now();
        if scene.is_none() {
            scene = Scene::build(&windows, &slots).ok();
            slots.iter_mut().for_each(|s| s.stale = true);
        }
        if let Some(current) = scene.as_mut()
            && let Err(_lost) = frame(current, &mut slots, &mut view, now)
        {
            // Rebuild on the next pass, after a short pause so a missing device does not spin.
            scene = None;
            timer.set(Duration::from_millis(500))?;
            platform::wait(&timer, Duration::from_secs(1));
            continue;
        }
        let next = slots
            .iter()
            .map(|s| s.next_due)
            .min()
            .unwrap_or(now + Duration::from_secs(1));
        timer.set(next.saturating_duration_since(Instant::now()))?;
        platform::wait(&timer, Duration::from_secs(1));
    }
    drop(pipeline);
    Ok(())
}

/// Updates and draws the widgets that are due, then commits once.
fn frame(
    scene: &mut Scene,
    slots: &mut [Slot],
    view: &mut View,
    now: Instant,
) -> Result<(), RenderError> {
    let display = view.display();
    for (slot, layer) in slots.iter_mut().zip(scene.layers.iter_mut()) {
        let visible = slot.widget.visible(display);
        scene.renderer.set_visible(layer, visible)?;
        if !visible || (now < slot.next_due && !slot.stale) {
            continue;
        }
        slot.next_due = now + slot.widget.interval();
        let redraw = match slot.widget.update(view, now) {
            _ if slot.stale => Redraw::All,
            redraw => redraw,
        };
        slot.stale = false;
        let widget = &slot.widget;
        match redraw {
            Redraw::Nothing => {}
            Redraw::All => scene
                .renderer
                .draw(layer, None, |canvas| widget.draw(canvas))?,
            Redraw::Area(area) => scene
                .renderer
                .draw(layer, Some(area), |canvas| widget.draw(canvas))?,
        }
    }
    scene.renderer.commit()?;
    Ok(())
}
