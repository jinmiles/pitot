# Track map widget

The track's outline with every car as a dot at its lap distance (`CarIdxLapDistPct`). Dots are colored by class
(`CarClassColor`); the player's is orange with a white ring and drawn on top, and cars on pit road are dimmed. The
pace car, spectators, and cars out of the world (`CarIdxTrackSurface` -1) are left out. A white tick across the
road marks the start line. Updates at 10 Hz by default.

## Where the outline comes from

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

### Accuracy

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

## Orientation

North is up, unless turning the map fills the widget better: of the turns in 5 degree steps, the one that draws the
track largest is used when it is more than 3% larger than north up. The outline is fitted once per track, with
14 pixels around it.

## Limits

- The pit lane is not drawn. Cars on pit road are placed by their lap distance on the track, dimmed.
- The outline follows the player's line on the mapping lap, not the track's center line.
- A short excursion off track between two updates (less than 0.1 s) is not seen and stays in the outline.
- With a recording of the player's car only (an `.ibt` replayed by `pitot-bench`), only the player's dot shows.

## Drawing

Two layers: the background and outline, drawn when the track or its outline changes (and while mapping, when the
progress changes by a percent), and the dots, drawn again only when a dot moved by half a pixel. The outline is one
Direct2D path with round joins. Updates do not allocate; the per-car buffers are sized once.

## Cost

Measured in the pull request that added the widget; see below.
