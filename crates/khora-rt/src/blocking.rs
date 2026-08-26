//! A bounded pool of threads for work that genuinely blocks.
//!
//! The reactor answers everything that can be waited on — sockets, deadlines —
//! by suspending a fiber instead of a worker. Some things cannot be waited on:
//! a filesystem read, a name lookup, a subprocess, a foreign library that goes
//! away for a while. Run one on a worker and the worker is gone for the
//! duration, along with every fiber queued behind it.
//!
//! So they run here, and the fiber that asked for them suspends the way it
//! would for a socket. `docs/design/scheduler.md` §9.
//!
//! # Bounded, and why that is the whole point
//!
//! A pool that starts a thread per blocking call is thread-per-fiber wearing a
//! different hat. So two limits: how many threads there can be, and how much
//! work may be queued for them. When the queue is full the *fiber* waits rather
//! than its worker, so a program asking for a million file reads gets slower
//! rather than larger.
//!
//! Threads are started on demand and retired when they go idle, so a program
//! that never blocks never pays for any of this.
//!
//! # What a blocking call is not
//!
//! **It is not a cancellation point.** A fiber cancelled while it is waiting
//! for `fread` to come back keeps waiting: the pool cannot interrupt foreign
//! code, and pretending otherwise would mean returning while a thread still
//! holds the caller's buffer. The cancellation is observed at the next `!`,
//! which is the same rule safepoints follow — `docs/design/scheduler.md` §1.

use crate::scheduler::{park_current, waker_for_current, Waker};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};

/// A unit of work, and everything it needs to report back.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// How long a thread sits idle before retiring.
///
/// Long enough that a program doing occasional file work keeps its threads
/// warm, short enough that a program that blocked once at startup does not
/// hold them for its whole life.
const IDLE: std::time::Duration = std::time::Duration::from_secs(10);

/// Everything the pool guards with one lock.
struct Queue {
    /// Work waiting for a thread, each with the fiber to hand back to.
    jobs: VecDeque<(Job, Waker)>,
    /// Threads that exist right now.
    started: usize,
    /// Threads inside a job right now.
    active: usize,
    /// Fibers waiting for room in `jobs`.
    blocked: VecDeque<Waker>,
    /// Jobs finished, ever.
    ran: u64,
    /// Times a submitter found the queue full and had to wait.
    waited: u64,
}

/// A bounded pool of blocking threads.
pub(crate) struct Pool {
    queue: Mutex<Queue>,
    /// Wakes a thread that is waiting for work.
    arrived: Condvar,
    /// The most threads that may exist at once.
    threads: usize,
    /// The most jobs that may be queued before submitters start waiting.
    depth: usize,
}

/// What the pool is doing, for `docs/design/scheduler.md` §14.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stats {
    pub(crate) queued: usize,
    pub(crate) active: usize,
    pub(crate) threads: usize,
    pub(crate) ran: u64,
    pub(crate) waited: u64,
}

impl Pool {
    pub(crate) fn new(threads: usize, depth: usize) -> Pool {
        Pool {
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                started: 0,
                active: 0,
                blocked: VecDeque::new(),
                ran: 0,
                waited: 0,
            }),
            arrived: Condvar::new(),
            threads: threads.max(1),
            depth: depth.max(1),
        }
    }

    pub(crate) fn stats(&self) -> Stats {
        let queue = self.queue.lock().expect("the blocking queue");
        Stats {
            queued: queue.jobs.len(),
            active: queue.active,
            threads: queue.started,
            ran: queue.ran,
            waited: queue.waited,
        }
    }

    /// Hands `job` to the pool, waiting for room if there is none.
    ///
    /// The waiting is the backpressure, and it is a *fiber* waiting: the worker
    /// underneath goes off and runs something else, so a program that
    /// oversubscribes the pool slows down without stalling.
    fn submit(&'static self, job: Job, done: Waker) {
        let mut queue = self.queue.lock().expect("the blocking queue");
        while queue.jobs.len() >= self.depth {
            // Only a fiber can wait without costing a thread. Off one there is
            // nothing to suspend, and exceeding the depth beats deadlocking.
            let Some(room) = waker_for_current() else { break };
            queue.waited += 1;
            queue.blocked.push_back(room);
            drop(queue);
            park_current();
            queue = self.queue.lock().expect("the blocking queue");
        }

        queue.jobs.push_back((job, done));
        // **Grow when the queue outruns the idle threads**, which is not the
        // same as growing when every thread is busy. The first version asked
        // `active == started`, and a thread that has been spawned but has not
        // yet reached its first job is neither busy nor able to help: with
        // forty jobs arriving faster than the one existing thread could pick
        // one up, the pool stayed at a single thread and ran the lot in
        // series. Idle threads are `started - active`, and one more job than
        // that is a job with nobody to run it.
        let idle = queue.started - queue.active;
        if queue.started < self.threads && queue.jobs.len() > idle {
            queue.started += 1;
            self.start();
        }
        drop(queue);
        self.arrived.notify_one();
    }

    /// Starts one thread. The caller has already counted it in `started`.
    fn start(&'static self) {
        let started = std::thread::Builder::new()
            .name("khora-blocking".to_string())
            .spawn(move || self.serve());
        if started.is_err() {
            // The machine would not give us a thread. Undo the count so the
            // next submission tries again rather than believing a thread
            // exists that does not.
            self.queue.lock().expect("the blocking queue").started -= 1;
        }
    }

    /// One blocking thread's whole life.
    fn serve(&'static self) {
        loop {
            let job = {
                let mut queue = self.queue.lock().expect("the blocking queue");
                loop {
                    if let Some(job) = queue.jobs.pop_front() {
                        queue.active += 1;
                        break job;
                    }
                    let (waited, timeout) = self
                        .arrived
                        .wait_timeout(queue, IDLE)
                        .expect("the blocking queue");
                    queue = waited;
                    // Retiring is decided under the lock and with the queue
                    // seen empty, so a thread cannot leave work behind it.
                    if timeout.timed_out() && queue.jobs.is_empty() {
                        queue.started -= 1;
                        return;
                    }
                }
            };

            let (job, done) = job;
            job();

            let mut queue = self.queue.lock().expect("the blocking queue");
            queue.active -= 1;
            queue.ran += 1;
            // A slot just came free. Whoever has been waiting longest for it
            // gets told, under the same lock that freed it.
            let room = queue.blocked.pop_front();
            drop(queue);
            if let Some(room) = room {
                room.wake();
            }

            // **The waiting fiber is told last, after the books are straight.**
            // Waking it from inside the job — which is where this used to be —
            // lets it run, finish, and have somebody read `stats` while this
            // thread has not yet decremented `active` or counted `ran`. That is
            // not a wrong answer the pool ever gives itself, but it is one an
            // observer can see, and it made two tests fail about one run in ten
            // with `active: 1, ran: 39` after everything had finished. Waking
            // here means a fiber back from a blocking call is proof that the
            // call is accounted for.
            done.wake();
        }
    }
}

/// The pool every Khora program shares, started the first time it is needed.
pub(crate) fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        // **Deliberately modest, and meant to be measured.** The bound's job is
        // to stop the pool becoming thread-per-fiber; the backpressure below it
        // is what handles a program that wants more concurrency than this. Two
        // per core is a starting point and not a result.
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = std::env::var("KHORA_BLOCKING_THREADS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or((cores * 2).max(4));
        Pool::new(threads, threads * 8)
    })
}

/// Runs `work` somewhere it cannot stall a worker, and returns what it gave.
///
/// On a fiber: the work goes to the pool, the fiber suspends, and the worker
/// carries on with something else. Anywhere else — the program's own
/// computation, a thread that is not a worker — there is no worker to protect,
/// so the work happens right here and the pool is not involved at all.
pub(crate) fn blocking<T, F>(work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    blocking_on(pool(), work)
}

/// [`blocking`], against a particular pool. Separate so tests can bound one
/// tightly enough to observe the bound.
pub(crate) fn blocking_on<T, F>(pool: &'static Pool, work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let Some(done) = waker_for_current() else { return work() };

    let slot: std::sync::Arc<Mutex<Option<T>>> = std::sync::Arc::new(Mutex::new(None));
    let into = slot.clone();
    pool.submit(
        Box::new(move || {
            let value = work();
            *into.lock().expect("the result slot") = Some(value);
        }),
        done,
    );

    // **The loop is the protocol.** A wake that is not ours — a cancellation,
    // another waiter's notification — puts us straight back round, because the
    // job is still running either way and the only thing that ends this wait is
    // a value in the slot. A wake that arrives before we park is not lost
    // either: `declare` refuses to wait when one is pending, so `park_current`
    // returns at once and the next look finds the value.
    loop {
        // **Taken into a local before the `if`, so the guard is visibly gone
        // by the suspension.** It was already gone -- a temporary in an
        // `if let` scrutinee drops at the end of the `if let` -- but a
        // `MutexGuard` held across a suspension is released on whichever
        // worker resumes the fiber rather than the one that took it, which is
        // the residual obligation `unsafe impl Send for Task` leaves on Rust
        // bodies. A safety property should not rest on a temporary-scope rule
        // the reader has to recall.
        let arrived = slot.lock().expect("the result slot").take();
        if let Some(value) = arrived {
            return value;
        }
        park_current();
    }
}

// What the pool is doing, as C symbols, beside the allocation counters in
// `crate::counters` and there for the same reason: `docs/design/scheduler.md`
// §14 asks for queued and active so that a slow result can say *why* it was
// slow. A saturated blocking pool looks exactly like a slow program from the
// outside, and these are the difference.

/// Jobs waiting for a blocking thread right now.
#[unsafe(no_mangle)]
pub extern "C" fn khora_blocking_queued() -> usize {
    pool().stats().queued
}

/// Blocking threads inside a job right now.
#[unsafe(no_mangle)]
pub extern "C" fn khora_blocking_active() -> usize {
    pool().stats().active
}

/// Blocking jobs finished since the process started.
#[unsafe(no_mangle)]
pub extern "C" fn khora_blocking_ran() -> i64 {
    pool().stats().ran as i64
}

/// Times a fiber found the pool's queue full and had to wait for room.
///
/// **The backpressure counter.** Zero means the pool has never been the
/// bottleneck; a number that climbs with load means it is, and that
/// `KHORA_BLOCKING_THREADS` is the knob.
#[unsafe(no_mangle)]
pub extern "C" fn khora_blocking_waited() -> i64 {
    pool().stats().waited as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coro::Task;
    use crate::scheduler::{schedule, Scheduler};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A pool of its own, leaked, because `blocking_on` wants a `'static` one
    /// and a test that shared the global could not bound it.
    fn a_pool(threads: usize, depth: usize) -> &'static Pool {
        Box::leak(Box::new(Pool::new(threads, depth)))
    }

    #[test]
    fn the_work_happens_on_another_thread() {
        let pool = a_pool(2, 8);
        let elsewhere = Arc::new(AtomicUsize::new(0));

        let seen = elsewhere.clone();
        let scheduler = Scheduler::new(1);
        scheduler.spawn(Task::new(move || {
            let worker = std::thread::current().id();
            let ran_on = blocking_on(pool, || std::thread::current().id());
            seen.store(usize::from(worker != ran_on), Ordering::SeqCst);
        }));
        scheduler.drain();

        assert_eq!(elsewhere.load(Ordering::SeqCst), 1, "it ran on the worker");
    }

    /// **The point of 11E.** One worker, one fiber that blocks for a while, and
    /// another that does not: the second must not be stuck behind the first.
    #[test]
    fn a_blocked_fiber_does_not_hold_its_worker() {
        let pool = a_pool(2, 8);
        let order = Arc::new(Mutex::new(Vec::new()));

        let scheduler = Scheduler::new(1);
        let slow = order.clone();
        scheduler.spawn(Task::new(move || {
            blocking_on(pool, || std::thread::sleep(std::time::Duration::from_millis(80)));
            slow.lock().expect("order").push("slow");
        }));
        let quick = order.clone();
        scheduler.spawn(Task::new(move || {
            quick.lock().expect("order").push("quick");
        }));
        scheduler.drain();

        assert_eq!(
            *order.lock().expect("order"),
            vec!["quick", "slow"],
            "the second fiber waited for the first to come back"
        );
    }

    /// The thread bound holds however much work arrives at once.
    #[test]
    fn the_pool_never_runs_more_threads_than_it_promised() {
        const LIMIT: usize = 3;
        let pool = a_pool(LIMIT, 64);
        let now = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let scheduler = Scheduler::new(4);
        scheduler.spawn(Task::new({
            let now = now.clone();
            let peak = peak.clone();
            move || {
                for _ in 0..40 {
                    let now = now.clone();
                    let peak = peak.clone();
                    schedule(Task::new(move || {
                        blocking_on(pool, move || {
                            let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(n, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(5));
                            now.fetch_sub(1, Ordering::SeqCst);
                        });
                    }));
                }
            }
        }));
        scheduler.drain();

        let most = peak.load(Ordering::SeqCst);
        let stats = pool.stats();
        assert!(most > 1, "nothing ever overlapped: {stats:?}");
        assert!(most <= LIMIT, "{most} at once against a bound of {LIMIT}: {stats:?}");
        assert_eq!(stats.ran, 40, "{stats:?}");
    }

    /// A queue with no room makes the *fiber* wait, and everything still runs.
    #[test]
    fn a_full_queue_pushes_back_on_the_submitter() {
        let pool = a_pool(1, 1);
        let done = Arc::new(AtomicUsize::new(0));

        let scheduler = Scheduler::new(2);
        scheduler.spawn(Task::new({
            let done = done.clone();
            move || {
                for _ in 0..20 {
                    let done = done.clone();
                    schedule(Task::new(move || {
                        blocking_on(pool, move || {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            done.fetch_add(1, Ordering::SeqCst);
                        });
                    }));
                }
            }
        }));
        scheduler.drain();

        assert_eq!(done.load(Ordering::SeqCst), 20);
        let stats = pool.stats();
        assert!(stats.waited > 0, "the queue never filled: {stats:?}");
        assert_eq!(stats.ran, 20, "{stats:?}");
        assert_eq!(stats.queued, 0, "{stats:?}");
        assert_eq!(stats.active, 0, "{stats:?}");
    }

    /// Off a scheduler there is no worker to protect, so the pool stays out of
    /// it and the work happens on the calling thread.
    #[test]
    fn without_a_scheduler_the_work_runs_where_it_was_asked_for() {
        let pool = a_pool(2, 8);
        let here = std::thread::current().id();
        assert_eq!(blocking_on(pool, || std::thread::current().id()), here);
        assert_eq!(pool.stats().ran, 0, "the pool should not have been touched");
    }

    /// Cancelling a fiber that is inside a blocking call does not cut the call
    /// short — it cannot, and the value still arrives.
    #[test]
    fn a_blocking_call_is_not_a_cancellation_point() {
        let pool = a_pool(2, 8);
        let got = Arc::new(AtomicUsize::new(0));

        let scheduler = Scheduler::new(2);
        let value = got.clone();
        let task = Task::new(move || {
            let answer = blocking_on(pool, || {
                std::thread::sleep(std::time::Duration::from_millis(40));
                7usize
            });
            value.store(answer, Ordering::SeqCst);
        });
        let id = task.fiber().id();
        scheduler.spawn(task);

        std::thread::sleep(std::time::Duration::from_millis(10));
        scheduler.cancel_fiber(id);
        scheduler.drain();

        assert_eq!(got.load(Ordering::SeqCst), 7, "the blocking call was cut short");
    }
}
