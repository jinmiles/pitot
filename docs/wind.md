# Wind widget

Shows the wind as one value, its speed in km/h, and, on a dial that turns with the car, where the air flows
relative to it. The direction is shown by the dial's arrow only; it is not written out as a compass point.
Updates at 10 Hz by default.

- **Dial**: the car's nose is at the top (amber mark, the player's color). The arrow runs from the side the wind comes from to the
  side it blows to: pointing down is a headwind, up a tailwind, sideways a crosswind. Its color follows the
  speed: grey below 10 km/h, white below 25, yellow below 40, red above. Below 1 km/h the air counts as calm and
  there is no arrow, and without the car's heading there is none either. The dial's north mark (`N`) and the
  ticks of the other compass points turn with the car.
- **Speed**: `WindVel` in whole km/h with its unit on the same line (`12 km/h`), `--` without data. The speed is
  not split into parts along and across the car; the arrow shows where it hits the car.

The text column sits right of the dial: the `Wind` label above the speed, the pair centered vertically on the
dial's middle at any size and text size.

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
60 s after a 12 s warm-up, two runs each, alternating, while other builds ran on the PC: overlay CPU 32.48 and
32.14 ms/s without it, 31.08 and 34.04 ms/s with it (+0.25 ms/s on average, within the run-to-run spread);
private bytes within the usual run-to-run spread (17.6 to 20.9 MB).
