//! Render counters, logged once at exit next to the data counters so a measurement can tell where the overlay's
//! time went: per widget, how often it was updated and asked GPUI to draw its window, how often GPUI drew it and how
//! long the widget's drawing took inside GPUI's frame, and how many paint messages its windows received (GPUI's
//! display-refresh thread invalidates every window at each refresh, drawn or not); for the scheduler, how often it
//! woke and why. Counting is a few integer additions per update, paint, or message; nothing is logged while the
//! overlay runs.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tracing::info;

use crate::wake::reason;

/// One widget's counters, kept across configuration reloads.
#[derive(Debug)]
pub struct WidgetStats {
    pub name: &'static str,
    updates: Cell<u64>,
    /// Updates that reported a change, which asked GPUI to draw the widget's window.
    redraws: Cell<u64>,
    /// GPUI's draws of the widget's window: each draw paints the widget once and presents the window.
    paints: Cell<u64>,
    paint_nanos: Cell<u64>,
    paint_max_nanos: Cell<u64>,
    /// Windows made for the widget.
    windows: Cell<u64>,
    /// `WM_PAINT` messages its windows received, counted on GPUI's thread by the window's subclass.
    paint_messages: Arc<AtomicU64>,
    /// Heap allocations during its paints (`--features alloc-count`).
    allocations: Cell<u64>,
}

impl WidgetStats {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            updates: Cell::new(0),
            redraws: Cell::new(0),
            paints: Cell::new(0),
            paint_nanos: Cell::new(0),
            paint_max_nanos: Cell::new(0),
            windows: Cell::new(0),
            paint_messages: Arc::new(AtomicU64::new(0)),
            allocations: Cell::new(0),
        }
    }

    pub fn updated(&self, redraw: bool) {
        add(&self.updates, 1);
        if redraw {
            add(&self.redraws, 1);
        }
    }

    pub fn window_made(&self) {
        add(&self.windows, 1);
    }

    /// The counter a widget window's subclass adds its paint messages to.
    pub fn paint_messages(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.paint_messages)
    }

    /// One paint, which took `took` and made `allocations` heap allocations (0 without `alloc-count`).
    pub fn painted(&self, took: Duration, allocations: u64) {
        let nanos = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
        add(&self.paints, 1);
        add(&self.paint_nanos, nanos);
        if nanos > self.paint_max_nanos.get() {
            self.paint_max_nanos.set(nanos);
        }
        add(&self.allocations, allocations);
    }

    fn log(&self, seconds: f64) {
        let paints = self.paints.get();
        let per_second = |n: u64| round1(n as f64 / seconds);
        let ms = |nanos: u64| nanos as f64 / 1.0e6;
        info!(
            widget = self.name,
            updates = self.updates.get(),
            redraws = self.redraws.get(),
            paints,
            paints_per_s = per_second(paints),
            paint_ms_per_s = round3(ms(self.paint_nanos.get()) / seconds),
            paint_us_mean = round1(if paints == 0 {
                0.0
            } else {
                self.paint_nanos.get() as f64 / paints as f64 / 1.0e3
            }),
            paint_ms_max = round3(ms(self.paint_max_nanos.get())),
            windows = self.windows.get(),
            paint_messages = self.paint_messages.load(Ordering::Relaxed),
            paint_messages_per_s = per_second(self.paint_messages.load(Ordering::Relaxed)),
            paint_allocations_mean = round1(if paints == 0 {
                0.0
            } else {
                self.allocations.get() as f64 / paints as f64
            }),
            "widget render counters at exit"
        );
    }
}

/// The scheduler's counters.
#[derive(Debug)]
pub struct RenderStats {
    started: Instant,
    steps: u64,
    /// Steps woken for each reason bit ([`reason`]), in bit order.
    by_reason: [u64; 6],
    /// Times GPUI became windowed.
    windowed: u64,
}

impl RenderStats {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            steps: 0,
            by_reason: [0; 6],
            windowed: 0,
        }
    }

    pub fn step(&mut self, reasons: u32) {
        self.steps += 1;
        for (bit, count) in self.by_reason.iter_mut().enumerate() {
            if reasons & (1 << bit) != 0 {
                *count += 1;
            }
        }
    }

    pub fn became_windowed(&mut self) {
        self.windowed += 1;
    }

    /// Logs the scheduler's counters and each widget's that was made at least once.
    pub fn log(&self, widgets: &[std::rc::Rc<WidgetStats>]) {
        let seconds = self.started.elapsed().as_secs_f64().max(1e-9);
        let count = |bit: u32| self.by_reason[bit.trailing_zeros() as usize];
        info!(
            seconds = round1(seconds),
            steps = self.steps,
            steps_per_s = round1(self.steps as f64 / seconds),
            tick = count(reason::TICK),
            timer = count(reason::TIMER),
            messages = count(reason::MESSAGE),
            command = count(reason::COMMAND),
            moves = count(reason::MOVE),
            config = count(reason::CONFIG),
            windowed = self.windowed,
            "render counters at exit"
        );
        for widget in widgets.iter().filter(|w| w.updates.get() > 0) {
            widget.log(seconds);
        }
    }
}

impl Default for RenderStats {
    fn default() -> Self {
        Self::new()
    }
}

fn add(cell: &Cell<u64>, n: u64) {
    cell.set(cell.get().saturating_add(n));
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_add_up() {
        let stats = WidgetStats::new("inputs");
        stats.updated(true);
        stats.updated(false);
        stats.painted(Duration::from_micros(300), 4);
        stats.painted(Duration::from_micros(100), 2);
        assert_eq!((stats.updates.get(), stats.redraws.get()), (2, 1));
        assert_eq!(stats.paints.get(), 2);
        assert_eq!(stats.paint_nanos.get(), 400_000);
        assert_eq!(stats.paint_max_nanos.get(), 300_000);
        assert_eq!(stats.allocations.get(), 6);
        stats.paint_messages().fetch_add(3, Ordering::Relaxed);
        assert_eq!(stats.paint_messages.load(Ordering::Relaxed), 3);
        let mut render = RenderStats::new();
        render.step(reason::TICK | reason::MESSAGE);
        render.step(reason::TIMER);
        assert_eq!(render.steps, 2);
        assert_eq!(render.by_reason[0], 1);
        assert_eq!(render.by_reason[1], 1);
        assert_eq!(render.by_reason[2], 1);
    }
}
