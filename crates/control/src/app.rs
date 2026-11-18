//! Process start: one Pitot window per Windows session, its link to the overlay, and where it opens.

use futures::StreamExt as _;
use gpui::{
    App, AppContext as _, Bounds, Pixels, TitlebarOptions, Window, WindowBounds, WindowKind,
    WindowOptions, point, px, size,
};
use pitot_overlay::config;
use pitot_overlay::platform::{self, ControlLink, SingleInstance};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::model::Placement;
use crate::panel::{Panel, Updates};
use crate::style;
use crate::updates::Updater;

/// Opens the Pitot window and runs until it closes. A second start brings the open window to the front instead.
pub fn run() {
    let instance = SingleInstance::acquire_control();
    if matches!(instance, Ok(None)) {
        platform::activate_control();
        return;
    }
    let (sender, mut events) = futures::channel::mpsc::unbounded();
    // Without the link the window still edits the configuration and sends commands, but hears nothing back.
    let link = ControlLink::start(move |event| {
        let _ = sender.unbounded_send(event);
    })
    .ok();
    // News from the update threads, which `Updates` starts.
    let (update_sender, mut update_events) = futures::channel::mpsc::unbounded();
    let updates = Updates {
        updater: Updater::installed(),
        events: update_sender,
    };

    gpui_platform::application().run(move |cx: &mut App| {
        let (bounds, display_id) = initial_bounds(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Pitot".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            focus: true,
            show: true,
            kind: WindowKind::Normal,
            is_resizable: false,
            is_minimizable: true,
            display_id,
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            if let Some(hwnd) = hwnd(window) {
                ControlLink::set_window(hwnd);
            }
            let panel = cx.new(|cx| Panel::new(window, cx, updates));
            let closing = panel.clone();
            window.on_window_should_close(cx, move |window, cx| {
                // Closing while the overlay quits for an update would leave neither running.
                if closing.read(cx).installing() {
                    return false;
                }
                save_placement(window);
                true
            });
            panel
        });
        let Ok(handle) = opened else {
            cx.quit();
            return;
        };
        // Activation before the panel watched it: above the widgets if the window opened active.
        let active = handle
            .update(cx, |_, window, _| window.is_window_active())
            .unwrap_or(false);
        ControlLink::keep_above_widgets(active);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.spawn(async move |cx| {
            while let Some(event) = events.next().await {
                let close = handle
                    .update(cx, |panel, window, cx| {
                        let close = panel.on_event(event, cx);
                        if close {
                            save_placement(window);
                        }
                        close
                    })
                    .unwrap_or(true);
                if close {
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            while let Some(event) = update_events.next().await {
                let close = handle
                    .update(cx, |panel, window, cx| {
                        let close = panel.on_update(event, cx);
                        if close {
                            save_placement(window);
                        }
                        close
                    })
                    .unwrap_or(true);
                // The installer waits for this process to exit.
                if close {
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
            }
        })
        .detach();
    });
    drop(link);
    drop(instance);
}

fn hwnd(window: &Window) -> Option<isize> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// Where the window opens: where it was last closed if that is still on a display, else centered on the primary
/// display.
fn initial_bounds(cx: &App) -> (Bounds<Pixels>, Option<gpui::DisplayId>) {
    let window_size = size(px(style::WINDOW_WIDTH), px(style::WINDOW_HEIGHT));
    let saved = config::window_file()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| Placement::parse(&text));
    if let Some(placement) = saved {
        let bounds = Bounds::new(point(px(placement.x), px(placement.y)), window_size);
        // The title bar must be on a display, so the window can be dragged back.
        let grip = point(bounds.center().x, bounds.origin.y + px(16.0));
        if let Some(display) = cx
            .displays()
            .into_iter()
            .find(|d| d.bounds().contains(&grip))
        {
            return (bounds, Some(display.id()));
        }
    }
    (Bounds::centered(None, window_size, cx), None)
}

/// Keeps where the window is, for the next time it opens.
pub fn save_placement(window: &Window) {
    // The restore bounds, in the form a new window takes them; the client area's bounds sit lower by the title bar.
    let origin = window.window_bounds().get_bounds().origin;
    let placement = Placement {
        x: f32::from(origin.x),
        y: f32::from(origin.y),
    };
    if let Some(path) = config::window_file() {
        let _ = std::fs::write(path, placement.to_text());
    }
}
