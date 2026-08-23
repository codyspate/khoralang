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
//! there is no `Rc`/`Arc` split to opt out with, and phase 9 for the escape
//! analysis that makes a non-escaping object cheap again.
//!
//! The header layout is unchanged: an `AtomicUsize` has the size and alignment
//! of a `usize`, so the contract with the code generator is untouched.

#![deny(missing_docs, unsafe_op_in_unsafe_fn)]

use std::alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout};
use std::cell::RefCell;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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

/// A counter that goes up by one every time it is read, starting at 1.
///
/// A testing aid, beside the allocation counters and there for the same
/// reason: some behaviour is only visible over repetition, and a Khora program
/// has no way to remember how many times it has done something. Mutable state
/// is D11's, and a test should not have to wait for it.
#[unsafe(no_mangle)]
pub extern "C" fn khora_tick() -> i64 {
    static TICKS: AtomicUsize = AtomicUsize::new(0);
    TICKS.fetch_add(1, COUNTER_ORDER) as i64 + 1
}

/// Whether `len` bytes starting at `data` are well-formed UTF-8.
///
/// The runtime's job because the answer is a table nobody should write twice,
/// and Rust's standard library already has it. Note what crosses: a pointer
/// and a length, and a `_Bool` back — the boundary rule holds here as it does
/// everywhere else.
///
/// # Safety
///
/// `data` must be null with a zero `len`, or address `len` initialized bytes
/// that stay live for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_utf8_valid(data: *const u8, len: i64) -> bool {
    if len <= 0 {
        return true;
    }
    if data.is_null() {
        return false;
    }
    // SAFETY: the contract above.
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
    std::str::from_utf8(bytes).is_ok()
}

/// Adds up `len` bytes starting at `data`.
///
/// A testing aid, and specifically a *foreign function* one: it is here so that
/// `Array::with_data` and `String::with_data` can be tested against something
/// that actually reads through the pointer they lend. A test that only checks
/// the pointer is non-null would pass just as well if the pointer addressed
/// the wrong place.
///
/// # Safety
///
/// `data` must be null with a zero `len`, or address `len` initialized bytes
/// that stay live for the call — which is exactly what a borrow guarantees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_sum_bytes(data: *const u8, len: i64) -> i64 {
    if data.is_null() || len <= 0 {
        return 0;
    }
    // SAFETY: the contract above.
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
    bytes.iter().map(|b| i64::from(*b)).sum()
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

/// Where `needle` first occurs in `hay`, or -1.
///
/// **A runtime call because the Khora version was 600 times slower.** Written
/// as a loop over `String::byte`, finding a six-byte word in eighty bytes took
/// 3,180 nanoseconds — a function call and a bounds check per byte, per
/// candidate position. `memmem` does it in single digits, and the request
/// parser calls it several times for every request a server answers.
///
/// An empty needle is found at zero, which is what every other language says
/// and what makes `split_once` on an empty separator terminate.
///
/// # Safety
///
/// Both pointers must address at least their stated length in readable bytes,
/// or be null with a length of zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_str_find(
    hay: *const u8,
    hay_len: usize,
    needle: *const u8,
    needle_len: usize,
) -> i64 {
    if needle_len == 0 {
        return 0;
    }
    if needle_len > hay_len || hay.is_null() || needle.is_null() {
        return -1;
    }
    // SAFETY: the caller guarantees both lengths are readable.
    let (hay, needle) = unsafe {
        (
            std::slice::from_raw_parts(hay, hay_len),
            std::slice::from_raw_parts(needle, needle_len),
        )
    };
    // The first byte narrows the candidates before anything longer is compared,
    // which is the whole of why this is not the loop it replaced.
    let first = needle[0];
    let last = hay_len - needle_len;
    let mut at = 0;
    while at <= last {
        match hay[at..=last].iter().position(|b| *b == first) {
            None => return -1,
            Some(step) => {
                let here = at + step;
                if &hay[here..here + needle_len] == needle {
                    return here as i64;
                }
                at = here + 1;
            }
        }
    }
    -1
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
type Finalizers = Mutex<Vec<Finalizer>>;

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
    // Locked, because a region is shareable and so two fibers may defer to one
    // at the same moment: a fiber that acquires a connection wants it released
    // by the scope that outlives it, which is the whole point of handing a
    // `Scope` across. `std::core::Share`.
    //
    // Uncontended almost always, and a region is not a hot path — it is
    // touched when a resource is acquired, not when one is used.
    //
    // SAFETY: as above; the box is alive until the region is released, and the
    // field is the only handle to it.
    unsafe { (*list).lock().unwrap_or_else(|e| e.into_inner()).push(Finalizer { closure, glue }) };
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
    let list = list.into_inner().unwrap_or_else(|e| e.into_inner());
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
struct FiberState {
    /// `None` once joined. Joining twice is not an error — the second is a
    /// no-op — because the handle's release joins whatever `join` did not.
    ///
    /// Behind a lock because `Fiber` is `Share`: two fibers may hold one handle
    /// and both call `join`, and "take the handle if it is there" is exactly
    /// the read-modify-write that has to happen once. Without it both could see
    /// `Some` and join the same thread twice.
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
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
unsafe fn fiber_state<'a>(fiber: *mut u8) -> Option<&'a FiberState> {
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
struct Children {
    /// The most children this nursery will hold at once, or zero for as many
    /// as are adopted.
    limit: usize,
    /// How long the list may get before it is worth sweeping.
    ///
    /// **Sweeping on every adoption was a third of a server's throughput.**
    /// Asking a child whether it has finished takes its lock, so a full pass
    /// over a bounded nursery of 256 was 256 lock-unlock pairs per connection —
    /// 2,134 requests a second against 6,406 for the same architecture written
    /// straight in Rust. Set to twice the survivors after each sweep, so the
    /// work is amortised to about one check per adoption however many children
    /// there are.
    sweep_at: usize,
    held: Vec<Handed>,
}

/// The shortest list worth walking. Below this a sweep costs more in
/// bookkeeping than the handles it reclaims.
const SWEEP_FLOOR: usize = 64;

type Crew = Mutex<Children>;

/// The tag every nursery object carries.
const FIBERS_TAG: u32 = 0;

/// The list behind a nursery handle, or null once it has been released.
///
/// # Safety
///
/// `fibers` must be a live object from [`khora_fibers_open`].
unsafe fn crew<'a>(fibers: *mut u8) -> Option<&'a Crew> {
    if fibers.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_fibers_open` wrote there.
    unsafe { (*fibers.add(KHORA_FIELD_OFFSET).cast::<*mut Crew>()).as_ref() }
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
    khora_fibers_open_bounded(0)
}

/// Opens a nursery that will hold at most `limit` running children.
///
/// **The bound is what turns a capacity ceiling into a queue.** A fiber is an
/// operating-system thread today, so a server adopting one per connection
/// spends about 33 KB apiece — measured — and an unbounded nursery meets its
/// ceiling by exhausting memory, which is the worst way to meet one. With a
/// bound, adopting the child past the limit *waits* for an older one to finish,
/// the accept loop stops accepting, and the connections pile up in the
/// listening socket's backlog where the operating system already knows how to
/// hold them. Overload becomes latency instead of collapse.
///
/// Zero means unbounded, which is right for a nursery over a known handful of
/// concurrent tasks — the shape `nursery(..)` is usually used for — and wrong
/// for one fed by the outside world.
#[unsafe(no_mangle)]
pub extern "C" fn khora_fibers_open_bounded(limit: i64) -> *mut u8 {
    let limit = if limit > 0 { limit as usize } else { 0 };
    let object = khora_alloc(std::mem::size_of::<*mut Crew>(), FIBERS_TAG);
    let list: Box<Crew> = Box::new(Mutex::new(Children {
        limit,
        sweep_at: SWEEP_FLOOR,
        held: Vec::new(),
    }));
    // SAFETY: `khora_alloc` returned an object with one field's worth of
    // space, zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Crew>().write(Box::into_raw(list));
    }
    object
}

/// Whether a fiber has already run to its end.
///
/// Asked without joining, so a nursery can let go of a child that has finished
/// without waiting on one that has not.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
unsafe fn fiber_finished(fiber: *mut u8) -> bool {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return true };
    let thread = state.thread.lock().unwrap_or_else(|e| e.into_inner());
    match thread.as_ref() {
        Some(handle) => handle.is_finished(),
        // Already joined by somebody, so there is nothing left to wait for.
        None => true,
    }
}

/// Makes `fiber` this nursery's responsibility, taking its reference.
///
/// **Children that have finished are let go of first**, and that sweep is not
/// housekeeping — without it a nursery only ever grows. A server adopts one
/// fiber per connection into a nursery it drains when it stops accepting,
/// which is never, so every answered request left its handle in the list: three
/// thousand requests, three thousand operating-system handles, none of them
/// pointing at a running thread. Measured on the link shortener, which is what
/// it took to see it.
///
/// **Not on every adoption**, which is the other measured thing. Asking a child
/// whether it has finished takes its lock, so sweeping each time cost a
/// bounded nursery 256 lock-unlock pairs per connection and two thirds of the
/// server's throughput. `sweep_at` holds it to about one check per adoption
/// amortised, by only walking the list once it has grown to twice what the
/// last sweep left behind.
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

    // Until there is room. Each turn sweeps what has finished and, if that was
    // not enough, takes the oldest child out to be waited for — outside the
    // lock, because a join is not instant once the child is still running and a
    // lock held across one is a nursery nobody else can adopt into.
    loop {
        let (done, waiting) = {
            // Locked: a nursery exists to be adopted into from more than one
            // fiber, so this is the one place contention is expected rather
            // than incidental.
            let mut crew = list.lock().unwrap_or_else(|e| e.into_inner());

            // Only when the list has grown past its mark, or when there is no
            // room and a sweep is the cheapest way to find some.
            let crowded = crew.limit > 0 && crew.held.len() >= crew.limit;
            let done: Vec<Handed> = if crowded || crew.held.len() >= crew.sweep_at {
                // SAFETY: every handle in the list was live when adopted and
                // this list has held the only reference since.
                let (done, keep): (Vec<Handed>, Vec<Handed>) = std::mem::take(&mut crew.held)
                    .into_iter()
                    .partition(|Handed(f)| unsafe { fiber_finished(*f) });
                crew.held = keep;
                crew.sweep_at = SWEEP_FLOOR.max(crew.held.len().saturating_mul(2));
                done
            } else {
                Vec::new()
            };

            if crew.limit == 0 || crew.held.len() < crew.limit {
                crew.held.push(Handed(fiber));
                (done, None)
            } else {
                // Oldest first, which is the order `khora_fibers_wait` uses and
                // the only one that cannot starve a child.
                (done, Some(crew.held.remove(0)))
            }
        };

        // Joining a thread that has already ended returns at once, but a drop
        // routine can reach another nursery, and a lock held across one of
        // those is a lock ordering nobody agreed to.
        for Handed(spent) in done {
            // SAFETY: as above; this is the last reference to each.
            unsafe {
                khora_fiber_join(spent);
                khora_drop(spent, Some(fiber_release_shim));
            }
        }

        match waiting {
            None => return,
            Some(Handed(oldest)) => {
                // SAFETY: as above.
                unsafe {
                    khora_fiber_join(oldest);
                    khora_drop(oldest, Some(fiber_release_shim));
                }
            }
        }
    }
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
    // **Drained in rounds, until a round finds nothing.** A child may adopt a
    // fiber of its own while this one is waiting — that is what a shareable
    // nursery is for — and a single pass would return with that grandchild
    // still running, which is precisely the promise a nursery makes.
    //
    // Taken under the lock and joined outside it, because holding the lock
    // across a join would deadlock against exactly that adoption.
    loop {
        let waiting = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        if waiting.is_empty() {
            return;
        }
        for Handed(fiber) in waiting {
            // SAFETY: each handle was live when adopted and this list has held
            // the only reference since.
            unsafe {
                khora_fiber_join(fiber);
                khora_drop(fiber, Some(fiber_release_shim));
            }
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

        // Same rounds as `khora_fibers_wait`, for the same reason: a child
        // being cancelled runs its finalizers on the way out, and one of those
        // may still be adopting. The list is this function's alone now — the
        // slot was nulled above — so each round takes what the last one did not
        // know about.
        let list = Box::from_raw(list);
        let mut round = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        while !round.is_empty() {
            for Handed(fiber) in round.iter() {
                khora_fiber_cancel(*fiber);
            }
            for Handed(fiber) in round {
                khora_fiber_join(fiber);
                khora_drop(fiber, Some(fiber_release_shim));
            }
            round = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        }
    }
}

// --- shared cells ----------------------------------------------------------

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

// --- the test runner -------------------------------------------------------

/// Calls a fallible Khora function of no arguments and returns its tag,
/// writing the payload through the pointer.
///
/// Generated code hands this over rather than the function itself, because a
/// tagged return is a 16-byte aggregate and how one of those comes back is a
/// target decision LLVM and rustc make separately. Only scalars cross here.
type Trampoline0 = extern "C" fn(*const u8, *mut u64) -> u32;

/// The same, for a function taking one pointer — a closure taking itself.
type Trampoline1 = extern "C" fn(*const u8, *mut u8, *mut u64) -> u32;

/// One test, waiting to be run.
struct PendingTest {
    name: String,
    code: Handed,
    call: Trampoline0,
}

/// The tests a program declared, in the order they were written.
static PENDING: Mutex<Vec<PendingTest>> = Mutex::new(Vec::new());

/// Registers a test. Called once per `test` block by the generated entry point.
///
/// # Safety
///
/// `name` must point at `len` bytes of UTF-8 that outlive the run — a string
/// literal does — and `code` must be a test's compiled body.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_test_register(
    name: *const u8,
    len: usize,
    code: *const u8,
    call: Trampoline0,
) {
    // SAFETY: the caller guarantees `len` bytes at `name`, live for the run.
    let bytes = if len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(name, len) } };
    let name = String::from_utf8_lossy(bytes).into_owned();
    if let Ok(mut pending) = PENDING.lock() {
        pending.push(PendingTest { name, code: Handed(code as *mut u8), call });
    }
}

/// Runs every registered test, one fiber each, and reports.
///
/// Returns the process's exit status: 0 when every test passed.
///
/// **One fiber each, all at once.** That is the point rather than a detail —
/// tests are the first thing anyone writes that is embarrassingly parallel, and
/// a test that only passes when it runs alone is a test that is lying. Isolated
/// by construction too: a fiber has its own cancellation flag, and nothing else
/// is shared but what the program itself shares.
#[unsafe(no_mangle)]
pub extern "C" fn khora_test_run() -> i32 {
    let tests: Vec<PendingTest> = match PENDING.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => return 1,
    };
    if tests.is_empty() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"no tests\n");
        return 0;
    }

    let running: Vec<_> = tests
        .into_iter()
        .map(|test| {
            let name = test.name.clone();
            let code = test.code;
            let call = test.call;
            let handle = std::thread::spawn(move || {
                let code = code;
                ON_FIBER.with(|f| f.set(true));
                let mut payload: u64 = 0;
                let which = (call)(code.0, &raw mut payload);
                Tagged { which, payload }
            });
            (name, handle)
        })
        .collect();

    let mut failed = 0usize;
    let mut total = 0usize;
    let mut out = std::io::stdout().lock();
    for (name, handle) in running {
        total += 1;
        let verdict = match handle.join() {
            // A test that ends any way other than "returned" did not pass.
            // Which way it was matters to the reader and not to the count.
            Ok(outcome) if outcome.which == 0 => "ok",
            Ok(outcome) if outcome.which == FAILED_WHICH => "FAILED",
            Ok(outcome) if outcome.which == CANCELLED_WHICH => "cancelled",
            Ok(outcome) => {
                // The error is nobody's to interpret here, and freeing its
                // fields would need a drop routine the runtime cannot know.
                // SAFETY: a live Khora object, or null.
                unsafe { khora_drop(outcome.payload as *mut u8, None) };
                "raised"
            }
            Err(_) => "panicked",
        };
        if verdict != "ok" {
            failed += 1;
        }
        let _ = writeln!(out, "test {name} ... {verdict}");
    }

    let passed = total - failed;
    let _ = writeln!(out, "\n{passed} passed, {failed} failed");
    i32::from(failed != 0)
}

// --- arrays ----------------------------------------------------------------

/// Field index of an array's length.
///
/// These four are `pub` because generated code reaches the elements directly —
/// an array read is a GEP and a load, not a call — so the layout is a contract
/// with the code generator exactly as the object header is, and it is written
/// down once here rather than twice.
pub const ARRAY_LEN_FIELD: usize = 0;
/// Field index of the routine that releases one element, or null.
pub const ARRAY_GLUE_FIELD: usize = 1;
/// Field index of the flag saying whether elements are counted at all.
pub const ARRAY_BOXED_FIELD: usize = 2;
/// Field index of the element size in bytes: 1, 2, 4 or 8.
///
/// An `Array<U8>` is a byte per element, not a word per element. A byte buffer
/// that costs eight bytes a byte is not a byte buffer, and every wire format,
/// file and string is one.
pub const ARRAY_STRIDE_FIELD: usize = 3;
/// How many fields precede the elements.
///
/// A whole number of words, so element zero is word-aligned and every stride
/// that divides a word is aligned along with it.
pub const ARRAY_HEADER_FIELDS: usize = 4;

/// The tag every array carries. Arrays are not an ADT, so nothing competes.
const ARRAY_TAG: u32 = 0;

/// Bytes of one field slot. Every Khora value is one word wide.
const FIELD_WORD: usize = std::mem::size_of::<usize>();

fn array_word(array: *const u8, index: usize) -> usize {
    // SAFETY: the caller guarantees a live array, whose first fields were
    // written by `khora_array_new`.
    unsafe { *array.add(KHORA_FIELD_OFFSET + index * FIELD_WORD).cast::<usize>() }
}

/// Allocates an array of `len` zeroed elements.
///
/// `boxed` says whether the elements are reference counted, and `glue` is what
/// releases one — the same pair every drop site already passes, kept in the
/// object because the length is only known at run time and the loop that
/// releases the elements therefore has to be too.
///
/// Zeroed matters: a null slot is what makes releasing an array that was never
/// filled a no-op rather than a wild free, the same reason `khora_alloc` zeroes.
#[unsafe(no_mangle)]
pub extern "C" fn khora_array_new(
    len: i64,
    fill: usize,
    stride: u8,
    boxed: u8,
    glue: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    if len < 0 {
        fatal("an array cannot have a negative length");
    }
    if !matches!(stride, 1 | 2 | 4 | 8) {
        fatal("an array element must be 1, 2, 4 or 8 bytes wide");
    }
    let len = len as usize;
    let stride = stride as usize;
    // Header first, then `len * stride` bytes rounded up to a whole word so the
    // allocation stays word-aligned and the next object after it does too.
    let Some(bytes) = len.checked_mul(stride) else {
        fatal("an array that large cannot be allocated");
    };
    let Some(fields) = bytes
        .checked_add(FIELD_WORD - 1)
        .map(|b| b / FIELD_WORD)
        .and_then(|words| words.checked_add(ARRAY_HEADER_FIELDS))
        .and_then(|n| n.checked_mul(FIELD_WORD))
    else {
        fatal("an array that large cannot be allocated");
    };

    let object = khora_alloc(fields, ARRAY_TAG);
    // SAFETY: `khora_alloc` returned space for the header fields and the
    // elements, zeroed and aligned.
    unsafe {
        let base = object.add(KHORA_FIELD_OFFSET).cast::<usize>();
        base.add(ARRAY_LEN_FIELD).write(len);
        base.add(ARRAY_GLUE_FIELD).write(glue.map_or(0, |g| g as usize));
        base.add(ARRAY_BOXED_FIELD).write(usize::from(boxed != 0));
        base.add(ARRAY_STRIDE_FIELD).write(stride);

        // Every slot holds the same value and every slot owns it, so the count
        // goes up once per slot. The caller keeps its own reference and
        // releases it after the call, which is why this does not take one.
        //
        // The fill arrives as a whole word and only its low `stride` bytes are
        // written, which is the same thing on a little-endian target — and both
        // targets Khora has are little-endian. A big-endian port would take the
        // *high* bytes, and this is where it would say so.
        let elements = base.add(ARRAY_HEADER_FIELDS).cast::<u8>();
        let source = fill.to_le_bytes();
        for index in 0..len {
            elements.add(index * stride).copy_from_nonoverlapping(source.as_ptr(), stride);
            if boxed != 0 {
                khora_dup(fill as *mut u8);
            }
        }
    }
    object
}

/// Writes a float and a newline.
///
/// Rust's shortest round-tripping form, which is what a reader wants: `0.1`
/// prints as `0.1` rather than as the seventeen digits that are literally
/// stored, and any two distinct doubles still print differently.
#[unsafe(no_mangle)]
pub extern "C" fn khora_print_float(value: f64) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
}

/// What the process was invoked with, as the C runtime handed it over.
///
/// **The one thing a program cannot ask the operating system for.** Every other
/// piece of its environment has a function — `getenv`, `time`, `fopen` — and
/// the arguments arrive once, through `main`, and are gone. So the generated
/// `main` hands them here before it does anything else, and this holds them for
/// whatever asks later.
///
/// `Relaxed` because they are written once, before any Khora code runs, and
/// only read afterwards; there is no ordering for anything else to depend on.
static ARG_COUNT: AtomicUsize = AtomicUsize::new(0);
static ARG_VECTOR: AtomicUsize = AtomicUsize::new(0);

/// Records `argc` and `argv`. Called by generated code, first thing in `main`.
///
/// # Safety
///
/// `argv` must be the `main` argument of the same name: `argc` pointers to
/// NUL-terminated strings, live for the process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_args_set(argc: i32, argv: *const *const u8) {
    ARG_COUNT.store(argc.max(0) as usize, COUNTER_ORDER);
    ARG_VECTOR.store(argv as usize, COUNTER_ORDER);
}

/// How many arguments there were, the program's own name included.
#[unsafe(no_mangle)]
pub extern "C" fn khora_arg_count() -> i64 {
    ARG_COUNT.load(COUNTER_ORDER) as i64
}

/// The `index`th argument, as a pointer to its NUL-terminated bytes.
///
/// Null when there is no such argument. What Khora does with it is read its
/// length with `strlen` and copy it with `memcpy`, both of which are ISO C —
/// so the only thing that had to be added here is the part C has no function
/// for.
#[unsafe(no_mangle)]
pub extern "C" fn khora_arg(index: i64) -> *const u8 {
    if index < 0 || index >= khora_arg_count() {
        return std::ptr::null();
    }
    let argv = ARG_VECTOR.load(COUNTER_ORDER) as *const *const u8;
    if argv.is_null() {
        return std::ptr::null();
    }
    // SAFETY: `argv` came from `main` per `khora_args_set`'s contract, and the
    // index was just checked against the count recorded with it.
    unsafe { *argv.add(index as usize) }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------
//
// ISO C offers `time`, which is whole seconds, and nothing finer that is
// portable: milliseconds are `GetSystemTimeAsFileTime` on Windows and
// `clock_gettime` on Unix, two different calls with two different epochs and
// two different headers. Rust's `std::time` has already made that choice on
// every target this runtime builds for, so binding it here is cheaper and more
// correct than a `#[cfg]` ladder in Khora would be — and it is the reason this
// pair lives in the runtime rather than in `std/env.kh` beside `getenv`.
//
// **Two clocks, because they measure two different things**, and the effect
// exposes both rather than picking one. The wall clock is what a timestamp on a
// log line means; it can jump — NTP steps it, an administrator sets it, a
// virtual machine resumes with a stale one — and it can jump *backwards*, so a
// duration computed from two readings of it can be negative or wildly wrong.
// The monotonic clock cannot go backwards and is what "how long did this take"
// actually wants; it has no epoch anybody outside the process can name, so it
// is useless for a timestamp. Neither one substitutes for the other, and a
// single `millis` would silently be the wrong one half the time.

/// Milliseconds since 1970, from the wall clock.
///
/// Negative before 1970, which a machine with a dead battery will report and
/// which is a truer answer than clamping to zero would be.
///
/// Saturates rather than wrapping at the far end. `i64` milliseconds run out in
/// the year 292,278,994; a clock claiming to be past that is broken, and the
/// useful response is a number that is still ordered rather than one that has
/// gone negative.
#[unsafe(no_mangle)]
pub extern "C" fn khora_unix_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_millis().min(i64::MAX as u128) as i64,
        Err(before) => -(before.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// Milliseconds on a clock that only goes forwards.
///
/// The origin is the first call, so the first reading is zero and everything
/// after it is "how long since the program started asking". An arbitrary origin
/// is not a shortcut: a monotonic clock's zero is arbitrary on every platform,
/// and pinning it here means a Khora program never sees a boot-time or an
/// uptime that differs between targets.
///
/// `Instant` is `QueryPerformanceCounter` on Windows and `CLOCK_MONOTONIC` on
/// Unix, and Rust guarantees the difference of two of them never goes
/// backwards even where the underlying counter misbehaves across cores.
#[unsafe(no_mangle)]
pub extern "C" fn khora_monotonic_millis() -> i64 {
    /// Written once, by whichever fiber reads the clock first. `OnceLock`
    /// rather than a `static mut` because fibers are threads and the first
    /// read can genuinely be a race between two of them; they then agree on
    /// the winner's origin, which is the whole point of a shared timeline.
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = *ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_millis().min(i64::MAX as u128) as i64
}

// ---------------------------------------------------------------------------
// Randomness
// ---------------------------------------------------------------------------
//
// **Three pure functions and a seed, and no generator state here at all.** The
// state lives on the Khora side in a `Shared<Int>`, which is a cell behind a
// mutex — see `khora_shared_update`. That is the answer to "fibers are threads,
// so what serializes two of them drawing at once": the same lock every other
// shared cell uses, taken by the step that advances the state, rather than a
// second mechanism invented here. A `thread_local!` generator was the
// alternative and was rejected for one reason: it cannot be *pinned*. A test
// that seeds a handler and then spawns a fiber would get a different sequence
// in the child, and reproducibility is the entire reason randomness is a
// capability instead of a function.
//
// The generator is splitmix64: state advances by a fixed odd constant and the
// output is a bijective mix of it. Chosen because the advance is one addition —
// so the part that has to happen under the lock is as short as it can be — and
// because it needs no state beyond the one word a `Shared<Int>` holds.
//
// **This is not a cryptographic generator.** Anyone who sees 64 bits of output
// can invert the mix and predict every draw after it. It is the right thing for
// a shuffle, a jitter, a load-balancing choice or a test fixture, and the wrong
// thing for a session token or a key. A CSPRNG is a different capability with
// a different name, and giving it one is how a program says which it needed.

/// The constant splitmix64 walks its state by: the odd number nearest
/// 2^64 divided by the golden ratio, which is where the "golden gamma" name
/// comes from. Odd, so adding it repeatedly visits all 2^64 states before
/// repeating — the period is the full cycle by construction, not by luck.
const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// The next state after `state`.
///
/// Half of the generator, split out from the mix so that the Khora side can put
/// exactly this — one wrapping addition — inside the cell's lock, and do the
/// mixing outside it. The constant lives here and only here; a Khora copy of it
/// would be a second place for the sequence to be defined from, and the two
/// drifting would break reproducibility silently.
#[unsafe(no_mangle)]
pub extern "C" fn khora_random_step(state: i64) -> i64 {
    (state as u64).wrapping_add(GOLDEN_GAMMA) as i64
}

/// The draw a state produces: splitmix64's finalizer.
///
/// A bijection, so distinct states give distinct draws and the sequence cannot
/// repeat before the state does. Pure, which is what lets it run outside the
/// lock.
#[unsafe(no_mangle)]
pub extern "C" fn khora_random_mix(state: i64) -> i64 {
    let mut z = state as u64;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) as i64
}

/// A seed nobody can guess and no two runs share.
///
/// The entropy is the operating system's, reached through the one door Rust's
/// standard library opens onto it: `RandomState` is the hash-table seed, and it
/// exists precisely so that a process's hashing cannot be predicted from
/// outside — which means it is keyed from `getrandom` on Linux and
/// `BCryptGenRandom` on Windows. Using it as a seed source is why this crate
/// still has no dependencies, and `docs/design/ecosystem.md` prefers binding
/// what exists to vendoring a copy of it.
///
/// The process time and a counter go in as well, so that two seeds taken in one
/// process differ even if some future standard library hands out the same keys
/// twice. The counter is `Relaxed` for the reason all the others here are: it
/// publishes no other memory, and two fibers seeding at once need distinct
/// values rather than an ordering.
///
/// Not for keys — see the note above the constant. This is unguessable in the
/// sense that a shuffle wants and not in the sense that a cipher does.
#[unsafe(no_mangle)]
pub extern "C" fn khora_random_seed() -> i64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_usize(NEXT.fetch_add(1, COUNTER_ORDER));
    hasher.write_i64(khora_unix_millis());
    hasher.write_i64(khora_monotonic_millis());
    khora_random_mix(hasher.finish() as i64)
}

/// A draw reduced to `[0, span)`, without division and without rejection.
///
/// Lemire's multiply-and-shift: take the draw as an unsigned 64-bit number,
/// multiply it by the span as a 128-bit product, and keep the high half. That
/// is the same thing as scaling the draw's position in `[0, 1)` up to
/// `[0, span)`, and it is one multiply where a modulo would be a division.
///
/// **The bias is real and bounded.** Because 2^64 is not usually a multiple of
/// the span, some outputs get one more of the 2^64 draws than others — a
/// relative bias of at most `span / 2^64`. For a span a program would plausibly
/// ask for that is smaller than one part in a hundred billion billion. The
/// version with no bias at all rejects and redraws, which means a loop, which
/// means the Khora side would have to hold the lock across an unbounded number
/// of steps. Not worth it for a bound nothing can measure.
///
/// An empty range stops the program, like an index outside an array does: there
/// is no number in `[low, low)` to return, and inventing `low` would turn a
/// caller's off-by-one into a value that looks legitimate everywhere it lands.
#[unsafe(no_mangle)]
pub extern "C" fn khora_random_scale(draw: i64, span: i64) -> i64 {
    if span <= 0 {
        fatal("a random range must not be empty: `low` has to be below `high`");
    }
    let wide = u128::from(draw as u64) * u128::from(span as u64);
    // The product's high half is below `span`, which is a positive `i64`, so
    // this narrowing cannot change the value.
    (wide >> 64) as i64
}

/// Writes `value` into `into` as text, and says how many bytes that took.
///
/// The shortest form that reads back as the same number, which is Rust's
/// `{}` and is what a reader means by "the number". Khora cannot produce it
/// itself: shortest-round-trip formatting is Ryū or Grisu, a table and a
/// thousand lines, and it is exactly the kind of thing
/// `docs/design/ecosystem.md` says to bind rather than write twice.
///
/// Returns the length needed when `capacity` is too small and writes nothing,
/// so a caller can size a buffer in two calls rather than guessing. 32 bytes
/// is always enough for an `f64`, which is why nobody will make the second
/// call.
///
/// # Safety
///
/// `into` must address `capacity` writable bytes, or be null with a zero
/// `capacity`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_float_text(value: f64, into: *mut u8, capacity: i64) -> i64 {
    let text = format!("{value}");
    let bytes = text.as_bytes();
    if capacity < bytes.len() as i64 || into.is_null() {
        return bytes.len() as i64;
    }
    // SAFETY: the contract above, and the length was just checked against it.
    unsafe { into.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
    bytes.len() as i64
}

/// Reports arithmetic that did not fit, and stops.
///
/// Overflow traps in every build. A program that passes its tests and then
/// wraps in production is the failure worth spending a branch to prevent, and
/// two behaviours — one for testing, one for shipping — put the difference
/// exactly where it is most expensive to find. `docs/roadmap.md` 6.2.
///
/// `Int::wrapping_add` and its siblings are how you ask for the other thing,
/// in the places that genuinely want it: a hash, a checksum, a PRNG.
///
/// # Safety
///
/// `what` must point at `len` bytes naming the operation — generated code
/// passes a string literal in `.rodata`, live for the program's whole run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_overflow(what: *const u8, len: usize) -> ! {
    let bytes = if len == 0 { &[][..] } else {
        // SAFETY: the caller passes a string literal in `.rodata`, live for the
        // program's whole run.
        unsafe { std::slice::from_raw_parts(what, len) }
    };
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "khora: {} overflowed", String::from_utf8_lossy(bytes));
    let _ = std::io::stdout().flush();
    std::process::exit(134)
}

/// Reports an index that was not in range, and stops.
///
/// A trap rather than a wrapped value or a poisoned read, for the same reason
/// integer overflow traps: a program that reads past its own array is wrong,
/// and the useful thing to do is say where.
#[unsafe(no_mangle)]
pub extern "C" fn khora_bounds_fail(index: i64, len: i64) -> ! {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "khora: index {index} is outside an array of {len}");
    let _ = std::io::stdout().flush();
    std::process::exit(134)
}

/// Releases every element of an array, then the array.
///
/// A `drop_fields` callback. The loop is here rather than generated because
/// the length is a run-time value; what to do with one element is generated,
/// and travels in the object as `glue`.
///
/// # Safety
///
/// `array` must be a live object from [`khora_array_new`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_array_release(array: *mut u8) {
    if array.is_null() {
        return;
    }
    if array_word(array, ARRAY_BOXED_FIELD) == 0 {
        return;
    }
    let len = array_word(array, ARRAY_LEN_FIELD);
    let glue = array_word(array, ARRAY_GLUE_FIELD);
    // SAFETY: a null glue is `None`, which is how a boxed element that owns
    // nothing is released — the same convention `khora_drop` already uses.
    let glue: Option<extern "C" fn(*mut u8)> =
        if glue == 0 {
            None
        } else {
            unsafe { Some(std::mem::transmute::<usize, extern "C" fn(*mut u8)>(glue)) }
        };

    for index in 0..len {
        debug_assert_eq!(
            array_word(array, ARRAY_STRIDE_FIELD),
            FIELD_WORD,
            "a counted element is a pointer, so it is always a whole word wide"
        );
        // SAFETY: every element slot is within the allocation, and holds
        // either null or a live object this array owned a reference to.
        unsafe {
            let slot = array
                .add(KHORA_FIELD_OFFSET + (ARRAY_HEADER_FIELDS + index) * FIELD_WORD)
                .cast::<*mut u8>();
            khora_drop(*slot, glue);
        }
    }
}
