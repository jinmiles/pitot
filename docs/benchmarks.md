# Benchmark plans

Plans for the [runner](runner.md) live in `benchmarks/`, with the configurations they put in place:

| Plan | Scenario | Replay | Apps | Runs |
|---|---|---|---|---|
| `benchmarks/tier-a/race.toml` | `tier-a-race` | Public ten-minute AI race | Pitot input trace only; Pitot four widgets | 5 each, 6 baselines, 60 s warm-up, 180 s measured |
| `benchmarks/tier-a/idle.toml` | `tier-a-idle` | None (sim not running) | Pitot four widgets | 3, 4 baselines, 30 s warm-up, 120 s measured |
| `benchmarks/tier-a/long.toml` | `tier-a-long` | Public AI race, looped | Pitot four widgets | 1, 2 baselines, 60 s warm-up, 60 min measured |

- `benchmarks/pitot/four.toml` is the reference set of the project plan: input trace, relative, standings, and
  fuel at their default positions and update rates, with every other widget (the status panel, the track map)
  off. `inputs.toml` has the input trace alone. Every field is spelled out, so a change of defaults does not change
  a benchmark; a new widget is added to both files as disabled.
- The race replay needs the capture under `data/irdashies/` (see [replay.md](replay.md)) and a release build:
  `cargo build --release -p pitot-overlay -p pitot-bench`.
- Each plan carries the budget of the project plan in its `[budget]` table, so the report marks every app
  against it.

```text
target/release/pitot-bench run benchmarks/tier-a/race.toml
target/release/pitot-bench report bench-results/tier-a-race/series/<series-id>.json
```

## Local copies

The configurations put widgets on the primary monitor. To run on another monitor, for example a virtual display
so the overlay does not cover the screen, copy the configuration and the plan to `*.local.toml` files, which git
ignores, add `monitor = <n>` to each widget, and point the copied plan at the copied configuration. Keep the
monitor's resolution and scaling the same as the primary monitor's, and note the change with the results.

## Before an official run

- Run on the machine the official results come from, with the fixed conditions of the project plan: power plan,
  driver, background applications, and resolution.
- Close every overlay the runner warns about.
- Run the whole plan in one series, and report it with `pitot-bench report`.
