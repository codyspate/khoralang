//! The runtime's half of the heap contract, expressed in LLVM types.
//!
//! `khora-rt`'s module documentation is the contract; this is the code
//! generator agreeing to it. Every number here is derived from that crate
//! rather than written down again, because the failure mode of the two sides
//! disagreeing is a heap that corrupts silently and crashes somewhere else
//! entirely.

use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;

use khora_rt::{KhoraHeader, KHORA_FIELD_OFFSET};

/// Bytes of one field slot.
///
/// Phase 2 uses a uniform boxed representation: every field is a machine word,
/// either an `Int`, a `Bool` widened to a word, or a pointer. `khora-rt`
/// guarantees the field area starts 8-aligned, so a word-indexed field is
/// naturally aligned.
pub const FIELD_WORD: u64 = std::mem::size_of::<usize>() as u64;

/// Byte offset of the variant tag from an object pointer.
///
/// Read out of the header struct rather than written as `8`: the runtime docs
/// promise the layout, and `offset_of!` makes that promise checkable by the
/// compiler instead of by a comment.
pub const TAG_OFFSET: u64 = std::mem::offset_of!(KhoraHeader, tag) as u64;

/// Byte offset of the first field from an object pointer.
pub const FIELD_OFFSET: u64 = KHORA_FIELD_OFFSET as u64;

/// The `which` a cancellation travels under.
///
/// Outside the range error-type ids are assigned from — they start at 1 and
/// count up — so no `catch` can name it and none will accidentally match it.
/// A cancellation is not an error the program declared; it is the runtime
/// asking the computation to stop, and only the entry point absorbs it.
pub const CANCELLED_WHICH: u64 = khora_rt::CANCELLED_WHICH as u64;

/// The `which` a failed assertion travels under. Beside the cancellation, and
/// outside the range error-type ids come from, so no `catch` can name it.
pub const FAILED_WHICH: u64 = khora_rt::FAILED_WHICH as u64;

/// The exit status of a program that was cancelled and never stopped being.
///
/// 128 + SIGINT, which is what a shell already means by "interrupted". A
/// program that raises and does not handle it exits 1; these are different
/// outcomes and worth telling apart from outside.
pub const CANCELLED_EXIT: u64 = 130;

/// The type name of a fiber handle. Like a region, a handle the runtime owns
/// — and like a region, releasing it is what makes the structure structured:
/// the release *joins*, so a fiber cannot outlive the binding that holds it.
pub const FIBER_TYPE: &str = khora_types::FIBER_TYPE;

/// The type name of a contiguous, fixed-length array.
pub const ARRAY_TYPE: &str = "Array";

/// An array's layout, restated from `khora-rt` rather than redefined: length,
/// the routine that releases one element, whether the elements are counted at
/// all, and then the elements. Generated code reaches an element directly, so
/// the two sides have to agree and there is one definition to agree with.
pub const ARRAY_LEN_FIELD: u64 = khora_rt::ARRAY_LEN_FIELD as u64;
pub const ARRAY_HEADER_FIELDS: u64 = khora_rt::ARRAY_HEADER_FIELDS as u64;

/// The type name of a nursery: the fibers a block is responsible for.
///
/// Releasing one cancels its children and waits, which is the answer for a
/// block that did not finish. On the ordinary path `Fibers::wait` has already
/// emptied it, so one object means both without being told which happened.
pub const FIBERS_TYPE: &str = "Fibers";

/// The synchronized cell. `docs/design/shared.md`.
pub const SHARED_TYPE: &str = "Shared";

/// The bounded channel. `docs/design/channels.md`.
pub const CHANNEL_TYPE: &str = "Channel";

/// The certified-closure wrapper, which is a closure and nothing else.
pub const SHARED_FN_TYPE: &str = khora_types::SHARED_FN_TYPE;

/// The type name of a region. Not an ADT: it is a handle the runtime owns,
/// and the only thing generated code does with one is hand it back and let it
/// be released.
pub const REGION_TYPE: &str = "Region";

/// The tag given to a `String`.
///
/// Strings are not an ADT, so no variant index competes for the value. Zero is
/// as good as any other; the runtime never interprets it.
pub const STRING_TAG: u64 = 0;

/// Field index of a string's byte length. The bytes follow it immediately.
pub const STRING_LEN_FIELD: u64 = 0;

/// Byte offset from a string object to its first byte.
pub const STRING_BYTES_OFFSET: u64 = FIELD_OFFSET + FIELD_WORD;

// The generated code assumes a 64-bit target throughout: `size_t` is lowered as
// `i64`, and a field slot holds a pointer. We generate for the host triple, so
// the host's word size is the target's.
const _: () = assert!(
    std::mem::size_of::<usize>() == 8,
    "the phase 2 backend generates for 64-bit hosts only"
);

/// Every runtime symbol generated code can call.
///
/// Declared once per module and handed around by value — inkwell's
/// `FunctionValue` is a `Copy` handle into the module, not an owner.
#[derive(Clone, Copy)]
pub struct Runtime<'ctx> {
    /// `void *khora_alloc(size_t field_bytes, uint32_t tag)`
    pub alloc: FunctionValue<'ctx>,
    /// `void khora_dup(void *object)`
    pub dup: FunctionValue<'ctx>,
    /// `void khora_drop(void *object, void (*drop_fields)(void *))`
    pub drop: FunctionValue<'ctx>,
    /// `int khora_contain_enabled(void)` — whether the host asked for a trap
    /// inside an exported call to be contained. Read by every export's
    /// wrapper, so the answer has to be cheap.
    pub contain_enabled: FunctionValue<'ctx>,
    /// `uint64_t khora_export_call(uint64_t (*body)(void *), void *ctx)`
    ///
    /// Runs one exported call under a landing point. See
    /// `khora-rt/src/contain.rs` and `csrc/guard.c`.
    pub export_call: FunctionValue<'ctx>,
    /// `void khora_single_threaded(void)` — told to the runtime by `main` when
    /// the compiler counted references non-atomically, so that a spawn is an
    /// abort rather than a race.
    pub single_threaded: FunctionValue<'ctx>,
    /// `void khora_drop_last(void *object, void (*drop_fields)(void *), size_t previous)`
    ///
    /// The slow half of a drop generated code decremented itself.
    pub drop_last: FunctionValue<'ctx>,
    /// `void *khora_drop_reuse(void *object, void (*drop_fields)(void *))`
    ///
    /// `khora_drop`, but the last reference keeps the memory and hands it back
    /// as a token. Null on any other outcome.
    pub drop_reuse: FunctionValue<'ctx>,
    /// `void khora_free_reuse(void *token)` — the safety net for one nothing
    /// spent.
    pub free_reuse: FunctionValue<'ctx>,
    /// `void *khora_alloc_reuse(void *token, size_t field_bytes, uint32_t tag)`
    ///
    /// `khora_alloc`, but spends a token from `khora_drop_reuse` when it fits.
    pub alloc_reuse: FunctionValue<'ctx>,
    /// `void khora_print_int(int64_t)`
    pub print_int: FunctionValue<'ctx>,
    /// `void khora_print_float(double)`
    pub print_float: FunctionValue<'ctx>,
    /// `void khora_print_bool(_Bool)`
    pub print_bool: FunctionValue<'ctx>,
    /// `void khora_print_str(const uint8_t *, size_t)`
    pub print_str: FunctionValue<'ctx>,
    /// `_Bool khora_str_eq(const uint8_t *, size_t, const uint8_t *, size_t)`
    pub str_eq: FunctionValue<'ctx>,
    /// `int64_t khora_str_find(const uint8_t *, size_t, const uint8_t *, size_t)`
    pub str_find: FunctionValue<'ctx>,
    /// `void *khora_region_open(void)`
    pub region_open: FunctionValue<'ctx>,
    /// `void khora_region_defer(void *region, void *closure, void (*glue)(void *))`
    pub region_defer: FunctionValue<'ctx>,
    /// `void khora_region_release(void *region)` — a `drop_fields` callback.
    pub region_release: FunctionValue<'ctx>,
    /// `void *khora_region_root(void)`
    pub region_root: FunctionValue<'ctx>,
    /// `uint8_t khora_cancelled(void)`
    pub cancelled: FunctionValue<'ctx>,
    /// `_Noreturn void khora_cancel_stop(void)`
    pub cancel_stop: FunctionValue<'ctx>,
    /// `void *khora_shared_open(uint64_t value, bool boxed, void (*glue)(void *))`
    pub shared_open: FunctionValue<'ctx>,
    /// `uint64_t khora_shared_get(void *cell)`
    pub shared_get: FunctionValue<'ctx>,
    /// `void khora_shared_set(void *cell, uint64_t value)`
    pub shared_set: FunctionValue<'ctx>,
    /// `uint64_t khora_shared_update(void *cell, void *change, Change call)`
    pub shared_update: FunctionValue<'ctx>,
    /// `uint64_t khora_shared_modify(void *cell, void *change, Modify call,
    ///                               uint64_t *answer)`
    pub shared_modify: FunctionValue<'ctx>,
    /// `void khora_shared_release(void *cell)`, a `drop_fields` callback.
    pub shared_release: FunctionValue<'ctx>,
    /// `void *khora_channel_open(int64_t capacity, int64_t strategy, bool boxed, void (*glue)(void *))`
    ///
    /// `strategy` is what a send does when the queue is full: 0 waits, 1
    /// refuses, 2 evicts the oldest. Recorded on the channel rather than
    /// passed per send, because a queue is lossy or it is not.
    pub channel_open: FunctionValue<'ctx>,
    /// `bool khora_channel_send(void *channel, uint64_t value)`
    pub channel_send: FunctionValue<'ctx>,
    /// `bool khora_channel_receive(void *channel, uint64_t *out)`
    pub channel_receive: FunctionValue<'ctx>,
    /// `void khora_channel_close(void *channel)`
    pub channel_close: FunctionValue<'ctx>,
    /// `bool khora_channel_poll(void *channel, uint64_t *out)`
    ///
    /// The same shape as `receive` and the same answer, minus the waiting.
    pub channel_poll: FunctionValue<'ctx>,

    /// `int64_t khora_channel_depth(void *channel)`
    pub channel_depth: FunctionValue<'ctx>,
    /// `void khora_channel_release(void *channel)`, a `drop_fields` callback.
    pub channel_release: FunctionValue<'ctx>,
    /// `void *khora_fiber_spawn(void *body, void (*glue)(void *),
    ///                            uint32_t (*call)(const void *, void *, uint64_t *))`
    pub fiber_spawn: FunctionValue<'ctx>,
    /// `uint32_t khora_fiber_join(void *fiber, uint64_t *out)`
    ///
    /// The same shape a fallible call has: 0 means `out` holds the answer, and
    /// anything else means it holds an error to re-raise.
    pub fiber_join: FunctionValue<'ctx>,
    /// `void khora_fiber_detach(void *fiber)`
    pub fiber_detach: FunctionValue<'ctx>,
    /// `void khora_fiber_wait(void *fiber)`
    ///
    /// A join that does not take the answer, for a caller who wanted the
    /// ordering. It is also the only way to wait for a fiber that may have been
    /// *cancelled* without the cancellation unwinding the waiter.
    pub fiber_wait: FunctionValue<'ctx>,
    /// `void khora_fiber_cancel(void *fiber)`
    pub fiber_cancel: FunctionValue<'ctx>,
    /// `void khora_fiber_release(void *fiber)` — a `drop_fields` callback.
    pub fiber_release: FunctionValue<'ctx>,
    /// `void *khora_fibers_open(void)`
    pub fibers_open: FunctionValue<'ctx>,
    /// `void *khora_fibers_open_bounded(int64_t limit)`
    pub fibers_bounded: FunctionValue<'ctx>,
    /// `void khora_fibers_adopt(void *fibers, void *fiber)`
    pub fibers_adopt: FunctionValue<'ctx>,
    /// `int64_t khora_fibers_wait(void *fibers)`, answering how many children
    /// ended with an error.
    pub fibers_wait: FunctionValue<'ctx>,
    /// `void khora_fibers_release(void *fibers)` — a `drop_fields` callback.
    pub fibers_release: FunctionValue<'ctx>,
    /// `void khora_args_set(int32_t argc, const char *const *argv)`
    pub args_set: FunctionValue<'ctx>,
    /// `_Bool khora_utf8_valid(const uint8_t *data, int64_t len)`
    pub utf8_valid: FunctionValue<'ctx>,
    /// `void *khora_array_new(int64_t len, size_t fill, uint8_t stride, _Bool boxed,
    ///                            void (*glue)(void *))`
    pub array_new: FunctionValue<'ctx>,
    /// `void khora_array_release(void *array)` — a `drop_fields` callback.
    pub array_release: FunctionValue<'ctx>,
    /// `_Noreturn void khora_bounds_fail(int64_t index, int64_t len)`
    pub bounds_fail: FunctionValue<'ctx>,
    /// `_Noreturn void khora_overflow(const uint8_t *what, size_t len)`
    pub overflow: FunctionValue<'ctx>,
    /// `void khora_unhandled(const uint8_t *name, size_t len)` — says which
    /// error left `main`. Returns, unlike its neighbours here: the program is
    /// ending correctly and this only says why.
    pub unhandled: FunctionValue<'ctx>,
    /// `void khora_assert_failed(uint32_t ordinal, uint32_t line)` — says which
    /// `assert` in the running test did not hold, and where it was written.
    pub assert_failed: FunctionValue<'ctx>,
    /// `void khora_begin(void)` — the first call every entry point makes.
    /// Installs the stack guard, which has to be in place before anything can
    /// exhaust the stack.
    pub begin: FunctionValue<'ctx>,
    /// `void khora_test_register(const uint8_t *name, size_t len, const void *code,
    ///                             uint32_t (*call)(const void *, uint64_t *))`
    pub test_register: FunctionValue<'ctx>,
    /// `int32_t khora_test_run(void)`
    pub test_run: FunctionValue<'ctx>,
    /// `void khora_safepoint(void)`, at every loop back-edge.
    pub safepoint: FunctionValue<'ctx>,
    /// The same pair for `bench` blocks. Same signatures, because a bench body
    /// and a test body are the same shape; only the runner differs.
    pub bench_register: FunctionValue<'ctx>,
    /// `int32_t khora_bench_run(void)`
    pub bench_run: FunctionValue<'ctx>,
    /// `void khora_region_close_root(void)`
    pub region_close_root: FunctionValue<'ctx>,
    /// `llvm.trap`, for a branch that exhaustiveness says cannot be taken.
    pub trap: FunctionValue<'ctx>,
}

impl<'ctx> Runtime<'ctx> {
    /// Declares the runtime interface into a fresh module.
    pub fn declare(
        ctx: &'ctx Context,
        module: &Module<'ctx>,
        target: &inkwell::targets::TargetData,
    ) -> Runtime<'ctx> {
        let void = ctx.void_type();
        let i64t = ctx.i64_type();
        let i32t = ctx.i32_type();
        let i8t = ctx.i8_type();
        let ptr = ctx.ptr_type(AddressSpace::default());

        // **The runtime ABI is fixed-width on every target, and `usize` does
        // not appear in it.** Seven of these took a Rust `usize`, declared here
        // as `i64` — right on the three 64-bit targets and wrong on every other
        // one. `wasm-ld` was the first linker to say so, because it checks
        // signatures where an ELF or COFF linker matches on the name alone and
        // would have passed a 64-bit argument to a function expecting 32.
        //
        // The fix could have been to read the pointer width from the data
        // layout and emit that. Making the *runtime* fixed-width instead is
        // better: a contract between generated code and the runtime that
        // changes shape per target is one more thing to get wrong, and it
        // would have had to be got right again at every call site. `khora-rt`
        // takes `u64` and narrows internally where it wants an index. The same
        // reasoning made `KhoraHeader::refcount` a `u64` rather than a
        // `usize`, so the object layout is one layout everywhere.
        let _ = target;

        let declare = |name: &str, ty: inkwell::types::FunctionType<'ctx>| {
            module.add_function(name, ty, Some(Linkage::External))
        };

        Runtime {
            alloc: declare("khora_alloc", ptr.fn_type(&[i64t.into(), i32t.into()], false)),
            dup: declare("khora_dup", void.fn_type(&[ptr.into()], false)),
            // `drop_fields` is `Option<extern "C" fn(*mut u8)>` on the Rust
            // side, which is one pointer wide with `None` as null. Under
            // opaque pointers a function pointer is just `ptr`, so passing
            // null and passing a routine are the same call shape.
            drop: declare("khora_drop", void.fn_type(&[ptr.into(), ptr.into()], false)),
            contain_enabled: declare("khora_contain_enabled", i32t.fn_type(&[], false)),
            export_call: declare(
                "khora_export_call",
                i64t.fn_type(&[ptr.into(), ptr.into()], false),
            ),
            single_threaded: declare("khora_single_threaded", void.fn_type(&[], false)),
            drop_last: declare(
                "khora_drop_last",
                void.fn_type(&[ptr.into(), ptr.into(), i64t.into()], false),
            ),
            drop_reuse: declare(
                "khora_drop_reuse",
                ptr.fn_type(&[ptr.into(), ptr.into()], false),
            ),
            free_reuse: declare("khora_free_reuse", void.fn_type(&[ptr.into()], false)),
            alloc_reuse: declare(
                "khora_alloc_reuse",
                ptr.fn_type(&[ptr.into(), i64t.into(), i32t.into()], false),
            ),
            print_int: declare("khora_print_int", void.fn_type(&[i64t.into()], false)),
            print_float: declare(
                "khora_print_float",
                void.fn_type(&[ctx.f64_type().into()], false),
            ),
            // C `_Bool`, so exactly one byte holding 0 or 1. Generated code
            // zero-extends its `i1`; any other bit pattern in that byte is
            // undefined behavior rather than a merely surprising result.
            print_bool: declare("khora_print_bool", void.fn_type(&[i8t.into()], false)),
            print_str: declare("khora_print_str", void.fn_type(&[ptr.into(), i64t.into()], false)),
            str_eq: declare(
                "khora_str_eq",
                i8t.fn_type(&[ptr.into(), i64t.into(), ptr.into(), i64t.into()], false),
            ),
            str_find: declare(
                "khora_str_find",
                i64t.fn_type(&[ptr.into(), i64t.into(), ptr.into(), i64t.into()], false),
            ),
            region_open: declare("khora_region_open", ptr.fn_type(&[], false)),
            region_defer: declare(
                "khora_region_defer",
                void.fn_type(&[ptr.into(), ptr.into(), ptr.into()], false),
            ),
            // Declared with the `drop_fields` shape, because that is what it
            // is: `khora_drop` calls it when the last reference to a region
            // goes, and the finalizers run there.
            region_release: declare("khora_region_release", void.fn_type(&[ptr.into()], false)),
            region_root: declare("khora_region_root", ptr.fn_type(&[], false)),
            cancelled: declare("khora_cancelled", i8t.fn_type(&[], false)),
            cancel_stop: declare("khora_cancel_stop", void.fn_type(&[], false)),
            shared_open: declare(
                "khora_shared_open",
                ptr.fn_type(&[i64t.into(), ctx.bool_type().into(), ptr.into()], false),
            ),
            shared_get: declare("khora_shared_get", i64t.fn_type(&[ptr.into()], false)),
            shared_set: declare(
                "khora_shared_set",
                void.fn_type(&[ptr.into(), i64t.into()], false),
            ),
            shared_update: declare(
                "khora_shared_update",
                i64t.fn_type(&[ptr.into(), ptr.into(), ptr.into()], false),
            ),
            shared_modify: declare(
                "khora_shared_modify",
                i64t.fn_type(&[ptr.into(), ptr.into(), ptr.into(), ptr.into()], false),
            ),
            shared_release: declare("khora_shared_release", void.fn_type(&[ptr.into()], false)),
            channel_open: declare(
                "khora_channel_open",
                ptr.fn_type(
                    &[i64t.into(), i64t.into(), ctx.bool_type().into(), ptr.into()],
                    false,
                ),
            ),
            channel_send: declare(
                "khora_channel_send",
                ctx.bool_type().fn_type(&[ptr.into(), i64t.into()], false),
            ),
            channel_receive: declare(
                "khora_channel_receive",
                ctx.bool_type().fn_type(&[ptr.into(), ptr.into()], false),
            ),
            channel_poll: declare(
                "khora_channel_poll",
                ctx.bool_type().fn_type(&[ptr.into(), ptr.into()], false),
            ),
            channel_close: declare("khora_channel_close", void.fn_type(&[ptr.into()], false)),
            channel_depth: declare("khora_channel_depth", i64t.fn_type(&[ptr.into()], false)),
            channel_release: declare(
                "khora_channel_release",
                void.fn_type(&[ptr.into()], false),
            ),
            fiber_spawn: declare(
                "khora_fiber_spawn",
                ptr.fn_type(
                    &[
                        ptr.into(),
                        ptr.into(),
                        ptr.into(),
                        ptr.into(),
                        ctx.bool_type().into(),
                        ptr.into(),
                    ],
                    false,
                ),
            ),
            fiber_join: declare(
                "khora_fiber_join",
                ctx.i32_type().fn_type(&[ptr.into(), ptr.into()], false),
            ),
            fiber_detach: declare("khora_fiber_detach", void.fn_type(&[ptr.into()], false)),
            fiber_wait: declare("khora_fiber_wait", void.fn_type(&[ptr.into()], false)),
            fiber_cancel: declare("khora_fiber_cancel", void.fn_type(&[ptr.into()], false)),
            fiber_release: declare("khora_fiber_release", void.fn_type(&[ptr.into()], false)),
            fibers_open: declare("khora_fibers_open", ptr.fn_type(&[], false)),
            fibers_bounded: declare(
                "khora_fibers_open_bounded",
                ptr.fn_type(&[i64t.into()], false),
            ),
            fibers_adopt: declare(
                "khora_fibers_adopt",
                void.fn_type(&[ptr.into(), ptr.into()], false),
            ),
            fibers_wait: declare("khora_fibers_wait", i64t.fn_type(&[ptr.into()], false)),
            fibers_release: declare("khora_fibers_release", void.fn_type(&[ptr.into()], false)),
            args_set: declare(
                "khora_args_set",
                void.fn_type(&[ctx.i32_type().into(), ptr.into()], false),
            ),
            utf8_valid: declare(
                "khora_utf8_valid",
                ctx.bool_type().fn_type(&[ptr.into(), i64t.into()], false),
            ),
            array_new: declare(
                "khora_array_new",
                ptr.fn_type(
                    // `len` is a Khora `Int` and stays 64-bit; `fill` is a
                    // `usize` on the Rust side and follows the pointer.
                    &[i64t.into(), i64t.into(), i8t.into(), i8t.into(), ptr.into()],
                    false,
                ),
            ),
            array_release: declare("khora_array_release", void.fn_type(&[ptr.into()], false)),
            bounds_fail: declare(
                "khora_bounds_fail",
                void.fn_type(&[i64t.into(), i64t.into()], false),
            ),
            overflow: declare("khora_overflow", void.fn_type(&[ptr.into(), i64t.into()], false)),
            unhandled: declare(
                "khora_unhandled",
                void.fn_type(&[ptr.into(), i64t.into()], false),
            ),
            assert_failed: declare(
                "khora_assert_failed",
                void.fn_type(&[i32t.into(), i32t.into()], false),
            ),
            begin: declare("khora_begin", void.fn_type(&[], false)),
            test_register: declare(
                "khora_test_register",
                void.fn_type(&[ptr.into(), i64t.into(), ptr.into(), ptr.into()], false),
            ),
            test_run: declare("khora_test_run", i32t.fn_type(&[], false)),
            safepoint: declare("khora_safepoint", void.fn_type(&[], false)),
            bench_register: declare(
                "khora_bench_register",
                void.fn_type(&[ptr.into(), i64t.into(), ptr.into(), ptr.into()], false),
            ),
            bench_run: declare("khora_bench_run", i32t.fn_type(&[], false)),
            region_close_root: declare("khora_region_close_root", void.fn_type(&[], false)),
            trap: declare("llvm.trap", void.fn_type(&[], false)),
        }
    }
}

/// A pointer to `offset` bytes past an object pointer.
///
/// Deliberately a byte GEP over `i8` rather than a structural GEP over a named
/// struct type. LLVM has opaque pointers, so a GEP's element type is a claim
/// the code generator makes rather than something carried by the pointer — and
/// the claim we want to make is exactly the byte offset the runtime documents.
/// Building a struct type here would restate the layout in a second place, and
/// a wrong restatement produces the silent corruption the contract exists to
/// prevent.
pub fn byte_offset<'ctx>(
    ctx: &'ctx Context,
    builder: &inkwell::builder::Builder<'ctx>,
    object: PointerValue<'ctx>,
    offset: u64,
    name: &str,
) -> PointerValue<'ctx> {
    let index = ctx.i64_type().const_int(offset, false);
    // SAFETY (inkwell's, not Rust's): a GEP with a wrong index is a segfault
    // waiting to happen, which is why inkwell marks this `unsafe`. The offset
    // is always `FIELD_OFFSET + 8 * i` for a field the object's variant
    // declares, or `TAG_OFFSET`, all of which are inside the allocation.
    unsafe {
        builder
            .build_in_bounds_gep(ctx.i8_type(), object, &[index], name)
            .expect("byte GEP")
    }
}

/// Loads an object's variant tag.
pub fn load_tag<'ctx>(
    ctx: &'ctx Context,
    builder: &inkwell::builder::Builder<'ctx>,
    object: PointerValue<'ctx>,
) -> IntValue<'ctx> {
    let slot = byte_offset(ctx, builder, object, TAG_OFFSET, "tag.ptr");
    builder
        .build_load(ctx.i32_type(), slot, "tag")
        .expect("loading a tag")
        .into_int_value()
}

/// A pointer to field `index` of an object.
pub fn field_pointer<'ctx>(
    ctx: &'ctx Context,
    builder: &inkwell::builder::Builder<'ctx>,
    object: PointerValue<'ctx>,
    index: u64,
) -> PointerValue<'ctx> {
    byte_offset(ctx, builder, object, FIELD_OFFSET + FIELD_WORD * index, "field.ptr")
}

/// The same, for a field whose index is only known at run time.
///
/// An array element. Two differences from [`field_pointer`]: the byte offset
/// is computed rather than folded, and the scale is the element's own width
/// rather than a word — an `Array<U8>` is a byte per element, because a byte
/// buffer that costs eight bytes a byte is not one. The layout claim is
/// otherwise identical and is stated once over there.
///
/// `stride` is 1, 2, 4 or 8, all of which divide a word, so an element is
/// aligned whenever the header is — and the header is a whole number of words
/// by construction.
pub fn element_pointer<'ctx>(
    ctx: &'ctx Context,
    builder: &inkwell::builder::Builder<'ctx>,
    object: PointerValue<'ctx>,
    index: inkwell::values::IntValue<'ctx>,
    stride: u64,
    base: u64,
) -> PointerValue<'ctx> {
    let i64t = ctx.i64_type();
    let scaled = builder
        .build_int_mul(index, i64t.const_int(stride, false), "field.bytes")
        .expect("scaling a field index");
    let offset = builder
        .build_int_add(scaled, i64t.const_int(FIELD_OFFSET + base, false), "field.offset")
        .expect("offsetting past the header");
    // SAFETY: **the obligation is the generated program's, not this
    // function's**, which is what makes it different from every other `unsafe`
    // in this repository. An `inbounds` GEP that leaves the object is undefined
    // behaviour *in the program being compiled*, and nothing a Rust reader can
    // see here discharges it -- what does is the bounds check emitted before
    // this address is used. Delete that check and this compiler still builds,
    // still passes its own tests, and starts emitting programs that read off
    // the end of a string. `docs/design/soundness.md`.
    unsafe {
        builder
            .build_in_bounds_gep(ctx.i8_type(), object, &[offset], "element.ptr")
            .expect("addressing an element")
    }
}
