//! Shared cells: the one way a mutable value crosses into another fiber.
//!
//! A `Shared<A>` is a lock and a word. What makes it the *only* way is not
//! here but in the checker — `docs/design/sharing.md` — and what is here is the
//! part that has to be right whatever the checker allows: the value behind the
//! lock is released by a callback registered when the cell was opened, because
//! generated code cannot reach through a `Mutex`.

use super::*;
use crate::counters::COUNTER_ORDER;
use crate::heap::{khora_alloc, khora_drop, khora_dup};
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

/// What a `Shared<A>` holds.
///
/// The value as the one word every Khora value fits in, plus what is needed to
/// let go of it. The runtime cannot know `A`, so the drop routine and whether
/// the word is even a pointer are recorded once when the cell is opened rather
/// than passed to every operation.
struct Cell {
    value: u64,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
}

/// A cell and the fiber currently changing it.
///
/// `holder` is outside the lock deliberately. It is not for excluding anyone —
/// the `Mutex` does that — but for saying *deadlock* out loud, and a check that
/// had to take the lock to read it would be the very thing it is trying to
/// report.
struct Held {
    holder: AtomicUsize,
    cell: Mutex<Cell>,
}

/// The tag every shared cell carries.
const SHARED_TAG: u32 = 0;

/// Fiber ids, handed out on first use and never reused.
static NEXT_FIBER: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// This fiber's id, which is only ever compared for equality.
    ///
    /// Not a thread id: `ThreadId` has no stable integer form on stable Rust,
    /// and this has to sit in an atomic so it can be read without locking.
    static FIBER: usize = NEXT_FIBER.fetch_add(1, COUNTER_ORDER) + 1;
}

/// Stops the program rather than letting a fiber wait for itself.
///
/// Every operation checks, not only `update`: the lock is held for the whole of
/// a change function, so a `get` or a `set` from inside one is the same
/// deadlock reached by a different door. Read without locking, which is why
/// `holder` lives outside the `Mutex` — a check that had to take the lock to
/// read it would be the very thing it is trying to report.
fn deny_reentry(held: &Held, doing: &str) -> usize {
    let me = FIBER.with(|id| *id);
    if held.holder.load(COUNTER_ORDER) == me {
        fatal(&format!(
            "this fiber is inside `Shared::update` on this cell, so it cannot also {doing} it: \
             a change function runs under the lock, and reaching the same cell again would \
             wait for itself"
        ));
    }
    me
}

/// Releases a word, given what was recorded about it.
///
/// # Safety
///
/// `value` must be null or a live object matching `boxed` and `glue`.
unsafe fn release_word(value: u64, boxed: bool, glue: Option<extern "C" fn(*mut u8)>) {
    if boxed {
        // SAFETY: per the contract above.
        unsafe { khora_drop(value as *mut u8, glue) };
    }
}

/// The cell behind a handle, or `None` once it has been released.
///
/// # Safety
///
/// `cell` must be a live object from [`khora_shared_open`].
unsafe fn held_of<'a>(cell: *mut u8) -> Option<&'a Held> {
    if cell.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_shared_open` wrote there. Shared rather than exclusive: a cell is
    // `Share`, so another fiber may be inside it at this moment.
    unsafe { (*cell.add(KHORA_FIELD_OFFSET).cast::<*mut Held>()).as_ref() }
}

/// Opens a cell holding `value`.
///
/// Takes ownership of the value: the cell releases it when the cell goes.
///
/// # Safety
///
/// `value` must be null or a live Khora object when `boxed`, and `glue` its
/// drop routine.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_open(
    value: u64,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Held>(), SHARED_TAG);
    let held: Box<Held> = Box::new(Held {
        holder: AtomicUsize::new(0),
        cell: Mutex::new(Cell { value, boxed, glue }),
    });
    // SAFETY: `khora_alloc` returned an object with one field's worth of space,
    // zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Held>().write(Box::into_raw(held));
    }
    object
}

/// The value in the cell, as a new reference.
///
/// Duplicated *under the lock*: between reading the word and claiming a
/// reference to it, another fiber's `set` could otherwise take the last one and
/// free it.
///
/// # Safety
///
/// `cell` must be a live object from [`khora_shared_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_get(cell: *mut u8) -> u64 {
    let Some(held) = (unsafe { held_of(cell) }) else {
        fatal("reading a shared cell that has already been released");
    };
    deny_reentry(held, "read");
    let cell = held.cell.lock().unwrap_or_else(|e| e.into_inner());
    if cell.boxed {
        // SAFETY: the cell has held a reference to this since it was stored.
        unsafe { khora_dup(cell.value as *mut u8) };
    }
    cell.value
}

/// Replaces the value, releasing the one that was there.
///
/// The old value is let go of *after* the lock, because releasing it runs its
/// drop routine — which may reach a cell of its own, and a lock held across
/// that is a lock ordering nobody agreed to.
///
/// # Safety
///
/// `cell` must be live, and `value` a live object owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_set(cell: *mut u8, value: u64) {
    let Some(held) = (unsafe { held_of(cell) }) else {
        fatal("writing a shared cell that has already been released");
    };
    deny_reentry(held, "write");
    let (old, boxed, glue) = {
        let mut cell = held.cell.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::mem::replace(&mut cell.value, value);
        (old, cell.boxed, cell.glue)
    };
    // SAFETY: the cell owned this reference and has just given it up.
    unsafe { release_word(old, boxed, glue) };
}

/// How generated code hands over a change function.
///
/// The closure's own parameter and result are `A`, which has no single machine
/// type, so the shim converting them to and from the one word every Khora value
/// fits in is emitted per instantiation on the other side of the boundary.
/// Only scalars and pointers cross here, as everywhere else.
type Change = extern "C" fn(*const u8, *mut u8, u64) -> u64;

/// Reads, transforms and writes, all as one step.
///
/// `change` is called with the current value and its result becomes the new
/// one. It runs **once**, under the lock, which is what makes the read and the
/// write atomic against every other fiber — and what makes a change function
/// that updates the cell it is changing a deadlock, reported here rather than
/// waited out.
///
/// **`change` cannot fail, and that is what makes this safe rather than
/// careful.** A function with no error row has no channel to be interrupted on
/// — the same reason `khora_fiber_spawn` takes a null trampoline for one — so
/// nothing can leave the critical section except by returning, and there is no
/// path on which the lock is still held. Work that can fail belongs outside:
/// compute it, then `set` the answer.
///
/// Returns the value the cell ended up holding, as a new reference, so a caller
/// can see what it did without a second read that another fiber could get
/// between.
///
/// # Safety
///
/// `cell` must be live, `change` a live Khora closure of type `(A) -> A`
/// borrowed for the call, and `call` the shim matching it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_update(
    cell: *mut u8,
    change: *mut u8,
    call: Change,
) -> u64 {
    let Some(held) = (unsafe { held_of(cell) }) else {
        fatal("updating a shared cell that has already been released");
    };

    let me = deny_reentry(held, "update");
    let mut cell = held.cell.lock().unwrap_or_else(|e| e.into_inner());
    held.holder.store(me, COUNTER_ORDER);
    let (boxed, glue) = (cell.boxed, cell.glue);

    // The change function takes its argument owned, like every other Khora
    // parameter, so it gets a reference of its own and consumes it.
    if boxed {
        // SAFETY: the cell has held a reference to this since it was stored.
        unsafe { khora_dup(cell.value as *mut u8) };
    }

    // SAFETY: the caller guarantees a live closure and a matching shim. A
    // closure's first field is its code pointer, which is the convention
    // generated code uses to call one.
    let produced = unsafe {
        let code = *change.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
        call(code, change, cell.value)
    };

    let old = std::mem::replace(&mut cell.value, produced);
    if boxed {
        // SAFETY: still under the lock, so nothing can have taken this. The
        // caller gets a reference of its own to what is now in there.
        unsafe { khora_dup(produced as *mut u8) };
    }
    held.holder.store(0, COUNTER_ORDER);
    drop(cell);

    // SAFETY: the cell owned this and has just given it up. Outside the lock,
    // because a drop routine can reach a cell of its own.
    unsafe { release_word(old, boxed, glue) };
    produced
}

/// How generated code hands over a change function that also answers.
///
/// Two words come back where [`Change`] has one, and only scalars cross here,
/// so the new state is returned and the answer is written through the pointer
/// — the same shape the tagged-return trampolines use for the same reason.
type Modify = extern "C" fn(*const u8, *mut u8, u64, *mut u64) -> u64;

/// Reads, transforms, writes, and gives back something that is not the state.
///
/// [`khora_shared_update`] can only answer with what it left in the cell, and
/// that is not always what the caller needs to know. A handler that inserts a
/// record under a generated key has to get *that* key back; searching the new
/// state for it afterwards is a guess, and a wrong one as soon as two fibers
/// insert records that look alike.
///
/// So the change function returns both, and both are installed and handed back
/// under the one lock.
///
/// # Safety
///
/// `cell` must be live, `change` a live Khora closure borrowed for the call,
/// `call` the shim matching it, and `answer` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_modify(
    cell: *mut u8,
    change: *mut u8,
    call: Modify,
    answer: *mut u64,
) -> u64 {
    let Some(held) = (unsafe { held_of(cell) }) else {
        fatal("updating a shared cell that has already been released");
    };

    let me = deny_reentry(held, "update");
    let mut cell = held.cell.lock().unwrap_or_else(|e| e.into_inner());
    held.holder.store(me, COUNTER_ORDER);
    let (boxed, glue) = (cell.boxed, cell.glue);

    if boxed {
        // SAFETY: the cell has held a reference to this since it was stored.
        unsafe { khora_dup(cell.value as *mut u8) };
    }

    let mut produced_answer: u64 = 0;
    // SAFETY: the caller guarantees a live closure and a matching shim.
    let produced = unsafe {
        let code = *change.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
        call(code, change, cell.value, &raw mut produced_answer)
    };

    let old = std::mem::replace(&mut cell.value, produced);
    held.holder.store(0, COUNTER_ORDER);
    drop(cell);

    // SAFETY: the cell owned this and has just given it up. Outside the lock,
    // because a drop routine can reach a cell of its own.
    unsafe { release_word(old, boxed, glue) };
    // SAFETY: the caller guarantees `answer` is writable.
    unsafe { *answer = produced_answer };
    produced
}

/// Releases a cell and the value in it.
///
/// This is a `drop_fields` callback: [`khora_drop`] calls it when the last
/// reference to the handle goes.
///
/// # Safety
///
/// `cell` must be a live object from [`khora_shared_open`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_release(cell: *mut u8) {
    if cell.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live handle; the field holds what
    // `khora_shared_open` wrote, and nothing else reads it after this.
    unsafe {
        let slot = cell.add(KHORA_FIELD_OFFSET).cast::<*mut Held>();
        let held = *slot;
        if held.is_null() {
            return;
        }
        slot.write(std::ptr::null_mut());

        let held = Box::from_raw(held);
        let cell = held.cell.into_inner().unwrap_or_else(|e| e.into_inner());
        release_word(cell.value, cell.boxed, cell.glue);
    }
}
