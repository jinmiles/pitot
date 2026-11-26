# Design language

How the overlay's widgets look: one font and one palette, kept as plain as possible. A widget sits on top of the
sim, so it has to be read at a glance and must not compete with the picture behind it.

## Font

Widget text is Segoe UI bold, at the four text sizes of [rendering](overlay-rendering.md#text) times
`[general] text_size`.

- Segoe UI is installed with every Windows, so the overlay carries no font file.
- Its bold digits all have one width (1178 of 2048 font units), so a number that changes keeps its width and does
  not move in its column. Of Segoe UI's weights only regular and bold have this; in semibold a `1` is 28% narrower
  than a `0`.
- Bold stays legible over a bright scene, and at the smallest text size.
- Numbers are put together from glyphs measured once per size, which is checked against GPUI's shaping of whole
  lines in this font ([rendering](overlay-rendering.md#text)).

## Colors

Defined once, in `crates/overlay/src/widgets/palette.rs`; widgets use its names, not color values.

**Surfaces and text have no hue.** A color appears only where it carries a meaning, and each meaning has one color.

| Name | Value | Used for |
|---|---|---|
| `SURFACE` | `#141414` | A widget's panel. Its alpha is the widget's `opacity` scaled by `[general] background_opacity`. |
| `RAISED` | `#242424` | A band or block drawn as background: a class header row, a tire block. |
| `TEXT` at 10% | | A row lifted without a hue, such as the sector the player is in: lighter than the panel over any scene. |
| `TEXT` | `#F2F2F2` | Values and names. |
| `DIM` | `#A3A3A3` | Labels, units, secondary values (such as the fuel estimate from the last lap). |
| `FAINT` | `#6B6B6B` | Things switched off or not known yet. |
| `CHIP` | `#E6E6E6` | A light chip with dark text, such as the iRating. |
| `ROAD`, `ROAD_EDGE` | `#5E5E5E`, `#050505` | The track on the track map and its edge. |

**Meanings.**

| Name | Value | Meaning |
|---|---|---|
| `PLAYER` | `#FFAA33` | The player: their name and position, a band behind their row (20%), and their marks on the map, the wind dial, and the steering wheel. |
| `GAIN` | `#5CC98F` | Time or places gained, a personal best, enough fuel, a pit window that is open. |
| `LOSS` | `#EE6B60` | Time or places lost, too little fuel, a pit window that is closing. |
| `BEST` | `#A884F3` | The fastest lap or sector of the session or the class. |
| `CAUTION` | `#E9C46A` | Something to watch: an out lap, ABS, a strong wind, worn tread, idle or replay. |
| `PIT` | `#4C8DE6` | On the pit road. |
| `LAP_AHEAD`, `LAP_BEHIND` | `#EE8B82`, `#86AEEA` | A car a lap or more ahead of or behind the player, as in iRacing's own relative. |

**Conventions kept as they are.** The pedals are green (throttle), red (brake), and blue (clutch), as in every
input overlay; tire compounds are red (soft), yellow (medium), white (hard), green (intermediate), and blue (wet),
as on their markings; tire temperatures run from blue (cold) through green to red (hot). These use the palette's
values. Class colors (`CarClassColor`) and license colors (`LicColor`) come from the session and are drawn as
given; text on a license chip is white or dark, whichever contrasts more with that license's color.

**Not used.** No color only to tell two things apart (such as two rows of estimates) or to decorate: a widget
without a meaning to show is white and grey.

## Type

Four text roles, the four text sizes, defined in `crates/overlay/src/widgets/design.rs`:

| Role | Size | Used for |
|---|---|---|
| `LABEL` | 12 px | Labels, units, column names, chips and badges, context beside the main values |
| `BODY` | 14 px | Rows of a table, a dense widget's values, the relative's header and footer |
| `VALUE` | 18 px | The values a widget is read for at a glance (fuel level, laps, pit window), the relative's rows |
| `HERO` | 26 px | The one value a small widget is about (the wind's speed) |

**Notation.**

- Labels are in sentence case (`Fuel level`, `Laps in race`, `Lane loss`); abbreviations stay in capitals (`GAP`,
  `INT`, `SOF`, `PIT`, `OUT`).
- A unit follows its value as a label: smaller and grey (`44.4 °C`, `18 km/h`, `10.7 L/s`).
- A value not known yet shows `—` in `FAINT` where a value stands beside its label, without its unit; in a table
  it is left blank.
- Numbers keep the formats of their widgets: lap times to the millisecond (`1:45.345`), gaps to a tenth, fuel to
  a hundredth of a liter, temperatures to a tenth of a degree.

## Spacing

On a 4 px grid, in widget pixels (they grow with `[general] text_size`):

- `PAD`, 8 px: from a widget's edge to its content, left and right, and around a block inside it.
- `GUTTER`, 8 px: between columns, between blocks (the tire corners), and between widgets in the default layout.
- Numbers are right-aligned in their columns, so their digits line up; names and labels are left-aligned.
- A band behind a row (the player's, the current sector's, a class header) is inset by half a `PAD` from the
  panel's edges, so it sits inside the panel's rounded corners.

## Structure

- **Tables** (relative, standings, pit stops, sectors): an optional header line (session context, or the column
  names), a 1 px divider (`TEXT` at 12%, inset by `PAD`), the rows, and an optional footer set off by another
  divider. A break in a list (the standings between a class's leaders and the cars around the player) is a
  divider too.
- **Readouts** (fuel, wind, tires): a label over each value; a table below when there is one (the fuel's
  estimates, with the row names on the left and the column names above).

## Empty states

A widget does not draw what it does not know.

- A chip without a value is not drawn (the iRating of an AI driver).
- A block whose values are all unknown is replaced by one line in `FAINT` saying when it fills: the fuel's
  estimates show "Estimates after a clean lap", the sectors "No sectors yet".

## Shape

Panels, chips, badges, bands, bars, and blocks have corners rounded by 4 px (`RADIUS` in `render.rs`). Lines,
graph traces, and marks that meet edge to edge stay square.

## Legibility

- Backgrounds are 80% opaque by default (`[general] background_opacity = 80`).
- Text has a black shadow one pixel down and right whose alpha is 0.9 times what the panel lets through: none on
  an opaque panel, 0.18 at the default 80, and 0.9 with no background, so text stays readable over a bright scene
  at any background setting.
- Rows are not striped: the player's band, the class headers, and the dividers already set rows apart, and stripes
  would add a third pattern to read past.

## Default layout

For a 1920 x 1080 monitor, 8 px from its edges and at least 8 px between widgets, on the 4 px grid, with nothing
overlapping at any text size (a test checks):

| Widget | Place | Size |
|---|---|---|
| Status | 8, 8 | 300 x 54, growing with the text (450 x 81 at 150%) |
| Standings | 8, 100 | 744 x 560 |
| Sectors | 760, 8 | 400 x 200 |
| Pit stops | 760, 216 | 400 x 280 |
| Input trace | 660, 952 | 600 x 120 |
| Track map | 1456, 8 | 456 x 328 |
| Wind | 1456, 344 | 224 x 168 |
| Tires | 1688, 344 | 224 x 168 |
| Relative | 1456, 520 | 456 x 396 |
| Fuel | 1456, 924 | 456 x 148 |

The right column is 456 px wide so the relative still fits at the largest text size (its smallest width is 300
widget pixels; at 150% it leaves out the rating and license columns, as below 400). On another monitor size, a
widget that would reach past an edge is moved inside it.
