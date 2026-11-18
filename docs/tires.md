# Tire widget

The player's four tires laid out like the car (front on top, left on the left), updated ten times a second by
default. Each tire shows three columns, the left, middle, and right of the tread as seen from behind the car:

- carcass temperature in °C (`LFtempCL`, `LFtempCM`, `LFtempCR` and the other corners) on a color swatch: blue below
  50 °C, green around 75 °C, yellow around 95 °C, red from 110 °C,
- tread remaining in percent (`LFwearL`, `LFwearM`, `LFwearR`): white from 80 %, yellow from 60 %, red below,
- the distance the tire has run since it was put on, in km (`LFodometer`).

The header shows the compound on the car (`PlayerTireCompound`, named and colored from the session info's
`DriverTires` as in the standings) and, when the session limits dry tire sets (`PlayerCarDryTireSetLimit` above 0),
the sets used over the limit (`TireSetsUsed`, `1/4`).

In a small box, or at a large [text size](configuration.md#text-size): a header narrower than 200 px leaves out the
sets and the pit lap, a tire block lower than 54 px leaves out its third line (distance or pressure), and a line
whose three numbers do not all fit their columns is left out while the temperature swatches keep their colors. At
150 the default box is 147 x 114 widget pixels: the temperatures show, the wear numbers do not.

## Values from the last stop

The sim updates wear and carcass temperatures in live telemetry only while the car is in its pit stall. In a home
PC capture of an online practice session, each of the 24 values changed exactly once in 3.5 minutes, when the car
stopped in its stall; the distances changed every 100 m. Live telemetry has no surface temperatures or hot
pressures at all (the sim writes those only to `.ibt` files).

So the numbers describe the tires at the last stop, not now:

- Until the values change during the connection, the widget cannot tell when they were taken (in the garage, or at a
  stop before the overlay started). The swatches and wear are dimmed and the header says `Last pit`.
- Once they change, the header shows the lap they were taken on (`Pit lap 12`) and the colors are full.
- A reconnect, or a switch between live values and a replay's capture, starts undated again.

## Replays

Live telemetry has no surface temperatures, but the sim's own telemetry files (`.ibt`) do, every tick. While the
sim plays a replay, the widget shows from those files the replayed moment's surface temperatures, tread, and
hot pressure (in kPa, in place of the distance), with `Replay` in the header. The files are found and read as in
[Sim replays](overlay-pipeline.md#the-sims-telemetry-files): five samples a second, so at the widget's default
10 Hz each sample shows within a tenth of a second of its moment.

- With a capture of the session, the moment is the capture's, and the other widgets show the player's view as
  recorded, whichever car the camera follows; so does this one.
- Without a capture, the other widgets hide. This widget shows only while the replay camera follows the player's
  car (`CamCarIdx` is `PlayerCarIdx`): the files hold the player's car alone.
- A moment no file covers (the telemetry recording was off, or between stints) shows the capture's values, or
  nothing without a capture.

## Drawing

The widget redraws only when something it shows changes, and then draws its window again whole. Text buffers are
allocated once, so updates do not allocate (`cargo test -p pitot-overlay --features alloc-check`).

## Cost

Cost numbers on this page were measured with the overlay's earlier Direct2D renderer, before the widgets were drawn with GPUI (#183); they are measured again at the next release.

Lab PC (Parsec virtual display, the four reference widgets, release build), the home PC capture of an online
practice session at 1x, `pitot-bench collect` for 60 s after 12 s, the same build with the tire widget off and on,
two runs each, alternating: overlay CPU 28.91 and 31.73 ms/s off, 28.78 and 30.08 ms/s on; private bytes at most
21.2 and 17.1 MB off, 20.5 and 17.5 MB on. The difference is below the run-to-run spread (other builds were running on
the PC); the release binary grew by 23.5 KB (1,117,184 to 1,140,736 bytes).

With replays from telemetry files and the 5 Hz default (same setup, the main build at 1 Hz against this one at
5 Hz, driving): overlay CPU 29.04 and 27.69 ms/s before, 29.66 and 27.67 ms/s after; private bytes at most 20.5 and
17.9 MB before, 21.7 and 17.7 MB after, again within the spread. Samples from telemetry files take 72 bytes each, about
1.3 MB per hour of driving, kept until the connection ends. The release binary grew by 46 KB (1,171,968 to 1,219,072
bytes), mostly the `.ibt` reader.
