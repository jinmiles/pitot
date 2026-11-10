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

mod vars;
mod wire;

pub use vars::Vars;
pub use wire::{InputSample, SampleRing, SeqBuffer};

#[cfg(windows)]
pub use threads::{Pipeline, PipelineStats, View, ViewChanges};

/// Whether widgets should show and the render loop should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// No sim connection.
    Disconnected,
    /// Connected, but the player is not driving (garage, menus, replay, spectating): widgets hide.
    Idle,
    /// The player is in the car on track; inputs are the driver's own.
    Driving,
}

/// Samples the ring holds: about eight seconds at 60 Hz, far more than one late frame.
pub const SAMPLE_CAPACITY: usize = 512;

#[cfg(windows)]
mod threads {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Weak};
    use std::thread::{self, JoinHandle};

    use pitot_irsdk::live::{LiveReader, ReaderConfig, SdkNames, Snapshot, Step, VarSet};
    use pitot_irsdk::session::{self, SessionInfo};

    use super::{Display, InputSample, SAMPLE_CAPACITY, SampleRing, SeqBuffer, Vars};

    /// Buffers for one connection, shared by the data thread (writer) and the render thread (reader).
    #[derive(Debug)]
    struct Connection {
        /// Layout of the requested variables; cloned by the render thread on connect.
        template: Snapshot,
        latest: SeqBuffer,
        samples: SampleRing,
        car_count: usize,
    }

    enum Message {
        Connected(Arc<Connection>),
        Disconnected,
        Session(Weak<Connection>, Arc<SessionInfo>),
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
        /// Starts reading from `names` and returns the pipeline and the render thread's view.
        pub fn start(names: SdkNames, config: ReaderConfig) -> std::io::Result<(Self, View)> {
            let mut set = VarSet::new();
            let vars = Vars::register(&mut set);
            let stop = Arc::new(AtomicBool::new(false));
            let stats = Arc::new(PipelineStats::default());
            let (to_render, from_threads) = mpsc::channel();
            let (to_session, session_jobs) = mpsc::channel::<(Weak<Connection>, Vec<u8>)>();

            let session = {
                let to_render = to_render.clone();
                let stats = Arc::clone(&stats);
                thread::Builder::new()
                    .name("pitot-session".into())
                    .spawn(move || {
                        // Blocks until the data thread sends text, and ends when it exits.
                        while let Ok((connection, text)) = session_jobs.recv() {
                            let Some(car_count) = connection.upgrade().map(|c| c.car_count) else {
                                continue;
                            };
                            match session::parse(&text, car_count) {
                                Ok(info) => {
                                    stats.session_parses.fetch_add(1, Ordering::Relaxed);
                                    if to_render
                                        .send(Message::Session(connection, Arc::new(info)))
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(_) => {
                                    stats.session_errors.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    })?
            };
            let data = {
                let stop = Arc::clone(&stop);
                let stats = Arc::clone(&stats);
                thread::Builder::new()
                    .name("pitot-data".into())
                    .spawn(move || {
                        let reader = LiveReader::new(names, set, config);
                        data_loop(reader, vars, &stop, &stats, &to_render, &to_session);
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

    fn data_loop(
        mut reader: LiveReader,
        vars: Vars,
        stop: &AtomicBool,
        stats: &PipelineStats,
        to_render: &Sender<Message>,
        to_session: &Sender<(Weak<Connection>, Vec<u8>)>,
    ) {
        let mut snapshot = Snapshot::new();
        let mut connection: Option<Arc<Connection>> = None;
        let mut session_text = Vec::new();
        let mut session_update = None;
        while !stop.load(Ordering::Relaxed) {
            match reader.step(&mut snapshot) {
                Ok(Step::Connected) => {
                    let car_count = snapshot.count(vars.car_lap_dist_pct);
                    let fresh = Arc::new(Connection {
                        template: snapshot.clone(),
                        latest: SeqBuffer::new(snapshot.bytes().len()),
                        samples: SampleRing::new(SAMPLE_CAPACITY),
                        car_count,
                    });
                    session_text.reserve(reader.session_info_capacity().unwrap_or(0));
                    session_update = None;
                    connection = Some(Arc::clone(&fresh));
                    if to_render.send(Message::Connected(fresh)).is_err() {
                        return;
                    }
                }
                Ok(Step::Tick { missed }) => {
                    let Some(current) = connection.as_ref() else {
                        continue;
                    };
                    stats.ticks.fetch_add(1, Ordering::Relaxed);
                    stats.missed.fetch_add(u64::from(missed), Ordering::Relaxed);
                    current.latest.write(snapshot.tick(), snapshot.bytes());
                    current.samples.push(InputSample {
                        tick: snapshot.tick(),
                        session_time: snapshot.f64(vars.session_time, 0).unwrap_or(0.0),
                        throttle: snapshot.f32(vars.throttle, 0).unwrap_or(0.0),
                        brake: snapshot.f32(vars.brake, 0).unwrap_or(0.0),
                        clutch: snapshot.f32(vars.clutch, 0).unwrap_or(1.0),
                        steering: snapshot.f32(vars.steering, 0).unwrap_or(0.0),
                        abs_active: snapshot.bool(vars.abs_active, 0).unwrap_or(false),
                        missed_before: missed,
                    });
                    let update = reader.session_info_update();
                    if update != session_update
                        && let Ok(Some(copied)) = reader.copy_session_info(&mut session_text)
                    {
                        session_update = Some(copied);
                        // Copying the text allocates, but only when the session info changes.
                        let job = (Arc::downgrade(current), session_text.clone());
                        if to_session.send(job).is_err() {
                            return;
                        }
                    }
                }
                Ok(Step::Disconnected) => {
                    connection = None;
                    if to_render.send(Message::Disconnected).is_err() {
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
                && let Some((version, tick)) = connection.latest.read(self.snapshot.bytes_mut())
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
