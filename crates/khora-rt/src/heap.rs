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
#[cfg(target_family = "wasm")]
use std::alloc::alloc_zeroed;
use std::alloc::{dealloc, handle_alloc_error};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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
    /// So past [`SHALLOW`] levels the recursion becomes a queue. `None` means
    /// nothing is draining, and the next free either recurses (see [`claim`])
    /// or owns the drain; `Some` means one is in progress, and a nested free
    /// hands its object over rather than descending into it. The order frees
    /// happen in is not observable — nothing runs at destruction but the release
    /// of children — so a queue is as correct as the stack was.
    ///
    /// Per thread, because reference counts are per object and two threads may be
    /// freeing different graphs at once. The cost is one thread-local access on
    /// the path that *actually frees*, which already pays for `dealloc`; the
    /// common case, a decrement that does not reach zero, never gets here.
    static PENDING: RefCell<Option<Vec<Deferred>>> = const { RefCell::new(None) };

    /// How many releases on this thread are nested inside one another without
    /// a drain, each one freeing its object directly from inside its parent's
    /// `drop_fields`. Never more than [`SHALLOW`]; see [`claim`].
    ///
    /// It belongs to the fiber, as the drain does (S3), and leaves with it at
    /// a switch: see [`take_drain`].
    static DIRECT: Cell<u32> = const { Cell::new(0) };
}

/// How deep a release may recurse before it falls back to the drain.
///
/// **What this prevents: paying for the queue on every small graph.** Most
/// objects a program frees are shallow -- a record and its strings, a JSON
/// node and its fields -- and queueing each child costs a vector allocation
/// for the drain, a push and a pop per child, and the thread-local calls
/// around them. Freeing them directly, by recursion, is what the queue
/// replaced, and for a graph this shallow the recursion is safe. On the
/// TechEmpower server it saves about 5% of user instructions per request on
/// `/json` and on `/fortunes`.
///
/// **What it costs: 24 more frames of stack at most**, a `release_now`, a
/// `drop_fields` and a `khora_drop` each, on top of what a drain uses. The
/// release at this depth claims a drain, and everything under it is queued
/// as before, so a list of any length is still freed in bounded stack.
const SHALLOW: u32 = 24;

/// What the caller of [`claim`] now owes the object it took to zero.
enum Claim {
    /// Release it directly, then [`leave_direct`].
    Direct,
    /// Nothing: it is queued behind the drain in progress.
    Queued,
    /// Release it, then [`drain`]: the caller opened the drain.
    Drain,
}

/// Decides how an object whose last reference is gone gets released.
///
/// **A drain in progress takes the object, whatever the depth.** Below
/// [`SHALLOW`] with no drain the caller releases directly; at it, the caller
/// opens a drain. The first rule is not what bounds the stack: a drain this
/// function opened runs at depth [`SHALLOW`], so its objects queue by the
/// depth alone. It decides only the drain [`khora_drop_reuse`] opens at any
/// depth, whose released subtree stays on the queue, as it was before direct
/// release existed.
///
/// Not inlined, for the reason [`leave_direct`] gives: it is reached again
/// from inside releases that may have moved the fiber to another worker.
#[inline(never)]
fn claim(ptr: *mut u8, glue: Option<extern "C" fn(*mut u8)>) -> Claim {
    PENDING.with(|pending| {
        let mut slot = pending.borrow_mut();
        if let Some(queue) = slot.as_mut() {
            queue.push((ptr as usize, glue));
            return Claim::Queued;
        }
        let depth = DIRECT.with(Cell::get);
        if depth < SHALLOW {
            DIRECT.with(|direct| direct.set(depth + 1));
            return Claim::Direct;
        }
        *slot = Some(Vec::new());
        Claim::Drain
    })
}

/// Ends a direct release that [`claim`] allowed.
///
/// **Not inlined, and it decrements rather than restoring a saved depth.**
/// The release before it can suspend (a finalizer that blocks, a fiber
/// handle's join) and come back on another worker. Inlined, the compiler may
/// reuse the thread-local's address computed before the switch, and write the
/// depth into the old worker's slot, under whichever fiber runs there now
/// (`crate::coro::installed` has the argument). A saved depth would be right
/// only on the thread it was read on; the fiber's own depth came with it
/// through [`restore_drain`], so taking one off is right on either.
#[inline(never)]
fn leave_direct() {
    DIRECT.with(|direct| direct.set(direct.get() - 1));
}

/// Releases the object the caller took to zero, by whichever route [`claim`]
/// picks. The tail of [`khora_drop`] and [`khora_drop_last`] for an object
/// with a field routine.
///
/// # Safety
///
/// As [`release_now`]: `ptr` is live with a count of zero, the caller holds
/// the only claim, and `drop_fields` matches its layout.
#[inline(always)]
unsafe fn release_last(ptr: *mut u8, drop_fields: Option<extern "C" fn(*mut u8)>) {
    match claim(ptr, drop_fields) {
        Claim::Direct => {
            // SAFETY: the caller's contract, passed on unchanged.
            unsafe { release_now(ptr, drop_fields) };
            leave_direct();
        }
        Claim::Queued => {}
        Claim::Drain => {
            // SAFETY: as above; the drain this call opened releases what the
            // object's fields queue.
            unsafe { release_now(ptr, drop_fields) };
            drain();
        }
    }
}

/// Releases everything queued, then ends the drain.
///
/// The borrow is taken to pop and released before the object is freed, because
/// freeing it queues its children and that borrows again. Overlapping the two
/// would panic.
fn drain() {
    loop {
        let next = next_queued();
        let Some((ptr, glue)) = next else { break };
        // SAFETY: the pointer was queued by a caller that had taken its
        // refcount to zero and had not freed it, so it is still allocated and
        // nothing else refers to it.
        unsafe { release_now(ptr as *mut u8, glue) };
    }
    end_drain();
}

/// The next object in this thread's drain. See [`leave_direct`] for why it is
/// not inlined.
#[inline(never)]
fn next_queued() -> Option<Deferred> {
    PENDING.with(|pending| pending.borrow_mut().as_mut().and_then(|queue| queue.pop()))
}

/// Ends this thread's drain.
#[inline(never)]
fn end_drain() {
    PENDING.with(|pending| *pending.borrow_mut() = None);
}

/// Swaps this thread's drain for `with`, answering what was there.
#[inline(never)]
fn swap_drain(with: Option<Vec<Deferred>>) -> Option<Vec<Deferred>> {
    PENDING.with(|pending| std::mem::replace(&mut *pending.borrow_mut(), with))
}

/// A suspended fiber's release in progress: its drain, and how deep its
/// direct releases were nested when it left.
pub(crate) struct Releasing {
    queue: Option<Vec<Deferred>>,
    depth: u32,
}

/// Swaps this thread's direct-release depth for `with`, answering what was
/// there. Not inlined, as [`swap_drain`] is not.
#[inline(never)]
fn swap_direct(with: u32) -> u32 {
    DIRECT.with(|direct| direct.replace(with))
}

/// Takes this thread's release in progress away, for a fiber about to
/// suspend. See [`crate::coro::suspend`].
///
/// **The depth goes too.** Left behind, a fiber parked [`SHALLOW`] deep would
/// hand the next fiber on its worker a depth that fiber never entered, so
/// every release that fiber made would open a drain -- and the parked fiber
/// would come back on another worker with that worker's depth, and leave
/// taking one off it.
pub(crate) fn take_drain() -> Releasing {
    Releasing { queue: swap_drain(None), depth: swap_direct(0) }
}

/// Puts a resumed fiber's release in progress back, on whichever thread it
/// resumed on.
pub(crate) fn restore_drain(releasing: Releasing) {
    let left = swap_drain(releasing.queue);
    let depth = swap_direct(releasing.depth);
    // A worker resumes fibers from its own loop, never from inside a release,
    // and every fiber that suspended took its release with it -- so there is
    // nothing here to overwrite.
    debug_assert!(left.is_none(), "a fiber resumed onto a worker that already had a drain open");
    debug_assert!(depth == 0, "a fiber resumed onto a worker that was already releasing directly");
}

/// Sets this thread's drain aside for as long as it is alive, and puts it back
/// when it is dropped.
///
/// # S3: a finalizer that silently never ran
///
/// A `drop_fields` callback normally releases children, and they queue behind
/// the drain in progress and cost no stack. A region's release does more than
/// that: it runs the program's finalizers, and a finalizer may block. On the
/// scheduler, blocking suspends the fiber and hands the worker to the next
/// fiber, and until the fix that happened *with the drain still open in the
/// worker's thread-local*. Every last-drop the next fiber made was queued
/// behind a fiber that might never resume. That included its own region, so
/// its finalizer never ran, while the fiber reported itself finished and
/// canceled. A fiber that did resume, on another worker, went on draining that
/// worker's queue, which belonged to somebody else. `p/starve` lost the
/// finalizer with four or more fibers blocked in cleanup, and so did 0.3.0.
///
/// The fix for that is in [`crate::coro::suspend`]: the drain belongs to the
/// fiber, so it leaves with the fiber at the switch ([`take_drain`] /
/// [`restore_drain`]).
///
/// # What this guard is for: a scope that ends inside a finalizer ends there
///
/// Without it, a region or a fiber handle dropped inside a finalizer was
/// queued behind that finalizer. Its finalizers, or its join, happened only
/// after the outer finalizer returned, so after the code that followed the
/// scope and relied on it. If the outer finalizer then blocked, they never
/// happened. That was true on the thread backend too. Inside the guard a
/// nested free starts its own drain and finishes it before it returns, so
/// ordinary graphs are still released iteratively. Only the finalizer's own
/// frees nest one level deeper.
pub(crate) struct Isolated(Option<Vec<Deferred>>);

impl Isolated {
    pub(crate) fn new() -> Isolated {
        Isolated(swap_drain(None))
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        // Anything opened inside has been drained by whoever opened it, since
        // a drain always ends before the `khora_drop` that claimed it returns.
        let left = swap_drain(self.0.take());
        debug_assert!(
            left.is_none(),
            "a drain opened inside an isolated release was still open when the release returned"
        );
    }
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
        // `khora_drop`, and either descend one level (see [`claim`]) or
        // queue themselves behind a drain.
        drop_fields(ptr);
    }

    // SAFETY: `ptr` came from `zeroed` with exactly `layout`, which was
    // rebuilt from the same `field_bytes` the allocation used and the constant
    // alignment; the refcount is zero, so no other reference exists; and the
    // callback has already released everything the object owned.
    unsafe { dealloc(ptr, layout) };

    if crate::counters::counting() {
        LIVE_COUNT.fetch_sub(1, COUNTER_ORDER);
    }
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

    let ptr = zeroed(layout);
    if ptr.is_null() {
        handle_alloc_error(layout);
    }

    // SAFETY: `ptr` is a fresh allocation of `KHORA_HEADER_SIZE + size` bytes
    // aligned to `KHORA_HEADER_ALIGN`, so it is valid and correctly aligned for
    // writing one `KhoraHeader`. Nothing else refers to it yet, so the write
    // cannot race and cannot clobber an initialized field.
    unsafe {
        ptr.cast::<KhoraHeader>().write(KhoraHeader {
            refcount: AtomicU64::new(1 | crate::share::owner_bits()),
            tag,
            field_bytes,
        });
    }

    if crate::counters::counting() {
        ALLOC_COUNT.fetch_add(1, COUNTER_ORDER);
        LIVE_COUNT.fetch_add(1, COUNTER_ORDER);
    }
    // Only while a guarded export call is on this thread's stack, which is a
    // thread-local read and a not-taken branch everywhere else.
    // `crate::contain` has the cost note.
    crate::contain::record(ptr);
    ptr
}

#[cfg(not(target_family = "wasm"))]
unsafe extern "C" {
    /// mimalloc's zeroed allocation, at the alignment every block it hands out
    /// already has.
    fn mi_zalloc(size: usize) -> *mut std::ffi::c_void;
}

/// `layout.size()` zeroed bytes, aligned to [`KHORA_HEADER_ALIGN`], or null.
///
/// **What this prevents: an aligned allocation's detour on every object.**
/// `alloc_zeroed` reaches mimalloc through its *aligned* entry, which checks
/// the alignment is a power of two and then whether the free block it found
/// happens to be aligned, before doing what `mi_zalloc` does. A Khora header
/// needs 8. mimalloc's size classes are whole words, so every block it hands
/// out is at least 8-aligned (16 from 16 bytes up, which its own internal
/// assertion checks), and a Khora object is never smaller than its 16-byte
/// header. So the plain entry is enough;
/// `every_object_is_zeroed_and_aligned_for_its_header` checks both halves
/// over a spread of sizes, on reused memory.
///
/// Sound only because the global allocator *is* mimalloc (`lib.rs`), so the
/// `dealloc` in [`release_now`] and [`release_raw`] reaches `mi_free`, which
/// takes a block from either entry. A build that swapped the global allocator
/// would have to swap this too; wasm, which keeps the default allocator, uses
/// `alloc_zeroed` for exactly that reason.
///
/// Small: the performance round put it at about 2 µs of a Postgres `/db`
/// request, from instruction counts rather than a timing.
fn zeroed(layout: std::alloc::Layout) -> *mut u8 {
    debug_assert!(layout.align() <= KHORA_HEADER_ALIGN && layout.size() >= KHORA_HEADER_SIZE);
    #[cfg(not(target_family = "wasm"))]
    {
        // SAFETY: `mi_zalloc` has no precondition; a null answer is handled
        // by the caller.
        unsafe { mi_zalloc(layout.size()) }.cast::<u8>()
    }
    #[cfg(target_family = "wasm")]
    {
        // SAFETY: `layout` always includes the header, so its size is
        // non-zero, which is `alloc_zeroed`'s one precondition.
        unsafe { alloc_zeroed(layout) }
    }
}

/// Increments an object's refcount. Null is a no-op.
///
/// **A plain add on a local object, in a program built with
/// `KHORA_RC_LOCAL=1`** ([`khora_rc_local`]); the locked add otherwise. See
/// [`classify`] for the test and for why a hand-written extern's call is
/// covered by it.
///
/// # Safety
///
/// `ptr` must be null or a live object from [`khora_alloc`], and the caller
/// must own a reference to it — that is what makes it live for the duration of
/// the call.
#[unsafe(no_mangle)]
// SHARE: counts or frees on the calling fiber; publishes nothing.
pub unsafe extern "C" fn khora_dup(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: by the contract above `ptr` points at a live object, so its
    // header is initialized.
    unsafe {
        let header = ptr.cast::<KhoraHeader>();
        // A static is in read-only memory, so this test is what keeps the add
        // below from faulting on one. See `KHORA_IMMORTAL`.
        match classify(&(*header).refcount) {
            Count::Static => {}
            Count::Local(word) => (*header).refcount.store(word + 1, Ordering::Relaxed),
            // Relaxed is enough: the caller already owns a reference, so the
            // object cannot be freed underneath this, and nothing is being
            // published. Ordering is only needed on the *last* release, where
            // `khora_drop` establishes it.
            Count::Locked => {
                (*header).refcount.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Set once, by the `main` of a program built with `KHORA_RC_LOCAL=1`.
static LOCAL_COUNTS: AtomicBool = AtomicBool::new(false);

/// Records that this program counts local objects without a lock prefix.
///
/// Called by the generated `main` of a program built with `KHORA_RC_LOCAL=1`,
/// before anything is allocated, so the runtime's counts take the same local
/// path as generated code's. **What it prevents: a switch-off build losing
/// its safety net.** Without the switch every count stays locked, in the
/// runtime too, so a program that reaches a `mut` field from two fibers
/// (a field race Khora does not yet refuse everywhere) races fields and
/// never counts. A library build never calls it, so its runtime counts stay
/// locked.
#[unsafe(no_mangle)]
pub extern "C" fn khora_rc_local() {
    LOCAL_COUNTS.store(true, Ordering::Relaxed);
}

/// What a runtime count has to do, read from the word it found.
enum Count {
    /// A static: nothing may write it.
    Static,
    /// Neither flag, and the program counts local objects plainly: the word
    /// as loaded, to be stored back one more or one less.
    Local(u64),
    /// Shared, or any object in a program without the switch: the locked
    /// read-modify-write.
    Locked,
}

/// Reads the count word once and says which of the three counts applies.
///
/// **The same test generated code makes** (`backend/counts.rs`): a relaxed
/// load, the two flag bits, then a plain update of a local object. A local
/// object is reachable from one fiber only: every runtime entry that hands
/// a value to another fiber marks it shared first (`crate::share`), and a
/// fiber moves between workers only through the scheduler's locked queues,
/// so a runtime count of a local object runs on the fiber that owns it --
/// the drop glue releasing a child the owner was holding, a runtime entry
/// releasing what that same fiber handed it -- and never concurrently with
/// another count of it. That covers a hand-written extern too: it can only
/// count an object its own fiber holds a reference to, and a local object
/// is held by one fiber. If a mark was missed, the owner check below is what
/// reports it, in a debug build.
///
/// Relaxed is enough for the load. The immortal bit is in the initializer
/// and never changes, and the shared bit is set before the publishing lock
/// that hands the object over, so a fiber that can reach a shared object
/// sees the bit.
///
/// **A mask of bit 62 for a static, not `>= KHORA_IMMORTAL`.** Bit 63 is
/// [`KHORA_SHARED`], and the compare would read every shared object as a
/// static: never counted, so never freed.
///
/// In a debug build this is also where the owner check runs, for every
/// runtime count of an object that is neither shared nor static.
#[inline(always)]
fn classify(count: &AtomicU64) -> Count {
    let word = count.load(Ordering::Relaxed);
    if word & KHORA_IMMORTAL != 0 {
        return Count::Static;
    }
    if crate::share::checking() {
        crate::share::khora_rc_check(word);
    }
    if word & KHORA_SHARED == 0 && LOCAL_COUNTS.load(Ordering::Relaxed) {
        Count::Local(word)
    } else {
        Count::Locked
    }
}

/// Takes one reference off a count word, answering the previous word.
///
/// The decrement of [`khora_drop`] and [`khora_drop_reuse`]: plain and
/// relaxed on a local object, the locked release subtract otherwise, and
/// `None` for a static. The caller takes the acquire fence on the last
/// reference, as before; after a local decrement it orders nothing a
/// program can see, since every count of the object was made by this
/// fiber, and on x86 it is no instruction at all.
#[inline(always)]
fn decrement(count: &AtomicU64) -> Option<u64> {
    match classify(count) {
        Count::Static => None,
        Count::Local(word) => {
            count.store(word.wrapping_sub(1), Ordering::Relaxed);
            Some(word)
        }
        Count::Locked => {
            // Release, so that everything this thread did to the object
            // happens before whichever thread performs the final decrement
            // sees the count reach zero. The matching acquire is the fence
            // the caller takes on the last reference.
            Some(count.fetch_sub(1, Ordering::Release))
        }
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
/// A plain subtract on a local object when the program was built with
/// `KHORA_RC_LOCAL=1`, the locked one otherwise; [`classify`] has why.
///
/// **An object with no field routine is freed at once, not queued.** It has
/// no children to release, so nothing can recurse; claiming the drain for it
/// cost three thread-local calls per string freed.
///
/// # Safety
///
/// `ptr` must be null or a live object from [`khora_alloc`], and the caller
/// must own the reference being released. `drop_fields` must match the object's
/// actual field layout — the runtime cannot check that, since it does not know
/// what the fields mean.
#[unsafe(no_mangle)]
// SHARE: counts or frees on the calling fiber; publishes nothing.
pub unsafe extern "C" fn khora_drop(ptr: *mut u8, drop_fields: Option<extern "C" fn(*mut u8)>) {
    if ptr.is_null() {
        return;
    }
    // Inside a `khora_share` walk the drop glue is being run to *find* the
    // children, not to release them. See `crate::share`.
    if crate::share::walked(ptr, drop_fields) {
        return;
    }
    let header = ptr.cast::<KhoraHeader>();

    // SAFETY: `ptr` points at a live object per the contract above, so its
    // header is initialized and valid to read and write.
    let Some(previous) = decrement(unsafe { &(*header).refcount }) else { return };
    let refcount = previous & KHORA_COUNT_MASK;
    // A decrement from a count of zero borrows from the bits above the
    // count, leaving the debug owner or a flag one less. That word is never
    // read again: the abort below is next.
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

    if drop_fields.is_none() {
        // SAFETY: this thread took the count to zero, so it holds the only
        // claim, and with no field routine nothing can be queued behind it.
        unsafe { release_now(ptr, None) };
        return;
    }

    // Directly while shallow, else queued behind the drain or opening one,
    // so a graph costs no more stack than [`SHALLOW`] levels. See [`claim`].
    //
    // SAFETY: this thread took the count to zero and holds the only claim.
    unsafe { release_last(ptr, drop_fields) };
}

/// Releases a reference and, if it was the last, keeps the memory.
///
/// The first half of reuse. This is [`khora_drop`] with one difference: on the
/// last reference it runs the field-dropping routine and returns the object's
/// memory **without freeing it**, so the caller can build the next object in
/// the same cell. On any other outcome — a shared object, a null pointer — it
/// behaves exactly as `khora_drop` does and returns null.
///
/// **A token only where the masked previous count was 1**, with the local
/// path as with the shared one. On a shared object the locked subtract and
/// the acquire fence after it order every other holder's release before
/// this. On a local object every other count was made by this same fiber,
/// in program order, whether by the relaxed store of generated code or a
/// locked operation here, so the subtract reads all of them. Either way the
/// caller held the one reference left and no other fiber can reach the cell.
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
// SHARE: counts or frees on the calling fiber; publishes nothing.
pub unsafe extern "C" fn khora_drop_reuse(
    ptr: *mut u8,
    drop_fields: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    let header = ptr.cast::<KhoraHeader>();

    // **A static is never unique.** Its count is huge, so the decrement below
    // would already answer "somebody else holds it". The test is here for
    // what a write would do: the static is in read-only memory. See
    // `KHORA_IMMORTAL`.
    //
    // SAFETY: live per the contract, so the header is initialized.
    let Some(previous) = decrement(unsafe { &(*header).refcount }) else {
        return std::ptr::null_mut();
    };
    let refcount = previous & KHORA_COUNT_MASK;
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

    if crate::counters::counting() {
        LIVE_COUNT.fetch_sub(1, COUNTER_ORDER);
    }
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
/// **The header written is local: no shared bit, this fiber as owner.** Right
/// with the local path as well as without it. A token means the calling
/// fiber held the cell's only reference (see [`khora_drop_reuse`]), so no
/// other fiber can count what is built here, and a plain count of it by this
/// fiber is sound until a runtime entry marks it again.
///
/// # Safety
///
/// `token` must be null or memory from [`khora_drop_reuse`] that has not been
/// spent, and `size` must not exceed [`MAX_FIELD_BYTES`].
#[unsafe(no_mangle)]
// SHARE: counts or frees on the calling fiber; publishes nothing.
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

    // **The fresh header is local, with this fiber as its owner.** A token
    // exists only where `khora_drop_reuse` took the count from 1 to 0, so the
    // caller held the one reference there was, after the acquire fence: no
    // other fiber can reach the cell. So a cell that had crossed and came back
    // unique is local again, and that is true, not merely allowed. Keeping
    // the shared bit would send a local object down the shared path for the
    // rest of its life; keeping the old owner would make the owner check trap
    // on the fiber that now rightly holds it.
    //
    // SAFETY: as above. The fields are about to be written by the caller, which
    // is the same contract `khora_alloc` leaves them under — except that they
    // are not zeroed here, because the caller writes every one of them before
    // anything can read them. `khora_alloc`'s zeroing exists for the window
    // between allocation and the first store, and a reused cell's window is
    // covered by `drop_fields` having already run.
    unsafe {
        token.cast::<KhoraHeader>().write(KhoraHeader {
            refcount: AtomicU64::new(1 | crate::share::owner_bits()),
            tag,
            field_bytes,
        });
    }
    if crate::counters::counting() {
        LIVE_COUNT.fetch_add(1, COUNTER_ORDER);
    }
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
/// with [`Ordering::Release`], `previous` must be what that decrement returned
/// (flags and all, or masked to the count: both read the same here), and
/// `drop_fields` must match the object's layout.
#[unsafe(no_mangle)]
// SHARE: counts or frees on the calling fiber; publishes nothing.
pub unsafe extern "C" fn khora_drop_last(
    ptr: *mut u8,
    drop_fields: Option<extern "C" fn(*mut u8)>,
    previous: u64,
) {
    if previous & KHORA_COUNT_MASK == 0 {
        fatal("drop of an object whose refcount is already zero (double free, or a missing dup)");
    }
    // Generated code tests only the low 32 bits of the count, so a count of
    // 2^32 or more whose low word is 0 or 1 arrives here although other
    // references remain. The whole count answers it.
    if previous & KHORA_COUNT_MASK > 1 {
        return;
    }

    // Acquire, pairing with every other thread's release, so their writes are
    // visible before the fields are read and the memory is freed. The matching
    // release is the caller's decrement. On x86 and AArch64 an acquire fence
    // after a locked subtract costs nothing; after a local one it is ordering
    // for the compiler only.
    std::sync::atomic::fence(Ordering::Acquire);

    // A leaf is freed at once, as in `khora_drop`.
    if drop_fields.is_none() {
        // SAFETY: this thread took the count to zero and holds the only
        // claim; with no field routine nothing can queue.
        unsafe { release_now(ptr, None) };
        return;
    }

    // As `khora_drop`, and this is the one generated code calls.
    //
    // SAFETY: still allocated — this thread took the count to zero and holds
    // the only claim on it.
    unsafe { release_last(ptr, drop_fields) };
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
// SHARE: counts or frees on the calling fiber; publishes nothing.
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
/// A static answers 2^40, without its immortal bit: the answer is a count,
/// and the flag is not part of one. Any other object answers its count
/// without the shared bit or the debug owner, for the same reason.
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
    let word = unsafe { (*ptr.cast::<KhoraHeader>()).refcount.load(Ordering::Relaxed) };
    if word & KHORA_IMMORTAL != 0 {
        return word & !KHORA_IMMORTAL;
    }
    word & KHORA_COUNT_MASK
}

/// Whether an object's shared bit is set. Null reads as false.
///
/// Exists for tests, as [`khora_refcount`] does: a test that cannot see the
/// bit can show that a missing mark crashed, but not that a present one is
/// there.
///
/// # Safety
///
/// `ptr` must be null or a live object from [`khora_alloc`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_is_shared(ptr: *const u8) -> bool {
    if ptr.is_null() {
        return false;
    }
    // SAFETY: `ptr` points at a live object per the contract above, so its
    // header is initialized and valid to read.
    let word = unsafe { (*ptr.cast::<KhoraHeader>()).refcount.load(Ordering::Relaxed) };
    word & KHORA_SHARED != 0
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
    // SAFETY: `ptr` came from `zeroed` with exactly this layout, and the
    // caller guarantees nothing else reaches it.
    unsafe { dealloc(ptr, layout) };
    if crate::counters::counting() {
        LIVE_COUNT.fetch_sub(1, COUNTER_ORDER);
    }
}

#[cfg(test)]
mod tests {
    //! Releasing directly while shallow ([`SHALLOW`]), against the three
    //! things the queue exists for: stack bounded by depth, a drain that
    //! belongs to its fiber (S3), and finalizers that free more.

    use super::*;
    use crate::region::{khora_region_defer, khora_region_open, release_shim};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    /// A link: field 0 is a child object, field 1 its drop glue (or null).
    /// The shape every generated glue has -- release each field through
    /// `khora_drop` -- with the child's glue carried in the object so one
    /// routine serves any graph.
    fn link(child: *mut u8, glue: Option<extern "C" fn(*mut u8)>) -> *mut u8 {
        let object = khora_alloc(16, 0);
        // SAFETY: a fresh object with two words of fields, reached by nothing
        // else yet.
        unsafe {
            let fields = object.add(KHORA_FIELD_OFFSET);
            fields.cast::<*mut u8>().write(child);
            fields.add(8).cast::<Option<extern "C" fn(*mut u8)>>().write(glue);
        }
        object
    }

    /// Releases a [`link`]'s child.
    fn release_child(object: *mut u8) {
        // SAFETY: called only on a live link, from its own glue, whose
        // reference to the child is being given up here.
        unsafe {
            let fields = object.add(KHORA_FIELD_OFFSET);
            let child = fields.cast::<*mut u8>().read();
            let glue = fields.add(8).cast::<Option<extern "C" fn(*mut u8)>>().read();
            khora_drop(child, glue);
        }
    }

    /// A list of `n` links, each one's child the next, counting releases in
    /// `glue`'s counter.
    fn list(n: usize, glue: extern "C" fn(*mut u8)) -> *mut u8 {
        let mut head = std::ptr::null_mut();
        for _ in 0..n {
            head = link(head, Some(glue));
        }
        head
    }

    /// A closure object of type `() -> ()`, as `crate::region`'s tests build
    /// one: a single field holding the code pointer.
    fn closure(code: extern "C" fn(*mut u8)) -> *mut u8 {
        let object = khora_alloc(8, 0);
        // SAFETY: one word of fields, freshly allocated.
        unsafe { object.add(KHORA_FIELD_OFFSET).cast::<extern "C" fn(*mut u8)>().write(code) };
        object
    }

    /// A region with one finalizer, as a link's child.
    fn region_running(finalizer: extern "C" fn(*mut u8)) -> *mut u8 {
        let region = khora_region_open();
        // SAFETY: a live region and a live closure with no captures.
        unsafe { khora_region_defer(region, closure(finalizer), None, None) };
        region
    }

    /// Waits up to five seconds for `done`, so a red run fails rather than
    /// hangs.
    fn eventually(done: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        true
    }

    /// **A list 100,000 deep is freed without running out of stack**, on a
    /// test thread's stack and on a fiber's. What this guards: a depth
    /// bound that stopped bounding. Direct release recurses three frames
    /// per level, so without the fall-back to the drain at [`SHALLOW`] this
    /// is 300,000 frames and the process dies.
    #[test]
    fn a_list_a_hundred_thousand_deep_is_freed_without_overflowing() {
        static FREED: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn counted(object: *mut u8) {
            FREED.fetch_add(1, Ordering::Relaxed);
            release_child(object);
        }
        const DEEP: usize = 100_000;

        let head = list(DEEP, counted);
        // SAFETY: the list's only reference.
        unsafe { khora_drop(head, Some(counted)) };
        assert_eq!(FREED.load(Ordering::Relaxed), DEEP, "every link of the list was released");

        let pool = crate::scheduler::Scheduler::new(1);
        pool.spawn(crate::coro::Task::new(|| {
            let head = list(DEEP, counted);
            // SAFETY: as above.
            unsafe { khora_drop(head, Some(counted)) };
        }));
        pool.drain();
        assert_eq!(FREED.load(Ordering::Relaxed), 2 * DEEP, "and again on a fiber's stack");
    }

    /// **A drain one fiber opened is not taken over by another** (S3), nor
    /// is how deep its direct releases were.
    ///
    /// Fiber A releases a list longer than [`SHALLOW`]: the first 24 links
    /// go directly, the 25th opens a drain, and a release queued behind it
    /// parks with a sibling still queued. Fiber B then runs on the same
    /// worker. Its releases must see neither A's queue nor A's depth: its
    /// shallow graph is released directly (its child freed inside its own
    /// glue, not after it), and its region's finalizer runs when the region
    /// ends. Then A resumes and finishes its own queue.
    ///
    /// A's sibling released before A parked means there was no bound at all.
    #[test]
    fn a_drain_opened_by_one_fiber_is_not_taken_over_by_another() {
        use crate::coro::Task;
        use crate::scheduler::{park_current, waker_for_current, Scheduler, Waker};

        static PARKED: Mutex<Option<Waker>> = Mutex::new(None);
        static SIBLING_FREED: AtomicUsize = AtomicUsize::new(0);
        static SIBLING_AT_PARK: AtomicUsize = AtomicUsize::new(usize::MAX);
        static A_LINKS: AtomicUsize = AtomicUsize::new(0);
        static IN_PARENT: AtomicUsize = AtomicUsize::new(0);
        static CHILD_INSIDE: AtomicUsize = AtomicUsize::new(usize::MAX);
        static FINALIZED: AtomicUsize = AtomicUsize::new(0);
        static B_SAW: AtomicUsize = AtomicUsize::new(usize::MAX);

        extern "C" fn a_link(object: *mut u8) {
            A_LINKS.fetch_add(1, Ordering::SeqCst);
            release_child(object);
        }
        extern "C" fn parks(_object: *mut u8) {
            SIBLING_AT_PARK.store(SIBLING_FREED.load(Ordering::SeqCst), Ordering::SeqCst);
            *PARKED.lock().unwrap() = waker_for_current();
            park_current();
        }
        extern "C" fn sibling(_object: *mut u8) {
            SIBLING_FREED.fetch_add(1, Ordering::SeqCst);
        }
        /// Releases the sibling, then the parker. Behind a drain the two
        /// are queued, and the parker, pushed last, is popped first.
        extern "C" fn pair(object: *mut u8) {
            A_LINKS.fetch_add(1, Ordering::SeqCst);
            release_child(object);
            // SAFETY: a pair is a link with a second link as its child's
            // sibling: see how the test builds it.
            unsafe {
                let parker = object.add(KHORA_FIELD_OFFSET + 16).cast::<*mut u8>().read();
                khora_drop(parker, Some(parks));
            }
        }
        extern "C" fn parent(object: *mut u8) {
            IN_PARENT.store(1, Ordering::SeqCst);
            release_child(object);
            IN_PARENT.store(0, Ordering::SeqCst);
        }
        extern "C" fn child(_object: *mut u8) {
            CHILD_INSIDE.store(IN_PARENT.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        extern "C" fn finalizer(_closure: *mut u8) {
            FINALIZED.fetch_add(1, Ordering::SeqCst);
        }

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(|| {
            // The pair: a link to the sibling, with the parker in a third
            // field.
            let pair_object = khora_alloc(24, 0);
            // SAFETY: a fresh object with three words of fields.
            unsafe {
                let fields = pair_object.add(KHORA_FIELD_OFFSET);
                fields.cast::<*mut u8>().write(khora_alloc(0, 0));
                fields.add(8).cast::<Option<extern "C" fn(*mut u8)>>().write(Some(sibling));
                fields.add(16).cast::<*mut u8>().write(khora_alloc(0, 0));
            }
            let mut head = pair_object;
            let mut glue: extern "C" fn(*mut u8) = pair;
            for _ in 0..(SHALLOW as usize + 6) {
                head = link(head, Some(glue));
                glue = a_link;
            }
            // SAFETY: the graph's only reference.
            unsafe { khora_drop(head, Some(glue)) };
        }));
        assert!(eventually(|| PARKED.lock().unwrap().is_some()), "fiber A never parked");
        assert_eq!(
            SIBLING_AT_PARK.load(Ordering::SeqCst),
            0,
            "the sibling was released before its queued neighbor parked: the release never \
             fell back to the drain, so it was not bounded"
        );

        pool.spawn(Task::new(|| {
            let object = link(link(std::ptr::null_mut(), Some(child)), Some(child));
            // SAFETY: the only reference; `parent` releases the link to `child`.
            unsafe { khora_drop(object, Some(parent)) };
            let holder = link(region_running(finalizer), Some(release_shim));
            // SAFETY: the only reference.
            unsafe { khora_drop(holder, Some(a_link_free)) };
            B_SAW.store(FINALIZED.load(Ordering::SeqCst), Ordering::SeqCst);
        }));
        extern "C" fn a_link_free(object: *mut u8) {
            release_child(object);
        }
        assert!(eventually(|| B_SAW.load(Ordering::SeqCst) != usize::MAX), "fiber B never finished");
        assert_eq!(
            CHILD_INSIDE.load(Ordering::SeqCst),
            1,
            "fiber B's shallow graph was not released directly: it inherited A's queue or depth"
        );
        assert_eq!(
            B_SAW.load(Ordering::SeqCst),
            1,
            "fiber B's region ended and its finalizer had not run: its release was queued behind A"
        );

        if let Some(waker) = PARKED.lock().unwrap().take() {
            waker.wake();
        }
        pool.drain();
        assert_eq!(SIBLING_FREED.load(Ordering::SeqCst), 1, "A's own drain finished when A resumed");
        assert_eq!(A_LINKS.load(Ordering::SeqCst), SHALLOW as usize + 7, "every link of A's graph was released");
    }

    /// **A finalizer that frees more during a shallow release works**: a
    /// deep list is freed in bounded stack, a region it ends runs its
    /// finalizer at once, and everything is freed exactly once.
    ///
    /// The region is three links down, so it is released directly with the
    /// depth at three and no drain open, and its finalizer's frees start
    /// from there.
    #[test]
    fn a_finalizer_frees_more_during_a_shallow_release() {
        static LIST_FREED: AtomicUsize = AtomicUsize::new(0);
        static INNER_RAN: AtomicUsize = AtomicUsize::new(0);
        static OUTER_SAW: AtomicUsize = AtomicUsize::new(usize::MAX);
        static LINKS: AtomicUsize = AtomicUsize::new(0);
        const DEEP: usize = 100_000;

        extern "C" fn counted(object: *mut u8) {
            LIST_FREED.fetch_add(1, Ordering::SeqCst);
            release_child(object);
        }
        extern "C" fn counted_link(object: *mut u8) {
            LINKS.fetch_add(1, Ordering::SeqCst);
            release_child(object);
        }
        extern "C" fn inner(_closure: *mut u8) {
            INNER_RAN.fetch_add(1, Ordering::SeqCst);
        }
        extern "C" fn outer(_closure: *mut u8) {
            let head = list(DEEP, counted);
            // SAFETY: the list's only reference.
            unsafe { khora_drop(head, Some(counted)) };
            let holder = link(region_running(inner), Some(release_shim));
            // SAFETY: the only reference.
            unsafe { khora_drop(holder, Some(counted_link)) };
            OUTER_SAW.store(INNER_RAN.load(Ordering::SeqCst), Ordering::SeqCst);
        }

        let mut head = region_running(outer);
        let mut glue: extern "C" fn(*mut u8) = release_shim;
        for _ in 0..3 {
            head = link(head, Some(glue));
            glue = counted_link;
        }
        // SAFETY: the graph's only reference.
        unsafe { khora_drop(head, Some(glue)) };

        assert_eq!(LIST_FREED.load(Ordering::SeqCst), DEEP, "the finalizer's list was freed");
        assert_eq!(OUTER_SAW.load(Ordering::SeqCst), 1, "the region ended in the finalizer ran its finalizer there");
        assert_eq!(LINKS.load(Ordering::SeqCst), 4, "every link was released once");
    }
}
