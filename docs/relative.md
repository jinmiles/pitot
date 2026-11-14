# Relative widget

Shows the three cars nearest on track ahead of the player, the player, and the three nearest behind, at 10 Hz by
default.

- **Order and gap**: cars are placed by lap distance, normalized to half a lap either way, so cars across the start
  line land on the correct side. The gap is that distance times the player's lap time. For cars of the player's
  class, `CarIdxEstTime` (the sim's estimate of time into the lap) refines the gap when it agrees with the
  distance. It is never compared across classes: each class's estimate follows its own pace, so a faster car
  ahead on track can have a lower estimate than the player. The gap shows without a sign (`1.4`): rows above the
  player are ahead, rows below are behind.
- **Lap difference**: from `CarIdxLap`, corrected when one of the two cars has just crossed the line. Cars at
  least a lap ahead are shown in red, cars at least a lap behind in blue.
- **Columns**: class position, car number in the class color, driver name, badges, license class and safety
  rating in the license color (`A 1.83` as `A1.8`), iRating, the maker's letter (the first letter of
  `CarScreenName`; no logos), and the gap. The player's name is orange.
- **Badges**: `PIT` while the car is on pit road (the row is dimmed), `OUT` on its out lap (from leaving pit road
  until it completes a lap), and a black `F` when `CarIdxSessionFlags` has the black flag.
- **Header**: water temperature (`WaterTemp`), track temperature (`TrackTempCrew`), the player's incidents
  (`PlayerCarMyIncidentCount`), and the local time.
- **Footer**: the player's lap, the time of day in the sim (`SessionTimeOfDay`), and the session type with its
  elapsed time over its length.
- **Excluded**: the pace car, spectators, and cars not in the world (`CarIdxTrackSurface` of -1).
- Per-car arrays come from the connection, sized by the sim's `CarIdx*` length. Names and numbers come from the
  session info.
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
