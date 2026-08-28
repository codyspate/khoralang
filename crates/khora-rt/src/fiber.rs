//! Fibers.
//!
//! **A fiber is an operating-system thread, or a coroutine on the scheduler.**
//! `docs/design/fibers.md` decided a fiber *is* the second and that the first
//! would do until the scheduler existed, on the argument that a program cannot
//! tell which it has. This file is where the two meet, and
//! `KHORA_FIBERS=scheduler` picks the coroutine.
//!
//! **Threads are the default, and the reason is a measurement**: `bench/service`
//! answers 782,149 requests a second on threads against about 429,000 on the
//! scheduler, on one machine in one sitting — the only kind of comparison
//! `bench/README.md` says travels. Both paths are kept because a number is only
//! worth having if it can be taken again.
//!
//! **One thing a program can tell, on the scheduler.** A thread gets the
//! operating system's stack — two megabytes on Linux, one on Windows — and a
//! coroutine gets `corosensei`'s megabyte with a guard page. Recursion that was
//! near the old limit is over the new one, and the failure is a clean fault at
//! the guard page rather than corruption.

use super::*;
use crate::coro::Task;
use crate::current::{enter, Fiber};
use crate::scheduler::{park_current, Scheduler};
use crate::heap::SINGLE_THREADED;
use crate::heap::{khora_alloc, khora_drop};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

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
    Thread(Mutex<Option<std::thread::JoinHandle<()>>>),
    /// A latch the child closes.
    Fiber(Arc<Done>),
}

impl Completion {
    fn wait(&self) {
        match self {
            Completion::Thread(handle) => {
                let taken = handle.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(thread) = taken {
                    // A child that panicked has already reported it; there is
                    // nothing this fiber can do with the payload, and turning
                    // it into a parent panic would lose the child's message
                    // behind a second one.
                    let _ = thread.join();
                }
            }
            Completion::Fiber(done) => done.wait(),
        }
    }

    pub(crate) fn finished(&self) -> bool {
        match self {
            Completion::Thread(handle) => {
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
        if reported && outcome.which != 0 && outcome.which != CANCELLED_WHICH {
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
    closed: std::sync::Condvar,
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

    /// Waits for the latch, whatever the caller is.
    fn wait(&self) {
        loop {
            // **The waker is enrolled under the same lock that reads the
            // flag.** A child that finishes between the two would otherwise
            // close the latch, find nobody waiting, and leave this fiber
            // parked for ever.
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.finished {
                    return;
                }
                match crate::scheduler::waker_for_current() {
                    Some(waker) => state.waiting.push(waker),
                    // Not a fiber, so there is no worker to give back: this
                    // thread does the waiting, which is what the program's own
                    // computation has always done at a `join`.
                    None => {
                        while !state.finished {
                            state = self.closed.wait(state).unwrap_or_else(|e| e.into_inner());
                        }
                        return;
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
    let legacy = Arc::new(Legacy {
        outcome: Mutex::new(None),
        boxed,
        glue: value_glue,
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
            // **Stored before the closure is released**, because releasing it
            // may run arbitrary drop routines and a joiner woken in the middle
            // of that must find the answer already there.
            *answers.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
            khora_drop(body, glue);
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
        Completion::Thread(Mutex::new(Some(std::thread::spawn(run))))
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
    state.completion.wait();
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
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
pub(crate) unsafe fn wait_for(fiber: *mut u8) {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return };
    state.completion.wait();
}

/// The same, for a program that wants the ordering and not the answer.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_wait(fiber: *mut u8) {
    // SAFETY: the caller guarantees a live handle.
    unsafe { wait_for(fiber) };
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
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return };
    if on_the_scheduler() {
        // Through the pool rather than the flag alone. Setting the flag is
        // what the child observes at its next `!`; waking it is what gets it
        // there, and a fiber asleep on a deadline or a socket would otherwise
        // sit on the cancellation until whatever it was waiting for happened
        // anyway. A thread blocked in a syscall has no equivalent, which is
        // one more thing the scheduler buys.
        fibers().cancel_fiber(state.fiber.id());
    } else {
        state.fiber.cancel();
    }
}

/// Joins a fiber and frees its handle.
///
/// This is a `drop_fields` callback, and it is where structured concurrency
/// comes from: releasing the last reference to a handle *waits*, so a fiber
/// cannot outlive the binding that holds it. Put the handle in a region and the
/// region waits; put it in a block and the block does.
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
        state.completion.wait();
        // **After the wait, so there is an answer to discard.** A fiber whose
        // handle is released without anybody ever joining it is the one case
        // that still gets a word on stderr -- the error would otherwise leave
        // no trace anywhere, which is what this printed before a fiber could
        // return anything at all.
        state.legacy.discard(!state.observed.load(Ordering::Relaxed));
    }
}
