# Overlay data pipeline

`pitot_overlay::pipeline` moves telemetry from the sim's shared memory to the render thread without locks and
without per-tick allocations.

## Threads

| Thread | Wakes on | Work |
|---|---|---|
| `pitot-data` | the data-valid event (60 Hz while driving), at most once per second otherwise | `LiveReader::step`; publishes the tick and an input sample; copies session info when `sessionInfoUpdate` changes; during a sim replay, publishes a capture's frame instead (see below) |
| `pitot-session` | work from the data thread | `SessionParser::parse`, then hands the result to the render thread and the text buffer back to the data thread; searches for a capture when a replay starts |
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

Widgets hide and the render loop can sleep outside `Driving`. While a capture plays during a sim replay, the
values are the capture's, so the state is the one recorded at the replayed moment.

## Sim replays

While the sim plays a replay, its telemetry stays live. A capture recorded on the home PC during an online
practice session shows this: in the first replay frame the cars' lap distances differed from the last live frame
by 0.00014 (one tick of movement) and from the frame recorded at the replayed moment by 0.19 of a lap, and the
player's own values were reset (speed 0, throttle 0, fuel 40). Only these say what the replay shows:

| Variable | Live | Replay |
|---|---|---|
| `IsReplayPlaying` | false | true |
| `ReplaySessionNum` | -1 | the replayed session |
| `ReplaySessionTime` | 0 | the replayed session time, advancing at the replay's speed |

`IsOnTrack` is false during the replay. `WeekendInfo.SimMode` stays `full` and `SubSessionID` is unchanged.

So the overlay shows a [capture](capture-format.md) of the session instead:

1. When a replay starts, the session thread looks for captures (`*.ptc`) in the
   [capture folders](configuration.md#replays), newest first. A capture belongs to the session when the session
   info that opens it has the live session's `SubSessionID`. Offline sessions all report `SubSessionID` 0, so
   there a capture must have the same track and cover the replayed moment. A matching capture is indexed by
   session time (`pitot_irsdk::capture::Seeker`: one pass over the file, 0.05 s for 3.5 minutes of a 72-car
   session on the lab PC) and handed to the data thread. Without one, widgets hide as before, and the search is
   repeated at most every 10 s. Frames recorded while the sim played a replay (`IsReplayPlaying` set) are not
   part of the session, since they hold live values: a capture made while watching a replay, such as a saved
   `.rpy` file, covers nothing and is skipped, and the replay part at the end of a session's capture is left out.
2. Each tick, the data thread reads `ReplaySessionNum` and `ReplaySessionTime` and moves the capture there. The
   frame is copied into the live layout (`pitot_irsdk::live::RowMap`: variables by name, missing ones zero), and
   the session info in effect at that frame is parsed as usual, so widgets cannot tell it from live data.
   - Playing forward (any speed up to 1 s per tick) adds every frame passed to the input trace.
   - Pausing or slow motion between two frames adds nothing.
   - Going back shows the frame there; the trace restarts when the replay plays forward again.
   - A jump forward, a new session, or the start of a replay restarts the trace with the 4.3 s (256 samples)
     before the moment. A restart is a new connection for the render thread, so widgets reset.
3. A moment the capture does not have shows the live values, so widgets hide. A capture that was still being
   recorded when it was indexed is searched again when the replay passes its end.
4. When the replay ends, the live values show again on a new connection, and the live session info is sent
   again. The capture's buffers are freed.

Opening the capture reads at most one chunk at a time, and every buffer is sized for the largest chunk and
session info text while indexing, so playing does not allocate per tick. The decoded chunk (ten seconds of
frames) is the main cost: about 6.5 MiB of private bytes for the 72-car capture, released when the replay ends.

## Measurements

- `cargo test -p pitot-overlay --features alloc-check --test alloc_check` counts allocations on every thread
  while 600 ticks flow through the data thread and the view: 0. The same for 600 ticks of a sim replay played
  from a capture: 0.
- Sim replay on the lab PC (Parsec virtual display, four widgets, release build): a replay stream made from the
  home PC capture (10 s live, then the replay at 1x, paused, back 20 s, 4x, 1/4 speed, then live again) with
  `pitot-bench replay`. The capture was found and shown 0.2 s after the replay started, and live values returned
  on time. 3,352 ticks, none missed, no samples dropped. Overlay CPU 29.6 ms/s while the capture played and 26.9
  to 30.6 ms/s live; private bytes 20.8 MiB while playing and 14.3 MiB live before (15.1 MiB after).
- With `pitot-bench replay` of the public AI race capture on the lab PC: the data thread read 301 ticks in 5 s
  with none missed or torn. The view received 300 consecutive input samples with none dropped, 72-entry per-car
  arrays, and session info for 41 drivers.
