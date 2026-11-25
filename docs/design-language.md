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
given.

**Not used.** No color only to tell two things apart (such as two rows of estimates) or to decorate: a widget
without a meaning to show is white and grey.
