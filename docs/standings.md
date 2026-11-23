# Standings widget

The session's order grouped by class, fastest class first (by `CarClassRelSpeed`), updated at 10 Hz by default.

## Where the values come from

Positions and lap times come from the session's results (`SessionInfo.Sessions[n].ResultsPositions`, `n` being the
current `SessionNum`), so laps driven before the player joined the session are included. Telemetry only knows laps
completed while the player is connected: in a home PC capture of an online practice session, the results had best
and last laps for all 14 cars that had driven, the telemetry for 8 of them.

The results change only when cars reach the line, and `CarIdxF2Time` (the race's time behind the leader) only a
second or two after that. So in a race, from the green flag, the order and the gaps are live instead: the order
follows each car's laps and lap distance, and the gaps are timed by the overlay (see [Live race order and
gaps](#live-race-order-and-gaps)).

| Value | Source |
|---|---|
| Class position | In a race from the green flag, the live order (below). Otherwise results `ClassPosition` (0-based in the session info, shown from 1); without a result, `CarIdxClassPosition` |
| Best lap | The lower of results `FastestTime` and `CarIdxBestLapTime` |
| Last lap | `CarIdxLastLapTime` when the car has one (it is newer), else results `LastTime` |
| Laps | Results `LapsComplete`, else `CarIdxLapCompleted`; in a race the higher of the two (the telemetry's is newer) |
| Gap and interval | In a race, timed live (below) to the class leader and to the car ahead in the class, `CarIdxF2Time` differences until a gap is timed; otherwise the difference in best laps |
| Tires | `CarIdxTireCompound`, named by the player's `DriverTires` list |
| Sector times | Timed by the overlay (below) |

## Layout

Each class starts with a header row; its leaders follow. In the player's class, the cars on each side of the player
follow too, after a thin line when cars are left out between them. The height sets the rows (24 px each), and the
cars fill them: three leaders per class and three cars on each side of the player, then, while rows are left, more
cars around the player, then more leaders, until every car is shown. A short widget shows fewer cars around the
player first, then fewer leaders per class (down to one), then only the player's class, so the player stays in
view. The background ends at the last row in use, and the space below stays transparent.

Header: class name in the class color, strength of field, cars in the world over cars entered, and laps: the class
leader's laps over the session's laps, or `≈` an estimate from the time left at the leader's pace in a timed
session. The first header also shows the session type's letter (P, Q, R), air and track temperature to one
decimal (`Air 21.3°C`, `Track 30.5°C` from `AirTemp`, `TrackTempCrew`), and the session time elapsed over its
length (or `≈` an estimate for a lap-limited session).

Car rows, left to right:

- class position, and in a race the places gained (green up arrow) or lost (red down arrow) since the start, by the
  live position,
- car number and driver name, and in qualifying an `OUT` badge (yellow, as in the relative) at the end of the
  name's place while the car is on an out lap,
- license class and safety rating (`D 2.83` as `D2.8`) in the license color,
- iRating, and in a race the estimated change if the race ended now,
- the maker's letter: the first letter of `CarScreenName` (BMW is B, Porsche is P), instead of a logo,
- tire compound as a colored ring (hard white, medium yellow, soft red, intermediate green, wet blue),
- gap to the class leader and interval to the car ahead (the leader shows the column names); `+1L` when the car
  ahead is one or more whole laps ahead,
- last lap to the millisecond (`1:45.345`): underlined in green when it is the car's best lap, in purple when it
  is the class's best,
- per sector, the car's last time in that sector minus the player's (green when the car was slower, red when
  faster), as many sectors as fit.

Cars on pit road are dimmed and show `PIT` instead of the last lap; the player's row is highlighted. The pace car
and spectators are left out.

In qualifying (a `SessionType` containing "Qualify": open, lone, or plain qualifying), an out lap runs from leaving
pit road until the car completes a lap or is on pit road again, the relative's rule. It includes a car's first run
out of the pits, while `CarIdxLapCompleted` is still -1. A car whose pit exit the overlay did not see (it was
already on track when the overlay connected) is not marked. Practice and race rows show no out laps. Optional columns are dropped when the widget is too narrow: sectors first, then the
iRating change, interval, places gained, maker, and tires.

## Calculations

- **Strength of field**: `1600 / ln 2 * ln(n / sum(exp(-iRating / (1600 / ln 2))))` over the class.
- **Estimated iRating change**: the community's model of the sim's formula. For each car, the chance of beating
  every other car follows from the two ratings; the change is the cars beaten minus the cars expected to be beaten,
  less `(n / 2 - place) / 100`, times `200 / n`. It is an estimate, shown only in races.
- **Start position**: from `QualifyResultsInfo` (the grid) when the session info has it, else the live class
  position first seen under green. `QualifyResultsInfo` positions are assumed 0-based like the results'; this needs a check
  against a race on the home PC.
- **Sector times**: the session info's `SplitTimeInfo` gives where sectors start. At each update the overlay reads
  every car's lap distance and interpolates, by distance, when it crossed a sector start since the previous update.
  A car on pit road, out of the world, unseen for 5 s, or skipping a sector restarts its timing. At the default
  10 Hz, a car moves about a tenth of a percent of a lap between updates, so the interpolation assumes a steady
  speed over that stretch. Crossing times are on the sim's tick count, like the race gaps below.

## Live race order and gaps

The overlay follows every car's progress, `CarIdxLapCompleted` plus `CarIdxLapDistPct`, at each update of the
standings or the relative (both 10 Hz by default), whichever comes first, in every session and from before the
green flag, so the first gaps are ready at the start. The relative uses the same clock for its gaps on track
([relative.md](relative.md)); the standings use it only in a race, so practice and qualifying are unchanged.

- **Order**: under green (`SessionState` racing), each class is sorted by progress, furthest first; that is the
  order the results take at the line, without waiting for it. Before the green flag (gridding, pace laps) the
  results' or grid order stands.
- **At the line** the lap distance can wrap a moment before the lap count goes up, or the other way round, and the
  sim can report a lap distance a hair below zero. Near the line, progress is kept continuous: of the reading and
  the reading a lap either side, the one closest to the previous progress is taken. A disagreement that lasts more
  than a second is real (a car towed across the line), and the sim's counters are taken as they are.
- **Cars out of the world** (disconnected, being towed) keep their last progress, so they keep their place until
  others pass it. When it returns it goes on from what the sim reports. Cars never seen (in the
  garage since the overlay connected) are placed by their results' laps. Cars on pit road keep their progress
  along the lap. The pace car and spectators are left out.
- **Finish**: when the checkered flag comes out, the overall leader's finish is the moment it last reached the line,
  and every car finishes the next time it reaches the line after that. A finisher keeps the laps and the time of
  its finish while it drives its cool-down lap, so finishers stay in the order they took the flag (cars with more
  laps first, then the earlier finish), which is the order the results give. If the overlay connects after the
  checkered flag, the leader's finish was not seen and the results' order is shown.
- **Gaps**: for each car, the overlay keeps the time it last passed each of 1,000 checkpoints around the lap
  (every tenth of a percent of lap distance), interpolated by distance between updates like the sector times.
  Times are on the sim's tick count, `SessionTick` over the tick rate, as the sim's lap times and results are;
  `SessionTime` slips by whole ticks against it now and then (see the [relative's accuracy](relative.md#accuracy)).
  They are kept as whole 0.1 ms after the clock's start (within 0.05 ms however long the session), so a pass takes
  6 bytes: 707 KiB for 72 cars with the relative's lap profiles. The interval to
  the car ahead is the time since the car ahead passed the spot where this car is now; the gap to the class leader
  the same with the leader. A car more than a lap behind shows whole laps, `+1L`. For a finisher, the gap is taken
  at its finish. Until the car ahead has passed the spot since the overlay connected, the gap falls back to the
  `CarIdxF2Time` difference, and so it does when updates are more than 5 s apart (after the widgets were hidden,
  for example). Practice and qualifying keep the best-lap order and gaps.

Checked against the AI race capture (41 cars, 3 classes, 10 minutes, about 4 laps), replayed through the overlay's
code at 10 Hz in all six phases of the 60 Hz frames, against references that do not come from the clock:

- **Order**: at each of 186 line crossings, the live class position matched the class position at the line from
  crossing times (186 of 186), and the results' class position for that lap at all 146 crossings after the start
  (checked at 2 Hz). At the start (the first crossing of the line, 40 cars) the results keep the grid order, so
  only 10 matched there. Between crossings the live order never contradicted the cars' progress (199,504 adjacent
  pairs).
- **Lap times** from the clock's line crossings against `CarIdxLastLapTime`: 636 of 636 within 0.8 ms (106 laps
  after the first, median 0.1 ms). On `SessionTime`, only 34% were within 1 ms; the rest were one tick, 16.7 ms,
  off.
- **At the line**, intervals against the results' `Time` (time behind the leader at the line) differences: median
  0.1 ms, largest 0.8 ms (780 intervals); gaps to the class leader: median 0.1 ms, largest 0.7 ms (810).
- **Between line crossings**, against gaps interpolated frame by frame (60 Hz) from each car's lap distance,
  without checkpoints, under green: median 0.14 ms, 90th percentile 0.7 ms, 99th percentile 1.5 ms, largest
  43 ms (382,832 gaps); the shown tenth differs from the reference's in 0.3% of readings. The error comes from the
  checkpoint spacing and is largest next to a very slow car (see the [relative's accuracy](relative.md#accuracy));
  the update rate hardly matters. With 200 checkpoints, before #195: median 1.8 ms, 99th percentile 28 ms,
  largest 0.37 s.
- **`CarIdxF2Time`** equals the results' `Time` once it updates, a median of 1.6 s (up to 2.7 s) after the car
  crosses the line. Its differences are exact but late, so it is only the fallback until a gap is timed.

## Drawing

Rows and text buffers are pooled and reused, so updates do not allocate. An update that changes nothing shown
draws nothing; any change (the header's session clock alone changes every second, and in a race the live gaps on
nearly every update) draws the widget's window again whole, as GPUI draws whole windows.

## Cost

Cost numbers on this page were measured with the overlay's earlier Direct2D renderer, before the widgets were drawn with GPUI (#183); they are measured again at the next release.

Lab PC (Parsec virtual display, four widgets, release build), the home PC capture of an online practice session
replayed at 1x, `pitot-bench collect` for 60 s after a 12 s warm-up, two runs each, alternating: overlay CPU 23.90
and 23.57 ms/s before this layout, 24.79 and 25.26 ms/s with it (+1.3 ms/s); private bytes 14.8 MiB in both. Most
of the difference is text with letters and digits (license, header values), which DirectWrite lays out on every
draw; drawing their digits from the glyph atlas is a later optimization.

Live race order and gaps, same setup but the AI race capture replayed at 4x so the race is under way,
`pitot-bench collect` for 60 s after 80 s, two runs each, alternating: overlay CPU 30.37 and 30.27 ms/s before,
31.43 and 31.43 ms/s with it (+1.1 ms/s); private bytes at most 20.6 and 18.1 MB before, 22.3 and 19.0 MB with it
(runs of the same build differ by up to 3 MB; the gap history is 0.17 MB for 72 cars). The gaps now change on every
update, so the gap columns of the rows shown are drawn twice a second instead of once a lap. At 4x, passes and line
crossings, which redraw whole rows, come four times as often as in a race.

These runs were at 2 Hz. At the 10 Hz default (#181), same setup with the AI race capture at 1x, `pitot-bench
collect` for 60 s after 20 s, three pairs: four widgets 31.71 ms/s before and 30.43 after, all ten widgets 37.83
and 35.10 (no increase measured; the PC was busy with other builds, and the race is not yet under way in that
stretch). The race under way at 10 Hz is measured with the next release.
