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
//! **It is not a cancellation point.** A fiber canceled while it is waiting
//! for `fread` to come back keeps waiting: the pool cannot interrupt foreign
//! code, and pretending otherwise would mean returning while a thread still
//! holds the caller's buffer. The cancellation is observed at the next `!`,
//! which is the same rule safepoints follow — `docs/design/scheduler.md` §1.

use crate::scheduler::{park_current_for, waker_for_current, Waker, Why};
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
    /// Work waiting for a thread, each with the fiber to hand back to, if a
    /// fiber is waiting for it. [`abandonable`] off a fiber has none: its
    /// job says it is done by itself.
    jobs: VecDeque<(Job, Option<Waker>)>,
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
    /// Notified when a job finishes and its slot frees, for an
    /// [`abandonable_on`] caller off a fiber that is waiting for room. Fibers
    /// wait in `blocked` instead.
    freed: Condvar,
    /// What the pool's threads are called, which is what a hang names.
    name: &'static str,
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
        Pool::named("khora-blocking", threads, depth)
    }

    fn named(name: &'static str, threads: usize, depth: usize) -> Pool {
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
            freed: Condvar::new(),
            name,
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
    ///
    /// **A waiter takes its own entry out of `blocked` every time it comes
    /// back to look**, whoever woke it. A finished job wakes exactly one
    /// waiter for the slot it freed, so an entry left behind by a fiber woken
    /// by something else -- a cancel, a stray wake -- is a wake spent on a
    /// fiber that has already moved on or enrolled again behind its own
    /// entry, and the waiter behind it is never told the slot exists: a fiber
    /// parked for room in an empty queue, for ever. The same rule as a
    /// channel's `withdraw`, for the same reason. What it costs: a scan of
    /// `blocked` per wake of a fiber that enrolled, which is short, because
    /// it only holds fibers that are waiting now.
    fn submit(&'static self, job: Job, done: Waker) {
        self.offer(job, Some(done), None);
    }

    /// [`Pool::submit`], and given `give_up`, stops waiting for room once it
    /// answers `Some`, and hands that back. `None` means the job was queued.
    ///
    /// **What `give_up` prevents: a cancel that waits for strangers' work.**
    /// Without it a canceled fiber woken while it waits for room finds the
    /// queue still full and parks again, so its cancel lands only once
    /// somebody else's job finishes -- and if those jobs are lookups against
    /// a dead name server, that is their timeout, not this fiber's.
    ///
    /// Without `give_up`, a caller off a fiber that finds the queue full
    /// queues anyway: there is nothing to suspend, and exceeding the depth
    /// beats deadlocking. With it, that caller waits on [`Pool::freed`] in
    /// slices that ask `give_up` between them, because a caller that can give
    /// up has no reason to exceed a bound it was given to respect.
    fn offer(
        &'static self,
        job: Job,
        done: Option<Waker>,
        give_up: Option<&dyn Fn() -> Option<Abandoned>>,
    ) -> Option<Abandoned> {
        let mut enrolled: Option<usize> = None;
        let mut queue = self.queue.lock().expect("the blocking queue");
        loop {
            if let Some(fiber) = enrolled.take() {
                if let Some(at) = queue.blocked.iter().position(|w| w.fiber() == fiber) {
                    queue.blocked.remove(at);
                }
            }
            if queue.jobs.len() < self.depth {
                break;
            }
            if let Some(why) = give_up.and_then(|asks| asks()) {
                return Some(why);
            }
            match (waker_for_current(), give_up) {
                (Some(room), _) => {
                    queue.waited += 1;
                    enrolled = Some(room.fiber());
                    queue.blocked.push_back(room);
                    drop(queue);
                    park_current_for(Why::BlockingRoom, 0);
                    queue = self.queue.lock().expect("the blocking queue");
                }
                (None, Some(_)) => {
                    queue.waited += 1;
                    queue = self
                        .freed
                        .wait_timeout(queue, ROOM_SLICE)
                        .expect("the blocking queue")
                        .0;
                }
                (None, None) => break,
            }
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
        None
    }

    /// Starts one thread. The caller has already counted it in `started`.
    fn start(&'static self) {
        let started = crate::stack::spawn_thread(
            std::thread::Builder::new().name(self.name.to_string()),
            move || self.serve(),
        );
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
            self.freed.notify_one();
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
            if let Some(done) = done {
                done.wake();
            }
        }
    }
}

/// How often a caller off a fiber that is waiting for room in a full pool
/// looks again for a stop or its deadline. A finished job also wakes it.
const ROOM_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

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
        park_current_for(Why::BlockingResult, std::sync::Arc::as_ptr(&slot) as usize);
    }
}

/// The pool name lookups run on, and only they.
///
/// **What this prevents: a dead name server stalling the program's file
/// reads.** An abandoned lookup cannot be interrupted: `getaddrinfo` runs to
/// the resolver's own timeout, seconds per lookup. On the shared pool, a
/// burst of abandoned lookups held every thread, and every later file read,
/// process wait and migration queued behind them: 100 canceled lookups
/// against a resolver that never answers held a file read on a fiber for
/// 29.7 s. Here they can only hold each other up.
///
/// **What it costs: four lookups at a time, program-wide.** With four
/// lookups in flight against a resolver that does not answer, the next one
/// waits for one of them to time out, giving up on a stop or its deadline;
/// it is never refused. A lookup abandoned before a thread reached it costs
/// nothing more: [`abandonable_on`] skips its work. `KHORA_LOOKUP_THREADS`
/// sets the number.
pub(crate) fn lookups() -> &'static Pool {
    static LOOKUPS: OnceLock<Pool> = OnceLock::new();
    LOOKUPS.get_or_init(|| {
        let threads = std::env::var("KHORA_LOOKUP_THREADS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(4);
        Pool::named("khora-lookup", threads, threads * 8)
    })
}

/// How an [`abandonable_on`] call ended without its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Abandoned {
    /// The calling fiber was asked to stop.
    Stopped,
    /// The deadline passed first.
    TimedOut,
}

/// Runs `work` on `pool` and waits for it, giving up when the calling fiber
/// is asked to stop or `deadline` passes.
///
/// **What this prevents: a cancel, or a deadline, that waits for a call
/// nobody can interrupt.** [`blocking`] waits for the value however long it
/// takes, which is right for a call holding the caller's buffer. A call that
/// owns everything it touches -- a name lookup, which takes a name and
/// answers addresses -- can be left to finish alone, and then a canceled
/// fiber need not wait for a resolver that is timing out.
///
/// **A call abandoned before a thread picks it up does not run.** One that a
/// thread had already started still runs to its end there, and its value is
/// dropped. So abandoning many calls to a resolver that never answers costs
/// one resolver timeout per thread, not one per call. While the pool's
/// threads are all held that way, the next caller waits for room, which it
/// also gives up on a stop or a deadline.
///
/// **Always on the pool, fiber or not**, unlike [`blocking`]: off a fiber the
/// caller waits on a condition variable a cancel notifies, the way a sleep on
/// the thread backend does, because the point is that the wait can end
/// before the work does.
pub(crate) fn abandonable_on<T, F>(
    pool: &'static Pool,
    work: F,
    deadline: Option<std::time::Instant>,
) -> Result<T, Abandoned>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let slot: std::sync::Arc<Answer<T>> = std::sync::Arc::new(Answer {
        value: Mutex::new(None),
        arrived: std::sync::Arc::new(Condvar::new()),
        abandoned: std::sync::atomic::AtomicBool::new(false),
    });
    // Whichever way this returns without the value, the job is told, so a
    // thread that has not reached it yet skips it.
    let _abandon = Abandon(&slot.abandoned);
    let into = slot.clone();
    let job: Job = Box::new(move || {
        if into.abandoned.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let value = work();
        *into.value.lock().expect("the result slot") = Some(value);
        into.arrived.notify_all();
    });
    let gave_up = || {
        if crate::current::current(|f| f.gives_up_waiting()) {
            Some(Abandoned::Stopped)
        } else if deadline.is_some_and(|at| std::time::Instant::now() >= at) {
            Some(Abandoned::TimedOut)
        } else {
            None
        }
    };
    if let Some(why) = gave_up() {
        return Err(why);
    }

    match waker_for_current() {
        Some(done) => {
            if let Some(why) = pool.offer(job, Some(done), Some(&gave_up)) {
                return Err(why);
            }
            loop {
                // Into a local first, for the reason `blocking_on` gives.
                let arrived = slot.value.lock().expect("the result slot").take();
                if let Some(value) = arrived {
                    return Ok(value);
                }
                if let Some(why) = gave_up() {
                    return Err(why);
                }
                crate::scheduler::park_current_until(
                    Why::BlockingResult,
                    std::sync::Arc::as_ptr(&slot) as usize,
                    deadline,
                );
            }
        }
        None => {
            if let Some(why) = pool.offer(job, None, Some(&gave_up)) {
                return Err(why);
            }
            let mut value = slot.value.lock().expect("the result slot");
            crate::current::current(|fiber| fiber.park_on(&slot.arrived));
            let answer = loop {
                if let Some(got) = value.take() {
                    break Ok(got);
                }
                if let Some(why) = gave_up() {
                    break Err(why);
                }
                // A cancel that lands between the look and the wait is seen
                // within one slice, as a parked `receive`'s is.
                let mut slice = crate::channel::LOOK_AGAIN;
                if let Some(at) = deadline {
                    slice = slice.min(at.saturating_duration_since(std::time::Instant::now()));
                }
                value = slot.arrived.wait_timeout(value, slice).expect("the result slot").0;
            };
            drop(value);
            crate::current::current(|fiber| fiber.unpark_from());
            answer
        }
    }
}

/// Where an [`abandonable_on`] call leaves its value, and what tells the
/// waiter off a fiber that it is there.
struct Answer<T> {
    value: Mutex<Option<T>>,
    arrived: std::sync::Arc<Condvar>,
    /// Set once nobody wants the value, so a thread that reaches the job
    /// later skips the work.
    abandoned: std::sync::atomic::AtomicBool,
}

/// Marks a job abandoned when its caller leaves, by any path.
///
/// Also set after the value arrived, which is harmless: the job has run.
struct Abandon<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for Abandon<'_> {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
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

    /// Canceling a fiber that is inside a blocking call does not cut the call
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

    /// Waits up to ten seconds for `done`, and says whether it came.
    fn within_ten_seconds(mut done: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        true
    }

    /// **A fiber woken while it waits for room leaves no entry behind.** A
    /// finished job wakes one waiter for the slot it freed. A waiter woken by
    /// something else -- a cancel, any stray wake -- that left its entry in
    /// the list had the next slot's wake spent on it, while the fiber queued
    /// behind it waited for a wake that never came: the long soak's hang,
    /// `queued: 0, active: 0` and fibers parked for room.
    ///
    /// One thread and room for one job. G holds the thread and Q fills the
    /// queue, so A waits for room. A is woken twice by somebody else, and B
    /// queues behind it. Then G is let go: G, Q and A's own job are the only
    /// three wakes that will ever come, and with A's two stale entries ahead
    /// of B, all three went to A.
    #[test]
    fn a_waiter_woken_by_something_else_does_not_take_the_next_waiters_room() {
        let pool = a_pool(1, 1);
        let scheduler = Scheduler::new(2);
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = Arc::new(AtomicUsize::new(0));
        let submitter = |finished: Arc<AtomicUsize>| {
            Task::new(move || {
                blocking_on(pool, || ());
                finished.fetch_add(1, Ordering::SeqCst);
            })
        };

        let held = gate.clone();
        scheduler.spawn(Task::new(move || {
            blocking_on(pool, move || {
                while !held.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            });
        }));
        assert!(within_ten_seconds(|| pool.stats().active == 1), "G never took the thread");
        scheduler.spawn(Task::new(|| blocking_on(pool, || ())));
        assert!(within_ten_seconds(|| pool.stats().queued == 1), "Q never queued");
        // G's and Q's submitters, parked for their results.
        assert!(within_ten_seconds(|| scheduler.waiting() == 2), "G and Q never parked");

        let a = submitter(finished.clone());
        let a_id = a.fiber().id();
        scheduler.spawn(a);
        for waited in 1..=3 {
            assert!(
                within_ten_seconds(|| pool.stats().waited == waited && scheduler.waiting() == 3),
                "A is not parked for room: {:?}",
                pool.stats()
            );
            if waited < 3 {
                scheduler.wake_fiber(a_id);
            }
        }
        scheduler.spawn(submitter(finished.clone()));
        assert!(
            within_ten_seconds(|| pool.stats().waited == 4 && scheduler.waiting() == 4),
            "B is not parked for room: {:?}",
            pool.stats()
        );

        gate.store(true, Ordering::SeqCst);
        let both = within_ten_seconds(|| finished.load(Ordering::SeqCst) == 2);
        let (stats, done) = (pool.stats(), finished.load(Ordering::SeqCst));
        if !both {
            // One more job's completion is the wake B was owed. Without it
            // B stays parked, and dropping the pool under it is not this
            // test's business.
            scheduler.spawn(Task::new(|| blocking_on(pool, || ())));
            within_ten_seconds(|| finished.load(Ordering::SeqCst) == 2);
        }
        assert!(
            both,
            "{done} of 2 submitters finished; the other waits for room in an empty queue: {stats:?}"
        );
    }

    /// Work that blocks until `release` is set, and counts how often it
    /// started: a resolver that never answers, standing in for `getaddrinfo`
    /// against a dead name server.
    fn hanging(release: Arc<std::sync::atomic::AtomicBool>, started: Arc<AtomicUsize>) -> impl FnOnce() + Send + 'static {
        move || {
            started.fetch_add(1, Ordering::SeqCst);
            while !release.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    /// **Abandoned lookups do not hold up a file read.** A lookup against a
    /// dead resolver is abandoned by its caller and still runs to the
    /// resolver's timeout. On the shared pool, enough of them held every
    /// thread, and a plain `blocking` call -- a file read, a process wait --
    /// queued behind them for as long: 29.7 s after 100 canceled lookups.
    ///
    /// More hanging lookups than the shared pool has threads, each abandoned
    /// by its deadline; then a `blocking` call on a fiber must come straight
    /// back.
    #[test]
    fn abandoned_lookups_leave_the_shared_pool_free() {
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));
        let callers: Vec<_> = (0..pool().threads + 4)
            .map(|_| {
                let work = hanging(release.clone(), started.clone());
                std::thread::spawn(move || {
                    let by = std::time::Instant::now() + std::time::Duration::from_millis(300);
                    abandonable_on(lookups(), work, Some(by))
                })
            })
            .collect();
        for caller in callers {
            assert_eq!(caller.join().expect("a lookup caller"), Err(Abandoned::TimedOut));
        }
        assert!(started.load(Ordering::SeqCst) > 0, "no lookup ever started");

        let answered = Arc::new(AtomicUsize::new(0));
        let scheduler = Scheduler::new(1);
        let into = answered.clone();
        let began = std::time::Instant::now();
        scheduler.spawn(Task::new(move || {
            into.store(blocking(|| 7), Ordering::SeqCst);
        }));
        let came = within_ten_seconds(|| answered.load(Ordering::SeqCst) == 7);
        let took = began.elapsed();
        // Let the hung lookups go either way, so the scheduler can drain.
        release.store(true, Ordering::SeqCst);
        assert!(came, "a blocking call waited more than 10 s behind abandoned lookups");
        scheduler.drain();
        assert!(took < std::time::Duration::from_secs(2), "{took:?}");
    }

    /// **A call abandoned before a thread reaches it does not run.** Without
    /// that, every abandoned lookup costs a resolver timeout whenever it is
    /// reached, long after its caller left, and a burst of them holds the
    /// threads for one timeout per call rather than one per thread.
    #[test]
    fn an_abandoned_call_that_never_started_is_skipped() {
        let pool = a_pool(1, 8);
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));
        let soon = || Some(std::time::Instant::now() + std::time::Duration::from_millis(50));

        // The one thread, held.
        let held = hanging(release.clone(), started.clone());
        assert_eq!(abandonable_on(pool, held, soon()), Err(Abandoned::TimedOut));
        assert!(within_ten_seconds(|| started.load(Ordering::SeqCst) == 1), "the first job never started");
        // Queued behind it, and given up on before the thread is free.
        let queued = hanging(release.clone(), started.clone());
        assert_eq!(abandonable_on(pool, queued, soon()), Err(Abandoned::TimedOut));

        release.store(true, Ordering::SeqCst);
        assert!(within_ten_seconds(|| pool.stats().ran == 2), "{:?}", pool.stats());
        assert_eq!(started.load(Ordering::SeqCst), 1, "the abandoned job ran anyway");
    }

    /// **Off a fiber, a caller that can give up does not overfill the pool.**
    /// It waits for room, and leaves at its deadline.
    #[test]
    fn a_caller_off_a_fiber_waits_for_room_and_gives_up_at_its_deadline() {
        let pool = a_pool(1, 1);
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));
        let soon = || Some(std::time::Instant::now() + std::time::Duration::from_millis(100));
        assert_eq!(abandonable_on(pool, hanging(release.clone(), started.clone()), soon()), Err(Abandoned::TimedOut));
        assert!(within_ten_seconds(|| started.load(Ordering::SeqCst) == 1), "the first job never started");
        assert_eq!(abandonable_on(pool, hanging(release.clone(), started.clone()), soon()), Err(Abandoned::TimedOut));
        let began = std::time::Instant::now();
        assert_eq!(abandonable_on(pool, || (), soon()), Err(Abandoned::TimedOut));
        assert!(began.elapsed() < std::time::Duration::from_secs(2), "{:?}", began.elapsed());
        assert_eq!(pool.stats().queued, 1, "{:?}", pool.stats());
        release.store(true, Ordering::SeqCst);
    }
}
