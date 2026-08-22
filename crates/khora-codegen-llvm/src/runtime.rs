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
