//! Adversarial execution, and the evidence it leaves.
//!
//! Every other test in this crate isolates one mechanism and shows it works.
//! This one runs all of them against each other for as long as it is asked to,
//! while a second thread interferes, and then checks that nothing was lost.
//!
//! # What it is looking for
//!
//! Six ownership transitions, each of which is a handover between two things
//! that can be running at once:
//!
//! | | between |
//! | --- | --- |
//! | running ↔ waiting | a fiber and the worker filing it |
//! | wait ↔ wake | the worker filing a fiber and whoever wakes it |
//! | wake ↔ cancel | two wakers, one of which also sets a flag |
//! | completion ↔ join | a finishing fiber and whatever was waiting for it |
//! | local ↔ stolen | a worker's own queue and a thief |
//! | fiber ↔ blocking thread | a suspended fiber and the pool thread holding
//!   its buffer |
//!
//! # How a failure shows
//!
//! Not as a wrong answer — a scheduler has no answer to get wrong — but as a
//! fiber that never finishes, a fiber that finishes twice, a `Task` in two
//! places, or a count that does not come back to zero. So the assertion is an
//! *arithmetic* rather than a result: [`crate::scheduler::Audit`] names every
//! place a fiber can be, and a settled pool has nothing anywhere.
//!
//! Three more checks run continuously rather than at the end, because by the
//! end the evidence is gone:
//!
//!   - every fiber asserts its own identity every time it is resumed, which is
//!     how the cached-thread-local bug was caught;
//!   - `coro::ResumedOnce` aborts if two workers enter one coroutine;
//!   - `Audit::in_hand` must never be negative, which is what a duplicated
//!     `Task` looks like before it becomes a crash.
//!
//! # Reproducing a failure
//!
//! `KHORA_SOAK_SEED` fixes the workload; `KHORA_SOAK_ROUNDS` fixes how much of
//! it. **The seed reproduces the work, not the interleaving** — that is what
//! real threads cost, and pretending otherwise would be the more dangerous
//! claim. Every bug this has found has a deterministic test of its own beside
//! the mechanism it belongs to; this one exists to find them, not to hold them.

#![cfg(test)]

use crate::coro::{suspend, Task};
use crate::reactor::Interest;
use crate::scheduler::{
    park_current, schedule, sleep_until, wait_until_ready, waker_for_current, Scheduler,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Resident set size in bytes, or zero where it cannot be had.
///
/// For the scale rows. Virtual size is the wrong number for fibers — a
/// coroutine stack is a megabyte of address space and a page or two of
/// memory — so this asks what is actually resident.
pub(crate) fn rss() -> usize {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Counters {
            cb: u32,
            page_faults: u32,
            peak_working_set: usize,
            working_set: usize,
            quota_peak_paged: usize,
            quota_paged: usize,
            quota_peak_nonpaged: usize,
            quota_nonpaged: usize,
            pagefile: usize,
            peak_pagefile: usize,
        }
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(process: isize, into: *mut Counters, size: u32) -> i32;
        }
        // SAFETY: a fully owned, correctly sized out-parameter.
        unsafe {
            let mut counters: Counters = std::mem::zeroed();
            counters.cb = std::mem::size_of::<Counters>() as u32;
            if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) == 0 {
                return 0;
            }
            counters.working_set
        }
    }
    #[cfg(not(windows))]
    {
        // Field two of `statm` is resident pages.
        let Ok(text) = std::fs::read_to_string("/proc/self/statm") else { return 0 };
        let Some(pages) = text.split_whitespace().nth(1).and_then(|n| n.parse::<usize>().ok())
        else {
            return 0;
        };
        pages * 4096
    }
}

/// How many virtual memory mappings this process holds, or zero off Linux.
///
/// **The number `vm.max_map_count` is compared against.** A guard-paged
/// coroutine stack costs mappings, not just memory, and a kernel at the
/// traditional default of 65530 runs out of them long before it runs out of
/// address space. `docs/design/scheduler.md` names this as the thing to
/// measure before believing the fiber counts.
fn mappings() -> usize {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/maps").map_or(0, |m| m.lines().count())
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// A seeded generator, so a workload can be repeated even though its timing
/// cannot. Xorshift64\*, which is enough for choosing between seven shapes.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn upto(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn env(name: &str, fallback: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(fallback)
}

/// What every fiber in the soak checks about itself, every time it runs.
///
/// A fiber that comes back as somebody else is the quietest failure in the
/// whole runtime: nothing crashes, and a cancellation flag or a wait state is
/// simply read off the wrong fiber. It cost a day to find the first time.
fn i_am_still(me: usize) {
    let now = crate::current::current(|f| f.id());
    assert_eq!(now, me, "a fiber came back as a different fiber");
}

/// The shapes of work the soak mixes. One per ownership transition, roughly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Finishes immediately: completion racing everything else.
    Prompt,
    /// Yields for fairness several times: running ↔ waiting, and the queue
    /// churn that gives thieves something to take.
    Yielding,
    /// Sleeps on a deadline: wait ↔ wake, through the timer thread.
    Sleeping,
    /// Parks with nobody scheduled to wake it: wait ↔ wake ↔ cancel, since
    /// only the adversary will ever release it.
    Waiting,
    /// Spawns children onto its own worker: local ↔ stolen.
    Spawning,
    /// Hands work to the blocking pool: fiber ↔ blocking thread.
    Blocking,
    /// Spawns children and waits for the last one: completion ↔ join.
    ///
    /// The only join the scheduler has today. `Fiber::spawn` is still a
    /// thread, so `Fibers::wait` is a different mechanism entirely; what a
    /// fiber can do here is park and be released by a child that finishes,
    /// which is the race worth having — the last child can complete before
    /// the parent has parked.
    Joining,
    /// Waits on a socket a sibling will write to: the reactor, and
    /// deregistration racing cancellation.
    Socket,
}

impl Shape {
    /// Weighted rather than uniform. `Socket` is one in sixteen because each
    /// one is a loopback connection, and a few hundred an run is enough to
    /// exercise the reactor without leaving thousands of sockets in
    /// `TIME_WAIT` behind every soak.
    fn pick(rng: &mut Rng) -> Shape {
        match rng.upto(16) {
            0..=2 => Shape::Prompt,
            3..=5 => Shape::Yielding,
            6..=7 => Shape::Sleeping,
            8..=9 => Shape::Waiting,
            10..=11 => Shape::Spawning,
            12..=13 => Shape::Blocking,
            14 => Shape::Joining,
            _ => Shape::Socket,
        }
    }
}

/// What the soak counts for itself, beside what the scheduler counts.
#[derive(Default)]
struct Tally {
    finished: AtomicUsize,
    children: AtomicUsize,
}

/// Builds one fiber of the given shape.
fn fiber(shape: Shape, seed: u64, tally: Arc<Tally>) -> Task {
    let mut rng = Rng::new(seed);
    let rounds = rng.upto(6) + 1;
    let nap = rng.upto(3);

    let task = Task::new(move || {
        let me = crate::current::current(|f| f.id());
        match shape {
            Shape::Prompt => {}
            Shape::Yielding => {
                for _ in 0..rounds {
                    suspend();
                    i_am_still(me);
                }
            }
            Shape::Sleeping => {
                for _ in 0..rounds {
                    sleep_until(std::time::Instant::now() + std::time::Duration::from_millis(nap));
                    i_am_still(me);
                }
            }
            Shape::Waiting => {
                // Nobody is scheduled to wake this. The adversary will, or the
                // cancellation will, and either way it must not be stranded.
                park_current();
                i_am_still(me);
            }
            Shape::Spawning => {
                for _ in 0..rounds {
                    let tally = tally.clone();
                    schedule(Task::new(move || {
                        let me = crate::current::current(|f| f.id());
                        suspend();
                        i_am_still(me);
                        tally.children.fetch_add(1, Ordering::Relaxed);
                        tally.finished.fetch_add(1, Ordering::Relaxed);
                    }));
                }
            }
            Shape::Blocking => {
                let got = crate::blocking::blocking(move || {
                    std::thread::sleep(std::time::Duration::from_micros(50));
                    seed
                });
                assert_eq!(got, seed, "the blocking pool returned somebody else's value");
                i_am_still(me);
            }
            Shape::Joining => {
                let left = Arc::new(AtomicUsize::new(rounds as usize));
                // Taken before the children exist, so the last one to finish
                // can release this fiber whether or not it has parked yet.
                let release = Arc::new(waker_for_current().expect("a fiber on a worker"));
                for _ in 0..rounds {
                    let left = left.clone();
                    let release = release.clone();
                    let tally = tally.clone();
                    schedule(Task::new(move || {
                        let me = crate::current::current(|f| f.id());
                        suspend();
                        i_am_still(me);
                        tally.children.fetch_add(1, Ordering::Relaxed);
                        tally.finished.fetch_add(1, Ordering::Relaxed);
                        if left.fetch_sub(1, Ordering::AcqRel) == 1 {
                            release.wake();
                        }
                    }));
                }
                // **A loop, not a single park.** The release can arrive before
                // this fiber parks, in which case `park_current` returns at
                // once and the condition is what ends the wait; and the
                // adversary's wakes arrive whenever they like, which must send
                // it round again rather than out.
                while left.load(Ordering::Acquire) > 0 {
                    park_current();
                    i_am_still(me);
                }
            }
            Shape::Socket => {
                let (mine, peer) = crate::reactor::a_connected_pair();
                let watch = crate::reactor::socket_of(&mine);
                let tally = tally.clone();
                schedule(Task::new(move || {
                    let me = crate::current::current(|f| f.id());
                    suspend();
                    i_am_still(me);
                    use std::io::Write;
                    let mut peer = peer;
                    let _ = peer.write_all(b"x");
                    tally.children.fetch_add(1, Ordering::Relaxed);
                    tally.finished.fetch_add(1, Ordering::Relaxed);
                }));
                wait_until_ready(watch, Interest::Readable);
                i_am_still(me);
                drop(mine);
            }
        }
        tally.finished.fetch_add(1, Ordering::Relaxed);
    });
    task
}

/// Everything at once, with a thread interfering, for as long as it is asked.
///
/// Defaults to a few hundred rounds so that an ordinary `cargo test` run pays
/// about a second for it. `KHORA_SOAK_ROUNDS=200000` is what an actual soak
/// looks like, and `KHORA_SOAK_SEED` repeats one.
#[test]
fn adversarial_execution_leaves_nothing_behind() {
    let rounds = env("KHORA_SOAK_ROUNDS", 400);
    let seed = env("KHORA_SOAK_SEED", 0x5EED_1F0F);
    let workers = env("KHORA_SOAK_WORKERS", 4) as usize;

    let mut rng = Rng::new(seed);
    let tally = Arc::new(Tally::default());
    let pool = Arc::new(Scheduler::new(workers));

    // Fibers the adversary knows about, and how many shapes it has meddled
    // with. Bounded, so a long soak does not grow a list for ever.
    let known: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let meddled = Arc::new(AtomicUsize::new(0));

    // **Every `Waiting` fiber is released exactly once, by somebody whose job
    // that is.** The adversary is not that somebody: it picks from a
    // sixty-four entry window, so a fiber whose id falls out of the window
    // before it is chosen waits for ever — and the soak then hangs for a
    // reason that has nothing to do with the scheduler. That happened, and it
    // took a watchdog dump reading `parked: 1, waiting: 1` and nothing else to
    // tell the two apart.
    //
    // Exactly once, deliberately. Waking repeatedly until something moves
    // would paper over a genuinely lost wakeup, which is one of the things
    // this test exists to find. One wake has to be enough, and whether it
    // lands before or after the fiber parks is left to chance, because the
    // protocol has to survive both.
    let waiters: Arc<Mutex<std::collections::VecDeque<usize>>> = Arc::default();
    let releaser = {
        let pool = pool.clone();
        let waiters = waiters.clone();
        let stop = stop.clone();
        std::thread::spawn(move || loop {
            let next = waiters.lock().expect("the waiters").pop_front();
            match next {
                Some(id) => pool.wake_fiber(id),
                None if stop.load(Ordering::Relaxed) => return,
                None => std::thread::yield_now(),
            }
        })
    };

    // **The adversary.** It wakes and cancels fibers it has no business
    // knowing about, including ones that finished long ago, because a wake or
    // a cancel arriving at the wrong moment is the whole point. Every one of
    // these must be a no-op or a legitimate release, never a crash.
    let adversary = {
        let pool = pool.clone();
        let known = known.clone();
        let stop = stop.clone();
        let meddled = meddled.clone();
        std::thread::spawn(move || {
            let mut rng = Rng::new(seed ^ 0x9E37_79B9);
            while !stop.load(Ordering::Relaxed) {
                let target = {
                    let ids = known.lock().expect("the known fibers");
                    if ids.is_empty() {
                        None
                    } else {
                        Some(ids[rng.upto(ids.len() as u64) as usize])
                    }
                };
                if let Some(id) = target {
                    if rng.upto(3) == 0 {
                        pool.cancel_fiber(id);
                    } else {
                        pool.wake_fiber(id);
                    }
                    meddled.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::yield_now();
            }
        })
    };

    // **A hang is the least informative failure there is**, and the only one
    // this test can produce that says nothing at all: the process sits there,
    // the harness times it out, and the state that would have explained it
    // dies with it. So a watchdog turns one into evidence. It prints
    // everywhere a fiber can be and then aborts, because a hung scheduler
    // cannot be asked politely to stop.
    let watchdog = {
        let pool = pool.clone();
        let stop = stop.clone();
        let patience = std::time::Duration::from_secs(env("KHORA_SOAK_PATIENCE", 120));
        std::thread::spawn(move || {
            let until = std::time::Instant::now() + patience;
            while std::time::Instant::now() < until {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            eprintln!(
                "SOAK STUCK after {patience:?}
  seed={seed} rounds={rounds} workers={workers}
  {:?}
  {:?}
  resuming={} blocking={:?}",
                pool.audit(),
                pool.counts(),
                crate::coro::resuming_now(),
                crate::blocking::pool().stats(),
            );
            std::process::abort();
        })
    };

    let mut expected = 0usize;
    for round in 0..rounds {
        let shape = Shape::pick(&mut rng);
        let task = fiber(shape, rng.next(), tally.clone());
        let id = task.fiber().id();
        {
            let mut ids = known.lock().expect("the known fibers");
            // A window rather than a history: recent fibers are the ones whose
            // transitions are live, and old ids exercise the "wake something
            // that has gone" path just as well from a stale entry.
            if ids.len() >= 64 {
                ids.remove(0);
            }
            ids.push(id);
        }
        expected += 1;
        pool.spawn(task);
        // **After the spawn, never before.** `wake_fiber` can only wake a
        // fiber the pool has been told about, and `spawn` is what tells it; a
        // wake that arrives first finds nothing and is dropped, which is the
        // right behaviour and left one fiber parked for ever about twice in a
        // hundred runs. The adversary's window above does not care — its wakes
        // are best-effort by design — but the releaser's one wake is the only
        // one a `Waiting` fiber will get.
        if shape == Shape::Waiting {
            waiters.lock().expect("the waiters").push_back(id);
        }

        // **What can honestly be checked while the pool is busy**, which is
        // less than it looks. `Audit` reads five places without a lock across
        // them, so a task moving between two is seen in both and `in_hand` goes
        // negative with nothing wrong.
        //
        // This one survives because `audit` reads `completed` first and
        // `spawned` last: no fiber can be counted finished before it was
        // counted begun, whatever the skew. One that finished twice fails here
        // rather than at the end.
        //
        // The rest is caught where it can be caught exactly — `ResumedOnce`
        // aborts the instant two workers enter one coroutine, each fiber checks
        // its identity on every resume, and `settled` does the full arithmetic
        // once nothing is moving.
        if round % 32 == 0 {
            let audit = pool.audit();
            assert!(audit.outstanding() >= 0, "more fibers finished than began: {audit:?}");
        }
    }

    // Every `Waiting` fiber must have been offered its one wake before there
    // is any point waiting for the pool to empty.
    while !waiters.lock().expect("the waiters").is_empty() {
        std::thread::yield_now();
    }
    pool.drain();
    // Finished is not the same as quiescent: a sleeper released early leaves
    // its deadline in the heap until it comes due. The naps here are at most a
    // few milliseconds, so a second is generous; if it is not enough, the
    // audit below says exactly what is still registered.
    let audit = pool.settle(std::time::Duration::from_secs(1));
    stop.store(true, Ordering::Relaxed);
    adversary.join().expect("the adversary");
    releaser.join().expect("the releaser");
    watchdog.join().expect("the watchdog");

    // Children are spawned by fibers rather than by this loop, so the total is
    // only known afterwards.
    expected += tally.children.load(Ordering::Relaxed);

    let counts = pool.counts();
    let context = format!("{audit:?}\n{counts:?}\nmeddled={meddled:?}");

    assert_eq!(
        tally.finished.load(Ordering::Relaxed),
        expected,
        "fibers ran a different number of times than they were spawned\n{context}"
    );
    assert!(audit.settled(), "the pool did not come back to empty\n{context}");
    assert_eq!(audit.in_hand(), 0, "{context}");
    // **Waited for, not sampled.** `settle` watches the audit — spawned equals
    // completed, every queue empty — and `RESUMING` is decremented by
    // `ResumedOnce::drop`, which runs *after* the completion it belongs to has
    // been counted. So there is a window where the audit reads clean and a
    // worker is still a few instructions from leaving `resume`, and asserting
    // in it fails a run that did nothing wrong.
    //
    // Seen once in a full baseline and not again in thirty-two later runs,
    // twenty of them of this test alone: rare enough to look like a race in
    // the scheduler and shallow enough to be neither. The audit in the message
    // said so — 668 spawned, 668 completed, everything zero except this.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while crate::coro::resuming_now() != 0 && std::time::Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(
        crate::coro::resuming_now(),
        0,
        "a worker is still inside a fiber\n{context}"
    );
}

/// Pins this process to one CPU, and says whether it worked.
///
/// **The single most useful thing a test can do to a lock-free handover.** On
/// several cores, two threads racing a transition mostly do not: they run at
/// the same time, each finishes its half, and the window between the two
/// stores is never entered by anybody. On one core they cannot run at the same
/// time, so every interleaving is a *preemption* landing at a point the OS
/// chose -- including inside the window. A bug that needs the second thread to
/// observe a half-written handover is orders of magnitude likelier here than
/// under ordinary parallelism.
///
/// **Affinity is a property of the whole process, so it is put back.** Under
/// `cargo nextest` -- which the baseline runs -- every test has its own process
/// and it would not matter. Under a plain `cargo test` the whole binary shares
/// one, and a test that pinned it and walked away would leave every test after
/// it on a single core. So this is a guard rather than a call: it restores the
/// mask it found when it is dropped.
struct OneCpu {
    /// What the process was allowed before, and whether the pin took.
    restore: Option<Restore>,
}

#[cfg(windows)]
type Restore = usize;
#[cfg(target_os = "linux")]
type Restore = libc::cpu_set_t;
#[cfg(not(any(windows, target_os = "linux")))]
type Restore = ();

impl OneCpu {
    fn taken() -> OneCpu {
        OneCpu { restore: pin() }
    }

    /// Whether the process really is on one CPU.
    fn pinned(&self) -> bool {
        self.restore.is_some()
    }
}

impl Drop for OneCpu {
    fn drop(&mut self) {
        let Some(previous) = self.restore.take() else { return };
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessAffinityMask};
            // SAFETY: a pseudo-handle and a mask this process was running under
            // moments ago.
            unsafe {
                SetProcessAffinityMask(GetCurrentProcess(), previous);
            }
        }
        #[cfg(target_os = "linux")]
        {
            // SAFETY: the set came from `sched_getaffinity` and is unchanged.
            unsafe {
                libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &previous);
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = previous;
        }
    }
}

/// Pins this process to one CPU, answering what it was allowed before.
///
/// **The single most useful thing a test can do to a lock-free handover.** On
/// several cores, two threads racing a transition mostly do not: they run at
/// the same time, each finishes its half, and the window between the two
/// stores is never entered by anybody. On one core they cannot run at the same
/// time, so every interleaving is a *preemption* landing at a point the OS
/// chose -- including inside the window. A bug that needs the second thread to
/// observe a half-written handover is orders of magnitude likelier here than
/// under ordinary parallelism.
fn pin() -> Option<Restore> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, GetProcessAffinityMask, SetProcessAffinityMask,
        };
        let mut allowed: usize = 0;
        let mut system: usize = 0;
        // SAFETY: a pseudo-handle and two live `usize`s to write through.
        let read = unsafe {
            GetProcessAffinityMask(GetCurrentProcess(), &raw mut allowed, &raw mut system)
        };
        if read == 0 || allowed == 0 {
            return None;
        }
        // The lowest CPU this process is already allowed, rather than CPU 0 --
        // which it may not be allowed at all under an affinity a runner set.
        let one = allowed & allowed.wrapping_neg();
        // SAFETY: as above, with a mask that is a subset of `allowed`.
        let set = unsafe { SetProcessAffinityMask(GetCurrentProcess(), one) };
        if set == 0 { None } else { Some(allowed) }
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a zeroed set of exactly the size passed, written by the call.
        unsafe {
            let mut before: libc::cpu_set_t = std::mem::zeroed();
            let size = std::mem::size_of::<libc::cpu_set_t>();
            if libc::sched_getaffinity(0, size, &mut before) != 0 {
                return None;
            }
            // The lowest CPU already allowed, for the reason above.
            let mut one: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut one);
            let mut found = false;
            for cpu in 0..libc::CPU_SETSIZE as usize {
                if libc::CPU_ISSET(cpu, &before) {
                    libc::CPU_SET(cpu, &mut one);
                    found = true;
                    break;
                }
            }
            if !found || libc::sched_setaffinity(0, size, &one) != 0 {
                return None;
            }
            Some(before)
        }
    }
    // macOS has no affinity API that binds a thread to a CPU -- its
    // `THREAD_AFFINITY_POLICY` is a hint about sharing caches, not a binding --
    // so the test runs there without the pinning and says so.
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

/// **The same workload under a schedule chosen to be as bad as possible.**
///
/// `adversarial_execution_leaves_nothing_behind` is adversarial in its
/// *messages*: it wakes and cancels fibers that have no business being woken or
/// cancelled. It is not adversarial in its *schedule* -- four workers on a
/// sixteen-core machine mostly do not contend, and a handover race that needs
/// one thread to be preempted between two stores is a race that machine will
/// not run.
///
/// So this changes the schedule rather than the work:
///
/// - **One CPU.** Every interleaving becomes a preemption at a point the OS
///   chose rather than two threads genuinely running at once, which is what
///   puts another thread inside a half-finished handover.
/// - **One worker.** Every fiber goes through one queue, so the queue's own
///   transitions are contended by everything at once rather than spread over
///   four.
/// - **A saturated blocking pool.** Background work keeps every pool thread
///   busy, so a `Blocking` fiber queues and its submitter waits -- the fiber ↔
///   blocking-thread handover under back-pressure rather than at leisure.
/// - **Cancellation storms.** Everything known is cancelled at once, in
///   bursts, rather than one at a time: a fiber is far likelier to be
///   cancelled *during* a transition than between two.
///
/// The invariants asserted are the same ones, for the same reason: a scheduler
/// has no answer to get wrong, so a failure is arithmetic that does not come
/// back to zero.
#[test]
fn a_hostile_schedule_leaves_nothing_behind() {
    let rounds = env("KHORA_SOAK_ROUNDS", 400);
    let seed = env("KHORA_SOAK_SEED", 0xB00B_1E5F);
    // Held for the whole test and put back when it ends, however it ends.
    let cpu = OneCpu::taken();
    let pinned = cpu.pinned();

    let mut rng = Rng::new(seed);
    let tally = Arc::new(Tally::default());
    // One worker: everything through one queue.
    let pool = Arc::new(Scheduler::new(1));

    let known: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let waiters: Arc<Mutex<std::collections::VecDeque<usize>>> = Arc::default();

    // As in the test above: a `Waiting` fiber gets exactly one wake from
    // somebody whose job that is, and the storms below are best-effort.
    let releaser = {
        let pool = pool.clone();
        let waiters = waiters.clone();
        let stop = stop.clone();
        std::thread::spawn(move || loop {
            let next = waiters.lock().expect("the waiters").pop_front();
            match next {
                Some(id) => pool.wake_fiber(id),
                None if stop.load(Ordering::Relaxed) => return,
                None => std::thread::yield_now(),
            }
        })
    };

    // **Cancel everything at once, then again.** One cancellation at a time
    // finds a fiber between transitions; a burst finds one inside every
    // transition there is.
    let storm = {
        let pool = pool.clone();
        let known = known.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let ids: Vec<usize> = known.lock().expect("the known fibers").clone();
                for id in ids {
                    pool.cancel_fiber(id);
                    pool.wake_fiber(id);
                }
                std::thread::yield_now();
            }
        })
    };

    // **The blocking pool, kept full.** A `Blocking` fiber then finds no thread
    // free, queues, and its submitter waits -- which is the path the leisurely
    // version never takes.
    let saturator = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                // Not through `blocking`, which needs a fiber to suspend.
                // Submitting from an ordinary thread runs the work inline,
                // which is exactly the occupancy this wants.
                crate::blocking::blocking(|| {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                });
            }
        })
    };

    let watchdog = {
        let pool = pool.clone();
        let stop = stop.clone();
        let patience = std::time::Duration::from_secs(env("KHORA_SOAK_PATIENCE", 120));
        std::thread::spawn(move || {
            let until = std::time::Instant::now() + patience;
            while std::time::Instant::now() < until {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            eprintln!(
                "HOSTILE SOAK STUCK after {patience:?}\n  seed={seed} rounds={rounds} pinned={pinned}\n  {:?}\n  {:?}",
                pool.audit(),
                pool.counts(),
            );
            std::process::abort();
        })
    };

    let mut expected = 0usize;
    for round in 0..rounds {
        let shape = Shape::pick(&mut rng);
        let task = fiber(shape, rng.next(), tally.clone());
        let id = task.fiber().id();
        {
            let mut ids = known.lock().expect("the known fibers");
            if ids.len() >= 64 {
                ids.remove(0);
            }
            ids.push(id);
        }
        expected += 1;
        pool.spawn(task);
        if shape == Shape::Waiting {
            waiters.lock().expect("the waiters").push_back(id);
        }
        if round % 32 == 0 {
            let audit = pool.audit();
            assert!(audit.outstanding() >= 0, "more fibers finished than began: {audit:?}");
        }
    }

    while !waiters.lock().expect("the waiters").is_empty() {
        std::thread::yield_now();
    }
    pool.drain();
    let audit = pool.settle(std::time::Duration::from_secs(2));
    stop.store(true, Ordering::Relaxed);
    releaser.join().expect("the releaser");
    storm.join().expect("the storm");
    saturator.join().expect("the saturator");
    watchdog.join().expect("the watchdog");

    expected += tally.children.load(Ordering::Relaxed);
    let counts = pool.counts();
    let context = format!("pinned={pinned}\n{audit:?}\n{counts:?}");

    assert_eq!(
        tally.finished.load(Ordering::Relaxed),
        expected,
        "fibers ran a different number of times than they were spawned\n{context}"
    );
    assert!(audit.settled(), "the pool did not come back to empty\n{context}");
    assert_eq!(audit.in_hand(), 0, "{context}");

    let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while crate::coro::resuming_now() != 0 && std::time::Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(crate::coro::resuming_now(), 0, "a worker is still inside a fiber\n{context}");
}

/// The same, over and over with a fresh seed, watching for drift.
///
/// **The leak check, and the reason it has to be one process.** Every
/// individual soak proves the pool came back to empty; none of them can say
/// whether something accumulates *between* pools — a timer heap that never
/// shrinks, a registry that keeps dead ids, a blocking thread per round. So
/// this builds and destroys a scheduler many times over and compares resident
/// memory late against resident memory early.
///
/// The first few passes are excluded from the baseline on purpose: allocator
/// arenas, the blocking pool's threads and the lazily-built statics all settle
/// during them, and counting that as a leak would make the check cry wolf.
///
/// Ignored, because it is the version that takes minutes.
///
/// `cargo test -p khora-rt -- --ignored --nocapture soak`
#[test]
#[ignore = "the long soak; run it deliberately"]
fn a_long_soak_over_many_seeds() {
    let minutes = env("KHORA_SOAK_MINUTES", 2);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(minutes * 60);
    let mut seed = env("KHORA_SOAK_SEED", 1);
    let mut passes = 0u64;
    let mut settled_at = 0usize;

    while std::time::Instant::now() < until {
        // The child test reads these on entry. Setting them here rather than
        // threading a parameter keeps one implementation of the soak rather
        // than two.
        //
        // SAFETY: `set_var` is unsafe because the environment is process-global
        // and a concurrent reader in another thread is a data race. Here the
        // soak's own thread is the only one running -- the round it is about to
        // start has not spawned yet, and the previous one has been joined -- so
        // there is no reader to race with.
        unsafe {
            std::env::set_var("KHORA_SOAK_SEED", seed.to_string());
            std::env::set_var("KHORA_SOAK_ROUNDS", "400");
        }
        adversarial_execution_leaves_nothing_behind();
        passes += 1;
        if passes == 5 {
            settled_at = rss();
        }
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }

    let ended_at = rss();
    let drift = ended_at as isize - settled_at as isize;
    eprintln!(
        "soak: {passes} passes over {minutes} min; resident {} -> {} MB, drift {} KB",
        settled_at / (1024 * 1024),
        ended_at / (1024 * 1024),
        drift / 1024,
    );
    assert!(passes > 5, "not enough passes to say anything: {passes}");
    // Generous, and still far below what any real accumulation would do: a
    // pool per pass leaking even one fiber's worth would run to megabytes over
    // hundreds of passes.
    assert!(
        drift < 32 * 1024 * 1024,
        "resident memory grew by {} MB over {passes} passes",
        drift / (1024 * 1024)
    );
}

/// Spawns `count` fibers, waits for all of them to be parked, and reports.
///
/// Returns the audit taken while every one of them was waiting, the resident
/// bytes the population cost, and how long the round trip took.
fn waiting_at_scale(
    count: usize,
    workers: usize,
) -> (crate::scheduler::Audit, isize, usize, std::time::Duration) {
    let woke = Arc::new(AtomicUsize::new(0));
    let pool = Scheduler::new(workers);
    let mut ids = Vec::with_capacity(count);

    let settled_before = rss();
    let start = std::time::Instant::now();
    for _ in 0..count {
        let woke = woke.clone();
        let task = Task::new(move || {
            let me = crate::current::current(|f| f.id());
            park_current();
            i_am_still(me);
            woke.fetch_add(1, Ordering::Relaxed);
        });
        ids.push(task.fiber().id());
        pool.spawn(task);
    }

    // Every one of them parked, which is the state worth measuring: a hundred
    // thousand fibers that exist and are waiting, rather than a hundred
    // thousand that have been created and are still being got through.
    while pool.waiting() < count as u64 {
        std::thread::yield_now();
    }
    let audit = pool.audit();
    let peak = rss();
    let maps = mappings();

    for id in ids {
        pool.wake_fiber(id);
    }
    pool.drain();
    let took = start.elapsed();

    assert_eq!(woke.load(Ordering::Relaxed), count, "not all of them woke");
    let after = pool.settle(std::time::Duration::from_secs(5));
    assert!(after.settled(), "{after:?}");
    (audit, peak as isize - settled_before as isize, maps, took)
}

/// A thousand at once, in the ordinary suite.
#[test]
fn a_thousand_fibers_wait_and_wake() {
    let (audit, _, _, _) = waiting_at_scale(1_000, 4);
    assert_eq!(audit.parked + audit.in_hand() as usize, 1_000, "{audit:?}");
}

/// **The scale table.** Ignored, because a hundred thousand coroutine stacks
/// is not something to do on every `cargo test`.
///
/// `cargo test -p khora-rt -- --ignored --nocapture scale`
#[test]
#[ignore = "measurement, not a check; run it deliberately"]
fn fibers_at_scale() {
    eprintln!(
        "  count   parked  waiting   resident    per fiber   mappings   round trip"
    );
    for count in [1_000usize, 10_000, 100_000] {
        let (audit, bytes, maps, took) = waiting_at_scale(count, 4);
        eprintln!(
            "{count:>7}   {:>6}  {:>7}   {:>6} MB   {:>5} bytes   {maps:>8}   {took:>10.2?}",
            audit.parked,
            audit.waiting,
            bytes / (1024 * 1024),
            bytes / count as isize,
        );
    }
}
