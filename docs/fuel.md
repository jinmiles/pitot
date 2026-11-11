# Fuel widget

Shows the fuel level and the laps left in the race, then two estimates side by side: one from the average of
recent clean laps (blue) and one from the last clean lap (yellow). Each gives fuel per lap, laps the fuel lasts,
fuel to add to finish (Refuel), and fuel left at the end (negative, in red, when it does not last). A `PIT` sign
lights when, at the average, the fuel does not last to the end. Values are liters with two decimals; `--.--` means
not known yet. Updates once per second by default.

- **Use per lap**: the average of the last five clean laps. A lap counts when it was driven in full under
  green-flag running, without visiting pit road and without fuel being added (any rise of more than 0.05 L).
  Laps are not counted while `SessionState` is get in car, warm-up, parade laps, or cool-down, or while
  `SessionFlags` has a full-course caution, one lap to green, or a red flag. The lap in progress when tracking
  starts (overlay started or reconnected mid-lap) is not counted either. Until the first clean lap the estimates
  show `--.--`.
- **Last lap**: the most recent of those clean laps.
- **Laps remain**: current fuel divided by use per lap.
- **Laps to go**: `SessionLapsRemainEx` in lap-limited sessions. In timed sessions, `SessionTimeRemain` divided
  by the last lap time (or the estimated lap time), rounded up, plus the lap in progress when time runs out.
- **Laps in race**: the laps to go above.
- **Refuel** and **fuel at end**: fuel at the end is current fuel minus laps to go times use per lap; refuel is
  its shortfall (0 when the fuel lasts).

Checked with `pitot-bench replay --ibt` of a real 12-lap race recording at 8x speed. After about four laps it
showed 40.2 L, 4.52 L per lap, and 8.9 laps left.

Lab PC comparison with this layout (Parsec virtual display, four widgets, release build, the home PC capture of an
online practice session at 1x, `pitot-bench collect` for 60 s after 12 s, two runs each, alternating): overlay
CPU 26.84 and 26.94 ms/s before, 27.24 and 27.16 ms/s after (+0.3 ms/s).
