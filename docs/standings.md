# Standings widget

The session's order grouped by class, fastest class first (by `CarClassRelSpeed`), updated at 2 Hz by default.

## Where the values come from

Positions and lap times come from the session's results (`SessionInfo.Sessions[n].ResultsPositions`, `n` being the
current `SessionNum`), so laps driven before the player joined the session are included. Telemetry only knows laps
completed while the player is connected: in a home PC capture of an online practice session, the results had best
and last laps for all 14 cars that had driven, the telemetry for 8 of them.

| Value | Source |
|---|---|
| Class position | Results `ClassPosition` (0-based in the session info, shown from 1); without a result, `CarIdxClassPosition` |
| Best lap | The lower of results `FastestTime` and `CarIdxBestLapTime` |
| Last lap | `CarIdxLastLapTime` when the car has one (it is newer), else results `LastTime` |
| Laps | Results `LapsComplete`, else `CarIdxLapCompleted` |
| Gap and interval | In a race, `CarIdxF2Time` to the class leader and to the car ahead in the class; otherwise the difference in best laps |
| Tires | `CarIdxTireCompound`, named by the player's `DriverTires` list |
| Sector times | Timed by the overlay (below) |

## Layout

Each class starts with a header row; the class leaders (three) follow. In the player's class, the three cars on
each side of the player follow too, after a thin line when cars are left out between them.

Header: class name in the class color, strength of field, cars in the world over cars entered, and laps: the class
leader's laps over the session's laps, or `≈` an estimate from the time left at the leader's pace in a timed
session. The first header also shows the session type's letter (P, Q, R), air and track temperature
(`AirTemp`, `TrackTempCrew`), and the session time elapsed over its length (or `≈` an estimate for a lap-limited
session).

Car rows, left to right:

- class position, and in a race the places gained (green up arrow) or lost (red down arrow) since the start,
- car number and driver name,
- license class and safety rating (`D 2.83` as `D2.8`) in the license color,
- iRating, and in a race the estimated change if the race ended now,
- the maker's letter: the first letter of `CarScreenName` (BMW is B, Porsche is P), instead of a logo,
- tire compound as a colored ring (hard white, medium yellow, soft red, intermediate green, wet blue),
- gap to the class leader and interval to the car ahead (the leader shows the column names),
- last lap to the millisecond (`1:45.345`): underlined in green when it is the car's best lap, in purple when it
  is the class's best,
- per sector, the car's last time in that sector minus the player's (green when the car was slower, red when
  faster), as many sectors as fit.

Cars on pit road are dimmed and show `PIT` instead of the last lap; the player's row is highlighted. The pace car
and spectators are left out. Optional columns are dropped when the widget is too narrow: sectors first, then the
iRating change, interval, places gained, maker, and tires.

## Calculations

- **Strength of field**: `1600 / ln 2 * ln(n / sum(exp(-iRating / (1600 / ln 2))))` over the class.
- **Estimated iRating change**: the community's model of the sim's formula. For each car, the chance of beating
  every other car follows from the two ratings; the change is the cars beaten minus the cars expected to be beaten,
  less `(n / 2 - place) / 100`, times `200 / n`. It is an estimate, shown only in races.
- **Start position**: from `QualifyResultsInfo` (the grid) when the session info has it, else the class position
  first seen under green. `QualifyResultsInfo` positions are assumed 0-based like the results'; this needs a check
  against a race on the home PC.
- **Sector times**: the session info's `SplitTimeInfo` gives where sectors start. At each update the overlay reads
  every car's lap distance and interpolates, by distance, when it crossed a sector start since the previous update.
  A car on pit road, out of the world, unseen for 5 s, or skipping a sector restarts its timing. At the default
  2 Hz, a car moves about half a percent of a lap between updates, so the interpolation assumes a steady speed over
  that stretch.

## Drawing

Rows and text buffers are pooled and reused, so updates do not allocate. Only the rows that changed are drawn
again (the header's session clock alone changes every second).

## Cost

Lab PC (Parsec virtual display, four widgets, release build), the home PC capture of an online practice session
replayed at 1x, `pitot-bench collect` for 60 s after a 12 s warm-up, two runs each, alternating: overlay CPU 23.90
and 23.57 ms/s before this layout, 24.79 and 25.26 ms/s with it (+1.3 ms/s); private bytes 14.8 MiB in both. Most
of the difference is text with letters and digits (license, header values), which DirectWrite lays out on every
draw; drawing their digits from the glyph atlas is a later optimization.
