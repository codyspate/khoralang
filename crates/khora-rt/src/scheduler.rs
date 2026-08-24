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

use crate::coro::{suspend, Ran, Task};
use crate::wait::{Timers, Wait, NOTIFIED, WAITING};

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

/// A loop went round again.
///
/// Emitted by code generation at every back-edge of a program that can spawn.
/// **A safepoint, not a cancellation point**: it cannot fail, nothing unwinds
/// through it, and a fiber that yields here is not thereby cancellable. That
/// distinction is what lets an infallible loop be preempted at all —
/// `docs/design/scheduler.md` §1.
///
/// Off a worker this is a thread-local load and a compare, because the budget
/// is only ever granted around a resume. A program that never spawns emits no
/// calls to it at all.
#[unsafe(no_mangle)]
pub extern "C" fn khora_safepoint() {
    if spend_safepoint() {
        crate::coro::suspend();
    }
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
    /// Fibers waiting for something, by fiber id.
    ///
    /// **The same lock covers parking and waking**, which is what closes the
    /// last gap in `crate::wait`'s invariant. A worker reads a suspended
    /// fiber's state and files the task here without letting go; a waker sets
    /// the state and takes the task out without letting go. Neither can see a
    /// half-finished version of the other.
    parked: Mutex<std::collections::HashMap<usize, Task>>,
    /// Deadlines, and the fibers waiting on them.
    timers: Mutex<Timers>,
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
    /// Fibers currently waiting for something.
    pub(crate) waiting: AtomicU64,
    /// Wakes delivered, whatever woke them.
    pub(crate) wakes: AtomicU64,
    /// Timers that fired.
    pub(crate) timers_fired: AtomicU64,
    /// Wakes that arrived before the fiber suspended, so it never waited.
    pub(crate) wakes_before_waiting: AtomicU64,
}

impl Counts {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            spawned: self.spawned.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            resumes: self.resumes.load(Ordering::Relaxed),
            preempted: self.preempted.load(Ordering::Relaxed),
            parks: self.parks.load(Ordering::Relaxed),
            waiting: self.waiting.load(Ordering::Relaxed),
            wakes: self.wakes.load(Ordering::Relaxed),
            timers_fired: self.timers_fired.load(Ordering::Relaxed),
            wakes_before_waiting: self.wakes_before_waiting.load(Ordering::Relaxed),
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
    pub(crate) waiting: u64,
    pub(crate) wakes: u64,
    pub(crate) timers_fired: u64,
    pub(crate) wakes_before_waiting: u64,
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
            parked: Mutex::new(std::collections::HashMap::new()),
            timers: Mutex::new(Timers::default()),
            arrived: Condvar::new(),
            stopping: AtomicBool::new(false),
            counts: Counts::default(),
        });

        let mut handles: Vec<std::thread::JoinHandle<()>> = (0..workers)
            .map(|index| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("khora-worker-{index}"))
                    .spawn(move || work(shared))
                    .expect("a worker thread")
            })
            .collect();

        // One thread for deadlines. A `sleep` that blocked a worker would
        // undo the entire phase, so time is something the scheduler waits on
        // rather than something a fiber does. `scheduler.md` §6.
        let ticking = shared.clone();
        handles.push(
            std::thread::Builder::new()
                .name("khora-timers".to_string())
                .spawn(move || tick(ticking))
                .expect("the timer thread"),
        );

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

    /// How many fibers are waiting for something right now.
    pub(crate) fn waiting(&self) -> u64 {
        self.shared.counts.waiting.load(Ordering::Relaxed)
    }

    /// Cancels a fiber, and wakes it if it is asleep.
    ///
    /// **Setting the flag is not enough.** Cancellation is observed by running
    /// code, which was sufficient while every fiber was a thread the operating
    /// system would schedule regardless. A fiber waiting on something that will
    /// never happen never runs again, so a flag it never reads is a leak with
    /// good intentions rather than a cancellation. `docs/design/scheduler.md`
    /// §5.
    ///
    /// The order matters: the flag first, then the wake. A fiber woken before
    /// the flag was set would look, see nothing, and go back to sleep.
    ///
    /// What happens after is unchanged, which is the point — the fiber resumes,
    /// observes the cancellation at its next cancellation point, and unwinds
    /// through ordinary Khora finalizers. The scheduler only guarantees it gets
    /// another chance to look.
    pub(crate) fn cancel_fiber(&self, fiber: usize) {
        let asleep =
            self.shared.parked.lock().expect("the parked fibers").get(&fiber).map(|t| t.fiber().clone());
        let Some(state) = asleep else { return };

        state.cancel();
        // Its deadline is no longer interesting, and a hundred thousand
        // cancelled sleepers would otherwise hold the heap open.
        self.shared.timers.lock().expect("the timers").forget(fiber);
        wake(&self.shared, fiber, state.wait());
    }

    /// Wakes a fiber by id, from outside.
    pub(crate) fn wake_fiber(&self, fiber: usize) {
        let state = self.shared.parked.lock().expect("the parked fibers")
            .get(&fiber)
            .map(|t| t.fiber().clone());
        if let Some(state) = state {
            wake(&self.shared, fiber, state.wait());
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

/// Suspends the running fiber until somebody wakes it.
///
/// Returns false when there is no scheduler to wait on — a fiber running
/// outside a pool, or the program's own computation — because there would be
/// nobody to wake it and sleeping for ever is worse than not sleeping.
///
/// **A wake that arrives before the suspension is not lost.** `declare` refuses
/// to wait when one is already pending, which is the first half of
/// `crate::wait`'s invariant; the worker handling the suspension closes the
/// second half.
pub(crate) fn park_current() -> bool {
    let Some(shared) = SHARED.with(|s| s.borrow().clone()) else { return false };
    let waiting = crate::current::current(|fiber| {
        if fiber.wait().declare() {
            true
        } else {
            // Somebody woke this fiber before it managed to wait. Take the
            // notification and carry on running.
            fiber.wait().take_notification();
            false
        }
    });
    if !waiting {
        shared.counts.wakes_before_waiting.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    shared.counts.waiting.fetch_add(1, Ordering::Relaxed);
    suspend();
    true
}

/// Suspends the running fiber until `at`.
pub(crate) fn sleep_until(at: std::time::Instant) -> bool {
    let Some(shared) = SHARED.with(|s| s.borrow().clone()) else { return false };
    let id = crate::current::current(|fiber| fiber.id());
    shared.timers.lock().expect("the timers").add(at, id);
    park_current()
}

/// Makes a waiting fiber runnable.
///
/// Safe to call whatever the fiber is doing: a wake for something not waiting
/// leaves a notification, which the next attempt to wait consumes instead of
/// sleeping.
// Private, because `Shared` is: a waker outside this module goes through
// `Scheduler::wake_fiber`, and a reactor will go through the same door.
fn wake(shared: &Arc<Shared>, fiber: usize, state: &Wait) {
    shared.counts.wakes.fetch_add(1, Ordering::Relaxed);

    // Under the same lock the worker parks with, so a wake cannot land between
    // the worker reading the state and filing the task.
    let mut parked = shared.parked.lock().expect("the parked fibers");
    if !state.wake() {
        // It was not waiting. The notification stands and whoever tries to
        // wait next will take it instead of sleeping.
        return;
    }
    let Some(task) = parked.remove(&fiber) else {
        // Suspended but not filed yet: its worker still holds it and will see
        // `NOTIFIED` when it looks.
        return;
    };
    drop(parked);

    state.running();
    shared.counts.waiting.fetch_sub(1, Ordering::Relaxed);
    inject(shared, task);
}

/// Wakes every fiber whose deadline has passed, for ever.
fn tick(shared: Arc<Shared>) {
    while !shared.stopping.load(Ordering::Acquire) {
        let now = std::time::Instant::now();
        let due = shared.timers.lock().expect("the timers").expired(now);
        if !due.is_empty() {
            shared.counts.timers_fired.fetch_add(due.len() as u64, Ordering::Relaxed);
        }
        for id in due {
            // The fiber may have been woken by something else and moved on, in
            // which case its state says so and the wake is a no-op.
            if let Some(state) = registered(&shared, id) {
                wake(&shared, id, state.wait());
            }
        }

        // Until the next deadline, or a short while if there is none. A
        // condvar the timer thread could wait on is the refinement; this is
        // one thread sleeping, not a worker.
        let nap = shared
            .timers
            .lock()
            .expect("the timers")
            .next_deadline()
            .map(|at| at.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or(std::time::Duration::from_millis(1))
            .min(std::time::Duration::from_millis(1));
        std::thread::sleep(nap);
    }
}

/// The wait state of a parked fiber, if it is one.
fn registered(shared: &Arc<Shared>, fiber: usize) -> Option<Arc<crate::current::Fiber>> {
    shared.parked.lock().expect("the parked fibers").get(&fiber).map(|t| t.fiber().clone())
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
            // Why it suspended is written in its wait state, and reading it
            // has to happen under the parking lock: a wake landing between the
            // read and the filing would find nothing to move.
            let mut parked = shared.parked.lock().expect("the parked fibers");
            match task.fiber().wait().peek() {
                WAITING => {
                    parked.insert(task.fiber().id(), task);
                }
                state => {
                    // `NOTIFIED` means a wake arrived while it was suspending,
                    // so it is runnable rather than waiting. `RUNNING` means it
                    // only yielded for fairness.
                    if state == NOTIFIED {
                        task.fiber().wait().running();
                        shared.counts.waiting.fetch_sub(1, Ordering::Relaxed);
                    }
                    drop(parked);
                    // Back to the end of this worker's queue: it gave the
                    // worker up, so everything already waiting goes first.
                    local.lock().expect("a local queue").push_back(task);
                }
            }
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

    // --- waiting -----------------------------------------------------------

    /// A fiber sleeps on a deadline and the scheduler wakes it. Nothing blocks
    /// a worker: this is the shape all of 11C's I/O will take.
    #[test]
    fn a_sleeping_fiber_is_woken_by_its_deadline() {
        let woke = Arc::new(AtomicUsize::new(0));
        let counter = woke.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            sleep_until(std::time::Instant::now() + std::time::Duration::from_millis(20));
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        pool.drain();

        assert_eq!(woke.load(Ordering::SeqCst), 1);
        assert!(pool.counts().timers_fired >= 1, "{:?}", pool.counts());
    }

    /// The point of a scheduler over threads: a worker with a sleeping fiber
    /// on it is free to run something else.
    #[test]
    fn a_worker_runs_others_while_one_fiber_sleeps() {
        let others = Arc::new(AtomicUsize::new(0));

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(|| {
            sleep_until(std::time::Instant::now() + std::time::Duration::from_millis(80));
        }));
        for _ in 0..16 {
            let counter = others.clone();
            pool.spawn(Task::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }));
        }

        // They must all finish long before the sleeper's deadline.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(60);
        while others.load(Ordering::SeqCst) < 16 {
            assert!(
                std::time::Instant::now() < deadline,
                "one sleeping fiber blocked the worker: {} of 16 ran",
                others.load(Ordering::SeqCst)
            );
            std::thread::yield_now();
        }
        pool.drain();
    }

    /// Waking by hand, which is what a reactor will do when a socket becomes
    /// readable.
    #[test]
    fn a_parked_fiber_is_woken_from_outside() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = ran.clone();
        let id = Arc::new(AtomicUsize::new(0));
        let mine = id.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            mine.store(crate::current::current(|f| f.id()), Ordering::SeqCst);
            park_current();
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        // Wait for it to be parked, then wake it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pool.waiting() == 0 {
            assert!(std::time::Instant::now() < deadline, "it never parked");
            std::thread::yield_now();
        }
        assert_eq!(ran.load(Ordering::SeqCst), 0, "it should still be waiting");

        pool.wake_fiber(id.load(Ordering::SeqCst));
        pool.drain();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(pool.counts().wakes >= 1);
    }

    /// **The second half of the invariant, end to end.** A wake that arrives
    /// before the fiber suspends must not be consumed — the fiber carries on
    /// rather than sleeping for ever on something that already happened.
    ///
    /// Arranged by waking a fiber's own id from inside it, immediately before
    /// it parks: the notification is already pending when `park_current` runs.
    #[test]
    fn a_wake_that_beats_the_park_does_not_strand_the_fiber() {
        let finished = Arc::new(AtomicUsize::new(0));
        let counter = finished.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            // Deliver the wake to ourselves first. Nothing is parked, so this
            // leaves a notification rather than queueing anything.
            crate::current::current(|f| f.wait().wake());
            // Which `park_current` must take instead of sleeping.
            park_current();
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        // No timer and no waker: if the notification were lost this never ends.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while finished.load(Ordering::SeqCst) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the fiber is stranded: the wake before the park was consumed"
            );
            std::thread::yield_now();
        }
        pool.drain();
        assert!(pool.counts().wakes_before_waiting >= 1, "{:?}", pool.counts());
    }

    /// Many fibers, many deadlines, all across several workers.
    #[test]
    fn many_sleeping_fibers_all_wake() {
        const COUNT: usize = 400;
        let woke = Arc::new(AtomicUsize::new(0));

        let pool = Scheduler::new(4);
        for n in 0..COUNT {
            let counter = woke.clone();
            let delay = std::time::Duration::from_millis((n % 20) as u64);
            pool.spawn(Task::new(move || {
                sleep_until(std::time::Instant::now() + delay);
                counter.fetch_add(1, Ordering::SeqCst);
            }));
        }
        pool.drain();
        assert_eq!(woke.load(Ordering::SeqCst), COUNT);
    }

    /// A fiber that sleeps repeatedly exercises park and wake over and over,
    /// which is where a lost wakeup would show as a hang rather than a wrong
    /// answer.
    #[test]
    fn a_fiber_can_sleep_many_times() {
        let rounds = Arc::new(AtomicUsize::new(0));
        let counter = rounds.clone();

        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            for _ in 0..50 {
                sleep_until(std::time::Instant::now());
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }));
        pool.drain();
        assert_eq!(rounds.load(Ordering::SeqCst), 50);
    }

    /// **The phase's criterion, at a tenth of scale.** Ten thousand fibers all
    /// waiting at once on two workers, then all woken.
    ///
    /// A tenth because this belongs in the ordinary suite. The full hundred
    /// thousand was measured separately and is in `docs/design/scheduler.md`:
    /// 418 MB resident, about 4,240 bytes each, and every one of them woke.
    ///
    /// What it guards is that waiting is *cheap and correct together*. A
    /// scheduler that parked fibers but lost one wake in ten thousand would
    /// hang here rather than fail an assertion, which is why the loop below
    /// has a deadline rather than waiting for ever.
    #[test]
    fn ten_thousand_fibers_wait_at_once_and_all_wake() {
        const COUNT: usize = 10_000;
        let woke = Arc::new(AtomicUsize::new(0));
        let ids = Arc::new(Mutex::new(Vec::with_capacity(COUNT)));

        let pool = Scheduler::new(2);
        for _ in 0..COUNT {
            let counter = woke.clone();
            let seen = ids.clone();
            pool.spawn(Task::new(move || {
                seen.lock().unwrap().push(crate::current::current(|f| f.id()));
                park_current();
                counter.fetch_add(1, Ordering::Relaxed);
            }));
        }

        // Every one of them parked, and none finished.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while (pool.waiting() as usize) < COUNT {
            assert!(
                std::time::Instant::now() < deadline,
                "only {} of {COUNT} parked",
                pool.waiting()
            );
            std::thread::yield_now();
        }
        assert_eq!(woke.load(Ordering::Relaxed), 0, "nothing should have finished");

        for id in ids.lock().unwrap().iter() {
            pool.wake_fiber(*id);
        }
        pool.drain();

        assert_eq!(woke.load(Ordering::Relaxed), COUNT, "{:?}", pool.counts());
        assert_eq!(pool.waiting(), 0, "nothing left waiting: {:?}", pool.counts());
    }

    /// A fiber asleep on something that will never happen still has to be
    /// cancellable, or a nursery closing over one waits for ever.
    #[test]
    fn cancelling_a_sleeping_fiber_wakes_it_to_notice() {
        let noticed = Arc::new(AtomicUsize::new(0));
        let counter = noticed.clone();
        let id = Arc::new(AtomicUsize::new(0));
        let mine = id.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            mine.store(crate::current::current(|f| f.id()), Ordering::SeqCst);
            // Nothing will ever wake this on its own merits.
            park_current();
            if crate::current::current(|f| f.is_cancelled()) {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pool.waiting() == 0 {
            assert!(std::time::Instant::now() < deadline, "it never parked");
            std::thread::yield_now();
        }

        pool.cancel_fiber(id.load(Ordering::SeqCst));
        pool.drain();

        assert_eq!(
            noticed.load(Ordering::SeqCst),
            1,
            "a cancelled sleeper must wake and see it: {:?}",
            pool.counts()
        );
    }

    /// A sleeper cancelled before its deadline must not be left in the timer
    /// heap: at scale that is a hundred thousand dead entries.
    #[test]
    fn cancelling_a_sleeper_forgets_its_deadline() {
        let id = Arc::new(AtomicUsize::new(0));
        let mine = id.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            mine.store(crate::current::current(|f| f.id()), Ordering::SeqCst);
            sleep_until(std::time::Instant::now() + std::time::Duration::from_secs(3_600));
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pool.waiting() == 0 {
            assert!(std::time::Instant::now() < deadline, "it never parked");
            std::thread::yield_now();
        }

        pool.cancel_fiber(id.load(Ordering::SeqCst));
        // Finishing at all is the assertion: an hour-long timer is still
        // pending, so this only returns because the cancellation woke it.
        pool.drain();
    }

    /// Off a scheduler there is nobody to wake anything, so sleeping would be
    /// for ever. Saying so beats hanging.
    #[test]
    fn parking_without_a_scheduler_is_refused() {
        assert!(!park_current(), "there is no worker here");
        assert!(!sleep_until(std::time::Instant::now()));
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
