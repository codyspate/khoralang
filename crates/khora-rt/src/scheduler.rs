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
    /// safepoint is emitted at every loop back-edge — and a call that cannot
    /// be inlined would show up where nothing else here would.
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
    /// sleeps for ever — `cancelling_a_sleeping_fiber_wakes_it_to_notice`.
    ///
    /// A waker needs the *state* to set `NOTIFIED` on, and that exists from the
    /// moment the fiber does.
    live: Mutex<std::collections::HashMap<usize, Arc<crate::current::Fiber>>>,
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
    /// Tasks that belong to no queue at this instant because somebody is
    /// carrying them between two.
    ///
    /// **The sixth place a fiber can be, and the audit is wrong without it.**
    /// A waker holds a task between `parked` and `inject`; a thief holds half a
    /// queue between two deques. Neither is a worker, so neither is bounded by
    /// the worker count, which is what `Audit::in_hand` would otherwise
    /// assume.
    in_transit: AtomicUsize,
    stopping: AtomicBool,
    counts: Counts,
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
            timers_dead: self.timers_dead.load(Ordering::Relaxed),
            timers_added: self.timers_added.load(Ordering::Relaxed),
            sockets_ready: self.sockets_ready.load(Ordering::Relaxed),
            wakes_before_waiting: self.wakes_before_waiting.load(Ordering::Relaxed),
            steals_attempted: self.steals_attempted.load(Ordering::Relaxed),
            steals_succeeded: self.steals_succeeded.load(Ordering::Relaxed),
            fibers_stolen: self.fibers_stolen.load(Ordering::Relaxed),
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
    pub(crate) fn new(workers: usize) -> Scheduler {
        let workers = match workers {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        // Built before the threads, because each worker's queue has to be
        // reachable by every other worker from the first instant one exists.
        let locals: Vec<Arc<Mutex<VecDeque<Task>>>> =
            (0..workers).map(|_| Arc::new(Mutex::new(VecDeque::new()))).collect();

        let shared = Arc::new(Shared {
            queued: Mutex::new(VecDeque::new()),
            locals,
            live: Mutex::new(std::collections::HashMap::new()),
            parked: Mutex::new(std::collections::HashMap::new()),
            timers: Mutex::new(Timers::default()),
            reactor: Reactor::default(),
            arrived: Condvar::new(),
            polling: AtomicBool::new(false),
            in_transit: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            counts: Counts::default(),
        });

        let mut handles: Vec<std::thread::JoinHandle<()>> = (0..workers)
            .map(|index| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("khora-worker-{index}"))
                    .spawn(move || work(shared, index))
                    .expect("a worker thread")
            })
            .collect();

        // One thread watching sockets. Like the timer thread, this is a
        // thread waiting so that no *worker* has to — which is the whole
        // distinction the phase turns on.
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
                            "khora-scheduler {:?} queued={} local={} parked={} live={}",
                            watching.counts.snapshot(),
                            watching.queued.lock().expect("the shared queue").len(),
                            watching
                                .locals
                                .iter()
                                .map(|q| q.lock().expect("a local queue").len())
                                .sum::<usize>(),
                            watching.parked.lock().expect("the parked fibers").len(),
                            watching.live.lock().expect("the live fibers").len(),
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
        let Some(state) = state_of(&self.shared, fiber) else { return };

        state.cancel();
        // Otherwise a hundred thousand cancelled sleepers hold the heap and
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
    let Some(shared) = shared_pool() else { return false };
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
    shared.timers.lock().expect("the timers").add(at, id);
    park_current()
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
    // In this thread's hands from here until `inject`, and in no queue.
    shared.in_transit.fetch_add(1, Ordering::AcqRel);
    drop(parked);

    state.running();
    if state.stop_counting() {
        shared.counts.waiting.fetch_sub(1, Ordering::Relaxed);
    }
    inject(shared, task);
    shared.in_transit.fetch_sub(1, Ordering::AcqRel);
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
    let parked = park_current();
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

/// Wakes every fiber whose socket has become ready, for ever.
fn watch(shared: Arc<Shared>) {
    while !shared.stopping.load(Ordering::Acquire) {
        // A short wait rather than an indefinite one, because a registration
        // arriving while `poll` is already blocked would otherwise not be seen
        // until something else woke it. A self-pipe or an event handle is the
        // refinement, and it is the same shape of change on all three
        // platforms.
        let ready = shared.reactor.poll(std::time::Duration::from_millis(50));
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

/// Wakes every fiber whose deadline has passed, for ever.
fn tick(shared: Arc<Shared>) {
    while !shared.stopping.load(Ordering::Acquire) {
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
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        match next(&shared, &local, me, &mut turn) {
            Some(task) => run(&shared, &local, task),
            // **An idle worker looks for I/O itself rather than sleeping while
            // another thread does it.** Readiness discovered here is readiness
            // discovered by the thread that is about to run the fiber, which
            // is one operating-system handoff shorter than being told. Only
            // one worker does this; the rest park as they always did.
            None if serve_io(&shared) => continue,
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
fn next(
    shared: &Arc<Shared>,
    local: &Arc<Mutex<VecDeque<Task>>>,
    me: usize,
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
    if let Some(task) = shared.queued.lock().expect("the shared queue").pop_front() {
        return Some(task);
    }
    steal(shared, local, me)
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
/// workers do not all descend on worker 0 together.
fn steal(
    shared: &Arc<Shared>,
    mine: &Arc<Mutex<VecDeque<Task>>>,
    me: usize,
) -> Option<Task> {
    let workers = shared.locals.len();
    if workers < 2 {
        return None;
    }
    shared.counts.steals_attempted.fetch_add(1, Ordering::Relaxed);

    for offset in 1..workers {
        let victim = (me + offset) % workers;
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
/// under it while it waited.
///
/// **The wait is bounded even though `inject` nudges.** A local queue has no
/// way to announce itself the way the shared one does, so going back to look is
/// how work in somebody else's deque is ever found.
fn serve_io(shared: &Arc<Shared>) -> bool {
    if shared.polling.swap(true, Ordering::AcqRel) {
        return false;
    }
    let ready = shared.reactor.poll(std::time::Duration::from_millis(10));
    shared.polling.store(false, Ordering::Release);

    for id in ready {
        shared.counts.sockets_ready.fetch_add(1, Ordering::Relaxed);
        if let Some(state) = state_of(shared, id) {
            wake(shared, id, state.wait());
        }
    }
    true
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coro::suspend;

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

    /// **The regression test for a hang.** Cancelling a fiber that has not run
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
    fn cancelling_a_fiber_before_anybody_holds_it_is_not_lost() {
        let noticed = Arc::new(AtomicUsize::new(0));
        let counter = noticed.clone();

        let pool = Scheduler::new(1);
        let task = Task::new(move || {
            // Nobody will ever wake this on its own merits.
            park_current();
            if crate::current::current(|f| f.is_cancelled()) {
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
    /// cancellable, and its watch must not outlive it.
    #[test]
    fn cancelling_a_fiber_waiting_on_a_socket_wakes_it() {
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
            if crate::current::current(|f| f.is_cancelled()) {
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
