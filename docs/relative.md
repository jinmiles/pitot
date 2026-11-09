# Relative widget

Shows the three cars nearest on track ahead of the player, the player, and the three nearest behind, at 10 Hz by
default.

- **Order and gap**: cars are placed by lap distance, normalized to half a lap either way, so cars across the start
  line land on the correct side. The gap is that distance times the player's lap time. For cars of the player's
  class, `CarIdxEstTime` (the sim's estimate of time into the lap) refines the gap when it agrees with the
  distance. It is never compared across classes: each class's estimate follows its own pace, so a faster car
  ahead on track can have a lower estimate than the player. As in the sim's own relative, cars ahead show a
  negative gap and cars behind a positive one.
- **Lap difference**: from `CarIdxLap`, corrected when one of the two cars has just crossed the line. Cars at
  least a lap ahead are shown in red, cars at least a lap behind in blue.
- **Columns**: class color, car number, driver name, iRating, gap. Cars on pit road are dimmed and show `PIT`.
- **Excluded**: the pace car, spectators, and cars not in the world (`CarIdxTrackSurface` of -1).
- Per-car arrays come from the connection, sized by the sim's `CarIdx*` length. Names and numbers come from the
  session info.
- Text buffers have fixed capacity and are reused, so updates do not allocate. A redraw happens only when a row
  changes.

## Cost

Development measurement on the lab PC (release build, 60 Hz replay, process CPU time over 10 s, not
`pitot-bench`), with status, input trace at 30 Hz, and relative at 15 Hz: 2.50% of one core, 0.31% of all
eight cores, 14.3 MB private bytes. This is over the all-core budget. Most of the added cost is DirectWrite
`DrawText` for about 28 strings per redraw, which the glyph atlas and cached layouts are meant to remove.
