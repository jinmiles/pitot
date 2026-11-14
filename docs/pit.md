# Pit widget

Lists the cars of the player's class, in class order around the player. Above them: the pit lane loss measured at
this track, the refueling speed, and what a stop that fills the player's whole tank costs. Updates at 10 Hz by
default; the standing time is measured to about one update.

| Column | Meaning |
|---|---|
| Stint | Laps on the current stint: since the car left pit road, or since the start |
| Stop | The lap the car came in on at its last stop (`L12`), or `PIT` while on pit road |
| Stood | Seconds it stood still in its pit stall at that stop |
| Fuel | The most fuel (liters) that standing time allows |
| Pit | In a race: laps until the car must pit, minus the player's (`+2`: two laps after the player, `-1`: one lap before). The player's row shows the player's own pit lap (`L23`: in at the end of lap 23) |
| Rejoin | In a race: the class position the car rejoins in after that stop, and its gap to the player then (`+3.2` ahead, `-1.4` behind) |

Pit and Rejoin are blank outside races, until the player's fuel use and the lane loss are known, while the car is
on pit road, and when its fuel lasts to the end.

The sim does not publish other cars' fuel or what their crews do. It does publish where every car is
(`CarIdxLapDistPct`), whether it is on pit road (`CarIdxOnPitRoad`), and whether it is in its pit stall
(`CarIdxTrackSurface`). Everything here follows from those.

## Stops

A visit to pit road counts as a stop when the car comes off the track onto pit road and later leaves pit road back
onto the track. A car that appears in its stall from the garage, or leaves the world from its stall, has not made
a stop. A car is standing still while it is in its pit stall and its lap distance does not change between
updates. A standing run seen from one update to another started somewhere in the step before and ended somewhere
in the step after, so it is counted one update interval longer than the updates span. If updates pause for more
than 2 s (the widget was hidden) or the session time goes back, visits in progress are dropped, since their start
or end was not seen.

## Fuel

The fuel a standing time allows is `(standing time - 3.0 s) x refueling speed`, capped at the player's usable tank
(`DriverCarFuelMaxLtr` times `DriverCarMaxFuelPct`). It is an **upper bound**: tire changes, repairs, and a slow
release also keep a car in its stall, and none of them can be seen for other cars. A car that changed tires and
took a splash of fuel shows as much fuel as its tire change took time.

- **Service overhead (3.0 s)**: from one fuel-only stop of the player in the home PC capture (FIA F4): the fuel
  started to flow 1.0 s after the car stopped, and the crew was done 2.0 s after the tank was full.
- **Refueling speed**: measured at the player's own stops, as the fuel level's rise per second while in the pit
  stall (between the first and the second-to-last rising update, so the partial updates at both ends do not
  count). It is remembered per car (`CarScreenName`). Until a stop of the player's measures it, the speed is
  10.7 L/s, from the same recorded stop: 26.7 L in 2.5 s. That default is shown dimmed. Running the home PC
  capture through the widget measures 10.66 L/s.
- **Other car models**: cars of the player's class that are not the player's car are assumed to have the same
  tank and refueling speed. Their fuel is dimmed. In a multi-make class this can be wrong in both directions.

## Pit lane loss

Each stop also gives the time lost driving pit lane instead of the track: time on pit road, minus the standing
time, minus the car's time at its pace (its best lap, else its class's estimated lap time) for the same stretch of
lap distance. The widget shows the median of the last nine stops of all cars. Stops outside 0 to 120 s of loss
(timing gaps, long repairs) are left out. Lap distance on pit road is the distance along the track, and the
stretch is assumed to be driven at the lap's average speed. Pit lanes sit along straights, faster than average, so
the loss comes out somewhat low. The braking before the pit entry line and the acceleration after the exit line
are not counted either.

The loss is remembered per track (`TrackID`). A practice session where other cars make stops measures it for the
race. Until it is measured at this track, it shows `--.-`.

**Full stop** is the lane loss plus the standing time for the player's whole tank: `3.0 s + tank / refueling speed`.

For other widgets, `PitModel::expected_loss(fuel)` gives the time a stop that adds `fuel` liters costs: lane loss
plus `service_time(fuel)`.

The pit widget and the [track map](trackmap.md) share one tracker, so the stops are followed once and both use
the same loss. Each widget asks it to follow the cars at its own rate; a request within 5 ticks of the previous
one is skipped, so two widgets at 10 Hz still follow the cars about 10 times a second.

## Next stops

Other cars' fuel is not known, so each car's stints are followed with the player's fuel per lap (the fuel widget's
average of clean laps):

- **First stint**: the car starts with the player's fuel at the start line of the first lap, worked back from the
  player's current fuel and laps until the player's first stop, within the tank. Same car, same pace laps: a full
  tank at the grid less the pace laps, when the player started full.
- **After a stop**: the fuel it had left when it came in (its stint's fuel less the laps driven) plus the fuel its
  standing time allows, within the tank. That fuel is an upper bound, so the next stint can come out long.
- **Must pit**: the car must come in at the end of the last lap its fuel covers. For the player, that is the
  current fuel from the current lap distance. For other cars, it is their stint's fuel from their completed laps.
- **Pit**: the laps until that lap for the car, minus the same for the player. A car just ahead on track that has
  already crossed the line counts one lap more than the player.
- **Rejoin**: at that stop the car takes the fuel for the rest of the race (laps to go as in the fuel widget, less
  the laps until the stop), within the tank. It loses `expected_loss` of that fuel. Its race gap to the player
  (`CarIdxF2Time` difference) less that loss gives its new gap. The class cars with a larger gap to the player are
  ahead of it, counting the player at 0. The other cars keep their current gaps, so this is where the car would
  rejoin if it stopped now, with the stop it will need.

Limits:

- A car that stopped before the overlay connected is followed from the start as if it had not stopped.
- Short fills, fuel strategies different from the player's, and other car models in the class are not seen.
- Rejoin ignores the other cars' own stops before then.

## Learned values

`%APPDATA%\Pitot\pit.toml` holds the refueling speed by car and the lane loss by track:

```toml
[refuel]
"FIA F4" = 10.66

[lane_loss]
"123" = 21.40
```

It is written when a stop of the player's ends, when the track changes, when the sim disconnects, and when the
widget closes. Delete a line to measure that value again.

## Cost

The widget follows every car at its rate without allocating, and draws only the rows that changed. The
`alloc-check` test covers a race with a stop during the measurement, and the next stops and rejoins are estimated
on every update.
