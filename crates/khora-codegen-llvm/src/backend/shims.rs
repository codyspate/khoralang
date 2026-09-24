//! Small generated functions the runtime calls back into.
//!
//! `Shared::update` hands the runtime a closure to run under a lock, and the
//! runtime cannot know the closure's type — so the shim is generated here,
//! where the type is known, and the runtime only ever sees a function pointer.
//! Overflow intrinsics are here for the same reason: one per width, declared
//! once and reused.

use super::*;

impl<'ctx> Backend<'ctx> {
    /// The LLVM integer type of a given width.
    ///
    /// Only four widths exist, so this is a match rather than
    /// `custom_width_int_type` — which takes a `NonZero` and hands back a
    /// `Result` for a question that cannot fail here.
    pub fn int_width(&self, bits: u32) -> inkwell::types::IntType<'ctx> {
        match bits {
            8 => self.ctx.i8_type(),
            16 => self.ctx.i16_type(),
            32 => self.ctx.i32_type(),
            _ => self.ctx.i64_type(),
        }
    }

    /// One of LLVM's `*.with.overflow` intrinsics, declared on first use.
    ///
    /// Each returns `{ i64, i1 }` — the result and whether it wrapped — so the
    /// check is a branch on a flag the same instruction already produced.
    pub fn overflow_intrinsic(&mut self, name: &str, bits: u32) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let width = self.int_width(bits);
        let pair = self.ctx.struct_type(&[width.into(), self.ctx.bool_type().into()], false);
        self.module.add_function(
            name,
            pair.fn_type(&[width.into(), width.into()], false),
            Some(Linkage::External),
        )
    }

    /// `llvm.fptosi.sat.i64.f64`, the conversion that is defined everywhere.
    ///
    /// **Plain `fptosi` is poison out of range**, and what that produced was
    /// not merely unspecified but *sign-flipped*: `1e30` and `9.3e18` both came
    /// back as `-9223372036854775808`, the most negative `Int`, from positive
    /// inputs. `Float::to_int`'s own documentation promised the opposite in as
    /// many words -- "clamps to the nearest end, and a `NaN` is zero.
    /// Undefined behaviour is the alternative and is not one" -- and the
    /// lowering's comment claimed the saturating form "is what this uses".
    /// Neither was true; nothing had asked the machine. Roadmap 16.
    pub fn saturating_fptosi(&mut self) -> FunctionValue<'ctx> {
        let name = "llvm.fptosi.sat.i64.f64";
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let i64 = self.ctx.i64_type();
        self.module.add_function(
            name,
            i64.fn_type(&[self.ctx.f64_type().into()], false),
            Some(Linkage::External),
        )
    }

    /// The shim `khora_shared_update` calls the change function through.
    ///
    /// The runtime cannot know `A`. It has the value as the one word every
    /// Khora value fits in, and a closure whose parameter and result are `A` —
    /// so the conversion happens here, once per `A`, on the side of the
    /// boundary that knows what `A` is. Only scalars and pointers cross, which
    /// is the same rule the foreign-function interface follows.
    ///
    /// `uint32_t shim(void *code, void *closure, uint64_t value, uint64_t *out)`.
    ///
    /// **Returns the change function's cancellation tag, and writes `out` only
    /// when it is 0.** The answer half of a pair whose tag is not 0 is a zero
    /// nobody computed -- a null, for a boxed `A` -- and a shim that wrote it
    /// would put it in the cell. `khora_shared_update` says what happens
    /// instead.
    pub fn change_shim(&mut self, value_ty: &Type) -> Option<FunctionValue<'ctx>> {
        let key = super::glue::type_key(value_ty);
        if let Some(f) = self.change_shims.get(&key) {
            return Some(*f);
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i64_type = self.ctx.i64_type();
        let i32_type = self.ctx.i32_type();
        let f = self.module.add_function(
            &format!("kh$change{}", self.change_shims.len()),
            i32_type.fn_type(&[ptr.into(), ptr.into(), i64_type.into(), ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.change_shims.insert(key, f);

        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let closure = f.get_nth_param(1).expect("the closure").into_pointer_value();
        let word = f.get_nth_param(2).expect("the value").into_int_value();
        let out = f.get_nth_param(3).expect("somewhere for the new value").into_pointer_value();

        let llvm_ty = self.llvm_type(value_ty)?;
        // The cell keeps its value while the change function consumes a copy:
        // the same second owner `Shared::get` makes. Erratum 90.
        let given = self.reload_kept(word, value_ty);
        let callee_type = self.plain_tagged_type(value_ty)?.fn_type(&[ptr.into(), llvm_ty.into()], false);
        let pair = self
            .builder
            .build_indirect_call(callee_type, code, &[closure.into(), given.into()], "changed")
            .expect("calling a change function")
            .try_as_basic_value()
            .basic()
            .expect("a change function gives back a pair")
            .into_struct_value();
        let which = self.change_tag(f, pair);
        let produced = self.builder.build_extract_value(pair, 1, "produced").expect("the answer half");
        let back = self.to_word(produced);
        self.builder.build_store(out, back).expect("handing back the new value");
        self.builder.build_return(Some(&which)).expect("answered");

        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        Some(f)
    }

    /// Reads a change function's tag and returns it from the shim `f` if it
    /// is not 0, leaving the builder on the path where it is.
    ///
    /// Nothing the shim holds needs releasing on the way out: the argument
    /// was the change function's to consume, and its unwind did.
    fn change_tag(
        &mut self,
        f: FunctionValue<'ctx>,
        pair: inkwell::values::StructValue<'ctx>,
    ) -> inkwell::values::IntValue<'ctx> {
        let which = self
            .builder
            .build_extract_value(pair, 0, "which")
            .expect("the tag half")
            .into_int_value();
        let zero = self.ctx.i32_type().const_zero();
        let stopped = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, which, zero, "stopped")
            .expect("testing the tag");
        let leave = self.ctx.append_basic_block(f, "stopped");
        let answered = self.ctx.append_basic_block(f, "answered");
        self.builder
            .build_conditional_branch(stopped, leave, answered)
            .expect("branching on the tag");
        self.builder.position_at_end(leave);
        self.builder.build_return(Some(&which)).expect("handing back the tag");
        self.builder.position_at_end(answered);
        zero
    }

    /// The shim `khora_shared_modify` calls its change function through.
    ///
    /// [`Backend::change_shim`] with one more thing to do. The change function
    /// gives back a `Changed<A, B>` — one heap object holding the new state and
    /// the answer — and the runtime cannot take a Khora record apart, so it is
    /// taken apart here, where the layout is known. Two words come out where
    /// only one can be returned, so the answer goes through a pointer.
    ///
    /// The record itself is released: it was built to carry two values across
    /// one call and nothing holds it afterwards.
    ///
    /// `uint32_t shim(void *code, void *closure, uint64_t value, uint64_t *state,
    ///                uint64_t *answer)`, returning the tag as [`Backend::change_shim`]
    /// does, and writing neither pointer when it is not 0.
    pub fn modify_shim(
        &mut self,
        carrier: &Type,
        state: &Type,
        answer: &Type,
    ) -> Option<FunctionValue<'ctx>> {
        let key = format!("{carrier}:{state}=>{answer}");
        if let Some(f) = self.modify_shims.get(&key) {
            return Some(*f);
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i64_type = self.ctx.i64_type();
        let f = self.module.add_function(
            &format!("kh$modify{}", self.modify_shims.len()),
            self.ctx
                .i32_type()
                .fn_type(&[ptr.into(), ptr.into(), i64_type.into(), ptr.into(), ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.modify_shims.insert(key, f);

        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let closure = f.get_nth_param(1).expect("the closure").into_pointer_value();
        let word = f.get_nth_param(2).expect("the value").into_int_value();
        let state_out = f.get_nth_param(3).expect("somewhere for the state").into_pointer_value();
        let out = f.get_nth_param(4).expect("somewhere for the answer").into_pointer_value();

        let state_ty = self.llvm_type(state)?;
        // As in `change_shim`: the cell still holds the state it lends.
        let given = self.reload_kept(word, state);
        let callee_type = self.plain_tagged_type(carrier)?.fn_type(&[ptr.into(), state_ty.into()], false);
        let pair = self
            .builder
            .build_indirect_call(callee_type, code, &[closure.into(), given.into()], "changed")
            .expect("calling a change function")
            .try_as_basic_value()
            .basic()
            .expect("a change function gives back a pair")
            .into_struct_value();
        let which = self.change_tag(f, pair);
        let changed = self.builder.build_extract_value(pair, 1, "changed").expect("the answer half");

        // Field order is declaration order, and `Changed` declares `state`
        // first.
        let (next, result) = if self.unboxed.holds(carrier) {
            // **Held inline, so the halves are taken, not counted.** The
            // change function handed back the carrier as a value, and a
            // value owns its fields: reading them out moves them, and there
            // is no carrier left to release. Counting each boxed half again
            // here, as the boxed branch must, left one reference per half
            // per call that nothing would ever release.
            let whole = changed.into_struct_value();
            let next = self.read_inline(whole, carrier, 0, state);
            let result = self.read_inline(whole, carrier, 1, answer);
            (next, result)
        } else {
            // Both are duplicated out of the record before it goes: the
            // record's glue releases what it held.
            let pair = changed.into_pointer_value();
            let fields = [state.clone(), answer.clone()];
            let next = self.read_from(pair, self.field_slot(&fields, 0), state);
            let result = self.read_from(pair, self.field_slot(&fields, 1), answer);
            let glue = self.drop_glue(carrier);
            self.builder
                .build_call(self.rt.drop, &[pair.into(), glue.into()], "")
                .expect("releasing the carrier");
            (next, result)
        };

        let result = self.to_word(result);
        self.builder.build_store(out, result).expect("handing back the answer");
        let next = self.to_word(next);
        self.builder.build_store(state_out, next).expect("handing back the new state");
        self.builder.build_return(Some(&which)).expect("answered");

        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        Some(f)
    }

    /// One field of a record, with a reference of its own.
    ///
    /// The shims are outside `Lower`, which is where the ordinary field read
    /// lives, so this is the small part of it they need.
    pub(super) fn read_from(
        &mut self,
        object: PointerValue<'ctx>,
        index: u64,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let slot = runtime::field_pointer(self.ctx, &self.builder, object, index);
        let llvm = self.llvm_type(ty).unwrap_or_else(|| self.ctx.i64_type().into());
        let value = self.builder.build_load(llvm, slot, "field").expect("loading a field");
        self.adjust_held(value, ty, Adjust::Up);
        value
    }
}
