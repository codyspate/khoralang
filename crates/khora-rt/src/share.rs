//! Marking a value shared when the runtime publishes it, and the debug check
//! that finds an entry that forgot to.
//!
//! **What this prevents: a local object reaching a second fiber unmarked.**
//! Every object starts local to the fiber that made it. A value becomes
//! reachable from another fiber only through a runtime entry: a spawn's
//! closure, a channel send, a cell's contents, a fiber's answer. Each of those
//! calls [`khora_share`] on what it publishes, before the lock or hand-off
//! that publishes it, so the receiving thread sees the
//! bit before it sees the pointer. A program built with `KHORA_RC_LOCAL=1`
//! reads it on every count: clear, and the count is a relaxed load, an add
//! and a relaxed store; set, and it is the locked read-modify-write. A missed
//! mark there is two threads plain-counting one object, and the owner check
//! below is what reports it. Without the switch every count is locked and
//! the bit feeds only the owner check.
//!
//! # The invariant a deep mark rests on
//!
//! **A shared object points only at shared or immortal objects.** The walk
//! establishes it. Afterwards, a `Share` value has no `mut` field, and the two
//! runtime containers that can be written after publication (`Shared`,
//! `Channel`) mark what is stored into them. So the walk stops at an object
//! already shared, and sending the same structure twice costs one load the
//! second time.
//!
//! **A deferred finalizer is not a route, though its captures need not be
//! `Share`.** `Region::defer` takes a finalizer that may hold a `mut` record
//! or a `Map` the deferring fiber goes on writing, which no mark could cover:
//! the first write after it stores a fresh local object into a shared one.
//! So the finalizer never crosses at all. A `Region` or `Scope` stays on
//! the fiber that opened it (`khora_types::REGION_TYPE`), and
//! `crate::region` traps a defer from any other fiber and the root region
//! from a spawned one, so a finalizer is deferred, run and released on one
//! fiber and its captures stay local.
//!
//! # Fiber migration is not a crossing
//!
//! A local object is touched by one fiber, and a fiber runs on one worker at
//! a time. It moves between workers only through the scheduler's queues,
//! which are `Mutex`es, so the old worker's unlock happens before the new
//! worker's lock and every count write made on one is visible on the other,
//! with the two never concurrent. That is the argument `Task`'s `Send` impl
//! already rests on for every other byte of the fiber's stack. It is also why
//! the owner recorded below is the *fiber*, not the thread: a thread owner
//! would call every migrated fiber's objects foreign.
//!
//! # The walk is the drop glue
//!
//! There is no separate marking routine per type. A type's `drop_fields`
//! already visits exactly the fields that hold a reference, by calling
//! [`crate::khora_drop`] on each with that child's own glue. While this module
//! is walking, `khora_drop` on this thread queues the child here instead of
//! releasing it. So drop and mark cannot disagree about which fields a value
//! has, which is the one mistake a second, generated walk could make. The
//! cost is a relaxed load of [`WALKS`] in `khora_drop`, a global that is
//! written only while some thread is marking.
//!
//! **This holds only while every glue releases its children through the
//! runtime's `khora_drop`.** A glue that used the inline decrement generated
//! code emits for a local drop would, inside a walk, decrement real counts
//! instead of queueing: a use-after-free, not a missed mark.
//!
//! # The owner check
//!
//! A debug build of a program asks for it ([`khora_rc_check_owners`]). From
//! then on [`crate::khora_alloc`] writes the allocating fiber's id into bits
//! 61..40 of the count word, and every count of a local object compares it
//! with the fiber doing the counting. A mismatch is a crossing no entry
//! marked, and it traps naming both fibers. Release builds never ask, write
//! zero, and check nothing. The header is the same 16 bytes in both, so
//! generated code does not know which it is running under. The alternative,
//! an 8-byte prefix before the header, needs a different allocation layout in
//! debug and a free path that knows which one it is freeing.

use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// How many threads are inside a walk right now.
///
/// Read by [`crate::khora_drop`] on every call, so a release that is not part
/// of a walk costs one relaxed load of a line nobody writes, not a
/// thread-local lookup.
static WALKS: AtomicUsize = AtomicUsize::new(0);

/// An object still to be visited, and its drop routine.
pub(crate) type Pending = (usize, Option<extern "C" fn(*mut u8)>);

thread_local! {
    /// This thread's walk: `Some` while one is in progress, holding what is
    /// still to be visited.
    ///
    /// **A work list, not recursion.** A list of a million cells is a million
    /// deep through its tails, and a recursive walk would run out of stack on
    /// it. `crate::heap`'s `PENDING` made the same move for freeing.
    ///
    /// A walk runs no Khora code and never suspends, so a fiber cannot migrate
    /// in the middle of one and leave this on the wrong thread.
    static WALK: RefCell<Option<Vec<Pending>>> = const { RefCell::new(None) };
}

/// Whether the owner check is on for this process. See [`khora_rc_check_owners`].
static CHECKING: AtomicBool = AtomicBool::new(false);

/// Turns on the debug owner check, for the rest of the process.
///
/// Called by the generated `main` of a debug build, before anything is
/// allocated. A release build never calls it, so its count words carry no
/// owner and nothing is checked.
#[unsafe(no_mangle)]
pub extern "C" fn khora_rc_check_owners() {
    CHECKING.store(true, Ordering::Relaxed);
}

/// Whether the owner check is on. See [`khora_rc_check_owners`].
#[inline(always)]
pub(crate) fn checking() -> bool {
    CHECKING.load(Ordering::Relaxed)
}

/// The owner bits [`crate::khora_alloc`] writes: the running fiber's id,
/// placed in [`KHORA_OWNER_MASK`], or zero when nothing is checked.
pub(crate) fn owner_bits() -> u64 {
    if !checking() {
        return 0;
    }
    (running_fiber() << KHORA_OWNER_SHIFT) & KHORA_OWNER_MASK
}

/// The running fiber's id.
#[cfg(not(target_family = "wasm"))]
fn running_fiber() -> u64 {
    crate::current::current(|fiber| fiber.id()) as u64
}

/// There are no fibers on wasm, so nothing is ever foreign: zero is "not
/// recorded", which the check skips.
#[cfg(target_family = "wasm")]
fn running_fiber() -> u64 {
    0
}

/// Traps when a local object is counted by a fiber that did not make it.
///
/// `word` is the count word as the operation found it. Shared and immortal
/// objects are exempt, and so is an object with no recorded owner: one made
/// before the check was on, or by a fiber whose id is zero in the owner bits.
///
/// **A detector, not a mode.** A trap here reports a missed mark. Without
/// `KHORA_RC_LOCAL=1` the count beside it is locked anyway, so the trap
/// names corruption that would happen only once local counts are plain; with
/// the switch, the count beside it is plain, and this is the one thing that
/// sees the race before it corrupts the heap. The cost is a call per count
/// of a local object, in debug builds only, which is the profile `khora
/// test` uses.
#[unsafe(no_mangle)]
// SHARE: reads a count word; takes no object.
pub extern "C" fn khora_rc_check(word: u64) {
    if word & (KHORA_SHARED | KHORA_IMMORTAL) != 0 {
        return;
    }
    let owner = (word & KHORA_OWNER_MASK) >> KHORA_OWNER_SHIFT;
    if owner == 0 {
        return;
    }
    let here = running_fiber() & (KHORA_OWNER_MASK >> KHORA_OWNER_SHIFT);
    if here != owner {
        fatal_owner(owner, here);
    }
}

/// The owner check's message. Its own function so the message is one string.
#[cold]
fn fatal_owner(owner: u64, here: u64) -> ! {
    let _ = writeln!(
        std::io::stderr(),
        "khora: object made on fiber {owner} was counted on fiber {here} without being \
         shared -- a runtime entry published it without marking it"
    );
    let _ = std::io::stderr().flush();
    std::process::exit(134)
}

/// Marks `ptr` and everything it reaches as shared.
///
/// Called by every runtime entry that makes a value reachable from another
/// fiber, before the operation that does so. Stops at an object that is
/// already shared or immortal, so marking a structure that was marked before
/// costs one load. Null is a no-op.
///
/// Iterative, over a work list, so a value of any depth costs no stack. The
/// cost is a pass over every newly marked cell: a header write and a call
/// into its type's glue.
///
/// # Safety
///
/// `ptr` must be null or a live object the caller holds a reference to, and
/// `glue` its drop routine: the one `khora_drop` would be given for it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_share(ptr: *mut u8, glue: Option<extern "C" fn(*mut u8)>) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: the caller's contract is `walk`'s.
    unsafe { walk(|| queue(ptr, glue)) };
}

/// Marks what a fiber's error payload reaches, through the routine that would
/// release it.
///
/// An error is released by `error_glue(which, payload)`, which calls
/// `khora_drop` on the boxed error with its own glue. Run inside a walk, that
/// call queues it instead, so the same routine serves as the error's mark.
///
/// # Safety
///
/// `error_glue` must be the compiler's error releaser, and `payload` a live
/// error of kind `which` the caller holds.
pub(crate) unsafe fn share_error(which: u32, payload: u64, error_glue: extern "C" fn(u32, u64)) {
    // SAFETY: the releaser only calls `khora_drop` on what the error holds,
    // which the walk turns into queueing.
    unsafe { walk(|| error_glue(which, payload)) };
}

/// Runs `seed` inside a walk, then visits everything it queued.
///
/// # Safety
///
/// Everything `seed` passes to `khora_drop` must be live, held by the caller,
/// and given its own drop routine.
unsafe fn walk(seed: impl FnOnce()) {
    // A walk inside a walk would be a glue calling back in here, which none
    // does. If one ever did, it adds to the outer list and the outer loop
    // visits it.
    let outer = WALK.with(|w| {
        let mut slot = w.borrow_mut();
        if slot.is_some() {
            true
        } else {
            *slot = Some(Vec::new());
            false
        }
    });
    WALKS.fetch_add(1, Ordering::Relaxed);
    seed();
    if !outer {
        while let Some((ptr, glue)) = WALK.with(|w| w.borrow_mut().as_mut().and_then(Vec::pop)) {
            // SAFETY: everything queued came from `seed` or from a glue run
            // on a live object, which is only ever its children.
            unsafe { visit(ptr as *mut u8, glue) };
        }
        WALK.with(|w| *w.borrow_mut() = None);
    }
    WALKS.fetch_sub(1, Ordering::Relaxed);
}

/// Adds one object to this thread's walk.
fn queue(ptr: *mut u8, glue: Option<extern "C" fn(*mut u8)>) {
    WALK.with(|w| {
        if let Some(list) = w.borrow_mut().as_mut() {
            list.push((ptr as usize, glue));
        }
    });
}

/// Marks one object and queues its children.
///
/// # Safety
///
/// `ptr` must be a live object and `glue` its drop routine.
unsafe fn visit(ptr: *mut u8, glue: Option<extern "C" fn(*mut u8)>) {
    // SAFETY: live per the contract, so the header is initialized.
    let count = unsafe { &(*ptr.cast::<KhoraHeader>()).refcount };
    let before = count.load(Ordering::Relaxed);
    if before & (KHORA_SHARED | KHORA_IMMORTAL) != 0 {
        return;
    }
    // Atomic, although the object is local to this thread by the invariant,
    // so that a missed crossing elsewhere costs a wrong answer from the owner
    // check rather than a torn count.
    count.fetch_or(KHORA_SHARED, Ordering::Relaxed);
    let Some(glue) = glue else { return };
    // **A runtime handle's release is not a visit.** It joins a fiber, runs
    // finalizers or frees a queue. Handles are born shared, so the test
    // above stops the walk before it gets here, but a handle made some other
    // way would otherwise be joined by being sent.
    if is_a_handle_release(glue) {
        return;
    }
    glue(ptr);
}

/// Whether `glue` is one of the runtime's own handle releases, which do more
/// than release fields.
#[cfg(not(target_family = "wasm"))]
fn is_a_handle_release(glue: extern "C" fn(*mut u8)) -> bool {
    let at = glue as usize;
    let releases: [unsafe extern "C" fn(*mut u8); 5] = [
        crate::khora_region_release,
        crate::khora_fiber_release,
        crate::khora_fibers_release,
        crate::khora_shared_release,
        crate::channel::khora_channel_release,
    ];
    releases.iter().any(|r| *r as usize == at)
}

/// A cell is the only handle on wasm, and it is born shared like the rest.
#[cfg(target_family = "wasm")]
fn is_a_handle_release(glue: extern "C" fn(*mut u8)) -> bool {
    glue as usize == crate::khora_shared_release as usize
}

/// Whether `khora_drop` on this thread is part of a walk, and if so queues
/// the object and answers true.
///
/// The first test is a relaxed load of a global. Only when some thread is
/// walking does this reach the thread-local.
#[inline(always)]
pub(crate) fn walked(ptr: *mut u8, glue: Option<extern "C" fn(*mut u8)>) -> bool {
    if WALKS.load(Ordering::Relaxed) == 0 {
        return false;
    }
    walked_slow(ptr, glue)
}

#[inline(never)]
fn walked_slow(ptr: *mut u8, glue: Option<extern "C" fn(*mut u8)>) -> bool {
    WALK.with(|w| match w.borrow_mut().as_mut() {
        Some(list) => {
            list.push((ptr as usize, glue));
            true
        }
        None => false,
    })
}

/// The references `object` holds, each with its own drop routine, found by
/// running `glue` with this thread's `khora_drop` turned into collecting.
///
/// For `crate::handoff`, which needs a closure's captures and a `Share`
/// value's fields but not the types they have. Nothing is marked and no count
/// changes. A runtime handle's release answers nothing: it is not a field
/// walk, and a handle is born shared, so no walk reaches one.
pub(crate) fn children(object: *mut u8, glue: extern "C" fn(*mut u8)) -> Vec<Pending> {
    if is_a_handle_release(glue) {
        return Vec::new();
    }
    let outer = WALK.with(|w| w.borrow_mut().replace(Vec::new()));
    WALKS.fetch_add(1, Ordering::Relaxed);
    glue(object);
    WALKS.fetch_sub(1, Ordering::Relaxed);
    WALK.with(|w| std::mem::replace(&mut *w.borrow_mut(), outer)).unwrap_or_default()
}

/// Makes a runtime-allocated handle shared from birth.
///
/// A channel, cell, fiber, nursery or region handle exists to be held by more
/// than one fiber, and its count is cold, so it is marked when made rather
/// than at every entry that could hand one over.
pub(crate) fn born_shared(object: *mut u8) {
    // SAFETY: `object` is a fresh allocation from `khora_alloc`, so its header
    // is initialized.
    let count: &AtomicU64 = unsafe { &(*object.cast::<KhoraHeader>()).refcount };
    count.fetch_or(KHORA_SHARED, Ordering::Relaxed);
}
