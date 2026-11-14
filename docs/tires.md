# Tire widget

The player's four tires laid out like the car (front on top, left on the left), updated once per second by default.
Each tire shows three columns, the left, middle, and right of the tread as seen from behind the car:

- carcass temperature in °C (`LFtempCL`, `LFtempCM`, `LFtempCR` and the other corners) on a color swatch: blue below
  50 °C, green around 75 °C, yellow around 95 °C, red from 110 °C,
- tread remaining in percent (`LFwearL`, `LFwearM`, `LFwearR`): white from 80 %, yellow from 60 %, red below,
- the distance the tire has run since it was put on, in km (`LFodometer`).

The header shows the compound on the car (`PlayerTireCompound`, named and colored from the session info's
`DriverTires` as in the standings) and, when the session limits dry tire sets (`PlayerCarDryTireSetLimit` above 0),
the sets used over the limit (`TireSetsUsed`, `1/4`).

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

## Drawing

The widget redraws only when something it shows changes. When one tire's values change alone, only its block is
drawn again; a stop changes every tire, and the dimming or dating restyles all of them. Text buffers are allocated
once, so updates do not allocate (`cargo test -p pitot-overlay --features alloc-check`).

## Cost

See the pull request that added the widget for its A/B numbers on the lab PC.
