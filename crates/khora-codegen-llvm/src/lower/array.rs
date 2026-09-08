//! Arrays: the element width, the bounds check, and the slot.
//!
//! Reading an element is a bounds check and a load with no call, because the
//! layout is a contract with the runtime rather than something it hides. What
//! the runtime keeps is allocation and release, both of which need the length.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// The element type of the `Array<A>` this call is about.
    ///
    /// Read from the expression's own type rather than from the receiver,
    /// because `Array::new` has no receiver and its result is the array.
    pub(super) fn array_element(&mut self, ty: &Type, range: TextRange) -> Option<Type> {
        match ty {
            Type::Adt { name, args, .. } if name == runtime::ARRAY_TYPE => {
                Some(args.first().cloned().unwrap_or(Type::Unit))
            }
            other => {
                let other = other.clone();
                self.fail(format!("`{other}` is not an array"), range);
                None
            }
        }
    }

    /// How many bytes one element of `ty` occupies inside an array.
    ///
    /// A pointer and an `Int` are a word; a fixed-width integer is its own
    /// width; a `Bool` is a byte, which it may as well be now that anything
    /// narrower than a word is possible at all. **A value held inline is as
    /// wide as it is**, which is what makes an array of them contiguous rather
    /// than an array of pointers to them -- and what the runtime is told, so
    /// that where an element begins is one answer and not two.
    pub(super) fn stride(&self, ty: &Type) -> u64 {
        match ty {
            Type::Fixed(kind) => u64::from(kind.bits) / 8,
            Type::Bool => 1,
            _ => self.be.field_words(ty) * runtime::FIELD_WORD,
        }
    }

    /// What an array's header should say about its elements, and how to let
    /// one go.
    ///
    /// Three answers rather than two: a counted pointer, a value held inline,
    /// or neither. See `khora_rt::array`'s constants, which is where the
    /// difference is spelled out.
    fn element_kind(&mut self, element: &Type) -> (u8, PointerValue<'ctx>) {
        if is_boxed(element, &self.be.unboxed) {
            return (runtime::ELEMENTS_ARE_POINTERS, self.be.drop_glue(element));
        }
        if self.be.unboxed.holds(element) {
            return (runtime::ELEMENTS_ARE_INLINE, self.be.element_glue(element));
        }
        (runtime::ELEMENTS_ARE_INERT, self.be.null_pointer())
    }

    /// Counts one reference per slot for a fill the runtime copied blindly.
    ///
    /// A loop rather than a multiply, because a reference count is not a
    /// number the generated code may reach into: the value may hold several
    /// pointers, and each of them is `khora_dup`'s business.
    fn retain_per_slot(
        &mut self,
        value: BasicValueEnum<'ctx>,
        element: &Type,
        length: IntValue<'ctx>,
    ) {
        if self.be.inline_retain(element).is_none() {
            return;
        }
        let i64t = self.be.ctx.i64_type();
        let counter = self.entry_slot(i64t.into(), "filled");
        self.be.builder.build_store(counter, i64t.const_zero()).expect("starting the count");
        let test = self.block("fill.test");
        let body = self.block("fill.body");
        let done = self.block("fill.done");
        self.br(test);

        self.at(test);
        let seen = self
            .be
            .builder
            .build_load(i64t, counter, "filled.now")
            .expect("reading the count")
            .into_int_value();
        let more = self
            .be
            .builder
            .build_int_compare(IntPredicate::ULT, seen, length, "fill.more")
            .expect("comparing against the length");
        self.be
            .builder
            .build_conditional_branch(more, body, done)
            .expect("branching on the count");

        self.at(body);
        self.retain(value, element);
        let next = self
            .be
            .builder
            .build_int_add(seen, i64t.const_int(1, false), "filled.next")
            .expect("counting one more slot");
        self.be.builder.build_store(counter, next).expect("storing the count");
        self.br(test);
        self.at(done);
    }

    /// Continues only if `index` is below `length`; otherwise stops the
    /// program, saying which index and what length.
    ///
    /// Unsigned, so a negative index is one enormous one and both ends are the
    /// same comparison. A trap rather than a wrapped index or a poisoned read,
    /// for the same reason integer overflow traps.
    pub(super) fn check_index(&mut self, index: IntValue<'ctx>, length: IntValue<'ctx>) {
        let in_range = self
            .be
            .builder
            .build_int_compare(IntPredicate::ULT, index, length, "in.range")
            .expect("comparing an index against a length");
        let ok = self.block("index.ok");
        let out = self.block("index.out");
        self.be
            .builder
            .build_conditional_branch(in_range, ok, out)
            .expect("branching on the bounds check");

        self.at(out);
        let fail = self.be.rt.bounds_fail;
        self.be
            .builder
            .build_call(fail, &[index.into(), length.into()], "")
            .expect("reporting an index out of range");
        self.be.builder.build_unreachable().expect("sealing after a bounds failure");

        self.at(ok);
    }

    /// The pointer to element `index`, with the bounds check in front of it.
    ///
    /// Checked rather than trusted, and a failure stops the program rather than
    /// reading whatever is next in memory — the same reasoning as trapping on
    /// integer overflow. A program that runs off its own array is wrong, and
    /// the useful thing is to say where.
    pub(super) fn array_slot(
        &mut self,
        array: inkwell::values::PointerValue<'ctx>,
        index: inkwell::values::IntValue<'ctx>,
        stride: u64,
    ) -> PointerValue<'ctx> {
        let i64_type = self.be.ctx.i64_type();
        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, array, runtime::ARRAY_LEN_FIELD);
        let length = self
            .be
            .builder
            .build_load(i64_type, length_slot, "array.len")
            .expect("reading an array's length")
            .into_int_value();
        self.check_index(index, length);
        // The header is counted in whole words and the elements in strides, so
        // the two are added as bytes rather than as indices.
        runtime::element_pointer(
            self.be.ctx,
            &self.be.builder,
            array,
            index,
            stride,
            runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
        )
    }

    /// `Array::empty`, `Array::new`, `Array::length`, `Array::get` and
    /// `Array::set`.
    ///
    /// Allocation and release are runtime calls because both need the length
    /// at run time; reading and writing an element are generated, so an array
    /// access is a bounds check and a load rather than a call.
    pub(super) fn array_intrinsic(
        &mut self,
        site: ExprId,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("with_data", _) => self.with_data(site, args, range),
            ("is_utf8", [array]) => self.is_utf8(*array, range),
            // `new` with the fill left out, because a zero-length array has
            // nothing to fill.
            //
            // It closes a real gap rather than saving an argument. `new` wants
            // a value of the element type, and a generic container has none to
            // give until something has been put in it — so before this there
            // was no way to write down an empty `Array<A>` at all, and
            // `std`'s `Vector<A>` had to hold `Array<Option<A>>` and pay an
            // allocation per element for the emptiness.
            //
            // The element type still decides the stride, the boxed flag and
            // the drop glue. Nothing is stored yet, but the array is released
            // like any other, and a header that lied about its elements would
            // be a wild free the day one is written.
            ("empty", []) => {
                let array_ty = self.types.of(site).clone();
                let element = self.array_element(&array_ty, range)?;
                let (holding, glue) = self.element_kind(&element);
                let len = self.be.ctx.i64_type().const_zero();
                // The fill is written once per slot, and there are no slots.
                let fill = self.be.ctx.i64_type().const_zero();
                let flag = self.be.ctx.i8_type().const_int(u64::from(holding), false);
                let stride = self.be.ctx.i8_type().const_int(self.stride(&element), false);
                let new = self.be.rt.array_new;
                let array = self
                    .be
                    .builder
                    .build_call(
                        new,
                        &[len.into(), fill.into(), stride.into(), flag.into(), glue.into()],
                        "array.empty",
                    )
                    .expect("allocating an empty array")
                    .try_as_basic_value()
                    .basic()
                    .expect("an array is a value");
                Some(array)
            }
            ("new", [length, fill]) => {
                let array_ty = self.types.of(site).clone();
                let element = self.array_element(&array_ty, range)?;
                let len = self.expr(*length)?.into_int_value();
                let value = self.expr(*fill)?;

                let (holding, glue) = self.element_kind(&element);
                let width = self.stride(&element);
                // **A fill held inline is passed by address.** Every slot gets
                // a copy of the same bytes, and the slot holds the value
                // rather than a pointer to it -- so `to_word`, which would box
                // it to fit, is exactly the wrong answer. The template is a
                // stack slot of this frame, which outlives the call that reads
                // it.
                let word = if holding == runtime::ELEMENTS_ARE_INLINE {
                    let template = self.entry_slot(value.get_type(), "fill");
                    self.be.builder.build_store(template, value).expect("writing the fill");
                    self.be
                        .builder
                        .build_ptr_to_int(template, self.be.ctx.i64_type(), "fill.at")
                        .expect("the fill's address as a word")
                } else {
                    self.be.to_word(value)
                };
                let flag = self.be.ctx.i8_type().const_int(u64::from(holding), false);
                let stride = self.be.ctx.i8_type().const_int(width, false);
                let new = self.be.rt.array_new;
                let array = self
                    .be
                    .builder
                    .build_call(
                        new,
                        &[len.into(), word.into(), stride.into(), flag.into(), glue.into()],
                        "array",
                    )
                    .expect("allocating an array")
                    .try_as_basic_value()
                    .basic()
                    .expect("an array is a value");
                // Every slot took its own reference. The runtime counts them
                // for a pointer, which it understands; for a value held inline
                // it copied the bytes and cannot know what is inside them, so
                // the references those bytes hold are counted here, once a
                // slot. Then the caller's own goes.
                if holding == runtime::ELEMENTS_ARE_INLINE {
                    self.retain_per_slot(value, &element, len);
                }
                self.drop(value, &element);
                Some(array)
            }
            ("length", [array]) => {
                let array_ty = self.types.of(*array).clone();
                let object = self.expr(*array)?.into_pointer_value();
                let slot = runtime::field_pointer(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::ARRAY_LEN_FIELD,
                );
                let length = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "array.len")
                    .expect("reading an array's length");
                self.release_unless_lent(*array, object.into(), &array_ty);
                Some(length)
            }
            ("get", [array, index]) => {
                let array_ty = self.types.of(*array).clone();
                let element = self.array_element(&array_ty, range)?;
                let object = self.expr(*array)?.into_pointer_value();
                let at = self.expr(*index)?.into_int_value();
                let slot = self.array_slot(object, at, self.stride(&element));

                let Some(llvm_ty) = self.be.llvm_type(&element) else {
                    return self.fail("an array of that element type cannot be read", range);
                };
                let value = self
                    .be
                    .builder
                    .build_load(llvm_ty, slot, "element")
                    .expect("reading an element");
                // The array keeps its own reference to the element, so the
                // caller is handed one of its own.
                self.retain(value, &element);
                self.release_unless_lent(*array, object.into(), &array_ty);
                Some(value)
            }
            ("set", [array, index, value]) => {
                let array_ty = self.types.of(*array).clone();
                let element = self.array_element(&array_ty, range)?;
                let object = self.expr(*array)?.into_pointer_value();
                let at = self.expr(*index)?.into_int_value();
                let new = self.expr(*value)?;
                let slot = self.array_slot(object, at, self.stride(&element));

                // A slot that owns something has to let go of it, whether it
                // holds a pointer or a value that holds one.
                if self.be.owns_a_reference(&element) {
                    let llvm_ty =
                        self.be.llvm_type(&element).expect("an owning element has a machine type");
                    let old = self
                        .be
                        .builder
                        .build_load(llvm_ty, slot, "overwritten")
                        .expect("reading the overwritten element");
                    self.be.builder.build_store(slot, new).expect("writing an element");
                    // Store first, so `a.set(i, a.get(i))` cannot free what it
                    // has just written.
                    self.drop(old, &element);
                } else {
                    self.be.builder.build_store(slot, new).expect("writing an element");
                }
                self.release_unless_lent(*array, object.into(), &array_ty);
                Some(self.be.unit_value())
            }
            _ => self.fail(
                format!("`Array::{name}` is not an array operation the backend knows"),
                range,
            ),
        }
    }
}
