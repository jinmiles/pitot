//! The reader blocks on the data-valid event instead of polling, and loses no ticks at 60 Hz.
#![cfg(windows)]

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use pitot_irsdk::layout::{VarHeader, VarType};
use pitot_irsdk::live::{LiveReader, ReaderConfig, SdkNames, Snapshot, Step, VarSet};
use pitot_irsdk::publish::{Publisher, PublisherConfig};

const TICKS: u32 = 120;

#[test]
fn reader_wakes_about_once_per_tick_without_losing_ticks() {
    let pid = std::process::id();
    let mapping = format!("Local\\PitotWakeMap-{pid}");
    let event = format!("Local\\PitotWakeEvent-{pid}");
    let names = SdkNames::new(&mapping, &event);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();

    let publisher = thread::spawn(move || {
        let config = PublisherConfig {
            vars: vec![VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid")],
            buf_len: 8,
            num_buf: 3,
            tick_rate: 60,
            session_info_capacity: 64,
        };
        let mut publisher =
            Publisher::create(&SdkNames::new(&mapping, &event), &config).expect("creates");
        publisher.set_connected(true).expect("connects");
        ready_tx.send(()).expect("reader alive");
        go_rx.recv().expect("reader connected");
        let start = Instant::now();
        for n in 0..TICKS {
            // Pace against the start time so sleep overshoot does not accumulate.
            let due = Duration::from_secs_f64(f64::from(n) / 60.0);
            if let Some(wait) = due.checked_sub(start.elapsed()) {
                thread::sleep(wait);
            }
            publisher
                .publish(&f64::from(n).to_le_bytes())
                .expect("publishes");
        }
        publisher.set_connected(false).expect("disconnects");
        // Keep the mapping alive until the reader has seen the disconnect.
        thread::sleep(Duration::from_millis(200));
    });

    ready_rx.recv().expect("publisher ready");
    let mut vars = VarSet::new();
    let time = vars.add("SessionTime");
    let mut reader = LiveReader::new(names, vars, ReaderConfig::default());
    let mut snapshot = Snapshot::new();
    assert_eq!(reader.step(&mut snapshot), Ok(Step::Connected));
    go_tx.send(()).expect("publisher alive");

    let (mut steps, mut ticks, mut missed, mut idle) = (0u32, 0u32, 0u32, 0u32);
    let mut last_time = -1.0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        steps += 1;
        match reader.step(&mut snapshot).expect("steps") {
            Step::Tick { missed: m } => {
                ticks += 1;
                missed += m;
                let t = snapshot.f64(time, 0).expect("present");
                assert!(t > last_time, "ticks arrive in order");
                last_time = t;
            }
            Step::Idle => idle += 1,
            Step::Disconnected => break,
            other => panic!("unexpected step {other:?}"),
        }
    }
    publisher.join().expect("publisher thread");
    eprintln!("steps={steps} ticks={ticks} missed={missed} idle={idle}");

    assert_eq!(
        ticks + missed,
        TICKS,
        "every published tick is read or counted as missed"
    );
    assert_eq!(missed, 0, "no tick lost at 60 Hz");
    // One wake per tick plus the final disconnect; timeouts only when the publisher thread runs late.
    assert!(idle <= TICKS / 10, "{idle} idle wakes for {TICKS} ticks");
    assert!(steps <= TICKS + idle + 2, "{steps} steps for {TICKS} ticks");
}
