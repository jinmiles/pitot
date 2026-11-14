# Standings widget

The session's order grouped by class, fastest class first (by `CarClassRelSpeed`), updated at 2 Hz by default.

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

Each class starts with a header row; the class leaders (three) follow. In the player's class, the three cars on
each side of the player follow too, after a thin line when cars are left out between them. The configured height
sets the most rows shown; the background ends at the last row in use, and the space below stays transparent.

Header: class name in the class color, strength of field, cars in the world over cars entered, and laps: the class
leader's laps over the session's laps, or `≈` an estimate from the time left at the leader's pace in a timed
session. The first header also shows the session type's letter (P, Q, R), air and track temperature
(`AirTemp`, `TrackTempCrew`), and the session time elapsed over its length (or `≈` an estimate for a lap-limited
session).

Car rows, left to right:

- class position, and in a race the places gained (green up arrow) or lost (red down arrow) since the start, by the
  live position,
- car number and driver name,
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
and spectators are left out. Optional columns are dropped when the widget is too narrow: sectors first, then the
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
  2 Hz, a car moves about half a percent of a lap between updates, so the interpolation assumes a steady speed over
  that stretch.

## Live race order and gaps

In a race session the overlay follows every car's progress, `CarIdxLapCompleted` plus `CarIdxLapDistPct`, at each
update (2 Hz by default) from before the green flag, so the first gaps are ready at the start.

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
- **Gaps**: for each car, the overlay keeps the time it last passed each of 200 checkpoints around the lap (every
  half percent of lap distance), interpolated by distance between updates like the sector times. The interval to
  the car ahead is the time since the car ahead passed the spot where this car is now; the gap to the class leader
  the same with the leader. A car more than a lap behind shows whole laps, `+1L`. For a finisher, the gap is taken
  at its finish. Until the car ahead has passed the spot since the overlay connected, the gap falls back to the
  `CarIdxF2Time` difference, and so it does when updates are more than 5 s apart (a rate below 0.2 Hz). Practice
  and qualifying keep the best-lap order and gaps.

Checked against the AI race capture (41 cars, 3 classes, 10 minutes, about 4 laps), replayed through the overlay's
code at 2 Hz:

- At each of 186 line crossings, the live class position matched the class position at the line from crossing
  times (186 of 186), and the results' class position for that lap at all 146 crossings after the start. At the
  start (the first crossing of the line, 40 cars) the results keep the grid order, so only 10 matched there.
- Intervals at the line against the results' `Time` (time behind the leader at the line) differences: median error
  0.3 ms, largest 15 ms (135 intervals); gaps to the class leader: median 0.3 ms, largest 17 ms.
- Gaps at every update against the same timing from every frame (60 Hz): intervals median 0.9 ms, 99th percentile
  8.5 ms, largest 18 ms (39,913 intervals); gaps to the class leader median 1.2 ms, largest 19 ms. A higher rate
  would not make the gaps noticeably more accurate.
- `CarIdxF2Time` differences, read 3 to 6 s after a crossing, matched the intervals at the line to a median of
  0.5 ms but were off by up to 0.5 s (gaps to the leader up to 2.8 s) in 5% of cases; it is only a fallback.

## Drawing

Rows and text buffers are pooled and reused, so updates do not allocate. Only the rows that changed are drawn
again (the header's session clock alone changes every second). In a race the live gaps change on nearly every
update while the rest of a row stays the same until cars pass each other or reach the line, so rows whose gaps
alone changed redraw only the gap and interval columns, and text outside the redrawn area is skipped rather than
laid out and clipped.

## Cost

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
