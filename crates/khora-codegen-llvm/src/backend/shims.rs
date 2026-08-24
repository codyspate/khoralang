//! Small generated functions the runtime calls back into.
//!
//! `Shared::update` hands the runtime a closure to run under a lock, and the
//! runtime cannot know the closure's type — so the shim is generated here,
//! where the type is known, and the runtime only ever sees a function pointer.
//! Overflow intrinsics are here for the same reason: one per width, declared
//! once and reused.

use super::*;

impl<'ctx> Backend<'ctx> {
    /// One of LLVM's `*.with.overflow` intrinsics, declared on first use.
    ///
    /// Each returns `{ i64, i1 }` — the result and whether it wrapped — so the
    /// check is a branch on a flag the same instruction already produced.
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

    /// A shim that calls a fallible function and hands back its tag.
    ///
    /// The runtime cannot call a fallible Khora function directly. Its return
    /// is a 16-byte aggregate, and how one of those comes back is a target
    /// decision that LLVM makes for `{ i32, i64 }` and rustc makes for a
    /// `repr(C)` struct of the same shape — on x86-64 Windows they disagree,
    /// and the disagreement is silent: the tag reads as zero and every failure
    /// looks like a pass.
    ///
    /// So nothing but scalars crosses the boundary. The aggregate is taken
    /// apart on *this* side, where both halves of the call are LLVM's and
    /// agree by construction, and the runtime gets an `i32` back and a
    /// pointer to write the payload through.
    ///
    /// `arity` is how many arguments the callee takes: a test takes none, a
    /// fiber's thunk takes its closure. One shim per arity, not per callee.
    /// The shim `khora_shared_update` calls the change function through.
    ///
    /// The runtime cannot know `A`. It has the value as the one word every
    /// Khora value fits in, and a closure whose parameter and result are `A` —
    /// so the conversion happens here, once per `A`, on the side of the
    /// boundary that knows what `A` is. Only scalars and pointers cross, which
    /// is the same rule the foreign-function interface follows.
    ///
    /// `uint64_t shim(void *code, void *closure, uint64_t value)`.
    pub fn change_shim(&mut self, value_ty: &Type) -> Option<FunctionValue<'ctx>> {
        let key = value_ty.to_string();
        if let Some(f) = self.change_shims.get(&key) {
            return Some(*f);
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i64_type = self.ctx.i64_type();
        let f = self.module.add_function(
            &format!("kh$change{}", self.change_shims.len()),
            i64_type.fn_type(&[ptr.into(), ptr.into(), i64_type.into()], false),
            Some(Linkage::Internal),
        );
        self.change_shims.insert(key, f);

        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let closure = f.get_nth_param(1).expect("the closure").into_pointer_value();
        let word = f.get_nth_param(2).expect("the value").into_int_value();

        let llvm_ty = self.llvm_type(value_ty)?;
        let given = self.word_to_value(word, value_ty);
        let callee_type = llvm_ty.fn_type(&[ptr.into(), llvm_ty.into()], false);
        let produced = self
            .builder
            .build_indirect_call(callee_type, code, &[closure.into(), given.into()], "changed")
            .expect("calling a change function")
            .try_as_basic_value()
            .basic()
            .expect("a change function gives back a value");
        let back = self.to_word(produced);
        self.builder.build_return(Some(&back)).expect("handing back the new value");

        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        Some(f)
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
    /// `uint64_t shim(void *code, void *closure, uint64_t value, uint64_t *answer)`.
    pub fn modify_shim(&mut self, state: &Type, answer: &Type) -> Option<FunctionValue<'ctx>> {
        let key = format!("{state}=>{answer}");
        if let Some(f) = self.modify_shims.get(&key) {
            return Some(*f);
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i64_type = self.ctx.i64_type();
        let f = self.module.add_function(
            &format!("kh$modify{}", self.modify_shims.len()),
            i64_type.fn_type(&[ptr.into(), ptr.into(), i64_type.into(), ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.modify_shims.insert(key, f);

        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let closure = f.get_nth_param(1).expect("the closure").into_pointer_value();
        let word = f.get_nth_param(2).expect("the value").into_int_value();
        let out = f.get_nth_param(3).expect("somewhere for the answer").into_pointer_value();

        let state_ty = self.llvm_type(state)?;
        let given = self.word_to_value(word, state);
        let callee_type = ptr.fn_type(&[ptr.into(), state_ty.into()], false);
        let pair = self
            .builder
            .build_indirect_call(callee_type, code, &[closure.into(), given.into()], "changed")
            .expect("calling a change function")
            .try_as_basic_value()
            .basic()
            .expect("a change function gives back a record")
            .into_pointer_value();

        // Field order is declaration order, and `Changed` declares `state`
        // first. Both are duplicated out of the record before it goes.
        let next = self.read_from(pair, 0, state);
        let result = self.read_from(pair, 1, answer);
        let glue = self.drop_glue(&Type::adt("Changed"));
        self.builder
            .build_call(self.rt.drop, &[pair.into(), glue.into()], "")
            .expect("releasing the carrier");

        let result = self.to_word(result);
        self.builder.build_store(out, result).expect("handing back the answer");
        let next = self.to_word(next);
        self.builder.build_return(Some(&next)).expect("handing back the new state");

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
        if is_boxed(ty) {
            self.builder
                .build_call(self.rt.dup, &[value.into()], "")
                .expect("keeping a field past its record");
        }
        value
    }
}
