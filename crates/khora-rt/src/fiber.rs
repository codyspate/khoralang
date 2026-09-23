//! Fibers.
//!
//! **A fiber is an operating-system thread, or a coroutine on the scheduler.**
//! `docs/design/fibers.md` decided a fiber *is* the second and that the first
//! would do until the scheduler existed, on the argument that a program cannot
//! tell which it has. This file is where the two meet, and
//! `KHORA_FIBERS=scheduler` picks the coroutine.
//!
//! **Threads are the default, and it is decided rather than pending.** The
//! argument, the numbers and the date are in `docs/design/fibers.md` under
//! "Which one 0.1.0 ships". The short version: threads are ahead at the
//! connection counts a service actually runs at, and the scheduler's
//! compensating benefit -- fiber density -- is measured on Windows and open on
//! Linux.
//!
//! **No numbers here.** There were, and the pair sat in this comment for
//! months while the scheduler got meaningfully faster and nothing updated it.
//! A measurement pasted into a comment is a measurement with no owner.
//!
//! **One thing a program can tell, on the scheduler.** A thread gets the
//! operating system's stack — two megabytes on Linux, one on Windows — and a
//! coroutine gets `corosensei`'s megabyte with a guard page. Recursion that was
//! near the old limit is over the new one, and the failure is a clean fault at
//! the guard page rather than corruption.

use super::*;
use crate::coro::Task;
use crate::current::{enter, Fiber, Stop};
use crate::scheduler::{park_current, Scheduler};
use crate::heap::SINGLE_THREADED;
use crate::heap::{khora_alloc, khora_drop};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};

/// A Khora pointer being moved to another fiber.
///
/// Raw pointers are not `Send`, and for good reason; this asserts that *these*
/// ones are safe to move, which they are because reference counts are atomic
/// (D10) and a spawned closure is handed over rather than shared — the caller
/// gives up its reference at the `spawn`.
pub(crate) struct Handed(pub(crate) *mut u8);

// SAFETY: see the type's documentation. The pointer is a Khora object with an
// atomic refcount, and exactly one fiber owns the reference being moved.
unsafe impl Send for Handed {}

/// What a fallible Khora function returns: `{ i32 which, i64 payload }`.
///
/// `which` is 0 for an ordinary return and otherwise the error's type id, with
/// [`CANCELLED_WHICH`] reserved for a cancellation. The layout is the code
/// generator's — see `docs/design/effect-runtime.md` §2 — and `repr(C)` is what
/// makes both sides agree about it.
#[repr(C)]
pub struct Tagged {
    /// 0 for an ordinary return; an error type's id, or one of the two
    /// reserved values, otherwise.
    pub which: u32,
    /// The error, as the one word every Khora value fits in.
    pub payload: u64,
}

/// The `which` a failed assertion travels under.
///
/// Beside the cancellation and outside the range error-type ids are assigned
/// from, for the same reason: no `catch` can name it, because `assert` is the
/// only thing that produces one and a test is the only thing that catches one.
pub const FAILED_WHICH: u32 = u32::MAX - 1;

/// The `which` a cancellation travels under.
///
/// Outside the range error-type ids are assigned from — they start at 1 and
/// count up — so no `catch` can name it and none will match it by accident.
/// The code generator's constant is defined *from* this one rather than beside
/// it, because two numbers that must agree are one number.
pub const CANCELLED_WHICH: u32 = u32::MAX;

/// The `which` [`khora_fiber_outcome`] reports a *stopped child* under.
///
/// **The failure a third number prevents: an asker swallowing its own
/// cancellation.** One tagged return carries two cancellations that want
/// opposite handling. The child's stop is what `Fiber::outcome` exists to hand
/// back as a value; the *asker's* — delivered while it was parked waiting —
/// must unwind it, because `docs/design/effect-runtime.md` §6 forbids any
/// construct from swallowing a cancellation aimed at the frame it is in.
/// Spelling both `CANCELLED_WHICH` would make the second indistinguishable
/// from the first, and the asker would carry on holding a cancellation it had
/// been told about and discarded.
///
/// Reserved beside [`FAILED_WHICH`] and [`CANCELLED_WHICH`] and outside the
/// range error-type ids are assigned from, so no `catch` can name it. Only
/// [`khora_fiber_outcome`] produces one: no other call on this boundary has
/// two cancellations to tell apart, and widening the meaning of a tag that
/// every reader already interprets is how a reserved range stops being one.
pub const STOPPED_WHICH: u32 = u32::MAX - 2;

/// Whether fibers are coroutines on the scheduler rather than threads.
///
/// Read once. A program that changed its mind halfway would have handles of
/// both kinds and no way to tell them apart.
pub(crate) fn on_the_scheduler() -> bool {
    static CHOSEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CHOSEN.get_or_init(|| {
        std::env::var("KHORA_FIBERS").map(|v| v == "scheduler").unwrap_or(false)
    })
}

/// How a fiber finishes, which is where the two implementations differ.
pub(crate) enum Completion {
    /// A thread to join. `None` once joined: joining twice is not an error,
    /// because the handle's release joins whatever `join` did not.
    ///
    /// Behind a lock because `Fiber` is `Share`: two fibers may hold one handle
    /// and both call `join`, and "take the handle if it is there" is the
    /// read-modify-write that has to happen once.
    Thread(Mutex<Option<std::thread::JoinHandle<()>>>, Arc<Done>),
    /// A latch the child closes.
    Fiber(Arc<Done>),
}

impl Completion {
    /// Waits for the fiber to finish, and gives up when this fiber is asked to
    /// stop.
    ///
    /// The waiter that must not give up -- a handle's release, which is where
    /// structured concurrency comes from -- uses
    /// [`FiberState::wait_passing_on_a_force`] instead.
    ///
    /// **Without this a parked joiner cannot be cancelled.** `join` and `wait`
    /// used to block on a `JoinHandle` or a latch with no flag check and no
    /// timeout, so a fiber parked in either observed a cancellation only once
    /// the child had finished on its own -- measured at 2000 ms against a
    /// 2000 ms child, on both backends, where the control stops in 0-2 ms
    /// (roadmap §16.7). A `main` that ends in a join would therefore not
    /// observe a signal at all.
    ///
    /// Answers true when it gave up rather than waited. The child is *not*
    /// stopped by this and is not waited for: the caller is unwinding and its
    /// own handle release is what still waits.
    fn wait_or_cancelled(&self) -> bool {
        self.wait_until(&|| crate::current::current(|fiber| fiber.gives_up_waiting()))
    }

    /// Waits until the fiber finishes or `give_up` says to stop asking.
    ///
    /// The two mechanisms `crate::channel::park_until_moved` uses, for the
    /// same reason: the condition variable registered with the fiber is what
    /// makes a cancellation immediate, and [`crate::channel::LOOK_AGAIN`] is
    /// what bounds the one that arrives between the check and the wait.
    fn wait_until(&self, give_up: &dyn Fn() -> bool) -> bool {
        match self {
            Completion::Thread(handle, done) => {
                // **The latch first, and the handle only once it is closed.**
                // A `JoinHandle` cannot be joined with a deadline, so a joiner
                // that blocked on it directly could not look at its flag
                // again. The child closes this latch as its last act on both
                // backends, so waiting on it is waiting for the thread -- and
                // it is a condition variable, which a cancellation can reach.
                if done.wait_until(give_up) {
                    return true;
                }
                let taken = handle.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(thread) = taken {
                    // A child that panicked has already reported it; there is
                    // nothing this fiber can do with the payload, and turning
                    // it into a parent panic would lose the child's message
                    // behind a second one.
                    let _ = thread.join();
                }
                false
            }
            Completion::Fiber(done) => done.wait_until(give_up),
        }
    }

    /// The latch the fiber closes as its last act, on either backend.
    pub(crate) fn latch(&self) -> Arc<Done> {
        match self {
            Completion::Thread(_, done) | Completion::Fiber(done) => done.clone(),
        }
    }

    pub(crate) fn finished(&self) -> bool {
        match self {
            Completion::Thread(handle, _) => {
                match handle.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    Some(thread) => thread.is_finished(),
                    // Already joined by somebody, so nothing is left to wait for.
                    None => true,
                }
            }
            Completion::Fiber(done) => done.finished(),
        }
    }
}

/// What a fiber answered, kept until somebody asks.
///
/// **Kept rather than reported.** Until a fiber could return a value there was
/// nowhere for its outcome to go, so an error that nobody joined was printed to
/// stderr and the object freed -- the runtime noticing a failure the program
/// could not. Now the outcome waits here and `join` takes a copy; the printing
/// survives only for the case it was always about, which is a fiber whose
/// handle is released without anybody ever having asked.
struct Legacy {
    /// `None` until the fiber finishes.
    ///
    /// A `Mutex` rather than atomics because a handle is `Share` and two
    /// fibers may be inside `join` at once, and "read it and take a reference
    /// to what is in it" is one operation or it is a race.
    outcome: Mutex<Option<Tagged>>,
    /// Whether a *successful* answer is a Khora pointer.
    ///
    /// An error's always is -- a raise carries an `Adt` and every `Adt` is
    /// boxed -- so only the `which == 0` word needs telling.
    boxed: bool,
    /// How to release a successful answer. Null for a value with no fields to
    /// let go of.
    glue: Option<extern "C" fn(*mut u8)>,
    /// Whether the "nobody was waiting for this" line has been written.
    ///
    /// Two places can write it: the fiber itself, the moment it finishes with
    /// an error, and `khora_fiber_release`, for a handle let go of later. Both
    /// are needed -- a held handle never reaches the second, and a fiber that
    /// has not finished never reaches the first -- and one failure is one
    /// message, so whichever arrives first claims it here.
    announced: std::sync::atomic::AtomicBool,
}

impl Legacy {
    /// Hands out a reference to the answer, leaving the stored one in place.
    ///
    /// **Every join gets its own reference**, which is what keeps "joining
    /// twice is joining once" true now that a join produces a value. The copy
    /// the state holds is released when the handle is, so the counts are the
    /// number of joiners plus one.
    fn observe(&self) -> Tagged {
        let held = self.outcome.lock().unwrap_or_else(|e| e.into_inner());
        match held.as_ref() {
            // Not finished, or already taken by a release. Neither can happen
            // to a caller that waited first and holds a live handle.
            None => Tagged { which: 0, payload: 0 },
            Some(outcome) => {
                if self.points_at_an_object(outcome) {
                    // SAFETY: the word is a live Khora object -- either a
                    // raised `Adt`, or a value of a type codegen said is
                    // boxed -- and this state holds a reference to it.
                    unsafe { crate::heap::khora_dup(outcome.payload as *mut u8) };
                }
                Tagged { which: outcome.which, payload: outcome.payload }
            }
        }
    }

    /// Whether the answer is an error, without taking it.
    ///
    /// For a nursery, which waits for its children without joining them. A
    /// cancellation is not a failure: it is what a nursery *does* to a child,
    /// and counting it would make every early exit look like one.
    fn failed(&self) -> bool {
        match self.outcome.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(outcome) => outcome.which != 0 && outcome.which != CANCELLED_WHICH,
            // Not finished, which a caller that waited first cannot see.
            None => false,
        }
    }

    /// Whether the stored word is a Khora object this state has a reference to.
    fn points_at_an_object(&self, outcome: &Tagged) -> bool {
        if outcome.payload == 0 || outcome.which == CANCELLED_WHICH {
            return false;
        }
        outcome.which != 0 || self.boxed
    }

    /// Lets go of the stored answer, reporting an error nobody ever asked for.
    ///
    /// A *value* nobody asked for is not worth a word -- plenty of fibers are
    /// spawned for what they do rather than for what they answer -- but an
    /// error is, because the alternative is a failure that left no trace
    /// anywhere.
    ///
    /// Idempotent, because it `take`s. The release calls it to get the message
    /// printed at the moment it means something, and `Drop` calls it again to
    /// catch the answer of a fiber that was detached and finished afterwards.
    fn discard(&self, reported: bool) {
        let taken = self.outcome.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(outcome) = taken else { return };
        if reported
            && outcome.which != 0
            && outcome.which != CANCELLED_WHICH
            && !self.announced.swap(true, Ordering::Relaxed)
        {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(b"khora: a fiber ended with an error nobody was waiting for\n");
        }
        if self.points_at_an_object(&outcome) {
            // An error's fields are not released: the runtime cannot know a
            // value's drop routine and the row said `'e`. A bounded leak, on a
            // path a joined fiber never takes.
            let glue = if outcome.which == 0 { self.glue } else { None };
            // SAFETY: see `points_at_an_object`; this reference is the state's
            // own and nothing reads it after this.
            unsafe { khora_drop(outcome.payload as *mut u8, glue) };
        }
    }
}

impl Drop for Legacy {
    /// The safety net for a detached fiber.
    ///
    /// A handle that is released waits and discards explicitly, so by the time
    /// this runs there is nothing left to do. A handle that was *detached* let
    /// go without waiting, and the child is still holding this -- so the
    /// answer arrives after nobody is listening, and this is the only thing
    /// left to let go of it. Silent, because a detached fiber's failure is one
    /// the program said it did not want to hear about.
    fn drop(&mut self) {
        self.discard(false);
    }
}

/// What a fiber handle points at.
pub(crate) struct FiberState {
    pub(crate) completion: Completion,
    /// The child's flag, shared with the child so a parent can set it.
    pub(crate) fiber: Arc<Fiber>,
    /// What it answered, once it has.
    legacy: Arc<Legacy>,
    /// Whether a joiner has taken the answer.
    ///
    /// Only read at release, to decide whether an error is worth printing. A
    /// fiber that was joined has already told somebody.
    observed: std::sync::atomic::AtomicBool,
}

/// A latch a fiber closes once and any number of joiners wait on.
///
/// **The completion-to-join handover, which is a two-sided problem.** A joiner
/// may be a fiber, which must give its worker back rather than hold one while
/// it waits; or it may be the program's own computation on a thread that is
/// not a worker at all, which has nothing to give back and must block. Both
/// happen, so both are here.
///
/// Idempotent by construction: a latch that is already closed is not waited on
/// at all, which is what lets `join` be called twice and lets a handle's
/// release join whatever an explicit `join` did not.
#[derive(Default)]
pub(crate) struct Done {
    state: Mutex<Latch>,
    /// For joiners that are threads rather than fibers.
    ///
    /// Behind an `Arc` so a waiting fiber can register it with itself: that
    /// registration is what lets `Fiber::cancel` notify the joiner rather than
    /// only set a flag it will not read again while it is parked here.
    closed: Arc<Condvar>,
}

#[derive(Default)]
struct Latch {
    finished: bool,
    /// Fibers to make runnable when it closes.
    waiting: Vec<crate::scheduler::Waker>,
}

impl Done {
    /// Closes the latch and releases everyone waiting on it.
    fn signal(&self) {
        let waiting = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.finished = true;
            std::mem::take(&mut state.waiting)
        };
        self.closed.notify_all();
        for waker in waiting {
            waker.wake();
        }
    }

    /// Whether the latch is already closed, without waiting on it.
    ///
    /// For a nursery sweeping the children that have finished: asking must not
    /// wait on the ones that have not.
    pub(crate) fn finished(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).finished
    }

    /// Waits for the latch, whatever the caller is, until `give_up` says stop.
    ///
    /// Answers true when it gave up. Both waiters are bounded by
    /// [`crate::channel::LOOK_AGAIN`] rather than blocking outright, for the
    /// reason `park_until_moved` states: a cancellation that lands between the
    /// check and the wait notifies a waiter that is not waiting yet, and that
    /// wake is lost. The registration is the fast path; the timeout is the
    /// bound.
    fn wait_until(&self, give_up: &dyn Fn() -> bool) -> bool {
        loop {
            // **The waker is enrolled under the same lock that reads the
            // flag.** A child that finishes between the two would otherwise
            // close the latch, find nobody waiting, and leave this fiber
            // parked for ever.
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.finished {
                    return false;
                }
                if give_up() {
                    return true;
                }
                match crate::scheduler::waker_for_current() {
                    Some(waker) => state.waiting.push(waker),
                    // Not a fiber, so there is no worker to give back: this
                    // thread does the waiting, which is what the program's own
                    // computation has always done at a `join`.
                    None => {
                        crate::current::current(|fiber| fiber.park_on(&self.closed));
                        while !state.finished {
                            let (held, _timed_out) = self
                                .closed
                                .wait_timeout(state, crate::channel::LOOK_AGAIN)
                                .unwrap_or_else(|e| e.into_inner());
                            state = held;
                            if !state.finished && give_up() {
                                drop(state);
                                crate::current::current(|fiber| fiber.unpark_from());
                                return true;
                            }
                        }
                        drop(state);
                        crate::current::current(|fiber| fiber.unpark_from());
                        return false;
                    }
                }
            }
            park_current();
        }
    }
}

/// The tag every fiber handle carries.
const FIBER_TAG: u32 = 0;

/// The pool every fiber in the process runs on, started the first time one is
/// spawned.
///
/// **Started lazily, and never stopped.** A program that spawns nothing pays
/// for nothing; a program that spawns keeps its workers until it exits, which
/// is when the operating system reclaims them. There is no shutdown because
/// there is no moment to run one: `main` returning ends the process, and a
/// pool that joined its workers first would be waiting on fibers nobody is
/// waiting for.
///
/// Zero means one worker per core, which is `Scheduler::new`'s reading of it.
fn fibers() -> &'static Scheduler {
    static FIBERS: std::sync::OnceLock<Scheduler> = std::sync::OnceLock::new();
    FIBERS.get_or_init(|| Scheduler::new(0))
}

/// Runs `body` on a fiber of its own, returning a handle to it.
///
/// Takes ownership of `body`: the fiber releases it when it finishes, so the
/// caller hands over a reference of its own.
///
/// **Exactly one of `call` and `plain` is given.** `call` is the trampoline
/// for a thunk that can fail, which hands back a tag; `plain` is the one for a
/// thunk that cannot, which hands back its answer as a word. A thunk with no
/// error row has no channel to say it was cancelled on, and so cannot be
/// stopped part-way -- which is the same fact the two trampolines encode.
///
/// `boxed` and `value_glue` describe the *answer*, so that a fiber nobody joins
/// does not leak it and a fiber joined twice does not free it twice.
///
/// # Safety
///
/// `body` must be a live Khora closure taking no arguments whose drop routine
/// is `glue`; whichever trampoline is given must match whether it returns the
/// tagged pair; and `boxed` must say truthfully whether its answer is a
/// pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_spawn(
    body: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
    call: Option<Trampoline1>,
    plain: Option<PlainTrampoline1>,
    boxed: bool,
    value_glue: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    // The compiler said this program has one thread and emitted non-atomic
    // reference counting on the strength of it. Carrying on would race every
    // count in the program. See `SINGLE_THREADED`.
    if SINGLE_THREADED.load(Ordering::Relaxed) == 1 {
        fatal("a fiber was spawned in a program compiled as single-threaded");
    }
    // **A spawn breaks the escape argument containment rests on.** A fiber
    // outlives the exported call that made it and may hold a reference to
    // something that call allocated, so discarding the call's registry would
    // free memory a live fiber is reading. Giving up on containing this call
    // is the safe direction; freeing under a running fiber is not.
    // `crate::contain`.
    crate::contain::disarm();
    let fiber = Fiber::spawned();
    let handed = Handed(body);
    let done = Arc::new(Done::default());
    let closes = done.clone();
    let child = fiber.clone();
    // A second reference, because `enter` takes the first and the answer is
    // decided after the thunk has returned. See `absorbed` below.
    let stopping = fiber.clone();
    let legacy = Arc::new(Legacy {
        outcome: Mutex::new(None),
        boxed,
        glue: value_glue,
        announced: std::sync::atomic::AtomicBool::new(false),
    });
    let answers = legacy.clone();

    let run = move || {
        // Named before it is destructured, so the closure captures the wrapper
        // rather than the pointer inside it. Rust captures fields precisely,
        // and a captured `*mut u8` is not `Send` however its container is
        // declared — the wrapper only helps if the wrapper is what moves.
        let handed = handed;
        let Handed(body) = handed;
        // On a thread this installs the identity for as long as the thread
        // runs the fiber. On the scheduler `Task::resume` has already done it,
        // around this turn and every other, on whichever worker took it — so
        // this entry is the outer one and restoring it changes nothing.
        let _entered = enter(child);

        // SAFETY: the caller guarantees a live `() -> ()` closure, and this
        // fiber now owns the reference. A closure's first field is its code
        // pointer; calling one with its own object is the convention generated
        // code uses.
        unsafe {
            let code = *body.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
            let outcome = match (call, plain) {
                (Some(run), _) => {
                    let mut payload: u64 = 0;
                    let which = run(code, body, &raw mut payload);
                    Tagged { which, payload }
                }
                (None, Some(run)) => Tagged { which: 0, payload: run(code, body) },
                // Nothing to call it with. Older callers passed neither and got
                // a void call; there is no longer a way to spell that, and
                // guessing at the callee's return type is how the wrong
                // register gets read.
                (None, None) => fatal("a fiber was spawned with no way to call its thunk"),
            };
            // Before anything else that can take time, because until this
            // runs every back-edge in the process pays for this fiber's
            // cancellation. `Fiber::retire`.
            stopping.retire();
            // **A cancellation the thunk absorbed is the fiber's answer**, and
            // the word it handed back is not.
            //
            // An infallible thunk has no channel to say it was stopped on, so
            // a total `catch` inside one releases its frame, calls
            // `khora_cancel_absorb` and returns a zero -- see that function
            // for why a zero and not something else. The zero arrives here as
            // an ordinary `which == 0`, which would make the handle report a
            // *value*: `join` would hand back nought, and a fiber that gave up
            // half way would be indistinguishable from one that finished. So
            // the record is read here, where the thunk has returned and
            // nothing else has looked at the answer yet.
            //
            // The word is released first where it is a pointer. A thunk whose
            // inner frame absorbed and whose outer frames carried on may hand
            // back a perfectly real object; replacing it without letting go of
            // it would leak one per cancelled fiber, which on a server is the
            // shape of leak with no allocation site to blame.
            //
            // **Except for an infallible thunk with a boxed answer**, which is
            // the one shape where this would do harm. `Fiber<A, {}>::join`
            // emits no branch on `which` -- there is no row for it to unwind
            // on, and the code generator says so in `fiber_intrinsic` -- so it
            // reads the word whatever the tag is. Storing a cancellation there
            // would hand a joiner a null typed as `A`. So that fiber keeps the
            // answer it produced: the handle cannot report a cancellation
            // because the *type* has nowhere to report one, which is the same
            // rule as everywhere else here rather than a new exception to it.
            let announce = !(plain.is_some() && boxed);
            let outcome = if stopping.has_absorbed() && announce {
                if boxed && outcome.which == 0 && outcome.payload != 0 {
                    // SAFETY (the enclosing block's): the caller promised
                    // `boxed` says truthfully whether a successful answer is a
                    // Khora pointer, and this fiber owns the reference the
                    // thunk just returned.
                    khora_drop(outcome.payload as *mut u8, value_glue);
                }
                Tagged { which: CANCELLED_WHICH, payload: 0 }
            } else {
                outcome
            };
            // **Stored before the closure is released**, because releasing it
            // may run arbitrary drop routines and a joiner woken in the middle
            // of that must find the answer already there.
            *answers.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
            khora_drop(body, glue);
        }
        // **A failure nobody is waiting for is said here, not at release.**
        //
        // `khora_fiber_release` already reports one, which covers a handle that
        // is let go of. It does not cover the shape a server is written in:
        // spawn the listener, keep the handle, and poll a stop flag. If the
        // bind fails, the fiber raises, the handle is still held, and the
        // release that would have printed never runs -- so the process sits
        // there having said nothing, serving nothing, and never exiting. That
        // is worse than a crash, because a supervisor that restarts on exit
        // sees a healthy process.
        //
        // Reported at most once: `announced` is the flag, and a later join or
        // release finds it already set and stays quiet rather than saying it
        // twice.
        {
            let held = answers.outcome.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(outcome) = held.as_ref() {
                if outcome.which != 0
                    && outcome.which != CANCELLED_WHICH
                    && !answers.announced.swap(true, Ordering::Relaxed)
                {
                    let mut err = std::io::stderr().lock();
                    let _ = err
                        .write_all(b"khora: a fiber ended with an error nobody was waiting for\n");
                }
            }
        }
        // Last, and after the closure has been released, so a joiner that
        // wakes immediately finds the fiber finished in every sense.
        closes.signal();
    };

    let completion = if on_the_scheduler() {
        let task = Task::with_fiber(fiber.clone(), run);
        // On a worker this goes to that worker's own queue, for the locality
        // 11D's stealing is built around; off one — the program's own
        // computation spawning its first fiber — it goes to the pool.
        if crate::coro::on_a_fiber() {
            crate::scheduler::schedule(task);
        } else {
            fibers().spawn(task);
        }
        Completion::Fiber(done)
    } else {
        Completion::Thread(Mutex::new(Some(std::thread::spawn(run))), done)
    };

    let object = khora_alloc(std::mem::size_of::<*mut FiberState>() as u64, FIBER_TAG);
    let state: Box<FiberState> = Box::new(FiberState {
        completion,
        fiber,
        legacy,
        observed: std::sync::atomic::AtomicBool::new(false),
    });
    // SAFETY: `khora_alloc` returned an object with one field's worth of space,
    // zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut FiberState>().write(Box::into_raw(state));
    }
    object
}

/// The state behind a fiber handle, or null if it has been released.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
pub(crate) unsafe fn fiber_state<'a>(fiber: *mut u8) -> Option<&'a FiberState> {
    if fiber.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_fiber_spawn` wrote there. Shared rather than exclusive: the handle
    // is shareable, so another fiber may be inside this state at the same
    // moment and a `&mut` would be undefined behaviour on its own.
    unsafe { (*fiber.add(KHORA_FIELD_OFFSET).cast::<*mut FiberState>()).as_ref() }
}

/// Waits for a fiber to finish, and answers what it answered.
///
/// The word goes through `out` and the tag comes back, the same shape every
/// fallible call in this runtime uses: a `which` of 0 means `out` holds the
/// value, and anything else means it holds an error to re-raise.
///
/// Idempotent: a fiber joined twice was joined once, and gets the answer
/// twice. Each join takes its own reference to a boxed answer, so two joiners
/// re-raising one error are two owners rather than a double free.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`], and `out` a
/// writable word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_join(fiber: *mut u8, out: *mut u64) -> u32 {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else {
        // SAFETY: the caller promised a writable word.
        unsafe { out.write(0) };
        return 0;
    };
    // **A joiner that is cancelled while parked unwinds rather than waits.**
    // The child is left running and is not cancelled here: what the joiner is
    // giving up is the *waiting*, and the handle's release still waits, which
    // is what keeps the child from outliving the binding.
    //
    // A zero word rather than the child's answer, because there is not one
    // yet. `CANCELLED_WHICH` is outside the range of error-type ids, so the
    // caller's `!` unwinds without any `catch` being able to name it, and the
    // word is never read on that path.
    if state.completion.wait_or_cancelled() {
        // SAFETY: the caller promised a writable word.
        unsafe { out.write(0) };
        return CANCELLED_WHICH;
    }
    state.observed.store(true, Ordering::Relaxed);
    let outcome = state.legacy.observe();
    // SAFETY: the caller promised a writable word.
    unsafe { out.write(outcome.payload) };
    outcome.which
}

/// Waits for a fiber without taking its answer.
///
/// **What a nursery does, and the difference from `join` is deliberate.** A
/// nursery waits for its children because it must not outlive them, not
/// because it wants what they computed -- it could not use it if it had it,
/// since every child's answer has a type of its own and a nursery holds them
/// as bare handles. So it waits, and the answer stays where it is until the
/// release lets go of it. That is also what keeps "a fiber that failed and
/// nobody joined says so on stderr" true of a child inside a nursery.
///
/// Answers whether it gave up on a cancellation rather than waited, which is
/// the one thing a caller with a `raises` row can act on.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
pub(crate) unsafe fn wait_or_cancel_for(fiber: *mut u8) -> bool {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return false };
    state.completion.wait_or_cancelled()
}

/// The same, for a waiter that must not give up.
///
/// **A nursery's own waits are here rather than above.** A nursery release
/// cancels its children and then waits for them, and a release that gave up on
/// its own cancellation would let a child outlive the binding — which is the
/// whole of what structured concurrency promises. So the cancellation this
/// fiber is already carrying does not shorten this wait. A force that arrives
/// during it is passed on: [`FiberState::wait_passing_on_a_force`].
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
pub(crate) unsafe fn wait_for(fiber: *mut u8) {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return };
    state.wait_passing_on_a_force();
}

impl FiberState {
    /// Waits for this fiber to finish, without giving up, and passes on a
    /// force the *waiter* receives while it waits.
    ///
    /// **What this prevents: a deadline that fires and ends nothing.** The
    /// deadline's shape is "cancel now, force if still running later", so the
    /// force nearly always lands on a fiber that is already in its cleanup,
    /// parked here on a child whose own shielded cleanup is stuck. Delivering
    /// the waiter's stop once, when the wait began, passed on the cancel it
    /// had then and nothing after; the force set a bit on a fiber parked in a
    /// wait that never looked at it, and the child was never told.
    ///
    /// So the wait gives up once, when the waiter is forced and has not yet
    /// passed it on, forces the child, and goes back to waiting. The child
    /// still finishes before this returns: a force shortens its cleanup, not
    /// this wait. Transitive without more machinery, because the forced child
    /// may itself be parked here on a grandchild, and is woken by the force to
    /// do the same.
    ///
    /// **A give-up rather than keeping the child findable by
    /// `cancel_open_crews`**, because one of the three waits that needs this --
    /// a handle's release -- has no crew to keep, and a fix in the wait covers
    /// all three with one piece of code. It costs one extra atomic load each
    /// time the waiter looks again: on every wake, and on a thread-backed
    /// waiter also every [`crate::channel::LOOK_AGAIN`], the bound the wait
    /// already had. On the scheduler the force's own wake (`stop_fiber`) is
    /// what gets it looked at.
    ///
    /// `passed_on` is what makes it give up *once*: without it a forced waiter
    /// would give up on every look and spin, re-forcing a child that is
    /// already forced, until the child finished. That is wasted CPU rather
    /// than a wrong answer, which is why no test catches its absence.
    pub(crate) fn wait_passing_on_a_force(&self) {
        let mut passed_on = false;
        while self
            .completion
            .wait_until(&|| !passed_on && crate::current::current(|me| me.is_forced()))
        {
            deliver_to(self, Stop::Force);
            passed_on = true;
        }
    }
}

/// Whether a finished fiber ended with an error, and takes the reporting on.
///
/// **The second half is why this is not a bare question.** A handle released
/// without anybody having joined it prints a line on stderr, because a failure
/// that nobody looked at would otherwise leave no trace anywhere. A nursery
/// that asks this *is* looking: it is about to raise `ChildFailed`, which is a
/// better report than the line and reaches the program rather than only the
/// terminal. So asking marks the answer observed, and the two reports do not
/// both happen.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`] that has finished.
pub(crate) unsafe fn failed_and_reported(fiber: *mut u8) -> bool {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return false };
    let failed = state.legacy.failed();
    if failed {
        state.observed.store(true, Ordering::Relaxed);
    }
    failed
}

/// Whether the fiber has finished, without waiting for it to.
///
/// **The question a supervisor loop has to be able to ask.** Every other way
/// of looking at a fiber blocks: `join` waits, `wait` waits, and letting the
/// handle go waits. A program that spawns a listener and then polls its own
/// stop flag has no way to notice that the listener is already gone -- so a
/// port that would not bind leaves the loop turning over a fiber that died
/// before the first pass. Answering this lets the loop end.
///
/// True the instant the outcome is stored, which is before the completion
/// latch is signalled, so a fiber this reports as finished is one whose answer
/// `join` can take without blocking.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_finished(fiber: *mut u8) -> bool {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else {
        // A handle already released has nothing left to wait for, which is
        // "finished" as far as a caller can tell.
        return true;
    };
    state
        .legacy
        .outcome
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

/// Whether the fiber was stopped rather than allowed to finish.
///
/// **The question `join` charges the process for.** A supervisor that notices
/// through [`khora_fiber_finished`] that its child is gone cannot tell a child
/// that bound and returned from one somebody cancelled, and the call that would
/// tell it is `join` -- which on a cancelled fiber unwinds its caller, and at
/// the entry point ends the program at 130. This answers without waiting and
/// without unwinding anything.
///
/// **Read from the fiber, not from the stored answer**, and that is the whole
/// of why it is truthful. The `announce` computation in [`khora_fiber_spawn`]
/// deliberately does not store `CANCELLED_WHICH` for an infallible thunk with
/// a boxed answer, because `Fiber<A, {}>::join` reads the word whatever the tag
/// is and a stored cancellation would hand a joiner a null typed as `A`. The
/// `absorbed` flag lives on the shared `Fiber` rather than in the `Tagged`, so
/// asking it reaches past that gate and **changes nothing about what `join`
/// reads**. The stored tag is consulted as well, for the fiber that never
/// reached its root -- a thread that unwound out of the thunk entirely.
///
/// Racy in the way [`khora_fiber_finished`] is racy and for the same reason: a
/// `false` is a fact about the instant it was asked, and a fiber cancelled a
/// microsecond later answers `true` at the next look. It says the fiber was
/// stopped; it says nothing about what the fiber had computed.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_cancelled(fiber: *mut u8) -> bool {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else {
        // A released handle has nothing left to answer about, and `finished`
        // takes the same position on the same state. Reporting a cancellation
        // for a fiber nobody can name any more would be inventing one.
        return false;
    };
    if state.fiber.has_absorbed() {
        return true;
    }
    match state.legacy.outcome.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(outcome) => outcome.which == CANCELLED_WHICH,
        // Not finished, and nothing has absorbed anything. Still running, as
        // far as this can be asked.
        None => false,
    }
}

/// Waits for a fiber, and answers what it ended as without unwinding.
///
/// **The question neither [`khora_fiber_join`] nor [`khora_fiber_cancelled`]
/// can answer.** `cancelled` says *whether* a fiber was stopped and has no way
/// to hand back what it computed; `join` hands back the answer and, on a
/// stopped fiber, answers [`CANCELLED_WHICH`] — which is in no row, so no
/// `catch` names it and at the entry point it ends the program at 130. A
/// caller that wants the answer *and* tolerates a stop has neither call.
///
/// # What comes back
///
/// The same `which`/`out` shape as `join`, with one tag more:
///
/// - `0` — `out` holds the answer, and this call took a reference to it.
/// - [`STOPPED_WHICH`] — the *child* was stopped. `out` is zero and holds
///   nothing to release; the stored answer, where there is one, stays with the
///   state and goes when the handle does.
/// - [`CANCELLED_WHICH`] — the *asker* was stopped while parked here. It
///   unwinds, exactly as a `join` in the same position does, because §6 of
///   `docs/design/effect-runtime.md` forbids swallowing a cancellation aimed
///   at the frame it reaches.
/// - anything else — the child's error, to re-raise, as `join` reports it.
///
/// # Why the stopped answer is read off the fiber rather than the stored tag
///
/// [`khora_fiber_spawn`]'s `announce` gate deliberately does not store
/// `CANCELLED_WHICH` for an infallible thunk with a boxed answer: `Fiber<A,
/// {}>::join` emits no branch on the tag, so it reads the word whatever the
/// tag says, and a stored cancellation would hand a joiner a null typed as
/// `A`. Reading the `absorbed` flag on the shared `Fiber` reaches past that
/// gate — the same route [`khora_fiber_cancelled`] takes, and for the same
/// reason — so **this changes nothing about what `join` reads**, and the two
/// questions cannot disagree about one fiber.
///
/// The cost is stated rather than hidden: a fiber that absorbed a cancellation
/// *and* went on to produce a value is reported stopped, and the value it
/// produced is not handed back. It is a value with a fabricated zero somewhere
/// inside it — `khora_cancel_absorb` returns one where the absorbing frame had
/// no channel — so the alternative is handing back an answer no part of the
/// program computed, wearing the type of one that was.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`], and `out` a
/// writable word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_outcome(fiber: *mut u8, out: *mut u64) -> u32 {
    // SAFETY: the caller promised a writable word. Written first, so every
    // early exit below leaves it defined rather than each remembering to.
    unsafe { out.write(0) };
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else {
        // A released handle has nothing left to answer about, and `finished`
        // and `cancelled` both take that position on the same state. Reporting
        // a stop for a fiber nobody can name any more would be inventing one.
        return 0;
    };
    // The asker's own cancellation, not the child's — and the child is left
    // running, exactly as a `join` giving up here leaves it. `STOPPED_WHICH`
    // is what the child's stop travels under, so the two are told apart by the
    // tag rather than by the caller guessing.
    if state.completion.wait_or_cancelled() {
        return CANCELLED_WHICH;
    }
    // Asked before the answer is taken, because taking it hands out a
    // reference this path must not hand out: `Outcome::Stopped` carries no
    // `A`, so nothing on the far side would release it.
    //
    // **Reordering this to read the stored outcome first leaks.** `observe`
    // dups the payload where it points at an object, and the stopped path
    // returns without writing `out`, so that reference has no owner. A review
    // argued the other order was needed to keep a child's failure from being
    // reported as a stop; the shape it described -- an inner frame absorbing a
    // cancellation, then the body raising -- was measured on this tree, with a
    // payload-carrying error, and the failure already arrives by name with its
    // payload intact. There is nothing here to trade a leak for.
    //
    // SAFETY: the caller guarantees a live handle, and this is the same
    // handle.
    if unsafe { khora_fiber_cancelled(fiber) } {
        return STOPPED_WHICH;
    }
    state.observed.store(true, Ordering::Relaxed);
    let outcome = state.legacy.observe();
    // SAFETY: the caller promised a writable word.
    unsafe { out.write(outcome.payload) };
    outcome.which
}

/// The same, for a program that wants the ordering and not the answer.
///
/// Answers [`CANCELLED_WHICH`] when the *waiter* was asked to stop and 0 when
/// the fiber finished, which is the shape every fallible call across this
/// boundary uses. There is no other tag: this never takes the fiber's answer,
/// so a child that failed is still the child's business.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_wait(fiber: *mut u8) -> u32 {
    // SAFETY: the caller guarantees a live handle.
    if unsafe { wait_or_cancel_for(fiber) } { CANCELLED_WHICH } else { 0 }
}

/// Lets go of a fiber without waiting for it.
///
/// **The valve, and the reason there has to be one.** Every other way out of a
/// handle waits: an explicit `join` waits, and so does the release, which is
/// where structured concurrency comes from. That is right, and it is also how
/// a program hangs -- one finalizer that never returns holds its nursery,
/// which holds its parent, up to `main`. `docs/design/scheduler.md` promises
/// both bounded cancellation latency and that a nursery exit leaves every child
/// stopped or joined, and those two are in tension exactly here.
///
/// So: signal, and go. The fiber keeps running, its answer is dropped when it
/// arrives, and nothing waits for it. A `timeout` over a body with an
/// uninterruptible tail is a lie without this -- it would promise to return in
/// five hundred milliseconds and then block on the tail.
///
/// **Cancels first.** A detached fiber that nobody asked to stop is a leak with
/// a nicer name; what a caller means by detaching is "I am no longer waiting
/// for this", and the honest reading of that is that the fiber should wind
/// itself up at its next cancellation point.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_detach(fiber: *mut u8) {
    if fiber.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live handle.
    unsafe { khora_fiber_cancel(fiber) };
    // SAFETY: the field holds what `khora_fiber_spawn` wrote, and nulling it
    // is what makes the eventual release find nothing to wait for.
    unsafe {
        let slot = fiber.add(KHORA_FIELD_OFFSET).cast::<*mut FiberState>();
        let state = *slot;
        if state.is_null() {
            return;
        }
        slot.write(std::ptr::null_mut());
        // Dropped without waiting, which is the whole of what detaching is.
        // Nothing in here is shared with the child except through an `Arc` --
        // the latch, the cancellation flag and the answer -- so the child
        // keeps what it still needs and lets go of it when it finishes,
        // `Legacy::drop` included. On the thread path this drops a
        // `JoinHandle`, which is how a thread is detached anyway.
        drop(Box::from_raw(state));
    }
}

/// Wakes a fiber known only by id, on the scheduler.
///
/// **The thread backend has nothing to do here** and says so by doing nothing:
/// `Fiber::cancel` already notified whatever condition variable the fiber
/// parked on, because a thread-backed fiber registers that condvar with
/// itself. On the scheduler the flag alone reaches a fiber that is running and
/// not one asleep on a deadline or a socket, so the wake has to go through the
/// pool.
///
/// For a caller that holds an id rather than a handle — the signal watcher,
/// which has the root fiber and no `Fiber<A, 'er>` object anywhere.
///
/// `cfg(unix)` because that watcher is the only caller and Windows has no
/// `sigwait` to run it: an unconditional definition is dead code there, and
/// the workspace denies warnings.
#[cfg(unix)]
pub(crate) fn cancel_by_id(id: usize) {
    if on_the_scheduler() {
        fibers().cancel_fiber(id);
    }
}

/// Asks a fiber to stop at its next cancellation point.
///
/// Returns immediately. The child stops where the source says it can — at a
/// `!` — and runs every finalizer between there and its root on the way out.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_cancel(fiber: *mut u8) {
    // SAFETY: the caller guarantees a live handle.
    unsafe { deliver(fiber, Stop::Cancel) }
}

/// Asks a fiber to stop at its next cancellation point **even inside its
/// cleanup**, and passes the same request to every child of its nurseries.
///
/// What [`khora_fiber_cancel`] cannot do, and on purpose: cancelling a fiber
/// that is already cancelled changes nothing, so its shielded finalizers run to
/// completion however many times it is asked. A finalizer that blocks for ever
/// therefore holds the fiber, and whoever waits for it, for ever. This is the
/// way out, and the only one; `crate::current::Fiber::force` has the full
/// argument, including why it is never undone.
///
/// Cancels the fiber too if nobody had. Idempotent. Returns immediately.
///
/// What it costs is what was asked for: cleanup cut off part-way, so a
/// `ROLLBACK` may not be sent. A single foreign call or file-system syscall
/// already in progress still runs to its end first.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_force(fiber: *mut u8) {
    // SAFETY: the caller guarantees a live handle.
    unsafe { deliver(fiber, Stop::Force) }
}

/// Cancels a fiber now, and forces it if it is still running `millis` later.
///
/// **What this prevents: a shutdown that waits for ever on cleanup that never
/// finishes, in a program that has nobody to send the force.** Cleanup runs
/// to completion and cancelling again does not cut it short, so a finalizer
/// blocked on a `receive` nobody answers holds the fiber, and its nursery,
/// and whoever waits on the nursery. The language has no timer of its own and
/// no default grace period, so the number is the caller's, always.
///
/// Returns immediately. A fiber that has already finished costs nothing more:
/// no deadline is recorded. Otherwise the deadline goes on one runtime-wide
/// list ([`Deadlines`]), kept by one thread, so a server bounding every
/// request's shutdown holds one entry per request -- a flag, a latch and a
/// time -- rather than one sleeping thread. A fiber that finishes first is
/// not forced.
///
/// **If the deadline thread cannot be started, the fiber is forced at once.**
/// The operating system refused a thread, the program is already short of
/// them, and waiting would need the thing that is missing; stopping the fiber
/// now is the bound the caller asked for, reached early, where the other
/// choice -- a panic out of an `extern "C"` function -- ends the process.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_cancel_within(fiber: *mut u8, millis: i64) {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return };
    let done = state.completion.latch();
    if done.finished() {
        return;
    }
    deliver_to(state, Stop::Cancel);
    let when = std::time::Instant::now() + std::time::Duration::from_millis(millis.max(0) as u64);
    if !Deadlines::add(Deadline { when, target: state.fiber.clone(), done }) {
        deliver_to(state, Stop::Force);
    }
}

/// One pending `cancel_within`: whom to force, and when, unless it has
/// finished by then.
struct Deadline {
    when: std::time::Instant,
    target: std::sync::Arc<crate::current::Fiber>,
    done: std::sync::Arc<Done>,
}

impl Deadline {
    /// Forces the fiber, the same road [`deliver_to`] takes, from the flag and
    /// the id rather than the handle, which the deadline list does not hold.
    fn expire(self) {
        if self.done.finished() {
            return;
        }
        crate::nursery::cancel_open_crews(self.target.id(), Stop::Force);
        if on_the_scheduler() {
            fibers().stop_fiber(self.target.id(), Stop::Force);
        } else {
            self.target.force();
        }
    }
}

impl PartialEq for Deadline {
    fn eq(&self, other: &Self) -> bool {
        self.when == other.when
    }
}
impl Eq for Deadline {}
impl PartialOrd for Deadline {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Deadline {
    /// Reversed, so a `BinaryHeap` -- a max-heap -- holds the soonest on top.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.when.cmp(&self.when)
    }
}

/// Every pending `cancel_within` in the process, and the one thread that
/// keeps them.
///
/// **What this prevents: a thread per call.** A `cancel_within` per request,
/// with a deadline of seconds, held one sleeping OS thread per request for the
/// whole deadline -- three thousand calls, three thousand threads -- and
/// `std::thread::spawn` panics when the OS refuses the next one. Here the cost
/// of a pending deadline is one heap entry, and the thread count is one.
///
/// The thread sleeps on a condition variable until the soonest deadline or a
/// new one, whichever comes first. It is started on the first deadline and
/// never ends, like the reactor.
struct Deadlines {
    heap: std::sync::Mutex<std::collections::BinaryHeap<Deadline>>,
    changed: std::sync::Condvar,
}

impl Deadlines {
    /// The one list, and whether its thread is running.
    fn get() -> Option<&'static Deadlines> {
        static LIST: std::sync::OnceLock<Option<&'static Deadlines>> = std::sync::OnceLock::new();
        *LIST.get_or_init(|| {
            let list: &'static Deadlines = Box::leak(Box::new(Deadlines {
                heap: std::sync::Mutex::new(std::collections::BinaryHeap::new()),
                changed: std::sync::Condvar::new(),
            }));
            std::thread::Builder::new()
                .name("khora-deadlines".into())
                .spawn(move || list.keep())
                .ok()
                .map(|_| list)
        })
    }

    /// Records `deadline`. False when there is no thread to keep it.
    fn add(deadline: Deadline) -> bool {
        let Some(list) = Deadlines::get() else { return false };
        let mut heap = list.heap.lock().unwrap_or_else(|e| e.into_inner());
        heap.push(deadline);
        list.changed.notify_one();
        true
    }

    /// The thread's whole life: wait for the soonest, expire what is due.
    fn keep(&self) {
        let mut heap = self.heap.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let now = std::time::Instant::now();
            let mut due = Vec::new();
            while heap.peek().is_some_and(|d| d.when <= now) {
                due.extend(heap.pop());
            }
            if !due.is_empty() {
                // Outside the lock: a force reaches the scheduler and the
                // nursery registry, and neither should wait on this list.
                drop(heap);
                for deadline in due {
                    deadline.expire();
                }
                heap = self.heap.lock().unwrap_or_else(|e| e.into_inner());
                continue;
            }
            heap = match heap.peek().map(|d| d.when.saturating_duration_since(now)) {
                Some(wait) => self.changed.wait_timeout(heap, wait).unwrap_or_else(|e| e.into_inner()).0,
                None => self.changed.wait(heap).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }
}

/// Delivers `stop` to a fiber by handle, and to its nurseries' children.
///
/// **One path for both kinds of stop**, because a force that took a shorter
/// route than a cancel would miss whichever child the shorter route skips --
/// and a forced parent whose child's cleanup is still shielded is still
/// waiting on that child.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
pub(crate) unsafe fn deliver(fiber: *mut u8, stop: Stop) {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return };
    deliver_to(state, stop);
}

/// [`deliver`], to the state behind a handle rather than the handle.
///
/// For [`khora_fiber_release`], which has taken the state out of its handle
/// before it waits, and still has to be able to pass a force on to it.
fn deliver_to(state: &FiberState, stop: Stop) {
    // **Its nurseries' children go with it, here.** A fiber whose body is a
    // nursery does not finish until its children do, so flagging it alone asks
    // it to stop and makes stopping impossible: it is blocked joining a child
    // nobody has told to stop, and will not read its own flag again until that
    // join returns. Delivered at the cancellation rather than waited for --
    // `khora_fibers_wait`'s between-rounds check cannot see a cancellation that
    // arrives mid-round, which is every cancellation that matters.
    crate::nursery::cancel_open_crews(state.fiber.id(), stop);
    if on_the_scheduler() {
        // Through the pool rather than the flag alone. Setting the flag is
        // what the child observes at its next `!`; waking it is what gets it
        // there, and a fiber asleep on a deadline or a socket would otherwise
        // sit on the cancellation until whatever it was waiting for happened
        // anyway. A thread blocked in a syscall has no equivalent, which is
        // one more thing the scheduler buys.
        fibers().stop_fiber(state.fiber.id(), stop);
    } else {
        match stop {
            Stop::Cancel => state.fiber.cancel(),
            Stop::Force => state.fiber.force(),
        }
    }
}

/// Joins a fiber and frees its handle.
///
/// This is a `drop_fields` callback, and it is where structured concurrency
/// comes from: releasing the last reference to a handle *waits*, so a fiber
/// cannot outlive the binding that holds it. Put the handle in a region and the
/// region waits; put it in a block and the block does.
///
/// **A cancelled or forced releaser passes its stop on to the child first**,
/// which is what a nursery release already does. Without it, a fiber cancelled while holding a
/// child's handle stops at its next `!` and then blocks here for the child's
/// full remaining run: the cancellation is observed promptly and the program
/// still waits out the work it asked to abandon. Measured at 2000 ms against a
/// 2000 ms child on both backends, which is the same number roadmap §16.7
/// recorded for the wait itself. A force that reaches the releaser *during*
/// the wait is passed on as well: [`FiberState::wait_passing_on_a_force`].
///
/// **What it stops need not be the releaser's own child.** A handle can sit
/// in a structure, and whichever fiber drops the last reference to that
/// structure releases the handle -- so a forced fiber that happens to be the
/// last holder forces a fiber somebody else spawned. That is accepted: the
/// last holder is the owner, and a cancel has always behaved the same way.
///
/// The flag rather than `stops_here`: this runs inside a region release, which
/// is [`crate::cancel::Shielded`], so the masked reading would say no every
/// time. What is being asked is "was this fiber cancelled", and the shield
/// exists to protect the cleanup rather than to hide that.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_release(fiber: *mut u8) {
    if fiber.is_null() {
        return;
    }
    // **A forced releaser forces**, for the reason `deliver` gives: this is the
    // other road a stop takes from a fiber to a child it holds, and it runs in
    // the releaser's cleanup -- exactly where a force has to reach.
    if let Some(stop) = crate::current::current(|holder| holder.pending_stop()) {
        // SAFETY: the caller guarantees a live handle, and delivering reads
        // the state without taking it.
        unsafe { deliver(fiber, stop) };
    }
    // SAFETY: the caller guarantees a live handle; the field holds what
    // `khora_fiber_spawn` wrote, and nothing else reads it after this.
    unsafe {
        let slot = fiber.add(KHORA_FIELD_OFFSET).cast::<*mut FiberState>();
        let state = *slot;
        if state.is_null() {
            return;
        }
        slot.write(std::ptr::null_mut());

        let state = Box::from_raw(state);
        state.wait_passing_on_a_force();
        // **After the wait, so there is an answer to discard.** A fiber whose
        // handle is released without anybody ever joining it is the one case
        // that still gets a word on stderr -- the error would otherwise leave
        // no trace anywhere, which is what this printed before a fiber could
        // return anything at all.
        state.legacy.discard(!state.observed.load(Ordering::Relaxed));
    }
}
