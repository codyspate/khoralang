//! Strings: building them, taking them apart, and lending their bytes.
//!
//! The layout is a length field and the bytes after it, so most of this is
//! generated code walking that. What is not: `String::with_data`, which lends a
//! pointer and a length for exactly the duration of a call, and is how a string
//! reaches a foreign function without anything escaping. `docs/design/ffi.md`.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// `a + b` on two strings.
    ///
    /// Generated rather than a runtime call, for the same reason `String::bytes`
    /// is: the string layout is the code generator's business, and the runtime
    /// stays a function of the data it is handed. `khora_alloc` and two
    /// `memcpy`s are the whole of it.
    ///
    /// Both operands are released afterwards. Neither is reused even when one
    /// is empty — returning the other would be one fewer allocation and a
    /// second rule about when the result shares storage, and a string is
    /// immutable so nothing would notice, but nothing needs it yet either.
    pub(super) fn concat(&mut self, left: BasicValueEnum<'ctx>, right: BasicValueEnum<'ctx>) -> Flow<'ctx> {
        let i64_type = self.be.ctx.i64_type();
        let (a, b) = (left.into_pointer_value(), right.into_pointer_value());
        let a_len = self.string_length(a);
        let b_len = self.string_length(b);
        let total = self
            .be
            .builder
            .build_int_add(a_len, b_len, "concat.len")
            .expect("adding two string lengths");

        let size = self
            .be
            .builder
            .build_int_add(total, i64_type.const_int(runtime::FIELD_WORD, false), "concat.size")
            .expect("sizing the result");
        let object = self
            .be
            .builder
            .build_call(
                self.be.rt.alloc,
                &[size.into(), self.be.ctx.i32_type().const_int(STRING_TAG, false).into()],
                "concat",
            )
            .expect("allocating a string")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();

        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, object, STRING_LEN_FIELD);
        self.be
            .builder
            .build_store(length_slot, total)
            .expect("storing the result's length");

        let out = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            STRING_BYTES_OFFSET,
            "concat.bytes",
        );
        for (source, len, offset) in [(a, a_len, None), (b, b_len, Some(a_len))] {
            let from = runtime::byte_offset(
                self.be.ctx,
                &self.be.builder,
                source,
                STRING_BYTES_OFFSET,
                "part.bytes",
            );
            let to = match offset {
                None => out,
                Some(at) => unsafe {
                    self.be
                        .builder
                        .build_in_bounds_gep(self.be.ctx.i8_type(), out, &[at], "concat.second")
                        .expect("addressing the second half")
                },
            };
            // Alignment 1: the bytes follow a length word so they are in fact
            // word-aligned, but the *second* copy starts wherever the first one
            // ended, which is any offset at all.
            self.be
                .builder
                .build_memcpy(to, 1, from, 1, len)
                .expect("copying a string");
        }

        self.drop(left, &Type::Str);
        self.drop(right, &Type::Str);
        Some(object.into())
    }

    /// `Array::with_data` and `String::with_data`: lend the elements to a body
    /// as a pointer and a count.
    ///
    /// **The lifetime is the call, and that is the whole design.** The obvious
    /// alternative — `Array::data(self) -> Ptr`, returning a bare pointer — is
    /// a dangling pointer waiting to happen: Perceus releases the array at its
    /// last *use*, and that use is the `data` call itself, so the array can be
    /// freed before the pointer is read. There is no scope that would fix it
    /// either. The innermost one is wrong for `if c { data(a) } else { data(b) }`,
    /// and the function's own is wrong for a loop, which would accumulate one
    /// live buffer per iteration. A body is the only bound that is right in all
    /// three.
    ///
    /// The array is released by a *scope* rather than by a statement after the
    /// call, so a body that raises does not leak it. That is errata 34, which
    /// has now been the answer three times.
    ///
    /// What this does not do is stop the pointer escaping — a body can write it
    /// into a `mut` field and read it later. That is the same line Rust draws:
    /// obtaining a pointer is safe, and what happens on the far side of the
    /// boundary is the binding author's responsibility. What it removes is the
    /// *accidental* case, the one the compiler creates behind your back.
    pub(super) fn with_data(&mut self, site: ExprId, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [subject, body] = args else {
            return self.fail("`with_data` takes a body to lend the data to", range);
        };
        let subject_ty = self.types.of(*subject).clone();
        let Some(shape) = FnShape::of(self.types.of(*body)) else {
            return self.fail("`with_data` takes a function to run", range);
        };

        // A `String` lends its bytes; an `Array<A>` lends its elements, and the
        // count is the element count rather than a byte count — the same number
        // `Array::length` gives, so the two never disagree.
        let (elements, count) = match &subject_ty {
            Type::Str => {
                let object = self.expr(*subject)?.into_pointer_value();
                let length = self.string_length(object);
                let bytes = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::STRING_BYTES_OFFSET,
                    "str.bytes",
                );
                self.scopes.push(vec![Cleanup::Temp(object.into(), Type::Str)]);
                (bytes, length)
            }
            _ => {
                let element = self.array_element(&subject_ty, range)?;
                // An array of Khora objects is an array of counted pointers,
                // and handing those to a foreign function is the mistake the
                // whole boundary exists to prevent.
                if is_boxed(&element) {
                    return self.fail(
                        format!(
                            "an `Array<{element}>` holds reference-counted objects, so its \
                             elements cannot be lent across the C ABI — only an array of \
                             numbers can. `docs/design/ffi.md`"
                        ),
                        range,
                    );
                }
                let object = self.expr(*subject)?.into_pointer_value();
                let length_slot = runtime::field_pointer(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::ARRAY_LEN_FIELD,
                );
                let length = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), length_slot, "array.len")
                    .expect("reading an array's length")
                    .into_int_value();
                let elements = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
                    "array.elements",
                );
                self.scopes.push(vec![Cleanup::Temp(object.into(), subject_ty.clone())]);
                (elements, length)
            }
        };

        let closure = self.expr(*body)?.into_pointer_value();
        let Invoked { raw, fallible } = self.invoke_closure_at(
            site,
            *body,
            closure,
            &shape,
            vec![elements.into(), count.into()],
            range,
        )?;
        let result = if fallible {
            let tagged = raw.expect("a fallible body returns a tagged value");
            self.split_tagged(tagged, &shape.ret, range)?
        } else {
            match shape.ret {
                Type::Unit => self.be.unit_value(),
                _ => raw.unwrap_or_else(|| self.be.unit_value()),
            }
        };
        // The closure's own scope, then the one holding what was lent.
        self.leave_scope();
        self.leave_scope();
        Some(result)
    }

    /// The bytes of an `Array<U8>`, as a pointer and a length.
    ///
    /// Shared by `is_utf8` and `from_bytes`, which want the same three values
    /// out of the same object and would otherwise each work them out.
    pub(super) fn byte_array(
        &mut self,
        array: ExprId,
        what: &str,
        range: TextRange,
    ) -> Option<(PointerValue<'ctx>, PointerValue<'ctx>, IntValue<'ctx>, Type)> {
        let array_ty = self.types.of(array).clone();
        let element = self.array_element(&array_ty, range)?;
        if element != Type::Fixed(khora_types::IntKind { signed: false, bits: 8 }) {
            self.fail(format!("`{what}` is about bytes, and `{element}` is not one"), range)?;
        }
        let object = self.expr(array)?.into_pointer_value();
        let length_slot = runtime::field_pointer(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::ARRAY_LEN_FIELD,
        );
        let length = self
            .be
            .builder
            .build_load(self.be.ctx.i64_type(), length_slot, "array.len")
            .expect("reading an array's length")
            .into_int_value();
        let elements = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
            "array.elements",
        );
        Some((object, elements, length, array_ty))
    }

    /// `Array::is_utf8`: whether these bytes are a `String`'s worth.
    ///
    /// Separate from the conversion, and paired with it the way `Array::length`
    /// is paired with `Array::get`: the check is how you avoid the trap, and
    /// having both means the *policy* — raise, substitute, give up — is written
    /// in Khora by whoever knows which is right.
    pub(super) fn is_utf8(&mut self, array: ExprId, range: TextRange) -> Flow<'ctx> {
        let (object, elements, length, array_ty) =
            self.byte_array(array, "Array::is_utf8", range)?;
        let answer = self
            .be
            .builder
            .build_call(self.be.rt.utf8_valid, &[elements.into(), length.into()], "utf8")
            .expect("checking for UTF-8")
            .try_as_basic_value()
            .basic()
            .expect("khora_utf8_valid returns a _Bool")
            .into_int_value();
        self.release_unless_lent(array, object.into(), &array_ty);
        // A C `_Bool` is one byte; Khora's `Bool` is an `i1`.
        let narrowed = self
            .be
            .builder
            .build_int_truncate_or_bit_cast(answer, self.be.ctx.bool_type(), "utf8.bit")
            .expect("narrowing a C bool");
        Some(narrowed.into())
    }

    /// `String::from_bytes`: the same bytes, as a `String`.
    ///
    /// **Stops the program if they are not UTF-8**, which is the same bargain
    /// `Array::get` makes about an index: the check exists — `Array::is_utf8` —
    /// and calling this without it is the mistake. Returning an `Option` was
    /// the alternative and would have put the decision in the wrong place: what
    /// to *do* about bytes that are not text depends entirely on where they
    /// came from, and only the caller knows.
    pub(super) fn string_from_bytes(&mut self, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [array] = args else {
            return self.fail("`String::from_bytes` takes an `Array<U8>`", range);
        };
        let (object, elements, length, array_ty) =
            self.byte_array(*array, "String::from_bytes", range)?;

        let valid = self
            .be
            .builder
            .build_call(self.be.rt.utf8_valid, &[elements.into(), length.into()], "utf8")
            .expect("checking for UTF-8")
            .try_as_basic_value()
            .basic()
            .expect("khora_utf8_valid returns a _Bool")
            .into_int_value();
        let ok = self
            .be
            .builder
            .build_int_truncate_or_bit_cast(valid, self.be.ctx.bool_type(), "utf8.bit")
            .expect("narrowing a C bool");
        self.guard(ok, "these bytes are not UTF-8, so they are not a String");

        // The check split the block, so the addresses are recomputed on the
        // side of the branch that continues.
        let elements = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
            "array.elements",
        );
        let i64_type = self.be.ctx.i64_type();
        let size = self
            .be
            .builder
            .build_int_add(length, i64_type.const_int(runtime::FIELD_WORD, false), "str.size")
            .expect("sizing a string")
        ;
        let string = self
            .be
            .builder
            .build_call(
                self.be.rt.alloc,
                &[size.into(), self.be.ctx.i32_type().const_int(STRING_TAG, false).into()],
                "str",
            )
            .expect("allocating a string")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();
        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, string, STRING_LEN_FIELD);
        self.be
            .builder
            .build_store(length_slot, length)
            .expect("storing a string length");
        let into = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            string,
            STRING_BYTES_OFFSET,
            "str.bytes",
        );
        self.be
            .builder
            .build_memcpy(into, 1, elements, 1, length)
            .expect("copying the bytes");

        self.drop(object.into(), &array_ty);
        Some(string.into())
    }

    /// `String::with_c_string`: lend the bytes with a zero byte after them.
    ///
    /// Every function in the C library that takes a string takes a
    /// `const char *` and finds the end by looking for a zero. A Khora string
    /// knows its length instead and has no zero to find, so a copy is the only
    /// honest answer — and it is a copy either way, since a borrowed view could
    /// not have the extra byte appended to it.
    ///
    /// The copy is an `Array<U8>` of `len + 1`, which `khora_array_new` has
    /// already zeroed, so the terminator is written by not writing anything.
    /// It is released by the same scope discipline `with_data` uses, so a body
    /// that raises does not leak it.
    ///
    /// A string containing an interior zero is *not* rejected. C will see a
    /// shorter string than Khora has; that is what C strings are, and refusing
    /// it here would be inventing a rule the boundary does not have.
    pub(super) fn with_c_string(&mut self, site: ExprId, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [subject, body] = args else {
            return self.fail("`with_c_string` takes a body to lend the string to", range);
        };
        let Some(shape) = FnShape::of(self.types.of(*body)) else {
            return self.fail("`with_c_string` takes a function to run", range);
        };

        let object = self.expr(*subject)?.into_pointer_value();
        let length = self.string_length(object);
        let bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_BYTES_OFFSET,
            "str.bytes",
        );

        let i8_type = self.be.ctx.i8_type();
        let with_room = self
            .be
            .builder
            .build_int_add(length, self.be.ctx.i64_type().const_int(1, false), "c.len")
            .expect("room for the terminator");
        let buffer = self
            .be
            .builder
            .build_call(
                self.be.rt.array_new,
                &[
                    with_room.into(),
                    self.be.ctx.i64_type().const_zero().into(),
                    i8_type.const_int(1, false).into(),
                    i8_type.const_zero().into(),
                    self.be.null_pointer().into(),
                ],
                "c.string",
            )
            .expect("allocating a C string")
            .try_as_basic_value()
            .basic()
            .expect("an array is a value")
            .into_pointer_value();
        let elements = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            buffer,
            runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
            "c.bytes",
        );
        self.be
            .builder
            .build_memcpy(elements, 1, bytes, 1, length)
            .expect("copying a string's bytes");

        // The string is done with as soon as its bytes are copied; the buffer
        // has to outlive the call, and a scope is what makes that true on the
        // raising path as well.
        self.drop(object.into(), &Type::Str);
        // The compiler's own array, named rather than resolved: this is
        // chosen here to pick drop glue, and nothing unifies it with a type a
        // program wrote.
        let buffer_ty = Type::Adt {
            name: runtime::ARRAY_TYPE.to_string(),
            home: None,
            args: vec![Type::Fixed(khora_types::IntKind { signed: false, bits: 8 })],
        };
        self.scopes.push(vec![Cleanup::Temp(buffer.into(), buffer_ty)]);

        let closure = self.expr(*body)?.into_pointer_value();
        let Invoked { raw, fallible } =
            self.invoke_closure_at(site, *body, closure, &shape, vec![elements.into()], range)?;
        let result = if fallible {
            let tagged = raw.expect("a fallible body returns a tagged value");
            self.split_tagged(tagged, &shape.ret, range)?
        } else {
            match shape.ret {
                Type::Unit => self.be.unit_value(),
                _ => raw.unwrap_or_else(|| self.be.unit_value()),
            }
        };
        self.leave_scope();
        self.leave_scope();
        Some(result)
    }

    /// `String::byte_length`, `String::byte` and `String::bytes`.
    ///
    /// **A string's length is in bytes, and its index is a byte index.** Named
    /// so, because a `String` is UTF-8 and a character is one to four of these
    /// — a `length` that quietly meant one of the two would be wrong for half
    /// its callers and silent about which half. Anything that wants characters
    /// wants a decoder, and that is a library on top of this rather than a
    /// different meaning for the same word.
    ///
    /// `bytes` copies. A string is immutable and an array is not, so handing
    /// out a view would let one be edited through the other; and the two have
    /// different headers besides.
    ///
    /// There is deliberately no `from_bytes` yet. Going the other way has to
    /// answer what happens to bytes that are not UTF-8, and the honest answer
    /// is a `Result` rather than a trap — bytes off a socket are data, not a
    /// programmer's mistake. That wants the error channel wired into an
    /// intrinsic, which is phase 7's problem and not a decision to make in
    /// passing.
    pub(super) fn string_intrinsic(&mut self, name: &str, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [subject, rest @ ..] = args else {
            return self.fail(format!("`String::{name}` takes a string"), range);
        };
        let object = self.expr(*subject)?.into_pointer_value();
        let length = self.string_length(object);
        let bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_BYTES_OFFSET,
            "str.bytes",
        );

        let result = match (name, rest) {
            ("byte_length", []) => length.into(),
            ("byte", [index]) => {
                let at = self.expr(*index)?.into_int_value();
                self.check_index(at, length);
                // Recomputed after the check, because the check split the block
                // and the pointer above belongs to the one before it.
                let bytes = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::STRING_BYTES_OFFSET,
                    "str.bytes",
                );
                let slot = unsafe {
                    self.be
                        .builder
                        .build_in_bounds_gep(self.be.ctx.i8_type(), bytes, &[at], "byte.ptr")
                        .expect("addressing a byte")
                };
                self.be
                    .builder
                    .build_load(self.be.ctx.i8_type(), slot, "byte")
                    .expect("reading a byte")
            }
            ("bytes", []) => {
                let i8_type = self.be.ctx.i8_type();
                let array = self
                    .be
                    .builder
                    .build_call(
                        self.be.rt.array_new,
                        &[
                            length.into(),
                            self.be.ctx.i64_type().const_zero().into(),
                            i8_type.const_int(1, false).into(),
                            i8_type.const_zero().into(),
                            self.be.null_pointer().into(),
                        ],
                        "str.array",
                    )
                    .expect("allocating a byte array")
                    .try_as_basic_value()
                    .basic()
                    .expect("an array is a value")
                    .into_pointer_value();
                let elements = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    array,
                    runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
                    "array.elements",
                );
                // Alignment 1 on both sides: the destination is word-aligned and
                // the source usually is, but neither is *guaranteed* to be by
                // anything written down, and claiming an alignment the data does
                // not have is undefined behaviour rather than a slow copy.
                self.be
                    .builder
                    .build_memcpy(elements, 1, bytes, 1, length)
                    .expect("copying a string's bytes");
                array.into()
            }
            ("slice", [from, to]) => self.string_slice(object, length, *from, *to)?,
            ("find", [needle, from]) => self.string_find(object, length, *needle, *from)?,
            _ => {
                return self.fail(
                    format!("`String::{name}` is not a string operation the backend knows"),
                    range,
                )
            }
        };

        self.release_unless_lent(*subject, object.into(), &Type::Str);
        Some(result)
    }

    /// `String::find`: where `needle` first occurs at or after `from`, or -1.
    ///
    /// **The offset is the point of it.** Splitting a header block by walking
    /// the string and slicing off the front costs a copy of everything that is
    /// left, once per header — quadratic in the number of headers, which for a
    /// request the eight-kilobyte limit allows is a real amount of work rather
    /// than a theoretical one. Searching from a cursor instead copies only the
    /// pieces that are kept.
    ///
    /// The search itself is `khora_str_find`, reached directly rather than
    /// through `String::with_data`: that lends the bytes to a closure, and the
    /// two nested closures a search needs are two heap allocations, which
    /// measured at 500 nanoseconds against 40 for the call on its own.
    pub(super) fn string_find(
        &mut self,
        object: PointerValue<'ctx>,
        length: IntValue<'ctx>,
        needle: ExprId,
        from: ExprId,
    ) -> Flow<'ctx> {
        let sought = self.expr(needle)?.into_pointer_value();
        let sought_length = self.string_length(sought);
        let sought_bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            sought,
            runtime::STRING_BYTES_OFFSET,
            "needle.bytes",
        );
        let from = self.expr(from)?.into_int_value();
        let i64_type = self.be.ctx.i64_type();
        // Clamped rather than checked, the same as `String::slice`: searching
        // from past the end finds nothing, which is the answer, not a mistake.
        let start = self.clamp(from, i64_type.const_zero(), length, "find.from");
        let bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_BYTES_OFFSET,
            "str.bytes",
        );
        let hay = unsafe {
            self.be
                .builder
                .build_in_bounds_gep(self.be.ctx.i8_type(), bytes, &[start], "find.hay")
                .expect("addressing the first byte to search")
        };
        let remaining = self
            .be
            .builder
            .build_int_sub(length, start, "find.remaining")
            .expect("how much is left");
        let at = self
            .be
            .builder
            .build_call(
                self.be.rt.str_find,
                &[hay.into(), remaining.into(), sought_bytes.into(), sought_length.into()],
                "find",
            )
            .expect("searching a string")
            .try_as_basic_value()
            .basic()
            .expect("khora_str_find returns an index")
            .into_int_value();
        self.drop(sought.into(), &Type::Str);

        // The runtime answers relative to where it was pointed. A hit is
        // shifted back to an index into the whole string; a miss stays -1.
        let shifted = self
            .be
            .builder
            .build_int_add(at, start, "find.absolute")
            .expect("shifting an index")
        ;
        let missed = self
            .be
            .builder
            .build_int_compare(IntPredicate::SLT, at, i64_type.const_zero(), "find.missed")
            .expect("was it found");
        Some(
            self.be
                .builder
                .build_select(missed, at, shifted, "find.at")
                .expect("choosing an answer"),
        )
    }

    /// `String::slice`: the bytes from `from` up to `to`, as a new string.
    ///
    /// **One allocation and one copy.** Written in Khora it built an
    /// `Array<U8>` and handed that to `String::from_bytes`, so every slice was
    /// two allocations, two copies and a walk over the bytes to re-establish
    /// that they were UTF-8 — 2,915 nanoseconds for eighty bytes. That is the
    /// single most expensive thing a request parser does, because
    /// `String::split_once` is two of them and parsing a request is a dozen
    /// splits.
    ///
    /// Both ends are clamped rather than checked, which is what makes
    /// `slice(text, 0, huge)` mean "the rest" instead of stopping the program.
    /// An index is a *request* here, unlike `String::byte` where it is a claim.
    ///
    /// The UTF-8 guarantee survives without the walk. The input is already
    /// valid, so the only way to produce something that is not is to cut
    /// through a multi-byte character — and that is visible as a continuation
    /// byte sitting at one end or the other, which is two reads rather than
    /// `count` of them.
    pub(super) fn string_slice(
        &mut self,
        object: PointerValue<'ctx>,
        length: IntValue<'ctx>,
        from: ExprId,
        to: ExprId,
    ) -> Flow<'ctx> {
        let from = self.expr(from)?.into_int_value();
        let to = self.expr(to)?.into_int_value();
        let i64_type = self.be.ctx.i64_type();
        let zero = i64_type.const_zero();

        // start = from clamped into `0 ..= length`, end into `start ..= length`,
        // so `count` cannot be negative however the caller was counting.
        let start = self.clamp(from, zero, length, "slice.start");
        let end = self.clamp(to, start, length, "slice.end");
        let count = self
            .be
            .builder
            .build_int_sub(end, start, "slice.count")
            .expect("sizing a slice");

        let bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_BYTES_OFFSET,
            "str.bytes",
        );
        let head_cut = self.cuts_a_character(object, bytes, length, start);
        let tail_cut = self.cuts_a_character(object, bytes, length, end);
        let cut = self
            .be
            .builder
            .build_or(head_cut, tail_cut, "slice.cut")
            .expect("either end");
        // An empty slice is `""`, which is UTF-8 whatever it was cut out of.
        let asked = self
            .be
            .builder
            .build_int_compare(IntPredicate::SGT, count, zero, "slice.asked")
            .expect("comparing a count");
        let cut = self.be.builder.build_and(asked, cut, "slice.cuts").expect("and");
        let ok = self.be.builder.build_not(cut, "slice.ok").expect("negating");
        self.guard(ok, "this slice cuts a character in half, so it is not a String");

        // The check split the block, so the source address is taken again on
        // the side of the branch that carries on.
        let bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_BYTES_OFFSET,
            "str.bytes",
        );
        let size = self
            .be
            .builder
            .build_int_add(count, i64_type.const_int(runtime::FIELD_WORD, false), "slice.size")
            .expect("sizing a string");
        let string = self
            .be
            .builder
            .build_call(
                self.be.rt.alloc,
                &[size.into(), self.be.ctx.i32_type().const_int(STRING_TAG, false).into()],
                "slice.str",
            )
            .expect("allocating a string")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();
        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, string, STRING_LEN_FIELD);
        self.be
            .builder
            .build_store(length_slot, count)
            .expect("storing a string length");
        let into = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            string,
            STRING_BYTES_OFFSET,
            "slice.bytes",
        );
        let source = unsafe {
            self.be
                .builder
                .build_in_bounds_gep(self.be.ctx.i8_type(), bytes, &[start], "slice.from")
                .expect("addressing the first byte")
        };
        // Alignment 1 on both sides, for the reason `String::bytes` gives: an
        // offset into a string is not aligned to anything in particular.
        self.be
            .builder
            .build_memcpy(into, 1, source, 1, count)
            .expect("copying a slice");
        Some(string.into())
    }

    /// `value` brought inside `low ..= high`, both ends inclusive.
    pub(super) fn clamp(
        &self,
        value: IntValue<'ctx>,
        low: IntValue<'ctx>,
        high: IntValue<'ctx>,
        name: &str,
    ) -> IntValue<'ctx> {
        let under = self
            .be
            .builder
            .build_int_compare(IntPredicate::SLT, value, low, "clamp.under")
            .expect("comparing against a floor");
        let raised = self
            .be
            .builder
            .build_select(under, low, value, "clamp.floor")
            .expect("raising to a floor")
            .into_int_value();
        let over = self
            .be
            .builder
            .build_int_compare(IntPredicate::SGT, raised, high, "clamp.over")
            .expect("comparing against a ceiling");
        self.be
            .builder
            .build_select(over, high, raised, name)
            .expect("lowering to a ceiling")
            .into_int_value()
    }

    /// Whether byte `at` is the middle of a character rather than the start of
    /// one — which is exactly when cutting a valid string there stops it being
    /// one.
    ///
    /// `at == length` is the end of the string and always a legal cut, so the
    /// answer is no without reading anything. The read still *happens*, off the
    /// length field instead of off the end of the bytes: a byte the object
    /// definitely owns, whose value is then thrown away by the `and`. Branching
    /// around it would cost more than the load it avoids.
    pub(super) fn cuts_a_character(
        &self,
        object: PointerValue<'ctx>,
        bytes: PointerValue<'ctx>,
        length: IntValue<'ctx>,
        at: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        let i8_type = self.be.ctx.i8_type();
        let inside = self
            .be
            .builder
            .build_int_compare(IntPredicate::SLT, at, length, "cut.inside")
            .expect("is there a byte here");
        let slot = unsafe {
            self.be
                .builder
                .build_in_bounds_gep(i8_type, bytes, &[at], "cut.slot")
                .expect("addressing a byte")
        };
        let anchor =
            runtime::field_pointer(self.be.ctx, &self.be.builder, object, STRING_LEN_FIELD);
        let probe = self
            .be
            .builder
            .build_select(inside, slot, anchor, "cut.probe")
            .expect("choosing something readable")
            .into_pointer_value();
        let byte = self
            .be
            .builder
            .build_load(i8_type, probe, "cut.byte")
            .expect("reading a byte")
            .into_int_value();
        // A UTF-8 continuation byte is `10xxxxxx`; every character starts with
        // something else.
        let masked = self
            .be
            .builder
            .build_and(byte, i8_type.const_int(0xC0, false), "cut.mask")
            .expect("masking a byte");
        let continues = self
            .be
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                masked,
                i8_type.const_int(0x80, false),
                "cut.continues",
            )
            .expect("comparing a byte");
        self.be.builder.build_and(inside, continues, "cut").expect("and")
    }

    /// The length word of a string object.
    pub(super) fn string_length(&mut self, object: PointerValue<'ctx>) -> IntValue<'ctx> {
        let slot = runtime::field_pointer(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_LEN_FIELD,
        );
        self.be
            .builder
            .build_load(self.be.ctx.i64_type(), slot, "str.len")
            .expect("reading a string length")
            .into_int_value()
    }

    /// The `print` intrinsic.
    ///
    /// Dispatched on the argument's type, because there is no prelude yet in
    /// which three differently-typed printers could be declared — see
    /// `crate::backend`. It consumes its argument the way any Khora function
    /// consumes a parameter, so a `String` handed to it is dropped here; the
    /// `dup` at the read site is what that drop balances.
    pub(super) fn print(&mut self, arg: ExprId, range: TextRange) -> Flow<'ctx> {
        let ty = self.types.of(arg).clone();
        let value = self.expr(arg)?;

        match ty {
            Type::Int => {
                let print = self.be.rt.print_int;
                self.be.builder.build_call(print, &[value.into()], "").expect("printing an Int");
            }
            Type::Float => {
                let print = self.be.rt.print_float;
                self.be.builder.build_call(print, &[value.into()], "").expect("printing a Float");
            }
            Type::Bool => {
                let byte = self
                    .be
                    .builder
                    .build_int_z_extend(value.into_int_value(), self.be.ctx.i8_type(), "bool.byte")
                    .expect("widening a Bool for the C ABI");
                let print = self.be.rt.print_bool;
                self.be.builder.build_call(print, &[byte.into()], "").expect("printing a Bool");
            }
            Type::Str => {
                let object = value.into_pointer_value();
                let length_slot = runtime::field_pointer(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    STRING_LEN_FIELD,
                );
                let length = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), length_slot, "str.len")
                    .expect("reading a string length");
                let bytes = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    STRING_BYTES_OFFSET,
                    "str.bytes",
                );
                let print = self.be.rt.print_str;
                self.be
                    .builder
                    .build_call(print, &[bytes.into(), length.into()], "")
                    .expect("printing a String");
                self.drop(value, &Type::Str);
            }
            other => {
                return self.fail(
                    format!(
                        "`print` shows `Int`, `Bool` and `String`; showing a `{other}` needs a \
                         typeclass, which arrives in phase 3"
                    ),
                    range,
                )
            }
        }
        Some(self.be.unit_value())
    }
}
