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
//! Refcounts are **atomic**, which is decision D10. A5 promises fibers running
//! across cores, and a spawned fiber shares at least the closure it was handed,
//! so a non-atomic count would be a data race in the first program anyone
//! writes. Correct by default; see `docs/design/effect-runtime.md` §9 for why
//! there is no `Rc`/`Arc` split to opt out with, and phase 6 for the escape
//! analysis that makes a non-escaping object cheap again.
//!
//! The header layout is unchanged: an `AtomicUsize` has the size and alignment
//! of a `usize`, so the contract with the code generator is untouched.

#![deny(missing_docs, unsafe_op_in_unsafe_fn)]

use std::alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout};
use std::cell::RefCell;
use std::io::Write;
use std::sync::Arc;
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
    ///
    /// Atomic, and the same width a plain `usize` would be — see the module
    /// documentation. Generated code never touches it; every change goes
    /// through [`khora_dup`] and [`khora_drop`], which is what made D10 a
    /// runtime question rather than a language one.
    pub refcount: AtomicUsize,
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
        ptr.cast::<KhoraHeader>()
            .write(KhoraHeader { refcount: AtomicUsize::new(1), tag, field_bytes });
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

    // Read the layout out of the header *before* running the callback, so a
    // callback that scribbles on the header cannot make the deallocation use a
    // layout that differs from the allocation's.
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
    unsafe { (*ptr.cast::<KhoraHeader>()).refcount.load(Ordering::Relaxed) }
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

// --- cancellation ----------------------------------------------------------

thread_local! {
    /// Whether *this* fiber has been asked to stop.
    ///
    /// One per fiber, which is what makes cancelling one not cancel the rest.
    /// The main thread is a fiber too — it gets the flag every thread gets —
    /// so nothing has to special-case the program's own computation.
    ///
    /// Shared rather than owned, because the handle a parent holds has to be
    /// able to set it from outside.
    static CANCELLED: RefCell<Arc<AtomicUsize>> =
        RefCell::new(Arc::new(AtomicUsize::new(0)));

    /// Whether this thread is a spawned fiber rather than the program itself.
    ///
    /// Only [`khora_cancel_stop`] asks, and only to tell a program that has
    /// nowhere left to unwind to from a *fiber* that has nowhere left to
    /// unwind to. The first is an outcome; the second is a hole.
    static ON_FIBER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The running fiber's cancellation flag.
fn cancel_flag() -> Arc<AtomicUsize> {
    CANCELLED.with(|c| c.borrow().clone())
}

/// Asks the running computation to stop.
///
/// It stops at the next *cancellation point*, which is a `!` in a function
/// that can raise — never between two statements that do not mention one. See
/// `docs/design/effect-runtime.md` §6 for why that is the promise worth making.
///
/// Idempotent: asking twice is asking once.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancel() {
    cancel_flag().store(1, COUNTER_ORDER);
}

/// Whether a cancellation is pending.
///
/// Read at every cancellation point, so it is on the hot path of any loop that
/// does fallible work. A relaxed load of a word, which is what it costs.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancelled() -> u8 {
    cancel_flag().load(COUNTER_ORDER) as u8
}

/// Stops a cancelled computation that has nowhere left to unwind to.
///
/// Reached when a cancellation arrives at a frame with no error channel — a
/// function that caught every error in its row, so its signature promises a
/// value it can no longer produce. There is no frame between there and the
/// root that could carry the cancellation.
///
/// On the program's own computation that is an *outcome*: the root region's
/// finalizers run and the process exits 130, which is what the entry point
/// would have done anyway.
///
/// On a spawned fiber it is a *hole*, and this says so rather than taking the
/// whole program down quietly. A fiber's root should absorb a cancellation and
/// stop that fiber — which needs the spawned thunk to return a tagged value,
/// so the runtime can see how it ended. `docs/design/fibers.md` calls this out
/// as the piece 5.3 has not built yet.
///
/// # Safety
///
/// Must be called with no Khora frame relying on returning: it does not.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_cancel_stop() -> ! {
    if ON_FIBER.with(|f| f.get()) {
        fatal(
            "a cancellation reached a fiber's root, which cannot absorb one yet; \
             see docs/design/fibers.md",
        );
    }
    // SAFETY: nothing returns past this, so no other frame observes the
    // released root.
    unsafe { khora_region_close_root() };
    let _ = std::io::stdout().flush();
    std::process::exit(130)
}

/// Clears a pending cancellation.
///
/// For tests, and for a supervisor that has finished unwinding one computation
/// and is about to start another.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancel_reset() {
    cancel_flag().store(0, COUNTER_ORDER);
}

// --- fibers ----------------------------------------------------------------

/// A Khora pointer being moved to another fiber.
///
/// Raw pointers are not `Send`, and for good reason; this asserts that *these*
/// ones are safe to move, which they are because reference counts are atomic
/// (D10) and a spawned closure is handed over rather than shared — the caller
/// gives up its reference at the `spawn`.
struct Handed(*mut u8);

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
struct Tagged {
    which: u32,
    payload: u64,
}

/// The `which` a cancellation travels under.
///
/// Outside the range error-type ids are assigned from — they start at 1 and
/// count up — so no `catch` can name it and none will match it by accident.
/// The code generator's constant is defined *from* this one rather than beside
/// it, because two numbers that must agree are one number.
pub const CANCELLED_WHICH: u32 = u32::MAX;

/// What a fiber handle points at.
struct FiberState {
    /// `None` once joined. Joining twice is not an error — the second is a
    /// no-op — because the handle's release joins whatever `join` did not.
    thread: Option<std::thread::JoinHandle<()>>,
    /// The child's flag, shared with the child so a parent can set it.
    cancel: Arc<AtomicUsize>,
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
/// `fallible` says whether the thunk returns the tagged pair, which is how a
/// fiber reports that it was cancelled or that it failed. An infallible thunk
/// has no channel to say either on, and so cannot be stopped part-way.
///
/// # Safety
///
/// `body` must be a live Khora closure of type `() -> ()` whose drop routine
/// is `glue`, returning the tagged pair exactly when `fallible` is non-zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fiber_spawn(
    body: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
    fallible: u8,
) -> *mut u8 {
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
            if fallible == 0 {
                let call: extern "C" fn(*mut u8) = std::mem::transmute(code);
                call(body);
            } else {
                let call: extern "C" fn(*mut u8) -> Tagged = std::mem::transmute(code);
                let outcome = call(body);
                finish_fiber(outcome);
            }
            khora_drop(body, glue);
        }
    });

    let object = khora_alloc(std::mem::size_of::<*mut FiberState>(), FIBER_TAG);
    let state: Box<FiberState> = Box::new(FiberState { thread: Some(thread), cancel });
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
unsafe fn fiber_state<'a>(fiber: *mut u8) -> Option<&'a mut FiberState> {
    if fiber.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_fiber_spawn` wrote there.
    unsafe { (*fiber.add(KHORA_FIELD_OFFSET).cast::<*mut FiberState>()).as_mut() }
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
    if let Some(thread) = state.thread.take() {
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

        let mut state = Box::from_raw(state);
        if let Some(thread) = state.thread.take() {
            let _ = thread.join();
        }
    }
}

// --- nurseries -------------------------------------------------------------

/// [`khora_fiber_release`] as a `drop_fields` callback. See [`release_shim`].
extern "C" fn fiber_release_shim(fiber: *mut u8) {
    // SAFETY: only ever reached through `khora_drop`, which calls it with the
    // object whose last reference it just released.
    unsafe { khora_fiber_release(fiber) };
}

/// The fibers a nursery is responsible for.
///
/// Held Rust-side for the same reason a region's finalizers are: adopting one
/// *grows* the list, and nothing in Khora grows a value in place.
type Crew = Vec<Handed>;

/// The tag every nursery object carries.
const FIBERS_TAG: u32 = 0;

/// The list behind a nursery handle, or null once it has been released.
///
/// # Safety
///
/// `fibers` must be a live object from [`khora_fibers_open`].
unsafe fn crew<'a>(fibers: *mut u8) -> Option<&'a mut Crew> {
    if fibers.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_fibers_open` wrote there.
    unsafe { (*fibers.add(KHORA_FIELD_OFFSET).cast::<*mut Crew>()).as_mut() }
}

/// Opens a nursery: a set of fibers that ends when the binding holding it does.
///
/// Releasing it *cancels then waits*, which is the answer for the path where
/// the block did not finish — a raise, or a cancellation, passing through. On
/// the ordinary path [`khora_fibers_wait`] has already emptied it, so the
/// release finds nothing to stop. That is what lets one object mean both "wait
/// for the children" and "the answer is no longer wanted" without ever being
/// told which happened.
#[unsafe(no_mangle)]
pub extern "C" fn khora_fibers_open() -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Crew>(), FIBERS_TAG);
    let list: Box<Crew> = Box::default();
    // SAFETY: `khora_alloc` returned an object with one field's worth of
    // space, zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Crew>().write(Box::into_raw(list));
    }
    object
}

/// Makes `fiber` this nursery's responsibility, taking its reference.
///
/// # Safety
///
/// `fibers` must be live from [`khora_fibers_open`] and `fiber` live from
/// [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fibers_adopt(fibers: *mut u8, fiber: *mut u8) {
    // SAFETY: the caller guarantees a live nursery.
    let Some(list) = (unsafe { crew(fibers) }) else {
        fatal("adopting a fiber into a nursery that has already ended");
    };
    list.push(Handed(fiber));
}

/// Waits for every fiber in the nursery, oldest first, and empties it.
///
/// Oldest first because there is no reason to prefer otherwise and an order
/// that is stated is easier to reason about than one that is not. Every child
/// is waited for regardless: a nursery that returned after the first one
/// finished would not be structured at all.
///
/// # Safety
///
/// `fibers` must be a live object from [`khora_fibers_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fibers_wait(fibers: *mut u8) {
    // SAFETY: the caller guarantees a live nursery.
    let Some(list) = (unsafe { crew(fibers) }) else { return };
    for Handed(fiber) in std::mem::take(list) {
        // SAFETY: each handle was live when adopted and this list has held the
        // only reference since.
        unsafe {
            khora_fiber_join(fiber);
            khora_drop(fiber, Some(fiber_release_shim));
        }
    }
}

/// Cancels every fiber in the nursery, then waits for all of them.
///
/// This is a `drop_fields` callback, and it is the whole of structured
/// concurrency's failure case: the block is leaving without finishing, so the
/// answers its children were computing are no longer wanted. Cancelled *first*
/// and in one pass, so the children stop concurrently rather than one waiting
/// out the next.
///
/// # Safety
///
/// `fibers` must be a live object from [`khora_fibers_open`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fibers_release(fibers: *mut u8) {
    if fibers.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live nursery; the field holds what
    // `khora_fibers_open` wrote, and nothing else reads it after this.
    unsafe {
        let slot = fibers.add(KHORA_FIELD_OFFSET).cast::<*mut Crew>();
        let list = *slot;
        if list.is_null() {
            return;
        }
        slot.write(std::ptr::null_mut());

        let list = Box::from_raw(list);
        for Handed(fiber) in list.iter() {
            khora_fiber_cancel(*fiber);
        }
        for Handed(fiber) in list.into_iter() {
            khora_fiber_join(fiber);
            khora_drop(fiber, Some(fiber_release_shim));
        }
    }
}
