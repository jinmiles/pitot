//! What the window shows and what clicks do: the overlay's switches, the background opacity and text size
//! sliders, one switch per widget, updates, and Quit.

use std::cell::Cell;
use std::rc::Rc;

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    App, Bounds, ClickEvent, Context, DispatchPhase, Div, ElementId, Entity, FocusHandle,
    IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Render, SharedString, Stateful, Styled, Subscription,
    Window, canvas, div, prelude::*, px, relative,
};
use pitot_overlay::config;
use pitot_overlay::platform::{self, ControlEvent, ControlLink, OverlayCommand};
use velopack::UpdateInfo;

use crate::model::{self, Overlay, Update};
use crate::style;
use crate::updates::{self, Event, Updater};

/// How a slider is being moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Adjusting {
    /// Dragged with the left button.
    Mouse,
    /// Stepped with the keys while it has the focus.
    Keys,
}

/// A setting of the Appearance section, each with its own slider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    /// `[general] background_opacity`.
    Opacity,
    /// `[general] text_size`.
    TextSize,
}

impl Setting {
    const ALL: [Self; 2] = [Self::Opacity, Self::TextSize];

    fn range(self) -> model::Range {
        match self {
            Self::Opacity => model::OPACITY,
            Self::TextSize => model::TEXT_SIZE,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Opacity => "background-opacity",
            Self::TextSize => "text-size",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Opacity => "Background opacity",
            Self::TextSize => "Text size",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Opacity => "Widget backgrounds only; text and graphs stay as they are",
            Self::TextSize => "Widgets keep their size, so larger text fits fewer rows",
        }
    }

    fn in_config(self, config: &config::Config) -> u8 {
        match self {
            Self::Opacity => config.general.background_opacity,
            Self::TextSize => config.general.text_size,
        }
    }

    /// Shows `value` on the widgets without saving it.
    fn preview(self, value: u8) -> OverlayCommand {
        match self {
            Self::Opacity => OverlayCommand::BackgroundPreview(value),
            Self::TextSize => OverlayCommand::TextSizePreview(value),
        }
    }

    /// Tells the overlay what the file holds.
    fn saved(self, value: u8) -> OverlayCommand {
        match self {
            Self::Opacity => OverlayCommand::BackgroundSaved(value),
            Self::TextSize => OverlayCommand::TextSizeSaved(value),
        }
    }

    /// Writes `value` into the file at `path`, in place.
    fn save(self, path: &std::path::Path, value: u8) -> Result<(), config::ConfigError> {
        match self {
            Self::Opacity => config::save_background_opacity(path, value),
            Self::TextSize => config::save_text_size(path, value),
        }
    }
}

/// A percent slider: its value as shown, how it moves, and where its track is.
struct Slider {
    value: u8,
    /// While it moves, how; the file still holds the value from before.
    adjusting: Option<Adjusting>,
    /// The track where it was last laid out, to turn the pointer's position into a value.
    track: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
}

/// The window's part in updates: the installation, if it runs from one, and where update threads send news.
pub struct Updates {
    pub updater: Option<Updater>,
    pub events: UnboundedSender<Event>,
}

pub struct Panel {
    overlay: Overlay,
    /// Per widget of [`model::widgets`], whether it is enabled in `config.toml`.
    enabled: Vec<bool>,
    /// The sliders of [`Setting::ALL`], in its order.
    sliders: [Slider; 2],
    /// `[general] check_updates`.
    check_updates: bool,
    update: Update,
    /// What the update button installs, once downloaded.
    pending: Option<Box<UpdateInfo>>,
    updates: Updates,
    /// The version shown in the header.
    version: SharedString,
    /// The last problem, such as a configuration file that does not parse.
    note: Option<SharedString>,
    _activation: Subscription,
}

impl Panel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, updates: Updates) -> Self {
        let activation = cx.observe_window_activation(window, |panel, window, cx| {
            let active = window.is_window_active();
            // The widgets are always on top; the window goes above them while it is used.
            ControlLink::keep_above_widgets(active);
            // The file may have been edited elsewhere while the window was in the background.
            if active {
                panel.load_config();
                cx.notify();
            } else {
                // The button may come up in another window, or a held key with another window focused.
                for setting in Setting::ALL {
                    panel.finish(setting, cx);
                }
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
        let defaults = config::Config::default();
        let slider = |setting: Setting, cx: &mut Context<Self>| Slider {
            value: setting.in_config(&defaults),
            adjusting: None,
            track: Rc::new(Cell::new(Bounds::default())),
            focus: cx.focus_handle(),
        };
        let sliders = Setting::ALL.map(|setting| slider(setting, cx));
        let installed = updates.updater.as_ref().map(Updater::version);
        let mut panel = Self {
            overlay,
            enabled: model::enabled(&defaults),
            sliders,
            check_updates: defaults.general.check_updates,
            update: if updates.updater.is_some() {
                Update::Off
            } else {
                Update::NotInstalled
            },
            pending: None,
            updates,
            version: model::version_text(installed.as_deref()).into(),
            note: None,
            _activation: activation,
        };
        panel.load_config();
        // Once per opening of the window.
        panel.follow_check_setting();
        panel
    }

    /// Whether an update is being installed: the window stays open until the installer takes over.
    pub fn installing(&self) -> bool {
        self.update.installing()
    }

    fn load_config(&mut self) {
        let Some(path) = config::config_file() else {
            self.note = Some("No APPDATA folder: the configuration cannot be found.".into());
            return;
        };
        match config::load_or_create(&path) {
            Ok(config) => {
                self.enabled = model::enabled(&config);
                self.check_updates = config.general.check_updates;
                // A moving slider keeps its own value until it is saved.
                for setting in Setting::ALL {
                    let slider = self.slider_mut(setting);
                    if slider.adjusting.is_none() {
                        slider.value = setting.in_config(&config);
                    }
                }
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
            ControlEvent::ConfigChanged => {
                self.load_config();
                self.follow_check_setting();
            }
            // The overlay quits for the update; the window closes once the installer waits for it.
            ControlEvent::Close if self.installing() => self.overlay = Overlay::NotRunning,
            ControlEvent::Close => return true,
            ControlEvent::Activate => {}
        }
        cx.notify();
        false
    }

    /// Takes news from an update thread. Returns `true` when the window should close.
    pub fn on_update(&mut self, event: Event, cx: &mut Context<Self>) -> bool {
        match event {
            Event::State(state) => {
                // A check that went on after checks were turned off shows nothing; an install always shows.
                let installs = state.installing() || state == Update::InstallFailed;
                if !self.check_updates && !installs {
                    return false;
                }
                self.update = state;
            }
            Event::Ready(update) => {
                if !self.check_updates {
                    return false;
                }
                self.update = Update::Ready {
                    version: update.TargetFullRelease.Version.clone(),
                };
                self.pending = Some(update);
            }
            Event::Quit => return true,
        }
        cx.notify();
        false
    }

    /// Starts a check when `[general] check_updates` is on and none runs, and shows that checks are off when it is
    /// off. With checks off, nothing is sent over the network.
    fn follow_check_setting(&mut self) {
        let Some(updater) = self.updates.updater.clone() else {
            return;
        };
        if !self.check_updates {
            if !self.update.installing() {
                self.update = Update::Off;
                self.pending = None;
            }
            return;
        }
        if !self.update.may_check() {
            return;
        }
        self.update = Update::Checking;
        let events = self.updates.events.clone();
        updates::check_and_download(updater, move |event| {
            let _ = events.unbounded_send(event);
        });
    }

    fn set_check_updates(&mut self, on: bool, cx: &mut Context<Self>) {
        let saved = config::config_file()
            .ok_or_else(|| "No APPDATA folder: the configuration cannot be found.".to_owned())
            .and_then(|path| config::save_check_updates(&path, on).map_err(|e| e.to_string()));
        match saved {
            Ok(()) => self.load_config(),
            Err(e) => self.note = Some(e.into()),
        }
        self.follow_check_setting();
        cx.notify();
    }

    /// The update button: quits the overlay, installs the downloaded update, and starts Pitot again on it.
    fn install_update(&mut self, cx: &mut Context<Self>) {
        let (Some(updater), Some(update)) = (self.updates.updater.clone(), self.pending.take())
        else {
            return;
        };
        self.update = Update::Closing {
            version: update.TargetFullRelease.Version.clone(),
        };
        let events = self.updates.events.clone();
        updates::install(updater, update, move |event| {
            let _ = events.unbounded_send(event);
        });
        cx.notify();
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
        match platform::start_overlay() {
            Ok(()) => self.overlay = Overlay::Starting,
            Err(e) => self.note = Some(format!("Cannot start the overlay: {e}").into()),
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

    fn slider(&self, setting: Setting) -> &Slider {
        &self.sliders[setting as usize]
    }

    fn slider_mut(&mut self, setting: Setting) -> &mut Slider {
        &mut self.sliders[setting as usize]
    }

    /// Shows `value` on the slider and on the widgets, without saving it.
    fn preview(&mut self, setting: Setting, value: u8, cx: &mut Context<Self>) {
        let slider = self.slider_mut(setting);
        if value == slider.value {
            return;
        }
        slider.value = value;
        // Without an overlay there is nothing to show it on; the value is saved when the slider stops.
        let _ = platform::send_to_overlay(setting.preview(value));
        cx.notify();
    }

    fn start_drag(
        &mut self,
        setting: Setting,
        x: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slider = self.slider_mut(setting);
        window.focus(&slider.focus);
        slider.adjusting = Some(Adjusting::Mouse);
        self.drag_to(setting, x, cx);
        cx.notify();
    }

    fn drag_to(&mut self, setting: Setting, x: Pixels, cx: &mut Context<Self>) {
        let track = self.slider(setting).track.get();
        let value = setting.range().value_at(
            f32::from(x),
            f32::from(track.origin.x),
            f32::from(track.size.width),
        );
        if let Some(value) = value {
            self.preview(setting, value, cx);
        }
    }

    fn step(&mut self, setting: Setting, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keys = &event.keystroke;
        let slider = self.slider_mut(setting);
        let Some(value) = setting
            .range()
            .step(slider.value, &keys.key, keys.modifiers.shift)
        else {
            return;
        };
        slider.adjusting.get_or_insert(Adjusting::Keys);
        self.preview(setting, value, cx);
        cx.stop_propagation();
    }

    /// Ends a move of a slider: saves its value into `config.toml` and tells the overlay what the file now holds.
    fn finish(&mut self, setting: Setting, cx: &mut Context<Self>) {
        let slider = self.slider_mut(setting);
        if slider.adjusting.take().is_none() {
            return;
        }
        let value = slider.value;
        let saved = config::config_file()
            .ok_or_else(|| "No APPDATA folder: the configuration cannot be found.".to_owned())
            .and_then(|path| setting.save(&path, value).map_err(|e| e.to_string()));
        // The file's value, which is the old one if the save failed.
        self.load_config();
        if let Err(e) = saved {
            self.note = Some(e.into());
        }
        // Also without a saved value, so the overlay goes back to what the file holds.
        let _ = platform::send_to_overlay(setting.saved(self.slider(setting).value));
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
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_size(style::title_size())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Pitot"),
                    )
                    .child(
                        div()
                            .text_size(style::small_size())
                            .text_color(style::text_dim())
                            .child(self.version.clone()),
                    ),
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
                    Some("Off hides every widget until you turn it on again".into()),
                    !hidden,
                    true,
                    // A click flips the switch: hidden widgets show.
                    cx.listener(move |panel, _: &ClickEvent, _, cx| panel.set_shown(hidden, cx)),
                ),
                switch_row(
                    "move",
                    "Move widgets",
                    Some("Drag widgets with the mouse; their places are saved".into()),
                    moving,
                    true,
                    cx.listener(move |panel, _: &ClickEvent, _, cx| panel.set_moving(!moving, cx)),
                ),
            ],
            Overlay::NotRunning | Overlay::Starting => vec![button_row(
                "start",
                "Start the overlay",
                None,
                self.overlay == Overlay::NotRunning && !self.installing(),
                cx.listener(|panel, _: &ClickEvent, _, cx| panel.start_overlay(cx)),
            )],
        }
    }

    /// A setting's row: its label and percent, a line of detail, and a slider that previews on the widgets while it
    /// moves and saves when it stops. A click or drag sets it; with the focus, the arrow keys (Shift for a page),
    /// Page Up and Page Down, Home and End step it.
    fn slider_row(&self, setting: Setting, window: &Window, cx: &mut Context<Self>) -> Div {
        let state = self.slider(setting);
        let fraction = setting.range().fraction(state.value);
        let focused = state.focus.is_focused(window);
        let knob = style::slider_knob();
        let track = Rc::clone(&state.track);
        let panel = cx.entity();
        let slider = div()
            .id(setting.id())
            .track_focus(&state.focus)
            .w_full()
            .py(px(10.0))
            // The knob stays inside the row at both ends.
            .px(knob / 2.0)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |panel, event: &MouseDownEvent, window, cx| {
                    panel.start_drag(setting, event.position.x, window, cx);
                }),
            )
            .on_key_down(cx.listener(move |panel, event: &KeyDownEvent, _, cx| {
                panel.step(setting, event, cx);
            }))
            .on_key_up(cx.listener(move |panel, _: &KeyUpEvent, _, cx| {
                if panel.slider(setting).adjusting == Some(Adjusting::Keys) {
                    panel.finish(setting, cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(style::slider_height())
                    .rounded_full()
                    .bg(style::switch_off())
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(style::accent()),
                    )
                    .child(
                        div()
                            .absolute()
                            .top((style::slider_height() - knob) / 2.0)
                            .left(relative(fraction))
                            .ml(-knob / 2.0)
                            .size(knob)
                            .rounded_full()
                            .bg(style::knob())
                            .when(focused, |k| k.border_2().border_color(style::accent())),
                    )
                    .child(
                        canvas(
                            move |bounds, _, _| track.set(bounds),
                            move |_, _, window, _| follow_drag(&panel, setting, window),
                        )
                        .absolute()
                        .size_full(),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .mx(px(8.0))
            .px(style::row_padding())
            .py(style::row_padding())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(setting.label())
                    .child(
                        div()
                            .text_color(style::text_dim())
                            .child(format!("{}%", state.value)),
                    ),
            )
            .child(
                div()
                    .text_size(style::small_size())
                    .text_color(style::text_dim())
                    .child(setting.detail()),
            )
            .child(slider)
    }

    /// The install button, while a downloaded update waits.
    fn update_banner(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let label = self.update.button()?;
        Some(button_row(
            "update",
            label,
            Some("Restarts the overlay and this window".into()),
            true,
            cx.listener(|panel, _: &ClickEvent, _, cx| panel.install_update(cx)),
        ))
    }

    /// The update checks' switch, which always says what a check sends and where.
    fn update_row(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let on = self.check_updates;
        switch_row(
            "check-updates",
            "Check for updates",
            Some(model::UPDATE_DISCLOSURE.into()),
            on,
            !self.installing(),
            cx.listener(move |panel, _: &ClickEvent, _, cx| panel.set_check_updates(!on, cx)),
        )
    }

    /// What the last check found, below the switch.
    fn update_status(&self) -> Option<Div> {
        let line = self.update.line()?;
        Some(
            div()
                .mx(px(8.0))
                .px(style::row_padding())
                .text_size(style::small_size())
                .text_color(style::text_dim())
                .child(line),
        )
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let overlay_rows = self.overlay_rows(cx);
        let slider_rows = Setting::ALL.map(|setting| self.slider_row(setting, window, cx));
        let widget_rows = self.widget_rows(cx);
        let update_banner = self.update_banner(cx);
        let update_row = self.update_row(cx);
        let update_status = self.update_status();
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
            .children(update_banner)
            .child(section("Overlay"))
            .children(overlay_rows)
            .child(section("Appearance"))
            .children(slider_rows)
            .child(section("Widgets"))
            .children(widget_rows)
            .child(section("Updates"))
            .child(update_row)
            .children(update_status)
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

/// While `setting`'s slider is dragged, follows the pointer anywhere (the window has the mouse captured until the
/// button comes up) and saves when the button comes up. Registered for the next frame, as the window's own
/// listeners are.
fn follow_drag(panel: &Entity<Panel>, setting: Setting, window: &mut Window) {
    let dragging = move |panel: &Entity<Panel>, cx: &App| {
        panel.read(cx).slider(setting).adjusting == Some(Adjusting::Mouse)
    };
    let moves = panel.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble || !dragging(&moves, cx) {
            return;
        }
        moves.update(cx, |panel, cx| {
            if event.pressed_button == Some(MouseButton::Left) {
                panel.drag_to(setting, event.position.x, cx);
            } else {
                // The button came up where the window did not see it.
                panel.finish(setting, cx);
            }
        });
    });
    let ups = panel.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && dragging(&ups, cx)
        {
            ups.update(cx, |panel, cx| panel.finish(setting, cx));
        }
    });
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
    detail: Option<SharedString>,
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

/// A clickable row with a label in the accent color and an optional line of detail.
fn button_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    detail: Option<SharedString>,
    usable: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    row(id, usable, on_click).child(
        div()
            .flex()
            .flex_col()
            .child(div().text_color(style::accent()).child(label.into()))
            .when_some(detail, |text, detail| {
                text.child(
                    div()
                        .text_size(style::small_size())
                        .text_color(style::text_dim())
                        .child(detail),
                )
            }),
    )
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
