//! Wake-ups for the overlay's scheduler, which runs as a task on GPUI's thread: the data and session threads, the
//! tray and move-handle windows, the configuration watch, and the scheduler's own timer each set a reason and wake
//! the task. Setting a reason and waking does not allocate or lock in our code; waking hands the task to GPUI's
//! executor, which posts one message to its thread.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::task::{Context, Poll};

use futures::task::AtomicWaker;

/// Why the scheduler woke, as bits.
pub mod reason {
    /// The data thread published a tick while the scheduler asked for ticks ([`super::Wake::arm_ticks`]).
    pub const TICK: u32 = 1;
    /// The data or session thread sent a message: a connection change, session info, or tire samples.
    pub const MESSAGE: u32 = 1 << 1;
    /// The scheduler's timer fired.
    pub const TIMER: u32 = 1 << 2;
    /// A command from the tray menu or the Pitot window.
    pub const COMMAND: u32 = 1 << 3;
    /// A move handle reported a drag.
    pub const MOVE: u32 = 1 << 4;
    /// The configuration directory changed.
    pub const CONFIG: u32 = 1 << 5;
}

/// The scheduler's wake-up: reasons waiting, whether ticks wake it, and the task to wake.
#[derive(Debug, Default)]
pub struct Wake {
    reasons: AtomicU32,
    ticks: AtomicBool,
    waker: AtomicWaker,
}

impl Wake {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `reasons` and wakes the scheduler. Any thread.
    pub fn signal(&self, reasons: u32) {
        self.reasons.fetch_or(reasons, Ordering::AcqRel);
        self.waker.wake();
    }

    /// A tick was published: wakes the scheduler once if it asked for ticks. The data thread calls it every tick.
    pub fn tick(&self) {
        if self.ticks.swap(false, Ordering::AcqRel) {
            self.signal(reason::TICK);
        }
    }

    /// Whether the next tick wakes the scheduler. Each wake by a tick disarms it, so the scheduler asks again only
    /// while a shown widget is due by the next tick.
    pub fn arm_ticks(&self, on: bool) {
        self.ticks.store(on, Ordering::Release);
    }

    /// Takes the reasons waiting, without waiting.
    pub fn take(&self) -> u32 {
        self.reasons.swap(0, Ordering::AcqRel)
    }

    /// Waits until a reason is set and returns the reasons.
    pub fn wait(&self) -> WakeFuture<'_> {
        WakeFuture { wake: self }
    }
}

/// [`Wake::wait`].
#[derive(Debug)]
pub struct WakeFuture<'a> {
    wake: &'a Wake,
}

impl Future for WakeFuture<'_> {
    type Output = u32;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
        // Registered before the reasons are read, so a reason set in between wakes the task again.
        self.wake.waker.register(cx.waker());
        match self.wake.take() {
            0 => Poll::Pending,
            reasons => Poll::Ready(reasons),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Wake as TaskWake, Waker};

    use super::*;

    struct Count(AtomicUsize);

    impl TaskWake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn reasons_wake_the_task_and_are_taken_once() {
        let wake = Wake::new();
        let count = Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&count));
        let mut cx = Context::from_waker(&waker);
        let mut future = wake.wait();
        assert_eq!(Pin::new(&mut future).poll(&mut cx), Poll::Pending);
        wake.signal(reason::COMMAND);
        wake.signal(reason::CONFIG);
        assert_eq!(count.0.load(Ordering::Relaxed), 1, "one wake until polled");
        assert_eq!(
            Pin::new(&mut future).poll(&mut cx),
            Poll::Ready(reason::COMMAND | reason::CONFIG)
        );
        assert_eq!(Pin::new(&mut future).poll(&mut cx), Poll::Pending);
    }

    #[test]
    fn ticks_wake_only_when_asked_for_and_once_per_ask() {
        let wake = Wake::new();
        wake.tick();
        assert_eq!(wake.take(), 0, "not asked for");
        wake.arm_ticks(true);
        wake.tick();
        wake.tick();
        assert_eq!(wake.take(), reason::TICK);
        wake.tick();
        assert_eq!(wake.take(), 0, "disarmed by the first");
    }
}
