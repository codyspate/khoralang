//! Fibers.
//!
//! **A fiber is an operating-system thread today.** `docs/design/fibers.md`
//! decided that a fiber *is* a stackful coroutine multiplexed onto worker
//! threads and that the first implementation makes each one a thread, on the
//! argument that a program cannot tell which it has. It cannot, and the cost is
//! that a fiber is worth roughly what a thread is worth — about 33 KB — so a
//! server holds thousands of connections and not hundreds of thousands.
//!
//! Replacing this with a scheduler is Phase 11 of `docs/roadmap.md`, and the
//! whole point of the interface below is that nothing above it changes when
//! that happens.

use super::*;
use crate::cancel::{CANCELLED, ON_FIBER};
use crate::heap::SINGLE_THREADED;
use crate::counters::COUNTER_ORDER;
use crate::heap::{khora_alloc, khora_drop};
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// What a fiber handle points at.
pub(crate) struct FiberState {
    /// `None` once joined. Joining twice is not an error — the second is a
    /// no-op — because the handle's release joins whatever `join` did not.
    ///
    /// Behind a lock because `Fiber` is `Share`: two fibers may hold one handle
    /// and both call `join`, and "take the handle if it is there" is exactly
    /// the read-modify-write that has to happen once. Without it both could see
    /// `Some` and join the same thread twice.
    pub(crate) thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The child's flag, shared with the child so a parent can set it.
    pub(crate) cancel: Arc<AtomicUsize>,
}

/// The tag every fiber handle carries.
const FIBER_TAG: u32 = 0;

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
    let cancel = Arc::new(AtomicUsize::new(0));
    let handed = Handed(body);
    let child_flag = cancel.clone();

    let thread = std::thread::spawn(move || {
        // Named before it is destructured, so the closure captures the wrapper
        // rather than the pointer inside it. Rust captures fields precisely,
        // and a captured `*mut u8` is not `Send` however its container is
        // declared — the wrapper only helps if the wrapper is what moves.
        let handed = handed;
        let Handed(body) = handed;
        CANCELLED.with(|c| *c.borrow_mut() = child_flag);
        ON_FIBER.with(|f| f.set(true));

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
    });

    let object = khora_alloc(std::mem::size_of::<*mut FiberState>(), FIBER_TAG);
    let state: Box<FiberState> = Box::new(FiberState { thread: Mutex::new(Some(thread)), cancel });
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
    let taken = state.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(thread) = taken {
        // A child that panicked has already reported it; there is nothing this
        // fiber can do with the payload, and turning it into a parent panic
        // would lose the child's message behind a second one.
        let _ = thread.join();
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
    state.cancel.store(1, COUNTER_ORDER);
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
        let taken = state.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(thread) = taken {
            let _ = thread.join();
        }
    }
}
