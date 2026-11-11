//! The Windows sampler: process handles for CPU and memory, PDH for GPU and DWM.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use super::Sample;
use super::gpu::{self, EngineLoad};
use super::tree::{self, ProcessEntry, Target};
use crate::sys::{self, Counter, PdhQuery, Process, SysError};

#[derive(Debug)]
struct Tracked {
    process: Process,
    created: u64,
    /// CPU time and cycles already counted.
    cpu: u64,
    cycles: u64,
}

#[derive(Debug)]
struct Counters {
    query: PdhQuery,
    engines: Option<Counter>,
    gpu_dedicated: Option<Counter>,
    gpu_shared: Option<Counter>,
    dwm_cpu: Option<Counter>,
    dwm_pid: Option<Counter>,
}

/// Samples a target's process tree. The first [`sample`](Self::sample) sets the baseline and returns no sample.
#[derive(Debug)]
pub struct Collector {
    target: Target,
    cores: u32,
    /// Processes created after this moment count from their start.
    started: u64,
    tracked: HashMap<u32, Tracked>,
    entries: Vec<ProcessEntry>,
    selected: Vec<u32>,
    counters: Option<Counters>,
    load: EngineLoad,
    dwm_load: EngineLoad,
    first: Option<Instant>,
    last: Option<Instant>,
    own_cpu: u64,
    own_cycles: u64,
    cycle_rate_mhz: Option<f64>,
    unreadable: BTreeSet<String>,
}

/// The rate of the CPU cycle counter in MHz, from a few 10 ms busy loops on this thread against the monotonic
/// clock. Being preempted only lowers a loop's rate, so the highest is kept. Current x64 CPUs count cycles at a
/// fixed rate (the invariant time-stamp counter, at the nominal clock speed), whatever the actual clock speed.
pub fn cycle_rate_mhz() -> Option<f64> {
    let mut best: f64 = 0.0;
    for _ in 0..5 {
        let start = Instant::now();
        let before = sys::current_thread_cycles().ok()?;
        while start.elapsed() < Duration::from_millis(10) {
            std::hint::spin_loop();
        }
        let cycles = sys::current_thread_cycles().ok()?.saturating_sub(before);
        best = best.max(cycles as f64 / start.elapsed().as_secs_f64() / 1.0e6);
    }
    (best > 0.0).then_some(best)
}

impl Collector {
    pub fn new(target: Target) -> Self {
        let counters = PdhQuery::open().ok().map(|mut query| Counters {
            engines: query.add(r"\GPU Engine(*)\Utilization Percentage").ok(),
            gpu_dedicated: query.add(r"\GPU Process Memory(*)\Dedicated Usage").ok(),
            gpu_shared: query.add(r"\GPU Process Memory(*)\Shared Usage").ok(),
            dwm_cpu: query.add(r"\Process(dwm)\% Processor Time").ok(),
            dwm_pid: query.add(r"\Process(dwm)\ID Process").ok(),
            query,
        });
        Self {
            target,
            cores: sys::logical_processors().max(1),
            started: sys::now_filetime(),
            tracked: HashMap::new(),
            entries: Vec::new(),
            selected: Vec::new(),
            counters,
            load: EngineLoad::default(),
            dwm_load: EngineLoad::default(),
            first: None,
            last: None,
            own_cpu: sys::current_process_cpu().unwrap_or(0),
            own_cycles: sys::current_process_cycles().unwrap_or(0),
            cycle_rate_mhz: cycle_rate_mhz(),
            unreadable: BTreeSet::new(),
        }
    }

    pub fn logical_processors(&self) -> u32 {
        self.cores
    }

    /// The cycle counter rate measured when the collector was created; `None` if it could not be read.
    pub fn cycle_rate_mhz(&self) -> Option<f64> {
        self.cycle_rate_mhz
    }

    pub fn gpu_available(&self) -> bool {
        self.counters.as_ref().is_some_and(|c| c.engines.is_some())
    }

    pub fn dwm_available(&self) -> bool {
        self.counters.as_ref().is_some_and(|c| c.dwm_cpu.is_some())
    }

    /// Executable names of tree members that could not be opened, such as elevated processes.
    pub fn unreadable(&self) -> impl Iterator<Item = &str> {
        self.unreadable.iter().map(String::as_str)
    }

    pub fn sample(&mut self) -> Result<Option<Sample>, SysError> {
        let now = Instant::now();
        sys::processes(&mut self.entries)?;
        self.select();

        let (mut cpu, mut cycles, mut private, mut working, mut live) = (0u64, 0u64, 0, 0, 0);
        let selected = &self.selected;
        self.tracked.retain(|pid, t| {
            let exited = t.process.exited();
            if !selected.contains(pid) && !exited {
                // Not part of the tree (a rejected candidate); nothing of it was counted.
                return false;
            }
            // An exited process still reports its final totals, so its last stretch is counted.
            if let Ok(times) = t.process.times() {
                cpu += times.cpu.saturating_sub(t.cpu);
                t.cpu = times.cpu;
            }
            if let Ok(total) = t.process.cycles() {
                cycles += total.saturating_sub(t.cycles);
                t.cycles = total;
            }
            if exited {
                return false;
            }
            if let Ok(memory) = t.process.memory() {
                private += memory.private_bytes;
                working += memory.working_set;
            }
            live += 1;
            true
        });

        let (mut gpu, mut gpu_3d, mut dwm_cpu, mut dwm_gpu) = (None, None, None, None);
        let (mut gpu_dedicated, mut gpu_shared) = (None, None);
        if let Some(c) = self.counters.as_mut()
            && c.query.collect().is_ok()
        {
            let dwm_pid = c.dwm_pid.and_then(|k| c.query.value(k)).map(|v| v as u32);
            dwm_cpu = c.dwm_cpu.and_then(|k| c.query.value(k));
            if let Some(engines) = c.engines {
                let (load, dwm_load) = (&mut self.load, &mut self.dwm_load);
                load.clear();
                dwm_load.clear();
                let read = c.query.each(engines, |name, value| {
                    if let Some((pid, adapter, kind)) = gpu::parse_engine(name) {
                        if selected.contains(&pid) {
                            load.add(adapter, kind, value);
                        } else if Some(pid) == dwm_pid {
                            dwm_load.add(adapter, kind, value);
                        }
                    }
                });
                if read.is_ok() {
                    gpu = Some(load.busiest());
                    gpu_3d = Some(load.of_kind("3d"));
                    dwm_gpu = dwm_pid.map(|_| dwm_load.busiest());
                }
            }
            let mut memory = |counter: Option<Counter>| {
                let mut sum = 0.0;
                let read = c.query.each(counter?, |name, value| {
                    if gpu::parse_pid(name).is_some_and(|pid| selected.contains(&pid)) {
                        sum += value;
                    }
                });
                read.is_ok().then_some(sum as u64)
            };
            gpu_dedicated = memory(c.gpu_dedicated);
            gpu_shared = memory(c.gpu_shared);
        }

        let own_cpu = sys::current_process_cpu().unwrap_or(self.own_cpu);
        let collector = own_cpu.saturating_sub(self.own_cpu);
        self.own_cpu = own_cpu;
        let own_cycles = sys::current_process_cycles().unwrap_or(self.own_cycles);
        let collector_cycles = own_cycles.saturating_sub(self.own_cycles);
        self.own_cycles = own_cycles;

        let Some(last) = self.last.replace(now) else {
            self.first = Some(now);
            return Ok(None);
        };
        let first = self.first.unwrap_or(last);
        let dt = now.duration_since(last).as_secs_f64().max(1e-6);
        let cpu_ms = cpu as f64 / 10_000.0;
        Ok(Some(Sample {
            t: now.duration_since(first).as_secs_f64(),
            dt,
            processes: live,
            cpu_ms,
            cpu_mcycles: cycles as f64 / 1.0e6,
            cpu_pct: cpu_ms / (dt * 1000.0 * f64::from(self.cores)) * 100.0,
            private_bytes: private,
            working_set_bytes: working,
            gpu_pct: gpu,
            gpu_3d_pct: gpu_3d,
            gpu_dedicated_bytes: gpu_dedicated,
            gpu_shared_bytes: gpu_shared,
            // `% Processor Time` of a process is relative to one core.
            dwm_cpu_ms: dwm_cpu.map(|pct| pct / 100.0 * dt * 1000.0),
            dwm_gpu_pct: dwm_gpu,
            collector_cpu_ms: collector as f64 / 10_000.0,
            collector_mcycles: collector_cycles as f64 / 1.0e6,
        }))
    }

    /// Updates `selected`, opening handles for new members. A process created after the collector started counts
    /// from its start; one that was already running counts from now.
    fn select(&mut self) {
        let entries = std::mem::take(&mut self.entries);
        let (tracked, unreadable, started) =
            (&mut self.tracked, &mut self.unreadable, self.started);
        let created = |entry: &ProcessEntry| {
            if let Some(t) = tracked.get(&entry.pid) {
                return Some(t.created);
            }
            let opened = Process::open(entry.pid).and_then(|p| Ok((p.times()?, p)));
            match opened {
                Ok((times, process)) => {
                    let fresh = times.created >= started;
                    let cycles = if fresh {
                        0
                    } else {
                        process.cycles().unwrap_or(0)
                    };
                    tracked.insert(
                        entry.pid,
                        Tracked {
                            process,
                            created: times.created,
                            cpu: if fresh { 0 } else { times.cpu },
                            cycles,
                        },
                    );
                    Some(times.created)
                }
                Err(_) => {
                    unreadable.insert(entry.name.clone());
                    None
                }
            }
        };
        tree::select(&entries, &self.target, created, &mut self.selected);
        self.entries = entries;
    }
}

#[cfg(test)]
mod tests {
    use super::cycle_rate_mhz;

    #[test]
    fn cycle_rate_is_steady() {
        let a = cycle_rate_mhz().expect("cycle counter readable");
        let b = cycle_rate_mhz().expect("cycle counter readable");
        assert!((100.0..20_000.0).contains(&a), "{a} MHz");
        assert!((a - b).abs() / a < 0.05, "{a} and {b} MHz");
    }
}
