//! Track map: the track's outline with every car as a dot at its lap distance.
//!
//! The outline comes from the player's own driving: one clean lap integrated from the car's velocity and heading
//! (see `outline`), saved per track in the tracks folder so later sessions have it at once. It is drawn once into
//! the bottom layer. The dots go on the layer above, which is drawn again only when a dot moved.
//!
//! The map is north up, unless turning it fills the widget better: of the turns in 5 degree steps, the one that
//! draws the track largest is used when it is more than 3 percent larger than north up.

mod outline;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::config::{self, WidgetConfig};
use crate::pipeline::View;
use crate::render::{Align, Canvas, Color, Rect};

use super::{LayerSpec, MAX_LAYERS, Redraw, Widget};
use outline::{Builder, Motion, Progress};

/// `PlayerTrackSurface` on the racing surface, and `CarIdxTrackSurface` for a car out of the world.
const ON_TRACK: i32 = 3;
const NOT_IN_WORLD: i32 = -1;
/// Space around the outline, in pixels.
const MARGIN: f32 = 14.0;
/// Turns tried when fitting the outline, in degrees.
const TURN_STEP: usize = 5;
/// North up unless a turn draws the track this much larger.
const NORTH_UP_PREFERENCE: f32 = 1.03;

const BACKGROUND: u32 = 0x10_1418;
const ROAD: u32 = 0x6A_727E;
const ROAD_EDGE: u32 = 0x05_0607;
const WHITE: u32 = 0xF0_F2F5;
const DIM: u32 = 0xA0_A6AE;
const PLAYER: u32 = 0xFF_B347;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Car,
    /// On pit road: dimmed.
    Pit,
    Player,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Dot {
    x: f32,
    y: f32,
    color: u32,
    kind: Kind,
}

/// The map's turn and scale for `points` in a `width` by `height` widget, written to `out` in widget pixels.
/// North is up before turning; `out` stays empty when the widget has no room.
fn fit(points: &[[f32; 2]], width: f32, height: f32, out: &mut Vec<[f32; 2]>) {
    out.clear();
    let (room_w, room_h) = (width - 2.0 * MARGIN, height - 2.0 * MARGIN);
    if points.is_empty() || room_w <= 0.0 || room_h <= 0.0 {
        return;
    }
    // Turning counterclockwise by `angle`: the scale that fits, and the turned bounds (x0, x1, y0, y1).
    let turned = |angle: f32| {
        let (sin, cos) = angle.sin_cos();
        let mut b = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];
        for p in points {
            let (x, y) = (p[0] * cos - p[1] * sin, p[0] * sin + p[1] * cos);
            b = [b[0].min(x), b[1].max(x), b[2].min(y), b[3].max(y)];
        }
        let scale = (room_w / (b[1] - b[0]).max(1.0)).min(room_h / (b[3] - b[2]).max(1.0));
        (scale, b)
    };
    let north = turned(0.0);
    let mut best = (0.0, north.0, north.1);
    for step in 1..180 / TURN_STEP {
        let angle = ((step * TURN_STEP) as f32).to_radians();
        let (scale, bounds) = turned(angle);
        if scale > best.1 {
            best = (angle, scale, bounds);
        }
    }
    if best.1 < north.0 * NORTH_UP_PREFERENCE {
        best = (0.0, north.0, north.1);
    }
    let (angle, scale, b) = best;
    let (sin, cos) = angle.sin_cos();
    let (mid_x, mid_y) = ((b[0] + b[1]) / 2.0, (b[2] + b[3]) / 2.0);
    out.extend(points.iter().map(|p| {
        let (x, y) = (p[0] * cos - p[1] * sin, p[0] * sin + p[1] * cos);
        // North (y) is up on screen.
        [
            width / 2.0 + (x - mid_x) * scale,
            height / 2.0 - (y - mid_y) * scale,
        ]
    }));
}

/// The point at lap distance `pct` on an outline sampled at evenly spaced lap distances.
fn point_at(points: &[[f32; 2]], pct: f32) -> [f32; 2] {
    let n = points.len();
    if n == 0 {
        return [0.0, 0.0];
    }
    let at = pct.clamp(0.0, 1.0) * n as f32;
    let i = at.floor();
    let t = at - i;
    let (a, b) = (points[i as usize % n], points[(i as usize + 1) % n]);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// To half pixels, so dots that did not visibly move do not cause a redraw.
fn snap_half(v: f32) -> f32 {
    (v * 2.0).round() / 2.0
}

/// The track map widget.
#[derive(Debug)]
pub struct TrackMapWidget {
    width: u32,
    height: u32,
    opacity: f32,
    /// Where outlines are kept; none without a configuration directory.
    folder: Option<PathBuf>,
    /// The session's track, once its session info arrived.
    track: Option<i64>,
    /// The outline in meters east and north of the line, or empty while the track is being mapped.
    outline: Vec<[f32; 2]>,
    /// The outline in widget pixels.
    screen: Vec<[f32; 2]>,
    builder: Builder,
    /// Share of the mapping lap done, in percent, or `None` while waiting for the line.
    progress: Option<u8>,
    dots: Vec<Dot>,
    next: Vec<Dot>,
}

impl TrackMapWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        Self::with_folder(config, config::tracks_dir())
    }

    /// A map that keeps its outlines in `folder`.
    pub fn with_folder(config: &WidgetConfig, folder: Option<PathBuf>) -> Self {
        Self {
            width: config.width.unwrap_or(440),
            height: config.height.unwrap_or(330),
            opacity: config.opacity,
            folder,
            track: None,
            outline: Vec::with_capacity(outline::POINTS),
            screen: Vec::with_capacity(outline::POINTS),
            builder: Builder::new(),
            progress: None,
            dots: Vec::with_capacity(72),
            next: Vec::with_capacity(72),
        }
    }

    fn file(&self, track: i64) -> Option<PathBuf> {
        self.folder
            .as_ref()
            .map(|dir| dir.join(format!("{track}.trk")))
    }

    /// Reads the track's outline, if one was saved. Runs when the session's track changes.
    fn load(&mut self) {
        self.outline.clear();
        self.screen.clear();
        let Some((track, path)) = self.track.and_then(|t| Some((t, self.file(t)?))) else {
            return;
        };
        match fs::read(&path) {
            Ok(bytes) if outline::from_bytes(&bytes, &mut self.outline) => {
                info!(track, "track map: outline loaded");
            }
            Ok(_) => warn!(
                track,
                "track map: the saved outline is damaged; mapping again"
            ),
            Err(_) => info!(
                track,
                "track map: no outline yet; mapping on the next clean lap"
            ),
        }
        self.fit();
    }

    fn fit(&mut self) {
        fit(
            &self.outline,
            self.width as f32,
            self.height as f32,
            &mut self.screen,
        );
    }

    /// Takes the finished lap as the outline and saves it, on its own thread so the render thread never waits
    /// for the disk.
    fn adopt(&mut self) {
        self.builder.outline_into(&mut self.outline);
        self.builder.restart();
        self.fit();
        let Some((track, path)) = self.track.and_then(|t| Some((t, self.file(t)?))) else {
            return;
        };
        info!(track, "track map: mapped from a clean lap");
        let bytes = outline::to_bytes(&self.outline);
        let saving = std::thread::Builder::new()
            .name("pitot-track-save".into())
            .spawn(move || match outline::save(&path, &bytes) {
                Ok(()) => info!(track, "track map: outline saved"),
                Err(e) => warn!(track, "track map: outline not saved: {e}"),
            });
        if let Err(e) = saving {
            warn!(track, "track map: outline not saved: {e}");
        }
    }

    /// Feeds the mapping lap with the player's car.
    fn map(&mut self, view: &View, redraw: &mut [Redraw; MAX_LAYERS]) {
        let (snap, vars) = (view.snapshot(), view.vars());
        let surface = snap.i32(vars.player_track_surface, 0);
        let motion = Motion {
            tick: snap.tick(),
            forward: snap.f32(vars.velocity_x, 0).unwrap_or(f32::NAN),
            left: snap.f32(vars.velocity_y, 0).unwrap_or(f32::NAN),
            heading: snap.f32(vars.yaw_north, 0).unwrap_or(f32::NAN),
            pct: snap.f32(vars.lap_dist_pct, 0).unwrap_or(-1.0),
            clean: snap.bool(vars.is_on_track, 0) == Some(true)
                && !snap.bool(vars.on_pit_road, 0).unwrap_or(false)
                && surface.is_none_or(|s| s == ON_TRACK),
        };
        let progress = match self.builder.observe(motion) {
            Progress::Done => {
                self.adopt();
                self.progress = None;
                redraw[0] = Redraw::All;
                return;
            }
            Progress::Mapping(share) => Some((share * 100.0) as u8),
            Progress::Waiting => None,
        };
        if progress != self.progress {
            self.progress = progress;
            redraw[0] = Redraw::All;
        }
    }

    /// Fills `next` with every car's dot.
    fn place_cars(&mut self, view: &View) {
        self.next.clear();
        let (snap, vars, session) = (view.snapshot(), view.vars(), view.session());
        let driver = |idx: usize| session.and_then(|s| s.driver_info.drivers.get(idx)?.as_ref());
        let player = snap
            .i32(vars.player_car_idx, 0)
            .and_then(|i| usize::try_from(i).ok())
            .or_else(|| session.and_then(|s| s.driver_info.player_car_idx));
        let count = snap.count(vars.car_lap_dist_pct);
        for car in 0..count {
            let Some(pct) = snap
                .f32(vars.car_lap_dist_pct, car)
                .filter(|p| (0.0..=1.0).contains(p))
            else {
                continue;
            };
            if snap.i32(vars.car_track_surface, car) == Some(NOT_IN_WORLD) {
                continue;
            }
            let info = driver(car);
            if info.is_some_and(|d| d.is_pace_car || d.is_spectator) {
                continue;
            }
            let kind = if Some(car) == player {
                Kind::Player
            } else if snap.bool(vars.car_on_pit_road, car) == Some(true) {
                Kind::Pit
            } else {
                Kind::Car
            };
            let [x, y] = point_at(&self.screen, pct);
            self.next.push(Dot {
                x: snap_half(x),
                y: snap_half(y),
                color: info.and_then(|d| d.car_class_color).unwrap_or(WHITE),
                kind,
            });
        }
        // Telemetry of the player's car only (from an `.ibt` file): the player's own lap distance.
        if count == 0
            && let Some(pct) = snap
                .f32(vars.lap_dist_pct, 0)
                .filter(|p| (0.0..=1.0).contains(p))
        {
            let [x, y] = point_at(&self.screen, pct);
            self.next.push(Dot {
                x: snap_half(x),
                y: snap_half(y),
                color: PLAYER,
                kind: Kind::Player,
            });
        }
    }

    fn dot_radius(&self) -> f32 {
        (self.width.min(self.height) as f32 / 48.0).clamp(3.5, 7.0)
    }

    fn road_width(&self) -> f32 {
        (self.width.min(self.height) as f32 / 40.0).clamp(4.0, 9.0)
    }

    fn draw_outline(&self, canvas: &mut Canvas<'_>) {
        let (w, h) = (self.width as f32, self.height as f32);
        if self.screen.len() < 2 {
            let message = if self.track.is_none() {
                "Waiting for the session"
            } else {
                "Drive a clean lap to map the track"
            };
            canvas.text(
                message,
                Rect::new(0.0, h / 2.0 - 22.0, w, 20.0),
                14.0,
                Align::Center,
                Color::rgb(DIM, 1.0),
            );
            if let Some(percent) = self.progress {
                let bar = Rect::new(w * 0.2, h / 2.0 + 6.0, w * 0.6, 6.0);
                canvas.fill(bar, Color::rgb(DIM, 0.25));
                let done = Rect::new(bar.x, bar.y, bar.w * f32::from(percent) / 100.0, bar.h);
                canvas.fill(done, Color::rgb(PLAYER, 1.0));
            }
            return;
        }
        let road = self.road_width();
        canvas.polyline(&self.screen, true, road + 3.0, Color::rgb(ROAD_EDGE, 0.9));
        canvas.polyline(&self.screen, true, road, Color::rgb(ROAD, 1.0));
        // The start line, across the road at the first point.
        let (a, b) = (self.screen[0], self.screen[1]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let length = dx.hypot(dy);
        if length > 0.0 {
            let half = road * 1.3;
            let (nx, ny) = (-dy / length * half, dx / length * half);
            canvas.line(
                (a[0] - nx, a[1] - ny),
                (a[0] + nx, a[1] + ny),
                2.5,
                Color::rgb(WHITE, 1.0),
            );
        }
    }

    fn draw_dots(&self, canvas: &mut Canvas<'_>) {
        let r = self.dot_radius();
        for dot in self.dots.iter().filter(|d| d.kind != Kind::Player) {
            let alpha = if dot.kind == Kind::Pit { 0.35 } else { 1.0 };
            canvas.fill_circle((dot.x, dot.y), r + 1.2, Color::rgb(ROAD_EDGE, 0.8 * alpha));
            canvas.fill_circle((dot.x, dot.y), r, Color::rgb(dot.color, alpha));
        }
        // The player on top.
        for dot in self.dots.iter().filter(|d| d.kind == Kind::Player) {
            canvas.fill_circle((dot.x, dot.y), r + 2.5, Color::rgb(WHITE, 1.0));
            canvas.fill_circle((dot.x, dot.y), r + 1.0, Color::rgb(PLAYER, 1.0));
        }
    }
}

impl Widget for TrackMapWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The outline and background below, the dots above.
    fn layers(&self) -> Vec<LayerSpec> {
        let layer = LayerSpec {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
            scrolling: false,
        };
        vec![layer, layer]
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(100)
    }

    fn update(&mut self, view: &mut View, _now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        if let Some(info) = view.session()
            && info.weekend.track_id != self.track
        {
            let length = info.weekend.track_length_km.map(|km| km * 1000.0);
            self.track = info.weekend.track_id;
            self.builder.restart();
            self.builder.set_expected_length(length);
            self.progress = None;
            self.load();
            self.dots.clear();
            redraw[0] = Redraw::All;
            redraw[1] = Redraw::All;
        }
        if self.outline.is_empty() {
            if self.track.is_some() {
                self.map(view, redraw);
            }
            if self.outline.is_empty() {
                return;
            }
        }
        self.place_cars(view);
        if self.next != self.dots {
            std::mem::swap(&mut self.dots, &mut self.next);
            redraw[1] = Redraw::All;
        }
    }

    fn draw(&self, layer: usize, canvas: &mut Canvas<'_>) {
        if layer == 0 {
            canvas.fill(
                Rect::new(0.0, 0.0, self.width as f32, self.height as f32),
                Color::rgb(BACKGROUND, self.opacity),
            );
            self.draw_outline(canvas);
        } else {
            self.draw_dots(canvas);
        }
    }

    fn reset(&mut self) {
        // The outline belongs to the track and stays; a lap being mapped starts over.
        self.builder.restart();
        self.progress = None;
        self.dots.clear();
        self.next.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ellipse(east: f32, north: f32) -> Vec<[f32; 2]> {
        (0..outline::POINTS)
            .map(|i| {
                let a = i as f32 / outline::POINTS as f32 * std::f32::consts::TAU;
                [east * a.cos(), north * a.sin()]
            })
            .collect()
    }

    fn bounds(points: &[[f32; 2]]) -> [f32; 4] {
        points
            .iter()
            .fold([f32::MAX, f32::MIN, f32::MAX, f32::MIN], |b, p| {
                [
                    b[0].min(p[0]),
                    b[1].max(p[0]),
                    b[2].min(p[1]),
                    b[3].max(p[1]),
                ]
            })
    }

    #[test]
    fn a_wide_track_stays_north_up() {
        let mut out = Vec::new();
        fit(&ellipse(1400.0, 1000.0), 440.0, 330.0, &mut out);
        let b = bounds(&out);
        assert!(
            (b[1] - b[0] - (440.0 - 2.0 * MARGIN)).abs() < 0.5,
            "fills the width: {b:?}"
        );
        assert!(
            b[0] >= MARGIN - 0.5 && b[3] <= 330.0 - MARGIN + 0.5,
            "{b:?}"
        );
        // East is right and north is up: the easternmost point (the first) is at the right, the northernmost
        // (a quarter in) at the top.
        let quarter = out[outline::POINTS / 4];
        assert!((out[0][0] - b[1]).abs() < 0.5 && (quarter[1] - b[2]).abs() < 0.5);
    }

    #[test]
    fn a_tall_track_is_turned_to_fill_a_wide_widget() {
        let mut out = Vec::new();
        fit(&ellipse(300.0, 1000.0), 440.0, 330.0, &mut out);
        let b = bounds(&out);
        assert!(b[1] - b[0] > b[3] - b[2], "the long side is across: {b:?}");
        assert!(b[0] >= MARGIN - 0.5 && b[1] <= 440.0 - MARGIN + 0.5);
        assert!(b[2] >= MARGIN - 0.5 && b[3] <= 330.0 - MARGIN + 0.5);
    }

    #[test]
    fn cars_are_placed_between_outline_points() {
        let points = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert_eq!(point_at(&points, 0.0), [0.0, 0.0]);
        assert_eq!(point_at(&points, 0.125), [5.0, 0.0]);
        assert_eq!(point_at(&points, 0.875), [0.0, 5.0], "back toward the line");
        assert_eq!(point_at(&points, 1.0), [0.0, 0.0]);
        assert_eq!(point_at(&[], 0.5), [0.0, 0.0]);
    }

    #[test]
    fn no_room_draws_nothing() {
        let mut out = vec![[1.0, 1.0]];
        fit(&ellipse(100.0, 100.0), 20.0, 20.0, &mut out);
        assert!(out.is_empty());
    }
}
