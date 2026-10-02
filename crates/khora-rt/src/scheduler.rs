//! Workers, queues, and whose turn it is.
//!
//! Fibers get stacks in [`crate::coro`]; this is what runs them on more than
//! one core.
//!
//! # Where a worker looks for work
//!
//! Its own queue, then the shared one, then a victim's — [`steal`] takes half,
//! rounded up — then it parks. Local queues therefore live in [`Shared`] rather
//! than in a thread-local, because a thief has to be able to reach one.
//!
//! # Fairness has two halves
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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::coro::{suspend, Ran, Task};
use crate::reactor::{Interest, Reactor, Socket, Watch};
use crate::wait::{Timers, Wait, NOTIFIED, WAITING};

/// How many turns a worker takes before looking at the shared queue first.
///
/// Small enough that an injected fiber waits a handful of turns; large enough
/// that the shared queue's lock is not taken on every one.
const GLOBAL_INTERVAL: usize = 31;

/// Safepoints a fiber may spend before it is asked to give the worker back.
const BUDGET: u32 = 128;

/// How long a worker's own queue may be before a wake stops adding to it.
///
/// **What this prevents: one worker's queue growing without limit from
/// wakes.** A wake made from a fiber goes to the waker's own worker, and only
/// a thief moves it from there. A fiber that wakes a crowd -- a channel send
/// on the scheduler wakes every fiber parked on the channel -- puts the whole
/// crowd behind one worker, and past this length the rest go to the shared
/// queue and wake somebody.
///
/// **Chosen from a sweep, and set above what the sweep's workload reaches.**
/// On the TechEmpower server on four CPUs under 256 connections, bounds of 8
/// and 64 turned 25-70% of wakes back into injections and cost up to 30%
/// more CPU per request, while each of the four workers ran between 24.2% and
/// 25.6% of the turns at every bound, unbounded included: stealing already
/// spreads a crowd, so a low bound bought nothing. At 512 no wake reached the
/// bound, and the numbers matched unbounded. So this caps a pathological
/// crowd rather than balancing the ordinary one; `docs/design/scheduler.md`
/// has the table.
///
/// What it costs: a wake past the bound pays the condvar and the reactor
/// nudge that the local path exists to avoid. What it does not do: bound the
/// queue itself, which spawns grow without limit.
const WAKE_LOCAL_BOUND: usize = 512;

/// How many turns a busy worker runs between its own looks at the reactor.
///
/// **What this prevents: a thread handoff for every readiness on a busy
/// pool.** When only a separate thread watched sockets, each readiness it
/// found was a shared-queue push, a condvar signal and the worker taken off
/// its CPU and put back: on one CPU that was a context switch each way per
/// request, and more than half of a JSON request's CPU. A worker that looks
/// itself finds the readiness on the thread that will run the fiber, and puts
/// it on its own queue.
///
/// What it costs: a zero-timeout `epoll_wait` every eight turns, about a
/// microsecond, paid whether or not anything is ready. Taken from the
/// prototype that measured it; not swept.
const LOOK_EVERY: usize = 8;

/// How long the reactor may go without a worker looking at it before the
/// backstop thread looks instead.
///
/// **What this prevents: readiness nobody collects while every worker is
/// held.** A worker looks between turns, so a pool whose every worker is in
/// a long turn -- a fiber computing without a safepoint, or blocked in a
/// foreign call -- looks at nothing, and a fiber whose socket became ready
/// would wait for the first turn to end. The backstop injects what it finds,
/// which is what wakes a worker that is parked.
///
/// What it costs: the backstop thread wakes this often for as long as the
/// pool exists, busy or idle. What it does not do: run the fiber sooner. A
/// readiness that arrives while every worker is held is found up to this
/// late, and runs when the first worker is free.
const BACKSTOP_GAP: std::time::Duration = std::time::Duration::from_millis(2);

/// Whether a wake made by a fiber may go to its own worker's queue, from the
/// value of `KHORA_WAKE_LOCAL`.
///
/// On unless the variable is exactly `0`. Anything else, unset included,
/// leaves it on: the switch exists to rule the local path in or out of a
/// problem, and a typo that silently turned it off would rule it out of the
/// wrong one.
fn wake_local_from(value: Option<&str>) -> bool {
    value != Some("0")
}

thread_local! {
    /// What the fiber running on this worker has left before it should yield.
    ///
    /// Per worker rather than per fiber: it is refilled at every resume, so it
    /// describes this turn rather than this fiber, and a fiber that migrates
    /// gets whatever its new worker grants it.
    ///
    /// **The one thread-local here that does not need
    /// [`crate::current::current`]'s `#[inline(never)]` treatment**, and it is
    /// worth saying why rather than leaving it to look like an oversight. That
    /// rule exists because a fiber can change thread between two accesses in
    /// one function. This one is never read across a switch: `refill`, the
    /// budget check and `withdraw` all run inside `run`, on the worker, and
    /// `khora_safepoint` reaches it through an `extern "C"` boundary that
    /// forces the address to be computed afresh on every call. A worker's
    /// budget is its own, and only its own thread ever touches it.
    ///
    /// That matters because this is the one hot path in the file — the
    /// safepoint is emitted at every loop back-edge, behind the poll word in
    /// [`crate::poll`], which is non-zero whenever a pool exists — and a call
    /// that cannot be inlined would show up where nothing else here would.
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
/// Emitted by code generation at every back-edge of a program that can spawn,
/// on the slow path of the load of [`crate::poll::khora_poll`]: reached only
/// while a scheduler pool exists or some fiber is canceled, because off a
/// pool there is no budget and this does nothing.
///
/// **A safepoint, not a cancellation point**: it cannot fail, nothing unwinds
/// through it, and a fiber that yields here is not thereby cancelable. That
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
    /// Every worker's own queue, indexed by worker.
    ///
    /// **Here rather than in a thread-local, because a thief has to reach its
    /// victim.** The thread-local stays as the fast path for a fiber spawning
    /// onto its own worker.
    locals: Vec<Arc<Mutex<VecDeque<Task>>>>,
    /// Every fiber this pool knows about, by id.
    ///
    /// **Separate from `parked`, and the separation is load-bearing.** A fiber
    /// that has suspended but whose worker has not yet filed it is in `parked`
    /// under neither key, so waking it through that map drops the wake and it
    /// sleeps for ever — `canceling_a_sleeping_fiber_wakes_it_to_notice`.
    ///
    /// A waker needs the *state* to set `NOTIFIED` on, and that exists from the
    /// moment the fiber does.
    live: Mutex<std::collections::HashMap<usize, Arc<crate::current::Fiber>, ById>>,
    /// Fibers waiting for something, by fiber id.
    ///
    /// **The same lock covers parking and waking**, which is what closes the
    /// last gap in `crate::wait`'s invariant. A worker reads a suspended
    /// fiber's state and files the task here without letting go; a waker sets
    /// the state and takes the task out without letting go. Neither can see a
    /// half-finished version of the other.
    parked: Mutex<std::collections::HashMap<usize, Task, ById>>,
    /// Deadlines, and the fibers waiting on them.
    timers: Mutex<Timers>,
    /// Wakes the timer thread when the first deadline arrives in an empty
    /// heap, or the pool stops. Waited on with `timers`' lock.
    ///
    /// **What this prevents: a thread waking a thousand times a second to
    /// find nothing.** A pool with no deadlines has nothing for the timer
    /// thread to do, and it sleeps here until it has.
    timer_added: Condvar,
    /// Sockets, and the fibers waiting on them.
    reactor: Reactor,
    /// Wakes a parked worker.
    arrived: Condvar,
    /// Held by the one idle worker currently waiting on the backend.
    ///
    /// **Exactly one, and that is the point:** every idle worker calling
    /// `epoll_wait` is a thundering herd. On a completion port all of them
    /// would be right, so who may block is a backend's business —
    /// `docs/design/scheduler.md` §10a.
    polling: AtomicBool,
    /// Whether a fiber's wake may go to its own worker's queue. See
    /// [`wake`].
    ///
    /// Read once, when the pool starts, so that one pool never runs both
    /// ways and a counter taken from it describes one of them.
    wake_local: bool,
    /// Turns each worker has given a fiber, indexed by worker.
    ///
    /// **The only record of how the work was shared out.** `resumes` is the
    /// total; a pool where one worker does it all and a pool where four share
    /// it read the same there, and that is the difference the local wake path
    /// could make. One cache line each, so counting a turn never contends with
    /// another worker counting one.
    turns: Vec<Turns>,
    /// Tasks that belong to no queue at this instant because somebody is
    /// carrying them between two.
    ///
    /// **The sixth place a fiber can be, and the audit is wrong without it.**
    /// A waker holds a task between `parked` and `inject`; a thief holds half a
    /// queue between two deques. Neither is a worker, so neither is bounded by
    /// the worker count, which is what `Audit::in_hand` would otherwise
    /// assume.
    in_transit: AtomicUsize,
    /// When the pool started, the origin of `last_look`.
    born: std::time::Instant,
    /// Microseconds after `born` at which a worker last finished looking at
    /// the reactor. The backstop reads it to decide whether anybody is.
    last_look: AtomicU64,
    /// Tests only: the backstop thread never looks, so a test can show that
    /// workers find readiness with nobody else watching.
    #[cfg(test)]
    backstop_off: AtomicBool,
    /// This pool's workers inside `Task::resume` right now.
    ///
    /// **Per pool, not the process's `coro::resuming_now`.** The soak's
    /// last check is that none of *its* workers is still inside a fiber, and
    /// the process-wide count also holds every other pool in the test
    /// binary: a test beside it keeping a worker in a spinning fiber failed
    /// the soak with its own pool settled. Under the same `cfg` as the
    /// process-wide count, so an ordinary release build pays nothing.
    #[cfg(any(debug_assertions, feature = "fiber-audit"))]
    resuming: AtomicUsize,
    stopping: AtomicBool,
    counts: Counts,
}

impl Shared {
    fn turns(&self) -> Vec<u64> {
        self.turns.iter().map(|t| t.0.load(Ordering::Relaxed)).collect()
    }
}

/// Everywhere a fiber can be, at one instant. See [`Scheduler::audit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Audit {
    pub(crate) spawned: u64,
    pub(crate) completed: u64,
    /// Waiting for any worker.
    pub(crate) queued: usize,
    /// Waiting for one particular worker, summed over all of them.
    pub(crate) local: usize,
    /// Suspended, filed by the worker that suspended them.
    pub(crate) parked: usize,
    /// Known to the pool: spawned and not yet finished.
    pub(crate) live: usize,
    /// Being carried between two queues by somebody who is not a worker.
    pub(crate) in_transit: usize,
    /// What the counter thinks is waiting, which `parked` should agree with
    /// once nothing is in flight.
    pub(crate) waiting: u64,
    /// Deadlines registered.
    pub(crate) timers: usize,
    /// Sockets registered.
    pub(crate) watched: usize,
}

impl Audit {
    /// Fibers begun and not yet finished.
    pub(crate) fn outstanding(&self) -> i64 {
        self.spawned as i64 - self.completed as i64
    }

    /// Fibers nobody has filed: being run by a worker, or carried by a waker.
    ///
    /// **Only meaningful when the pool is quiescent.** Five places are read
    /// without a lock across them, so a task moving between two of them is
    /// counted twice and this reads negative on a busy pool with nothing wrong.
    /// Making it sound while busy costs an atomic on the hottest path in the
    /// file, to catch what [`Audit::settled`] catches free once the pool goes
    /// quiet.
    pub(crate) fn in_hand(&self) -> i64 {
        self.outstanding() - (self.queued + self.local + self.parked + self.in_transit) as i64
    }

    /// Whether the pool is empty and self-consistent.
    ///
    /// Everything begun has finished, nothing is filed anywhere, nothing is
    /// registered, and the two independent accounts of who is waiting — the
    /// parked map and the counter — agree at zero.
    pub(crate) fn settled(&self) -> bool {
        self.outstanding() == 0
            && self.queued == 0
            && self.local == 0
            && self.parked == 0
            && self.live == 0
            && self.in_transit == 0
            && self.waiting == 0
            && self.timers == 0
            && self.watched == 0
    }
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
    /// Sockets that became ready.
    pub(crate) sockets_ready: AtomicU64,
    /// Wakes that arrived before the fiber suspended, so it never waited.
    pub(crate) wakes_before_waiting: AtomicU64,
    /// Sweeps over the other workers looking for something to take.
    pub(crate) steals_attempted: AtomicU64,
    /// Sweeps that found something.
    pub(crate) steals_succeeded: AtomicU64,
    /// Deadlines registered.
    ///
    /// Beside `timers_fired` because the pair is what `docs/design/scheduler.md`
    /// §6 needs in order to decide whether the heap should stay a heap — and
    /// because the two of them currently disagree with the clock in a way
    /// nobody has explained. See the note there.
    pub(crate) timers_added: AtomicU64,
    /// Deadlines that came due for a fiber that had already finished.
    pub(crate) timers_dead: AtomicU64,
    /// Fibers actually moved from one worker to another.
    ///
    /// Separate from the sweep counts because a sweep takes half a queue: a
    /// high attempt count with a low success rate is workers spinning, and a
    /// high fibers-moved with few sweeps is a pool sharing out a burst.
    pub(crate) fibers_stolen: AtomicU64,
    /// Wakes that went to the waking fiber's own worker's queue.
    pub(crate) wakes_local: AtomicU64,
    /// Wakes that went to the shared queue, with a condvar signal and a
    /// reactor nudge.
    ///
    /// Beside `wakes_local` because together they say which path a workload
    /// takes, and `wakes` counts attempts, including the ones that found
    /// nothing to move.
    pub(crate) wakes_injected: AtomicU64,
    /// Wakes that could have been local and went to the shared queue because
    /// the waker's queue was at [`WAKE_LOCAL_BOUND`].
    ///
    /// Included in `wakes_injected`. Zero under a workload means the bound
    /// never mattered to it.
    pub(crate) wakes_over_bound: AtomicU64,
    /// Looks at the reactor made by a worker's own thread, busy or idle.
    ///
    /// Beside `backstop_polls` because the pair says who is finding
    /// readiness: a busy server whose backstop count climbs has workers held
    /// by long turns, and every wake the backstop delivers is a condvar
    /// signal the worker's own look would not have paid.
    pub(crate) worker_polls: AtomicU64,
    /// Looks at the reactor made by the backstop thread, because no worker
    /// had looked for [`BACKSTOP_GAP`].
    pub(crate) backstop_polls: AtomicU64,
    /// Times the timer thread looked at its heap.
    ///
    /// **What it exposes: a timer thread that ticks with nothing to wait
    /// for.** An idle pool should leave this still; a count that climbs by
    /// a thousand a second with no timers registered is the thread polling
    /// instead of sleeping.
    pub(crate) timer_passes: AtomicU64,
}

/// One worker's count of turns, alone on its cache line.
#[derive(Default)]
#[repr(align(64))]
struct Turns(AtomicU64);

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
            timers_dead: self.timers_dead.load(Ordering::Relaxed),
            timers_added: self.timers_added.load(Ordering::Relaxed),
            sockets_ready: self.sockets_ready.load(Ordering::Relaxed),
            wakes_before_waiting: self.wakes_before_waiting.load(Ordering::Relaxed),
            steals_attempted: self.steals_attempted.load(Ordering::Relaxed),
            steals_succeeded: self.steals_succeeded.load(Ordering::Relaxed),
            fibers_stolen: self.fibers_stolen.load(Ordering::Relaxed),
            wakes_local: self.wakes_local.load(Ordering::Relaxed),
            wakes_injected: self.wakes_injected.load(Ordering::Relaxed),
            wakes_over_bound: self.wakes_over_bound.load(Ordering::Relaxed),
            worker_polls: self.worker_polls.load(Ordering::Relaxed),
            backstop_polls: self.backstop_polls.load(Ordering::Relaxed),
            timer_passes: self.timer_passes.load(Ordering::Relaxed),
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
    pub(crate) timers_dead: u64,
    pub(crate) timers_added: u64,
    pub(crate) sockets_ready: u64,
    pub(crate) wakes_before_waiting: u64,
    pub(crate) steals_attempted: u64,
    pub(crate) steals_succeeded: u64,
    pub(crate) fibers_stolen: u64,
    pub(crate) wakes_local: u64,
    pub(crate) wakes_injected: u64,
    pub(crate) wakes_over_bound: u64,
    pub(crate) worker_polls: u64,
    pub(crate) backstop_polls: u64,
    pub(crate) timer_passes: u64,
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

/// This worker's queue, if it is a worker at all.
///
/// **`#[inline(never)]`, for the reason in [`crate::current::current`].** A
/// fiber can change worker between two reads of a thread-local in one
/// function, and a cached base address would then name the wrong worker's
/// queue — which would put a spawned fiber on a queue nobody owns.
#[inline(never)]
fn local_queue() -> Option<Arc<Mutex<VecDeque<Task>>>> {
    LOCAL.with(|l| l.borrow().clone())
}

/// The pool this worker belongs to. See [`local_queue`].
#[inline(never)]
fn shared_pool() -> Option<Arc<Shared>> {
    SHARED.with(|s| s.borrow().clone())
}

/// Attaches this thread to a pool, or detaches it. See [`local_queue`].
#[inline(never)]
fn attach(local: Option<Arc<Mutex<VecDeque<Task>>>>, shared: Option<Arc<Shared>>) {
    LOCAL.with(|l| *l.borrow_mut() = local);
    SHARED.with(|s| *s.borrow_mut() = shared);
}

/// A pool of workers running fibers.
pub(crate) struct Scheduler {
    shared: Arc<Shared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Scheduler {
    /// Starts `workers` threads. Zero means one per available core.
    ///
    /// Reads `KHORA_WAKE_LOCAL` here, once, for the life of the pool.
    pub(crate) fn new(workers: usize) -> Scheduler {
        let wake_local = wake_local_from(std::env::var("KHORA_WAKE_LOCAL").ok().as_deref());
        Scheduler::started(workers, wake_local)
    }

    /// [`Scheduler::new`], with the wake path chosen by the caller rather
    /// than the environment.
    ///
    /// For tests, which run in one process and cannot each set a variable
    /// the others read.
    pub(crate) fn started(workers: usize, wake_local: bool) -> Scheduler {
        Scheduler::started_with(workers, wake_local, false)
    }

    /// [`Scheduler::started`] with the backstop thread off from its first
    /// instant. Setting `backstop_off` after `started` returned raced the
    /// thread: it sleeps [`BACKSTOP_GAP`] and then looks, and on a loaded
    /// host that first look could land before the store, so a test counting
    /// `backstop_polls == 0` failed about one run in thirty.
    #[cfg(test)]
    pub(crate) fn started_without_backstop(workers: usize, wake_local: bool) -> Scheduler {
        Scheduler::started_with(workers, wake_local, true)
    }

    fn started_with(workers: usize, wake_local: bool, backstop_off: bool) -> Scheduler {
        #[cfg(not(test))]
        let _ = backstop_off;
        let workers = match workers {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        // Built before the threads, because each worker's queue has to be
        // reachable by every other worker from the first instant one exists.
        let locals: Vec<Arc<Mutex<VecDeque<Task>>>> =
            (0..workers).map(|_| Arc::new(Mutex::new(VecDeque::new()))).collect();

        // Before any worker exists, because a worker grants a safepoint budget
        // and a back-edge that has not been told a pool exists skips the
        // safepoint. `crate::poll`.
        crate::poll::pool_started();
        let shared = Arc::new(Shared {
            queued: Mutex::new(VecDeque::new()),
            locals,
            live: Mutex::new(std::collections::HashMap::default()),
            parked: Mutex::new(std::collections::HashMap::default()),
            timers: Mutex::new(Timers::default()),
            timer_added: Condvar::new(),
            reactor: Reactor::default(),
            arrived: Condvar::new(),
            polling: AtomicBool::new(false),
            wake_local,
            turns: (0..workers).map(|_| Turns::default()).collect(),
            in_transit: AtomicUsize::new(0),
            born: std::time::Instant::now(),
            last_look: AtomicU64::new(0),
            #[cfg(test)]
            backstop_off: AtomicBool::new(backstop_off),
            #[cfg(any(debug_assertions, feature = "fiber-audit"))]
            resuming: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            counts: Counts::default(),
        });

        let mut handles: Vec<std::thread::JoinHandle<()>> = (0..workers)
            .map(|index| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("khora-worker-{index}"))
                    .spawn(move || {
                        // A coroutine that overflows faults on this thread,
                        // so this is where the report needs its room.
                        let _room = crate::stack::guard_this_thread();
                        work(shared, index)
                    })
                    .expect("a worker thread")
            })
            .collect();

        // One thread that looks at sockets when no worker has lately: every
        // worker held in a long turn still leaves somebody watching. See
        // `watch`.
        let watching = shared.clone();
        handles.push(
            std::thread::Builder::new()
                .name("khora-reactor".to_string())
                .spawn(move || watch(watching))
                .expect("the reactor thread"),
        );

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

        // How the counters are read out of a program that does not end: a
        // server runs until it is killed, so there is no moment to print them
        // at. `KHORA_SCHEDULER_REPORT=500` prints to stderr every five hundred
        // milliseconds, and the difference between two lines is the
        // interesting part. `docs/design/scheduler.md` §14.
        if let Some(every) = std::env::var("KHORA_SCHEDULER_REPORT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            let watching = shared.clone();
            let started = std::thread::Builder::new()
                .name("khora-report".to_string())
                .spawn(move || {
                    let gap = std::time::Duration::from_millis(every.max(1));
                    while !watching.stopping.load(Ordering::Acquire) {
                        std::thread::sleep(gap);
                        eprintln!(
                            "khora-scheduler {:?} queued={} local={} parked={} live={} turns={:?}",
                            watching.counts.snapshot(),
                            watching.queued.lock().expect("the shared queue").len(),
                            watching
                                .locals
                                .iter()
                                .map(|q| q.lock().expect("a local queue").len())
                                .sum::<usize>(),
                            watching.parked.lock().expect("the parked fibers").len(),
                            watching.live.lock().expect("the live fibers").len(),
                            watching.turns(),
                        );
                    }
                });
            if let Ok(handle) = started {
                handles.push(handle);
            }
        }

        Scheduler { shared, workers: handles }
    }

    /// Hands a fiber to the pool.
    pub(crate) fn spawn(&self, task: Task) {
        self.shared.counts.spawned.fetch_add(1, Ordering::Relaxed);
        remember(&self.shared, &task);
        inject(&self.shared, task);
    }

    pub(crate) fn counts(&self) -> Snapshot {
        self.shared.counts.snapshot()
    }

    /// Turns each worker has given a fiber so far, by worker.
    pub(crate) fn turns(&self) -> Vec<u64> {
        self.shared.turns()
    }

    /// How many of this pool's workers are inside a fiber right now, or
    /// `None` where the count is not compiled in -- for the reason
    /// `coro::resuming_now` answers `None`: a caller must not read "nobody
    /// is counting" as zero.
    pub(crate) fn resuming_now(&self) -> Option<usize> {
        #[cfg(any(debug_assertions, feature = "fiber-audit"))]
        {
            Some(self.shared.resuming.load(Ordering::Acquire))
        }
        #[cfg(not(any(debug_assertions, feature = "fiber-audit")))]
        {
            None
        }
    }

    /// Every parked fiber, with what it parked for: its id, its [`Why`], the
    /// address of what it waits on, and its wait state.
    ///
    /// **For a dump, and for nothing that has to be right while the pool
    /// moves.** The list is read under the parking lock, so it is exact at
    /// one instant, and stale the instant after. `None` where the audit is
    /// not compiled in, for the reason [`Scheduler::resuming_now`] gives.
    pub(crate) fn stranded(&self) -> Option<Vec<(usize, Why, usize, u8)>> {
        #[cfg(any(debug_assertions, feature = "fiber-audit"))]
        {
            let parked = self.shared.parked.lock().expect("the parked fibers");
            let mut out: Vec<(usize, Why, usize, u8)> = parked
                .iter()
                .map(|(id, task)| {
                    let (why, on) = task.fiber().waits_on();
                    (*id, why, on, task.fiber().wait().peek())
                })
                .collect();
            out.sort_unstable_by_key(|entry| entry.0);
            Some(out)
        }
        #[cfg(not(any(debug_assertions, feature = "fiber-audit")))]
        {
            None
        }
    }

    /// Waits until every fiber handed over has finished.
    ///
    /// For tests and for a program's own shutdown; a nursery's `Fibers::wait`
    /// is what a program actually uses.
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

    /// Waits for the pool to be empty and self-consistent, or gives up.
    ///
    /// **`drain` and this are different questions.** `drain` waits for every
    /// fiber to finish; a pool can satisfy that while still holding registered
    /// state, because a fiber woken before its deadline leaves the deadline
    /// behind and the timer thread only discards it when it comes due. So a
    /// pool with unexpired timers is finished but not yet quiescent, and a
    /// test that asserted [`Audit::settled`] the instant `drain` returned
    /// would be asserting the wrong thing.
    ///
    /// Returns the last audit taken either way, so a caller that ran out of
    /// patience can say what it was still waiting for.
    pub(crate) fn settle(&self, patience: std::time::Duration) -> Audit {
        let until = std::time::Instant::now() + patience;
        loop {
            let audit = self.audit();
            if audit.settled() || std::time::Instant::now() >= until {
                return audit;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Everywhere a fiber can be at one instant, for 11F.
    ///
    /// **The point is the arithmetic, not any single number.** A `Task` is
    /// moved, so at any moment it is in exactly one place: the shared queue, a
    /// worker's queue, the parked map, somebody's hand, or gone. `in_hand` is
    /// derived rather than counted, because a worker holds its task in a stack
    /// frame — so a derivation that goes negative, or fails to reach zero when
    /// everything has finished, means a task was lost or run twice.
    pub(crate) fn audit(&self) -> Audit {
        // **The order of these reads is the whole soundness argument.** There
        // is no lock held across all of them — taking one would mean ordering
        // five mutexes against every other path in this file — so the audit is
        // skewed by whatever happens while it runs. The skew is made
        // one-sided on purpose:
        //
        //   - `completed` is read *first*. A fiber counted here has already
        //     been popped from every queue, so nothing counted as completed
        //     can also be found filed below.
        //   - `spawned` is read *last*. `spawn` counts a fiber before it
        //     injects it, so everything found filed below is already included.
        //
        // That makes `outstanding` an over-estimate and never an under-one,
        // so `in_hand` can read high on a busy pool but never negative — and a
        // negative answer is therefore real. The other order gives `in_hand:
        // -1` under a concurrent spawner, which looks exactly like the bug it
        // is not.
        let completed = self.shared.counts.completed.load(Ordering::Acquire);
        let queued = self.shared.queued.lock().expect("the shared queue").len();
        let local: usize = self
            .shared
            .locals
            .iter()
            .map(|q| q.lock().expect("a local queue").len())
            .sum();
        let parked = self.shared.parked.lock().expect("the parked fibers").len();
        let live = self.shared.live.lock().expect("the live fibers").len();
        let in_transit = self.shared.in_transit.load(Ordering::Acquire);
        let timers = self.shared.timers.lock().expect("the timers").len();
        let watched = self.shared.reactor.len();
        let waiting = self.shared.counts.waiting.load(Ordering::Acquire);
        let spawned = self.shared.counts.spawned.load(Ordering::Acquire);
        Audit {
            spawned,
            completed,
            queued,
            local,
            parked,
            live,
            in_transit,
            waiting,
            timers,
            watched,
        }
    }

    /// Cancels a fiber, and wakes it if it is asleep.
    ///
    /// **Setting the flag is not enough.** Cancellation is observed by running
    /// code, and a fiber waiting on something that will never happen never runs
    /// again — so a flag it cannot reach is a leak with good intentions.
    /// `docs/design/scheduler.md` §5.
    ///
    /// The order matters: flag, then wake. A fiber woken before the flag was
    /// set looks, sees nothing, and goes back to sleep. All this guarantees is
    /// another chance to look; the fiber still observes the cancellation at its
    /// own next cancellation point.
    pub(crate) fn cancel_fiber(&self, fiber: usize) {
        self.stop_fiber(fiber, crate::current::Stop::Cancel);
    }

    /// [`Scheduler::cancel_fiber`], for either kind of stop. The timers,
    /// reactor and wake are the same for both, because a forced fiber asleep
    /// in shielded cleanup is exactly the one that must be woken to see it.
    pub(crate) fn stop_fiber(&self, fiber: usize, stop: crate::current::Stop) {
        let Some(state) = state_of(&self.shared, fiber) else { return };

        state.stop(stop);
        // Otherwise a hundred thousand canceled sleepers hold the heap and
        // the watch list open.
        self.shared.timers.lock().expect("the timers").forget(fiber);
        self.shared.reactor.forget(fiber);
        wake(&self.shared, fiber, state.wait());
    }

    /// How many sockets fibers are waiting on.
    pub(crate) fn watching(&self) -> usize {
        self.shared.reactor.len()
    }

    /// Wakes a fiber by id, from outside.
    ///
    /// **Only a fiber this pool has been told about** — one that has been
    /// through `spawn`. A wake for an unknown id is dropped rather than
    /// remembered, because remembering notifications for fibers that may never
    /// arrive is an unbounded leak. So a caller that publishes an id before
    /// handing over the task strands the fiber; publish it after.
    pub(crate) fn wake_fiber(&self, fiber: usize) {
        if let Some(state) = state_of(&self.shared, fiber) {
            wake(&self.shared, fiber, state.wait());
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.arrived.notify_all();
        // Under the timers' lock, for the reason in `sleep_until`: a timer
        // thread that read `stopping` as false is already waiting when this
        // gets the lock, and one that has not read it yet will see it true.
        {
            let _timers = self.shared.timers.lock().expect("the timers");
            self.shared.timer_added.notify_all();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        crate::poll::pool_stopped();
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
    park_current_for(Why::Unnamed, 0)
}

/// What a parked fiber is waiting for, for a hung pool's dump.
///
/// **Said at the park, because nowhere else knows.** By the time a dump
/// runs, the parked map holds a `Task` and the counters hold a number; which
/// channel, join or pool queue the fiber went to sleep on was known only to
/// the call that parked it. A soak that hung with `waiting: 6` could not say
/// whether six wakes were lost or six fibers had nobody to wake them, and the
/// two have opposite fixes.
///
/// Recorded only where the fiber audit is compiled in (debug, or
/// `fiber-audit`); elsewhere `park_current_for` ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    /// A bare [`park_current`]: whoever holds the fiber's id or a [`Waker`]
    /// for it is the one to wake it.
    Unnamed,
    /// [`sleep_until`]'s deadline.
    Deadline,
    /// A socket, through [`wait_until_ready_by`].
    Socket,
    /// Room in a full channel or hand-off; `on` is its queue's address.
    ChannelRoom,
    /// A value in an empty channel or hand-off; `on` is its queue's address.
    ChannelValue,
    /// Room in the blocking pool's queue.
    BlockingRoom,
    /// A blocking-pool job's result; `on` is the result slot.
    BlockingResult,
    /// Another fiber finishing; `on` is its completion latch.
    Join,
}

/// [`park_current`], saying what for and on what, so that a hang can be read.
/// See [`Why`].
pub(crate) fn park_current_for(why: Why, on: usize) -> bool {
    let Some(shared) = shared_pool() else { return false };
    #[cfg(any(debug_assertions, feature = "fiber-audit"))]
    crate::current::current(|fiber| fiber.set_waits_on(why, on));
    #[cfg(not(any(debug_assertions, feature = "fiber-audit")))]
    let _ = (why, on);
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
    // After the increment, and before the suspension that lets anybody else
    // observe this fiber, so every decrement below has something to take.
    crate::current::current(|fiber| fiber.wait().start_counting());
    suspend();
    true
}

/// Suspends the running fiber until `at`.
pub(crate) fn sleep_until(at: std::time::Instant) -> bool {
    let Some(shared) = shared_pool() else { return false };
    let id = crate::current::current(|fiber| fiber.id());
    {
        let mut timers = shared.timers.lock().expect("the timers");
        let first = timers.len() == 0;
        timers.add(at, id);
        // Under the lock, so the timer thread cannot be between finding the
        // heap empty and waiting: it is either waiting, and this wakes it, or
        // it has not looked yet and will find this deadline.
        if first {
            shared.timer_added.notify_one();
        }
    }
    park_current_for(Why::Deadline, 0)
}

/// Makes one particular fiber runnable, from anywhere, later.
///
/// **For work that leaves the scheduler entirely.** A blocking-pool thread
/// holds one across a foreign call it cannot interrupt, and hands the fiber
/// back when the call returns — without needing to know what `Shared` is,
/// which is what keeps that type private.
pub(crate) struct Waker {
    shared: Arc<Shared>,
    fiber: usize,
}

impl Waker {
    /// The fiber this wakes, so a list of wakers can find one fiber's entry.
    pub(crate) fn fiber(&self) -> usize {
        self.fiber
    }

    /// Makes the fiber runnable. Safe whatever it is doing, and safe if it has
    /// already finished — a wake for a fiber the pool has forgotten is a
    /// lookup that finds nothing.
    pub(crate) fn wake(&self) {
        if let Some(state) = state_of(&self.shared, self.fiber) {
            wake(&self.shared, self.fiber, state.wait());
        }
    }
}

/// A waker for whatever is running here, if it is a fiber on a worker.
///
/// `None` off a worker or outside a fiber, which is the caller's signal that
/// there is no worker to give back and nothing to be gained by suspending.
pub(crate) fn waker_for_current() -> Option<Waker> {
    if !crate::coro::on_a_fiber() {
        return None;
    }
    let shared = shared_pool()?;
    let fiber = crate::current::current(|f| f.id());
    Some(Waker { shared, fiber })
}

/// Makes a waiting fiber runnable.
///
/// Safe to call whatever the fiber is doing: a wake for something not waiting
/// leaves a notification, which the next attempt to wait consumes instead of
/// sleeping.
// Private, because `Shared` is: a waker outside this module goes through
// `Scheduler::wake_fiber`, and a reactor will go through the same door.
/// Makes a parked fiber runnable again.
///
/// # The ownership invariant, which this function is where you break
///
/// **A `Task` has exactly one owner at every instant.** It is a moved value,
/// so at any moment it is in exactly one place -- the shared queue, one
/// worker's queue, the parked map, a worker's stack frame, or gone -- and
/// [`Shared::audit`] is the arithmetic that says so. Two owners means the same
/// fiber stack resumed from two threads, which is not a data race that shows
/// up as a wrong number: it is two threads running one coroutine.
///
/// A wake is the one operation that could produce a second owner, because it
/// arrives from outside and knows nothing about what the fiber is doing. Any
/// number of them can arrive for one fiber, from any number of threads, at any
/// point in the park. **Two claims stop it, and both are needed:**
///
/// 1. `state.wake()` is a compare-exchange from `WAITING`. Exactly one caller
///    sees `true`; every other one returns above, having left the notification
///    standing for whoever waits next.
/// 2. `parked.remove(&fiber)` takes the task out of the map. Exactly one
///    caller gets `Some`, and it is holding the only copy from that point on.
///
/// Neither alone is enough. Without the first, two wakes both find the task
/// and one of them injects a fiber that is already running. Without the
/// second, the winner of the compare-exchange could race the *worker* that is
/// still filing the task -- which is why the `Some`-less branch below returns
/// rather than treating a missing entry as an error.
///
/// Both happen under `shared.parked`'s lock, which is the same lock the worker
/// parks with, so a wake cannot land between the worker reading the state and
/// filing the task.
///
/// `in_transit` is the third place a task can be, and exists so the audit
/// still balances while this function is carrying one between the map and a
/// queue.
///
/// `an_avalanche_of_wakes_resumes_a_fiber_once` is the test.
fn wake(shared: &Arc<Shared>, fiber: usize, state: &Wait) {
    deliver(shared, fiber, state, None)
}

/// [`wake`], for readiness a worker's own thread found by looking at the
/// reactor: the task goes on `mine`, that worker's queue, rather than
/// through [`inject`]. See [`look`].
fn wake_found(shared: &Arc<Shared>, fiber: usize, state: &Wait, mine: &Arc<Mutex<VecDeque<Task>>>) {
    deliver(shared, fiber, state, Some(mine))
}

/// The body of [`wake`] and [`wake_found`]: the two claims, then the queue.
fn deliver(
    shared: &Arc<Shared>,
    fiber: usize,
    state: &Wait,
    found_by: Option<&Arc<Mutex<VecDeque<Task>>>>,
) {
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
        // `NOTIFIED` when it looks. Not an error, and not something to assert
        // against -- it is the second claim doing its job, and the worker is
        // the owner in this branch.
        return;
    };
    // Won both claims, so this thread is the sole owner until `inject`. A
    // second winner would have to have come through `state.wake()` returning
    // true twice for one park, which is the thing the compare-exchange makes
    // impossible.
    debug_assert!(
        !parked.contains_key(&fiber),
        "the parked map held two entries for fiber {fiber}, so two threads own one task",
    );
    // In this thread's hands from here until `inject`, and in no queue.
    shared.in_transit.fetch_add(1, Ordering::AcqRel);
    drop(parked);

    state.running();
    if state.stop_counting() {
        shared.counts.waiting.fetch_sub(1, Ordering::Relaxed);
    }
    let task = match found_by {
        Some(mine) => onto_own_queue(shared, mine, task),
        None => wake_locally(shared, task),
    };
    let task = match task {
        None => {
            shared.counts.wakes_local.fetch_add(1, Ordering::Relaxed);
            shared.in_transit.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        Some(task) => task,
    };
    shared.counts.wakes_injected.fetch_add(1, Ordering::Relaxed);
    inject(shared, task);
    shared.in_transit.fetch_sub(1, Ordering::AcqRel);
}

/// Puts a woken task on the waking fiber's own worker's queue, or hands it
/// back for [`inject`].
///
/// **What this prevents: a condvar signal, a reactor nudge and an idle
/// worker's wake-up for every fiber-to-fiber handoff.** A channel send, a
/// pool lease and a join are each a fiber waking another, and `inject` pays
/// for three things none of them needs: the shared queue's lock, a condvar
/// signal, and -- while a worker is idle in `epoll_wait` -- a write to the
/// nudge socket and the read that drains it. On a database request that was
/// nineteen system calls where two would do. The worker the
/// waker is running on reaches its own queue as soon as the waker suspends,
/// which for a fiber about to wait for a reply is at once.
///
/// **Only from a fiber, on a worker of this pool.** A wake from anywhere else
/// is handed back:
///
///   - the backstop and timer threads, and a thread that is not the
///     scheduler's at all, have no queue of their own;
///   - a worker's own thread outside a fiber has no fiber whose suspension
///     it is about to run, except for readiness it found itself, which
///     [`wake_found`] handles;
///   - a fiber of another pool would put the task on a worker that does not
///     know it.
///
/// **Nothing is signaled, which is sound for a worker that goes to sleep and
/// not, on its own, for one whose thread blocks.** The push happens on the
/// worker's own thread, before `run` returns to `work`, and `next` looks at
/// this queue before `serve_io` or `park` can put the worker to sleep, under
/// that queue's lock: no instant has the task queued and the worker committed
/// to sleeping without having looked. But a waker that then blocks the
/// thread -- on a `Shared` cell the woken fiber holds, say -- keeps the worker
/// from `next` until the woken fiber has run, and only another worker can run
/// it. What finds it is the steal every worker makes on its
/// `GLOBAL_INTERVAL` tick, busy or idle, from a worker that has not started
/// a fiber since the thief's previous tick; see [`next`].
///
/// What it costs: nothing else is told the task exists. Idle workers find it
/// by stealing when their parking timeout sends them round again -- a
/// millisecond from `park`, up to ten from `serve_io` -- and busy ones within
/// two of their ticks, about sixty turns. So a waker that keeps its worker
/// after waking holds the woken fiber back by up to that long, and
/// [`WAKE_LOCAL_BOUND`] caps how many it can hold back at once. The same holds
/// for readiness a worker finds with [`look`], which goes on its queue by the
/// same rule.
fn wake_locally(shared: &Arc<Shared>, task: Task) -> Option<Task> {
    if !shared.wake_local || !crate::coro::on_a_fiber() {
        return Some(task);
    }
    let (Some(queue), Some(mine)) = (local_queue(), shared_pool()) else {
        return Some(task);
    };
    if !Arc::ptr_eq(&mine, shared) {
        return Some(task);
    }
    onto_own_queue(shared, &queue, task)
}

/// Pushes a woken task onto a worker's own queue, or hands it back for
/// [`inject`] when the queue is at [`WAKE_LOCAL_BOUND`] or the local path is
/// switched off.
fn onto_own_queue(
    shared: &Arc<Shared>,
    queue: &Arc<Mutex<VecDeque<Task>>>,
    task: Task,
) -> Option<Task> {
    if !shared.wake_local {
        return Some(task);
    }
    let mut queue = queue.lock().expect("a local queue");
    if queue.len() >= WAKE_LOCAL_BOUND {
        drop(queue);
        shared.counts.wakes_over_bound.fetch_add(1, Ordering::Relaxed);
        return Some(task);
    }
    queue.push_back(task);
    None
}

/// Suspends the running fiber until `socket` is ready.
///
/// Called by an operation that has already tried and would have blocked. The
/// caller retries when this returns — readiness is a hint, not a promise, and
/// a second `EWOULDBLOCK` simply comes back here.
///
/// Returns false off a scheduler, where there is nobody to watch anything and
/// the caller should block the thread as it always did.
pub(crate) fn wait_until_ready(socket: Socket, interest: Interest) -> bool {
    matches!(wait_until_ready_by(socket, interest, None), Waited::Ready)
}

/// How a wait for a socket ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waited {
    /// Worth trying the operation again. Readiness is a hint, not a promise —
    /// a spurious wake reports this too, and the retry is what settles it.
    Ready,
    /// The deadline passed first.
    TimedOut,
    /// No scheduler, so there was no worker to give back and nothing to wait
    /// on. The caller must block the thread itself.
    Unscheduled,
}

/// Suspends the running fiber until `socket` is ready or `deadline` passes.
///
/// **This is what replaces `SO_RCVTIMEO`, and it has to.** A socket the reactor
/// drives is non-blocking, so the kernel's receive timeout can never fire: it
/// applies only to a call that would have blocked, and none of them do any
/// more. A server relying on it to shed a slow client would park a fiber on
/// that client for ever. `crate::net` keeps the meaning by reporting a timeout
/// the way the kernel used to. `docs/design/scheduler.md` §6.
///
/// The deadline is absolute, so a spurious wake can re-enter this with the same
/// one and not extend it.
pub(crate) fn wait_until_ready_by(
    socket: Socket,
    interest: Interest,
    deadline: Option<std::time::Instant>,
) -> Waited {
    let Some(shared) = shared_pool() else { return Waited::Unscheduled };
    let fiber = crate::current::current(|f| f.id());

    if let Some(at) = deadline {
        if std::time::Instant::now() >= at {
            return Waited::TimedOut;
        }
    }

    // **The deadline rides on the watch rather than the timer heap.** It used
    // to be pushed onto `Timers`, which cost a global mutex and a heap
    // insertion for every read that would block — one apiece, measured. The
    // reactor already holds this wait; it can hold when to give up on it.
    shared.reactor.register(Watch { socket, interest, fiber, deadline });
    let parked = park_current_for(Why::Socket, 0);
    // Woken by something else — a cancellation, or another registration — so
    // this one must come off, or a later readiness wakes a fiber that has
    // stopped caring about this socket.
    shared.reactor.forget(fiber);

    if !parked {
        return Waited::Unscheduled;
    }
    match deadline {
        Some(at) if std::time::Instant::now() >= at => Waited::TimedOut,
        _ => Waited::Ready,
    }
}

/// Wakes every fiber whose socket has become ready, when no worker is looking.
///
/// **A backstop, not the poller.** Workers look at the reactor themselves --
/// every [`LOOK_EVERY`] turns while busy, and whenever idle -- and deliver
/// what they find to their own queues. This thread looks only when no worker
/// has for [`BACKSTOP_GAP`]: every worker held in a long turn, or blocked in
/// a call. What it finds goes through [`inject`], since it has no queue of
/// its own and a parked worker has to be woken to run it.
///
/// A zero-timeout look, never a blocking one: blocking here would put this
/// thread back in `epoll_wait` beside the workers, and a readiness it
/// collected would be the thread handoff the workers' looks exist to avoid.
fn watch(shared: Arc<Shared>) {
    while !shared.stopping.load(Ordering::Acquire) {
        std::thread::sleep(BACKSTOP_GAP);
        #[cfg(test)]
        if shared.backstop_off.load(Ordering::Acquire) {
            continue;
        }
        let since = micros_since(&shared).saturating_sub(shared.last_look.load(Ordering::Acquire));
        if since < BACKSTOP_GAP.as_micros() as u64 {
            continue;
        }
        // Somebody is in `serve_io`'s wait, which delivers whatever arrives.
        if shared.polling.swap(true, Ordering::AcqRel) {
            continue;
        }
        let ready = shared.reactor.poll(std::time::Duration::ZERO);
        shared.polling.store(false, Ordering::Release);
        shared.counts.backstop_polls.fetch_add(1, Ordering::Relaxed);
        if ready.is_empty() {
            continue;
        }
        shared.counts.sockets_ready.fetch_add(ready.len() as u64, Ordering::Relaxed);
        for id in ready {
            if let Some(state) = state_of(&shared, id) {
                wake(&shared, id, state.wait());
            }
        }
    }
}

/// Microseconds since the pool started, the clock `last_look` is kept in.
fn micros_since(shared: &Shared) -> u64 {
    shared.born.elapsed().as_micros() as u64
}

/// Wakes every fiber whose deadline has passed, for ever.
///
/// **Asleep on `timer_added` while there are no deadlines**, rather than
/// waking every millisecond to find the heap empty. A deadline added to an
/// empty heap wakes it ([`sleep_until`]), and so does the pool stopping.
///
/// While there are deadlines it still wakes at least every millisecond,
/// because a deadline added ahead of the soonest one does not wake it.
fn tick(shared: Arc<Shared>) {
    loop {
        shared.counts.timer_passes.fetch_add(1, Ordering::Relaxed);
        let now = std::time::Instant::now();
        let due = shared.timers.lock().expect("the timers").expired(now);
        if !due.is_empty() {
            shared.counts.timers_fired.fetch_add(due.len() as u64, Ordering::Relaxed);
        }
        for id in due {
            match state_of(&shared, id) {
                // The fiber may have been woken by something else and moved
                // on, in which case its state says so and this is a no-op.
                Some(state) => wake(&shared, id, state.wait()),
                // A deadline whose fiber was released early and then
                // finished, so the entry sat in the heap until it came due.
                // Bounded rather than a leak — and a heap mostly full of these
                // is the cue to stop using a heap.
                None => {
                    shared.counts.timers_dead.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // Checked and waited on under one lock, so a deadline added or a
        // stop made between the look and the wait still wakes this.
        let mut timers = shared.timers.lock().expect("the timers");
        loop {
            if shared.stopping.load(Ordering::Acquire) {
                return;
            }
            if timers.len() > 0 {
                break;
            }
            timers = shared.timer_added.wait(timers).expect("the timers");
        }
        let nap = timers
            .next_deadline()
            .map(|at| at.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or_default()
            .min(std::time::Duration::from_millis(1));
        drop(timers);
        std::thread::sleep(nap);
    }
}

/// Records a fiber so it can be woken before anybody is holding its task.
fn remember(shared: &Arc<Shared>, task: &Task) {
    shared.live.lock().expect("the live fibers").insert(task.fiber().id(), task.fiber().clone());
}

/// The state of a fiber this pool knows about.
fn state_of(shared: &Arc<Shared>, fiber: usize) -> Option<Arc<crate::current::Fiber>> {
    shared.live.lock().expect("the live fibers").get(&fiber).cloned()
}

/// Puts a fiber on the shared queue and wakes somebody.
fn inject(shared: &Arc<Shared>, task: Task) {
    shared.queued.lock().expect("the shared queue").push_back(task);
    shared.arrived.notify_one();
    // **And the worker waiting on the backend, which the condvar cannot
    // reach.** A worker that is idle now waits in `poll` rather than on
    // `arrived`, so a task arriving has to be able to end that wait too —
    // otherwise the one worker best placed to run it is the last to hear.
    // `docs/design/scheduler.md` §10a: a task becoming runnable and a socket
    // becoming ready are the same kind of event, so they end the same wait.
    shared.reactor.nudge();
}

/// Puts a fiber where the running worker will reach it soonest.
///
/// Falls back to the shared queue when there is no worker — a fiber spawned
/// from the program's own computation rather than from inside another fiber.
pub(crate) fn schedule(task: Task) -> bool {
    let local = local_queue();
    let shared = shared_pool();
    match (local, shared) {
        (Some(queue), Some(shared)) => {
            shared.counts.spawned.fetch_add(1, Ordering::Relaxed);
            remember(&shared, &task);
            queue.lock().expect("a local queue").push_back(task);
            // A parked worker cannot steal from a queue it is not awake to
            // look at, so pushing locally still has to nudge.
            shared.arrived.notify_one();
            true
        }
        _ => false,
    }
}

/// One worker's whole life.
fn work(shared: Arc<Shared>, me: usize) {
    let local = shared.locals[me].clone();
    attach(Some(local.clone()), Some(shared.clone()));

    let mut turn = 0usize;
    let mut seen = vec![u64::MAX; shared.locals.len()];
    let mut since_look = 0usize;
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        // **A busy worker looks for I/O between turns.** It never reaches
        // `serve_io` while it has work, so without this every readiness on a
        // loaded pool was found by another thread and handed across. See
        // [`LOOK_EVERY`].
        since_look += 1;
        if since_look >= LOOK_EVERY {
            since_look = 0;
            look(&shared, &local);
        }
        match next(&shared, &local, me, &mut turn, &mut seen) {
            Some(task) => {
                shared.turns[me].0.fetch_add(1, Ordering::Relaxed);
                run(&shared, &local, task)
            }
            // Out of work: a look that costs nothing to wait for comes before
            // the one that waits, since what it finds is runnable now.
            None if look(&shared, &local) => {
                since_look = 0;
                continue;
            }
            // **An idle worker looks for I/O itself rather than sleeping while
            // another thread does it.** Readiness discovered here is readiness
            // discovered by the thread that is about to run the fiber, which
            // is one operating-system handoff shorter than being told. Only
            // one worker does this; the rest park as they always did.
            None if serve_io(&shared, &local) => continue,
            None => {
                if !park(&shared, &local) {
                    break;
                }
            }
        }
    }

    attach(None, None);
}

/// The next fiber for this worker, or nothing.
///
/// `seen` is each worker's turn count at this worker's last tick, which is
/// how the tick tells a stuck worker from a busy one.
fn next(
    shared: &Arc<Shared>,
    local: &Arc<Mutex<VecDeque<Task>>>,
    me: usize,
    turn: &mut usize,
    seen: &mut [u64],
) -> Option<Task> {
    *turn = turn.wrapping_add(1);

    // Every so often, the shared queue first. Otherwise a fiber that spawns in
    // a loop keeps this worker in its own queue for ever and nothing injected
    // from outside is ever seen.
    //
    // **And then a steal, busy or not: what rescues a fiber whose worker's
    // thread is blocked.** A worker that runs a fiber blocking the thread --
    // on a `Shared` cell's mutex, stdout's lock, a DNS lookup, any foreign
    // call -- does not come back to its queue until the call returns, and
    // what is on that queue may be the very fiber the call is waiting for: a
    // fiber woken there, preempted there holding the lock, or spawned there.
    // A worker steals when it runs out of work, and on a loaded pool none
    // does, so without this that fiber waited for ever: a valid program
    // deadlocked. Catching every call that can block a thread instead is not
    // possible from here, since a foreign function is one of them.
    //
    // **Only from a worker that looks stuck**: one that has not started a
    // fiber since this worker's last tick. Stealing from every worker on
    // every tick moved half a busy queue each time with nobody blocked, and
    // cost up to 45% of the pool's turns with 2,000 runnable fibers on four
    // workers. A worker running one long fiber with no safepoints also looks
    // stuck; taking its queue is right then too, since it is not getting to
    // it.
    //
    // What it costs: reading one counter per other worker every
    // `GLOBAL_INTERVAL` turns. What it does not do: run the fiber sooner than
    // two of this worker's ticks after the thread blocked, so a blocked
    // worker's queue waits up to about sixty turns on each other worker. And
    // it needs one worker whose thread is not blocked: with one worker, or
    // every worker blocked on the same lock, nothing is left to steal.
    if *turn % GLOBAL_INTERVAL == 0 {
        if let Some(task) = shared.queued.lock().expect("the shared queue").pop_front() {
            return Some(task);
        }
        if let Some(task) = steal_where(shared, local, me, |victim| {
            let now = shared.turns[victim].0.load(Ordering::Relaxed);
            let stuck = seen[victim] == now;
            seen[victim] = now;
            stuck
        }) {
            return Some(task);
        }
    }
    if let Some(task) = local.lock().expect("a local queue").pop_front() {
        return Some(task);
    }
    if let Some(task) = shared.queued.lock().expect("the shared queue").pop_front() {
        return Some(task);
    }
    steal_where(shared, local, me, |_| true)
}

/// Takes work from another worker, and returns one fiber to run now.
///
/// **Both ends of the deque are load-bearing.** The owner takes from the front
/// and a thief from the back, so the two contend for the lock but never for the
/// same fiber. Front-for-the-owner is also what keeps the queue FIFO: a fiber
/// spawning in a loop cannot bury one that arrived earlier, which a LIFO owner
/// would trade away for cache warmth.
///
/// **Half a queue, not one fiber**, or the thief is back contending on the next
/// tick and a pool sharing out a burst spends it all on lock traffic.
///
/// Victims are visited starting at `me + 1` rather than zero, so that idle
/// workers do not all descend on worker 0 together. `may_take` is asked about
/// each other worker in that order, before its queue is locked, until one
/// yields a fiber; a worker it refuses is passed over.
fn steal_where(
    shared: &Arc<Shared>,
    mine: &Arc<Mutex<VecDeque<Task>>>,
    me: usize,
    mut may_take: impl FnMut(usize) -> bool,
) -> Option<Task> {
    let workers = shared.locals.len();
    if workers < 2 {
        return None;
    }
    shared.counts.steals_attempted.fetch_add(1, Ordering::Relaxed);

    for offset in 1..workers {
        let victim = (me + offset) % workers;
        if !may_take(victim) {
            continue;
        }
        let mut taken = {
            let mut queue = shared.locals[victim].lock().expect("a local queue");
            let taken = take_half(&mut queue);
            // Out of the victim's queue and not yet in ours.
            shared.in_transit.fetch_add(taken.len(), Ordering::AcqRel);
            taken
            // The victim's lock goes here, before ours is taken. Two thieves
            // holding one queue each and reaching for the other's is a
            // deadlock, and this is the line that makes it impossible.
        };
        let Some(first) = taken.pop_front() else { continue };

        shared.counts.steals_succeeded.fetch_add(1, Ordering::Relaxed);
        let moved = taken.len() + 1;
        shared.counts.fibers_stolen.fetch_add(moved as u64, Ordering::Relaxed);
        if !taken.is_empty() {
            mine.lock().expect("a local queue").extend(taken);
        }
        // The rest are queued and `first` is about to be this worker's, which
        // is what `in_hand` counts.
        shared.in_transit.fetch_sub(moved, Ordering::AcqRel);
        return Some(first);
    }
    None
}

/// Removes the back half of `queue` and returns it, oldest of the half first.
///
/// Rounded up, so that stealing from a queue of one takes the one — an empty
/// steal from a non-empty victim would leave a fiber stranded behind a worker
/// that is busy.
fn take_half(queue: &mut VecDeque<Task>) -> VecDeque<Task> {
    let half = queue.len().div_ceil(2);
    if half == 0 {
        return VecDeque::new();
    }
    queue.split_off(queue.len() - half)
}

/// Gives a fiber a turn, and decides what happens to it afterwards.
fn run(shared: &Arc<Shared>, local: &Arc<Mutex<VecDeque<Task>>>, mut task: Task) {
    shared.counts.resumes.fetch_add(1, Ordering::Relaxed);
    refill();
    #[cfg(any(debug_assertions, feature = "fiber-audit"))]
    let outcome = {
        // A guard, as `coro::ResumedOnce` is, so that a fiber that panics
        // still leaves the count.
        struct Inside<'a>(&'a AtomicUsize);
        impl Drop for Inside<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Release);
            }
        }
        shared.resuming.fetch_add(1, Ordering::Relaxed);
        let _inside = Inside(&shared.resuming);
        task.resume()
    };
    #[cfg(not(any(debug_assertions, feature = "fiber-audit")))]
    let outcome = task.resume();
    let spent = REMAINING.with(|r| r.get()) == 0;
    withdraw();

    match outcome {
        Ran::Finished => {
            shared.counts.completed.fetch_add(1, Ordering::Relaxed);
            // Otherwise the registry is every fiber the pool ever ran.
            shared.live.lock().expect("the live fibers").remove(&task.fiber().id());
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
                        // Only if this fiber was actually counted as waiting.
                        // A fiber that took a wake while running and then
                        // yielded for fairness reaches here too, and owes
                        // nothing.
                        if task.fiber().wait().stop_counting() {
                            shared.counts.waiting.fetch_sub(1, Ordering::Relaxed);
                        }
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

/// Waits on the backend, if nobody else is, and wakes whatever is ready.
///
/// True when this worker did the waiting, whether or not it found anything —
/// the caller goes round again either way, because the queues may have changed
/// under it while it waited. What it finds goes on this worker's own queue,
/// as [`look`]'s does.
///
/// **The wait is bounded even though `inject` nudges.** A local queue has no
/// way to announce itself the way the shared one does, so going back to look is
/// how work in somebody else's deque is ever found.
fn serve_io(shared: &Arc<Shared>, local: &Arc<Mutex<VecDeque<Task>>>) -> bool {
    if shared.polling.swap(true, Ordering::AcqRel) {
        return false;
    }
    let ready = shared.reactor.poll(std::time::Duration::from_millis(10));
    looked(shared, local, ready);
    true
}

/// Looks at the backend without waiting, if nobody else is, and puts what is
/// ready on this worker's own queue. True when it found something.
///
/// **Why the worker's own queue, not the shared one:** the worker is about to
/// go round its loop and run it, and anything else is the thread handoff this
/// look exists to avoid. On a pool with more than one worker the others take
/// a share by stealing, as they do a crowd a fiber woke.
///
/// What it costs: a zero-timeout `epoll_wait` (or, where there is no `epoll`,
/// a `poll` over every registered socket), taken only when no other thread is
/// already in one. With no socket registered, only the reactor's lock to read
/// that.
fn look(shared: &Arc<Shared>, local: &Arc<Mutex<VecDeque<Task>>>) -> bool {
    if shared.reactor.len() == 0 {
        // Nothing to find, and so nothing for the backstop to find either:
        // counted as a look so that a pool with no sockets keeps it asleep.
        shared.last_look.store(micros_since(shared), Ordering::Release);
        return false;
    }
    if shared.polling.swap(true, Ordering::AcqRel) {
        return false;
    }
    let ready = shared.reactor.poll(std::time::Duration::ZERO);
    let found = !ready.is_empty();
    looked(shared, local, ready);
    found
}

/// Ends a worker's look at the backend, and delivers what it found to the
/// worker's own queue.
///
/// `polling` must be held by the caller, and is let go here.
fn looked(shared: &Arc<Shared>, local: &Arc<Mutex<VecDeque<Task>>>, ready: Vec<usize>) {
    // Before `polling` is let go. The backstop reads this first and takes
    // `polling` second, so the other order let it read a stale time, find
    // the reactor free, and look again straight after a worker had. Harmless
    // either way (a look finds what is ready, or nothing); this only makes
    // it rarer.
    shared.last_look.store(micros_since(shared), Ordering::Release);
    shared.polling.store(false, Ordering::Release);
    shared.counts.worker_polls.fetch_add(1, Ordering::Relaxed);
    for id in ready {
        shared.counts.sockets_ready.fetch_add(1, Ordering::Relaxed);
        if let Some(state) = state_of(shared, id) {
            wake_found(shared, id, state.wait(), local);
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
        // **Waking up means going back to `next`, not looking again here.**
        // This loop sees only two of the four places work lives, and stealing
        // is in `next`. Re-checking these two and going back to sleep left
        // three workers asleep through twenty milliseconds of work sitting in
        // the fourth one's queue.
        //
        // Checking every worker's queue from here instead livelocks: seeing
        // work elsewhere is not being able to take it, so the worker spins
        // between `park` and a losing steal, and four spinners on an unfair
        // lock starve the one making progress. That hung
        // `a_fiber_keeps_its_identity_across_workers` outright.
        //
        // A millisecond and then a proper look is neither. `notify_one` wakes
        // exactly one worker, so two pushes that wake the same one leave
        // another asleep, and this bounds how long that lasts.
        let (queued_again, timed_out) = shared
            .arrived
            .wait_timeout(queued, std::time::Duration::from_millis(1))
            .expect("the shared queue");
        if timed_out.timed_out() {
            return true;
        }
        queued = queued_again;
    }
}

/// The hasher for [`Shared::live`] and [`Shared::parked`], which are keyed by
/// fiber id.
///
/// **What this prevents: SipHash on every park and every wake.** The default
/// hasher resists keys chosen to collide, and a fiber id is a counter the
/// runtime hands out, so nobody chooses it. SipHash over those maps was 8.5% of
/// a twenty-query request's instructions once the allocation costs were gone.
///
/// A multiply rather than the id itself: the standard map takes a seven-bit tag
/// from the *top* of the hash to reject a slot without comparing keys, and the
/// top bits of a small counter are all zero, so every entry would share one tag
/// and every probe would compare keys. Multiplying by an odd constant keeps
/// distinct ids distinct and moves their differences into those bits.
///
/// Then the high half folded into the low: the map picks the bucket from the
/// *low* bits, which a multiply leaves as the id's trailing zeros. Without the
/// fold, fibers alive at ids 4096 apart share one bucket in 4096 and each
/// lookup walks them all; with it, they spread. What it costs: a shift and an
/// xor per hash.
#[derive(Clone, Copy, Default)]
struct ById;

impl std::hash::BuildHasher for ById {
    type Hasher = IdHash;
    fn build_hasher(&self) -> IdHash {
        IdHash(0)
    }
}

/// See [`ById`].
struct IdHash(u64);

impl std::hash::Hasher for IdHash {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write_usize(&mut self, id: usize) {
        // 2^64 divided by the golden ratio, which is odd, so no two ids
        // collide. The multiply spreads an id's differences upward only: the
        // low bits, which pick the bucket, keep the id's trailing zeros. The
        // xor brings the high half down, so ids a power of two apart land in
        // different buckets. It stays one-to-one, as any `x ^ (x >> k)` is.
        let p = (id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = p ^ (p >> 32);
    }

    /// Only a `usize` key is hashed here, and a key type that hashes as bytes
    /// would otherwise get a hash of zero for everything and a map that is a
    /// list. Folded in anyway rather than a panic, because a panic here is in
    /// the middle of a wake.
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coro::suspend;

    /// **What this guards: a fiber-id hash that the map cannot tell apart.**
    /// Every id must hash differently, and the top seven bits, which the map
    /// uses to skip a slot without comparing keys, must vary as well. A hash of
    /// the id itself passes the first and fails the second, and would make
    /// every lookup compare the key of every fiber in its group.
    ///
    /// The low bits pick the bucket, and they must vary for ids that share a
    /// power-of-two stride: the ids alive at once are every 4096th when a
    /// program keeps one long fiber per round of 4096 it spawns. A bare
    /// multiply keeps the id's twelve trailing zero bits, so those ids would
    /// all land in one bucket in 4096, and the map would degrade to a list.
    #[test]
    fn fiber_ids_hash_apart_in_the_bits_the_map_reads() {
        use std::hash::BuildHasher;
        let hashes: Vec<u64> = (1usize..=4096).map(|id| ById.hash_one(id)).collect();
        let distinct: std::collections::HashSet<u64> = hashes.iter().copied().collect();
        assert_eq!(distinct.len(), hashes.len(), "two fiber ids hashed alike");
        let tags: std::collections::HashSet<u64> = hashes.iter().map(|h| h >> 57).collect();
        assert_eq!(tags.len(), 128, "only {} of the 128 tags in use", tags.len());

        // 4096 strided ids into a table of 4096 buckets, as the map would
        // hold them: an even spread fills about 63% of the buckets
        // (1 - 1/e); fewer than half means the stride is showing through.
        let strided: Vec<u64> = (1usize..=4096).map(|k| ById.hash_one(k * 4096)).collect();
        let buckets: std::collections::HashSet<u64> = strided.iter().map(|h| h & 4095).collect();
        assert!(
            buckets.len() > 2048,
            "ids 4096 apart fill {} of 4096 buckets",
            buckets.len()
        );
    }

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

    /// The point of having more than one worker: fibers that each take a
    /// little wall-clock time must overlap.
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

    /// A fiber that suspends without a budget still comes back.
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

    /// A fiber sleeps on a deadline and the scheduler wakes it, with no worker
    /// blocked — the shape every I/O wait takes.
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

    /// Waking by hand, the way the reactor does when a socket becomes
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

    /// **The reduction that found the cached-TLS bug, kept as its
    /// regression test.**
    ///
    /// `many_sleeping_fibers_all_wake` hit it first, which made it look like a
    /// timer bug for a day. It is not: this has no deadlines, no `Timers` and
    /// no timer thread — four hundred fibers that park, four workers, and one
    /// thread waking them a millisecond later.
    ///
    /// **That millisecond is load-bearing, and so is every number here.** A
    /// waker that spins instead of sleeping never reproduces anything, because
    /// the fibers take the already-notified path in `declare` and never
    /// actually suspend, so nothing migrates between workers — and migration
    /// is the whole point. Fewer workers, or fewer fibers, and the window
    /// closes. Before `coro::installed` stopped the compiler caching a
    /// thread-local address across a stack switch, this died of `SIGSEGV`
    /// about once in ten runs; after, zero in eighty.
    #[test]
    fn parking_and_waking_at_scale_survives_fibers_changing_worker() {
        const COUNT: usize = 400;
        let woke = Arc::new(AtomicUsize::new(0));
        let ids: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let pool = Arc::new(Scheduler::new(4));
        let waker = {
            let pool = pool.clone();
            let ids = ids.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let seen: Vec<usize> = ids.lock().expect("ids").clone();
                    for id in seen {
                        pool.wake_fiber(id);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            })
        };

        for _ in 0..COUNT {
            let counter = woke.clone();
            let task = Task::new(move || {
                park_current();
                counter.fetch_add(1, Ordering::SeqCst);
            });
            ids.lock().expect("ids").push(task.fiber().id());
            pool.spawn(task);
        }
        pool.drain();
        stop.store(true, Ordering::SeqCst);
        waker.join().expect("the waker");
        assert_eq!(woke.load(Ordering::SeqCst), COUNT);
    }

    /// `take_half` leaves the front and returns the back, oldest first.
    ///
    /// The direction is the whole contention argument, and a scheduler test
    /// would pass with the ends swapped.
    #[test]
    fn a_thief_takes_the_back_half_and_leaves_the_front() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut queue: VecDeque<Task> = VecDeque::new();
        for n in 0..6 {
            let seen = order.clone();
            queue.push_back(Task::new(move || seen.lock().expect("order").push(n)));
        }

        let taken = take_half(&mut queue);
        assert_eq!(queue.len(), 3, "the front half stays with its owner");
        assert_eq!(taken.len(), 3);

        // Run both halves to find out which fibers ended up where.
        for mut task in queue.into_iter().chain(taken) {
            while task.resume() == Ran::Suspended {}
        }
        let ran = order.lock().expect("order").clone();
        assert_eq!(ran, vec![0, 1, 2, 3, 4, 5], "owner keeps 0..3, thief takes 3..6");
    }

    /// An odd queue rounds up, so a victim holding one fiber can be robbed.
    ///
    /// Rounding down would leave the last fiber stranded behind a busy worker,
    /// which is the case stealing exists for.
    #[test]
    fn stealing_from_a_queue_of_one_takes_the_one() {
        let mut queue: VecDeque<Task> = VecDeque::new();
        queue.push_back(Task::new(|| {}));
        let taken = take_half(&mut queue);
        assert_eq!(taken.len(), 1);
        assert!(queue.is_empty());
    }

    /// **The point of the phase.** One fiber spawns a pile of work onto its
    /// own worker's queue, and the pool shares it out.
    ///
    /// Everything spawned from inside a fiber goes to that fiber's worker, for
    /// locality. Without stealing the other three workers have no way to reach
    /// any of it — they would wake on the parking timeout, find the shared
    /// queue empty, and go back to sleep while one worker did all the work.
    ///
    /// **Each child does a little real work, and that is not padding.** With
    /// instant children the owner can finish all two hundred before a thief
    /// has woken and swept, so `fibers_stolen` is legitimately zero and the
    /// test fails for no reason — which it did, about once in twenty. A
    /// hundred microseconds each puts twenty milliseconds of work in one
    /// queue, against a millisecond of parking timeout, so a thief that wants
    /// some cannot miss.
    #[test]
    fn a_burst_spawned_on_one_worker_is_shared_out() {
        const COUNT: usize = 200;
        const EACH: std::time::Duration = std::time::Duration::from_micros(100);

        let done = Arc::new(AtomicUsize::new(0));
        let workers = Arc::new(Mutex::new(std::collections::HashSet::new()));

        let pool = Scheduler::new(4);
        let counter = done.clone();
        let seen = workers.clone();
        pool.spawn(Task::new(move || {
            for _ in 0..COUNT {
                let counter = counter.clone();
                let seen = seen.clone();
                schedule(Task::new(move || {
                    let until = std::time::Instant::now() + EACH;
                    while std::time::Instant::now() < until {
                        std::hint::spin_loop();
                    }
                    seen.lock().expect("workers").insert(std::thread::current().id());
                    counter.fetch_add(1, Ordering::SeqCst);
                }));
            }
        }));
        pool.drain();

        assert_eq!(done.load(Ordering::SeqCst), COUNT);
        let counts = pool.counts();
        assert!(counts.fibers_stolen > 0, "nothing was stolen: {counts:?}");
        assert!(
            workers.lock().expect("workers").len() > 1,
            "one worker ran all {COUNT} of them: {counts:?}"
        );
    }

    /// A pool with one worker never sweeps, because there is nobody to rob.
    #[test]
    fn a_single_worker_does_not_try_to_steal_from_itself() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = ran.clone();
        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        pool.drain();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert_eq!(pool.counts().steals_attempted, 0, "{:?}", pool.counts());
    }

    /// **A wake for a fiber that is not waiting must not be counted as one.**
    ///
    /// Found by 11F's soak, as `waiting: 18446744073709551588` — minus
    /// twenty-eight, in a pool where every fiber had finished and every queue
    /// was empty.
    ///
    /// `NOTIFIED` means two different things and the counting conflated them.
    /// Reached from `WAITING` it means "a fiber that was waiting has been
    /// released", and something must give the waiting total back. Reached from
    /// `RUNNING` it means "do not sleep next time", and nothing was ever
    /// added. The worker sees only the state, so a fiber that took a spurious
    /// wake while running and then yielded for fairness had a decrement
    /// charged against a fiber that never waited.
    ///
    /// Deterministic: the fiber spins until it has been woken, so the wake is
    /// guaranteed to land while it is running rather than while it waits.
    #[test]
    fn a_wake_for_a_running_fiber_is_not_counted_as_a_wait() {
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let poked = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let pool = Scheduler::new(1);
        let task = Task::new({
            let started = started.clone();
            let poked = poked.clone();
            move || {
                started.store(true, Ordering::SeqCst);
                // Still RUNNING, by construction: parking here would be the
                // case that *should* be counted.
                while !poked.load(Ordering::SeqCst) {
                    std::hint::spin_loop();
                }
                suspend();
            }
        });
        let id = task.fiber().id();
        pool.spawn(task);

        while !started.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        pool.wake_fiber(id);
        poked.store(true, Ordering::SeqCst);
        pool.drain();

        let counts = pool.counts();
        assert_eq!(counts.waiting, 0, "the waiting total went negative: {counts:?}");
        assert!(pool.audit().settled(), "{:?}", pool.audit());
    }

    /// **A socket nobody writes to gives the fiber back at its deadline** —
    /// the whole point of `wait_until_ready_by`, since `SO_RCVTIMEO` cannot
    /// fire on a socket that never blocks.
    #[test]
    fn a_socket_wait_ends_at_its_deadline() {
        let outcome = Arc::new(Mutex::new(None));
        let seen = outcome.clone();

        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            // Connected, so it stays open, and silent, so it never becomes
            // readable. Only the deadline can end this.
            let (mine, _peer) = crate::reactor::a_connected_pair();
            let began = std::time::Instant::now();
            let ended = wait_until_ready_by(
                crate::reactor::socket_of(&mine),
                Interest::Readable,
                Some(began + std::time::Duration::from_millis(60)),
            );
            *seen.lock().expect("the outcome") = Some((ended, began.elapsed()));
        }));
        pool.drain();

        let (ended, took) = outcome.lock().expect("the outcome").expect("it ran");
        assert_eq!(ended, Waited::TimedOut);
        assert!(took >= std::time::Duration::from_millis(55), "returned early: {took:?}");
        assert!(pool.settle(std::time::Duration::from_secs(2)).settled());
    }

    /// A deadline that has already gone is not a wait at all.
    #[test]
    fn a_deadline_in_the_past_does_not_register_anything() {
        let outcome = Arc::new(Mutex::new(None));
        let seen = outcome.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            let (mine, _peer) = crate::reactor::a_connected_pair();
            *seen.lock().expect("the outcome") = Some(wait_until_ready_by(
                crate::reactor::socket_of(&mine),
                Interest::Readable,
                Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ));
        }));
        pool.drain();

        assert_eq!(outcome.lock().expect("the outcome").expect("it ran"), Waited::TimedOut);
        assert!(pool.settle(std::time::Duration::from_secs(2)).settled());
    }

    /// A peer that writes in time wins the race against the deadline.
    #[test]
    fn readiness_beats_a_deadline_that_has_not_come() {
        use std::io::Write;
        let outcome = Arc::new(Mutex::new(None));
        let seen = outcome.clone();

        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            let (mine, mut peer) = crate::reactor::a_connected_pair();
            let socket = crate::reactor::socket_of(&mine);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let _ = peer.write_all(b"x");
                // Held open until the write has certainly been seen.
                std::thread::sleep(std::time::Duration::from_millis(200));
            });
            *seen.lock().expect("the outcome") = Some(wait_until_ready_by(
                socket,
                Interest::Readable,
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5)),
            ));
        }));
        pool.drain();

        assert_eq!(outcome.lock().expect("the outcome").expect("it ran"), Waited::Ready);
    }

    /// Off a scheduler it says so, rather than pretending to wait.
    #[test]
    fn a_deadline_without_a_scheduler_is_refused() {
        let (mine, _peer) = crate::reactor::a_connected_pair();
        assert_eq!(
            wait_until_ready_by(
                crate::reactor::socket_of(&mine),
                Interest::Readable,
                Some(std::time::Instant::now() + std::time::Duration::from_secs(1)),
            ),
            Waited::Unscheduled
        );
    }

    /// Many fibers, many deadlines, all across several workers.
    ///
    /// The test that caught the cached-thread-local bug: `SIGSEGV` in
    /// seventeen runs out of sixty on Linux, and green every time on Windows.
    /// See `local_queue`'s `#[inline(never)]`.
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

    /// Park and wake over and over, where a lost wakeup shows as a hang rather
    /// than a wrong answer.
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

    /// **Ten thousand fibers waiting at once on two workers, then all woken.**
    ///
    /// A tenth of the phase's criterion, because this belongs in the ordinary
    /// suite; the full hundred thousand is measured in
    /// `docs/design/scheduler.md` at 418 MB resident, about 4,240 bytes each.
    /// A lost wake in ten thousand hangs rather than fails, which is why the
    /// loop below has a deadline.
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
    /// cancelable, or a nursery closing over one waits for ever.
    #[test]
    fn canceling_a_sleeping_fiber_wakes_it_to_notice() {
        let noticed = Arc::new(AtomicUsize::new(0));
        let counter = noticed.clone();
        let id = Arc::new(AtomicUsize::new(0));
        let mine = id.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            mine.store(crate::current::current(|f| f.id()), Ordering::SeqCst);
            // Nothing will ever wake this on its own merits.
            park_current();
            if crate::current::current(|f| f.is_canceled()) {
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
            "a canceled sleeper must wake and see it: {:?}",
            pool.counts()
        );
    }

    /// **The regression test for a hang.** Canceling a fiber that has not run
    /// yet, so nobody is holding its task and it is in no queue a waker can
    /// search.
    ///
    /// `cancel_fiber` used to look the fiber up in the *parked* map, which is
    /// a race with two losing sides: a fiber suspended but not yet filed is in
    /// neither map, so the cancel found nothing and did nothing, and the fiber
    /// slept for ever. It passed until the reactor thread changed the timing
    /// enough to lose.
    ///
    /// The state lives from the moment the fiber does, so this now works
    /// through the ordinary protocol: the cancel leaves a notification, the
    /// fiber's first attempt to wait takes it instead of sleeping, and it sees
    /// the cancellation on the other side.
    #[test]
    fn canceling_a_fiber_before_anybody_holds_it_is_not_lost() {
        let noticed = Arc::new(AtomicUsize::new(0));
        let counter = noticed.clone();

        let pool = Scheduler::new(1);
        let task = Task::new(move || {
            // Nobody will ever wake this on its own merits.
            park_current();
            if crate::current::current(|f| f.is_canceled()) {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        let id = task.fiber().id();
        pool.spawn(task);

        // No waiting for it to park: the point is that this lands first.
        pool.cancel_fiber(id);
        pool.drain();

        assert_eq!(
            noticed.load(Ordering::SeqCst),
            1,
            "the cancellation was dropped: {:?}",
            pool.counts()
        );
    }

    /// A sleeper canceled before its deadline must not be left in the timer
    /// heap: at scale that is a hundred thousand dead entries.
    #[test]
    fn canceling_a_sleeper_forgets_its_deadline() {
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

    // --- sockets ------------------------------------------------------------

    /// A fiber waits on a socket, and is woken when bytes arrive.
    ///
    /// The shape every Khora `read()!` takes: try, would block, park, wake,
    /// retry.
    #[test]
    fn a_fiber_waiting_on_a_socket_is_woken_by_its_peer() {
        use crate::reactor::{a_connected_pair, socket_of, Interest};
        use std::io::{Read, Write};

        let (client, mut server) = a_connected_pair();
        let socket = socket_of(&client);
        let got = Arc::new(Mutex::new(Vec::new()));
        let seen = got.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            // Nothing has arrived yet, so this is the would-block path.
            assert!(wait_until_ready(socket, Interest::Readable));
            let mut client = client;
            let mut buffer = [0u8; 5];
            client.read_exact(&mut buffer).expect("the bytes are there");
            seen.lock().unwrap().extend_from_slice(&buffer);
        }));

        // Give it time to park before anything is sent, so the wake is a real
        // one rather than the socket already being ready.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pool.watching() == 0 {
            assert!(std::time::Instant::now() < deadline, "it never registered");
            std::thread::yield_now();
        }
        assert!(got.lock().unwrap().is_empty(), "nothing should have been read yet");

        server.write_all(b"hello").expect("writing");
        pool.drain();

        assert_eq!(&*got.lock().unwrap(), b"hello");
        assert!(pool.counts().sockets_ready >= 1, "{:?}", pool.counts());
    }

    /// **The property the whole phase is for.** One worker, one fiber blocked
    /// on a socket that nobody will write to — and the worker keeps running
    /// everything else.
    ///
    /// On threads this is impossible: the blocked read owns the thread.
    #[test]
    fn a_worker_is_not_blocked_by_a_fiber_waiting_on_a_socket() {
        use crate::reactor::{a_connected_pair, socket_of, Interest};

        let (client, _peer) = a_connected_pair();
        let socket = socket_of(&client);
        let others = Arc::new(AtomicUsize::new(0));

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            // Nobody ever writes, so this waits until the pool stops.
            let _client = client;
            wait_until_ready(socket, Interest::Readable);
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pool.watching() == 0 {
            assert!(std::time::Instant::now() < deadline, "it never registered");
            std::thread::yield_now();
        }

        for _ in 0..32 {
            let counter = others.clone();
            pool.spawn(Task::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }));
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while others.load(Ordering::SeqCst) < 32 {
            assert!(
                std::time::Instant::now() < deadline,
                "the blocked socket read owned the worker: {} of 32 ran",
                others.load(Ordering::SeqCst)
            );
            std::thread::yield_now();
        }
    }

    /// Many fibers on many sockets, each woken by its own peer and nobody
    /// else's.
    #[test]
    fn many_fibers_wait_on_their_own_sockets() {
        use crate::reactor::{a_connected_pair, socket_of, Interest};
        use std::io::{Read, Write};

        const COUNT: usize = 64;
        let done = Arc::new(AtomicUsize::new(0));
        let mut peers = Vec::new();

        let pool = Scheduler::new(2);
        for n in 0..COUNT {
            let (client, peer) = a_connected_pair();
            let socket = socket_of(&client);
            let counter = done.clone();
            pool.spawn(Task::new(move || {
                let mut client = client;
                wait_until_ready(socket, Interest::Readable);
                let mut byte = [0u8; 1];
                client.read_exact(&mut byte).expect("its own byte");
                assert_eq!(byte[0], (n % 251) as u8, "a fiber read somebody else's socket");
                counter.fetch_add(1, Ordering::SeqCst);
            }));
            peers.push(peer);
        }

        for (n, peer) in peers.iter_mut().enumerate() {
            peer.write_all(&[(n % 251) as u8]).expect("writing");
        }
        pool.drain();
        assert_eq!(done.load(Ordering::SeqCst), COUNT);
    }

    /// A fiber waiting on a socket that will never be ready still has to be
    /// cancelable, and its watch must not outlive it.
    #[test]
    fn canceling_a_fiber_waiting_on_a_socket_wakes_it() {
        use crate::reactor::{a_connected_pair, socket_of, Interest};

        let (client, _peer) = a_connected_pair();
        let socket = socket_of(&client);
        let noticed = Arc::new(AtomicUsize::new(0));
        let counter = noticed.clone();
        let id = Arc::new(AtomicUsize::new(0));
        let mine = id.clone();

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(move || {
            let _client = client;
            mine.store(crate::current::current(|f| f.id()), Ordering::SeqCst);
            wait_until_ready(socket, Interest::Readable);
            if crate::current::current(|f| f.is_canceled()) {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pool.watching() == 0 {
            assert!(std::time::Instant::now() < deadline, "it never registered");
            std::thread::yield_now();
        }

        pool.cancel_fiber(id.load(Ordering::SeqCst));
        pool.drain();

        assert_eq!(noticed.load(Ordering::SeqCst), 1, "{:?}", pool.counts());
        assert_eq!(pool.watching(), 0, "its watch should be gone");
    }

    /// Off a scheduler there is nobody to watch anything, so the caller has to
    /// know to block the thread as it always did.
    #[test]
    fn waiting_on_a_socket_without_a_scheduler_is_refused() {
        use crate::reactor::{a_connected_pair, socket_of, Interest};
        let (client, _peer) = a_connected_pair();
        assert!(!wait_until_ready(socket_of(&client), Interest::Readable));
    }

    /// Off a scheduler there is nobody to wake anything, so sleeping would be
    /// for ever. Saying so beats hanging.
    #[test]
    fn parking_without_a_scheduler_is_refused() {
        assert!(!park_current(), "there is no worker here");
        assert!(!sleep_until(std::time::Instant::now()));
    }

    // --- where a wake goes ----------------------------------------------------

    /// Waits until `count` fibers are filed in the parked map.
    ///
    /// **Filed, not merely waiting.** `waiting` counts a fiber from just before
    /// it suspends, and a wake that lands before its worker files it takes
    /// neither path in `wake`: the worker finds `NOTIFIED` and requeues it
    /// itself. A test that counted paths after waiting for `waiting` would
    /// count a race.
    fn until_parked(pool: &Scheduler, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while pool.audit().parked < count {
            assert!(
                std::time::Instant::now() < deadline,
                "only {} of {count} parked",
                pool.audit().parked
            );
            std::thread::yield_now();
        }
    }

    /// [`Scheduler::drain`] with a deadline, so that a woken fiber stranded on
    /// a queue nobody looks at fails the test with the counts rather than
    /// hanging it.
    fn drained(pool: &Scheduler) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let counts = pool.counts();
            if counts.completed == counts.spawned && pool.audit().queued == 0 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the pool never drained: {counts:?} {:?}",
                pool.audit()
            );
            std::thread::yield_now();
        }
    }

    /// Spawns `count` fibers that each hand over a waker and park once.
    ///
    /// Answers the wakers and a count of how many have run since.
    fn parked_fibers(pool: &Scheduler, count: usize) -> (Arc<Mutex<Vec<Waker>>>, Arc<AtomicUsize>) {
        let wakers = Arc::new(Mutex::new(Vec::new()));
        let resumed = Arc::new(AtomicUsize::new(0));
        for _ in 0..count {
            let wakers = wakers.clone();
            let resumed = resumed.clone();
            pool.spawn(Task::new(move || {
                wakers.lock().unwrap().push(waker_for_current().expect("on a worker"));
                park_current();
                resumed.fetch_add(1, Ordering::SeqCst);
            }));
        }
        until_parked(pool, count);
        (wakers, resumed)
    }

    /// **The local wake path, end to end.** A fiber woken by another fiber
    /// on the same worker is put on that worker's queue and runs there, and
    /// nothing goes through the shared queue.
    ///
    /// Two workers, so an injected wake could have gone to either. The woken
    /// fiber runs on the waker's worker unless the idle one stole it, which is
    /// what stealing is for; the counts say whether it did, and the thread
    /// check applies when it did not.
    #[test]
    fn a_fiber_woken_by_a_fiber_runs_on_the_wakers_worker() {
        let pool = Scheduler::started(2, true);
        let wakers = Arc::new(Mutex::new(Vec::new()));
        let ran_on = Arc::new(Mutex::new(None));
        {
            let wakers = wakers.clone();
            let ran_on = ran_on.clone();
            pool.spawn(Task::new(move || {
                wakers.lock().unwrap().push(waker_for_current().expect("on a worker"));
                park_current();
                *ran_on.lock().unwrap() = Some(std::thread::current().id());
            }));
        }
        until_parked(&pool, 1);

        let waker: Waker = wakers.lock().unwrap().pop().expect("a waker");
        let woke_on = Arc::new(Mutex::new(None));
        let mine = woke_on.clone();
        pool.spawn(Task::new(move || {
            *mine.lock().unwrap() = Some(std::thread::current().id());
            waker.wake();
        }));
        drained(&pool);

        let counts = pool.counts();
        assert_eq!(counts.wakes_local, 1, "the wake did not go to the waker's worker: {counts:?}");
        assert_eq!(counts.wakes_injected, 0, "a fiber-to-fiber wake went through inject: {counts:?}");
        if counts.fibers_stolen == 0 {
            assert_eq!(*ran_on.lock().unwrap(), *woke_on.lock().unwrap(), "{counts:?}");
        }
        assert!(pool.settle(std::time::Duration::from_secs(2)).settled(), "{:?}", pool.audit());
    }

    /// The same on four workers, sixteen times: every fiber-to-fiber wake is
    /// local, however the workers share the wakers out.
    #[test]
    fn every_fiber_to_fiber_wake_on_a_pool_is_local() {
        const PAIRS: usize = 16;
        let pool = Scheduler::started(4, true);
        let (wakers, resumed) = parked_fibers(&pool, PAIRS);
        for waker in wakers.lock().unwrap().drain(..) {
            pool.spawn(Task::new(move || waker.wake()));
        }
        drained(&pool);

        assert_eq!(resumed.load(Ordering::SeqCst), PAIRS);
        let counts = pool.counts();
        assert_eq!(counts.wakes_local, PAIRS as u64, "{counts:?}");
        assert_eq!(counts.wakes_injected, 0, "{counts:?}");
        assert!(pool.settle(std::time::Duration::from_secs(2)).settled(), "{:?}", pool.audit());
    }

    /// **`KHORA_WAKE_LOCAL=0` sends every wake through the shared queue**,
    /// fiber-to-fiber ones included.
    #[test]
    fn with_wake_local_off_every_wake_is_injected() {
        const PAIRS: usize = 16;
        let pool = Scheduler::started(4, false);
        let (wakers, resumed) = parked_fibers(&pool, PAIRS);
        for waker in wakers.lock().unwrap().drain(..) {
            pool.spawn(Task::new(move || waker.wake()));
        }
        drained(&pool);

        assert_eq!(resumed.load(Ordering::SeqCst), PAIRS);
        let counts = pool.counts();
        assert_eq!(counts.wakes_local, 0, "{counts:?}");
        assert_eq!(counts.wakes_injected, PAIRS as u64, "{counts:?}");
    }

    /// The switch is off for `0` alone. Unset, empty and anything else leave
    /// the local path on.
    #[test]
    fn only_zero_turns_wake_local_off() {
        assert!(!wake_local_from(Some("0")));
        assert!(wake_local_from(None));
        assert!(wake_local_from(Some("1")));
        assert!(wake_local_from(Some("")));
        assert!(wake_local_from(Some("off")));
    }

    /// **A wake from a thread that is not a worker goes through the shared
    /// queue**: there is no worker queue of the waker's to put it on, and one
    /// chosen for it would be one nobody was about to look at.
    #[test]
    fn a_wake_from_a_foreign_thread_is_injected() {
        const FIBERS: usize = 8;
        let pool = Scheduler::started(2, true);
        let (wakers, resumed) = parked_fibers(&pool, FIBERS);
        let taken: Vec<Waker> = wakers.lock().unwrap().drain(..).collect();
        std::thread::spawn(move || {
            for waker in taken {
                waker.wake();
            }
        })
        .join()
        .expect("the foreign waker");
        drained(&pool);

        assert_eq!(resumed.load(Ordering::SeqCst), FIBERS);
        let counts = pool.counts();
        assert_eq!(counts.wakes_local, 0, "{counts:?}");
        assert_eq!(counts.wakes_injected, FIBERS as u64, "{counts:?}");
    }

    /// **A fiber on one pool waking a fiber on another goes through the
    /// shared queue of the other.** Its own worker's queue belongs to the
    /// wrong pool: the woken fiber would run on a worker that does not know
    /// it, and each pool's audit would be off by one in opposite directions.
    #[test]
    fn a_wake_from_a_fiber_of_another_pool_is_injected() {
        let theirs = Scheduler::started(1, true);
        let ours = Scheduler::started(1, true);
        let (wakers, resumed) = parked_fibers(&theirs, 1);
        let waker = wakers.lock().unwrap().pop().expect("a waker");
        ours.spawn(Task::new(move || waker.wake()));
        drained(&ours);
        drained(&theirs);

        assert_eq!(resumed.load(Ordering::SeqCst), 1);
        assert_eq!(ours.counts().wakes_local + ours.counts().wakes_injected, 0);
        let counts = theirs.counts();
        assert_eq!(counts.wakes_local, 0, "{counts:?}");
        assert_eq!(counts.wakes_injected, 1, "{counts:?}");
        assert!(theirs.settle(std::time::Duration::from_secs(2)).settled(), "{:?}", theirs.audit());
        assert!(ours.settle(std::time::Duration::from_secs(2)).settled(), "{:?}", ours.audit());
    }

    /// Spawns a fiber that reads `rounds` bytes from `client`, one wait per
    /// byte, counting each into `got`. `sleeps` timer waits come first, so a
    /// test can see which path a timer's wake took.
    fn a_reader(
        client: std::net::TcpStream,
        rounds: usize,
        sleeps: usize,
        got: Arc<AtomicUsize>,
    ) -> Task {
        use crate::reactor::{socket_of, Interest};
        use std::io::Read;
        let socket = socket_of(&client);
        Task::new(move || {
            for _ in 0..sleeps {
                sleep_until(std::time::Instant::now() + std::time::Duration::from_millis(5));
            }
            // Non-blocking and retried, as every socket operation on the
            // scheduler is: readiness is a hint, and a second report of one
            // readiness leaves a wake standing that ends the next wait before
            // the byte is there. A blocking read would then hold the worker.
            let mut client = client;
            client.set_nonblocking(true).expect("non-blocking");
            for _ in 0..rounds {
                let mut byte = [0u8; 1];
                loop {
                    match client.read(&mut byte) {
                        Ok(1) => break,
                        Ok(_) => return,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            wait_until_ready(socket, Interest::Readable);
                        }
                        Err(_) => return,
                    }
                }
                got.fetch_add(1, Ordering::SeqCst);
            }
        })
    }

    /// Writes `rounds` bytes to `peer`, each only once the reader is filed
    /// waiting on the socket, and waits for it to be read.
    ///
    /// Filed, not merely registered, so every byte's wake is a real one
    /// rather than a wake that beat the park, and its path is counted.
    fn feed(pool: &Scheduler, peer: &mut std::net::TcpStream, rounds: usize, read: &AtomicUsize) {
        use std::io::Write;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        for round in 0..rounds {
            while pool.watching() == 0 {
                assert!(std::time::Instant::now() < deadline, "round {round} never registered");
                std::thread::yield_now();
            }
            until_parked(pool, 1);
            peer.write_all(b"x").expect("writing");
            while read.load(Ordering::SeqCst) <= round {
                assert!(
                    std::time::Instant::now() < deadline,
                    "round {round} never read: {:?}",
                    pool.counts()
                );
                std::thread::yield_now();
            }
        }
    }

    /// **Wakes from the timer thread go through the shared queue, and
    /// readiness a worker finds itself goes on that worker's own queue.**
    ///
    /// The timer thread has no queue of its own and must wake a worker. A
    /// worker that finds readiness in its own look at the reactor is about to
    /// go round its loop and run the fiber, and a trip through the shared
    /// queue would be the thread handoff the look exists to avoid. The
    /// backstop thread's wakes are injected too;
    /// `the_backstop_delivers_readiness_while_every_worker_is_held` has that
    /// half, since the backstop only looks when no worker can.
    ///
    /// With the backstop off, so every readiness here is found by a worker
    /// and the counts are exact. `KHORA_WAKE_LOCAL=0` puts the worker's finds
    /// back through the shared queue, like every other wake.
    #[test]
    fn wakes_from_timers_are_injected_and_readiness_a_worker_finds_is_local() {
        use crate::reactor::a_connected_pair;
        const ROUNDS: usize = 20;
        const SLEEPS: usize = 3;

        for wake_local in [true, false] {
            let pool = Scheduler::started_without_backstop(2, wake_local);
            let (client, mut peer) = a_connected_pair();
            let read = Arc::new(AtomicUsize::new(0));
            pool.spawn(a_reader(client, ROUNDS, SLEEPS, read.clone()));
            feed(&pool, &mut peer, ROUNDS, &read);
            drained(&pool);

            let counts = pool.counts();
            assert!(counts.timers_fired >= SLEEPS as u64, "{counts:?}");
            assert_eq!(counts.backstop_polls, 0, "{counts:?}");
            // Every byte's wake is exact, because `feed` writes only once
            // the reader is filed. A timer's wake is counted only if the
            // fiber was filed before its deadline, which a loaded machine
            // may not manage in 5 ms, so those are a range; with the backstop
            // off, the timer thread is the only thing here that can inject.
            if wake_local {
                assert_eq!(counts.wakes_local, ROUNDS as u64, "{counts:?}");
                assert!((1..=SLEEPS as u64).contains(&counts.wakes_injected), "{counts:?}");
            } else {
                assert_eq!(counts.wakes_local, 0, "{counts:?}");
                let injected = ROUNDS as u64 + 1..=(ROUNDS + SLEEPS) as u64;
                assert!(injected.contains(&counts.wakes_injected), "{counts:?}");
            }
        }
    }

    /// **A busy worker finds readiness itself, with nobody else looking.**
    /// One worker, kept busy by a fiber that yields for ever, so it never
    /// runs out of work and never reaches `serve_io`; and the backstop
    /// thread switched off. The only thing left that can see the socket
    /// become ready is the look the worker takes between turns. Without it
    /// the reader waits for ever.
    #[test]
    fn a_busy_workers_own_look_delivers_readiness_with_the_backstop_stalled() {
        use crate::reactor::a_connected_pair;
        const ROUNDS: usize = 20;

        let pool = Scheduler::started_without_backstop(1, true);
        let done = Arc::new(AtomicBool::new(false));
        let spinning = done.clone();
        pool.spawn(Task::new(move || {
            while !spinning.load(Ordering::SeqCst) {
                suspend();
            }
        }));
        let (client, mut peer) = a_connected_pair();
        let read = Arc::new(AtomicUsize::new(0));
        pool.spawn(a_reader(client, ROUNDS, 0, read.clone()));
        feed(&pool, &mut peer, ROUNDS, &read);
        done.store(true, Ordering::SeqCst);
        drained(&pool);

        let counts = pool.counts();
        assert_eq!(counts.backstop_polls, 0, "{counts:?}");
        assert!(counts.worker_polls >= 1, "{counts:?}");
        assert_eq!(counts.wakes_local, ROUNDS as u64, "{counts:?}");
    }

    /// **The backstop still delivers readiness when every worker is held.**
    /// Two workers, each running a fiber that spins without a safepoint, so
    /// neither goes round its loop to look. The reader's byte arrives while
    /// both are held; the backstop thread has to find it and inject it, and
    /// it runs once a worker is let go.
    ///
    /// Found is the oracle, not run: nothing can run the reader while the
    /// workers are held, so the test waits for the wake to be counted, then
    /// lets them go.
    #[test]
    fn the_backstop_delivers_readiness_while_every_worker_is_held() {
        use crate::reactor::a_connected_pair;
        use std::io::Write;

        /// Lets the holders go however the test ends, so a failed assertion
        /// does not leave the pool's drop waiting on two spinning workers.
        struct Release(Arc<AtomicBool>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let pool = Scheduler::started(2, true);
        let (client, mut peer) = a_connected_pair();
        let read = Arc::new(AtomicUsize::new(0));
        pool.spawn(a_reader(client, 1, 0, read.clone()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while pool.watching() == 0 {
            assert!(std::time::Instant::now() < deadline, "the reader never registered");
            std::thread::yield_now();
        }
        until_parked(&pool, 1);

        let release = Release(Arc::new(AtomicBool::new(false)));
        let holding = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let (go, held) = (release.0.clone(), holding.clone());
            pool.spawn(Task::new(move || {
                held.fetch_add(1, Ordering::SeqCst);
                while !go.load(Ordering::SeqCst) {
                    std::hint::spin_loop();
                }
            }));
        }
        while holding.load(Ordering::SeqCst) < 2 {
            assert!(std::time::Instant::now() < deadline, "the workers were never both held");
            std::thread::yield_now();
        }

        let before = pool.counts();
        peer.write_all(b"x").expect("writing");
        while pool.counts().wakes_injected == before.wakes_injected {
            assert!(
                std::time::Instant::now() < deadline,
                "nobody found the readiness while every worker was held: {:?}",
                pool.counts()
            );
            std::thread::yield_now();
        }
        drop(release);
        drained(&pool);

        assert_eq!(read.load(Ordering::SeqCst), 1);
        let counts = pool.counts();
        assert!(counts.backstop_polls > before.backstop_polls, "{counts:?}");
        assert_eq!(counts.wakes_local, before.wakes_local, "{counts:?}");
    }

    /// **A pool with no deadlines leaves the timer thread asleep**, and a
    /// deadline added to it afterwards still fires.
    ///
    /// The first half is what the condvar is for: a thread that woke every
    /// millisecond to find an empty heap passed a hundred times in the
    /// window. The second is what makes the first safe: a sleep that waited
    /// for a tick that never comes would wait for ever.
    #[test]
    fn a_pool_with_no_timers_leaves_the_timer_thread_asleep() {
        let pool = Scheduler::started(1, true);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let before = pool.counts().timer_passes;
        std::thread::sleep(std::time::Duration::from_millis(100));
        let after = pool.counts().timer_passes;
        assert!(after - before <= 1, "the timer thread ran {} times with no timers", after - before);

        let woke = Arc::new(AtomicBool::new(false));
        let mine = woke.clone();
        pool.spawn(Task::new(move || {
            sleep_until(std::time::Instant::now() + std::time::Duration::from_millis(5));
            mine.store(true, Ordering::SeqCst);
        }));
        drained(&pool);
        assert!(woke.load(Ordering::SeqCst));
        assert!(pool.counts().timers_fired >= 1, "{:?}", pool.counts());
    }

    /// **The bound.** Below it a fiber's wake is local; at it, the wake goes
    /// to the shared queue.
    ///
    /// One worker, and the waker fills its own queue by spawning, so the
    /// queue's length when the wake arrives is exactly what was spawned.
    #[test]
    fn a_wake_past_the_bound_is_injected() {
        for (queued, local, injected) in [(WAKE_LOCAL_BOUND - 1, 1, 0), (WAKE_LOCAL_BOUND, 0, 1)] {
            let pool = Scheduler::started(1, true);
            let (wakers, resumed) = parked_fibers(&pool, 1);
            let waker = wakers.lock().unwrap().pop().expect("a waker");
            pool.spawn(Task::new(move || {
                for _ in 0..queued {
                    schedule(Task::new(|| {}));
                }
                waker.wake();
            }));
            drained(&pool);

            assert_eq!(resumed.load(Ordering::SeqCst), 1);
            let counts = pool.counts();
            assert_eq!(counts.wakes_local, local, "{queued} queued: {counts:?}");
            assert_eq!(counts.wakes_injected, injected, "{queued} queued: {counts:?}");
            assert_eq!(counts.wakes_over_bound, injected, "{queued} queued: {counts:?}");
        }
    }

    /// **A worker that queues a wake locally and then has nothing else to
    /// run still runs the woken fiber.** The waker parks straight after
    /// waking, so the next thing its worker does is look for work -- and the
    /// woken fiber exists only in that worker's own queue. A worker that
    /// went to sleep, or to `epoll_wait`, without looking there would leave
    /// it stranded until something unrelated woke the worker.
    ///
    /// One worker, so nobody can steal it out from under the test.
    #[test]
    fn a_worker_that_wakes_locally_and_then_parks_runs_the_woken_fiber() {
        let pool = Scheduler::started(1, true);
        let (wakers, resumed) = parked_fibers(&pool, 1);
        let waker = wakers.lock().unwrap().pop().expect("a waker");
        let second = Arc::new(Mutex::new(None));
        let mine = second.clone();
        pool.spawn(Task::new(move || {
            *mine.lock().unwrap() = waker_for_current();
            waker.wake();
            park_current();
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while resumed.load(Ordering::SeqCst) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the woken fiber was stranded on its waker's queue: {:?} {:?}",
                pool.counts(),
                pool.audit()
            );
            std::thread::yield_now();
        }
        assert_eq!(pool.counts().wakes_local, 1, "{:?}", pool.counts());

        until_parked(&pool, 1);
        second.lock().unwrap().take().expect("the waker's waker").wake();
        drained(&pool);
    }

    /// **The starvation guard.** One fiber wakes a crowd and then holds its
    /// worker; every one of the crowd is on that worker's queue, and three
    /// other workers are idle. The crowd must be shared out by stealing,
    /// without the waker ever giving its worker back.
    ///
    /// Measured by a handshake: the waker spins until every woken fiber has
    /// run, which can only happen on another worker. The deadline is there to
    /// turn a failure into a message, not to measure anything.
    #[test]
    fn a_crowd_woken_by_one_busy_fiber_is_shared_out() {
        const CROWD: usize = 32;
        const { assert!(CROWD < WAKE_LOCAL_BOUND, "the crowd must fit under the bound to be all local") };

        let pool = Scheduler::started(4, true);
        let wakers = Arc::new(Mutex::new(Vec::new()));
        let ran = Arc::new(AtomicUsize::new(0));
        let threads = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..CROWD {
            let wakers = wakers.clone();
            let ran = ran.clone();
            let threads = threads.clone();
            pool.spawn(Task::new(move || {
                wakers.lock().unwrap().push(waker_for_current().expect("on a worker"));
                park_current();
                threads.lock().unwrap().push(std::thread::current().id());
                ran.fetch_add(1, Ordering::SeqCst);
            }));
        }
        until_parked(&pool, CROWD);

        let crowd: Vec<Waker> = wakers.lock().unwrap().drain(..).collect();
        let waker_thread = Arc::new(Mutex::new(None));
        let spread = Arc::new(AtomicBool::new(false));
        let (seen, done, mine) = (ran.clone(), spread.clone(), waker_thread.clone());
        pool.spawn(Task::new(move || {
            *mine.lock().unwrap() = Some(std::thread::current().id());
            for waker in &crowd {
                waker.wake();
            }
            // Holding the worker: no suspension, no safepoint.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while seen.load(Ordering::SeqCst) < CROWD && std::time::Instant::now() < deadline {
                std::hint::spin_loop();
            }
            done.store(seen.load(Ordering::SeqCst) == CROWD, Ordering::SeqCst);
        }));
        drained(&pool);

        let counts = pool.counts();
        assert_eq!(counts.wakes_local, CROWD as u64, "{counts:?}");
        assert!(
            spread.load(Ordering::SeqCst),
            "the crowd stayed behind its busy waker: {counts:?} turns={:?}",
            pool.turns()
        );
        let busy = waker_thread.lock().unwrap().expect("the waker ran");
        assert!(
            threads.lock().unwrap().iter().all(|t| *t != busy),
            "a woken fiber ran on the busy waker's worker"
        );
        assert!(counts.fibers_stolen >= CROWD as u64, "{counts:?}");
        // Every turn is some worker's, and more than the waker's worker took
        // one: the per-worker count is what the load gate reads balance from.
        let turns = pool.turns();
        assert_eq!(turns.iter().sum::<u64>(), counts.resumes, "turns={turns:?} {counts:?}");
        assert!(turns.iter().filter(|t| **t > 0).count() > 1, "turns={turns:?}");
    }

    // --- a worker whose thread is blocked -------------------------------------

    /// A lock that holds the thread of whoever waits for it, and may be
    /// released on a different thread from the one that took it.
    ///
    /// **Not `std::sync::Mutex`**, whose guard must be dropped on the thread
    /// that locked it on some platforms, and a fiber that takes one and then
    /// suspends may resume on another worker. What these tests need from it
    /// is only that a waiter keeps its worker's thread, which a spin does.
    struct SpinLock(AtomicBool);

    impl SpinLock {
        fn new() -> SpinLock {
            SpinLock(AtomicBool::new(false))
        }

        fn take(&self) {
            while self.0.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                std::hint::spin_loop();
            }
        }

        fn give_back(&self) {
            self.0.store(false, Ordering::Release);
        }

        /// [`SpinLock::take`] that gives up after ten seconds, and says
        /// whether it got the lock.
        ///
        /// **What the tests below wait with, so that a stranded fiber fails
        /// the test instead of hanging it.** A waiter that spins for ever
        /// keeps its worker for ever, and dropping the pool joins that worker:
        /// the test ran on past its own assert and never finished.
        fn take_in_time(&self) -> bool {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while self.0.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                std::hint::spin_loop();
            }
            true
        }
    }

    /// Spins until `flag` is set or ten seconds pass, and says which.
    ///
    /// Blocks the worker's thread as [`SpinLock::take_in_time`] does, and is
    /// bounded for the same reason.
    fn spin_until_set(flag: &AtomicBool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !flag.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::hint::spin_loop();
        }
        true
    }

    /// Spawns `count` fibers that give their worker back at every turn until
    /// `stop`, so that every worker's own queue is never empty and no worker
    /// ever finds itself idle. Waits until every one has run once.
    ///
    /// **This is the state that hides a stranded fiber.** An idle worker
    /// steals, so a fiber stuck on a blocked worker's queue is rescued by the
    /// first worker that runs out of work. A loaded pool has no such worker.
    fn busy_spinners(pool: &Scheduler, count: usize, stop: &Arc<AtomicBool>) {
        let started = Arc::new(AtomicUsize::new(0));
        for _ in 0..count {
            let (stop, started) = (stop.clone(), started.clone());
            pool.spawn(Task::new(move || {
                started.fetch_add(1, Ordering::SeqCst);
                while !stop.load(Ordering::SeqCst) {
                    suspend();
                }
            }));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while started.load(Ordering::SeqCst) < count {
            assert!(std::time::Instant::now() < deadline, "the spinners never started");
            std::thread::yield_now();
        }
    }

    /// Waits up to `patience` for `flag`, and answers whether it came.
    ///
    /// The watchdog in the tests below, and never the thing a test decides
    /// on: each decides on whether the flag was set *while the spinners were
    /// still busy*, then stops them so the pool can drain either way.
    fn came(flag: &AtomicBool, patience: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + patience;
        while !flag.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }

    /// **The review's deadlock, in Rust.** X holds a lock and parks on it --
    /// a change function in `Shared::update` waiting on a channel is the Khora
    /// spelling. W wakes X, and then blocks its worker's thread on that lock.
    /// X is on W's worker's queue, and every other worker is kept busy.
    ///
    /// Nothing but another worker can run X, and a busy worker never looked
    /// anywhere but its own queue and the shared one, so W waited for ever:
    /// on every run, on two workers and on four.
    #[test]
    fn a_waker_that_blocks_on_a_lock_its_wakee_holds_is_not_stranded() {
        for workers in [2usize, 4] {
            let pool = Scheduler::started(workers, true);
            let lock = Arc::new(SpinLock::new());
            let released = Arc::new(AtomicBool::new(false));
            let x_waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
            {
                let (lock, x_waker, released) = (lock.clone(), x_waker.clone(), released.clone());
                pool.spawn(Task::new(move || {
                    lock.take();
                    *x_waker.lock().unwrap() = waker_for_current();
                    park_current();
                    released.store(true, Ordering::SeqCst);
                    lock.give_back();
                }));
            }
            until_parked(&pool, 1);
            let stop = Arc::new(AtomicBool::new(false));
            busy_spinners(&pool, 2 * workers, &stop);

            let got_it = Arc::new(AtomicBool::new(false));
            {
                let (lock, x_waker, got_it) = (lock.clone(), x_waker.clone(), got_it.clone());
                let released = released.clone();
                pool.spawn(Task::new(move || {
                    x_waker.lock().unwrap().take().expect("X's waker").wake();
                    // Holds this worker's thread until X has run, or gives
                    // up; `got_it` stays false then and the test fails.
                    if lock.take_in_time() {
                        let in_order = released.load(Ordering::SeqCst);
                        lock.give_back();
                        got_it.store(in_order, Ordering::SeqCst);
                    }
                }));
            }
            let in_time = came(&got_it, std::time::Duration::from_secs(10));
            stop.store(true, Ordering::SeqCst);
            drained(&pool);
            assert!(
                in_time,
                "{workers} workers: the fiber W woke was stranded behind W's blocked thread \
                 while the other workers were busy: {:?}",
                pool.counts()
            );
        }
    }

    /// **The same, without a lock: the waker keeps its worker until the
    /// fiber it woke has run.** A spinning waker is not a deadlock -- it
    /// bounds only how late the woken fiber is -- but it is the plainest
    /// form of the question: does a busy pool ever look at a worker's queue
    /// while that worker is not getting to it?
    #[test]
    fn a_waker_that_holds_its_worker_does_not_hold_back_the_fiber_it_woke() {
        for workers in [2usize, 4] {
            let pool = Scheduler::started(workers, true);
            let (wakers, resumed) = parked_fibers(&pool, 1);
            let waker = wakers.lock().unwrap().pop().expect("a waker");
            let stop = Arc::new(AtomicBool::new(false));
            busy_spinners(&pool, 2 * workers, &stop);

            let ran = Arc::new(AtomicBool::new(false));
            {
                let (ran, resumed) = (ran.clone(), resumed.clone());
                pool.spawn(Task::new(move || {
                    waker.wake();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while resumed.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
                        std::hint::spin_loop();
                    }
                    ran.store(resumed.load(Ordering::SeqCst) == 1, Ordering::SeqCst);
                }));
            }
            let in_time = came(&ran, std::time::Duration::from_secs(12));
            stop.store(true, Ordering::SeqCst);
            drained(&pool);
            assert!(
                in_time,
                "{workers} workers: the woken fiber did not run while its waker held the worker: {:?}",
                pool.counts()
            );
        }
    }

    /// **The case that needs no wake at all, and was there before the local
    /// wake path.** H takes a lock and is preempted holding it, which puts H
    /// back on its own worker's queue; B, next on that queue, blocks the
    /// thread on the same lock. H can only run elsewhere, and every other
    /// worker is busy. On either wake path, since nothing here is a wake.
    ///
    /// **B waits for H to hold the lock before it reaches for it.** Both
    /// start on one worker's queue, H first, but nothing keeps them in that
    /// order: a steal of the back half of a queue of two takes B alone, and B
    /// then ran first on the other worker (2 rounds in 2,000 with four
    /// spinners on two CPUs). B took the free lock, its assert panicked on
    /// the worker with the lock still held, H spun on it for ever, and
    /// dropping the pool joined that worker: the test hung. Waiting on
    /// `holding` blocks B's thread whichever order they run in, which is the
    /// case under test either way.
    #[test]
    fn a_fiber_preempted_holding_a_lock_is_not_stranded_behind_one_blocked_on_it() {
        for wake_local in [false, true] {
            let pool = Scheduler::started(2, wake_local);
            let stop = Arc::new(AtomicBool::new(false));
            busy_spinners(&pool, 4, &stop);

            let lock = Arc::new(SpinLock::new());
            let holding = Arc::new(AtomicBool::new(false));
            let released = Arc::new(AtomicBool::new(false));
            let got_it = Arc::new(AtomicBool::new(false));
            {
                let (lock, got_it, released) = (lock.clone(), got_it.clone(), released.clone());
                let holding = holding.clone();
                // One fiber schedules both, so both land on its worker's queue,
                // H first.
                pool.spawn(Task::new(move || {
                    let (held, done, has_it) = (lock.clone(), released.clone(), holding.clone());
                    schedule(Task::new(move || {
                        // Nobody else takes it before `has_it` is set.
                        held.take();
                        has_it.store(true, Ordering::SeqCst);
                        // Preempted holding it: back to the end of this
                        // worker's queue, behind B.
                        suspend();
                        done.store(true, Ordering::SeqCst);
                        held.give_back();
                    }));
                    schedule(Task::new(move || {
                        // Each wait blocks this worker's thread, and gives
                        // up in time; `got_it` stays false then.
                        if spin_until_set(&holding) && lock.take_in_time() {
                            let in_order = released.load(Ordering::SeqCst);
                            lock.give_back();
                            got_it.store(in_order, Ordering::SeqCst);
                        }
                    }));
                }));
            }
            let in_time = came(&got_it, std::time::Duration::from_secs(10));
            stop.store(true, Ordering::SeqCst);
            drained(&pool);
            assert!(
                in_time,
                "wake_local={wake_local}: a fiber preempted holding a lock was stranded behind \
                 the fiber blocked on it: {:?}",
                pool.counts()
            );
        }
    }

    /// **A busy pool with nobody blocked moves (almost) nothing between
    /// workers.** Every worker has a queue of fibers that only yield, so none
    /// is ever idle and none is ever stuck: the tick has nothing to rescue.
    /// Stealing on every tick regardless moved half of some worker's queue
    /// each time, which with a long queue cost up to 45% of the pool's fiber
    /// turns in moving fibers back and forth.
    ///
    /// Counted over a window after the spinners are spread: fewer than half
    /// the ticks may take anything. An unconditional tick takes something on
    /// nearly every one (152,528 steals in 152,526 ticks, measured), so half
    /// still tells the two apart by a wide margin. The margin is for a worker
    /// thread the OS deschedules, which does look stuck and is correctly
    /// stolen from: on GitHub's three-CPU macOS runner, with the rest of the
    /// suite running beside it, 16% of ticks took something. That didn't
    /// reproduce on Linux pinned to three CPUs (0 of 10 whole-suite runs), so
    /// the bound is set by the measurement, not by a model of the runner.
    #[test]
    fn a_busy_pool_with_nobody_blocked_steals_almost_nothing_on_the_tick() {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        if cpus < 2 {
            eprintln!("  note: one CPU, so no worker can steal from another; not checked");
            return;
        }
        let pool = Scheduler::started(cpus.min(4), true);
        let stop = Arc::new(AtomicBool::new(false));
        busy_spinners(&pool, 64, &stop);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let before = pool.counts();
        let turns_before: u64 = pool.turns().iter().sum();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let after = pool.counts();
        let turns_after: u64 = pool.turns().iter().sum();
        stop.store(true, Ordering::SeqCst);
        drained(&pool);
        let ticks = (turns_after - turns_before) / GLOBAL_INTERVAL as u64;
        let steals = after.steals_succeeded - before.steals_succeeded;
        let moved = after.fibers_stolen - before.fibers_stolen;
        assert!(ticks > 100, "too few turns to judge: {ticks} ticks");
        assert!(
            steals * 2 < ticks,
            "{steals} steals moved {moved} fibers in {ticks} ticks, with no worker blocked"
        );
    }

    // `a_fiber_keeps_its_identity_across_workers` used to live here, and it
    // never checked that a fiber changed worker. It asserted that an identity
    // was stable across a suspension, which is also true of a fiber that spent
    // its whole life on one thread -- so on a run where nothing migrated it
    // passed having tested nothing, and `docs/design/soundness.md` named it as
    // the test protecting thread-affinity.
    //
    // It is `crate::migration` now, under the same name so that every citation
    // still resolves, and it retries until it observes a migration.

    /// **One park, one resumption, however many wakes arrive.**
    ///
    /// The release gate asks that every runnable `Task` have exactly one owner
    /// at every instant, and that wake tokens never create a second. `wake`
    /// carries the argument; this is the part that can fail.
    ///
    /// Each fiber parks and is then woken by every thread at once. A runtime
    /// where two wakes could both claim a parked task would inject it twice,
    /// and the fiber would resume twice from one park -- so the assertion is
    /// that resumptions equal parks, not that nothing crashed. The audit is
    /// checked as well, because a task injected twice is also a task counted
    /// twice.
    #[test]
    fn an_avalanche_of_wakes_resumes_a_fiber_once() {
        const FIBERS: usize = 24;
        const PARKS: usize = 8;
        const SHOUTERS: usize = 4;

        let resumed = Arc::new(AtomicUsize::new(0));
        let parked = Arc::new(Mutex::new(Vec::<Waker>::new()));

        let pool = Scheduler::new(4);
        for _ in 0..FIBERS {
            let woke = resumed.clone();
            let box_office = parked.clone();
            pool.spawn(Task::new(move || {
                for _ in 0..PARKS {
                    let Some(waker) = waker_for_current() else { break };
                    // Handed out before parking, so the shouters below can
                    // reach a fiber that is not yet asleep -- which is the
                    // race the second claim in `wake` exists for.
                    box_office.lock().unwrap().push(waker);
                    park_current();
                    woke.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        // Wake everything, repeatedly, from several threads at once. Every
        // wake after the first for a given park is the one that must not find
        // a task to take.
        let stop = Arc::new(AtomicUsize::new(0));
        let shouters: Vec<_> = (0..SHOUTERS)
            .map(|_| {
                let box_office = parked.clone();
                let finished = stop.clone();
                std::thread::spawn(move || {
                    while finished.load(Ordering::SeqCst) == 0 {
                        let wakers: Vec<Waker> = box_office.lock().unwrap().drain(..).collect();
                        for waker in &wakers {
                            waker.wake();
                            waker.wake();
                        }
                        std::thread::yield_now();
                    }
                })
            })
            .collect();

        pool.drain();
        stop.store(1, Ordering::SeqCst);
        for shouter in shouters {
            let _ = shouter.join();
        }

        assert_eq!(
            resumed.load(Ordering::SeqCst),
            FIBERS * PARKS,
            "a fiber resumed a different number of times than it parked, so a park was claimed \
             twice or not at all"
        );

        let audit = pool.audit();
        assert_eq!(audit.parked, 0, "a fiber was left parked: {audit:?}");
        assert_eq!(audit.in_transit, 0, "a task was left in transit: {audit:?}");
        assert_eq!(
            audit.spawned, audit.completed,
            "every fiber spawned should have completed exactly once: {audit:?}"
        );
    }
}

