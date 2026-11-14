//! What the window shows and what clicks do: the overlay's switches, one switch per widget, and Quit.

use gpui::{
    App, ClickEvent, Context, Div, ElementId, IntoElement, ParentElement, Render, SharedString,
    Stateful, Styled, Subscription, Window, div, prelude::*, px,
};
use pitot_overlay::config;
use pitot_overlay::platform::{self, ControlEvent, ControlLink, OVERLAY_EXE, OverlayCommand};

use crate::model::{self, Overlay};
use crate::style;

pub struct Panel {
    overlay: Overlay,
    /// Per widget of [`model::widgets`], whether it is enabled in `config.toml`.
    enabled: Vec<bool>,
    /// The last problem, such as a configuration file that does not parse.
    note: Option<SharedString>,
    _activation: Subscription,
}

impl Panel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation = cx.observe_window_activation(window, |panel, window, cx| {
            let active = window.is_window_active();
            // The widgets are always on top; the window goes above them while it is used.
            ControlLink::keep_above_widgets(active);
            // The file may have been edited elsewhere while the window was in the background.
            if active {
                panel.load_config();
                cx.notify();
            }
        });
        let overlay = if platform::send_to_overlay(OverlayCommand::Hello) {
            // Settles when the answer arrives.
            Overlay::Running {
                hidden: false,
                moving: false,
            }
        } else {
            Overlay::NotRunning
        };
        let mut panel = Self {
            overlay,
            enabled: model::enabled(&config::Config::default()),
            note: None,
            _activation: activation,
        };
        panel.load_config();
        panel
    }

    fn load_config(&mut self) {
        let Some(path) = config::config_file() else {
            self.note = Some("No APPDATA folder: the configuration cannot be found.".into());
            return;
        };
        match config::load_or_create(&path) {
            Ok(config) => {
                self.enabled = model::enabled(&config);
                self.note = None;
            }
            Err(e) => self.note = Some(e.to_string().into()),
        }
    }

    /// Takes news from the overlay. Returns `true` when the window should close.
    pub fn on_event(&mut self, event: ControlEvent, cx: &mut Context<Self>) -> bool {
        match event {
            ControlEvent::State { hidden, moving } => {
                self.overlay = Overlay::Running { hidden, moving };
            }
            ControlEvent::ConfigChanged => self.load_config(),
            ControlEvent::Close => return true,
            ControlEvent::Activate => {}
        }
        cx.notify();
        false
    }

    /// Sends `command`; an overlay that does not answer is not running any more.
    fn send(&mut self, command: OverlayCommand) {
        if !platform::send_to_overlay(command) {
            self.overlay = Overlay::NotRunning;
        }
    }

    fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if let Overlay::Running { hidden, .. } = &mut self.overlay {
            // Shown at once; the overlay's answer settles it.
            *hidden = !shown;
        }
        self.send(OverlayCommand::SetHidden(!shown));
        cx.notify();
    }

    fn set_moving(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Overlay::Running { moving, hidden } = &mut self.overlay {
            *moving = on;
            // Moving shows the widgets.
            *hidden &= !on;
        }
        self.send(OverlayCommand::SetMoving(on));
        cx.notify();
    }

    fn start_overlay(&mut self, cx: &mut Context<Self>) {
        match platform::start_sibling(OVERLAY_EXE) {
            Ok(()) => self.overlay = Overlay::Starting,
            Err(e) => self.note = Some(format!("Cannot start {OVERLAY_EXE}: {e}").into()),
        }
        cx.notify();
    }

    fn set_widget(&mut self, name: &'static str, on: bool, cx: &mut Context<Self>) {
        let saved = config::config_file()
            .ok_or_else(|| "No APPDATA folder: the configuration cannot be found.".to_owned())
            .and_then(|path| config::save_enabled(&path, name, on).map_err(|e| e.to_string()));
        match saved {
            // The overlay sees the file change and reloads it.
            Ok(()) => self.load_config(),
            Err(e) => self.note = Some(e.into()),
        }
        cx.notify();
    }

    fn header(&self) -> Div {
        let dot = if self.overlay.is_running() {
            style::running()
        } else {
            style::stopped()
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px(style::gutter())
            .pt(style::gutter())
            .pb(px(8.0))
            .child(
                div()
                    .text_size(style::title_size())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Pitot"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(style::small_size())
                    .text_color(style::text_dim())
                    .child(div().size(px(8.0)).rounded_full().bg(dot))
                    .child(self.overlay.status()),
            )
    }

    fn overlay_rows(&self, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        match self.overlay {
            Overlay::Running { hidden, moving } => vec![
                switch_row(
                    "show",
                    "Show widgets",
                    Some("Off hides every widget until you turn it on again"),
                    !hidden,
                    true,
                    // A click flips the switch: hidden widgets show.
                    cx.listener(move |panel, _: &ClickEvent, _, cx| panel.set_shown(hidden, cx)),
                ),
                switch_row(
                    "move",
                    "Move widgets",
                    Some("Drag widgets with the mouse; their places are saved"),
                    moving,
                    true,
                    cx.listener(move |panel, _: &ClickEvent, _, cx| panel.set_moving(!moving, cx)),
                ),
            ],
            Overlay::NotRunning | Overlay::Starting => vec![button_row(
                "start",
                "Start the overlay",
                self.overlay == Overlay::NotRunning,
                cx.listener(|panel, _: &ClickEvent, _, cx| panel.start_overlay(cx)),
            )],
        }
    }

    fn widget_rows(&self, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        model::widgets()
            .iter()
            .zip(self.enabled.iter().copied())
            .enumerate()
            .map(|(index, (widget, on))| {
                let name = widget.name;
                switch_row(
                    ("widget", index),
                    widget.label,
                    None,
                    on,
                    true,
                    cx.listener(move |panel, _: &ClickEvent, _, cx| {
                        panel.set_widget(name, !on, cx);
                    }),
                )
            })
            .collect()
    }
}

impl Render for Panel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let overlay_rows = self.overlay_rows(cx);
        let widget_rows = self.widget_rows(cx);
        let quit = div()
            .id("quit")
            .px(px(14.0))
            .py(px(6.0))
            .rounded(style::radius())
            .border_1()
            .border_color(style::border())
            .cursor_pointer()
            .hover(|s| s.bg(style::row_hover()))
            .active(|s| s.bg(style::row_pressed()))
            .child("Quit Pitot")
            .on_click(cx.listener(|_, _: &ClickEvent, window, cx| {
                crate::app::save_placement(window);
                platform::send_to_overlay(OverlayCommand::Quit);
                cx.quit();
            }));
        div()
            .id("panel")
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .bg(style::background())
            .text_color(style::text())
            .font_family(style::FONT)
            .text_size(style::text_size())
            .child(self.header())
            .child(section("Overlay"))
            .children(overlay_rows)
            .child(section("Widgets"))
            .children(widget_rows)
            .when_some(self.note.clone(), |panel, note| {
                panel.child(
                    div()
                        .px(style::gutter())
                        .pt(px(8.0))
                        .text_size(style::small_size())
                        .text_color(style::stopped())
                        .child(note),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .px(style::gutter())
                    .py(style::gutter())
                    .child(quit),
            )
    }
}

fn section(title: &'static str) -> Div {
    div()
        .px(style::gutter())
        .pt(px(12.0))
        .pb(px(4.0))
        .text_size(style::small_size())
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(style::text_dim())
        .child(title)
}

/// A clickable row with a label, an optional line of detail, and a switch on the right.
fn switch_row(
    id: impl Into<ElementId>,
    label: &'static str,
    detail: Option<&'static str>,
    on: bool,
    usable: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    row(id, usable, on_click)
        .child(
            div()
                .flex()
                .flex_col()
                .child(label)
                .when_some(detail, |text, detail| {
                    text.child(
                        div()
                            .text_size(style::small_size())
                            .text_color(style::text_dim())
                            .child(detail),
                    )
                }),
        )
        .child(switch(on, usable))
}

/// A clickable row with a single label in the accent color.
fn button_row(
    id: impl Into<ElementId>,
    label: &'static str,
    usable: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    row(id, usable, on_click)
        .text_color(style::accent())
        .child(label)
}

fn row(
    id: impl Into<ElementId>,
    usable: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let row = div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .mx(px(8.0))
        .px(style::row_padding())
        .py(style::row_padding())
        .rounded(style::radius());
    if usable {
        row.cursor_pointer()
            .hover(|s| s.bg(style::row_hover()))
            .active(|s| s.bg(style::row_pressed()))
            .on_click(on_click)
    } else {
        row.opacity(style::DISABLED_OPACITY)
    }
}

/// A pill with a knob: right and blue when on.
fn switch(on: bool, usable: bool) -> Div {
    let track = if on {
        style::accent()
    } else {
        style::switch_off()
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .w(style::switch_width())
        .h(style::switch_height())
        .p(px(2.0))
        .rounded_full()
        .bg(track)
        .when(on, |s| s.justify_end())
        .when(!usable, |s| s.opacity(style::DISABLED_OPACITY))
        .child(div().size(px(16.0)).rounded_full().bg(style::knob()))
}
