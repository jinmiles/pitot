# Overlay data pipeline

`pitot_overlay::pipeline` moves telemetry from the sim's shared memory to the render thread (the scheduler on
GPUI's thread, see [rendering](overlay-rendering.md#schedule)) without locks and without per-tick allocations.

## Threads

| Thread | Wakes on | Work |
|---|---|---|
| `pitot-data` | the data-valid event (60 Hz while driving), at most once per second otherwise | `LiveReader::step`; publishes the tick and an input sample; copies session info when `sessionInfoUpdate` changes; during a sim replay, publishes a capture's frame instead (see below) |
| `pitot-record` | once a second, while recording | writes the rows the data thread handed over into the capture (see below) |
| `pitot-session` | work from the data thread | `SessionParser::parse`, then hands the result to the render thread and the text buffer back to the data thread; searches for a capture when a replay starts |
| render (caller) | its own schedule, and the wake-ups below | `View::poll` and `View::drain_samples` once per update |

## Handoff

- **Latest tick**: a sequence-locked buffer of atomic words (`SeqBuffer`). The data thread writes the requested
  variables. The render thread copies them into its own `Snapshot` and retries if a write overlapped the copy.
- **Tick time**: `View::tick_time` is `SessionTick` over the connection's tick rate (the header's `tickRate`; a
  capture's own while one plays). Every duration and time difference the widgets and trackers take is on it: lap,
  sector, and gap times, pit lane and standing times, the refueling speed, history sample times, and the checks
  for stale observations or time going back. The sim's own timing (`CarIdxLastLapTime`, the results' `Time`)
  follows the tick count, while `SessionTime` slips by whole ticks against it: it repeats a value for a frame and
  catches up with a double step later (7 times each way in the 10-minute AI race capture). `SessionTime` is read
  only as the session clock: the elapsed time the widgets show, and replay positions (`ReplaySessionTime`, the
  capture and telemetry file lookups), which the sim gives in `SessionTime`.
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

- **Wake-ups**: `View::wake` is the render thread's wake-up (`wake.rs`). Every message the data and session threads
  send sets a reason and wakes it. Each published tick wakes it too while it asked for ticks (`Wake::arm_ticks`, set
  while a shown widget is due by the next tick); each such wake disarms the next. Setting a reason is an atomic or,
  and waking takes the task's waker from an `AtomicWaker` and wakes it, which hands the task to GPUI's executor.

Every shared value is an atomic, so the pipeline has no `unsafe` and no data races.

## Variables

The data thread copies only the variables it was asked for, resolved by name once per connection.
`pipeline::Vars` holds those of the pipeline and the first widgets. A widget that reads others declares them in
its own module with `var_group!` ([adding a widget](overlay-rendering.md#adding-a-widget)), and `Pipeline::start`
registers every widget's group after `Vars`, in the order of the widget list. A name asked for again keeps its
first handle (`VarSet::add`), so it is copied once however many groups name it. `View::var_set` is the whole set,
and `View::resolve::<G>()` gives a group's handles; the render thread resolves each widget's group when it makes the
widget. Every group is registered whether its widget is on or not, so turning a widget on needs no new
connection. A variable the sim does not publish reads as `None`.

## Display state

`View::display` returns:
- `Disconnected` without a connection or before its first tick,
- `Replay` when `IsReplayPlaying` is set: the sim plays a replay and no capture of it shows,
- `Driving` when `IsOnTrack` is set,
- `Idle` otherwise.

Widgets hide and the render loop can sleep outside `Driving`; only the tire widget, which has its own source for
replays (below), also shows in `Replay`. While a capture plays during a sim replay, the values are the
capture's, so the state is the one recorded at the replayed moment.

## Recording

With recording on (the default, see [configuration](configuration.md#recording-and-replays)), the data thread
reads each tick's whole row ahead of the requested variables; only the variables go to the render thread. While
the sim does not play a replay, it copies the row into a buffer from a pool and hands it to the `pitot-record`
thread over a bounded queue, with session info whenever it changes and gaps when ticks were missed. The recorder
wakes once a second, writes what arrived into the capture, and returns the buffers. So recording adds no wake-up
per tick, and nothing allocates per tick once a connection has filled the pool (120 buffers, two seconds of ticks;
a full queue drops rows and records them as a gap).

The overlay writes two-second chunks (Zstandard level 3, built for size): the writer holds a chunk's rows and its
compressed form, about 1 MB each for a full-grid session, and a replay decodes one chunk at a time. Ten-second
chunks, as `pitot-bench` writes, took over 20 MB while recording.

Lab PC (Parsec virtual display, four widgets, release build, the home PC capture of an online practice session
replayed at 1x, `pitot-bench collect` for 60 s after 12 s, two runs each, alternating, same build): overlay CPU
26.49 and 26.45 ms/s without recording, 28.57 and 28.75 ms/s with it (+2.2 ms/s; the busiest second 34 ms/s against
30); private bytes 17.2 and 16.8 MiB without, 23.3 and 22.1 MiB with (+5 to 6 MiB). Each 75 s recording was 3.45 MB,
about 0.17 GB per hour, with 4,498 frames and none missed.

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
   [capture folders](configuration.md#recording-and-replays), newest first. A capture belongs to the session when the session
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
   - While the replay plays (`ReplayPlaySpeed` not 0, at any speed), the input trace gets one sample per tick:
     the frame at the replayed moment, so it scrolls as evenly as live. The sim's replay clock is uneven: in
     the home PC capture it moved 0.8 to 1.4 frames per tick at 1x (190 of 217 ticks moved exactly one), so
     adding the frames passed would add none on some ticks and two on others. At 4x the trace shows every
     fourth frame; in slow motion a frame repeats. A sim without `ReplayPlaySpeed` gets every frame passed.
   - Paused (`ReplayPlaySpeed` 0) adds nothing.
   - Going back shows the frame there; the trace restarts when the replay plays forward again.
   - A jump forward, a new session, or the start of a replay restarts the trace with the 4.3 s (256 samples)
     before the moment. A restart is a new connection for the render thread, so widgets reset.
3. A moment the capture does not have shows the live values, so widgets hide. A capture that was still being
   recorded when it was indexed is searched again when the replay passes its end.
4. When the replay ends, the live values show again on a new connection, and the live session info is sent
   again. The capture's buffers are freed.

### The sim's telemetry files

Live telemetry, and so a capture, has no tire surface temperatures or hot pressures: the sim writes them only to
its own `.ibt` files, while its telemetry recording is on (one file per stint in the car, in
`Documents\iRacing\telemetry` unless [configured](configuration.md#telemetry-files)). For replays, the tire
widget asks the session thread for those files (`View::want_tires`), at most every 10 s and only while the
samples it has do not cover the replayed moment:

- A file belongs to the session by the `SubSessionID` in its session info. An online session takes every
  file of its `SubSessionID`; an offline session (`SubSessionID` 0) takes the newest file of its track that
  covers the moment. Files are listed and identified again only when their time or size changes.
- Each file is read five samples a second (every twelfth record at 60 Hz): `SessionNum`, `SessionTime`, and per
  tire the surface temperature, tread, and hot pressure at the left, middle, and right, kept in 72 bytes per
  sample (about 1.3 MB per hour of driving). A file the sim is still writing (its record count is 0 until it
  closes the file) is read on from the last record read when it has grown.
- The samples reach the render thread as immutable shared segments. The widget looks up the replayed moment by
  binary search: the moment a capture shows while one plays, else `ReplaySessionNum` and `ReplaySessionTime`.
  A moment more than 0.5 s after the last sample before it (between stints) has none.

Opening the capture reads at most one chunk at a time, and every buffer is sized for the largest chunk and
session info text while indexing, so playing does not allocate per tick. The decoded chunk is the main cost:
about 6.5 MiB of private bytes for a 72-car capture in ten-second chunks, about a fifth of that in the overlay's
own two-second chunks, released when the replay ends.

## Measurements

- `cargo test -p pitot-overlay --features alloc-check --test alloc_check` counts allocations on every thread
  while 600 ticks flow through the data thread and the view: 0. The same for 600 ticks of a sim replay played
  from a capture, and for 600 ticks recorded (data, recorder, and render threads): 0.
- Sim replay on the lab PC (Parsec virtual display, four widgets, release build): a replay stream made from the
  home PC capture (10 s live, then the replay at 1x, paused, back 20 s, 4x, 1/4 speed, then live again) with
  `pitot-bench replay`. The capture was found and shown 0.2 s after the replay started, and live values returned
  on time. 3,352 ticks, none missed, no samples dropped. Overlay CPU 29.6 ms/s while the capture played and 26.9
  to 30.6 ms/s live; private bytes 20.8 MiB while playing and 14.3 MiB live before (15.1 MiB after).
- With `pitot-bench replay` of the public AI race capture on the lab PC: the data thread read 301 ticks in 5 s
  with none missed or torn. The view received 300 consecutive input samples with none dropped, 72-entry per-car
  arrays, and session info for 41 drivers.
