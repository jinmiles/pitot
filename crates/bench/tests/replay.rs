//! A replay is read back through the live reader with the values and session info that were recorded.
#![cfg(windows)]

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use pitot_bench::replay::{FrameSource, Item, ReplayError, ReplayOptions, SourceLayout, run};
use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{LiveReader, ReaderConfig, SdkNames, Snapshot, Step, VarSet};

const FRAMES: u32 = 90;

/// In-memory source: session info, then frames carrying their index, with a revision halfway.
struct Synthetic {
    layout: SourceLayout,
    next: u32,
}

impl Synthetic {
    fn new() -> Self {
        Self {
            layout: SourceLayout {
                vars: vec![
                    VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
                    VarHeader::new(VarType::Int, 8, 1, "Lap").expect("valid"),
                ],
                buf_len: 12,
                tick_rate: 60,
                session_info_capacity: 1024,
                player_car_only: false,
            },
            next: 0,
        }
    }
}

impl FrameSource for Synthetic {
    fn layout(&self) -> &SourceLayout {
        &self.layout
    }

    fn next(&mut self, buf: &mut Vec<u8>) -> Result<Option<Item>, ReplayError> {
        buf.clear();
        let step = self.next;
        self.next += 1;
        if step == 0 || step == FRAMES / 2 + 1 {
            buf.extend_from_slice(format!("WeekendInfo:\n TrackID: {step}\n").as_bytes());
            return Ok(Some(Item::SessionInfo));
        }
        let frame = if step > FRAMES / 2 + 1 {
            step - 2
        } else {
            step - 1
        };
        if frame >= FRAMES {
            return Ok(None);
        }
        buf.extend_from_slice(&f64::from(frame).to_le_bytes());
        buf.extend_from_slice(&i32::try_from(frame / 30).expect("small").to_le_bytes());
        Ok(Some(Item::Frame))
    }

    fn rewind(&mut self) -> Result<(), ReplayError> {
        self.next = 0;
        Ok(())
    }
}

fn names(tag: &str) -> (String, String) {
    let pid = std::process::id();
    (
        format!("Local\\PitotReplayMap-{tag}-{pid}"),
        format!("Local\\PitotReplayEvent-{tag}-{pid}"),
    )
}

#[test]
fn replay_is_read_back_in_order_at_the_recorded_rate() {
    let (mapping, event) = names("order");
    let (done_tx, done_rx) = mpsc::channel();
    let replay = {
        let (mapping, event) = (mapping.clone(), event.clone());
        thread::spawn(move || {
            let options = ReplayOptions {
                names: SdkNames::new(&mapping, &event),
                linger: Duration::from_millis(300),
                ..ReplayOptions::default()
            };
            // Let the reader start first, as an overlay would be waiting for the sim.
            thread::sleep(Duration::from_millis(50));
            let summary = run(&mut Synthetic::new(), &options).expect("replays");
            done_tx.send(()).expect("test alive");
            summary
        })
    };

    let mut vars = VarSet::new();
    let time = vars.add("SessionTime");
    let lap = vars.add("Lap");
    let config = ReaderConfig {
        retry_interval: Duration::from_millis(5),
        idle_timeout: Duration::from_millis(200),
    };
    let mut reader = LiveReader::new(SdkNames::new(&mapping, &event), vars, config);
    let mut snapshot = Snapshot::new();
    let mut session_info = Vec::new();
    let (mut seen, mut missed, mut updates) = (Vec::new(), 0, Vec::new());
    let mut first_tick_at = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && done_rx.try_recv().is_err() {
        match reader.step(&mut snapshot).expect("steps") {
            Step::Tick { missed: m } => {
                first_tick_at.get_or_insert_with(Instant::now);
                missed += m;
                seen.push((
                    snapshot.f64(time, 0).expect("time"),
                    snapshot.i32(lap, 0).expect("lap"),
                ));
                if let Ok(Some(update)) = reader.copy_session_info(&mut session_info)
                    && updates.last() != Some(&update)
                {
                    updates.push(update);
                }
            }
            Step::Disconnected => break,
            _ => {}
        }
    }
    let span = first_tick_at.map(|at| at.elapsed()).unwrap_or_default();
    let summary = replay.join().expect("replay thread");

    assert_eq!(summary.frames, u64::from(FRAMES));
    assert_eq!(summary.session_infos, 2);
    // The reader normally connects after the first frame is published and starts with the second.
    let read = seen.len() as u32 + missed;
    assert!(
        read == FRAMES - 1 || read == FRAMES,
        "every frame after connecting is read or counted: {read}"
    );
    assert_eq!(missed, 0);
    assert!(
        seen.windows(2).all(|w| w[1].0 == w[0].0 + 1.0),
        "frames arrive in order"
    );
    assert_eq!(seen.last().map(|s| s.1), Some(2));
    assert_eq!(updates, [1, 2], "both session info revisions are visible");
    assert_eq!(&session_info[..], b"WeekendInfo:\n TrackID: 46\n");
    // 88 frame periods at 60 Hz is about 1.47 s; allow for scheduling noise.
    assert!(
        span > Duration::from_millis(1300) && span < Duration::from_millis(2500),
        "{span:?}"
    );
}

#[test]
fn replay_refuses_names_in_use() {
    let (mapping, event) = names("busy");
    let names = SdkNames::new(&mapping, &event);
    let options = ReplayOptions {
        names: names.clone(),
        max_frames: Some(1),
        linger: Duration::from_millis(400),
        ..ReplayOptions::default()
    };
    let first = thread::spawn(move || run(&mut Synthetic::new(), &options).map(|s| s.frames));
    thread::sleep(Duration::from_millis(150));
    let second = ReplayOptions {
        names,
        linger: Duration::ZERO,
        ..ReplayOptions::default()
    };
    assert!(matches!(
        run(&mut Synthetic::new(), &second),
        Err(ReplayError::NamesInUse)
    ));
    assert_eq!(first.join().expect("first replay").ok(), Some(1));
}
