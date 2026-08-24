//! Workers, queues, and whose turn it is.
//!
//! Phase 11B. Fibers get stacks in [`crate::coro`]; this is what runs them on
//! more than one core.
//!
//! # Deliberately the simple version
//!
//! A worker takes from its own queue, then from the shared one, then parks.
//! There is **no work stealing**, which means a worker with a full local queue
//! can be busy while another sleeps. That is 11D, and it is separate on purpose:
//! stealing is where the subtle bugs live, and putting it in now would make
//! every failure ambiguous between "the scheduler is wrong" and "the stealing
//! is wrong".
//!
//! # Fairness has two halves and only one of them is here
//!
//! **Between queues.** A fiber that spawns in a loop pushes to its worker's
//! local queue every time, and a worker that always drained local first would
//! never look at the shared queue again — so anything injected from outside,
//! including everything a reactor will wake, would starve. Every
//! [`GLOBAL_INTERVAL`] turns the worker looks at the shared queue first.
//!
//! **Within a fiber.** A fiber that never suspends holds its worker. The
//! runtime's answer is [`crate::coro::suspend`] called from a safepoint, and
//! what decides *when* is [`Budget`]: each resume grants a number of
//! safepoints, and the one that spends the last of them yields.
//!
//! That is fairness measured in safepoints rather than in time, which is worth
//! being honest about: a fiber doing something expensive between two safepoints
//! still holds its worker for exactly that long. A timer setting a flag is the
//! refinement, and it wants something to measure first.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::coro::{Ran, Task};

/// How many turns a worker takes before looking at the shared queue first.
///
/// Small enough that an injected fiber waits a handful of turns; large enough
/// that the shared queue's lock is not taken on every one.
const GLOBAL_INTERVAL: usize = 31;

/// Safepoints a fiber may spend before it is asked to give the worker back.
const BUDGET: u32 = 128;

thread_local! {
    /// What the fiber running on this worker has left before it should yield.
    ///
    /// Per worker rather than per fiber: it is refilled at every resume, so it
    /// describes this turn rather than this fiber, and a fiber that migrates
    /// gets whatever its new worker grants it.
    static REMAINING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Spends one safepoint. True when the fiber should give the worker back.
pub(crate) fn spend_safepoint() -> bool {
    REMAINING.with(|r| {
        let left = r.get();
        if left == 0 {
            return false;
        }
        r.set(left - 1);
        left == 1
    })
}

/// Grants a fresh budget, at the start of a turn.
fn refill() {
    REMAINING.with(|r| r.set(BUDGET));
}

/// Withdraws the budget, so a safepoint outside a fiber does nothing.
fn withdraw() {
    REMAINING.with(|r| r.set(0));
}

/// What the workers share.
struct Shared {
    /// Fibers nobody has a worker for yet: spawned from outside, or woken.
    queued: Mutex<VecDeque<Task>>,
    /// Wakes a parked worker.
    arrived: Condvar,
    stopping: AtomicBool,
    counts: Counts,
}

/// Cheap counters, so a bad result can say *why*.
///
/// `docs/design/scheduler.md` §14: without these a slow run says "a hundred
/// thousand connections is slow", and with them it says which queue was empty.
#[derive(Default)]
pub(crate) struct Counts {
    pub(crate) spawned: AtomicU64,
    pub(crate) completed: AtomicU64,
    pub(crate) resumes: AtomicU64,
    /// Resumes that ended because the fiber ran out of budget.
    pub(crate) preempted: AtomicU64,
    pub(crate) parks: AtomicU64,
}

impl Counts {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            spawned: self.spawned.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            resumes: self.resumes.load(Ordering::Relaxed),
            preempted: self.preempted.load(Ordering::Relaxed),
            parks: self.parks.load(Ordering::Relaxed),
        }
    }
}

/// What the counters said at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) spawned: u64,
    pub(crate) completed: u64,
    pub(crate) resumes: u64,
    pub(crate) preempted: u64,
    pub(crate) parks: u64,
}

thread_local! {
    /// The queue belonging to the worker on this thread.
    ///
    /// A fiber that spawns another puts it here, because the thing it just
    /// created is the thing this core's caches are warmest for.
    static LOCAL: std::cell::RefCell<Option<Arc<Mutex<VecDeque<Task>>>>> =
        const { std::cell::RefCell::new(None) };

    /// The scheduler this worker belongs to, so a fiber can spawn onto it.
    static SHARED: std::cell::RefCell<Option<Arc<Shared>>> =
        const { std::cell::RefCell::new(None) };
}

/// A pool of workers running fibers.
pub(crate) struct Scheduler {
    shared: Arc<Shared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Scheduler {
    /// Starts `workers` threads. Zero means one per available core.
    pub(crate) fn new(workers: usize) -> Scheduler {
        let workers = match workers {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        let shared = Arc::new(Shared {
            queued: Mutex::new(VecDeque::new()),
            arrived: Condvar::new(),
            stopping: AtomicBool::new(false),
            counts: Counts::default(),
        });

        let handles = (0..workers)
            .map(|index| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("khora-worker-{index}"))
                    .spawn(move || work(shared))
                    .expect("a worker thread")
            })
            .collect();

        Scheduler { shared, workers: handles }
    }

    /// Hands a fiber to the pool.
    pub(crate) fn spawn(&self, task: Task) {
        self.shared.counts.spawned.fetch_add(1, Ordering::Relaxed);
        inject(&self.shared, task);
    }

    pub(crate) fn counts(&self) -> Snapshot {
        self.shared.counts.snapshot()
    }

    /// Waits until every fiber handed over has finished.
    ///
    /// For tests and for a program's own shutdown. A production scheduler
    /// wants this to be a nursery's business rather than the pool's, which is
    /// what 11C's integration with `Fibers::wait` is for.
    pub(crate) fn drain(&self) {
        loop {
            let counts = self.shared.counts.snapshot();
            let queued = self.shared.queued.lock().expect("the shared queue").len();
            if counts.completed == counts.spawned && queued == 0 {
                return;
            }
            std::thread::yield_now();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.arrived.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Puts a fiber on the shared queue and wakes somebody.
fn inject(shared: &Arc<Shared>, task: Task) {
    shared.queued.lock().expect("the shared queue").push_back(task);
    shared.arrived.notify_one();
}

/// Puts a fiber where the running worker will reach it soonest.
///
/// Falls back to the shared queue when there is no worker — a fiber spawned
/// from the program's own computation rather than from inside another fiber.
pub(crate) fn schedule(task: Task) -> bool {
    let local = LOCAL.with(|l| l.borrow().clone());
    let shared = SHARED.with(|s| s.borrow().clone());
    match (local, shared) {
        (Some(queue), Some(shared)) => {
            shared.counts.spawned.fetch_add(1, Ordering::Relaxed);
            queue.lock().expect("a local queue").push_back(task);
            // Somebody else may be parked with nothing to do while this
            // worker's queue grows. Until 11D can steal, the wake is what
            // keeps that from being permanent.
            shared.arrived.notify_one();
            true
        }
        _ => false,
    }
}

/// One worker's whole life.
fn work(shared: Arc<Shared>) {
    let local: Arc<Mutex<VecDeque<Task>>> = Arc::new(Mutex::new(VecDeque::new()));
    LOCAL.with(|l| *l.borrow_mut() = Some(local.clone()));
    SHARED.with(|s| *s.borrow_mut() = Some(shared.clone()));

    let mut turn = 0usize;
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        match next(&shared, &local, &mut turn) {
            Some(task) => run(&shared, &local, task),
            None => {
                if !park(&shared, &local) {
                    break;
                }
            }
        }
    }

    LOCAL.with(|l| *l.borrow_mut() = None);
    SHARED.with(|s| *s.borrow_mut() = None);
}

/// The next fiber for this worker, or nothing.
fn next(
    shared: &Arc<Shared>,
    local: &Arc<Mutex<VecDeque<Task>>>,
    turn: &mut usize,
) -> Option<Task> {
    *turn = turn.wrapping_add(1);

    // Every so often, the shared queue first. Otherwise a fiber that spawns in
    // a loop keeps this worker in its own queue for ever and nothing injected
    // from outside is ever seen.
    if *turn % GLOBAL_INTERVAL == 0 {
        if let Some(task) = shared.queued.lock().expect("the shared queue").pop_front() {
            return Some(task);
        }
    }
    if let Some(task) = local.lock().expect("a local queue").pop_front() {
        return Some(task);
    }
    shared.queued.lock().expect("the shared queue").pop_front()
}

/// Gives a fiber a turn, and decides what happens to it afterwards.
fn run(shared: &Arc<Shared>, local: &Arc<Mutex<VecDeque<Task>>>, mut task: Task) {
    shared.counts.resumes.fetch_add(1, Ordering::Relaxed);
    refill();
    let outcome = task.resume();
    let spent = REMAINING.with(|r| r.get()) == 0;
    withdraw();

    match outcome {
        Ran::Finished => {
            shared.counts.completed.fetch_add(1, Ordering::Relaxed);
        }
        Ran::Suspended => {
            if spent {
                shared.counts.preempted.fetch_add(1, Ordering::Relaxed);
            }
            // Back to the end of this worker's queue: it suspended, so
            // everything else waiting goes first.
            local.lock().expect("a local queue").push_back(task);
        }
    }
}

/// Sleeps until something arrives or the pool stops. False means stop.
fn park(shared: &Arc<Shared>, local: &Arc<Mutex<VecDeque<Task>>>) -> bool {
    shared.counts.parks.fetch_add(1, Ordering::Relaxed);
    let mut queued = shared.queued.lock().expect("the shared queue");
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            return false;
        }
        if !queued.is_empty() || !local.lock().expect("a local queue").is_empty() {
            return true;
        }
        // A timeout rather than a plain wait, because until 11D can steal, a
        // fiber sitting in another worker's local queue produces no
        // notification this worker will ever see. Waking to look is the cheap
        // stand-in for stealing, and it goes away when stealing arrives.
        let (guard, _) = shared
            .arrived
            .wait_timeout(queued, std::time::Duration::from_millis(1))
            .expect("the shared queue");
        queued = guard;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coro::suspend;
    use std::sync::atomic::AtomicUsize;

    /// Spends a safepoint the way generated code will, and yields if the
    /// budget says to.
    fn safepoint() {
        if spend_safepoint() {
            suspend();
        }
    }

    #[test]
    fn a_fiber_handed_over_runs_and_finishes() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = ran.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        pool.drain();

        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert_eq!(pool.counts().completed, 1);
    }

    #[test]
    fn many_fibers_all_finish() {
        const COUNT: usize = 500;
        let done = Arc::new(AtomicUsize::new(0));

        let pool = Scheduler::new(4);
        for _ in 0..COUNT {
            let counter = done.clone();
            pool.spawn(Task::new(move || {
                for _ in 0..8 {
                    suspend();
                }
                counter.fetch_add(1, Ordering::SeqCst);
            }));
        }
        pool.drain();

        assert_eq!(done.load(Ordering::SeqCst), COUNT);
        assert_eq!(pool.counts().completed, COUNT as u64);
    }

    /// The point of having more than one worker. Fibers that each take a
    /// little wall-clock time must overlap, or this is a very complicated way
    /// to run things one at a time.
    #[test]
    fn fibers_run_on_more_than_one_worker() {
        let seen = Arc::new(Mutex::new(std::collections::HashSet::new()));

        let pool = Scheduler::new(4);
        for _ in 0..32 {
            let names = seen.clone();
            pool.spawn(Task::new(move || {
                let name = std::thread::current().name().unwrap_or_default().to_string();
                names.lock().unwrap().insert(name);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }));
        }
        pool.drain();

        let workers = seen.lock().unwrap().len();
        assert!(workers > 1, "everything ran on one worker: {:?}", seen.lock().unwrap());
    }

    /// A fiber that suspends without a budget still comes back. This is the
    /// ordinary I/O shape, before there is any I/O.
    #[test]
    fn a_suspended_fiber_is_resumed_until_it_finishes() {
        let steps = Arc::new(AtomicUsize::new(0));
        let counter = steps.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            for _ in 0..64 {
                counter.fetch_add(1, Ordering::SeqCst);
                suspend();
            }
        }));
        pool.drain();

        assert_eq!(steps.load(Ordering::SeqCst), 64);
    }

    /// **The reason safepoints exist.** A fiber with no cancellation points
    /// and no I/O must not own its worker: another fiber on the same worker
    /// has to get a turn.
    #[test]
    fn a_looping_fiber_does_not_starve_the_one_behind_it() {
        let spinner_ran = Arc::new(AtomicUsize::new(0));
        let other_ran = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        // One worker, so the two can only interleave by preemption.
        let pool = Scheduler::new(1);

        let spun = spinner_ran.clone();
        let halt = stop.clone();
        pool.spawn(Task::new(move || {
            while !halt.load(Ordering::Relaxed) {
                spun.fetch_add(1, Ordering::Relaxed);
                safepoint();
            }
        }));

        let other = other_ran.clone();
        pool.spawn(Task::new(move || {
            other.fetch_add(1, Ordering::SeqCst);
        }));

        // The second fiber cannot run at all unless the first gives the worker
        // back, so waiting for it is the assertion.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while other_ran.load(Ordering::SeqCst) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the spinning fiber never yielded: it ran {} times",
                spinner_ran.load(Ordering::Relaxed)
            );
            std::thread::yield_now();
        }

        stop.store(true, Ordering::Relaxed);
        pool.drain();
        assert!(pool.counts().preempted > 0, "the budget should have run out at least once");
    }

    /// A safepoint outside a fiber does nothing at all, so the same generated
    /// code is correct in a program that never spawns one.
    #[test]
    fn a_safepoint_off_a_worker_is_inert() {
        assert!(!spend_safepoint(), "no budget, so nothing to spend");
        safepoint();
    }

    /// Injected fibers must not be starved by a worker that keeps refilling
    /// its own queue.
    #[test]
    fn a_fiber_injected_from_outside_is_not_starved() {
        let spinning = Arc::new(AtomicBool::new(true));
        let arrived = Arc::new(AtomicUsize::new(0));

        let pool = Scheduler::new(1);
        let halt = spinning.clone();
        pool.spawn(Task::new(move || {
            while halt.load(Ordering::Relaxed) {
                suspend();
            }
        }));

        let landed = arrived.clone();
        pool.spawn(Task::new(move || {
            landed.fetch_add(1, Ordering::SeqCst);
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while arrived.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "the injected fiber never ran");
            std::thread::yield_now();
        }
        spinning.store(false, Ordering::Relaxed);
        pool.drain();
    }

    #[test]
    fn dropping_the_pool_stops_its_workers() {
        let pool = Scheduler::new(3);
        pool.spawn(Task::new(|| {}));
        pool.drain();
        drop(pool);
        // Reaching here means every worker joined rather than spinning.
    }

    /// A fiber's identity has to be right on whichever worker picked it up,
    /// which is the whole reason `current` came before any of this.
    #[test]
    fn a_fiber_keeps_its_identity_across_workers() {
        let matched = Arc::new(AtomicUsize::new(0));
        let pool = Scheduler::new(4);

        for _ in 0..64 {
            let hits = matched.clone();
            let task = Task::new(move || {
                let first = crate::current::current(|f| f.id());
                for _ in 0..8 {
                    suspend();
                    assert_eq!(crate::current::current(|f| f.id()), first, "identity changed");
                }
                hits.fetch_add(1, Ordering::SeqCst);
            });
            pool.spawn(task);
        }
        pool.drain();
        assert_eq!(matched.load(Ordering::SeqCst), 64);
    }
}
