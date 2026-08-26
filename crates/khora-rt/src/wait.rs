//! How a fiber says it is waiting, and how a wake is never lost.
//!
//! A fiber that suspends for fairness will be resumed because its
//! worker put it back on a queue. A fiber that suspends to *wait* will not:
//! somebody else has to say when. That somebody races the suspension, and the
//! race is the whole content of this module.
//!
//! ```text
//! fiber decides to wait
//!                       ↘
//!                         the thing it is waiting for happens
//! ```
//!
//! If the wake lands in that gap and is dropped, the fiber sleeps for ever on
//! an event that already occurred. `docs/design/scheduler.md` §4 states the
//! rule the implementation has to be able to point at:
//!
//! > **If a wake races with a suspension, the fiber either stays running or
//! > becomes runnable. It may never end up waiting having consumed a wakeup.**
//!
//! # How that is kept
//!
//! Three states, and a rule about who may hold the `Task`.
//!
//! ```text
//! RUNNING  ──park──▶  WAITING  ──wake──▶  NOTIFIED
//!    ▲                                        │
//!    └────────────── resumed ─────────────────┘
//! ```
//!
//! **Only the worker running a fiber holds its `Task`.** A waker never does, so
//! a wake cannot enqueue a fiber that is still running — it can only leave a
//! `NOTIFIED` behind. The worker reads the state *after* the suspension returns
//! and decides: `WAITING` means park it where a waker can find it, `NOTIFIED`
//! means the wake already arrived and it goes straight back on the queue.
//!
//! The remaining gap — between the worker reading the state and putting the
//! task somewhere a waker can reach — is closed by doing both under the same
//! lock as the wake. `crate::scheduler` owns that lock, because it owns the
//! tasks.

#![allow(dead_code)]

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Instant;

/// Running, or suspended only for fairness. Its worker will resume it.
pub(crate) const RUNNING: u8 = 0;
/// Waiting for something. Nobody will resume it until a wake arrives.
pub(crate) const WAITING: u8 = 1;
/// A wake arrived. Whoever next looks must make it runnable.
pub(crate) const NOTIFIED: u8 = 2;

/// A fiber's place in the sleep/wake protocol.
#[derive(Debug)]
pub(crate) struct Wait(AtomicU8, AtomicBool);

impl Default for Wait {
    fn default() -> Wait {
        Wait(AtomicU8::new(RUNNING), AtomicBool::new(false))
    }
}

impl Wait {
    /// Says this fiber is about to wait.
    ///
    /// Returns false when a wake is already pending, in which case the fiber
    /// must **not** suspend: the event it was going to wait for has happened.
    /// That is the first half of the invariant — a wake that arrives before the
    /// suspension leaves the fiber running rather than being consumed.
    pub(crate) fn declare(&self) -> bool {
        // `NOTIFIED` → take the notification and stay running.
        // `RUNNING`  → publish the intent to wait.
        self.0
            .compare_exchange(RUNNING, WAITING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Delivers a wake. True when the caller is the one that must make the
    /// fiber runnable.
    ///
    /// Exactly one caller gets true for each transition out of `WAITING`, which
    /// is what stops a fiber being queued twice by two wakers.
    pub(crate) fn wake(&self) -> bool {
        self.0.swap(NOTIFIED, Ordering::AcqRel) == WAITING
    }

    /// What the state is, for a worker deciding what to do with a suspended
    /// task.
    pub(crate) fn peek(&self) -> u8 {
        self.0.load(Ordering::Acquire)
    }

    /// Back to running, once a worker has taken responsibility for the fiber.
    pub(crate) fn running(&self) {
        self.0.store(RUNNING, Ordering::Release);
    }

    /// Says this fiber has been added to the waiting total.
    ///
    /// **`NOTIFIED` cannot answer this on its own**, which is the whole reason
    /// this flag exists. The state says a notification is pending; it does not
    /// say whether the fiber was ever waiting to begin with. Reached from
    /// `WAITING` it means a wait is being released and the total owes one
    /// back; reached from `RUNNING` — a wake that arrived while the fiber was
    /// busy — nothing was ever added, and a worker that later saw the fiber
    /// yield for fairness would take one anyway. That is how the total went to
    /// minus twenty-eight in 11F's soak.
    ///
    /// Set after the total is incremented, so nobody can see the flag before
    /// there is something to take.
    pub(crate) fn start_counting(&self) {
        self.1.store(true, Ordering::Release);
    }

    /// Takes this fiber's place in the waiting total back, if it had one.
    ///
    /// Exactly one caller gets `true` per wait, which is what keeps a wake and
    /// the worker that files the suspension from both subtracting.
    pub(crate) fn stop_counting(&self) -> bool {
        self.1.swap(false, Ordering::AcqRel)
    }

    /// Takes a pending notification, if there is one.
    ///
    /// For a fiber that declared a wait, was woken before it suspended, and now
    /// has to clear the flag before waiting again.
    pub(crate) fn take_notification(&self) -> bool {
        self.0
            .compare_exchange(NOTIFIED, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// Deadlines, soonest first.
///
/// A heap rather than a hierarchical wheel. A wheel is better at very large
/// scale and this is enough to establish the semantics — `scheduler.md` §6 says
/// to measure before replacing it, and there is now something to measure.
#[derive(Default)]
pub(crate) struct Timers {
    /// `Reverse`, because `BinaryHeap` is a max-heap and the interesting end is
    /// the soonest deadline.
    pending: BinaryHeap<Reverse<Timer>>,
}

/// One fiber waiting for a moment to arrive.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Timer {
    pub(crate) at: Instant,
    pub(crate) fiber: usize,
}

impl Ord for Timer {
    fn cmp(&self, other: &Timer) -> std::cmp::Ordering {
        // By deadline, then by fiber, so two timers at the same instant have a
        // stable order rather than an arbitrary one.
        self.at.cmp(&other.at).then(self.fiber.cmp(&other.fiber))
    }
}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Timer) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Timers {
    pub(crate) fn add(&mut self, at: Instant, fiber: usize) {
        self.pending.push(Reverse(Timer { at, fiber }));
    }

    /// Every fiber whose deadline has passed.
    pub(crate) fn expired(&mut self, now: Instant) -> Vec<usize> {
        let mut out = Vec::new();
        while let Some(Reverse(timer)) = self.pending.peek() {
            if timer.at > now {
                break;
            }
            out.push(self.pending.pop().expect("just peeked").0.fiber);
        }
        out
    }

    /// When the soonest deadline is, for a reactor deciding how long to block.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending.peek().map(|Reverse(timer)| timer.at)
    }

    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Forgets a fiber's timer.
    ///
    /// Linear, and deliberately so at this stage: a cancelled timer is rare and
    /// leaving it to fire harmlessly is also correct — the fiber it names is no
    /// longer waiting, so the wake finds nothing. This exists for the case
    /// where a hundred thousand cancelled timers would otherwise sit in the
    /// heap holding it open.
    pub(crate) fn forget(&mut self, fiber: usize) {
        let kept: Vec<Reverse<Timer>> =
            self.pending.drain().filter(|Reverse(t)| t.fiber != fiber).collect();
        self.pending = kept.into_iter().collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    // --- the invariant ------------------------------------------------------

    #[test]
    fn an_ordinary_wait_then_wake() {
        let wait = Wait::default();
        assert!(wait.declare(), "nothing pending, so it may wait");
        assert_eq!(wait.peek(), WAITING);
        assert!(wait.wake(), "the waker owns making it runnable");
        assert_eq!(wait.peek(), NOTIFIED);
    }

    /// The first half of the invariant: a wake that lands *before* the fiber
    /// declares its wait must leave it running rather than being consumed.
    #[test]
    fn a_wake_before_the_wait_stops_the_fiber_waiting_at_all() {
        let wait = Wait::default();
        assert!(!wait.wake(), "it was not waiting, so nobody needs to queue it");
        assert!(!wait.declare(), "and now it must not wait");
        assert!(wait.take_notification(), "the notification is there to take");
        assert_eq!(wait.peek(), RUNNING);
    }

    /// Two wakers, one transition. Otherwise a fiber is queued twice and run by
    /// two workers at once.
    #[test]
    fn only_one_waker_is_told_to_queue_it() {
        let wait = Wait::default();
        assert!(wait.declare());
        assert!(wait.wake());
        assert!(!wait.wake(), "the second wake must not queue it again");
    }

    /// The same, hammered from many threads at once. Exactly one of them may
    /// come away believing it owns the fiber.
    #[test]
    fn concurrent_wakers_produce_exactly_one_owner() {
        for _ in 0..200 {
            let wait = Arc::new(Wait::default());
            assert!(wait.declare());

            let owners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hands: Vec<_> = (0..8)
                .map(|_| {
                    let wait = wait.clone();
                    let owners = owners.clone();
                    std::thread::spawn(move || {
                        if wait.wake() {
                            owners.fetch_add(1, Ordering::SeqCst);
                        }
                    })
                })
                .collect();
            for hand in hands {
                hand.join().expect("a waker");
            }
            assert_eq!(owners.load(Ordering::SeqCst), 1);
        }
    }

    /// A wake racing a declaration, from two threads, many times. Whatever the
    /// interleaving, the fiber must not end up waiting with the wake gone.
    #[test]
    fn a_wake_racing_a_wait_never_disappears() {
        for _ in 0..2_000 {
            let wait = Arc::new(Wait::default());

            let waker = {
                let wait = wait.clone();
                std::thread::spawn(move || wait.wake())
            };
            let declared = wait.declare();
            let queued = waker.join().expect("the waker");

            // Either the waker will queue it, or it never waited. Never
            // neither: that is a fiber asleep on an event that happened.
            assert!(
                queued || !declared,
                "the wake was lost: declared={declared} queued={queued}"
            );
        }
    }

    // --- timers -------------------------------------------------------------

    #[test]
    fn timers_come_out_soonest_first() {
        let now = Instant::now();
        let mut timers = Timers::default();
        timers.add(now + Duration::from_millis(30), 3);
        timers.add(now + Duration::from_millis(10), 1);
        timers.add(now + Duration::from_millis(20), 2);

        assert_eq!(timers.expired(now + Duration::from_millis(25)), [1, 2]);
        assert_eq!(timers.expired(now + Duration::from_millis(25)), [] as [usize; 0]);
        assert_eq!(timers.expired(now + Duration::from_millis(40)), [3]);
    }

    #[test]
    fn nothing_is_expired_before_its_deadline() {
        let now = Instant::now();
        let mut timers = Timers::default();
        timers.add(now + Duration::from_secs(60), 1);
        assert!(timers.expired(now).is_empty());
        assert_eq!(timers.len(), 1);
    }

    #[test]
    fn the_next_deadline_is_the_soonest_one() {
        let now = Instant::now();
        let mut timers = Timers::default();
        assert_eq!(timers.next_deadline(), None);
        timers.add(now + Duration::from_secs(9), 1);
        timers.add(now + Duration::from_secs(2), 2);
        assert_eq!(timers.next_deadline(), Some(now + Duration::from_secs(2)));
    }

    /// Two fibers at the same instant is the ordinary case at scale, and the
    /// order has to be decided by something rather than by heap luck.
    #[test]
    fn timers_at_one_instant_have_a_stable_order() {
        let at = Instant::now();
        let mut timers = Timers::default();
        for fiber in [7, 3, 5] {
            timers.add(at, fiber);
        }
        assert_eq!(timers.expired(at), [3, 5, 7]);
    }

    #[test]
    fn a_forgotten_timer_does_not_fire() {
        let now = Instant::now();
        let mut timers = Timers::default();
        timers.add(now, 1);
        timers.add(now, 2);
        timers.forget(1);
        assert_eq!(timers.expired(now), [2]);
    }

    #[test]
    fn a_hundred_thousand_timers_sort_correctly() {
        let now = Instant::now();
        let mut timers = Timers::default();
        // Interleaved rather than in order, so the heap does real work.
        for fiber in 0..100_000usize {
            let offset = (fiber * 7919) % 100_000;
            timers.add(now + Duration::from_micros(offset as u64), fiber);
        }
        assert_eq!(timers.len(), 100_000);

        let fired = timers.expired(now + Duration::from_secs(1));
        assert_eq!(fired.len(), 100_000);
        assert!(timers.next_deadline().is_none());
    }
}
