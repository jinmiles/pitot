# Capture format

Pitot captures (`.ptc`) hold what the sim published through SDK shared memory, so it can be replayed exactly: the
variable layout, every data buffer row as raw bytes, every session info revision, ticks the recorder missed, and
disconnects. The content follows irDashies `.irdt` v1 tapes; the file adds compression in ten-second chunks, an
index for seeking, checksums per chunk, and a metadata block.

Format version 1. All integers are little-endian. CRC-32 is the IEEE polynomial (as in zip and PNG; `123456789`
gives `0xCBF43926`).

```text
file header      64 bytes
variable table   variable count x 144 bytes
chunk            repeated
index
metadata         JSON
trailer          32 bytes
```

Captures come from the [recorder](recorder.md) or from conversion (`pitot-bench convert`, see
[replay.md](replay.md)). A capture whose recorder stopped early has no index, metadata, or trailer. Its chunks are
still readable in order, up to the last complete one. The reader is `pitot_irsdk::capture::CaptureReader`, so the
overlay can read captures too, and `pitot_irsdk::capture::Seeker` finds frames by session time for
[sim replays](overlay-pipeline.md#sim-replays). The writer is `pitot_irsdk::capture::CaptureWriter`; its metadata
names the program that wrote the capture (`tool`). `pitot-bench` writes ten-second chunks; the overlay's own
recordings use two-second chunks to keep less in memory. Readers take the chunk length from the header.

## File header

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | Magic `PITOTCAP` |
| 8 | 4 | Format version, 1 |
| 12 | 4 | Header size, 64 |
| 16 | 4 | Flags. Bit 0: player car only (no `CarIdx*` data, as converted from `.ibt`) |
| 20 | 4 | Tick rate in Hz (`tickRate` of the SDK header) |
| 24 | 4 | Buffer length: bytes per data buffer row (`bufLen`) |
| 28 | 4 | Variable count (`numVars`) |
| 32 | 4 | Session info capacity: size of the sim's session info area (`sessionInfoLen`) |
| 36 | 4 | SDK version (`ver`) |
| 40 | 8 | Clock frequency of record times, in Hz |
| 48 | 4 | Frames per chunk |
| 52 | 4 | CRC-32 of the variable table |
| 56 | 4 | Reserved, 0 |
| 60 | 4 | CRC-32 of bytes 0 to 59 |

The variable table follows: the SDK's variable headers, 144 bytes each, as they were in shared memory.

## Chunks

A chunk holds the records of one stretch of the capture, normally `frames per chunk` frames (ten seconds), and
can be decoded on its own.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | Magic `CHNK` |
| 4 | 4 | Compressed length |
| 8 | 4 | Uncompressed length |
| 12 | 4 | Records in the chunk |
| 16 | 8 | Number of the chunk's first frame, counted from 0 over the capture |
| 24 | 4 | Frames in the chunk |
| 28 | 8 | Time of the chunk's first record, in clock units since the capture started |
| 36 | 4 | CRC-32 of bytes 0 to 35 |
| 40 | | Compressed records: one Zstandard frame with its content checksum |

The decompressed records follow each other without padding. Each starts with a kind byte and the time since the
previous record (the chunk's start time for the first), as an unsigned LEB128 number of clock units.

| Kind | Record | Fields after kind and time |
|---|---|---|
| 1 | Frame | Tick (`i32`, the SDK `tickCount`), buffer index (`i32`), then the row: buffer length bytes, XOR the previous frame of the same chunk (the first frame of a chunk is stored as is) |
| 2 | Session info | Update (`i32`, `sessionInfoUpdate`), length (`u32`), then the text without its NUL padding |
| 3 | Gap | Missed ticks (`u32`): ticks the recorder did not see between two frames |
| 4 | Disconnect | None: the sim stopped reporting a connection |
| 5 | End | None: the recording ended normally; the last record of the last chunk |
| 6 | Session info, carried | As kind 2. The revision in effect where the chunk starts, repeated so the chunk decodes on its own. Readers going from the start skip it; readers that seek use it. |

A chunk after the first starts with a kind 6 record when a session info revision exists. XOR against the previous
frame turns unchanged bytes into zeros, which compress well; frames are restored by XOR with the frame before.

## Index

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | Magic `INDX` |
| 4 | 4 | Chunk count |
| 8 | 28 each | Per chunk: file offset (`u64`), first frame (`u64`), start time (`u64`), frames (`u32`) |
| | 4 | CRC-32 of the index from its magic to the last entry |

## Metadata

UTF-8 JSON with the tool and version that wrote the capture, the source (`live`, `irdt`, or `ibt`, and the source
file's name and size for conversions), the compression level, and counts: frames, seconds, session info
revisions, gaps, missed ticks, and disconnects. It never contains session info content, so it holds no driver
names. Session info itself does; see [anonymize.md](anonymize.md).

## Trailer

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | Magic `PTCEND\0\0` |
| 8 | 8 | Index offset |
| 16 | 8 | Metadata offset |
| 24 | 4 | Metadata length |
| 28 | 4 | CRC-32 of bytes 0 to 27 |

## Why this layout

Measured on the public ten-minute AI race capture (36,000 frames of 8,611 bytes, 70 session info revisions):
the `.irdt` original is about 2.1 GB per hour. With ten-second chunks and XOR against the previous frame:

| Compression | GB per hour | Compress a chunk | Decompress a chunk |
|---|---|---|---|
| zlib 6 | 0.241 | 65 ms | 6.1 ms |
| zlib 9 | 0.242 | 231 ms | 6.2 ms |
| xz (LZMA) 6 | 0.211 | 522 ms | 35.9 ms |
| LZ4 | 0.306 | 3.4 ms | 3.5 ms |
| Zstandard 3 | 0.227 | 8.5 ms | 3.5 ms |
| Zstandard 9 | 0.225 | 27.5 ms | 3.3 ms |
| Zstandard 19 | 0.216 | 409 ms | 3.2 ms |

Zstandard at level 19 comes within 3% of LZMA and decompresses ten times faster; the replay decompresses one chunk
every ten seconds, so playback costs almost nothing. Level 3 compresses a chunk in under 10 ms, which keeps the
live recorder light; the converter uses level 19. Both levels decompress at the same speed.

The whole capture, with session info and its carried copies, converts from 348.2 MB to 36.9 MB (10.6%, about
0.22 GB per hour) in 26 s on the lab PC, and the converter's check found all 36,000 frames and 70 session info
revisions identical to the `.irdt` original. One hour fits far below the 2 GiB limit of a GitHub release asset,
and the chunk index makes any point of a long capture reachable without reading what comes before it. Replaying
the capture used under 1 ms of CPU per second.
