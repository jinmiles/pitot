# Live telemetry reader

`pitot_irsdk::live::LiveReader` reads the sim's shared memory (`Local\IRSDKMemMapFileName`) and waits on its
data-valid event (`Local\IRSDKDataValidEvent`). It is Windows-only and meant for a dedicated data thread that
calls `step` in a loop.

## Connection

- While the sim is not running, a step tries to open the mapping and event at most once per `retry_interval`
  (1 s by default) and returns `NotRunning`.
- A mapping whose header does not report the connected status is treated as not running.
- On connect, the requested variables are looked up by name once and copied into a compact `Snapshot` sized for
  them. A variable that is missing or has an unknown type is unavailable; only features that need it are
  affected.
- Per-car arrays are sized from the variable table (72 entries in a July 2026 live capture), never from a
  constant.
- When the status bit clears, the step returns `Disconnected` and releases the mapping. When the header layout
  changes (for example a different car), the variables are resolved again and the step returns `Connected`.

## Reading ticks

- Each step returns at most one tick. Ticks still held in the sim's rotating buffers are returned oldest first,
  so a late reader catches up without losing samples. `Tick { missed }` reports ticks that were overwritten
  before they could be read.
- A copy is accepted only if the buffer's tick count is the same before and after it. A torn copy is retried
  once; if the retry is also torn, the step returns `Torn` and the snapshot must be ignored.
- If the tick counter goes backwards (a sim restart), reading restarts from the newest buffer.
- Session info is copied on request with the same before-and-after check on `sessionInfoUpdate`. Reserve
  `session_info_capacity()` bytes once so the copy does not allocate.

## Wake-up policy

- While ticks arrive, a step waits on the event with a timeout of two tick periods, so a missed signal costs
  at most a short delay and no data.
- Without a tick for `idle_timeout` (1 s by default), a step waits `idle_timeout`, so an idle sim wakes the
  reader at most once per second.
- If a wait is signaled but finds no new data, the next step sleeps one tick period before waiting again. This
  bounds wake-ups even if the event were left signaled.

Measured with the in-process publisher at 60 Hz: 120 ticks took 121 steps (one per tick plus the final
disconnect), with no idle wakes and no missed ticks.

## Allocation check

`cargo test -p pitot-irsdk --features alloc-check --test alloc_check` installs a counting allocator and checks
that reading 6000 ticks, an idle wait, and a session info copy make no heap allocations after warm-up. CI runs
it on every pull request.

## Publisher

`pitot_irsdk::publish::Publisher` creates a simulated mapping and an auto-reset event for tests and the Tier A
replayer. It refuses to start when either name already exists, so it never writes into a running sim's memory.

## To verify against the sim

These checks need the sim and are done on the home PC:

- Connect, disconnect, sim restart, and session change behave as described above.
- Event semantics: whether the sim's event is manual-reset or auto-reset, and whether it is pulsed. The reader
  works with either, but the replayer should match the sim.
- Whether the status bit clears when the sim exits, and how `tickCount` behaves across session changes.
