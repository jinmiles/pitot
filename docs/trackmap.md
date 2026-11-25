# Track map widget

Every car as a dot at its lap distance (`CarIdxLapDistPct`), around a circle (`shape = "circle"`, the default) or
along the track's outline recorded from a clean lap (`shape = "outline"`). Dots are colored by class
(`CarClassColor`); the player's is amber with a white ring and drawn on top, and cars on pit road are hollow rings
in their class color. The pace car, spectators, and cars out of the world (`CarIdxTrackSurface` -1) are left out.
Updates at 10 Hz by default.

```toml
[widgets.trackmap]
shape = "circle"   # or "outline"
```

## Circle

Start/finish at 12 o'clock, marked by a white tick across the ring. Cars go clockwise in driving order, at
`CarIdxLapDistPct` × 360°. Cars on pit road sit on a smaller ring inside, labeled `PIT` at 6 o'clock, as hollow
rings in their class color. A short tick marks each sector start from the session's `SplitTimeInfo` (up to 16). The
circle needs no lap, so it shows from the first moment of every session; nothing of the outline runs in this mode (no
integration, no `.trk` files read or written).

The ring is a lap of distance, not of time: cars equally far apart on the ring are equally far apart in lap
distance, which is not the same gap in seconds where the track is slow or fast.

## Outline

The track's outline, with a white tick across the road at the start line.

### Where the outline comes from

The live telemetry has no position (`Lat` and `Lon` are only in `.ibt` files), so the overlay records the outline
from the player's own driving the first time a track is driven, and keeps it:

1. Each update, the car's velocity in its own frame (`VelocityX` forward, `VelocityY` to the left) is turned by
   its heading (`YawNorth`, clockwise from north) into meters per second east and north, and integrated with the
   time between the two updates' ticks (trapezoid rule).
2. A lap starts at a crossing of the line and is sampled at 1,000 evenly spaced lap distances (`LapDistPct`),
   interpolating between updates.
3. The lap counts only if it is clean from line to line: on track (`PlayerTrackSurface` on track, `IsOnTrack`),
   never on pit road, without a pause longer than a second between updates, and without a jump in lap distance
   (a tow or a reset). Anything else waits for the next crossing.
4. At the closing crossing, what is left between the end and the start (the integration's drift) is spread over
   the lap, in proportion to lap distance. The lap is rejected if that drift exceeds 5% of the lap's length, or if
   the length differs from the session's `TrackLength` by more than 15% (the car's line is not the track's
   center line, but a wrong integration is far off).
5. The outline is saved as `%APPDATA%\Pitot\tracks\<TrackID>.trk` (the sim's `TrackID` differs per track
   configuration), on its own thread, through a temporary file. The next session on the track loads it at once.

Until a track has an outline, the widget says so, and while the mapping lap runs it shows how much of it is done.
Deleting the file maps the track again.

The file is a 16-byte header (`PTRK`, version 1, point count, a reserved word) and the 1,000 points as pairs of
little-endian `f32`, meters east and north of the line. A file that is not exactly that is ignored and the track is
mapped again.

#### Accuracy

Checked against the latitude and longitude in two sim `.ibt` files, integrating every sixth tick as the widget
does at 10 Hz (the same lap's `Lat`/`Lon`, interpolated to the outline's lap distances, in local meters on the
WGS84 ellipsoid):

| Recording | Laps | Error (RMS) | Error (max) |
|---|---|---|---|
| GT3 at Sebring (5.8 km) | 4 | 0.38 to 1.64 m | 0.66 to 2.89 m |
| Formula car at Monza (5.8 km) | 1 | 0.80 m | 1.31 m |

Most of each error is one offset for the whole lap (up to 1.4 m); the shape itself is within 0.2 to 0.8 m RMS. At
every tick (60 Hz) the errors are the same within 0.05 m, so the widget integrates at its own rate instead of on
the data thread. At the default size, one pixel is about 3 m at these tracks.

### Orientation

North is up, unless turning the map fills the widget better: of the turns in 5 degree steps, the one that draws the
track largest is used when it is more than 3% larger than north up. The outline is fitted once per track, with
14 pixels around it.

### Limits

- The pit lane is not drawn. Cars on pit road are placed by their lap distance on the track, as hollow rings.
- The outline follows the player's line on the mapping lap, not the track's center line.
- A short excursion off track between two updates (less than 0.1 s) is not seen and stays in the outline.
- With a recording of the player's car only (an `.ibt` replayed by `pitot-bench`), only the player's dot shows.

## Pit exit

An amber ring (the player's color) marks where the player would rejoin after a stop made now: the player's lap distance moved back by
the stop's expected loss, converted with the player's last lap time (else the session's estimated lap time). The
loss is the pit lane loss measured at this track plus the standing time for the fuel the stop adds (see
[pit.md](pit.md#pit-lane-loss)), from the pit tracker the map shares with the pit widget:

- in a race, once the fuel use per lap is known: the fuel left to finish (laps to go times fuel per lap, less the
  fuel in the tank), within the room left in the tank;
- otherwise, filling the tank.

The ring shows only once the pit lane loss is measured at the track (until then the pit widget shows `--.-`), and
not while the player is on pit road or after the checkered flag. The conversion uses the lap's average speed:
where the field is slower or faster than average around the rejoin point, the ring is off by that difference.
Where other cars rejoin after their next stops is in the pit widget, as an order and a gap at each car's own
future stop; it is not drawn, since it is not a place on the current map.

## Drawing

Two layers, drawn bottom to top in the widget's window: the background with the circle or outline, and the dots.
The window is drawn again when the track, its outline, or (for the circle) the sector starts change (and while
mapping, when the progress changes by a percent), or when a dot moved by half a pixel. The outline is one path with
round joins; the circle is two ring strokes and the ticks. Updates do not allocate; the per-car buffers are sized
once.

## Cost

Cost numbers on this page were measured with the overlay's earlier Direct2D renderer, before the widgets were drawn with GPUI (#183); they are measured again at the next release.

The circle, against the outline it replaced as the default: lab PC (Parsec virtual display, release builds), the
public AI race capture at 1x, the four reference widgets with the pit widget and the map (the outline with track
18 saved, against the circle), a learned lane loss so the pit exit shows, `pitot-bench collect` for 60 s after a
12 s warm-up, alternating: overlay CPU 32.62 and 27.79 ms/s with the outline, 27.73 and 26.61 ms/s with the circle;
private bytes at most 21.8 and 19.0 MB against 21.9 and 18.1 MB. No cost shows above the run-to-run spread.

The outline, when it was added: lab PC (Parsec virtual display, release build), the public AI race capture
replayed at 1x with the track's outline saved, `pitot-bench collect` for 60 s after a 12 s warm-up, the four reference widgets with the map off and on,
two runs each, alternating: overlay CPU 27.20 and 24.09 ms/s without the map, 29.57 and 28.79 ms/s with it (+2.4
and +4.7 ms/s); private bytes at most 20.2 and 17.4 MB without, 22.1 and 19.8 MB with (+1.9 and +2.4 MB, of which
the two 440×330 surfaces are about 1.2 MB). Most of the CPU goes to drawing the whole dots layer again at 10 Hz.
Moving each dot as its own composition visual, without drawing, is a later optimization. The release binary grows
by 31 KB.
