//! The context switch: a fiber that is not a thread.
//!
//! Phase 11A. This builds the mechanism and tests it in isolation; it is not
//! yet what `Fiber::spawn` uses, and that is deliberate. A single worker
//! running many fibers cooperatively deadlocks the moment one of them makes a
//! blocking read, so switching `spawn` over waits for the reactor in 11C.
//! Until then the two live side by side and every existing test keeps passing
//! against threads.
//!
//! # Why a dependency, when the rest of this repository writes its own
//!
//! `semver` is hand-written here, and so is the git plumbing, because getting
//! those wrong produces a wrong answer that a test can see. Getting a context
//! switch wrong produces memory corruption on a platform nobody ran.
//!
//! The specific thing that decided it is Windows. A switch there is not just
//! registers: the thread environment block carries `StackBase`, `StackLimit`
//! and `DeallocationStack`, and structured exception handling and the guard
//! page both read them. A switch that moves to another stack without updating
//! the TEB gets a stack overflow check against the wrong bounds — which
//! presents as corruption much later, somewhere else. Then the same again for
//! aarch64's link register and frame records, and CFI annotations on every
//! variant so a panic can unwind across a switch.
//!
//! That is three targets of assembly, of which this machine can execute one.
//! `corosensei` has all three, is by the author of `parking_lot` and
//! `hashbrown`, and exposes a `Stack` trait — so the slab strategy in
//! `docs/design/scheduler.md` remains ours to implement. Replacing it with our
//! own assembly is a later option, taken when there is something to measure
//! and three machines to measure it on, exactly as the same document says
//! about `io_uring`.
//!
//! # What a fiber is here
//!
//! A stack, a body, and the [`Fiber`] identity that follows it across
//! switches. Resuming installs that identity so `khora_cancelled` and
//! `Shared::update` answer about the fiber rather than about the worker
//! carrying it — which is what `crate::current` is for and why it landed
//! first.

// Nothing outside the tests below calls any of this yet, and that is 11A's
// whole shape: the mechanism is built and proven before anything depends on
// it, because switching `Fiber::spawn` over without a reactor deadlocks the
// first program that reads a socket. The attribute comes off in 11B, when a
// scheduler resumes these.
#![allow(dead_code)]

use std::cell::Cell;
use std::sync::Arc;

use corosensei::{Coroutine, CoroutineResult, Yielder};

use crate::current::{enter, Fiber};

/// What a resume did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ran {
    /// It suspended and can be resumed again.
    Suspended,
    /// It finished. Resuming again is a mistake the caller must not make.
    Finished,
}

thread_local! {
    /// The suspension point of the fiber running on this thread, if one is.
    ///
    /// Not on [`Fiber`], which is shared across threads and outlives any
    /// particular execution. A yielder belongs to the *running-ness*: it is
    /// valid only between entering a body and leaving it, and only on the
    /// thread doing the running.
    ///
    /// **It has to be re-installed on every resume, not once per body**, and
    /// getting that wrong is how this was first written. A body sets it when it
    /// starts; then it suspends, the worker runs somebody else who sets it to
    /// *their* yielder, and when the first fiber is resumed control returns
    /// inside its own `suspend` with the wrong pointer installed. The next
    /// suspension goes through another fiber's yielder, which is undefined and
    /// obligingly appears to work — every yielder switches back to the same
    /// worker — until enough fibers and workers are involved, at which point it
    /// is an access violation with no obvious author.
    ///
    /// So there are three writers, and all three are needed: the body on entry,
    /// [`suspend`] on the way back from a suspension, and [`Task::resume`]
    /// restoring whatever the worker had.
    static YIELDER: Cell<*const Yielder<(), ()>> = const { Cell::new(std::ptr::null()) };
}

/// A fiber with a stack of its own.
/// Reads the yielder installed on *this* thread, right now.
///
/// **`#[inline(never)]` is the whole point of this function.** A thread-local
/// is reached through a register-held base address, and the compiler is
/// entitled to compute that address once and reuse it â€” it has no reason to
/// think the thread could change underneath. Here it can: every call to
/// [`suspend`] may come back on a different worker. Inlined, the address
/// computed before a switch gets reused after it, and the access lands in the
/// TLS of the thread that *used* to be running this fiber.
///
/// That is not a theory. ThreadSanitizer named it exactly:
///
/// ```text
/// Read of size 8 at 0x7ffff43fd668 by thread T2:
/// Previous write of size 8 at 0x7ffff43fd668 by thread T5:
/// Location is TLS of thread T2.
/// ```
///
/// The consequence is worse than a lost write. The thread that should have
/// been given the yielder never gets it, so it keeps a stale one, and the next
/// suspension switches to a stack pointer belonging to some other worker â€”
/// which is a `SIGSEGV` a quarter of the time, on another thread, much later.
///
/// Not inlining puts the address computation inside the callee, where it runs
/// after the switch and on the thread that is actually executing. The inline
/// assembly in the switch itself does not help: it clobbers memory, and a
/// cached TLS address is a register.
#[inline(never)]
fn installed() -> *const Yielder<(), ()> {
    YIELDER.with(|y| y.get())
}

/// Installs `yielder` on *this* thread. See [`installed`] for why it is not
/// inlined; the same reasoning applies, and this is the direction that
/// corrupts another thread rather than merely misreading its own.
#[inline(never)]
fn install(yielder: *const Yielder<(), ()>) {
    YIELDER.with(|y| y.set(yielder));
}

/// Set while a `Task` is inside `resume`, in debug builds only.
///
/// **Two workers resuming one coroutine is the failure this cannot detect
/// after the fact.** Both switch to the same stack, and what comes out is a
/// crash somewhere unrelated, minutes later, with nothing left to say what
/// happened. So it is checked at the door instead. 11F's soak runs in debug,
/// which is where this is on; release builds have neither the flag nor the
/// branch.
#[cfg(debug_assertions)]
static RESUMING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) struct Task {
    fiber: Arc<Fiber>,
    coroutine: Coroutine<(), (), ()>,
}

// SAFETY: a suspended coroutine is a stack, and whether it may cross to another
// thread depends on what is *on* that stack — which no Rust type system can
// see, which is why `corosensei` declines to implement this and documents
// exactly this escape hatch for callers who can reason about their own bodies.
//
// Khora bodies can be reasoned about, and the language already did the work:
//
//   - Every value on a Khora stack is a Khora value, reference-counted. When a
//     program can spawn at all, those counts are atomic — `SINGLE_THREADED` is
//     the flag, and `khora_fiber_spawn` aborts a program that spawns after the
//     compiler emitted non-atomic counting on the strength of it never doing
//     so. So moving a stack between workers cannot race a count.
//   - What may cross *into* a fiber is `Share`, checked by the type checker and
//     restricted to the module declaring the type — `docs/design/sharing.md`.
//     A fiber's stack therefore holds only what was already allowed to be there.
//   - Foreign code is the exception, and it is handled by policy rather than by
//     types: `docs/design/scheduler.md` §8 says a fiber may not suspend inside
//     an `extern` call, so no C library's thread-affine state is ever live
//     across a migration.
//
// **The obligation this leaves on a Rust body**, which the tests in this crate
// are: anything held across a `suspend()` must be `Send`. Captures are already
// checked, because `Task::new` requires a `Send` closure; a local created
// inside the body and held across a suspension is not, and would be unsound.
unsafe impl Send for Task {}

impl Task {
    /// Builds a fiber that will run `body` when it is first resumed.
    ///
    /// Nothing runs here. A fiber that is never resumed costs its stack and no
    /// instructions, which is the property a hundred thousand of them depend
    /// on.
    pub(crate) fn new(body: impl FnOnce() + Send + 'static) -> Task {
        Task::with_fiber(Fiber::spawned(), body)
    }

    /// The same, for a fiber whose identity somebody else already holds —
    /// a parent that needs to be able to cancel it.
    pub(crate) fn with_fiber(fiber: Arc<Fiber>, body: impl FnOnce() + Send + 'static) -> Task {
        let coroutine = Coroutine::new(move |yielder: &Yielder<(), ()>, ()| {
            // Entry. `suspend` re-installs this after every wake, and
            // `Task::resume` puts the worker's back when this turn ends.
            install(yielder as *const _);
            body();
            // Nothing is running on a fiber stack once the body returns, and
            // leaving a dangling yielder installed is how the next thing to
            // call `suspend` on this worker reaches a stack that is gone.
            install(std::ptr::null());
        });
        Task { fiber, coroutine }
    }

    /// The identity a parent cancels through.
    pub(crate) fn fiber(&self) -> &Arc<Fiber> {
        &self.fiber
    }

    /// Runs the fiber until it suspends or finishes.
    ///
    /// Installing the identity around the resume is the whole integration with
    /// [`crate::current`]: inside, `khora_cancelled` reads this fiber's flag,
    /// and `Shared::update` sees this fiber's id, whichever worker is carrying
    /// it. The guard puts the previous one back on the way out, including if
    /// the body panics.
    pub(crate) fn resume(&mut self) -> Ran {
        #[cfg(debug_assertions)]
        let _once = ResumedOnce::claim(&self.fiber);
        let _entered = enter(self.fiber.clone());
        // Whatever was installed belongs to whoever is resuming us — a worker
        // with nothing, or an outer fiber if these ever nest. Either way it is
        // theirs again the moment this turn ends.
        let outer = installed();
        let ran = match self.coroutine.resume(()) {
            CoroutineResult::Yield(()) => Ran::Suspended,
            CoroutineResult::Return(()) => Ran::Finished,
        };
        install(outer);
        ran
    }

    pub(crate) fn finished(&self) -> bool {
        self.coroutine.done()
    }
}

/// Whether the caller is running on a fiber stack at all.
///
/// The blocking pool asks before it does anything clever: a caller with no
/// fiber has no worker to give back, so handing its work to another thread and
/// waiting would be strictly worse than doing the work here.
pub(crate) fn on_a_fiber() -> bool {
    !installed().is_null()
}

/// Asserts that nobody else is resuming this fiber, for as long as it lives.
///
/// A guard rather than a pair of calls, so that a panic inside the fiber
/// releases the claim rather than making every later resume look like a
/// duplicate.
#[cfg(debug_assertions)]
struct ResumedOnce<'a>(&'a Fiber);

#[cfg(debug_assertions)]
impl<'a> ResumedOnce<'a> {
    fn claim(fiber: &'a Arc<Fiber>) -> ResumedOnce<'a> {
        let already = fiber.resuming.swap(true, std::sync::atomic::Ordering::AcqRel);
        assert!(
            !already,
            "fiber {} resumed by two workers at once",
            fiber.id()
        );
        RESUMING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ResumedOnce(fiber)
    }
}

#[cfg(debug_assertions)]
impl Drop for ResumedOnce<'_> {
    fn drop(&mut self) {
        self.0.resuming.store(false, std::sync::atomic::Ordering::Release);
        RESUMING.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How many fibers are inside `resume` right now. Debug builds only.
#[cfg(debug_assertions)]
pub(crate) fn resuming_now() -> usize {
    RESUMING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Gives the worker back to whoever resumed this fiber.
///
/// Returns `false` when nothing is running on a fiber stack — the program's own
/// computation, or a thread that spawned no coroutine — because there is
/// nowhere to yield *to* and pretending otherwise would abort a correct
/// program.
///
/// **This is a safepoint, not a cancellation point.** It cannot fail, nothing
/// unwinds through it, and a fiber that yields is not thereby cancellable.
/// `docs/design/scheduler.md` §1 is about why those have to be different
/// things: a cancellation is observed at `!` in something that can raise, so an
/// infallible loop has no cancellation point and would otherwise own a worker
/// until the process ended.
pub(crate) fn suspend() -> bool {
    let yielder = installed();
    if yielder.is_null() {
        return false;
    }
    // SAFETY: non-null only between entering a body and leaving it, and only
    // on the thread running it. `Task::with_fiber` sets it on entry and clears
    // it on exit, and the line below keeps it right across a wake.
    unsafe { (*yielder).suspend(()) };

    // Resumed, and possibly not where we left off: this may be a different
    // worker entirely. Two things have to be put right. Somebody else ran on
    // whichever worker this is and installed their own yielder, so this
    // fiber's has to go back before it can suspend again — and the write has
    // to reach *this* thread's slot, which is why it goes through `install`
    // rather than touching `YIELDER` here. See `installed` for what happens
    // when it does not.
    install(yielder);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// **Four hundred coroutines, four threads, and every one of them moves.**
    ///
    /// This exists to answer one question, because a crash in `scheduler`
    /// hinges on it: is corosensei safe when a coroutine is resumed by a
    /// thread other than the one that resumed it last? Its `Yielder` is
    /// documented as a parent link updated on every resume, and its docs warn
    /// that *references* to a yielder must not cross threads — which is not
    /// the same claim, and the difference matters to every fiber this
    /// scheduler migrates.
    ///
    /// So: no scheduler, no wait states, no timers. A queue, four threads
    /// pulling from it, and the same install-on-entry, re-install-after-wake
    /// dance `suspend` does. If this ever fails, the bug is underneath us. It
    /// does not fail, which is why `scheduler.md` can say the Linux crash is
    /// ours.
    #[test]
    fn a_coroutine_survives_being_resumed_by_a_different_thread() {
        const COUNT: usize = 400;
        const HOPS: usize = 4;
        const WORKERS: usize = 4;

        struct Migrating(Coroutine<(), (), (), corosensei::stack::DefaultStack>);
        // SAFETY: the same argument as `Task`'s, and the point of the test.
        unsafe impl Send for Migrating {}

        let done = Arc::new(AtomicUsize::new(0));
        let queue: Arc<Mutex<Vec<Migrating>>> = Arc::new(Mutex::new(Vec::new()));

        for _ in 0..COUNT {
            let counter = done.clone();
            queue.lock().expect("the queue").push(Migrating(Coroutine::new(
                move |yielder: &Yielder<(), ()>, ()| {
                    install(yielder as *const _);
                    for _ in 0..HOPS {
                        let mine = installed();
                        // SAFETY: ours, and we are the thread running it.
                        unsafe { (*mine).suspend(()) };
                        install(mine);
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    install(std::ptr::null());
                },
            )));
        }

        let mut workers = Vec::new();
        for _ in 0..WORKERS {
            let queue = queue.clone();
            let done = done.clone();
            workers.push(std::thread::spawn(move || {
                while done.load(Ordering::SeqCst) != COUNT {
                    let Some(mut co) = queue.lock().expect("the queue").pop() else {
                        std::thread::yield_now();
                        continue;
                    };
                    let outer = installed();
                    let finished = matches!(co.0.resume(()), CoroutineResult::Return(()));
                    install(outer);
                    if !finished {
                        queue.lock().expect("the queue").push(co);
                    }
                }
            }));
        }
        for worker in workers {
            worker.join().expect("a worker");
        }
        assert_eq!(done.load(Ordering::SeqCst), COUNT);
    }

    #[test]
    fn a_task_runs_when_it_is_resumed_and_not_before() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = ran.clone();
        let mut task = Task::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(ran.load(Ordering::SeqCst), 0, "building one runs nothing");
        assert_eq!(task.resume(), Ran::Finished);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(task.finished());
    }

    #[test]
    fn a_task_stops_where_it_suspended_and_carries_on_from_there() {
        let steps = Arc::new(AtomicUsize::new(0));
        let counter = steps.clone();
        let mut task = Task::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            suspend();
            counter.fetch_add(1, Ordering::SeqCst);
            suspend();
            counter.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(task.resume(), Ran::Suspended);
        assert_eq!(steps.load(Ordering::SeqCst), 1);
        assert_eq!(task.resume(), Ran::Suspended);
        assert_eq!(steps.load(Ordering::SeqCst), 2);
        assert_eq!(task.resume(), Ran::Finished);
        assert_eq!(steps.load(Ordering::SeqCst), 3);
    }

    /// Suspension works from arbitrary depth, which is the whole difference
    /// between a stackful coroutine and a state machine: the compiler does not
    /// have to know a call can suspend.
    #[test]
    fn a_task_suspends_from_the_bottom_of_a_call_chain() {
        fn deep(n: usize) {
            if n == 0 {
                suspend();
            } else {
                deep(n - 1);
            }
        }
        let mut task = Task::new(|| deep(64));
        assert_eq!(task.resume(), Ran::Suspended);
        assert_eq!(task.resume(), Ran::Finished);
    }

    /// One worker, many fibers, interleaved. This is what a scheduler will do,
    /// with a queue instead of a loop.
    #[test]
    fn one_thread_interleaves_many_tasks() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut tasks: Vec<Task> = (0..3)
            .map(|id| {
                let log = order.clone();
                Task::new(move || {
                    for step in 0..3 {
                        log.lock().unwrap().push((id, step));
                        suspend();
                    }
                })
            })
            .collect();

        for _ in 0..3 {
            for task in &mut tasks {
                task.resume();
            }
        }

        let seen = order.lock().unwrap().clone();
        assert_eq!(seen.len(), 9, "{seen:?}");
        // Round-robin: every fiber takes one step before any takes a second.
        assert_eq!(&seen[..3], &[(0, 0), (1, 0), (2, 0)], "{seen:?}");
        assert_eq!(&seen[3..6], &[(0, 1), (1, 1), (2, 1)], "{seen:?}");
    }

    /// The reason `current` landed first. A fiber's identity has to follow it
    /// onto whichever stack is running, or `Shared::update` and
    /// `khora_cancelled` answer about the worker.
    #[test]
    fn the_running_fiber_is_the_task_being_resumed() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut tasks: Vec<Task> = (0..2)
            .map(|_| {
                let log = seen.clone();
                Task::new(move || {
                    log.lock().unwrap().push(crate::current::current(|f| f.id()));
                    suspend();
                    log.lock().unwrap().push(crate::current::current(|f| f.id()));
                })
            })
            .collect();

        let expected: Vec<usize> = tasks.iter().map(|t| t.fiber().id()).collect();
        for _ in 0..2 {
            for task in &mut tasks {
                task.resume();
            }
        }

        let ids = seen.lock().unwrap().clone();
        assert_eq!(ids, [expected[0], expected[1], expected[0], expected[1]], "{ids:?}");
    }

    /// And the worker gets its own identity back, so anything running between
    /// resumes is not mistaken for the fiber.
    #[test]
    fn the_worker_is_itself_again_between_resumes() {
        let worker = crate::current::current(|f| f.id());
        let mut task = Task::new(|| {
            suspend();
        });
        task.resume();
        assert_eq!(crate::current::current(|f| f.id()), worker);
        assert_ne!(task.fiber().id(), worker);
    }

    /// A cancellation set from outside is visible to the fiber when it next
    /// runs — the property a scheduler needs in order to wake a suspended
    /// fiber so it can observe one.
    #[test]
    fn a_cancellation_set_between_resumes_is_seen_by_the_fiber() {
        let saw = Arc::new(AtomicUsize::new(0));
        let flag = saw.clone();
        let mut task = Task::new(move || {
            suspend();
            if crate::current::current(|f| f.is_cancelled()) {
                flag.store(1, Ordering::SeqCst);
            }
        });

        task.resume();
        task.fiber().cancel();
        task.resume();

        assert_eq!(saw.load(Ordering::SeqCst), 1, "the fiber should have seen it");
    }

    /// **The regression test for the bug this module shipped first.**
    ///
    /// The yielder was installed once when a body started. Two fibers taking
    /// turns on one worker meant the second overwrote it, and when the first
    /// was resumed it suspended through the second's yielder — undefined, and
    /// it appears to work, because every yielder switches back to the same
    /// worker. It only became an access violation at five hundred fibers
    /// across four workers.
    ///
    /// What makes this deterministic is asserting on a value that lives *on
    /// the fiber's own stack* across a suspension. Suspending through somebody
    /// else's yielder puts the wrong stack back, and a local read after the
    /// wake is no longer the one written before it.
    #[test]
    fn each_fiber_comes_back_to_its_own_stack() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut tasks: Vec<Task> = (1..=3)
            .map(|mark: u64| {
                let log = seen.clone();
                Task::new(move || {
                    // On this fiber's stack, and nowhere else.
                    let mine = [mark; 16];
                    for _ in 0..4 {
                        suspend();
                        log.lock().unwrap().push(mine[15]);
                    }
                })
            })
            .collect();

        for _ in 0..5 {
            for task in &mut tasks {
                if !task.finished() {
                    task.resume();
                }
            }
        }

        let marks = seen.lock().unwrap().clone();
        assert_eq!(marks.len(), 12, "{marks:?}");
        for round in marks.chunks(3) {
            assert_eq!(round, [1, 2, 3], "a fiber woke on the wrong stack: {marks:?}");
        }
    }

    /// Nothing is running on a fiber stack here, so there is nowhere to yield
    /// to. Saying so beats aborting a correct program.
    #[test]
    fn suspending_outside_a_fiber_answers_rather_than_aborts() {
        assert!(!suspend());
    }

    #[test]
    fn a_task_that_never_suspends_just_finishes() {
        let mut task = Task::new(|| {});
        assert_eq!(task.resume(), Ran::Finished);
    }

    /// Ten thousand fibers, all suspended, on one thread.
    ///
    /// A tenth of the phase's target, kept small enough to belong in the
    /// ordinary suite. The full hundred thousand was measured separately and
    /// the number is in `docs/design/scheduler.md`: about 4 KB of resident
    /// memory each, against roughly 33 KB for a thread.
    ///
    /// What this actually guards is the thing that would regress silently. If
    /// stacks stop being committed lazily — a change to the allocator, an
    /// eagerly touched page — this still passes and the memory goes up eight
    /// times, so the assertion is deliberately about *reaching* the count with
    /// every fiber live at once.
    #[test]
    fn ten_thousand_fibers_live_at_once_on_one_thread() {
        const COUNT: usize = 10_000;
        let woken = Arc::new(AtomicUsize::new(0));

        let mut tasks: Vec<Task> = (0..COUNT)
            .map(|_| {
                let counter = woken.clone();
                Task::new(move || {
                    suspend();
                    counter.fetch_add(1, Ordering::Relaxed);
                })
            })
            .collect();

        for task in &mut tasks {
            assert_eq!(task.resume(), Ran::Suspended);
        }
        assert_eq!(tasks.len(), COUNT, "all of them are still alive and suspended");
        assert_eq!(woken.load(Ordering::Relaxed), 0);

        for task in &mut tasks {
            assert_eq!(task.resume(), Ran::Finished);
        }
        assert_eq!(woken.load(Ordering::Relaxed), COUNT);
    }
}
