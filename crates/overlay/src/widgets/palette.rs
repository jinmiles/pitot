//! The widgets' colors: one palette, described in `docs/design-language.md`.
//!
//! Surfaces and text have no hue. A color is used only where it carries a meaning, and each meaning has one color:
//! the player is amber, time gained green, time lost red, a best lap purple, a caution yellow, and the pit road
//! blue. Colors that come from the sim (class and license colors) are drawn as given. Values are `0xRRGGBB`; the
//! alpha is given where they are drawn.

/// A widget's panel. Its alpha is the widget's `opacity`, scaled by `[general] background_opacity`.
pub const SURFACE: u32 = 0x14_1414;
/// A band or block on the panel: a header row, a tire block.
pub const RAISED: u32 = 0x24_2424;

/// The track on the track map, and the dark edge that sets it and the dots off from what is behind.
pub const ROAD: u32 = 0x5E_5E5E;
pub const ROAD_EDGE: u32 = 0x05_0505;

/// Values and names.
pub const TEXT: u32 = 0xF2_F2F2;
/// Labels, units, and secondary values.
pub const DIM: u32 = 0xA3_A3A3;
/// Values not known yet and things switched off.
pub const FAINT: u32 = 0x6B_6B6B;
/// A light chip with dark text, such as the iRating.
pub const CHIP: u32 = 0xE6_E6E6;

/// The alpha of a band of [`TEXT`] that lifts a row without a hue, such as the sector the player is in. Lighter than
/// the panel whatever is behind it, where an opaque gray would be darker than the panel over a bright scene.
pub const LIFT_BAND: f32 = 0.10;

/// The player: their name, number, row band, and marks on the map and dials.
pub const PLAYER: u32 = 0xFF_AA33;
/// The alpha of the band behind the player's row.
pub const PLAYER_BAND: f32 = 0.20;

/// Time or places gained, a personal best, enough fuel, a window that is open.
pub const GAIN: u32 = 0x5C_C98F;
/// Time or places lost, too little fuel, a window that is closing.
pub const LOSS: u32 = 0xEE_6B60;
/// The fastest lap or sector of the session or the class (timing screens' purple).
pub const BEST: u32 = 0xA8_84F3;
/// Something to watch: an out lap, ABS, a strong wind, idle or replay.
pub const CAUTION: u32 = 0xE9_C46A;
/// On the pit road.
pub const PIT: u32 = 0x4C_8DE6;

/// A car a lap or more ahead of the player (iRacing's relative uses the same convention).
pub const LAP_AHEAD: u32 = 0xEE_8B82;
/// A car a lap or more behind the player.
pub const LAP_BEHIND: u32 = 0x86_AEEA;

/// The pedals, in the colors every input overlay uses.
pub const THROTTLE: u32 = GAIN;
pub const BRAKE: u32 = 0xEE_5D52;
pub const CLUTCH: u32 = 0x6F_A8E6;

/// A cold tire, and a wet-weather compound.
pub const COLD: u32 = CLUTCH;

/// Tire compounds, in the colors of their markings: soft red, medium yellow, hard white, intermediate green, wet
/// blue.
pub const COMPOUND_SOFT: u32 = BRAKE;
pub const COMPOUND_MEDIUM: u32 = CAUTION;
pub const COMPOUND_HARD: u32 = CHIP;
pub const COMPOUND_INTER: u32 = GAIN;
pub const COMPOUND_WET: u32 = COLD;

/// Shown for a class or license color the session does not give.
pub const UNKNOWN_CLASS: u32 = 0xFF_FFFF;
pub const UNKNOWN_LICENSE: u32 = 0x80_8080;
