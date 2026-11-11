# Session info

The sim publishes session info as YAML text in shared memory and in `.ibt` files.
`pitot_irsdk::session::parse` extracts the parts the overlay uses into typed structures.

## Extracted data

| Section | Fields |
|---|---|
| `WeekendInfo` | Track ID, names, length, category, event type, official flag, number of car classes, series and session IDs |
| `DriverInfo` | Player and pace car index, fuel capacity, the player's tire compounds (`DriverTires`: index and type), and per car: names, user ID, team, car number, car name and short name, class (ID, name, color, relative speed, estimated lap time), iRating, license and color, incidents, spectator, pace car, and AI flags |
| `SessionInfo` | Per session: number, type, name, laps and time (or unlimited), and result positions |
| `SplitTimeInfo` | Sector numbers and start positions |
| `QualifyResultsInfo` | The qualifying result positions a race starts from, with the same fields as session results |

Drivers are stored by `CarIdx` in a list sized by the caller, normally the live `CarIdx*` array length. Missing or
unexpected fields are `None` instead of failing the extraction.

## Preparing the text

The sim's text is not strict YAML. Before parsing:

1. **Decode.** The text is Windows-1252 unless `WeekendInfo` declares `Encoding: UTF8`. Recordings checked so far
   declare `Encoding: ISO_8859_1`; bytes 0x80-0x9F are read as Windows-1252, and the five undefined bytes become
   U+FFFD.
2. **Keep only the needed sections.** `WeekendInfo`, `SessionInfo`, `DriverInfo`, `SplitTimeInfo`, and
   `QualifyResultsInfo` are kept; setup, camera, and radio sections are dropped, so a malformed unused section
   cannot break parsing.
3. **Remove non-printable characters.**
4. **Quote free text.** `UserName`, `TeamName`, `AbbrevName`, `Initials`, and `DriverSetupName` are written
   unquoted, so names such as `*Star` or `A: B` would otherwise be misread. These values, and any value that
   starts with a comma, are wrapped in double quotes with `"` and `\` escaped.

## Reading the fields

The prepared text is read with `saphyr-parser` as a stream of events, and each wanted field is stored as it
arrives. No document tree is built: a tree of the whole text is about 800 KB of short-lived allocations for
41 cars, and building one on every update fragmented the process heap so that the overlay's committed memory
kept rising for about two hours of a race before it leveled off. Scalars are typed with `saphyr`'s core schema
rules, the same as before. Only the first document is read.

`SessionParser` keeps the prepared text's buffer between updates, and the raw text is decoded into it line by
line, so an update allocates no buffer the size of the text. The overlay parses every update with one
`SessionParser`.

## Cost

Measured on real recordings and on the last revision of a 41-car AI race widened to 64 cars (release build,
median of 30 runs; peak is the most heap memory live at once during the parse, for a `SessionParser` that has
parsed the text before, as in the overlay):

| Input | Size | Parse | Allocations | Peak |
|---|---|---|---|---|
| 16 cars | 46.9 KB | 1.1 ms | 3,247 | 74 KB |
| 24 cars | 60.7 KB | 1.6 ms | 4,636 | 81 KB |
| 41 cars | 78.7 KB | 1.9 ms | 5,556 | 81 KB |
| 64 cars | 110.8 KB | 2.9 ms | 8,462 | 82 KB |

A single `session::parse` also allocates the prepared text, which raises the peak to 121 KB for 16 cars and
193 KB for 64.

The text changed 70 times in a ten-minute live race, so parsing stays off the data thread and runs only when
`sessionInfoUpdate` changes.

## Privacy

Session info contains other drivers' names and customer IDs. Parse errors report a line and column only, never
text, and nothing here logs session info.
