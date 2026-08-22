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
    /// `void *khora_fiber_spawn(void *body, void (*glue)(void *),
    ///                            uint32_t (*call)(const void *, void *, uint64_t *))`
    pub fiber_spawn: FunctionValue<'ctx>,
    /// `void khora_fiber_join(void *fiber)`
    pub fiber_join: FunctionValue<'ctx>,
    /// `void khora_fiber_cancel(void *fiber)`
    pub fiber_cancel: FunctionValue<'ctx>,
    /// `void khora_fiber_release(void *fiber)` — a `drop_fields` callback.
    pub fiber_release: FunctionValue<'ctx>,
    /// `void *khora_fibers_open(void)`
    pub fibers_open: FunctionValue<'ctx>,
    /// `void khora_fibers_adopt(void *fibers, void *fiber)`
    pub fibers_adopt: FunctionValue<'ctx>,
    /// `void khora_fibers_wait(void *fibers)`
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
    /// `void khora_test_register(const uint8_t *name, size_t len, const void *code,
    ///                             uint32_t (*call)(const void *, uint64_t *))`
    pub test_register: FunctionValue<'ctx>,
    /// `int32_t khora_test_run(void)`
    pub test_run: FunctionValue<'ctx>,
    /// `void khora_region_close_root(void)`
    pub region_close_root: FunctionValue<'ctx>,
    /// `llvm.trap`, for a branch that exhaustiveness says cannot be taken.
    pub trap: FunctionValue<'ctx>,
}

impl<'ctx> Runtime<'ctx> {
    /// Declares the runtime interface into a fresh module.
    pub fn declare(ctx: &'ctx Context, module: &Module<'ctx>) -> Runtime<'ctx> {
        let void = ctx.void_type();
        let i64t = ctx.i64_type();
        let i32t = ctx.i32_type();
        let i8t = ctx.i8_type();
        let ptr = ctx.ptr_type(AddressSpace::default());

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
            fiber_spawn: declare(
                "khora_fiber_spawn",
                ptr.fn_type(&[ptr.into(), ptr.into(), ptr.into()], false),
            ),
            fiber_join: declare("khora_fiber_join", void.fn_type(&[ptr.into()], false)),
            fiber_cancel: declare("khora_fiber_cancel", void.fn_type(&[ptr.into()], false)),
            fiber_release: declare("khora_fiber_release", void.fn_type(&[ptr.into()], false)),
            fibers_open: declare("khora_fibers_open", ptr.fn_type(&[], false)),
            fibers_adopt: declare(
                "khora_fibers_adopt",
                void.fn_type(&[ptr.into(), ptr.into()], false),
            ),
            fibers_wait: declare("khora_fibers_wait", void.fn_type(&[ptr.into()], false)),
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
            test_register: declare(
                "khora_test_register",
                void.fn_type(&[ptr.into(), i64t.into(), ptr.into(), ptr.into()], false),
            ),
            test_run: declare("khora_test_run", i32t.fn_type(&[], false)),
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
    unsafe {
        builder
            .build_in_bounds_gep(ctx.i8_type(), object, &[offset], "element.ptr")
            .expect("addressing an element")
    }
}
