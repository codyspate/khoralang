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
//! void *khora_drop_reuse(void *object, void (*drop_fields)(void *object));
//! void *khora_alloc_reuse(void *token, size_t size, uint32_t tag);
//! void  khora_free_reuse(void *token);
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

// One module per runtime responsibility. They were one file until the section
// banners inside it started disagreeing with their contents — strings under
// "Allocation and reference counting", the process arguments under "arrays" —
// which is the point at which a banner is worse than no banner. Roadmap 9.6.1.
//
// **Private, and re-exported wholesale.** The public surface is every
// `khora_rt::khora_*` there has ever been and nothing else: this crate's API is
// a C ABI, callers reach it by symbol, and `khora_rt::heap::khora_alloc` would
// be a second name for one function. Splitting the file was not supposed to add
// anything to the API and this is what makes sure it did not.
mod args;
mod array;
mod cancel;
mod coro;
mod counters;
mod current;
mod fiber;
mod heap;
mod nursery;
mod print;
mod random;
mod reactor;
mod region;
mod scheduler;
mod shared;
mod benching;
mod testing;
mod text;
mod time;
mod trap;
mod wait;
pub mod tls;

pub use args::*;
pub use array::*;
pub use cancel::*;
pub use counters::*;
pub use fiber::*;
pub use heap::*;
pub use nursery::*;
pub use print::*;
pub use random::*;
pub use region::*;
pub use shared::*;
pub use testing::*;
pub use text::*;
pub use time::*;
pub use trap::*;

use std::alloc::Layout;
use std::io::Write;
use std::sync::atomic::AtomicUsize;

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
    /// documentation.
    ///
    /// **The first field, and generated code relies on that.** This said that
    /// generated code never touches it and that every change goes through
    /// [`khora_dup`] and [`khora_drop`]. It no longer does: the call was a
    /// large fraction of what reference counting cost, so the backend emits the
    /// add and the subtract inline against offset zero and only the last
    /// reference calls [`khora_drop_last`]. `docs/design/reuse.md` §3.
    ///
    /// So moving this field is not a layout change the runtime can make on its
    /// own. The two functions below remain the C ABI for anything else linking
    /// against `khora-rt`, and are still what drop glue calls.
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

/// A tagged return: which channel, and the payload.
///
/// Every fallible Khora function returns one. `which` is zero for a value,
/// [`FAILED_WHICH`] for an error and [`CANCELLED_WHICH`] for a cancellation.
#[repr(C)]
pub struct Tagged {
    /// Which channel the payload belongs to.
    pub which: u32,
    /// The value, the error, or nothing.
    pub payload: u64,
}

/// Calls a fallible Khora function of no arguments and returns its tag,
/// writing the payload through the pointer. What a `test` block compiles to.
///
/// Generated code hands this over rather than the function itself, because a
/// tagged return is a 16-byte aggregate and how one of those comes back is a
/// target decision LLVM and rustc make separately. Only scalars cross here.
pub(crate) type Trampoline0 = extern "C" fn(*const u8, *mut u64) -> u32;

/// The same, for a function taking one pointer — a closure taking itself.
/// What a fiber body compiles to.
pub(crate) type Trampoline1 = extern "C" fn(*const u8, *mut u8, *mut u64) -> u32;
