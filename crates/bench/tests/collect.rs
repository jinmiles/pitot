//! The collector on real processes: this test process, a child process tree, and a missing target.
#![cfg(windows)]

use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use pitot_bench::collect::{Collector, Target};

fn busy(duration: Duration) {
    let end = Instant::now() + duration;
    let mut x = 1u64;
    while Instant::now() < end {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    }
    std::hint::black_box(x);
}

#[test]
fn measures_this_process() {
    let mut collector = Collector::new(Target::Pid(std::process::id()));
    assert!(collector.sample().expect("baseline").is_none());
    busy(Duration::from_millis(400));
    let sample = collector
        .sample()
        .expect("sample")
        .expect("a sample after the baseline");
    // Other tests run in this process too, and their child processes join its tree, so only lower bounds hold.
    assert!(sample.processes >= 1, "{sample:?}");
    assert!(sample.cpu_ms >= 200.0, "busy for 400 ms: {sample:?}");
    assert!(sample.cpu_mcycles > 0.0, "{sample:?}");
    assert!(sample.private_bytes > 0 && sample.working_set_bytes > 0);
    assert!(sample.dt >= 0.4 && sample.cpu_pct > 0.0);
}

#[test]
fn follows_child_processes_and_their_exit() {
    let mut child = Command::new("cmd")
        .args(["/c", "ping -n 3 127.0.0.1 >nul"])
        .stdout(Stdio::null())
        .spawn()
        .expect("start cmd");
    let mut collector = Collector::new(Target::Pid(child.id()));
    collector.sample().expect("baseline");
    thread::sleep(Duration::from_millis(500));
    let running = collector.sample().expect("sample").expect("sample");
    assert!(running.processes >= 2, "cmd and ping: {running:?}");
    child.wait().expect("wait for cmd");
    let done = collector.sample().expect("sample").expect("sample");
    assert_eq!(done.processes, 0, "{done:?}");
    assert_eq!(done.private_bytes, 0);
}

#[test]
fn missing_target_reads_as_empty() {
    let mut collector = Collector::new(Target::Names(vec!["no-such-process-for-pitot".into()]));
    collector.sample().expect("baseline");
    thread::sleep(Duration::from_millis(100));
    let sample = collector.sample().expect("sample").expect("sample");
    assert_eq!(
        (sample.processes, sample.cpu_ms, sample.private_bytes),
        (0, 0.0, 0)
    );
}
