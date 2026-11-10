# Anonymizer

Session info in `.ibt` files and captures holds other drivers' names and customer IDs. `pitot-bench anonymize`
replaces them, so a file can become a test fixture or be shared.

```text
pitot-bench anonymize --in <file> --out <file>
```

The input is a capture (`.ptc`) or an `.ibt` file; the output has the same kind. Convert `.irdt` tapes to
captures first.

## What is replaced

In every session info revision:

| Values of | Become |
|---|---|
| `UserName` | `Driver 001`, `Driver 002`, ... |
| `AbbrevName` | `D001`, `D002`, ... |
| `Initials` | `DR` |
| `TeamName` | `Team 001`, `Team 002`, ... |
| `UserID`, `TeamID`, `DriverUserID`, `CustID` | 100001, 100002, ... (0 and -1 stay) |
| `SessionID`, `SubSessionID`, `LeagueID` | 1, 2, ... (0 and -1 stay) |
| `DriverSetupName` | `setup.sto` |

- The same original always gets the same pseudonym within a file, so a driver keeps their name over every
  revision, and a user ID shared by `UserID` and `TeamID` maps to the same number.
- Names of three characters or more are also replaced wherever else they appear, for example in private radio
  channel names.
- Pace cars keep their name.
- Everything else is copied byte for byte: telemetry values, record times, other session info fields such as car
  numbers, classes, iRating, and licenses.

The sim's session info is not strict YAML (free text is unquoted), so the anonymizer does not parse and re-emit
it. It rewrites the values of the keys above line by line and leaves every other byte as it was.

## Check

After writing, the anonymizer reads the output back and fails if any original name or ID is still present. It
prints only counts (drivers, teams, IDs, sessions), never names.

## Limits

- Car, helmet, and suit paint designs, car numbers, clubs, and ratings stay. Together they can hint at who a
  driver is in a small field; review a fixture before publishing it.
- `.ibt` files must have the sim's layout: headers, variable headers, session info, then the records.
