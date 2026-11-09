//! Values of the `SessionState` and `SessionFlags` telemetry variables, mirroring `irsdk_defines.h`.

/// `SessionState` values (`irsdk_SessionState`).
pub mod session_state {
    pub const INVALID: i32 = 0;
    pub const GET_IN_CAR: i32 = 1;
    pub const WARMUP: i32 = 2;
    pub const PARADE_LAPS: i32 = 3;
    pub const RACING: i32 = 4;
    pub const CHECKERED: i32 = 5;
    pub const COOL_DOWN: i32 = 6;
}

/// Session-wide `SessionFlags` bits (`irsdk_Flags`). `YELLOW` and `YELLOW_WAVING` are local yellows; a
/// full-course yellow is `CAUTION`.
pub mod session_flags {
    pub const CHECKERED: u32 = 0x0000_0001;
    pub const WHITE: u32 = 0x0000_0002;
    pub const GREEN: u32 = 0x0000_0004;
    pub const YELLOW: u32 = 0x0000_0008;
    pub const RED: u32 = 0x0000_0010;
    pub const BLUE: u32 = 0x0000_0020;
    pub const DEBRIS: u32 = 0x0000_0040;
    pub const CROSSED: u32 = 0x0000_0080;
    pub const YELLOW_WAVING: u32 = 0x0000_0100;
    pub const ONE_LAP_TO_GREEN: u32 = 0x0000_0200;
    pub const GREEN_HELD: u32 = 0x0000_0400;
    pub const TEN_TO_GO: u32 = 0x0000_0800;
    pub const FIVE_TO_GO: u32 = 0x0000_1000;
    pub const RANDOM_WAVING: u32 = 0x0000_2000;
    pub const CAUTION: u32 = 0x0000_4000;
    pub const CAUTION_WAVING: u32 = 0x0000_8000;
}
