# `.ibt` telemetry files

The sim writes an `.ibt` file while the player is in the car. `pitot_irsdk::ibt::IbtReader` reads it.

## Layout

| Part | Location | Notes |
|---|---|---|
| SDK header | offset 0, 112 bytes | Same structure as the live shared-memory header. `numBuf` is 1. |
| Disk sub-header | offset 112, 32 bytes | Session start date and times, lap count, record count. |
| Variable table | `varHeaderOffset`, 144 bytes per variable | Same structure as the live variable table. |
| Session info | `sessionInfoOffset`, `sessionInfoLen` bytes | YAML text, NUL padded. |
| Records | `varBuf[0].bufOffset`, `bufLen` bytes each | One record per tick at `tickRate` (60 Hz). |

Variable offsets inside a record are not aligned to the value size (for example a 4-byte float at offset 522),
so values are decoded byte-wise.

## Behavior observed in real recordings

Checked against two race recordings and, for comparison, one live shared-memory capture.

- **One session info snapshot.** The YAML is written once, when recording starts. `sessionInfoUpdate` is 0 and
  the race results are empty. Updates made during the session are not stored, while the live session info of a
  ten-minute race changed 70 times.
- **Player car only.** There are no `CarIdx*` arrays, so relative and standings data cannot be reconstructed from
  an `.ibt`. Camera, replay, and garage state variables (`Cam*`, `Replay*`, `IsReplayPlaying`, `IsInGarage`) are
  also missing.
- **Disk-only variables.** `Lat`, `Lon`, and `Alt` exist in `.ibt` files but not in live shared memory.
- **Uneven `SessionTime`.** `SessionTick` advances by one per record, but `SessionTime` occasionally advances by
  two ticks (33.3 ms). Pace playback by record index, not by `SessionTime`, and do not assume uniform time steps
  when seeking.
- **Session info encoding.** The YAML is not UTF-8: non-ASCII bytes in driver names decode as Windows-1252.

## Reader behavior

- Records are read on demand into a reusable buffer; sequential reads do not seek or allocate.
- The record count comes from the disk sub-header. A count of zero (a recording that was not closed cleanly)
  falls back to the number of complete records in the file. A count larger than the file holds is an error.
- `seek_session_time` binary-searches for the first record at or after a (`SessionNum`, `SessionTime`) pair and
  assumes records are ordered by session number and then time.

## Privacy

Session info contains other drivers' names and customer IDs. Keep real `.ibt` files in the git-ignored `data/`
directory. Tests use synthetic files built in code.
