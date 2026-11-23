# Relative widget

Shows the three cars nearest on track ahead of the player, the player, and the three nearest behind, at 10 Hz by
default. A taller widget shows more cars, the same number on each side: rows are 33⅓ px, so the default 300 px
holds the header, seven cars, and the footer, and every 67 px more adds a car on each side. Rows share the height
left over; text keeps its size. A box narrower than 400 px leaves out the iRating and license boxes, so the name
keeps their room, and one narrower than 380 px leaves the time of day out of the footer. Sizes are in widget pixels:
at a [text size](configuration.md#text-size) of 150 the default box is 307 x 200 of them, one car on each side.

- **Order**: cars are placed by lap distance, normalized to half a lap either way, so cars across the start line
  land on the correct side.
- **Gap**: measured on track. For a car ahead, the time since it was last where the player is now; for a car
  behind, the time since the player was last where that car is. Laps and classes do not matter, so lapped and
  lapping cars and other classes are timed like any other, in practice, qualifying, and races alike. The passes
  come from the clock the standings use (see [standings.md](standings.md#live-race-order-and-gaps)): every car's
  laps and lap distance at each update of either widget, with the time each car passed each of 200 checkpoints
  around the lap, interpolated between updates. The gap shows without a sign (`1.4`): rows above the player are
  ahead, rows below are behind.
- **Estimated gap**: used when the pass has not been seen yet (the overlay just connected, or a car just came out
  of the garage), when either car is on pit road (pit road does not follow the track's speeds), or when the pass
  is older than the player's lap time. Like the measured gap, it follows the car ahead of the two on track (the
  leading car), so a car of another class is timed at its own pace, not the player's. In order:
  1. **Lap profile**: the time the leading car took on its last usable lap from the other car's lap distance to
     its own, interpolated between the clock's 200 checkpoints, across the line when needed. A lap is usable when
     the clock saw every checkpoint of it, with observations at most 1 s apart, and the car stayed on track all
     the way (`CarIdxTrackSurface` on track and not `CarIdxOnPitRoad`: no pit road, pit stall, pit approach, or
     off-track excursion, any of which distorts the stretch it covers). Each usable lap replaces the car's
     previous one; profiles last as long as the clock's other history (the connection and the session). The
     profile is used on pit road too: it is from a lap on track, so it tells how far apart the two cars are at
     track speed, which is what the sim's own relative shows for a car in the pits, while a measured gap there
     would count the pit lane's speed limit and the stop.
  2. **Estimated time**: without a profile, the share of a lap between the two by `CarIdxEstTime` (the sim's
     estimate of time into the lap), each car's taken over its own `CarClassEstLapTime` because the sim times
     each car against its own class's lap (in AI races, its own car's), times the leading car's best lap
     (`CarIdxBestLapTime`; without one, its class's estimated lap, or `DriverCarEstLapTime` for the player). Used
     when it puts the leading car ahead.
  3. **Distance**: otherwise the lap distance between the two times that lap time.
- **Lap difference**: from `CarIdxLap`, corrected when one of the two cars has just crossed the line. Cars at
  least a lap ahead are shown in red, cars at least a lap behind in blue.
- **Columns**: class position, car number in the class color, driver name, badges, license class and safety
  rating in the license color (`A 1.83` as `A1.8`), iRating, the maker's letter (the first letter of
  `CarScreenName`; no logos), and the gap. The player's name is orange.
- **Badges**: `PIT` while the car is on pit road (the row is dimmed), `OUT` on its out lap (from leaving pit road
  until it completes a lap), and a black `F` when `CarIdxSessionFlags` has the black flag.
- **Header**: track temperature (`TrackTempCrew`) to one decimal (`31.4`), the player's incidents
  (`PlayerCarMyIncidentCount`), and the local time. Water and oil temperatures are not shown.
- **Footer**: the player's lap, the time of day in the sim (`SessionTimeOfDay`), and the session type with its
  elapsed time over its length.
- **Excluded**: the pace car, spectators, and cars not in the world (`CarIdxTrackSurface` of -1).
- Per-car arrays come from the connection, sized by the sim's `CarIdx*` length. Names and numbers come from the
  session info. The clock's lap profiles take 201 values per car, allocated with the rest of the clock once per
  connection.
- Text buffers have fixed capacity and are reused, so updates do not allocate. An update that changes nothing
  shown draws nothing; a change draws the widget's window again whole.

## Accuracy

Checked by replaying captures through the overlay's clock: at 10 Hz as the widget runs, against the same clock fed
every frame (60 Hz), for the three cars ahead and behind of every car in turn as the player. Captures: an AI race
(41 cars in 3 classes, 10 min) and an online practice recorded on the home PC (18 cars in one class, 3.5 min).

| Capture | Measured gap error (median, 99th percentile, largest) | Estimate used |
|---|---|---|
| AI race | 0.0, 0.4 ms, 13.5 ms (1,347,318 gaps) | 1.5%, pass not seen yet (the start of the capture) |
| Online practice | 0.0, 0.6 ms, 29 ms (71,482 gaps) | 25% a car on pit road, 5.5% pass not seen yet |

No pass was older than a lap.

### Estimates

Every estimate was forced for every pair that has a measured gap on the every-frame clock, which stands for the
truth, and compared with it. Error of the gap shown, in seconds: median, 90th and 99th percentile, largest. Pairs
with neither car on pit road:

| Estimate | AI race, same class | AI race, other class | Practice, same class |
|---|---|---|---|
| Before: distance x the player's lap, `CarIdxEstTime` difference for the same class | 0.19, 0.77, 1.55, 2.66 | 0.24, 0.96, 2.08, 3.35 | 0.47, 2.68, 14.2, 18.1 |
| Lap profile of the leading car, where it has one | 0.035, 0.22, 0.61, 2.60 | 0.046, 0.24, 0.58, 1.04 | 0.056, 0.13, 0.18, 0.19 |
| Distance x the leading car's class lap, `CarIdxEstTime` difference for the same class | 0.19, 0.78, 1.55, 2.66 | 0.24, 0.96, 2.11, 3.35 | 0.47, 2.68, 14.2, 18.1 |
| `CarIdxEstTime` scaled by the two class laps (as iRDashies does) | 0.13, 0.56, 1.40, 2.75 | 0.19, 0.65, 1.22, 2.93 | 0.47, 2.68, 14.2, 18.1 |
| Estimated-time share x the leading car's best lap (now the fallback) | 0.091, 0.37, 0.86, 2.63 | 0.19, 0.53, 0.93, 2.97 | 0.21, 2.51, 13.2, 18.1 |

Pairs: AI race 1,048,176 same class and 299,142 other class (685,850 and 210,888 with a profile), practice 71,568
(2,470 with a profile: in 3.5 minutes few cars completed a lap on track that the clock saw whole). On the same pairs
as the profile, other classes in the AI race: before 0.31, 1.13, 2.22, 3.35; the fallback 0.21, 0.56, 0.94, 1.52.

- The AI race's class lap estimates (106, 110, and 121 s) are far from the laps the AI drove (123, 126, and 130 s),
  so estimates built on them alone run short, and the leading car's class lap does no better than the player's.
  The best lap gives the pace, the estimated times give the shape of the track between the two cars. In the
  practice session, with one class, the old estimate and the class-lap variants are the same.
- Only the trailing car on pit road (the leading car on track, so the measured gap is still a truth), practice:
  before 0.58, 2.72, 15.9, 18.2 (10,446 pairs); lap profile 0.080, 0.12, 0.15, 0.15 (332); fallback 0.16, 2.50,
  14.5, 16.8. With the leading car on pit road there is no truth: its measured gap counts the pit lane.
- Where the widget shows an estimate and a truth exists (practice, 11,810 pairs): before 0.71, 7.5, 22, 77; now
  0.19, 7.4, 22, 77. The long tails are cars driving slowly, which no estimate can know.

Paths the widget takes, as a share of all pairs:

| Capture | Measured | Lap profile | Estimated time | Distance |
|---|---|---|---|---|
| AI race, before | 98.49% | | 1.07% (same class only) | 0.44% |
| AI race, now | 98.49% | 0.00% | 1.46% | 0.05% |
| Practice, before | 69.53% | | 30.13% | 0.34% |
| Practice, now | 69.53% | 0.32% | 29.81% | 0.34% |

The AI race has no pit stops, so its estimates are all from the first lap of the capture, before any car has a
profile; in the practice session most cars on pit road had not driven a usable lap yet. In longer sessions more of
the estimates come from lap profiles.

## Cost

Cost numbers on this page were measured with the overlay's earlier Direct2D renderer, before the widgets were drawn with GPUI (#183); they are measured again at the next release.

Development measurement on the lab PC (release build, 60 Hz replay, process CPU time over 10 s, not
`pitot-bench`), with status, input trace at 30 Hz, and relative at 15 Hz: 2.50% of one core, 0.31% of all
eight cores, 14.3 MB private bytes. This is over the all-core budget. Most of the added cost is DirectWrite
`DrawText` for about 28 strings per redraw, which the glyph atlas and cached layouts are meant to remove.

Lab PC comparison with the header, footer, badges, and new columns (Parsec virtual display, four widgets, release
build, the home PC capture of an online practice session at 1x, `pitot-bench collect` for 60 s after 12 s, two runs
each, alternating): overlay CPU 25.96 and 26.46 ms/s before, 27.39 and 27.02 ms/s after (+1.0 ms/s); private bytes
0.9 to 1.2 MiB higher in each pair, not yet traced.

Measured gaps on the clock shared with the standings, same setup: overlay CPU 29.44 and 27.71 ms/s before, 29.67 and
27.03 ms/s after; private bytes at most 21.5 and 17.4 MiB before, 21.3 and 17.2 MiB after. No difference shows: the
clock already ran for the standings, and observing it at 10 Hz and reading six gaps costs microseconds per update.
Further runs were discarded because compiles on the same PC raised both sides by about 5 ms/s while they ran.
