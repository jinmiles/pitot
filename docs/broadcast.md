# Broadcast commands

`pitot_irsdk::broadcast` controls the sim the way the SDK does: it registers the `IRSDK_BROADCASTMSG` window
message and posts it to all top-level windows with `SendNotifyMessageW`. The Tier B replay controller uses it to
render the same scene on every run.

## Commands

| Command | Message | `wParam` high word | `lParam` |
|---|---|---|---|
| `Camera` with a race position, incident, leader, or exiting focus | 0 | position, or -3, -2, -1 | group, camera |
| `Camera` with a car number | 1 | padded car number | group, camera |
| `ReplaySpeed` | 3 | speed (0 pauses, negative rewinds) | 1 for slow motion |
| `ReplayPosition` | 4 | origin (begin, current, end) | frame |
| `ReplaySearch` | 5 | search mode (start, end, session, lap, frame, incident) | 0 |
| `Telemetry` | 10 | stop, start, restart | 0 |
| `ReplaySessionTime` | 12 | session number | session time in ms |

`wParam` is `MAKELONG(message, var1)`, and two-value parameters are packed as `MAKELONG(low, high)`. Negative
16-bit values are sent as two's complement words.

Car numbers keep their leading zeros. As in the SDK, a number with leading zeros is sent as
`number + 1000 * digits`, so `07` is 2007 and `7` is 7.

## Validation

Packing is unit-tested against the SDK's encoding on any platform. Tests only register the message: sending
would post to every window on the machine.

Effect on the sim (seek, speed, camera lock, telemetry start and stop) has to be checked on the home PC.
