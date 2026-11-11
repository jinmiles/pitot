# Recorder

`pitot-bench record` writes what the sim publishes through SDK shared memory into a
[capture](capture-format.md), so it can be replayed later exactly as it happened.

```text
pitot-bench record [--out <file.ptc>] [--seconds <n>] [--level <n>]
```

- Without `--out`, the capture goes to `captures/<YYYYMMDD-HHMMSS>.ptc`, which git ignores.
- The recorder waits for the sim, and records until `--seconds` pass or Ctrl+C. Both finish the file with its
  index and metadata. A recorder that is killed leaves a capture that stays readable up to its last whole chunk.
- `--level` is the Zstandard level, 3 by default, which compresses a ten-second chunk in about 10 ms. Convert
  later with a higher level for a smaller file.

## What is recorded

- Every data buffer row, whole, with the SDK tick count and the buffer it was read from.
- Session info whenever `sessionInfoUpdate` changes, written before the frame it was seen with.
- Ticks the recorder missed, as gaps with their count, and reads that were torn twice, as a count in the summary.
- Disconnects. When the sim reconnects with the same layout, recording continues in the same file. A new layout,
  such as another car or a session type with other variables, finishes the file and starts the next one with
  `-2`, `-3`, ... before the extension.

## Replays in the overlay

The overlay records sessions itself and plays them while the sim plays a replay (see
[Recording and replays](configuration.md#recording-and-replays)), so `pitot-bench record` is not needed for that.
Captures it writes in the folder that holds `pitot-overlay.exe` (its `captures` folder) are found as well.

## Checking a recording

```text
pitot-bench replay --capture captures/<file>.ptc
```

The summary at the end of a recording lists frames, session info revisions, gaps, missed ticks, disconnects, and
torn reads per file.

## Privacy

Captures hold other drivers' names and IDs in session info. Anonymize them with
[`pitot-bench anonymize`](anonymize.md) before committing or sharing them.
