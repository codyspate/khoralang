//! Fibers.
//!
//! **A fiber is an operating-system thread, and can be a coroutine on
//! Phase 11's scheduler instead.** `docs/design/fibers.md` decided a fiber
//! *is* the second of those and that the first would do until the scheduler
//! existed, on the argument that a program cannot tell which it has. The
//! scheduler now exists, and this file is where the two meet.
//!
//! `KHORA_FIBERS=scheduler` picks it. **Threads are the default, and the
//! reason is a measurement rather than caution**: `bench/service` answers
//! 782,149 requests a second on threads and about 429,000 on the scheduler,
//! on one machine in one sitting, which is the comparison `bench/README.md`
//! says is the only kind that travels.
//!
//! It was 59,965 until 11H found that the reactor's `poll` could not be
//! interrupted by a registration arriving while it waited — twelve times
//! became 1.8 times by adding a socket the reactor could be poked through.
//! What is left of the gap is written up there, along with the two things
//! tried that did not help.
//!
//! Both paths are kept because the number is only worth having if it can be
//! taken again.
//!
//! **One thing a program can tell, on the scheduler**, written here so nobody
//! has to find it. A thread gets the operating system's stack — two megabytes
//! on Linux, one on Windows. A coroutine gets `corosensei`'s, which is a
//! megabyte with a guard page. Khora recursing deeply enough to have been near
//! the old limit is over the new one, and the failure is a clean fault at the
//! guard page rather than corruption.

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

/// What a fiber handle points at.
pub(crate) struct FiberState {
    pub(crate) completion: Completion,
    /// The child's flag, shared with the child so a parent can set it.
    pub(crate) fiber: Arc<Fiber>,
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
/// The fiber is an operating-system thread today and a stackful coroutine
/// later. `docs/design/fibers.md` decides that, and the decision is that a
/// program cannot tell which — the handle is the same, and so is everything a
/// program can do with it.
///
/// `call` is null for a thunk that cannot fail, and otherwise the trampoline
/// that runs it and hands back its tag. A thunk with no error row has no
/// channel to say it was cancelled on, and so cannot be stopped part-way.
///
/// # Safety
///
/// `body` must be a live Khora closure of type `() -> ()` whose drop routine
/// is `glue`, and `call` must match whether it returns the tagged pair.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_spawn(
    body: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
    call: Option<Trampoline1>,
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
            match call {
                None => {
                    let run: extern "C" fn(*mut u8) = std::mem::transmute(code);
                    run(body);
                }
                Some(run) => {
                    let mut payload: u64 = 0;
                    let which = run(code, body, &raw mut payload);
                    finish_fiber(Tagged { which, payload });
                }
            }
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
    let state: Box<FiberState> = Box::new(FiberState { completion, fiber });
    // SAFETY: `khora_alloc` returned an object with one field's worth of space,
    // zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut FiberState>().write(Box::into_raw(state));
    }
    object
}

/// What to do with how a fiber ended.
///
/// A cancellation is the ordinary way for a stopped fiber to finish and needs
/// no announcement — it carries no payload either, so there is nothing to
/// release. An *error* nobody is waiting for is a different matter: it is
/// reported here rather than dropped in silence, which is what a panicking
/// thread does everywhere else.
///
/// The error object is freed but not its fields, because the runtime cannot
/// know a value's drop routine and the row said `'e`. That is a bounded leak
/// on a path that should not survive nurseries, where the error goes to a
/// parent who knows exactly what it is.
///
/// # Safety
///
/// `outcome.payload` must be null or a live Khora object when `which` is
/// neither 0 nor a cancellation.
unsafe fn finish_fiber(outcome: Tagged) {
    if outcome.which == 0 || outcome.which == CANCELLED_WHICH {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(b"khora: a fiber ended with an error nobody was waiting for\n");
    // SAFETY: per the contract above; a null callback releases the object
    // without touching fields whose types are not known here.
    unsafe { khora_drop(outcome.payload as *mut u8, None) };
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

/// Waits for a fiber to finish.
///
/// Idempotent: a fiber joined twice was joined once. That matters because the
/// handle's release joins whatever an explicit `join` did not, which is what
/// makes a fiber unable to outlive the binding that holds it.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_join(fiber: *mut u8) {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return };
    state.completion.wait();
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
    }
}
