//! Moves telemetry from the sim to the render thread.
//!
//! - The data thread runs a `LiveReader`. Each tick it publishes the requested variables into a [`SeqBuffer`]
//!   and pushes an input sample into a [`SampleRing`], so the input trace gets every sample even when a frame
//!   is late. Neither step locks or allocates.
//! - When `sessionInfoUpdate` changes, the data thread copies the text and hands it to the session thread,
//!   which parses it and passes the result to the render thread.
//! - The render thread owns a [`View`] and calls [`View::poll`] once per frame.
//!
//! Buffers are created per connection, so per-car arrays are sized from the sim's `CarIdx*` length. They reach
//! the render thread over a channel only when the connection changes.
//!
//! While the sim plays a replay, the data thread publishes a capture of the session at the replayed moment
//! instead of the live values (see `replay`). Each restart of the capture's input trace, and each switch
//! between capture and live values, hands the render thread a new connection, so widgets reset.

#[cfg(windows)]
mod record;
#[cfg(windows)]
mod replay;
mod vars;
mod wire;

pub use vars::Vars;
pub use wire::{InputSample, SampleRing, SeqBuffer};

#[cfg(windows)]
pub use record::RecordingConfig;
#[cfg(windows)]
pub use threads::{Pipeline, PipelineStats, Source, View, ViewChanges};

/// Whether widgets should show and the render loop should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// No sim connection.
    Disconnected,
    /// Connected, but the player is not driving (garage, menus, spectating, or a replay without a capture):
    /// widgets hide.
    Idle,
    /// The player is in the car on track; inputs are the driver's own.
    Driving,
}

/// Samples the ring holds: about eight seconds at 60 Hz, far more than one late frame.
pub const SAMPLE_CAPACITY: usize = 512;

/// Readies the session info buffer for a new connection: room for the whole session info `area`, so copies
/// during the connection do not allocate. The buffer still holds the previous connection's text, so reserving
/// without clearing it first would grow the buffer to twice the area.
fn ready_session_buffer(buffer: &mut Vec<u8>, area: usize) {
    buffer.clear();
    buffer.reserve_exact(area);
}

#[cfg(windows)]
mod threads {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Weak};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use pitot_irsdk::capture::{CaptureLayout, Moment};
    use pitot_irsdk::live::{LiveReader, ReaderConfig, SdkNames, Snapshot, Step, VarSet};
    use pitot_irsdk::session::{SessionInfo, SessionParser};
    use tracing::info;

    use super::record::{Recorder, RecordingConfig};
    use super::replay::{self, CaptureCache, Found, Plan, Player, SessionIds};
    use super::{
        Display, InputSample, SAMPLE_CAPACITY, SampleRing, SeqBuffer, Vars, ready_session_buffer,
    };

    /// Least time between two searches for a capture during one replay.
    const SEARCH_INTERVAL: Duration = Duration::from_secs(10);

    /// Where a connection's values come from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Source {
        /// The sim, as it publishes them.
        Sim,
        /// The sim connection again, after a capture played during a replay.
        SimAgain,
        /// A capture of the session, at the moment the sim's replay shows. `first` marks the first connection
        /// of a replay; later ones restart the trace after a jump.
        Capture { first: bool },
    }

    /// Buffers for one connection, shared by the data thread (writer) and the render thread (reader).
    #[derive(Debug)]
    struct Connection {
        /// Layout of the requested variables; cloned by the render thread on connect.
        template: Snapshot,
        latest: SeqBuffer,
        samples: SampleRing,
        car_count: usize,
        source: Source,
        /// While recording, every snapshot starts with the tick's whole row for the recorder; only the requested
        /// variables after it go to the render thread.
        row_len: usize,
    }

    impl Connection {
        fn new(template: &Snapshot, car_count: usize, source: Source, row_len: usize) -> Arc<Self> {
            Arc::new(Self {
                template: template.clone(),
                latest: SeqBuffer::new(template.bytes().len().saturating_sub(row_len)),
                samples: SampleRing::new(SAMPLE_CAPACITY),
                car_count,
                source,
                row_len,
            })
        }

        /// The part of a snapshot the render thread gets.
        fn publish(&self, snapshot: &Snapshot) {
            let bytes = snapshot.bytes();
            self.latest
                .write(snapshot.tick(), &bytes[self.row_len.min(bytes.len())..]);
        }
    }

    enum Message {
        Connected(Arc<Connection>),
        Disconnected,
        Session(Weak<Connection>, Arc<SessionInfo>),
    }

    /// Work for the session thread.
    enum Job {
        /// Session info text to parse for a connection. Live text identifies the session for capture searches.
        Parse {
            connection: Weak<Connection>,
            text: Vec<u8>,
            live: bool,
        },
        /// Look for a capture of the live session that covers the replayed moment.
        Find { at: Moment },
    }

    /// Counters for diagnostics and tests.
    #[derive(Debug, Default)]
    pub struct PipelineStats {
        pub ticks: AtomicU64,
        pub missed: AtomicU64,
        pub torn: AtomicU64,
        pub errors: AtomicU64,
        pub session_parses: AtomicU64,
        pub session_errors: AtomicU64,
    }

    /// The data and session threads. Dropping it stops them.
    #[derive(Debug)]
    pub struct Pipeline {
        stop: Arc<AtomicBool>,
        stats: Arc<PipelineStats>,
        data: Option<JoinHandle<()>>,
        session: Option<JoinHandle<()>>,
    }

    impl Pipeline {
        /// Starts reading from `names` and returns the pipeline and the render thread's view. During replays,
        /// captures of the session are looked up in the folders `captures`. With `recording`, sessions are
        /// recorded while the player drives.
        pub fn start(
            names: SdkNames,
            config: ReaderConfig,
            captures: Vec<PathBuf>,
            recording: Option<RecordingConfig>,
        ) -> std::io::Result<(Self, View)> {
            // A recording reads each tick's whole row, ahead of the requested variables.
            let mut set = if recording.is_some() {
                VarSet::whole_row()
            } else {
                VarSet::new()
            };
            let vars = Vars::register(&mut set);
            let recorder = recording.map(Recorder::start).transpose()?;
            let stop = Arc::new(AtomicBool::new(false));
            let stats = Arc::new(PipelineStats::default());
            let (to_render, from_threads) = mpsc::channel();
            let (to_session, session_jobs) = mpsc::channel::<Job>();
            // Text buffers go back to the data thread once parsed, so updates reuse them instead of allocating.
            let (recycle, recycled) = mpsc::channel::<Vec<u8>>();
            let (to_data, found) = mpsc::channel::<Option<Found>>();

            let session = {
                let to_render = to_render.clone();
                let stats = Arc::clone(&stats);
                thread::Builder::new()
                    .name("pitot-session".into())
                    .spawn(move || {
                        session_loop(
                            &session_jobs,
                            &to_render,
                            &recycle,
                            &to_data,
                            &stats,
                            &captures,
                        );
                    })?
            };
            let data = {
                let stop = Arc::clone(&stop);
                let stats = Arc::clone(&stats);
                thread::Builder::new()
                    .name("pitot-data".into())
                    .spawn(move || {
                        let reader = LiveReader::new(names, set, config);
                        let channels = DataChannels {
                            to_render,
                            to_session,
                            recycled,
                            found,
                        };
                        data_loop(reader, vars, &stop, &stats, &channels, recorder);
                    })?
            };
            let view = View {
                vars,
                messages: from_threads,
                connection: None,
                snapshot: Snapshot::new(),
                session: None,
                version: 0,
                stats: Arc::clone(&stats),
                dropped_before: 0,
            };
            Ok((
                Self {
                    stop,
                    stats,
                    data: Some(data),
                    session: Some(session),
                },
                view,
            ))
        }

        pub fn stats(&self) -> &PipelineStats {
            &self.stats
        }
    }

    impl Drop for Pipeline {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            // The data thread notices within one reader timeout; the session thread ends when its sender drops.
            for handle in [self.data.take(), self.session.take()]
                .into_iter()
                .flatten()
            {
                let _ = handle.join();
            }
        }
    }

    /// Parses session info and searches for captures. Blocks until the data thread sends work, and ends when it
    /// exits.
    fn session_loop(
        jobs: &Receiver<Job>,
        to_render: &Sender<Message>,
        recycle: &Sender<Vec<u8>>,
        to_data: &Sender<Option<Found>>,
        stats: &PipelineStats,
        captures: &[PathBuf],
    ) {
        let mut parser = SessionParser::new();
        let mut cache = CaptureCache::default();
        let mut live_ids: Option<SessionIds> = None;
        while let Ok(job) = jobs.recv() {
            match job {
                Job::Parse {
                    connection,
                    text,
                    live,
                } => {
                    let car_count = connection.upgrade().map(|c| c.car_count);
                    let parsed = car_count.map(|count| parser.parse(&text, count));
                    // Fails only once the data thread has exited.
                    let _ = recycle.send(text);
                    match parsed {
                        None => {}
                        Some(Ok(info)) => {
                            stats.session_parses.fetch_add(1, Ordering::Relaxed);
                            if live {
                                live_ids = Some(SessionIds::of(&info));
                            }
                            if to_render
                                .send(Message::Session(connection, Arc::new(info)))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Some(Err(_)) => {
                            stats.session_errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Job::Find { at } => {
                    let found = live_ids
                        .and_then(|ids| replay::find(captures, ids, at, &mut parser, &mut cache));
                    match &found {
                        Some(f) => {
                            info!(capture = %f.name, "replay: playing the capture of this session")
                        }
                        None => info!("replay: no capture of this session"),
                    }
                    if to_data.send(found).is_err() {
                        return;
                    }
                }
            }
        }
    }

    struct DataChannels {
        to_render: Sender<Message>,
        to_session: Sender<Job>,
        recycled: Receiver<Vec<u8>>,
        found: Receiver<Option<Found>>,
    }

    impl DataChannels {
        /// Sends session info text to parse for `connection`, in a buffer the session thread is done with, so
        /// only the first updates allocate. `false` once the session thread has exited.
        fn parse(&self, connection: &Arc<Connection>, text: &[u8], live: bool) -> bool {
            let mut buffer = self.recycled.try_recv().unwrap_or_default();
            buffer.clear();
            buffer.extend_from_slice(text);
            self.to_session
                .send(Job::Parse {
                    connection: Arc::downgrade(connection),
                    text: buffer,
                    live,
                })
                .is_ok()
        }
    }

    /// The data thread's state for the sim's replays.
    #[derive(Debug, Default)]
    struct Replay {
        player: Option<Player>,
        searching: bool,
        last_search: Option<Instant>,
        /// The current connection shows the capture.
        showing: bool,
        /// A connection of this replay showed the capture already.
        started: bool,
    }

    impl Replay {
        fn may_search(&self, now: Instant) -> bool {
            !self.searching
                && self
                    .last_search
                    .is_none_or(|at| now - at >= SEARCH_INTERVAL)
        }
    }

    fn sample(snapshot: &Snapshot, vars: &Vars, missed: u32) -> InputSample {
        InputSample {
            tick: snapshot.tick(),
            session_time: snapshot.f64(vars.session_time, 0).unwrap_or(0.0),
            throttle: snapshot.f32(vars.throttle, 0).unwrap_or(0.0),
            brake: snapshot.f32(vars.brake, 0).unwrap_or(0.0),
            clutch: snapshot.f32(vars.clutch, 0).unwrap_or(1.0),
            steering: snapshot.f32(vars.steering, 0).unwrap_or(0.0),
            abs_active: snapshot.bool(vars.abs_active, 0).unwrap_or(false),
            missed_before: missed,
        }
    }

    /// The render or session thread has exited.
    struct Gone;

    /// Publishes the capture at the moment the sim's replay shows. Returns the connection that now shows it, or
    /// `None` to show the live values instead. A search needs the live session info sent first (`live_sent`):
    /// it identifies the session, and the session thread handles jobs in order.
    fn play_replay(
        replay: &mut Replay,
        current: &Arc<Connection>,
        live: &Snapshot,
        live_sent: bool,
        vars: &Vars,
        channels: &DataChannels,
    ) -> Result<Option<Arc<Connection>>, Gone> {
        let at = Moment::new(
            live.i32(vars.replay_session_num, 0).unwrap_or(-1),
            live.f64(vars.replay_session_time, 0).unwrap_or(f64::NAN),
        );
        let plan = replay.player.as_ref().map_or(Plan::Outside, |p| p.plan(at));
        if plan == Plan::Outside {
            let search = replay.player.as_ref().is_none_or(|p| {
                let end = p.end();
                p.growing() && at.session == end.session && at.time > end.time
            });
            let now = Instant::now();
            if search && live_sent && replay.may_search(now) {
                replay.searching = true;
                replay.last_search = Some(now);
                channels
                    .to_session
                    .send(Job::Find { at })
                    .map_err(|_| Gone)?;
            }
            return Ok(None);
        }
        let Some(player) = replay.player.as_mut() else {
            return Ok(None);
        };
        let restart = plan == Plan::Restart || !replay.showing;
        let connection = if restart {
            let source = Source::Capture {
                first: !replay.started,
            };
            let fresh = Connection::new(live, current.car_count, source, current.row_len);
            if player
                .restart(at, |s| {
                    fresh.samples.push(sample(s, vars, 0));
                })
                .is_err()
            {
                return Ok(None);
            }
            fresh
        } else {
            // The sim's replay clock moves 0.8 to 1.4 frames per tick at 1x, so adding every frame passed would
            // scroll the trace unevenly. While the replay plays, the trace gets the frame at the replayed moment
            // once per tick instead, as live. Without `ReplayPlaySpeed`, every frame passed is added.
            let speed = live.i32(vars.replay_play_speed, 0);
            let moved = match (plan, speed) {
                (Plan::Forward, Some(_)) => player.forward(at, |_| {}),
                (Plan::Forward, None) => player.forward(at, |s| {
                    current.samples.push(sample(s, vars, 0));
                }),
                (Plan::Back, _) => player.back(at),
                _ => Ok(()),
            };
            if moved.is_err() {
                return Ok(None);
            }
            if matches!(plan, Plan::Forward | Plan::Same) && speed.is_some_and(|s| s != 0) {
                current.samples.push(sample(player.snapshot(), vars, 0));
            }
            Arc::clone(current)
        };
        if restart || plan != Plan::Same {
            connection.publish(player.snapshot());
        }
        if restart {
            replay.started = true;
            channels
                .to_render
                .send(Message::Connected(Arc::clone(&connection)))
                .map_err(|_| Gone)?;
            // The new connection has no session info yet, so the one in effect is sent even if unchanged.
            player.take_session_change();
            if let Some(text) = player.session()
                && !channels.parse(&connection, text, false)
            {
                return Err(Gone);
            }
        } else if let Some(text) = player.take_session_change()
            && !channels.parse(&connection, text, false)
        {
            return Err(Gone);
        }
        Ok(Some(connection))
    }

    fn data_loop(
        mut reader: LiveReader,
        vars: Vars,
        stop: &AtomicBool,
        stats: &PipelineStats,
        channels: &DataChannels,
        mut recorder: Option<Recorder>,
    ) {
        let mut snapshot = Snapshot::new();
        let mut connection: Option<Arc<Connection>> = None;
        let mut session_text = Vec::new();
        let mut session_update = None;
        let mut recorded_update = None;
        let mut replay = Replay::default();
        while !stop.load(Ordering::Relaxed) {
            match reader.step(&mut snapshot) {
                Ok(Step::Connected) => {
                    let car_count = snapshot.count(vars.car_lap_dist_pct);
                    let mut row_len = 0;
                    if let (Some(recorder), Some(layout)) = (recorder.as_mut(), reader.layout()) {
                        row_len = layout.buf_len;
                        recorder.connected(
                            CaptureLayout {
                                vars: layout.vars.clone(),
                                buf_len: layout.buf_len,
                                tick_rate: layout.tick_rate,
                                session_info_capacity: layout.session_info_capacity,
                                player_car_only: false,
                            },
                            layout.sdk_version,
                        );
                    }
                    recorded_update = None;
                    let fresh = Connection::new(&snapshot, car_count, Source::Sim, row_len);
                    ready_session_buffer(
                        &mut session_text,
                        reader.session_info_capacity().unwrap_or(0),
                    );
                    session_update = None;
                    replay = Replay::default();
                    connection = Some(Arc::clone(&fresh));
                    if channels.to_render.send(Message::Connected(fresh)).is_err() {
                        return;
                    }
                }
                Ok(Step::Tick { missed }) => {
                    let Some(mut current) = connection.clone() else {
                        continue;
                    };
                    stats.ticks.fetch_add(1, Ordering::Relaxed);
                    stats.missed.fetch_add(u64::from(missed), Ordering::Relaxed);
                    if let Ok(found) = channels.found.try_recv() {
                        replay.searching = false;
                        if let Some(found) = found {
                            replay.player = Some(Player::new(found, reader.var_set(), &snapshot));
                        }
                    }
                    if snapshot.bool(vars.is_replay_playing, 0) == Some(true) {
                        let live_sent = session_update.is_some();
                        match play_replay(
                            &mut replay,
                            &current,
                            &snapshot,
                            live_sent,
                            &vars,
                            channels,
                        ) {
                            Err(Gone) => return,
                            Ok(Some(shown)) => {
                                replay.showing = true;
                                connection = Some(shown);
                                continue;
                            }
                            Ok(None) => {}
                        }
                    } else if replay.player.is_some() || replay.started || replay.searching {
                        // The replay ended: free the capture's buffers. A search still running answers into the
                        // next replay, which is fine for the same session.
                        replay = Replay {
                            showing: replay.showing,
                            searching: replay.searching,
                            ..Replay::default()
                        };
                    }
                    if replay.showing {
                        // Back to live values: widgets reset, and the live session info is sent again.
                        replay.showing = false;
                        let fresh = Connection::new(
                            &snapshot,
                            current.car_count,
                            Source::SimAgain,
                            current.row_len,
                        );
                        if channels
                            .to_render
                            .send(Message::Connected(Arc::clone(&fresh)))
                            .is_err()
                        {
                            return;
                        }
                        session_update = None;
                        connection = Some(Arc::clone(&fresh));
                        current = fresh;
                    }
                    current.publish(&snapshot);
                    current.samples.push(sample(&snapshot, &vars, missed));
                    let update = reader.session_info_update();
                    if update != session_update
                        && let Ok(Some(copied)) = reader.copy_session_info(&mut session_text)
                    {
                        session_update = Some(copied);
                        if !channels.parse(&current, &session_text, true) {
                            return;
                        }
                    }
                    // Ticks while the sim plays a replay hold live values, not the session: not recorded.
                    let replaying = snapshot.bool(vars.is_replay_playing, 0) == Some(true);
                    if let Some(recorder) = recorder.as_mut().filter(|_| !replaying) {
                        if session_update != recorded_update
                            && let Some(update) = session_update
                        {
                            recorder.session_info(update, &session_text);
                            recorded_update = session_update;
                        }
                        let row = &snapshot.bytes()[..current.row_len.min(snapshot.bytes().len())];
                        recorder.frame(snapshot.tick(), snapshot.buffer() as i32, row, missed);
                    }
                }
                Ok(Step::Disconnected) => {
                    if let Some(recorder) = recorder.as_mut() {
                        recorder.ended();
                    }
                    connection = None;
                    replay = Replay::default();
                    if channels.to_render.send(Message::Disconnected).is_err() {
                        return;
                    }
                }
                Ok(Step::Torn) => {
                    stats.torn.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Step::Idle | Step::NotRunning) => {}
                Err(_) => {
                    stats.errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// What changed in a [`View::poll`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct ViewChanges {
        /// Connected or disconnected; widgets should reset their state.
        pub connection: bool,
        /// A newer tick is in the snapshot.
        pub tick: bool,
        /// New session info arrived.
        pub session: bool,
    }

    /// The render thread's view of the pipeline.
    #[derive(Debug)]
    pub struct View {
        vars: Vars,
        messages: Receiver<Message>,
        connection: Option<Arc<Connection>>,
        snapshot: Snapshot,
        session: Option<Arc<SessionInfo>>,
        version: u64,
        stats: Arc<PipelineStats>,
        /// Samples dropped by connections that were replaced.
        dropped_before: u64,
    }

    impl View {
        /// Adds the current connection's dropped samples to the running total before it is replaced.
        fn retire_connection(&mut self) {
            if let Some(old) = self.connection.take() {
                self.dropped_before += old.samples.dropped();
            }
        }

        /// Takes connection changes, session info, and the latest tick. Allocates only when the connection
        /// changes.
        pub fn poll(&mut self) -> ViewChanges {
            let mut changes = ViewChanges::default();
            while let Ok(message) = self.messages.try_recv() {
                match message {
                    Message::Connected(connection) => {
                        self.retire_connection();
                        self.snapshot = connection.template.clone();
                        self.connection = Some(connection);
                        self.session = None;
                        self.version = 0;
                        changes.connection = true;
                    }
                    Message::Disconnected => {
                        self.retire_connection();
                        self.session = None;
                        changes.connection = true;
                    }
                    Message::Session(for_connection, info) => {
                        let current = self.connection.as_ref().map(Arc::downgrade);
                        if current.is_some_and(|c| c.ptr_eq(&for_connection)) {
                            self.session = Some(info);
                            changes.session = true;
                        }
                    }
                }
            }
            if let Some(connection) = &self.connection
                && connection.latest.version() != self.version
                && let Some((version, tick)) = connection
                    .latest
                    .read(&mut self.snapshot.bytes_mut()[connection.row_len..])
            {
                self.version = version;
                self.snapshot.set_tick(tick);
                changes.tick = true;
            }
            changes
        }

        /// Hands every input sample received since the last call to `f`, oldest first.
        pub fn drain_samples(&mut self, mut f: impl FnMut(InputSample)) -> usize {
            let Some(connection) = &self.connection else {
                return 0;
            };
            let mut n = 0;
            while let Some(sample) = connection.samples.pop() {
                f(sample);
                n += 1;
            }
            n
        }

        pub fn vars(&self) -> &Vars {
            &self.vars
        }

        /// The latest tick's variables.
        pub fn snapshot(&self) -> &Snapshot {
            &self.snapshot
        }

        pub fn session(&self) -> Option<&SessionInfo> {
            self.session.as_deref()
        }

        pub fn is_connected(&self) -> bool {
            self.connection.is_some()
        }

        /// Where the current connection's values come from.
        pub fn source(&self) -> Option<Source> {
            self.connection.as_ref().map(|c| c.source)
        }

        /// Per-car array length of the current connection.
        pub fn car_count(&self) -> usize {
            self.connection.as_ref().map_or(0, |c| c.car_count)
        }

        /// Input samples of the current connection dropped because the render thread fell a full ring behind.
        pub fn dropped_samples(&self) -> u64 {
            self.connection.as_ref().map_or(0, |c| c.samples.dropped())
        }

        /// Dropped input samples since the pipeline started, over every connection.
        pub fn dropped_samples_total(&self) -> u64 {
            self.dropped_before + self.dropped_samples()
        }

        pub fn stats(&self) -> &PipelineStats {
            &self.stats
        }

        pub fn display(&self) -> Display {
            if self.connection.is_none() || self.version == 0 {
                return Display::Disconnected;
            }
            let on_track = self
                .snapshot
                .bool(self.vars.is_on_track, 0)
                .unwrap_or(false);
            let replay = self
                .snapshot
                .bool(self.vars.is_replay_playing, 0)
                .unwrap_or(false);
            if on_track && !replay {
                Display::Driving
            } else {
                Display::Idle
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ready_session_buffer;

    #[test]
    fn session_buffer_keeps_its_size_across_connections() {
        let area = 512 * 1024;
        let mut buffer = Vec::new();
        ready_session_buffer(&mut buffer, area);
        let first = buffer.capacity();
        assert!(first >= area);
        // The copy fills the area and keeps only the text.
        buffer.resize(area, 0);
        buffer.truncate(79_000);
        ready_session_buffer(&mut buffer, area);
        assert_eq!(
            buffer.capacity(),
            first,
            "a reconnect does not grow the buffer"
        );
    }
}
