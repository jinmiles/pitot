# Overlay data pipeline

`pitot_overlay::pipeline` moves telemetry from the sim's shared memory to the render thread without locks and
without per-tick allocations.

## Threads

| Thread | Wakes on | Work |
|---|---|---|
| `pitot-data` | the data-valid event (60 Hz while driving), at most once per second otherwise | `LiveReader::step`; publishes the tick and an input sample; copies session info when `sessionInfoUpdate` changes |
| `pitot-session` | session info text from the data thread | `SessionParser::parse`, then hands the result to the render thread and the text buffer back to the data thread |
| render (caller) | its own schedule | `View::poll` and `View::drain_samples` once per frame |

## Handoff

- **Latest tick**: a sequence-locked buffer of atomic words (`SeqBuffer`). The data thread writes the requested
  variables. The render thread copies them into its own `Snapshot` and retries if a write overlapped the copy.
- **Input samples**: a bounded single-producer, single-consumer ring of atomic words (`SampleRing`, 512 samples,
  about 8.5 s at 60 Hz). Every tick's throttle, brake, clutch, steering, and ABS state is queued, so the input
  trace draws every sample even when a frame is late. Samples carry the number of ticks the reader missed just
  before them, and a full ring counts rejected samples, so lost data is always visible.
- **Connection changes and session info**: sent over a channel. The buffers are created per connection, sized
  from that connection's variables and `CarIdx*` length, so these are the only allocations. Session info that
  arrives for an old connection is ignored.
- **Session info text**: the data thread copies each new text into a buffer the session thread has returned
  after parsing, and the parser keeps its own text buffer, so updates do not allocate buffers the size of the
  text. Only the first updates and the parse itself (its events and the result) allocate.

Every shared value is an atomic, so the pipeline has no `unsafe` and no data races.

## Display state

`View::display` returns:
- `Disconnected` without a connection or before its first tick,
- `Driving` when `IsOnTrack` is set and `IsReplayPlaying` is not,
- `Idle` otherwise.

Widgets hide and the render loop can sleep outside `Driving`.

## Measurements

- `cargo test -p pitot-overlay --features alloc-check --test alloc_check` counts allocations on every thread
  while 600 ticks flow through the data thread and the view: 0.
- With `pitot-bench replay` of the public AI race capture on the lab PC: the data thread read 301 ticks in 5 s
  with none missed or torn. The view received 300 consecutive input samples with none dropped, 72-entry per-car
  arrays, and session info for 41 drivers.
