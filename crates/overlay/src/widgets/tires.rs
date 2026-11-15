//! Tires: the four tires laid out like the car, each with tread remaining and carcass temperature at its left,
//! middle, and right, and the distance it has run; the compound on the car and, when limited, the dry sets used.
//!
//! The sim updates wear and carcass temperatures in live telemetry only while the car is in its pit stall, so
//! they describe the last stop, not the tire now. Until they change during the connection the widget cannot
//! tell how old they are and dims them; afterwards the header shows the lap they were taken on. The distance
//! updates every 100 m.
//!
//! While the sim plays a replay, the widget shows the surface temperatures, tread, and hot pressures of the
//! replayed moment instead, from the sim's own telemetry files of the session (live telemetry has no surface
//! temperatures). Without such a file it shows the capture's values, or nothing when no capture plays either.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use pitot_irsdk::capture::Moment;
use pitot_irsdk::live::{Snapshot, VarId};

use crate::config::{NoOptions, WidgetConfig};
use crate::pipeline::{Display, Source, TireSample, Vars, View};
use crate::render::{Align, Canvas, Color, Rect};

use super::text::FixedText;
use super::{Build, MAX_LAYERS, Redraw, Register, Widget};

/// Left front, right front, left rear, right rear: the order of the blocks, left to right and top to bottom.
const CORNERS: usize = 4;
/// Left, middle, and right of a tire.
const ZONES: usize = 3;

/// Handles of one corner's variables.
#[derive(Debug, Clone, Copy)]
struct CornerVars {
    wear: [VarId; ZONES],
    carcass: [VarId; ZONES],
    odometer: VarId,
}

fn corner_vars(v: &Vars) -> [CornerVars; CORNERS] {
    [
        CornerVars {
            wear: [v.lf_wear_l, v.lf_wear_m, v.lf_wear_r],
            carcass: [v.lf_carcass_l, v.lf_carcass_m, v.lf_carcass_r],
            odometer: v.lf_odometer,
        },
        CornerVars {
            wear: [v.rf_wear_l, v.rf_wear_m, v.rf_wear_r],
            carcass: [v.rf_carcass_l, v.rf_carcass_m, v.rf_carcass_r],
            odometer: v.rf_odometer,
        },
        CornerVars {
            wear: [v.lr_wear_l, v.lr_wear_m, v.lr_wear_r],
            carcass: [v.lr_carcass_l, v.lr_carcass_m, v.lr_carcass_r],
            odometer: v.lr_odometer,
        },
        CornerVars {
            wear: [v.rr_wear_l, v.rr_wear_m, v.rr_wear_r],
            carcass: [v.rr_carcass_l, v.rr_carcass_m, v.rr_carcass_r],
            odometer: v.rr_odometer,
        },
    ]
}

/// The values that change only in the pit stall: wear, then carcass temperatures, per corner and zone.
type PitValues = [f32; CORNERS * ZONES * 2];

fn pit_values(snap: &Snapshot, corners: &[CornerVars; CORNERS]) -> PitValues {
    let mut values = [f32::NAN; CORNERS * ZONES * 2];
    for (c, corner) in corners.iter().enumerate() {
        for z in 0..ZONES {
            values[c * ZONES + z] = snap.f32(corner.wear[z], 0).unwrap_or(f32::NAN);
            values[CORNERS * ZONES + c * ZONES + z] =
                snap.f32(corner.carcass[z], 0).unwrap_or(f32::NAN);
        }
    }
    values
}

/// Notices when the pit stall values change, which dates them.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PitWatch {
    seen: Option<PitValues>,
    /// The lap the values were taken on, once they changed during the connection.
    taken: Option<i32>,
}

impl PitWatch {
    /// Feeds the latest values and the lap the car is on.
    fn observe(&mut self, values: &PitValues, lap: Option<i32>) {
        let changed = self.seen.is_some_and(|seen| {
            seen.iter()
                .zip(values)
                .any(|(a, b)| a.to_bits() != b.to_bits())
        });
        if changed {
            self.taken = Some(lap.unwrap_or(0).max(0));
        }
        self.seen = Some(*values);
    }

    /// The values changed during the connection, so they are from a stop seen by the overlay.
    fn dated(&self) -> bool {
        self.taken.is_some()
    }
}

/// Blue when cold, green in the usual window, yellow then red when hot, for carcass temperatures in °C.
pub(crate) fn temperature_color(celsius: f32) -> u32 {
    const STOPS: [(f32, u32); 4] = [
        (50.0, 0x4F_A3FF),
        (75.0, 0x3D_D68C),
        (95.0, 0xFF_D24D),
        (110.0, 0xFF_4D4D),
    ];
    if celsius.is_nan() || celsius <= STOPS[0].0 {
        return STOPS[0].1;
    }
    for pair in STOPS.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if celsius <= t1 {
            return mix(c0, c1, (celsius - t0) / (t1 - t0));
        }
    }
    STOPS[STOPS.len() - 1].1
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let channel = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((x + (y - x) * t.clamp(0.0, 1.0)).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// White with most of the tread left, yellow at half wear, red when nearly worn through.
pub(crate) fn wear_color(remaining: f32) -> u32 {
    if remaining >= 0.8 {
        WHITE
    } else if remaining >= 0.6 {
        CAUTION
    } else {
        SHORT
    }
}

/// One tire's text and colors.
#[derive(Debug, Clone, PartialEq)]
struct Corner {
    temps: [FixedText; ZONES],
    temp_colors: [u32; ZONES],
    wear: [FixedText; ZONES],
    wear_colors: [u32; ZONES],
    distance: FixedText,
}

impl Corner {
    fn new() -> Self {
        Self {
            temps: std::array::from_fn(|_| FixedText::new(6)),
            temp_colors: [0; ZONES],
            wear: std::array::from_fn(|_| FixedText::new(6)),
            wear_colors: [0; ZONES],
            distance: FixedText::new(10),
        }
    }

    fn fill(&mut self, snap: &Snapshot, vars: &CornerVars) {
        for z in 0..ZONES {
            let temp = snap
                .f32(vars.carcass[z], 0)
                .filter(|t| t.is_finite() && *t > -50.0);
            write_or_dash(&mut self.temps[z], temp, |out, t| {
                let _ = write!(out, "{t:.0}");
            });
            self.temp_colors[z] = temp.map_or(DIM, temperature_color);
            // Zero tread is what an unknown car reports too; a real tire is never worn to nothing.
            let wear = snap
                .f32(vars.wear[z], 0)
                .filter(|w| w.is_finite() && *w > 0.0);
            write_or_dash(&mut self.wear[z], wear, |out, w| {
                let _ = write!(out, "{:.0}", (w * 100.0).min(100.0));
            });
            self.wear_colors[z] = wear.map_or(DIM, wear_color);
        }
        let meters = snap
            .f32(vars.odometer, 0)
            .filter(|m| m.is_finite() && *m >= 0.0);
        write_or_dash(&mut self.distance, meters, |out, m| {
            let _ = write!(out, "{:.1}", m / 1000.0);
        });
    }

    /// Corner `c` at a replayed moment: surface temperatures, tread, and, in place of the distance, hot pressure.
    fn fill_replay(&mut self, sample: &TireSample, c: usize) {
        for z in 0..ZONES {
            let temp = sample.temp(c, z);
            write_or_dash(&mut self.temps[z], temp, |out, t| {
                let _ = write!(out, "{t:.0}");
            });
            self.temp_colors[z] = temp.map_or(DIM, temperature_color);
            let wear = sample.wear(c, z).filter(|w| *w > 0.0);
            write_or_dash(&mut self.wear[z], wear, |out, w| {
                let _ = write!(out, "{:.0}", (w * 100.0).min(100.0));
            });
            self.wear_colors[z] = wear.map_or(DIM, wear_color);
        }
        write_or_dash(&mut self.distance, sample.pressure(c), |out, kpa| {
            let _ = write!(out, "{kpa:.0}");
        });
    }
}

fn write_or_dash(out: &mut FixedText, value: Option<f32>, write: impl FnOnce(&mut FixedText, f32)) {
    out.clear();
    match value {
        Some(v) => write(out, v),
        None => out.push("--"),
    }
}

/// Where the values come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Live telemetry, or a capture during a replay: wear and carcass temperatures from the last stop.
    Live,
    /// A replayed moment from the sim's telemetry files: surface temperatures, tread, and hot pressure.
    Replay,
    /// A replay with neither a capture nor a telemetry file: nothing to show.
    Blank,
}

/// Everything the widget shows.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    mode: Mode,
    corners: [Corner; CORNERS],
    compound: FixedText,
    compound_color: u32,
    /// `used/limit` when dry sets are limited, else empty.
    sets: FixedText,
    /// The lap the pit values were taken on, or empty while they are undated.
    taken: FixedText,
    dated: bool,
}

impl Shown {
    fn new() -> Self {
        Self {
            mode: Mode::Live,
            corners: std::array::from_fn(|_| Corner::new()),
            compound: FixedText::new(16),
            compound_color: 0,
            sets: FixedText::new(8),
            taken: FixedText::new(8),
            dated: false,
        }
    }
}

/// Bits of [`Shown`] regions for partial redraws: the header, then each corner.
const HEADER: u8 = 1;

fn changed_regions(a: &Shown, b: &Shown) -> u8 {
    if a.mode != b.mode {
        return 0xFF;
    }
    let mut mask = 0;
    if a.compound != b.compound
        || a.compound_color != b.compound_color
        || a.sets != b.sets
        || a.taken != b.taken
        || a.dated != b.dated
    {
        // Dating dims or lights every corner.
        mask |= if a.dated == b.dated { HEADER } else { 0xFF };
    }
    for c in 0..CORNERS {
        if a.corners[c] != b.corners[c] {
            mask |= 2 << c;
        }
    }
    mask
}

/// The tire widget.
#[derive(Debug)]
pub struct TiresWidget {
    width: u32,
    height: u32,
    opacity: f32,
    watch: PitWatch,
    shown: Shown,
    next: Shown,
}

impl TiresWidget {
    pub fn new(config: &WidgetConfig) -> Self {
        Self {
            width: config.width.unwrap_or(220).max(MIN_SIZE.0),
            height: config.height.unwrap_or(170).max(MIN_SIZE.1),
            opacity: config.opacity,
            watch: PitWatch::default(),
            shown: Shown::new(),
            next: Shown::new(),
        }
    }

    /// Fills the next state from the view: live values, or a replayed moment's when the sim's telemetry files have
    /// it.
    fn fill(&mut self, view: &mut View, now: Instant) {
        let display = view.display();
        let at = replay_moment(view, display);
        if let Some(at) = at {
            view.want_tires(at, now);
        }
        // During a live replay only this widget shows, so it follows the camera: the files hold the player's
        // car only. A capture replays the player's whole view, as the other widgets show it.
        let sample = at
            .and_then(|at| view.tire_sample(at))
            .copied()
            .filter(|_| display != Display::Replay || camera_on_player(view));
        // A live replay keeps the driving connection, but its values are not the car's: they must not date the
        // last stop.
        if display != Display::Replay {
            self.watch_stop(view);
        }
        self.fill_live(view);
        let next = &mut self.next;
        next.mode = match (sample, display) {
            (Some(sample), _) => {
                for (c, corner) in next.corners.iter_mut().enumerate() {
                    corner.fill_replay(&sample, c);
                }
                next.dated = true;
                next.taken.clear();
                Mode::Replay
            }
            (None, Display::Replay) => Mode::Blank,
            (None, _) => Mode::Live,
        };
    }

    /// Feeds the pit stall values to the watch that dates them.
    fn watch_stop(&mut self, view: &View) {
        let (snap, vars) = (view.snapshot(), view.vars());
        self.watch
            .observe(&pit_values(snap, &corner_vars(vars)), snap.i32(vars.lap, 0));
    }

    /// Fills the next state from the snapshot's live values.
    fn fill_live(&mut self, view: &View) {
        let (snap, vars) = (view.snapshot(), view.vars());
        let corners = corner_vars(vars);
        let next = &mut self.next;
        for (corner, ids) in next.corners.iter_mut().zip(&corners) {
            corner.fill(snap, ids);
        }
        next.dated = self.watch.dated();
        next.taken.clear();
        if let Some(lap) = self.watch.taken {
            let _ = write!(next.taken, "{lap}");
        }
        let compound = snap
            .i32(vars.tire_compound, 0)
            .filter(|&t| t >= 0)
            .and_then(|t| {
                view.session()?
                    .driver_info
                    .tires
                    .iter()
                    .find(|tire| tire.index == Some(i64::from(t)))?
                    .compound
                    .as_deref()
            });
        next.compound.set(compound.unwrap_or(""));
        next.compound_color = compound.map_or(0, compound_color);
        next.sets.clear();
        if let (Some(limit), Some(used)) = (
            snap.i32(vars.tire_set_limit, 0).filter(|&l| l > 0),
            snap.i32(vars.tire_sets_used, 0),
        ) {
            let _ = write!(next.sets, "{used}/{limit}");
        }
    }

    fn header_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width as f32, HEADER_HEIGHT)
    }

    /// The block of corner `c`: left front, right front, left rear, right rear.
    fn corner_rect(&self, c: usize) -> Rect {
        let (w, h) = (self.width as f32, self.height as f32);
        let block_w = (w - 2.0 * PAD - GAP) / 2.0;
        let block_h = (h - HEADER_HEIGHT - PAD - GAP) / 2.0;
        let (col, row) = ((c % 2) as f32, (c / 2) as f32);
        Rect::new(
            PAD + col * (block_w + GAP),
            HEADER_HEIGHT + row * (block_h + GAP),
            block_w,
            block_h,
        )
    }

    fn draw_corner(&self, c: usize, canvas: &mut Canvas<'_>) {
        let corner = &self.shown.corners[c];
        let block = self.corner_rect(c);
        canvas.fill_background(block, Color::rgb(BLOCK, self.opacity));
        let cell_w = block.w / ZONES as f32;
        let row_h = block.h / 3.0;
        let replay = self.shown.mode == Mode::Replay;
        // Undated values may be from the garage or a stop before the overlay started.
        let strength = if self.shown.dated { 0.85 } else { 0.35 };
        let ink = if self.shown.dated { BLOCK } else { WHITE };
        for z in 0..ZONES {
            let cell = Rect::new(block.x + z as f32 * cell_w, block.y, cell_w, row_h);
            let swatch = Rect::new(cell.x + 1.0, cell.y + 1.0, cell.w - 2.0, cell.h - 2.0);
            canvas.fill(swatch, Color::rgb(corner.temp_colors[z], strength));
            canvas.text(
                corner.temps[z].as_str(),
                cell,
                14.0,
                Align::Center,
                Color::rgb(ink, 1.0),
            );
            let cell = Rect::new(cell.x, block.y + row_h, cell_w, row_h);
            let alpha = if self.shown.dated { 1.0 } else { 0.55 };
            canvas.text(
                corner.wear[z].as_str(),
                cell,
                14.0,
                Align::Center,
                Color::rgb(corner.wear_colors[z], alpha),
            );
        }
        let distance = Rect::new(block.x, block.y + 2.0 * row_h, block.w * 0.6, row_h);
        canvas.text(
            corner.distance.as_str(),
            distance,
            12.0,
            Align::Right,
            Color::rgb(WHITE, 1.0),
        );
        let unit = Rect::new(
            distance.x + distance.w + 3.0,
            distance.y,
            block.w * 0.4 - 3.0,
            row_h,
        );
        let unit_name = if replay { "kPa" } else { "km" };
        canvas.text(unit_name, unit, 12.0, Align::Left, Color::rgb(DIM, 1.0));
    }

    fn draw_header(&self, canvas: &mut Canvas<'_>) {
        let s = &self.shown;
        let w = self.width as f32;
        let dim = Color::rgb(DIM, 1.0);
        let mid = HEADER_HEIGHT / 2.0;
        let mut x = PAD;
        if !s.compound.as_str().is_empty() {
            canvas.circle((x + 5.0, mid), 4.5, 2.5, Color::rgb(s.compound_color, 1.0));
            x += 14.0;
            canvas.text(
                s.compound.as_str(),
                Rect::new(x, 0.0, w * 0.32, HEADER_HEIGHT),
                12.0,
                Align::Left,
                Color::rgb(WHITE, 1.0),
            );
        }
        if !s.sets.as_str().is_empty() {
            let label = Rect::new(w * 0.40, 0.0, 30.0, HEADER_HEIGHT);
            canvas.text("Sets", label, 12.0, Align::Left, dim);
            canvas.text(
                s.sets.as_str(),
                Rect::new(label.x + label.w, 0.0, w * 0.2, HEADER_HEIGHT),
                12.0,
                Align::Left,
                Color::rgb(WHITE, 1.0),
            );
        }
        // Where the wear and temperatures are from: the replayed moment, a stop on a known lap, or an unknown
        // last stop.
        let right = Rect::new(w * 0.62, 0.0, w * 0.38 - PAD, HEADER_HEIGHT);
        if s.mode == Mode::Replay {
            canvas.text(
                "Replay",
                right,
                12.0,
                Align::Right,
                Color::rgb(CAUTION, 1.0),
            );
        } else if s.dated {
            let number = Rect::new(right.x + right.w - 26.0, 0.0, 26.0, HEADER_HEIGHT);
            canvas.text(
                s.taken.as_str(),
                number,
                12.0,
                Align::Right,
                Color::rgb(WHITE, 1.0),
            );
            canvas.text(
                "Pit lap",
                Rect::new(right.x, 0.0, right.w - 29.0, HEADER_HEIGHT),
                12.0,
                Align::Right,
                dim,
            );
        } else {
            canvas.text("Last pit", right, 12.0, Align::Right, dim);
        }
    }
}

/// Header height and the margins around the tire blocks, in pixels.
const HEADER_HEIGHT: f32 = 22.0;
const PAD: f32 = 8.0;
const GAP: f32 = 6.0;
/// Three temperatures across each tire block, and its three lines.
const MIN_SIZE: (u32, u32) = (200, 150);

const WHITE: u32 = 0xF0_F2F5;
const DIM: u32 = 0xA0_A6AE;
const CAUTION: u32 = 0xFF_D24D;
const SHORT: u32 = 0xFF_6B5E;
const BACKGROUND: u32 = 0x10_1418;
const BLOCK: u32 = 0x1C_2229;

/// Compound colors by `TireCompoundType`, as in the standings.
fn compound_color(compound: &str) -> u32 {
    super::standings::tire_color(compound)
}

impl Register for TiresWidget {
    type Vars = ();
    type Options = NoOptions;

    fn build(cx: &Build<'_>, _: ()) -> Self {
        Self::new(cx.config)
    }
}

impl Widget for TiresWidget {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn min_size(&self) -> (u32, u32) {
        MIN_SIZE
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(MIN_SIZE.0);
        self.height = height.max(MIN_SIZE.1);
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(200)
    }

    fn update(&mut self, view: &mut View, now: Instant, redraw: &mut [Redraw; MAX_LAYERS]) {
        self.fill(view, now);
        let mask = changed_regions(&self.shown, &self.next);
        if mask == 0 {
            return;
        }
        std::mem::swap(&mut self.shown, &mut self.next);
        redraw[0] = match mask {
            HEADER => Redraw::Area(self.header_rect()),
            m if m.count_ones() == 1 => {
                let c = (m >> 1).trailing_zeros() as usize;
                Redraw::Area(self.corner_rect(c))
            }
            _ => Redraw::All,
        };
    }

    fn draw(&self, _layer: usize, canvas: &mut Canvas<'_>) {
        if self.shown.mode == Mode::Blank {
            return;
        }
        let (w, h) = (self.width as f32, self.height as f32);
        canvas.fill_background(
            Rect::new(0.0, 0.0, w, h),
            Color::rgb(BACKGROUND, self.opacity),
        );
        self.draw_header(canvas);
        for c in 0..CORNERS {
            self.draw_corner(c, canvas);
        }
    }

    fn visible(&self, display: Display) -> bool {
        matches!(display, Display::Driving | Display::Replay)
    }

    fn reset(&mut self) {
        self.watch = PitWatch::default();
    }
}

/// The moment a replay shows: a capture's own clock while it plays, else the sim's replay position. `None`
/// outside replays.
fn replay_moment(view: &View, display: Display) -> Option<Moment> {
    let (snap, vars) = (view.snapshot(), view.vars());
    let (num, time) = match view.source() {
        Some(Source::Capture { .. }) => (vars.session_num, vars.session_time),
        _ if display == Display::Replay => (vars.replay_session_num, vars.replay_session_time),
        _ => return None,
    };
    let at = Moment::new(snap.i32(num, 0)?, snap.f64(time, 0)?);
    (at.session >= 0 && at.time.is_finite()).then_some(at)
}

/// The replay camera shows the player's car, or the sim does not say.
fn camera_on_player(view: &View) -> bool {
    let (snap, vars) = (view.snapshot(), view.vars());
    match (
        snap.i32(vars.cam_car_idx, 0),
        snap.i32(vars.player_car_idx, 0),
    ) {
        (Some(camera), Some(player)) => camera == player,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(wear: f32, temp: f32) -> PitValues {
        let mut v = [wear; CORNERS * ZONES * 2];
        v[CORNERS * ZONES..].fill(temp);
        v
    }

    #[test]
    fn pit_values_are_dated_once_they_change() {
        let mut watch = PitWatch::default();
        watch.observe(&values(1.0, 66.9), Some(1));
        watch.observe(&values(1.0, 66.9), Some(2));
        assert!(!watch.dated(), "unchanged since the connection");
        watch.observe(&values(0.99, 78.4), Some(3));
        assert!(watch.dated());
        assert_eq!(watch.taken, Some(3));
        watch.observe(&values(0.99, 78.4), Some(7));
        assert_eq!(watch.taken, Some(3), "the stop's lap stays");
        // Unavailable values stay unavailable without dating anything.
        let mut watch = PitWatch::default();
        watch.observe(&[f32::NAN; CORNERS * ZONES * 2], Some(1));
        watch.observe(&[f32::NAN; CORNERS * ZONES * 2], Some(2));
        assert!(!watch.dated());
    }

    #[test]
    fn temperatures_run_from_blue_to_red() {
        assert_eq!(temperature_color(20.0), 0x4F_A3FF);
        assert_eq!(temperature_color(75.0), 0x3D_D68C);
        assert_eq!(temperature_color(130.0), 0xFF_4D4D);
        assert_eq!(temperature_color(f32::NAN), 0x4F_A3FF);
        let between = temperature_color(85.0);
        assert!(
            between != 0x3D_D68C && between != 0xFF_D24D,
            "{between:06x}"
        );
    }

    #[test]
    fn compounds_are_named_in_any_case() {
        assert_eq!(compound_color("Soft"), 0xFF_4D4D);
        assert_eq!(compound_color("WET"), 0x4F_A3FF);
        assert_eq!(compound_color("Intermediate"), 0x3D_D68C);
        assert_eq!(compound_color("Hard"), 0xE8_EAED);
        assert_eq!(compound_color(""), 0xE8_EAED);
    }

    #[test]
    fn wear_turns_yellow_then_red() {
        assert_eq!(wear_color(0.97), WHITE);
        assert_eq!(wear_color(0.7), CAUTION);
        assert_eq!(wear_color(0.4), SHORT);
    }

    #[test]
    fn one_changed_corner_redraws_only_that_block() {
        let a = Shown::new();
        let mut b = Shown::new();
        assert_eq!(changed_regions(&a, &b), 0);
        b.corners[2].distance.set("7.4");
        assert_eq!(changed_regions(&a, &b), 2 << 2);
        b.sets.set("1/4");
        assert_eq!(changed_regions(&a, &b), HEADER | 2 << 2);
        let mut c = Shown::new();
        c.dated = true;
        assert_eq!(
            changed_regions(&a, &c),
            0xFF,
            "dating restyles every corner"
        );
    }
}

#[cfg(all(test, windows))]
mod pipeline_tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use pitot_irsdk::layout::{VarHeader, VarType};
    use pitot_irsdk::live::{ReaderConfig, SdkNames};
    use pitot_irsdk::publish::{Publisher, PublisherConfig};

    use super::*;
    use crate::pipeline::Pipeline;

    const SESSION: &str = "---
DriverInfo:
 DriverCarIdx: 0
 DriverTires:
 - TireIndex: 0
   TireCompoundType: \"Hard\"
 - TireIndex: 1
   TireCompoundType: \"Wet\"
...
";
    /// Four ints (lap, compound, set limit, sets used), then per corner wear L/M/R, carcass L/M/R, and
    /// distance, then the lap distance of four cars.
    const INTS: usize = 16;
    const ROW: usize = INTS + CORNERS * 7 * 4 + 4 * 4;

    fn publisher(names: &SdkNames) -> Publisher {
        let mut vars = vec![
            VarHeader::new(VarType::Int, 0, 1, "Lap").expect("valid"),
            VarHeader::new(VarType::Int, 4, 1, "PlayerTireCompound").expect("valid"),
            VarHeader::new(VarType::Int, 8, 1, "PlayerCarDryTireSetLimit").expect("valid"),
            VarHeader::new(VarType::Int, 12, 1, "TireSetsUsed").expect("valid"),
        ];
        let mut at = INTS as i32;
        for corner in ["LF", "RF", "LR", "RR"] {
            for name in [
                "wearL", "wearM", "wearR", "tempCL", "tempCM", "tempCR", "odometer",
            ] {
                vars.push(
                    VarHeader::new(VarType::Float, at, 1, &format!("{corner}{name}"))
                        .expect("valid"),
                );
                at += 4;
            }
        }
        vars.push(VarHeader::new(VarType::Float, at, 4, "CarIdxLapDistPct").expect("valid"));
        let config = PublisherConfig {
            vars,
            buf_len: ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 4096,
        };
        let mut publisher = Publisher::create(names, &config).expect("creates");
        publisher.set_connected(true).expect("connects");
        publisher
            .set_session_info(SESSION.as_bytes())
            .expect("fits");
        publisher
    }

    /// A row with every corner at `wear` and `temp` and 1.2 km run, except the left front as given.
    fn row(lap: i32, wear: f32, temp: f32, left_front: [f32; 7]) -> Vec<u8> {
        let mut row = vec![0u8; ROW];
        for (i, v) in [lap, 0, 4, 1].iter().enumerate() {
            row[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        for c in 0..CORNERS {
            let values = if c == 0 {
                left_front
            } else {
                [wear, wear, wear, temp, temp, temp, 1200.0]
            };
            for (k, v) in values.iter().enumerate() {
                let at = INTS + (c * 7 + k) * 4;
                row[at..at + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        row
    }

    /// Publishes `row` until the view has it with the session info, then fills the widget.
    fn show(publisher: &mut Publisher, view: &mut View, row: &[u8], widget: &mut TiresWidget) {
        let lap = i32::from_le_bytes(row[0..4].try_into().expect("4 bytes"));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            publisher.publish(row).expect("publishes");
            thread::sleep(Duration::from_millis(5));
            view.poll();
            if view.session().is_some() && view.snapshot().i32(view.vars().lap, 0) == Some(lap) {
                break;
            }
            assert!(Instant::now() < deadline, "the row arrives");
        }
        widget.watch_stop(view);
        widget.fill_live(view);
    }

    #[test]
    fn shows_the_last_stop_and_dates_it_once_it_changes() {
        let pid = std::process::id();
        let names = SdkNames::new(
            &format!("Local\\PitotTiresMap-{pid}"),
            &format!("Local\\PitotTiresEvent-{pid}"),
        );
        let mut publisher = publisher(&names);
        let config = ReaderConfig {
            retry_interval: Duration::from_millis(5),
            idle_timeout: Duration::from_millis(50),
        };
        let (_pipeline, mut view) =
            Pipeline::start(names, config, Vec::new(), None).expect("starts");
        let mut widget = TiresWidget::new(crate::config::Config::default().widget("tires").expect("tires"));

        let unchanged = [1.0, 1.0, 1.0, 66.9, 66.9, 66.9, 0.0];
        let first = row(1, 1.0, 66.9, unchanged);
        show(&mut publisher, &mut view, &first, &mut widget);
        let s = &widget.next;
        assert_eq!(s.corners[0].temps[1].as_str(), "67");
        assert_eq!(s.corners[0].wear[0].as_str(), "100");
        assert_eq!(s.corners[0].distance.as_str(), "0.0");
        assert_eq!(s.corners[3].distance.as_str(), "1.2");
        assert_eq!(s.compound.as_str(), "Hard");
        assert_eq!(s.compound_color, compound_color("Hard"));
        assert_eq!(s.sets.as_str(), "1/4");
        assert!(!s.dated, "values from before the connection");
        assert_eq!(s.taken.as_str(), "");

        // A stop on lap 3: the sim refreshes wear and carcass temperatures.
        let stopped = [0.984, 0.967, 0.962, 73.3, 83.0, 89.0, 7400.0];
        let after = row(3, 0.99, 80.0, stopped);
        show(&mut publisher, &mut view, &after, &mut widget);
        let s = &widget.next;
        let lf = &s.corners[0];
        let temps: Vec<&str> = lf.temps.iter().map(FixedText::as_str).collect();
        assert_eq!(temps, ["73", "83", "89"]);
        let wear: Vec<&str> = lf.wear.iter().map(FixedText::as_str).collect();
        assert_eq!(wear, ["98", "97", "96"]);
        assert_eq!(lf.distance.as_str(), "7.4");
        assert_eq!(lf.temp_colors[0], temperature_color(73.3));
        assert!(s.dated);
        assert_eq!(s.taken.as_str(), "3");
        assert_eq!(s.mode, Mode::Live);
    }

    /// A live session in a replay: replay position, camera, and the player's car.
    const REPLAY_ROW: usize = 32;
    const REPLAY_SESSION: &str = "---
WeekendInfo:
 TrackID: 5
 SubSessionID: 77
DriverInfo:
 DriverCarIdx: 0
...
";

    fn replay_row(time: f64, camera: i32) -> Vec<u8> {
        let mut row = vec![0u8; REPLAY_ROW];
        row[0..8].copy_from_slice(&time.to_le_bytes());
        row[8..12].copy_from_slice(&2i32.to_le_bytes());
        row[12] = 1;
        row[16..20].copy_from_slice(&camera.to_le_bytes());
        row
    }

    #[test]
    fn replays_show_surface_temperatures_from_the_sims_telemetry_files() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("pitot-tires-replay-{pid}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates the folder");
        let records = crate::pipeline::ibt_test::stint(2, 100.0, 10.0);
        std::fs::write(
            dir.join("stint.ibt"),
            crate::pipeline::ibt_test::ibt(77, 5, &records),
        )
        .expect("writes");

        let names = SdkNames::new(
            &format!("Local\\PitotTiresReplayMap-{pid}"),
            &format!("Local\\PitotTiresReplayEvent-{pid}"),
        );
        let config = PublisherConfig {
            vars: vec![
                VarHeader::new(VarType::Double, 0, 1, "ReplaySessionTime").expect("valid"),
                VarHeader::new(VarType::Int, 8, 1, "ReplaySessionNum").expect("valid"),
                VarHeader::new(VarType::Bool, 12, 1, "IsReplayPlaying").expect("valid"),
                VarHeader::new(VarType::Int, 16, 1, "CamCarIdx").expect("valid"),
                VarHeader::new(VarType::Int, 20, 1, "PlayerCarIdx").expect("valid"),
                VarHeader::new(VarType::Float, 24, 2, "CarIdxLapDistPct").expect("valid"),
            ],
            buf_len: REPLAY_ROW,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 4096,
        };
        let mut publisher = Publisher::create(&names, &config).expect("creates");
        publisher.set_connected(true).expect("connects");
        publisher
            .set_session_info(REPLAY_SESSION.as_bytes())
            .expect("fits");
        let reader = ReaderConfig {
            retry_interval: Duration::from_millis(5),
            idle_timeout: Duration::from_millis(50),
        };
        let (_pipeline, mut view) =
            Pipeline::start(names, reader, Vec::new(), None).expect("starts");
        view.set_telemetry_folders(vec![dir.clone()]);
        let mut widget = TiresWidget::new(crate::config::Config::default().widget("tires").expect("tires"));
        let mut redraw = [Redraw::Nothing; MAX_LAYERS];
        // The search needs the live session info first, which names the session.
        let deadline = Instant::now() + Duration::from_secs(5);
        while view.session().is_none() {
            assert!(Instant::now() < deadline, "session info");
            publisher.publish(&replay_row(105.0, 0)).expect("publishes");
            thread::sleep(Duration::from_millis(5));
            view.poll();
        }
        assert_eq!(view.display(), Display::Replay);
        assert!(widget.visible(Display::Replay));
        assert!(!widget.visible(Display::Idle));
        let mut show = |time: f64, camera: i32, want: Mode| -> Shown {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                publisher
                    .publish(&replay_row(time, camera))
                    .expect("publishes");
                thread::sleep(Duration::from_millis(5));
                view.poll();
                widget.update(&mut view, Instant::now(), &mut redraw);
                if widget.shown.mode == want {
                    return widget.shown.clone();
                }
                assert!(Instant::now() < deadline, "shows {want:?}");
            }
        };

        let s = show(105.0, 0, Mode::Replay);
        let lf = &s.corners[0];
        let temps: Vec<&str> = lf.temps.iter().map(FixedText::as_str).collect();
        assert_eq!(temps, ["85", "85", "85"], "85.0, 85.1, 85.2 °C");
        assert_eq!(lf.wear[0].as_str(), "95");
        assert_eq!(lf.distance.as_str(), "170", "hot pressure in kPa");
        assert_eq!(s.corners[3].distance.as_str(), "173");
        assert_eq!(s.corners[0].temp_colors[0], temperature_color(85.0));
        let s = show(109.0, 0, Mode::Replay);
        assert_eq!(s.corners[0].temps[0].as_str(), "89", "follows the replay");
        show(106.0, 1, Mode::Blank);
        show(150.0, 0, Mode::Blank);
        let _ = std::fs::remove_dir_all(dir);
    }
}
