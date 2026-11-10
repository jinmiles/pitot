# Replaying recordings

`pitot-bench replay` publishes a recording into the sim's shared memory (`Local\IRSDKMemMapFileName`) and
signals its data-valid event (`Local\IRSDKDataValidEvent`), so an overlay connects exactly as it would to the
sim. It runs on a machine without the sim installed.

```text
pitot-bench replay (--irdt <file> | --ibt <file> | --capture <file>) [--loop] [--speed <factor>] [--seconds <n>]
```

| Option | Effect |
|---|---|
| `--irdt <file>` | irDashies `.irdt` v1 tape: a raw capture of the live shared memory, including `CarIdx*` data and every session info revision |
| `--ibt <file>` | `.ibt` telemetry file: player car only and one session info snapshot, so per-car widgets have no data |
| `--capture <file>` | Pitot capture (`.ptc`, see [capture-format.md](capture-format.md)): the same content as `.irdt` in about a tenth of the size |
| `--loop` | Start over at the end. Readers see a disconnect and reconnect, so they reset their state. |
| `--speed <factor>` | Playback speed; 1 is the recorded rate |
| `--seconds <n>` | Stop after `n` seconds of wall time |

## Behavior

- Frames are published at the recorded tick rate on an absolute schedule: a late frame is published at once and
  does not delay the following ones. The exit summary counts frames that were more than one period late.
- Frame bytes are published unchanged, in recorded order, and session info revisions appear where they were
  recorded. Gap records (ticks the recorder missed) are skipped.
- `.irdt` record checksums and capture chunk checksums are verified while reading; a damaged record stops the
  replay with an error.
- The status bit is set before the first frame and cleared at the end, at recorded disconnects, and at each loop
  boundary.
- If the shared-memory name already exists, because the sim or another replay is running, the replay refuses to
  start, so it never writes into a running sim's memory.

## Example

The public ten-minute AI race capture from irDashies (40 cars, several classes, race start) works as overlay
input. Its SHA-256 is `6e23e8abab49c1a0ae7c8f6d1f298d31145a6189d3029b8ca540aad56a22b168`. Keep it under `data/`,
which is git-ignored.

```text
cargo run -p pitot-bench --release -- replay --irdt data/irdashies/ai-race-10min.irdt --loop
```

Measured on the lab PC: 481 frames in 8.0 s with no late frames. A reader in another process saw every tick in
order (no missed or torn reads), 72-entry per-car arrays with 41 cars on track, and session info that parsed to
41 drivers.

## Limits

- The data-valid event is auto-reset. Whether the sim uses auto-reset or manual-reset (pulsed) events is still to
  be checked on a machine with the sim; it matters when several processes wait on the event at once.
- Captures come from the [recorder](recorder.md) or from conversion:

```text
pitot-bench convert (--irdt <file> | --ibt <file>) --out <file.ptc> [--level <n>]
```

  The converter keeps every `.irdt` record's time, tick, buffer index, session info update number, and gap, uses
  Zstandard level 19 unless `--level` says otherwise, and then replays the capture against its source to check
  that every frame and session info revision is identical.
