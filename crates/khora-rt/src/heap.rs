//! Allocation and reference counting.
//!
//! The object header is [`crate::KhoraHeader`] and the layout contract with the
//! code generator is in the crate documentation. This is what acts on it:
//! `alloc`, the `dup`/`drop` pair, and the reuse tokens that let a `match` arm
//! build its result in the cell it matched.
//!
//! **Generated code counts references inline** — the add and the subtract are
//! emitted at the call site against offset zero, and only the last reference
//! calls in here, to [`khora_drop_last`]. `khora_dup` and `khora_drop` remain
//! because `khora-rt` is a C ABI anything may link against, and because drop
//! glue calls them for the fields it releases. `docs/design/reuse.md` §3.

use super::*;
use crate::counters::{ALLOC_COUNT, COUNTER_ORDER, LIVE_COUNT};
use std::alloc::{alloc_zeroed, dealloc, handle_alloc_error};
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// An object waiting to be released, and the callback that releases its
/// fields.
///
/// The pointer is kept as a `usize` because this lives in a `thread_local` and
/// a raw pointer there buys nothing: it is turned back the moment it is used,
/// under the same guarantee the queueing caller gave.
type Deferred = (usize, Option<extern "C" fn(*mut u8)>);

thread_local! {
    /// Objects whose last reference is gone and whose fields are not released yet.
    ///
    /// **Freeing used to be as deep as the value being freed.** A `drop_fields`
    /// callback releases the object's children by calling back in here, so
    /// releasing a cons list of a hundred thousand elements was a hundred thousand
    /// nested frames and the process died with no stack left. That was the last
    /// thing in the language whose cost was proportional to the *depth* of a
    /// value: every traversal in `std`'s `List` is a loop, and a list a traversal
    /// consumes is freed one node at a time as it walks — but one that is merely
    /// released, an intermediate inside `sort` or a field of a record going out of
    /// scope, went the deep way. It is why `List::sort` still gave out in the tens
    /// of thousands after everything else stopped.
    ///
    /// So the recursion becomes a queue. `None` means nothing is draining and the
    /// next free owns the drain; `Some` means one is in progress, and a nested
    /// free hands its object over rather than descending into it. The order frees
    /// happen in is not observable — nothing runs at destruction but the release
    /// of children — so a queue is as correct as the stack was.
    ///
    /// Per thread, because reference counts are per object and two threads may be
    /// freeing different graphs at once. The cost is one thread-local access on
    /// the path that *actually frees*, which already pays for `dealloc`; the
    /// common case, a decrement that does not reach zero, never gets here.
    static PENDING: RefCell<Option<Vec<Deferred>>> = const { RefCell::new(None) };
}

/// Hands an object to whoever is draining, or claims the drain.
///
/// `true` when the object was queued and this call is done with it. `false`
/// when there was no drain in progress — the caller now owns one, and must
/// release the object itself and then call [`drain`].
fn queued(ptr: *mut u8, glue: Option<extern "C" fn(*mut u8)>) -> bool {
    PENDING.with(|pending| {
        let mut slot = pending.borrow_mut();
        match slot.as_mut() {
            Some(queue) => {
                queue.push((ptr as usize, glue));
                true
            }
            None => {
                *slot = Some(Vec::new());
                false
            }
        }
    })
}

/// Releases everything queued, then ends the drain.
///
/// The borrow is taken to pop and released before the object is freed, because
/// freeing it queues its children and that borrows again. Overlapping the two
/// would panic.
fn drain() {
    loop {
        let next = PENDING.with(|pending| {
            pending.borrow_mut().as_mut().and_then(|queue| queue.pop())
        });
        let Some((ptr, glue)) = next else { break };
        // SAFETY: the pointer was queued by a caller that had taken its
        // refcount to zero and had not freed it, so it is still allocated and
        // nothing else refers to it.
        unsafe { release_now(ptr as *mut u8, glue) };
    }
    PENDING.with(|pending| *pending.borrow_mut() = None);
}

/// Runs an object's field-releasing callback and frees it.
///
/// The tail every drop path shares, with no decrement of its own: the caller
/// has already taken the refcount to zero.
///
/// # Safety
///
/// `ptr` must be a live object whose refcount has reached zero, and
/// `drop_fields` must be the callback for its layout.
unsafe fn release_now(ptr: *mut u8, drop_fields: Option<extern "C" fn(*mut u8)>) {
    // Read the layout out of the header *before* running the callback, so a
    // callback that scribbles on the header cannot make the deallocation use a
    // layout that differs from the allocation's.
    //
    // SAFETY: the object is still allocated at this point.
    let layout = object_layout(unsafe { (*ptr.cast::<KhoraHeader>()).field_bytes });

    if let Some(drop_fields) = drop_fields {
        // Releases the children this object owns. They come back through
        // `khora_drop`, see a drain in progress, and queue themselves rather
        // than descending.
        drop_fields(ptr);
    }

    // SAFETY: `ptr` came from `alloc_zeroed` with exactly `layout`, which was
    // rebuilt from the same `field_bytes` the allocation used and the constant
    // alignment; the refcount is zero, so no other reference exists; and the
    // callback has already released everything the object owned.
    unsafe { dealloc(ptr, layout) };

    LIVE_COUNT.fetch_sub(1, COUNTER_ORDER);
    crate::contain::forget(ptr);
}

/// Allocates a heap object with `size` bytes of fields and the given `tag`,
/// with a refcount of 1.
///
/// Returns a pointer to the object's *header*. The fields live at
/// `ptr + KHORA_FIELD_OFFSET` (16 bytes past the header) and are **zeroed**.
///
/// Zeroing is not decoration. Generated code stores fields one at a time after
/// this returns, and a `drop_fields` callback that ran over a half-built object
/// would otherwise interpret uninitialized bytes as pointers. Zero plus
/// [`khora_drop`]'s null tolerance makes that case a no-op instead of a wild
/// free. Generated code must still store a field before *reading* it; a zeroed
/// field is droppable, not meaningful.
///
/// Aborts if the allocator fails or if `size` exceeds [`MAX_FIELD_BYTES`].
#[unsafe(no_mangle)]
pub extern "C" fn khora_alloc(size: u64, tag: u32) -> *mut u8 {
    if size > MAX_FIELD_BYTES as u64 {
        fatal("allocation exceeds the maximum object size");
    }
    let field_bytes = size as u32;
    let layout = object_layout(field_bytes);

    // SAFETY: `layout` always includes the header, so its size is non-zero,
    // which is `alloc_zeroed`'s one precondition.
    let ptr = unsafe { alloc_zeroed(layout) };
    if ptr.is_null() {
        handle_alloc_error(layout);
    }

    // SAFETY: `ptr` is a fresh allocation of `KHORA_HEADER_SIZE + size` bytes
    // aligned to `KHORA_HEADER_ALIGN`, so it is valid and correctly aligned for
    // writing one `KhoraHeader`. Nothing else refers to it yet, so the write
    // cannot race and cannot clobber an initialized field.
    unsafe {
        ptr.cast::<KhoraHeader>()
            .write(KhoraHeader { refcount: AtomicU64::new(1), tag, field_bytes });
    }

    ALLOC_COUNT.fetch_add(1, COUNTER_ORDER);
    LIVE_COUNT.fetch_add(1, COUNTER_ORDER);
    // Only while a guarded export call is on this thread's stack, which is a
    // thread-local read and a not-taken branch everywhere else.
    // `crate::contain` has the cost note.
    crate::contain::record(ptr);
    ptr
}

/// Increments an object's refcount. Null is a no-op.
///
/// # Safety
///
/// `ptr` must be null or a live object from [`khora_alloc`], and the caller
/// must own a reference to it — that is what makes it live for the duration of
/// the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_dup(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: by the contract above `ptr` points at a live object, so its
    // header is initialized.
    unsafe {
        let header = ptr.cast::<KhoraHeader>();
        // Relaxed is enough: the caller already owns a reference, so the
        // object cannot be freed underneath this, and nothing is being
        // published. Ordering is only needed on the *last* release, where
        // `khora_drop` establishes it.
        (*header).refcount.fetch_add(1, Ordering::Relaxed);
    }
}

/// Decrements an object's refcount, freeing it when the count reaches zero.
/// Null is a no-op.
///
/// `drop_fields` is the object's field-dropping routine, or `None` for an
/// object that owns no references (one whose fields are all `Int`/`Bool`, or
/// which has no fields at all). It is called with **the same header pointer**
/// this function received, immediately before the memory is released, and must
/// drop only what the object owns — the child references in its fields. It must
/// not free, dup or resurrect the object itself.
///
/// Emit one routine per *type*, switching on the tag, rather than one per
/// variant. A drop site usually knows only the static type of the value it is
/// releasing, so a routine that assumes one variant's fields will read past the
/// end of a smaller sibling — `Nil` has no tail to load, and the byte after it
/// belongs to the allocator.
///
/// Null tolerance exists so the code generator can emit a drop for a slot that
/// is only conditionally initialized without guarding every site, and so the
/// most common code generation slip fails safe.
///
/// Aborts on a refcount that is already zero, which means a double free or a
/// missing [`khora_dup`].
///
/// # Safety
///
/// `ptr` must be null or a live object from [`khora_alloc`], and the caller
/// must own the reference being released. `drop_fields` must match the object's
/// actual field layout — the runtime cannot check that, since it does not know
/// what the fields mean.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_drop(ptr: *mut u8, drop_fields: Option<extern "C" fn(*mut u8)>) {
    if ptr.is_null() {
        return;
    }
    let header = ptr.cast::<KhoraHeader>();

    // Release, so that everything this thread did to the object happens
    // before whichever thread performs the final decrement sees the count
    // reach zero. The standard reference-counting pair; the matching acquire
    // is the fence below.
    //
    // SAFETY: `ptr` points at a live object per the contract above, so its
    // header is initialized and valid to read and write.
    let refcount = unsafe { (*header).refcount.fetch_sub(1, Ordering::Release) };
    if refcount == 0 {
        fatal("drop of an object whose refcount is already zero (double free, or a missing dup)");
    }
    if refcount > 1 {
        return;
    }

    // Last reference, and this thread is the one that took it to zero. The
    // acquire pairs with every other thread's release, so their writes are
    // visible before the fields are read and the memory is freed.
    std::sync::atomic::fence(Ordering::Acquire);

    // Queued if something above is already freeing, so a graph is released
    // iteratively rather than as deep as it is. See [`PENDING`].
    if queued(ptr, drop_fields) {
        return;
    }
    // SAFETY: this thread took the count to zero and holds the only claim.
    unsafe { release_now(ptr, drop_fields) };
    drain();
}

/// Releases a reference and, if it was the last, keeps the memory.
///
/// The first half of reuse. This is [`khora_drop`] with one difference: on the
/// last reference it runs the field-dropping routine and returns the object's
/// memory **without freeing it**, so the caller can build the next object in
/// the same cell. On any other outcome — a shared object, a null pointer — it
/// behaves exactly as `khora_drop` does and returns null.
///
/// The value returned is a *token*, and the caller owes it to
/// [`khora_alloc_reuse`] on every path. It is memory with no owner: nothing
/// will free it and no counter is tracking it. `docs/design/reuse.md` §2 is
/// where the code generator's rule for guaranteeing that lives — the token may
/// only be taken where the arm reaches its constructor unconditionally.
///
/// The live-object counter goes down here rather than in `khora_alloc_reuse`,
/// so that a program observing it between the two sees the object gone. It is
/// the same object either way; what reuse saves is the allocator, not the
/// bookkeeping.
///
/// # Safety
///
/// As [`khora_drop`]: `ptr` must be null or live, the caller must own the
/// reference being released, and `drop_fields` must match the layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_drop_reuse(
    ptr: *mut u8,
    drop_fields: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    let header = ptr.cast::<KhoraHeader>();

    // SAFETY: live per the contract, so the header is initialized.
    let refcount = unsafe { (*header).refcount.fetch_sub(1, Ordering::Release) };
    if refcount == 0 {
        fatal("drop of an object whose refcount is already zero (double free, or a missing dup)");
    }
    if refcount > 1 {
        // Somebody else still holds it, so there is nothing to hand over and
        // the caller's `khora_alloc_reuse` will allocate as usual.
        return std::ptr::null_mut();
    }

    std::sync::atomic::fence(Ordering::Acquire);

    // **The object is kept, so it is not queued — but its children are.**
    // Claiming the drain around the callback is what makes a reused cell's
    // subtree release iteratively; without it, rebuilding a long list in place
    // frees the old tails as deep as they are.
    let owns_drain = !PENDING.with(|pending| pending.borrow().is_some());
    if owns_drain {
        PENDING.with(|pending| *pending.borrow_mut() = Some(Vec::new()));
    }
    if let Some(drop_fields) = drop_fields {
        drop_fields(ptr);
    }
    if owns_drain {
        drain();
    }

    LIVE_COUNT.fetch_sub(1, COUNTER_ORDER);
    ptr
}

/// Builds an object, in the memory a token carries when it fits.
///
/// The second half of reuse, and the only place a token from
/// [`khora_drop_reuse`] may be spent. Three cases, and the counters stay
/// honest in all of them:
///
/// - a null token allocates, exactly as [`khora_alloc`] would;
/// - a token whose cell is the right size is rewritten in place with the new
///   tag and a refcount of one — no allocator call at all;
/// - a token of the wrong size is freed and replaced, because a `Cons` cell
///   cannot hold a bigger variant and writing one there would run off the end.
///
/// The last case is why a caller may hand over a token without first proving
/// the shapes agree: the size lives in the header, so the check is one
/// comparison here rather than a static analysis there.
///
/// # Safety
///
/// `token` must be null or memory from [`khora_drop_reuse`] that has not been
/// spent, and `size` must not exceed [`MAX_FIELD_BYTES`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_alloc_reuse(token: *mut u8, size: u64, tag: u32) -> *mut u8 {
    if token.is_null() {
        return khora_alloc(size, tag);
    }
    if size > MAX_FIELD_BYTES as u64 {
        fatal("allocation exceeds the maximum object size");
    }
    let field_bytes = size as u32;

    // SAFETY: the token is memory from `khora_drop_reuse`, whose header is
    // still readable — it read the layout out of it itself.
    let held = unsafe { (*token.cast::<KhoraHeader>()).field_bytes };
    if held != field_bytes {
        // SAFETY: the token owns this memory and nothing else refers to it; the
        // layout is rebuilt from the header the allocation used.
        unsafe { dealloc(token, object_layout(held)) };
        return khora_alloc(size, tag);
    }

    // SAFETY: as above. The fields are about to be written by the caller, which
    // is the same contract `khora_alloc` leaves them under — except that they
    // are not zeroed here, because the caller writes every one of them before
    // anything can read them. `khora_alloc`'s zeroing exists for the window
    // between allocation and the first store, and a reused cell's window is
    // covered by `drop_fields` having already run.
    unsafe {
        token
            .cast::<KhoraHeader>()
            .write(KhoraHeader { refcount: AtomicU64::new(1), tag, field_bytes });
    }
    LIVE_COUNT.fetch_add(1, COUNTER_ORDER);
    token
}

/// Set when the compiler decided this program cannot start a thread.
///
/// Generated code then counts references with plain arithmetic rather than
/// atomics, which is only sound if it was right. [`khora_fiber_spawn`] checks
/// this, so being wrong is a message naming the mistake rather than a data
/// race in a refcount — which would be memory corruption a long way from its
/// cause, and the single worst failure mode this runtime has.
pub(crate) static SINGLE_THREADED: AtomicUsize = AtomicUsize::new(0);

/// Records that generated code is counting references non-atomically.
///
/// Called once from `main`, before anything else. `docs/design/reuse.md` §4.
#[unsafe(no_mangle)]
pub extern "C" fn khora_single_threaded() {
    SINGLE_THREADED.store(1, Ordering::Relaxed);
}

/// The slow half of a drop the caller decremented itself.
///
/// Generated code decrements the refcount inline and calls this only when the
/// reference it released looks like the last one — see `docs/design/reuse.md`
/// §3. `previous` is what the decrement returned, so that the already-zero
/// check happens here rather than in the emitted code, where it would be a
/// branch on every drop in the program to catch a bug that must not happen.
///
/// The fence, the field-dropping callback and the deallocation are all here,
/// which is the point: the common case is a decrement and a not-taken branch,
/// and only the last reference pays for a call.
///
/// # Safety
///
/// `ptr` must be a live object whose refcount the caller has just decremented
/// with [`Ordering::Release`], `previous` must be what that decrement returned,
/// and `drop_fields` must match the object's layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_drop_last(
    ptr: *mut u8,
    drop_fields: Option<extern "C" fn(*mut u8)>,
    previous: u64,
) {
    if previous == 0 {
        fatal("drop of an object whose refcount is already zero (double free, or a missing dup)");
    }

    // Acquire, pairing with every other thread's release, so their writes are
    // visible before the fields are read and the memory is freed. The matching
    // release is the caller's decrement.
    std::sync::atomic::fence(Ordering::Acquire);

    // As `khora_drop`, and this is the one generated code calls: a graph is
    // released iteratively, so freeing costs no stack. See [`PENDING`].
    if queued(ptr, drop_fields) {
        return;
    }
    // SAFETY: still allocated — this thread took the count to zero and holds
    // the only claim on it.
    unsafe { release_now(ptr, drop_fields) };
    drain();
}

/// Frees a token nothing spent.
///
/// A safety net rather than part of the design. The code generator only takes
/// a token where the arm reaches its constructor unconditionally, so nothing
/// should ever reach this — but "should" and "does" differ by one unforeseen
/// lowering path, and the difference between them is a silent leak. Emitting
/// this at the end of an arm makes that case cost an extra call instead.
///
/// # Safety
///
/// `token` must be null or unspent memory from [`khora_drop_reuse`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_free_reuse(token: *mut u8) {
    if token.is_null() {
        return;
    }
    // SAFETY: the token owns this memory, its header is still readable, and
    // its fields were released by `khora_drop_reuse`.
    unsafe {
        let held = (*token.cast::<KhoraHeader>()).field_bytes;
        dealloc(token, object_layout(held));
    }
}

/// Reads an object's refcount. Null reads as zero.
///
/// Exists for tests: reference counting is invisible when it works, and a test
/// that cannot see the count can only assert that nothing crashed.
///
/// # Safety
///
/// `ptr` must be null or a live object from [`khora_alloc`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_refcount(ptr: *const u8) -> u64 {
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: `ptr` points at a live object per the contract above, so its
    // header is initialized and valid to read.
    unsafe { (*ptr.cast::<KhoraHeader>()).refcount.load(Ordering::Relaxed) }
}

/// Frees an object without touching its reference count or its children.
///
/// **Only for [`crate::contain::discard`]**, and only sound because of the
/// invariant that function documents: every object in a discarded call's
/// registry was allocated during that call, so everything any of them points
/// at is in the list too and is freed by its own entry. Running drop glue here
/// would cascade into children that are then visited again, which is a double
/// free; decrementing instead would leave a tree whose root is gone.
///
/// # Safety
///
/// `ptr` must be a live object from [`khora_alloc`] that nothing outside the
/// discarded call can reach, and must be freed exactly once.
pub(crate) unsafe fn release_raw(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live object, so its header is readable
    // and `field_bytes` is the one the allocation used.
    let field_bytes = unsafe { (*ptr.cast::<KhoraHeader>()).field_bytes };
    let layout = object_layout(field_bytes);
    // SAFETY: `ptr` came from `alloc_zeroed` with exactly this layout, and the
    // caller guarantees nothing else reaches it.
    unsafe { dealloc(ptr, layout) };
    LIVE_COUNT.fetch_sub(1, COUNTER_ORDER);
}
