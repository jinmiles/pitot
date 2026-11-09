# Fuel widget

Shows current fuel, fuel used per lap, laps left on the current fuel, and how much to add (or how much is spare)
to finish. Updates once per second by default.

- **Use per lap**: the average of the last five clean laps. A lap counts when it was completed without visiting
  pit road and without fuel being added (any rise of more than 0.05 L). Until the first clean lap the widget
  shows `measuring`.
- **Laps left**: current fuel divided by use per lap.
- **Laps to go**: `SessionLapsRemainEx` in lap-limited sessions. In timed sessions, `SessionTimeRemain` divided
  by the last lap time (or the estimated lap time), rounded up, plus the lap in progress when time runs out.
- **To finish**: laps to go times use per lap, minus current fuel. Shown in orange as `add N L` when short, in
  green as `N L spare` otherwise.

Checked with `pitot-bench replay --ibt` of a real 12-lap race recording at 8x speed. After about four laps it
showed 40.2 L, 4.52 L per lap, and 8.9 laps left.
