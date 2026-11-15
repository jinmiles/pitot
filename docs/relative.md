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
  is older than the player's lap time. It is the distance times the player's lap time; for cars of the player's
  class, `CarIdxEstTime` (the sim's estimate of time into the lap) refines it when it agrees with the distance. It
  is never compared across classes: each class's estimate follows its own pace, so a faster car ahead on track can
  have a lower estimate than the player.
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
  session info.

## Accuracy

Checked by replaying captures through the overlay's clock: at 10 Hz as the widget runs, against the same clock fed
every frame (60 Hz), for the three cars ahead and behind of every car in turn.

| Capture | Measured gap error (median, 99th percentile, largest) | Estimate used | Old estimate error (median, 90th percentile, largest) |
|---|---|---|---|
| AI race, 41 cars, 3 classes, 10 min | 0.0, 0.4 ms, 13.5 ms (1,347,318 gaps) | 1.5%, pass not seen yet (the start of the capture) | 0.20 s, 0.80 s, 3.3 s |
| Online practice, home PC, 3.5 min | 0.0, 0.6 ms, 29 ms (71,482 gaps) | 25% a car on pit road, 5.5% pass not seen yet | 0.47 s, 2.7 s, 18 s |

No pass was older than a lap. The old estimate's largest errors are across classes and in practice, where the
player's lap time says little about other cars.
- Text buffers have fixed capacity and are reused, so updates do not allocate. Only the span of changed lines is
  drawn again, and license letters are drawn apart from their digits, which come from the glyph atlas.

## Cost

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
