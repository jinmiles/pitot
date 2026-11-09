//! Broadcast messages that control the sim: camera focus, replay playback, and telemetry recording.
//!
//! The SDK sends a registered window message to all top-level windows with `wParam = MAKELONG(message, var1)`
//! and `lParam = var2` or `MAKELONG(var2, var3)`. [`Command::pack`] builds those parameters and is platform
//! independent; [`Broadcaster`] sends them on Windows.

use std::fmt;

/// Name of the registered window message.
pub const BROADCAST_MESSAGE_NAME: &str = "IRSDK_BROADCASTMSG";

/// Errors from packing or sending a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastError {
    /// A parameter does not fit the message.
    OutOfRange { field: &'static str },
    /// A Win32 call failed with this HRESULT.
    Os { call: &'static str, code: i32 },
}

impl fmt::Display for BroadcastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange { field } => write!(f, "`{field}` is out of range"),
            Self::Os { call, code } => write!(f, "{call} failed with HRESULT {code:#010x}"),
        }
    }
}

impl std::error::Error for BroadcastError {}

/// Message numbers (`irsdk_BroadcastMsg`).
mod msg {
    pub const CAM_SWITCH_POS: u16 = 0;
    pub const CAM_SWITCH_NUM: u16 = 1;
    pub const REPLAY_SET_PLAY_SPEED: u16 = 3;
    pub const REPLAY_SET_PLAY_POSITION: u16 = 4;
    pub const REPLAY_SEARCH: u16 = 5;
    pub const TELEM_COMMAND: u16 = 10;
    pub const REPLAY_SEARCH_SESSION_TIME: u16 = 12;
}

/// A car number as shown on the car, where leading zeros are significant (`07` differs from `7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarNumber {
    number: u16,
    digits: u8,
}

impl CarNumber {
    /// Parses one to three digits, keeping leading zeros.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() || text.len() > 3 || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            number: text.parse().ok()?,
            digits: u8::try_from(text.len()).ok()?,
        })
    }

    /// The value the sim expects (`irsdk_padCarNum`): the number itself, or with leading zeros
    /// `number + 1000 * digits`.
    pub fn sdk_value(&self) -> u16 {
        let natural = match self.number {
            0..=9 => 1,
            10..=99 => 2,
            _ => 3,
        };
        if self.digits > natural {
            self.number + 1000 * u16::from(self.digits)
        } else {
            self.number
        }
    }
}

/// What the camera follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The car at this race position.
    Position(u16),
    /// The car with this number.
    Car(CarNumber),
    Incident,
    Leader,
    Exiting,
}

/// Where a play position is measured from (`irsdk_RpyPosMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayOrigin {
    Begin,
    Current,
    End,
}

/// Replay jumps (`irsdk_RpySrchMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaySearch {
    ToStart,
    ToEnd,
    PrevSession,
    NextSession,
    PrevLap,
    NextLap,
    PrevFrame,
    NextFrame,
    PrevIncident,
    NextIncident,
}

/// Telemetry recording control (`irsdk_TelemCommandMode`). Recording happens only while in the car.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Telemetry {
    Stop,
    Start,
    /// Writes the current file and starts a new one.
    Restart,
}

/// A command for the sim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Points the camera at `focus` using camera `camera` of camera group `group`.
    Camera {
        focus: Focus,
        group: u16,
        camera: u16,
    },
    /// Plays the replay at `speed` (0 pauses, negative rewinds). With `slow_motion`, the rate is `1 / speed`.
    ReplaySpeed {
        speed: i16,
        slow_motion: bool,
    },
    /// Moves the replay to `frame` relative to `origin`.
    ReplayPosition {
        origin: ReplayOrigin,
        frame: i32,
    },
    ReplaySearch(ReplaySearch),
    /// Moves the replay to `session_time_ms` within session `session_num`.
    ReplaySessionTime {
        session_num: u16,
        session_time_ms: i32,
    },
    Telemetry(Telemetry),
}

/// Window message parameters for one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packed {
    pub wparam: usize,
    pub lparam: isize,
}

const fn make_long(low: u16, high: u16) -> u32 {
    (low as u32) | ((high as u32) << 16)
}

/// `var1` and the low and high words of `var2` are 16-bit; negative values are sent as two's complement words.
const fn word(value: i16) -> u16 {
    value as u16
}

impl Command {
    /// Packs the command into window message parameters exactly as the SDK does.
    pub fn pack(&self) -> Result<Packed, BroadcastError> {
        let (message, var1, lparam): (u16, u16, i32) = match *self {
            Self::Camera {
                focus,
                group,
                camera,
            } => {
                let (message, target) = match focus {
                    Focus::Position(position) => (msg::CAM_SWITCH_POS, position),
                    Focus::Car(car) => (msg::CAM_SWITCH_NUM, car.sdk_value()),
                    Focus::Incident => (msg::CAM_SWITCH_POS, word(-3)),
                    Focus::Leader => (msg::CAM_SWITCH_POS, word(-2)),
                    Focus::Exiting => (msg::CAM_SWITCH_POS, word(-1)),
                };
                (message, target, make_long(group, camera) as i32)
            }
            Self::ReplaySpeed { speed, slow_motion } => (
                msg::REPLAY_SET_PLAY_SPEED,
                word(speed),
                i32::from(slow_motion),
            ),
            Self::ReplayPosition { origin, frame } => {
                if frame < 0 {
                    return Err(BroadcastError::OutOfRange { field: "frame" });
                }
                let origin = match origin {
                    ReplayOrigin::Begin => 0,
                    ReplayOrigin::Current => 1,
                    ReplayOrigin::End => 2,
                };
                (msg::REPLAY_SET_PLAY_POSITION, origin, frame)
            }
            Self::ReplaySearch(search) => (msg::REPLAY_SEARCH, search as u16, 0),
            Self::ReplaySessionTime {
                session_num,
                session_time_ms,
            } => {
                if session_time_ms < 0 {
                    return Err(BroadcastError::OutOfRange {
                        field: "session_time_ms",
                    });
                }
                (
                    msg::REPLAY_SEARCH_SESSION_TIME,
                    session_num,
                    session_time_ms,
                )
            }
            Self::Telemetry(mode) => (msg::TELEM_COMMAND, mode as u16, 0),
        };
        Ok(Packed {
            wparam: make_long(message, var1) as usize,
            lparam: lparam as isize,
        })
    }
}

#[cfg(windows)]
pub use sender::Broadcaster;

#[cfg(windows)]
mod sender {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
    };
    use windows::core::HSTRING;

    use super::{BROADCAST_MESSAGE_NAME, BroadcastError, Command};

    /// Sends commands to the sim through the registered broadcast window message.
    #[derive(Debug, Clone, Copy)]
    pub struct Broadcaster {
        message: u32,
    }

    impl Broadcaster {
        /// Registers the broadcast message. Works whether or not the sim is running.
        pub fn new() -> Result<Self, BroadcastError> {
            let name = HSTRING::from(BROADCAST_MESSAGE_NAME);
            // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
            let message = unsafe { RegisterWindowMessageW(&name) };
            if message == 0 {
                let code = windows::core::Error::from_thread().code().0;
                return Err(BroadcastError::Os {
                    call: "RegisterWindowMessageW",
                    code,
                });
            }
            Ok(Self { message })
        }

        /// Registered message number.
        pub fn message(&self) -> u32 {
            self.message
        }

        /// Posts `command` to all top-level windows without waiting for them to process it.
        pub fn send(&self, command: &Command) -> Result<(), BroadcastError> {
            let packed = command.pack()?;
            // SAFETY: HWND_BROADCAST with a registered message id and plain integer parameters; the call does
            // not retain pointers and returns without waiting for receivers.
            unsafe {
                SendNotifyMessageW(
                    HWND_BROADCAST,
                    self.message,
                    WPARAM(packed.wparam),
                    LPARAM(packed.lparam),
                )
            }
            .map_err(|e| BroadcastError::Os {
                call: "SendNotifyMessageW",
                code: e.code().0,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(command: Command) -> (usize, isize) {
        let packed = command.pack().expect("packs");
        (packed.wparam, packed.lparam)
    }

    fn car(text: &str) -> CarNumber {
        CarNumber::parse(text).expect("valid car number")
    }

    #[test]
    fn car_numbers_follow_sdk_padding() {
        assert_eq!(car("7").sdk_value(), 7);
        assert_eq!(car("07").sdk_value(), 2007);
        assert_eq!(car("007").sdk_value(), 3007);
        assert_eq!(car("42").sdk_value(), 42);
        assert_eq!(car("042").sdk_value(), 3042);
        assert_eq!(car("100").sdk_value(), 100);
        assert_eq!(car("0").sdk_value(), 0);
        assert_eq!(car("00").sdk_value(), 2000);
        assert_eq!(CarNumber::parse("1000"), None);
        assert_eq!(CarNumber::parse("7a"), None);
        assert_eq!(CarNumber::parse(""), None);
    }

    #[test]
    fn packs_camera_commands() {
        let at = |focus| Command::Camera {
            focus,
            group: 3,
            camera: 1,
        };
        assert_eq!(pack(at(Focus::Position(1))), (0x0001_0000, 0x0001_0003));
        assert_eq!(pack(at(Focus::Car(car("07")))), (0x07D7_0001, 0x0001_0003));
        assert_eq!(pack(at(Focus::Incident)), (0xFFFD_0000, 0x0001_0003));
        assert_eq!(pack(at(Focus::Leader)), (0xFFFE_0000, 0x0001_0003));
        assert_eq!(pack(at(Focus::Exiting)), (0xFFFF_0000, 0x0001_0003));
    }

    #[test]
    fn packs_replay_commands() {
        let speed = |speed, slow_motion| Command::ReplaySpeed { speed, slow_motion };
        assert_eq!(pack(speed(1, false)), (0x0001_0003, 0), "1x");
        assert_eq!(pack(speed(0, false)), (0x0000_0003, 0), "pause");
        assert_eq!(pack(speed(-2, false)), (0xFFFE_0003, 0), "rewind");
        assert_eq!(pack(speed(4, true)), (0x0004_0003, 1), "quarter speed");
        let position = Command::ReplayPosition {
            origin: ReplayOrigin::Begin,
            frame: 123_456,
        };
        assert_eq!(
            pack(position),
            (0x0000_0004, 123_456),
            "frame uses the full lParam"
        );
        let end = Command::ReplayPosition {
            origin: ReplayOrigin::End,
            frame: 0,
        };
        assert_eq!(pack(end), (0x0002_0004, 0));
        assert_eq!(
            pack(Command::ReplaySearch(ReplaySearch::NextLap)),
            (0x0005_0005, 0)
        );
        assert_eq!(
            pack(Command::ReplaySearch(ReplaySearch::NextIncident)),
            (0x0009_0005, 0)
        );
        let session_time = Command::ReplaySessionTime {
            session_num: 2,
            session_time_ms: 90_500,
        };
        assert_eq!(pack(session_time), (0x0002_000C, 90_500));
    }

    #[test]
    fn packs_telemetry_commands() {
        assert_eq!(pack(Command::Telemetry(Telemetry::Stop)), (0x0000_000A, 0));
        assert_eq!(pack(Command::Telemetry(Telemetry::Start)), (0x0001_000A, 0));
        assert_eq!(
            pack(Command::Telemetry(Telemetry::Restart)),
            (0x0002_000A, 0)
        );
    }

    #[test]
    fn rejects_negative_positions_and_times() {
        let frame = Command::ReplayPosition {
            origin: ReplayOrigin::Current,
            frame: -1,
        };
        assert_eq!(
            frame.pack(),
            Err(BroadcastError::OutOfRange { field: "frame" })
        );
        let time = Command::ReplaySessionTime {
            session_num: 0,
            session_time_ms: -5,
        };
        assert_eq!(
            time.pack(),
            Err(BroadcastError::OutOfRange {
                field: "session_time_ms"
            })
        );
    }

    #[test]
    fn camera_group_and_camera_fill_both_words() {
        let command = Command::Camera {
            focus: Focus::Position(64),
            group: 0xFFFF,
            camera: 0x8000,
        };
        let (wparam, lparam) = pack(command);
        assert_eq!(wparam, 0x0040_0000);
        assert_eq!(
            lparam, 0x8000_FFFFu32 as i32 as isize,
            "sign-extended like the SDK's int cast"
        );
    }

    #[cfg(windows)]
    #[test]
    fn registers_the_broadcast_message() {
        // Registration only; sending would post to every window on the machine.
        let broadcaster = Broadcaster::new().expect("registers");
        assert!(
            broadcaster.message() >= 0xC000,
            "registered messages are in 0xC000..=0xFFFF"
        );
        assert_eq!(
            Broadcaster::new().map(|b| b.message()),
            Ok(broadcaster.message())
        );
    }
}
