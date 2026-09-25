//! Values that are one object for the whole program.
//!
//! A string literal, and a constructor with no fields. Both are entirely
//! described by their contents, so every occurrence can be the same address.
//! Both are **immortal**: the count word carries `KHORA_IMMORTAL`, nothing
//! counts them and nothing frees them. `adjust_count` and the runtime's
//! `khora_dup`, `khora_drop` and `khora_drop_reuse` test the word before they
//! write it.

use super::*;

/// The count word a static is born with.
///
/// The immortal bit, which every count operation tests, plus 2^40, which is
/// what `khora_refcount` reports for a static once it has masked the bit off.
const IMMORTAL_COUNT: u64 = khora_rt::KHORA_IMMORTAL | (1 << 40);

impl<'ctx> Backend<'ctx> {
    /// The one object a field-less constructor ever produces.
    ///
    /// A case with no fields is entirely described by its tag, so every
    /// `Option::None` in a program is the same value and there is no reason for
    /// them to be different addresses. One private global per `(type, case)`,
    /// with the header `khora_alloc` would have written and no field storage
    /// after it.
    ///
    /// Keyed by type *and* case, because a case name alone is not unique — two
    /// types may each have a `None`, and giving them one object would make a
    /// tag comparison say they matched.
    ///
    /// Immortal, for the reason [`Backend::static_string`] gives.
    pub fn static_variant(&mut self, owner: &str, case: &str, tag: u32) -> PointerValue<'ctx> {
        let key = format!("{owner}::{case}");
        if let Some(found) = self.static_variants.get(&key) {
            return *found;
        }

        let i64_type = self.ctx.i64_type();
        let i32_type = self.ctx.i32_type();
        // The header alone: a refcount, a tag, and a field area of zero bytes.
        let shape =
            self.ctx.struct_type(&[i64_type.into(), i32_type.into(), i32_type.into()], false);
        let initial = self.ctx.const_struct(
            &[
                i64_type.const_int(IMMORTAL_COUNT, false).into(),
                i32_type.const_int(u64::from(tag), false).into(),
                i32_type.const_zero().into(),
            ],
            false,
        );

        let global = self.module.add_global(shape, None, &format!("kh$case${key}"));
        global.set_initializer(&initial);
        global.set_linkage(Linkage::Private);
        global.set_constant(true);
        global.set_alignment(8);

        let pointer = global.as_pointer_value();
        self.static_variants.insert(key, pointer);
        pointer
    }

    /// One `String` object per distinct literal, in static storage.
    ///
    /// See [`Lower::string_literal`] for why. Cached by text, so a literal
    /// repeated across a program is one object however many times it is
    /// written.
    ///
    /// **Immortal, and read-only.** Every fiber on every core reaches the same
    /// `""` and `"application/json"`, so a count written on each `dup` and
    /// `drop` made those cache lines bounce between cores. Skipping those
    /// counts raised a four-core string handler's throughput 1.83×. The
    /// count word carries `KHORA_IMMORTAL` and nothing
    /// writes it. So the global is `constant`, and a count operation that
    /// forgot the test faults at the write. Without that, it would corrupt a
    /// literal that every later use reads.
    pub fn static_string(&mut self, text: &str) -> PointerValue<'ctx> {
        if let Some(found) = self.static_strings.get(text) {
            return *found;
        }

        let bytes = text.as_bytes();
        let len = bytes.len() as u64;
        let i64_type = self.ctx.i64_type();
        let i32_type = self.ctx.i32_type();
        let byte_array = self.ctx.i8_type().array_type(len as u32);

        // The header, then the length field, then the bytes: exactly what
        // `khora_alloc` would have produced.
        let shape = self.ctx.struct_type(
            &[i64_type.into(), i32_type.into(), i32_type.into(), i64_type.into(), byte_array.into()],
            false,
        );
        let initial = self.ctx.const_struct(
            &[
                i64_type.const_int(IMMORTAL_COUNT, false).into(),
                i32_type.const_int(runtime::STRING_TAG, false).into(),
                i32_type.const_int(runtime::FIELD_WORD + len, false).into(),
                i64_type.const_int(len, false).into(),
                self.ctx.const_string(bytes, false).into(),
            ],
            false,
        );

        let global = self.module.add_global(shape, None, "kh$string");
        global.set_initializer(&initial);
        global.set_linkage(Linkage::Private);
        global.set_constant(true);
        global.set_alignment(8);

        let pointer = global.as_pointer_value();
        self.static_strings.insert(text.to_string(), pointer);
        pointer
    }
}
