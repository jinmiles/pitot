//! Lock-free handoff between one writer thread and one reader thread, built only from atomics.
//!
//! Every shared word is an atomic, so there is no `unsafe` and no data race. Storage is allocated once at
//! construction; writes and reads do not allocate.

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering, fence};

fn words_for(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

/// Latest value of a fixed-size byte buffer, published with a sequence lock.
///
/// One thread writes; any number of threads may read. A read that overlaps a write is retried.
#[derive(Debug)]
pub struct SeqBuffer {
    seq: AtomicU64,
    tick: AtomicI32,
    words: Box<[AtomicU32]>,
    len: usize,
}

impl SeqBuffer {
    pub fn new(len: usize) -> Self {
        Self {
            seq: AtomicU64::new(0),
            tick: AtomicI32::new(0),
            words: (0..words_for(len)).map(|_| AtomicU32::new(0)).collect(),
            len,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Sequence number of the last completed write; 0 before the first.
    pub fn version(&self) -> u64 {
        self.seq.load(Ordering::Acquire) / 2
    }

    /// Publishes `bytes` (which must be `len` bytes long; extra bytes are ignored, missing ones are zero).
    /// Only one thread may call this.
    pub fn write(&self, tick: i32, bytes: &[u8]) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        let mut chunks = bytes[..bytes.len().min(self.len)].chunks(4);
        for word in self.words.iter() {
            let mut raw = [0u8; 4];
            if let Some(chunk) = chunks.next() {
                raw[..chunk.len()].copy_from_slice(chunk);
            }
            word.store(u32::from_le_bytes(raw), Ordering::Relaxed);
        }
        self.tick.store(tick, Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// Copies the latest complete write into `out` and returns its version and tick, or `None` if a write
    /// kept overlapping the copy. `out` should be `len` bytes long.
    pub fn read(&self, out: &mut [u8]) -> Option<(u64, i32)> {
        for _ in 0..8 {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                core::hint::spin_loop();
                continue;
            }
            for (word, chunk) in self.words.iter().zip(out.chunks_mut(4)) {
                let raw = word.load(Ordering::Relaxed).to_le_bytes();
                chunk.copy_from_slice(&raw[..chunk.len()]);
            }
            let tick = self.tick.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return Some((before / 2, tick));
            }
        }
        None
    }
}

/// One input trace sample.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct InputSample {
    pub tick: i32,
    pub session_time: f64,
    pub throttle: f32,
    pub brake: f32,
    /// As reported by the sim: 1 is engaged (pedal released). Display `1 - clutch`.
    pub clutch: f32,
    /// Steering wheel angle in radians.
    pub steering: f32,
    pub abs_active: bool,
    /// Ticks lost just before this sample (overwritten in the sim's buffers before they were read).
    pub missed_before: u32,
}

const SAMPLE_WORDS: usize = 8;

impl InputSample {
    fn to_words(self) -> [u32; SAMPLE_WORDS] {
        let time = self.session_time.to_bits();
        [
            self.tick as u32,
            time as u32,
            (time >> 32) as u32,
            self.throttle.to_bits(),
            self.brake.to_bits(),
            self.clutch.to_bits(),
            self.steering.to_bits(),
            u32::from(self.abs_active) | (self.missed_before.min(u32::MAX >> 1) << 1),
        ]
    }

    fn from_words(w: [u32; SAMPLE_WORDS]) -> Self {
        Self {
            tick: w[0] as i32,
            session_time: f64::from_bits(u64::from(w[1]) | (u64::from(w[2]) << 32)),
            throttle: f32::from_bits(w[3]),
            brake: f32::from_bits(w[4]),
            clutch: f32::from_bits(w[5]),
            steering: f32::from_bits(w[6]),
            abs_active: w[7] & 1 == 1,
            missed_before: w[7] >> 1,
        }
    }
}

/// Bounded single-producer, single-consumer queue of input samples.
#[derive(Debug)]
pub struct SampleRing {
    slots: Box<[[AtomicU32; SAMPLE_WORDS]]>,
    head: AtomicU64,
    tail: AtomicU64,
    dropped: AtomicU64,
}

impl SampleRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: (0..capacity.max(1))
                .map(|_| core::array::from_fn(|_| AtomicU32::new(0)))
                .collect(),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Samples rejected because the consumer fell behind by a full ring.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Appends a sample. Only the producer thread may call this. Returns `false` when the ring is full.
    pub fn push(&self, sample: InputSample) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head - tail >= self.slots.len() as u64 {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let slot = &self.slots[(head % self.slots.len() as u64) as usize];
        for (word, value) in slot.iter().zip(sample.to_words()) {
            word.store(value, Ordering::Relaxed);
        }
        self.head.store(head + 1, Ordering::Release);
        true
    }

    /// Takes the oldest sample. Only the consumer thread may call this.
    pub fn pop(&self) -> Option<InputSample> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        let slot = &self.slots[(tail % self.slots.len() as u64) as usize];
        let words = core::array::from_fn(|i| slot[i].load(Ordering::Relaxed));
        self.tail.store(tail + 1, Ordering::Release);
        Some(InputSample::from_words(words))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn seq_buffer_round_trips_bytes() {
        let buffer = SeqBuffer::new(7);
        let mut out = [0u8; 7];
        assert_eq!(buffer.read(&mut out), Some((0, 0)));
        buffer.write(42, &[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(buffer.read(&mut out), Some((1, 42)));
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7]);
        buffer.write(43, &[9; 3]);
        assert_eq!(buffer.read(&mut out), Some((2, 43)));
        assert_eq!(out, [9, 9, 9, 0, 0, 0, 0], "missing bytes are zero");
        assert_eq!(buffer.version(), 2);
    }

    #[test]
    fn seq_buffer_reads_are_never_torn() {
        let buffer = Arc::new(SeqBuffer::new(256));
        let writer = {
            let buffer = Arc::clone(&buffer);
            thread::spawn(move || {
                for n in 0..20_000u32 {
                    buffer.write(n as i32, &[(n % 251) as u8; 256]);
                }
            })
        };
        let mut out = [0u8; 256];
        let (mut reads, mut last) = (0, 0);
        while !writer.is_finished() || reads == 0 {
            if let Some((version, _)) = buffer.read(&mut out) {
                assert!(
                    out.iter().all(|&b| b == out[0]),
                    "mixed bytes from two writes"
                );
                assert!(version >= last, "versions go forward");
                last = version;
                reads += 1;
            }
        }
        writer.join().expect("writer");
    }

    fn sample(tick: i32) -> InputSample {
        InputSample {
            tick,
            session_time: f64::from(tick) / 60.0,
            throttle: 0.5,
            brake: 0.25,
            clutch: 1.0,
            steering: -0.75,
            abs_active: tick % 2 == 0,
            missed_before: 3,
        }
    }

    #[test]
    fn sample_ring_is_fifo_and_bounded() {
        let ring = SampleRing::new(4);
        for tick in 0..4 {
            assert!(ring.push(sample(tick)));
        }
        assert!(!ring.push(sample(4)), "full");
        assert_eq!(ring.dropped(), 1);
        assert_eq!(ring.pop(), Some(sample(0)));
        assert!(ring.push(sample(5)));
        let rest: Vec<i32> = core::iter::from_fn(|| ring.pop()).map(|s| s.tick).collect();
        assert_eq!(rest, [1, 2, 3, 5]);
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn sample_ring_delivers_every_sample_across_threads() {
        let ring = Arc::new(SampleRing::new(64));
        let producer = {
            let ring = Arc::clone(&ring);
            thread::spawn(move || {
                for tick in 0..50_000 {
                    while !ring.push(sample(tick)) {
                        thread::yield_now();
                    }
                }
            })
        };
        let mut expected = 0;
        while expected < 50_000 {
            if let Some(s) = ring.pop() {
                assert_eq!(s, sample(expected));
                expected += 1;
            }
        }
        producer.join().expect("producer");
    }
}
