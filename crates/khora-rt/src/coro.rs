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
    static YIELDER: Cell<*const Yielder<(), ()>> = const { Cell::new(std::ptr::null()) };
}

/// A fiber with a stack of its own.
pub(crate) struct Task {
    fiber: Arc<Fiber>,
    coroutine: Coroutine<(), (), ()>,
}

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
            // The yielder is reachable from `suspend` for as long as the body
            // runs, and unreachable outside it.
            let previous = YIELDER.with(|y| y.replace(yielder as *const _));
            body();
            YIELDER.with(|y| y.set(previous));
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
        let _entered = enter(self.fiber.clone());
        match self.coroutine.resume(()) {
            CoroutineResult::Yield(()) => Ran::Suspended,
            CoroutineResult::Return(()) => Ran::Finished,
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.coroutine.done()
    }
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
    let yielder = YIELDER.with(|y| y.get());
    if yielder.is_null() {
        return false;
    }
    // SAFETY: non-null only between entering a body and leaving it, and only
    // on the thread running it. `Task::with_fiber` sets it around exactly that
    // window and restores the previous value after.
    unsafe { (*yielder).suspend(()) };
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
