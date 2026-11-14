//! Track outlines recorded from the player's own driving, and their files.
//!
//! The live telemetry has no position, only the car's velocity in its own frame (`VelocityX` forward, `VelocityY`
//! to the left) and its heading (`YawNorth`, clockwise from north). Turning the velocity by the heading and
//! integrating it gives the car's path in meters east and north. One clean lap, from the line to the line, sampled
//! at evenly spaced lap distances, is the outline. What is left when the loop closes (the integration's drift) is
//! spread over the lap, more toward its end.
//!
//! This file depends only on `std`, so local tools can check it against the positions in `.ibt` files.

use std::path::Path;
use std::{fs, io};

/// Points per outline, evenly spaced by lap distance from the start line.
pub const POINTS: usize = 1000;
/// Telemetry ticks per second.
const TICK_RATE: f64 = 60.0;
/// A longer pause between observations restarts the lap.
const MAX_GAP_S: f64 = 1.0;
/// Most lap distance between two observations. More is a tow or a reset.
const MAX_STEP: f64 = 0.15;
/// The outline's length may differ this much from the track's (the car's line is not the center line).
const LENGTH_TOLERANCE: f64 = 0.15;
/// Largest drift accepted when the loop closes, as a share of the lap's length.
const MAX_CLOSING: f64 = 0.05;

/// One observation of the player's car.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    /// The sim's tick count.
    pub tick: i32,
    /// `VelocityX` and `VelocityY`, m/s.
    pub forward: f32,
    pub left: f32,
    /// `YawNorth`, radians clockwise from north.
    pub heading: f32,
    /// `LapDistPct`.
    pub pct: f32,
    /// On track and in the car: in the world, not on pit road, not off track.
    pub clean: bool,
}

/// How far the mapping lap is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    /// Waiting for a clean crossing of the line.
    Waiting,
    /// Share of the lap recorded so far.
    Mapping(f32),
    /// A clean lap closed; [`Builder::outline_into`] has it.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Last {
    tick: i32,
    pct: f32,
    velocity: [f64; 2],
    position: [f64; 2],
}

/// Integrates observations into an outline. Allocates only in [`Builder::new`].
#[derive(Debug, Clone, PartialEq)]
pub struct Builder {
    bins: Vec<[f64; 2]>,
    filled: usize,
    recording: bool,
    done: bool,
    last: Option<Last>,
    /// The track's length in meters, when the session says.
    expected_length: Option<f64>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

/// Velocity in meters per second east and north.
fn world_velocity(forward: f32, left: f32, heading: f32) -> [f64; 2] {
    let (sin, cos) = f64::from(heading).sin_cos();
    let (f, l) = (f64::from(forward), f64::from(left));
    // Forward is (sin, cos) east and north; left is forward turned a quarter counterclockwise, (-cos, sin).
    [f * sin - l * cos, f * cos + l * sin]
}

fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

impl Builder {
    pub fn new() -> Self {
        Self {
            bins: vec![[0.0; 2]; POINTS],
            filled: 0,
            recording: false,
            done: false,
            last: None,
            expected_length: None,
        }
    }

    /// The track's length, to reject laps whose integration went wrong.
    pub fn set_expected_length(&mut self, meters: Option<f64>) {
        self.expected_length = meters.filter(|m| m.is_finite() && *m > 100.0);
    }

    /// Starts over: the next clean crossing of the line starts a lap.
    pub fn restart(&mut self) {
        self.recording = false;
        self.filled = 0;
        self.done = false;
        self.last = None;
    }

    fn abandon(&mut self) {
        self.recording = false;
        self.filled = 0;
    }

    pub fn progress(&self) -> Progress {
        if self.done {
            Progress::Done
        } else if self.recording {
            Progress::Mapping(self.filled as f32 / POINTS as f32)
        } else {
            Progress::Waiting
        }
    }

    /// Takes one observation. Observations may be a few ticks apart; the velocity is averaged between them.
    pub fn observe(&mut self, m: Motion) -> Progress {
        if self.done {
            return Progress::Done;
        }
        let usable = m.clean
            && (0.0..=1.0).contains(&m.pct)
            && m.forward.is_finite()
            && m.left.is_finite()
            && m.heading.is_finite();
        if !usable {
            self.abandon();
            self.last = None;
            return self.progress();
        }
        let velocity = world_velocity(m.forward, m.left, m.heading);
        let fresh = Last {
            tick: m.tick,
            pct: m.pct,
            velocity,
            position: [0.0; 2],
        };
        let Some(last) = self.last else {
            self.last = Some(fresh);
            return self.progress();
        };
        let dt = f64::from(m.tick.wrapping_sub(last.tick)) / TICK_RATE;
        if dt <= 0.0 {
            return self.progress();
        }
        let crossed = last.pct > 0.75 && m.pct < 0.25;
        // Lap distance of the previous observation, in the lap of this one.
        let from = f64::from(last.pct) - if crossed { 1.0 } else { 0.0 };
        let dp = f64::from(m.pct) - from;
        if dt > MAX_GAP_S || !(-1.0e-4..=MAX_STEP).contains(&dp) {
            self.abandon();
            self.last = Some(fresh);
            return self.progress();
        }
        let position = [
            last.position[0] + dt * (last.velocity[0] + velocity[0]) / 2.0,
            last.position[1] + dt * (last.velocity[1] + velocity[1]) / 2.0,
        ];
        self.last = Some(Last { position, ..fresh });
        if dp <= 0.0 {
            return self.progress();
        }
        if self.recording {
            // The lap's points up to this observation, or up to the line when it crossed.
            let until = if crossed { 1.0 } else { f64::from(m.pct) };
            self.fill(last.position, position, f64::from(last.pct), dp, until);
        }
        if crossed {
            let at_line = lerp(last.position, position, -from / dp);
            if self.recording && self.filled == POINTS && self.finish(at_line) {
                self.done = true;
                return Progress::Done;
            }
            self.recording = true;
            self.bins[0] = at_line;
            self.filled = 1;
            self.fill(last.position, position, from, dp, f64::from(m.pct));
        }
        self.progress()
    }

    /// Fills the points at lap distances up to `until` on the way from `a`, at lap distance `from`, to a point
    /// `dp` further.
    fn fill(&mut self, a: [f64; 2], b: [f64; 2], from: f64, dp: f64, until: f64) {
        while self.filled < POINTS {
            let target = self.filled as f64 / POINTS as f64;
            if target > until {
                break;
            }
            self.bins[self.filled] = lerp(a, b, (target - from) / dp);
            self.filled += 1;
        }
    }

    /// Closes the loop at `at_line`. False when the lap does not look like the track.
    fn finish(&mut self, at_line: [f64; 2]) -> bool {
        let start = self.bins[0];
        let closing = [at_line[0] - start[0], at_line[1] - start[1]];
        for (i, bin) in self.bins.iter_mut().enumerate() {
            let share = i as f64 / POINTS as f64;
            bin[0] -= start[0] + closing[0] * share;
            bin[1] -= start[1] + closing[1] * share;
        }
        let length: f64 = (0..POINTS)
            .map(|i| distance(self.bins[i], self.bins[(i + 1) % POINTS]))
            .sum();
        let drift = closing[0].hypot(closing[1]);
        let length_ok = self
            .expected_length
            .is_none_or(|expected| (length / expected - 1.0).abs() <= LENGTH_TOLERANCE);
        length_ok && length > 0.0 && drift <= MAX_CLOSING * length
    }

    /// Copies the finished outline, in meters east and north of the line. Does not allocate when `out` has room.
    pub fn outline_into(&self, out: &mut Vec<[f32; 2]>) {
        out.clear();
        if self.done {
            out.extend(self.bins.iter().map(|b| [b[0] as f32, b[1] as f32]));
        }
    }
}

const MAGIC: &[u8; 4] = b"PTRK";
const VERSION: u32 = 1;
const HEADER: usize = 16;
/// Farthest a point may be from the line, in meters. Larger is not a track.
const MAX_EXTENT: f32 = 50_000.0;

/// The file for an outline: a 16-byte header (magic, version, point count, reserved), then each point as two
/// little-endian `f32`, meters east and north of the line.
pub fn to_bytes(points: &[[f32; 2]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER + points.len() * 8);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&(points.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for p in points {
        bytes.extend_from_slice(&p[0].to_le_bytes());
        bytes.extend_from_slice(&p[1].to_le_bytes());
    }
    bytes
}

/// Reads an outline file's contents into `out`. False, with `out` empty, when the file is not a whole outline.
pub fn from_bytes(bytes: &[u8], out: &mut Vec<[f32; 2]>) -> bool {
    out.clear();
    let word =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    if bytes.len() != HEADER + POINTS * 8
        || &bytes[..4] != MAGIC
        || word(4) != VERSION
        || word(8) as usize != POINTS
    {
        return false;
    }
    let value =
        |at: usize| f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    for i in 0..POINTS {
        let at = HEADER + i * 8;
        let p = [value(at), value(at + 4)];
        if !p.iter().all(|v| v.is_finite() && v.abs() <= MAX_EXTENT) {
            out.clear();
            return false;
        }
        out.push(p);
    }
    true
}

/// Writes `bytes` to `path` through a temporary file, so a reader never sees half a file.
pub fn save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("tmp");
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A car driving a circle of `radius` counterclockwise seen from above, at `speed`, observed every `step`
    /// ticks, starting at the line (the circle's southernmost point) heading east.
    fn circle(radius: f64, speed: f64, step: i32, ticks: i32) -> impl Iterator<Item = Motion> {
        let lap = 2.0 * std::f64::consts::PI * radius / speed * TICK_RATE;
        (0..ticks).step_by(step as usize).map(move |tick| {
            let pct = (f64::from(tick) / lap).fract();
            let angle = pct * 2.0 * std::f64::consts::PI;
            // Heading east at the bottom, turning left: compass heading 90 degrees minus the angle travelled.
            let heading =
                (std::f64::consts::FRAC_PI_2 - angle).rem_euclid(2.0 * std::f64::consts::PI);
            Motion {
                tick,
                forward: speed as f32,
                left: 0.0,
                heading: heading as f32,
                pct: pct as f32,
                clean: true,
            }
        })
    }

    fn run(builder: &mut Builder, motions: impl Iterator<Item = Motion>) -> Progress {
        let mut progress = Progress::Waiting;
        for m in motions {
            progress = builder.observe(m);
            if progress == Progress::Done {
                break;
            }
        }
        progress
    }

    #[test]
    fn a_clean_lap_maps_the_circle() {
        let (radius, speed) = (300.0, 50.0);
        let mut builder = Builder::new();
        builder.set_expected_length(Some(2.0 * std::f64::consts::PI * radius));
        // Two and a half laps at 10 Hz: the first crossing starts the lap, the second closes it.
        assert_eq!(
            run(&mut builder, circle(radius, speed, 6, 6000)),
            Progress::Done
        );
        let mut points = Vec::new();
        builder.outline_into(&mut points);
        assert_eq!(points.len(), POINTS);
        assert_eq!(points[0], [0.0, 0.0], "starts at the line");
        // The circle's center is `radius` north of the line.
        let worst = points
            .iter()
            .map(|p| (f64::from(p[0]).hypot(f64::from(p[1]) - radius) - radius).abs())
            .fold(0.0, f64::max);
        assert!(worst < 1.0, "off the circle by {worst} m");
        // A quarter lap in, counterclockwise from the south, the car is at the circle's east.
        let quarter = points[POINTS / 4];
        assert!(
            (f64::from(quarter[0]) - radius).abs() < 1.0
                && (f64::from(quarter[1]) - radius).abs() < 1.0,
            "{quarter:?}"
        );
    }

    #[test]
    fn leaving_the_track_restarts_the_lap() {
        // A lap is about 2,262 ticks: the first crossing is near tick 2,262, the second near 4,524.
        let spoiled = 2_400..4_400;
        let mut builder = Builder::new();
        let mut progress = Progress::Waiting;
        let mut dropped = false;
        for mut m in circle(300.0, 50.0, 6, 8000) {
            if spoiled.contains(&m.tick) && (0.4..0.41).contains(&m.pct) {
                m.clean = false;
            }
            progress = builder.observe(m);
            if spoiled.contains(&m.tick) && m.pct > 0.45 {
                assert_eq!(progress, Progress::Waiting, "the lap was dropped");
                dropped = true;
            }
            if progress == Progress::Done {
                assert!(m.tick > 6_700, "mapped by the lap after the spoiled one");
                break;
            }
        }
        assert!(dropped);
        assert_eq!(progress, Progress::Done);
    }

    #[test]
    fn a_lap_far_from_the_track_length_is_rejected() {
        let mut builder = Builder::new();
        builder.set_expected_length(Some(5_000.0));
        assert_ne!(
            run(&mut builder, circle(300.0, 50.0, 6, 6000)),
            Progress::Done
        );
    }

    #[test]
    fn a_reset_restarts_the_lap() {
        let mut builder = Builder::new();
        let laps: Vec<Motion> = circle(300.0, 50.0, 6, 6000).collect();
        for m in &laps[..400] {
            builder.observe(*m);
        }
        assert!(matches!(builder.progress(), Progress::Mapping(_)));
        // A jump of a third of a lap (a tow) drops the lap.
        let mut jumped = laps[400];
        jumped.pct = (jumped.pct + 0.33).fract();
        builder.observe(jumped);
        assert_eq!(builder.progress(), Progress::Waiting);
    }

    #[test]
    fn files_round_trip_and_reject_damage() {
        let points: Vec<[f32; 2]> = (0..POINTS).map(|i| [i as f32, -(i as f32) / 2.0]).collect();
        let bytes = to_bytes(&points);
        let mut out = Vec::new();
        assert!(from_bytes(&bytes, &mut out));
        assert_eq!(out, points);
        assert!(!from_bytes(&bytes[..bytes.len() - 1], &mut out));
        assert!(out.is_empty());
        let mut bad = bytes.clone();
        bad[HEADER..HEADER + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(!from_bytes(&bad, &mut out));
        let mut other = bytes;
        other[0] = b'X';
        assert!(!from_bytes(&other, &mut out));
    }
}
