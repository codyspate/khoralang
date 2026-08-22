//! The Khora runtime: reference-counted allocation and intrinsics.
//!
//! Every executable the compiler produces links against this archive. It is
//! deliberately tiny — there is no collector, no scheduler and no object graph
//! walker here, because Perceus (`khora-perceus`, roadmap 2.3) inserts precise
//! `dup`/`drop` at compile time. All this crate owns is the heap layout those
//! instructions agree on, plus the handful of intrinsics that have to exist as
//! machine code rather than as generated IR.
//!
//! # The heap object layout
//!
//! This is the contract between the code generator and the runtime. Both sides
//! reproduce it from these numbers; if they disagree, nothing crashes at the
//! point of disagreement, which is why it is written down here rather than
//! inferred from the Rust struct on either side.
//!
//! On a 64-bit target:
//!
//! | Offset | Size | Field | Who reads it |
//! | --- | --- | --- | --- |
//! | 0 | 8 | `refcount: usize` | runtime (`khora_dup`, `khora_drop`) |
//! | 8 | 4 | `tag: u32` | generated code, to switch on an ADT variant |
//! | 12 | 4 | `field_bytes: u32` | runtime, to rebuild the allocation layout |
//! | 16 | *n* | the object's fields | generated code |
//!
//! So [`KHORA_HEADER_SIZE`] is 16, [`KHORA_HEADER_ALIGN`] is 8, and **fields
//! begin at offset [`KHORA_FIELD_OFFSET`] = 16 from the pointer `khora_alloc`
//! returned**. Every pointer that crosses this API — the return of
//! [`khora_alloc`], the argument to [`khora_dup`] and [`khora_drop`], the
//! argument handed to a `drop_fields` callback — is a pointer to the *header*,
//! never an interior pointer to the fields. Generated code adds
//! `KHORA_FIELD_OFFSET` itself when it wants a field, and the runtime never
//! sees the result.
//!
//! The constants are `pub`, so `khora-codegen-llvm` can emit GEPs against them
//! instead of hard-coding 16 in two places that then drift apart.
//!
//! # Why `field_bytes` lives in what would otherwise be padding
//!
//! `refcount` and `tag` leave four bytes of tail padding, because the struct
//! has to be 8-aligned for `refcount`. [`khora_drop`] needs that space: it is
//! handed nothing but a pointer, and `std::alloc::dealloc` requires the
//! *identical* `Layout` that allocated the block — a mismatch is undefined
//! behavior, not a leak. Alignment is a constant, so the only unknown is the
//! size, and storing it in padding makes the layout reconstructible for free.
//!
//! The cost is that one object's fields cannot exceed [`MAX_FIELD_BYTES`]
//! (`u32::MAX`). [`khora_alloc`] aborts rather than truncating if that is ever
//! exceeded; a single ADT node that large is not a thing Khora can currently
//! express.
//!
//! # Alignment
//!
//! The whole allocation is aligned to [`KHORA_HEADER_ALIGN`] (8), and the field
//! area starts at a multiple of 8, so every field is 8-aligned. That is exactly
//! enough for phase 2's uniform boxed representation, where every field is a
//! machine word — an `Int`, a `Bool` widened to a word, or a pointer. A field
//! type needing stronger alignment (a 16-byte vector, `u128`) would require
//! raising this constant *and* the code generator's view of it together, since
//! `Layout` is checked on the way out as well as the way in.
//!
//! # The symbols, as C sees them
//!
//! ```c
//! void *khora_alloc(size_t size, uint32_t tag);
//! void  khora_dup(void *object);
//! void  khora_drop(void *object, void (*drop_fields)(void *object));
//! size_t khora_refcount(const void *object);
//! void  khora_print_int(int64_t value);
//! void  khora_print_bool(_Bool value);
//! void  khora_print_str(const uint8_t *bytes, size_t len);
//! _Bool khora_str_eq(const uint8_t *a, size_t a_len,
//!                    const uint8_t *b, size_t b_len);
//! size_t khora_alloc_count(void);
//! size_t khora_live_count(void);
//! void  khora_reset_counters(void);
//! ```
//!
//! Two of those have a sharper edge than they look:
//!
//! - `drop_fields` is `Option<extern "C" fn(*mut u8)>` on the Rust side, which
//!   is one pointer wide with `None` represented as null. An object that owns
//!   no references is dropped by passing a **null function pointer**, not by
//!   calling a different entry point.
//! - `khora_print_bool` takes a C `_Bool`: one byte holding exactly 0 or 1.
//!   Generated code must zero-extend its `i1` to `i8`; any other bit pattern in
//!   that byte is undefined behavior rather than a merely surprising result.
//!
//! # Linking
//!
//! The archive carries Rust's `std` with it, so the link line needs the
//! platform's system libraries too. Rather than hard-coding a list that will
//! rot, ask the compiler that produced the archive:
//!
//! ```text
//! cargo rustc -p khora-rt --release --crate-type staticlib -- --print native-static-libs
//! ```
//!
//! # Threads
//!
//! Refcounts are non-atomic. Perceus's ownership discipline is single-threaded
//! by construction, atomic RC is a large tax on the hot path, and phase 2 has
//! no concurrency at all. The *allocation counters* are atomic, because they
//! are process-global statistics that a threaded test harness may touch from
//! several threads at once.

#![deny(missing_docs, unsafe_op_in_unsafe_fn)]

use std::alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The header every Khora heap object begins with.
///
/// `#[repr(C)]` because the code generator reproduces this layout from the
/// documented offsets; Rust's default representation is free to reorder fields
/// and would silently break that agreement.
#[repr(C)]
#[derive(Debug)]
pub struct KhoraHeader {
    /// Number of live references. An object is freed when this reaches zero.
    pub refcount: usize,
    /// Which variant of its ADT the object is. Opaque to the runtime.
    pub tag: u32,
    /// Bytes of fields following the header — the `size` passed to
    /// [`khora_alloc`], stored so [`khora_drop`] can rebuild the `Layout`.
    pub field_bytes: u32,
}

/// Size of [`KhoraHeader`] in bytes: 16 on a 64-bit target.
pub const KHORA_HEADER_SIZE: usize = std::mem::size_of::<KhoraHeader>();

/// Alignment of every Khora heap allocation: 8 on a 64-bit target.
pub const KHORA_HEADER_ALIGN: usize = std::mem::align_of::<KhoraHeader>();

/// Offset from an object pointer to its first field.
///
/// Fields are packed immediately after the header, so this is the header size.
/// Generated code adds it to reach field storage; see the module documentation.
pub const KHORA_FIELD_OFFSET: usize = KHORA_HEADER_SIZE;

/// Largest field area a single object may have.
///
/// Bounded because the size is stored in the header's padding word — see the
/// module documentation.
pub const MAX_FIELD_BYTES: usize = u32::MAX as usize;

// The documented table is the contract, so pin the numbers it quotes. A change
// to `KhoraHeader` that moved a field would otherwise be caught only by the
// code generator producing wrong programs.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(KHORA_HEADER_SIZE == 16);
    assert!(KHORA_HEADER_ALIGN == 8);
    assert!(KHORA_FIELD_OFFSET == 16);
};

/// The layout of an object whose field area is `field_bytes` long.
///
/// The single place the layout is computed, so allocation and deallocation
/// cannot disagree about it.
fn object_layout(field_bytes: u32) -> Layout {
    let size = KHORA_HEADER_SIZE + field_bytes as usize;
    match Layout::from_size_align(size, KHORA_HEADER_ALIGN) {
        Ok(layout) => layout,
        // Unreachable: `size` is at most `u32::MAX + 16` and the alignment is a
        // valid power of two, so the only failure mode `Layout` has is out of
        // reach. Aborting beats an `unwrap` panic, which would unwind out of an
        // `extern "C"` frame.
        Err(_) => fatal("object layout overflowed"),
    }
}

/// Reports a violated runtime invariant and terminates the process.
///
/// Aborts rather than panicking: these functions are called from generated
/// machine code across a C ABI boundary, where unwinding is not something the
/// caller has frames for. A wrong `dup`/`drop` pairing corrupts the heap
/// silently, so failing loudly at the first detected inconsistency is worth far
/// more than limping on.
fn fatal(message: &str) -> ! {
    let _ = writeln!(std::io::stderr(), "khora runtime: {message}");
    std::process::abort()
}

// ---------------------------------------------------------------------------
// Allocation accounting
// ---------------------------------------------------------------------------

/// Total objects allocated since the process started or the counters were reset.
static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Objects allocated and not yet freed.
static LIVE_COUNT: AtomicUsize = AtomicUsize::new(0);

// `Relaxed` throughout: the counters publish no other memory, so nothing is
// ordered against them. A test that counts across threads establishes its
// happens-before by joining them, which is a far stronger edge than any
// ordering on the counter itself would give.
const COUNTER_ORDER: Ordering = Ordering::Relaxed;

/// Number of objects [`khora_alloc`] has produced since the last
/// [`khora_reset_counters`].
#[unsafe(no_mangle)]
pub extern "C" fn khora_alloc_count() -> usize {
    ALLOC_COUNT.load(COUNTER_ORDER)
}

/// Number of objects allocated and not yet freed.
///
/// This is the leak check the roadmap's phase 2 exit criterion is written
/// against: run a compiled program to completion and this must be zero.
#[unsafe(no_mangle)]
pub extern "C" fn khora_live_count() -> usize {
    LIVE_COUNT.load(COUNTER_ORDER)
}

/// Resets both counters to zero, for test isolation.
///
/// Call it when nothing is live. Resetting while objects are still allocated
/// leaves the live count describing a different population from the one that
/// will later be freed, and it will wrap when those frees arrive.
#[unsafe(no_mangle)]
pub extern "C" fn khora_reset_counters() {
    ALLOC_COUNT.store(0, COUNTER_ORDER);
    LIVE_COUNT.store(0, COUNTER_ORDER);
}

// ---------------------------------------------------------------------------
// Allocation and reference counting
// ---------------------------------------------------------------------------

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
pub extern "C" fn khora_alloc(size: usize, tag: u32) -> *mut u8 {
    if size > MAX_FIELD_BYTES {
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
        ptr.cast::<KhoraHeader>().write(KhoraHeader { refcount: 1, tag, field_bytes });
    }

    ALLOC_COUNT.fetch_add(1, COUNTER_ORDER);
    LIVE_COUNT.fetch_add(1, COUNTER_ORDER);
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
    // header is initialized and uniquely reachable by this thread (refcounts
    // are non-atomic, see the module documentation).
    unsafe {
        let header = ptr.cast::<KhoraHeader>();
        (*header).refcount += 1;
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

    // SAFETY: `ptr` points at a live object per the contract above, so its
    // header is initialized and valid to read and write.
    let refcount = unsafe { (*header).refcount };
    if refcount == 0 {
        fatal("drop of an object whose refcount is already zero (double free, or a missing dup)");
    }
    if refcount > 1 {
        // SAFETY: as above; we still hold a reference, so the object outlives
        // this write.
        unsafe { (*header).refcount = refcount - 1 };
        return;
    }

    // Last reference. Read the layout out of the header *before* running the
    // callback, so a callback that scribbles on the header cannot make the
    // deallocation use a layout that differs from the allocation's.
    //
    // SAFETY: as above; the object is still allocated at this point.
    let layout = object_layout(unsafe { (*header).field_bytes });

    if let Some(drop_fields) = drop_fields {
        // The callback recursively drops the children this object owns. It is
        // called with the header pointer, matching every other pointer in this
        // API; it offsets to the fields itself.
        drop_fields(ptr);
    }

    // SAFETY: `ptr` came from `alloc_zeroed` with exactly `layout`, which was
    // rebuilt from the same `field_bytes` the allocation used and the constant
    // alignment; the refcount is zero, so no other reference exists; and the
    // callback has already released everything the object owned.
    unsafe { dealloc(ptr, layout) };

    LIVE_COUNT.fetch_sub(1, COUNTER_ORDER);
}

/// Whether two strings hold the same bytes.
///
/// Takes bytes and lengths rather than object pointers, matching
/// [`khora_print_str`]: the header layout is the code generator's business, and
/// the runtime stays a function of the data it is handed.
///
/// A null pointer is only valid with a zero length, which is how an
/// uninitialized slot compares equal to `""` and to itself.
///
/// # Safety
///
/// Each pointer must be null or address `len` initialized bytes that stay live
/// and unmodified for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_str_eq(
    a: *const u8,
    a_len: usize,
    b: *const u8,
    b_len: usize,
) -> bool {
    if a_len != b_len {
        return false;
    }
    if a_len == 0 {
        return true;
    }
    if a.is_null() || b.is_null() {
        fatal("string comparison of a null pointer with a non-zero length");
    }
    // SAFETY: the caller guarantees `len` initialized bytes at each pointer,
    // live and unmodified for this call, and each length came from an
    // allocation so is far below `isize::MAX`.
    unsafe { std::slice::from_raw_parts(a, a_len) == std::slice::from_raw_parts(b, b_len) }
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
pub unsafe extern "C" fn khora_refcount(ptr: *const u8) -> usize {
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: `ptr` points at a live object per the contract above, so its
    // header is initialized and valid to read.
    unsafe { (*ptr.cast::<KhoraHeader>()).refcount }
}

// ---------------------------------------------------------------------------
// Printing
// ---------------------------------------------------------------------------
//
// Each of these takes the lock once and emits the value and its newline
// together, so two prints cannot interleave halfway through a line. Rust's
// stdout is line buffered, so the trailing newline also flushes: generated
// programs exit through a C `main` without running Rust's shutdown, and
// anything still sitting in a buffer at that point would simply be lost.
//
// Write errors are discarded. A closed or full stdout is not a condition a
// Khora program can react to, and turning it into an abort would make `print`
// the most dangerous statement in the language.

/// Prints an `Int` and a newline to stdout.
#[unsafe(no_mangle)]
pub extern "C" fn khora_print_int(value: i64) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
}

/// Prints a `Bool` and a newline to stdout, spelled as Khora spells it.
///
/// Takes a C `_Bool`, so generated code must pass a byte that is exactly 0 or
/// 1 — see the module documentation.
#[unsafe(no_mangle)]
pub extern "C" fn khora_print_bool(value: bool) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(if value { b"true\n" } else { b"false\n" });
}

/// Prints `len` bytes from `ptr`, then a newline, to stdout.
///
/// Takes a raw pointer and a length rather than a heap object, so the code
/// generator can pass a string literal in `.rodata` directly, without boxing
/// it. The bytes are written through unvalidated: Khora `String`s are UTF-8 by
/// construction, so validating here would charge every print for a property the
/// type system already guarantees.
///
/// # Safety
///
/// If `len` is non-zero, `ptr` must point at `len` initialized bytes that stay
/// valid and unmodified for the duration of the call. A zero `len` ignores
/// `ptr` entirely, so the empty string may be passed as null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_print_str(ptr: *const u8, len: usize) {
    let mut out = std::io::stdout().lock();
    if len != 0 {
        if ptr.is_null() {
            fatal("print of a null string with a non-zero length");
        }
        // SAFETY: the caller guarantees `len` initialized bytes at `ptr`, live
        // and unmodified for this call, and `len` is far below `isize::MAX`
        // because it came from an allocation.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let _ = out.write_all(bytes);
    }
    let _ = out.write_all(b"\n");
}

// --- regions ---------------------------------------------------------------

/// One deferred finalizer: the closure to call, and the glue to release it
/// with.
///
/// The glue travels with the closure because the runtime cannot work it out.
/// A closure's drop routine is *generated* — one shared routine switching on
/// the site tag — so the only thing that knows the pointer is the code that
/// built the closure, which is exactly the code that defers it.
#[repr(C)]
struct Finalizer {
    closure: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
}

/// A region's finalizers, in the order they were deferred.
///
/// Held Rust-side rather than as a Khora list because deferring *grows* it, and
/// nothing in Khora can grow a value in place. The Khora object is a handle:
/// one field holding a pointer to this.
type Finalizers = Vec<Finalizer>;

/// The tag every region object carries. Regions are not an ADT, so no variant
/// index competes for it.
const REGION_TAG: u32 = 0;

/// The region that ends when the program does.
///
/// One per program, created on first use and released by the generated entry
/// point after `main` returns — on the failing path as well as the ordinary
/// one, because a finalizer that only runs when nothing went wrong is not a
/// finalizer.
static mut ROOT: *mut u8 = std::ptr::null_mut();

/// A reference to the root region.
///
/// # Safety
///
/// Single-threaded, like everything else here: fibers running across cores
/// (A5) will need this behind the same lock the refcounts eventually go
/// behind. `docs/roadmap.md` D10.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_root() -> *mut u8 {
    // SAFETY: single-threaded per the note above.
    unsafe {
        if ROOT.is_null() {
            ROOT = khora_region_open();
        }
        khora_dup(ROOT);
        ROOT
    }
}

/// Releases the root region, running whatever was deferred to it.
///
/// Called once by the generated entry point. A second call is a no-op, so a
/// program that never touched the root region costs nothing.
///
/// # Safety
///
/// Must be called after every other Khora frame has returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_close_root() {
    // SAFETY: single-threaded, and the caller guarantees nothing else is
    // still running.
    unsafe {
        let root = ROOT;
        ROOT = std::ptr::null_mut();
        if !root.is_null() {
            khora_drop(root, Some(release_shim));
        }
    }
}

/// [`khora_region_release`] as a `drop_fields` callback.
///
/// The callback type is a *safe* `extern "C" fn`, because that is what
/// generated code passes and generated code has no notion of Rust's `unsafe`.
/// The release itself keeps its contract, so the shim is where the claim that
/// the contract holds is made — once, here, rather than at every drop site.
extern "C" fn release_shim(region: *mut u8) {
    // SAFETY: only ever reached through `khora_drop`, which calls it with the
    // object whose last reference it just released.
    unsafe { khora_region_release(region) };
}

/// Opens a region, returning a Khora object that owns it.
///
/// The object is ordinary in every way that matters — reference counted,
/// dropped by [`khora_drop`] — which is the whole design. Its release runs the
/// finalizers, so a region ends exactly when the binding holding it does: at
/// the end of a block, at an early `return`, or on a raise passing through.
/// Every one of those paths already releases a boxed local, so none of them
/// needed a new rule.
#[unsafe(no_mangle)]
pub extern "C" fn khora_region_open() -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Finalizers>(), REGION_TAG);
    let list: Box<Finalizers> = Box::default();
    // SAFETY: `khora_alloc` returned an object with one field's worth of
    // space, zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>().write(Box::into_raw(list));
    }
    object
}

/// Registers a finalizer to run when `region` ends.
///
/// Takes ownership of `closure`: the region releases it after calling it, so
/// the caller hands over a reference of its own rather than lending one.
///
/// # Safety
///
/// `region` must be a live object from [`khora_region_open`], and `closure` a
/// live Khora closure of type `() -> ()` whose drop routine is `glue`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_defer(
    region: *mut u8,
    closure: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
) {
    if region.is_null() {
        fatal("deferring a finalizer to a null region");
    }
    // SAFETY: the caller guarantees a live region, whose field holds the
    // pointer `khora_region_open` wrote there.
    let list = unsafe { *region.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>() };
    if list.is_null() {
        fatal("deferring a finalizer to a region that has already been released");
    }
    // SAFETY: as above; the box is alive until the region is released, and no
    // other reference to it exists — the field is the only handle.
    unsafe { (*list).push(Finalizer { closure, glue }) };
}

/// Runs a region's finalizers and frees its list.
///
/// This is a `drop_fields` callback: [`khora_drop`] calls it when the last
/// reference to the region goes, and frees the object itself afterwards.
///
/// **Reverse order.** A finalizer deferred later may depend on one deferred
/// earlier — a transaction rolled back before the connection it ran on is
/// closed — so the last acquired is the first released, the same rule a stack
/// of scopes follows.
///
/// A finalizer that itself defers is deferring to a region that is already
/// releasing, which [`khora_region_defer`] rejects rather than silently
/// dropping.
///
/// # Safety
///
/// `region` must be a live object from [`khora_region_open`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_release(region: *mut u8) {
    if region.is_null() {
        return;
    }
    let slot = unsafe { region.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>() };
    // SAFETY: the caller guarantees a live region; the field holds what
    // `khora_region_open` wrote, and nothing else reads it after this.
    let list = unsafe { *slot };
    if list.is_null() {
        return;
    }
    // Cleared before running anything, so a finalizer that reaches this region
    // again finds it released rather than re-entering the list being drained.
    unsafe { slot.write(std::ptr::null_mut()) };

    // SAFETY: the pointer came from `Box::into_raw` in `khora_region_open` and
    // has not been freed — the null check above is what guarantees that.
    let list = unsafe { Box::from_raw(list) };
    for finalizer in list.into_iter().rev() {
        // SAFETY: a closure's first field is its code pointer, and a `() -> ()`
        // closure is called with its own object as the only argument. This is
        // the same convention generated code uses to call one.
        unsafe {
            let code = *finalizer.closure.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
            let call: extern "C" fn(*mut u8) = std::mem::transmute(code);
            call(finalizer.closure);
            khora_drop(finalizer.closure, finalizer.glue);
        }
    }
}
