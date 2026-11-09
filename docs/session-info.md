# Session info

The sim publishes session info as YAML text in shared memory and in `.ibt` files.
`pitot_irsdk::session::parse` extracts the parts the overlay uses into typed structures.

## Extracted data

| Section | Fields |
|---|---|
| `WeekendInfo` | Track ID, names, length, category, event type, official flag, number of car classes, series and session IDs |
| `DriverInfo` | Player and pace car index, fuel capacity, and per car: names, user ID, team, car number, class (ID, name, color, relative speed, estimated lap time), iRating, license and color, incidents, spectator, pace car, and AI flags |
| `SessionInfo` | Per session: number, type, name, laps and time (or unlimited), and result positions |
| `SplitTimeInfo` | Sector numbers and start positions |

Drivers are stored by `CarIdx` in a list sized by the caller, normally the live `CarIdx*` array length. Missing or
unexpected fields are `None` instead of failing the extraction.

## Preparing the text

The sim's text is not strict YAML. Before parsing:

1. **Decode.** The text is Windows-1252 unless `WeekendInfo` declares `Encoding: UTF8`. Recordings checked so far
   declare `Encoding: ISO_8859_1`; bytes 0x80-0x9F are read as Windows-1252, and the five undefined bytes become
   U+FFFD.
2. **Keep only the needed sections.** `WeekendInfo`, `SessionInfo`, `DriverInfo`, and `SplitTimeInfo` are kept;
   setup, camera, radio, and qualifying sections are dropped, so a malformed unused section cannot break parsing.
3. **Remove non-printable characters.**
4. **Quote free text.** `UserName`, `TeamName`, `AbbrevName`, `Initials`, and `DriverSetupName` are written
   unquoted, so names such as `*Star` or `A: B` would otherwise be misread. These values, and any value that
   starts with a comma, are wrapped in double quotes with `"` and `\` escaped.

The prepared text is parsed with `saphyr`.

## Cost

Measured on real recordings and a 64-car variant of a 40-car AI race (release build, median of 30 runs):

| Input | Size | Parse | Allocations |
|---|---|---|---|
| 16 cars | 46.9 KB | 1.7 ms | 5,402 |
| 24 cars | 60.7 KB | 2.3 ms | 7,726 |
| 41 cars | 78.7 KB | 2.8 ms | 9,145 |
| 64 cars | 103.8 KB | 4.0 ms | 12,795 |

The text changed 70 times in a ten-minute live race, so parsing stays off the data thread and runs only when
`sessionInfoUpdate` changes.

## Privacy

Session info contains other drivers' names and customer IDs. Parse errors report a line and column only, never
text, and nothing here logs session info.
