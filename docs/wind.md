# Wind widget

Shows the wind speed and, on a dial that turns with the car, where the air flows relative to it. Updates at 5 Hz
by default.

- **Dial**: the car's nose is at the top (orange mark). The arrow runs from the side the wind comes from to the
  side it blows to: pointing down is a headwind, up a tailwind, sideways a crosswind. `N` and the ticks of the
  other compass points turn with the car. Its color follows the speed: grey below 10 km/h, white below 25,
  yellow below 40, red above. Below 1 km/h the air counts as calm and there is no arrow.
- **Speed**: `WindVel` in whole km/h.
- **From**: the compass point (of eight) the wind comes from.
- **Head / Tail** and **Cross**: the parts of the speed along the car (headwind or tailwind) and across it.

## Where the values come from

| Value | Source |
|---|---|
| Wind speed | `WindVel` (m/s at the start/finish line) |
| Wind direction | `WindDir` (radians at the start/finish line) |
| Car heading | `YawNorth` (radians) |

The relative direction is `WindDir - YawNorth`. The checks behind the conventions:

- **`YawNorth`** is the car's compass heading, clockwise from north. In the two `.ibt` recordings on the lab PC
  (Sebring and Monza), the course over ground from consecutive `Lat`/`Lon` samples on straights matches it within
  0.2 degrees (median 0.0, 10th to 90th percentile -0.13 to +0.19 degrees). `Yaw` is the same angle counter-clockwise
  in the track's frame: `YawNorth + Yaw` is constant and equals `TrackNorthOffset` from the session info.
- **`WindDir`** uses the same compass frame and is read as the direction the wind comes from, the meteorological
  convention. The SDK's description ("Wind direction at start/finish line") does not say which; iRDashies (MIT)
  reads it the same way. A physics check in the same recordings (acceleration at full throttle in the same gear
  and speed, against the wind along the heading) leaned that way over the whole lap but was not conclusive at the
  same place on track, so the convention still needs a check in a sim session with a set wind direction.

`YawNorth` is the player's car, also while spectating another car.

## Drawing

Angles are kept in steps of 2 degrees and speeds in whole km/h, and the widget redraws only when what it shows
changes. Text buffers are reused, so updates do not allocate.

## Cost

Lab PC (Parsec virtual display, release build), the home PC capture of an online practice session replayed at
1x, the four reference widgets plus the wind widget against the same build without it, `pitot-bench collect` for
60 s after a 12 s warm-up, two runs each, alternating: see the pull request that added the widget.
