# Fuel widget

Shows the fuel level, the laps left in the race, and the pit window, then two estimates side by side: one from
the average of recent clean laps (white) and one from the last clean lap (dimmed). Each gives fuel per lap, laps the
fuel lasts, fuel to add to finish (Refuel), and fuel left at the end (negative, in red, when it does not last). A
`PIT` sign lights when, at the average, the fuel does not last to the end. Values are liters with two decimals;
`--.--` means not known yet. Updates 30 times per second by default (`rate_hz = 30.0`); it draws only when a value
changes. A box narrower than 380 px leaves out the fourth column (the pit sign and the fuel at the end), and one
lower than 120 px the last lap's line; at a [text size](configuration.md#text-size) of 150 the default box is
280 x 100 widget pixels and shows three columns and the average's line.

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
- **Pit window**: the laps on which the next stop can be made (`L23-31`: entering the pits on lap 23 to 31, by
  the sim's lap numbers), `no stop` when the fuel lasts to the end, `--.--` until the use per lap, the race
  length, and the tank size are known. It uses the average use per lap. The tank holds `DriverCarFuelMaxLtr` times
  `DriverCarMaxFuelPct` from the session info (a balance of performance can limit the fill). The stops needed
  fill the shortfall a full tank at a time; the label says `Pit 1 of 2` when more than one is needed. The window
  opens on the first lap from which a full tank, and full tanks at any later stops, reach the finish, and closes
  on the last lap the fuel reaches. Green while the current lap is in the window, red on its last lap.

  Laps to go count from the car's position, as for the estimates, and stops are made on whole laps, so when one
  stop would leave the car a fraction of a lap short, the window counts one more. In the Sebring recording on the
  lab PC, the tank is 104 L at 50%: 52 L, the fuel level at the start.

Checked with `pitot-bench replay --ibt` of a real 12-lap race recording at 8x speed. After about four laps it
showed 40.2 L, 4.52 L per lap, and 8.9 laps left.

Lab PC comparison with this layout (Parsec virtual display, four widgets, release build, the home PC capture of an
online practice session at 1x, `pitot-bench collect` for 60 s after 12 s, two runs each, alternating): overlay
CPU 26.84 and 26.94 ms/s before, 27.24 and 27.16 ms/s after (+0.3 ms/s).
