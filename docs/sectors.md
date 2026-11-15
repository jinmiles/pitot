# Sectors widget

The player's lap sector by sector: each sector's time against the player's best, the class's best this session,
and the cars directly ahead and behind in class order. Updated at 5 Hz by default.

## Layout

One row per sector of the track (`SplitTimeInfo`), under a header with the column names and the car numbers of
the two neighbours (`#12`). The row of the sector the player is in is highlighted. A box narrower than 340 px leaves
out the Behind column, and the header shows the car numbers only when a column is at least 64 px wide (at a
[text size](configuration.md#text-size) of 150, the default box is 267 widget pixels wide: four columns).

| Column | Shows |
|---|---|
| S | The sector's number |
| Time | The player's last time in the sector: on the current lap once the sector is driven, until then the last lap's (dimmed). Purple when it is the class's best time in the sector, green when it is the player's best |
| Best | The time minus the player's best time in the sector |
| Class | The time minus the best time in the sector of any car of the player's class |
| Ahead, Behind | The time gained (green) or lost (red) in the sector against the car directly ahead or behind in class order, without a sign |

Times under a minute show to the millisecond (`32.456`), differences to the hundredth (`0.23`). A cell stays empty
while either time is missing: a neighbour that has not driven the sector since the overlay started, or no
neighbour on that side (the class leader has no car ahead). Before the session info has sectors, the widget says
*No sectors yet*.

## Where the values come from

- **Sector times** are timed by the overlay, like the standings' sector columns: at each update it reads every
  car's lap distance (`CarIdxLapDistPct`) and interpolates, by distance, when the car crossed a sector start since
  the previous update. A car on pit road, out of the world, unseen for 5 s, or skipping a sector restarts its
  timing. At the default 5 Hz a car moves about a fifth of a percent of a lap between updates.
- **Best times** are the best the overlay timed since the session started (`SessionNum` changed) or the overlay
  connected. Laps driven before that are not known: the session's results have lap times, not sector times.
- **Class order** comes from the session's results (`ClassPosition`), else from `CarIdxClassPosition`.

## Gains and losses

In a race, the car ahead and the car behind are on track around the player, so the comparison is the time gained
or lost against them in the sector: the player's sector time minus theirs, for passes of the sector next to each
other in time. The car ahead's pass is the one that ended last before the player's; the car behind's is the one
that ended first after it, within one lap (the player's best lap time, or 10 minutes before the player has one).
The car behind finishes the sector after the player, so until it does, its cell shows the comparison from the
player's previous pass, dimmed. A different car ahead or behind (a pass, a pit stop) clears that side.

In practice and qualifying the class order follows best laps and the neighbours can be anywhere on track, so they
are compared by their best time in the sector instead: the player's last time minus the neighbour's best time in
that sector.

## Drawing

Rows and text buffers are allocated when the widget is created and reused, so updates do not allocate (checked by
the `alloc-check` test). Only the rows that changed are drawn again. Digits come from the glyph atlas.

## Cost

Lab PC (Parsec virtual display, the four reference widgets plus this one, release build), the home PC capture of
an online practice session replayed at 1x, `pitot-bench collect` for 60 s after a 12 s warm-up, two runs each,
alternating, the same build with the widget off and on: overlay CPU 30.32 and 29.74 ms/s off, 30.45 and
30.26 ms/s on (about +0.3 ms/s); private bytes at most 19.9 and 17.2 MiB off, 20.2 and 17.8 MiB on (+0.3 to
+0.6 MiB). Other builds ran on the PC at the same time, so the absolute numbers are higher than in quieter runs.
The executable grows by 20.5 KiB.
