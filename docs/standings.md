# Standings widget

Race order grouped by class, fastest class first (by `CarClassRelSpeed`), updated at 2 Hz by default.

- Within a class, cars are ordered by `CarIdxClassPosition`; cars without a position yet follow in car index
  order.
- Each class shows its four leaders, plus the player's row when the player is further back. A header with the
  class color separates classes in multiclass sessions; when the session gives no class name (AI sessions), the
  leader's car name is used.
- Columns: class position, car number, driver name, iRating, license (in the license color), last lap, and gap
  to the class leader from `CarIdxF2Time`. Gaps under 0.05 s are hidden, since F2Time is not maintained in every
  session state.
- Cars on pit road are dimmed and show `PIT` instead of the last lap. The pace car and spectators are excluded.
- Rows and text buffers are pooled and reused, so updates do not allocate. A redraw happens only when a row
  changes.

## Cost

Development measurement on the lab PC (release build, 60 Hz replay, process CPU time over 10 s, not
`pitot-bench`), with status, input trace (30 Hz), relative (15 Hz), and standings (4 Hz): 2.19% of one core,
0.27% of all eight cores, 14.7 MB private bytes. This is over the all-core budget, mostly DirectWrite text
drawing, which the glyph atlas and cached layouts are meant to remove.
