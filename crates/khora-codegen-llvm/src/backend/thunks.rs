//! Trampolines: how a Khora function is called from Rust.
//!
//! A tagged return is a 16-byte aggregate, and how one of those comes back is a
//! decision LLVM and rustc make separately — errata 35. So nothing structured
//! crosses: the runtime is handed a function that takes scalars and writes the
//! payload through a pointer.

use super::*;

impl<'ctx> Backend<'ctx> {
    /// A shim that calls a fallible function and hands back its tag.
    ///
    /// The runtime cannot call a fallible Khora function directly. Its return is
    /// a 16-byte aggregate, and how one of those comes back is a target decision
    /// LLVM makes for `{ i32, i64 }` and rustc makes for a `repr(C)` struct of
    /// the same shape — on x86-64 Windows they disagree, silently: the tag reads
    /// as zero and every failure looks like a pass.
    ///
    /// So the aggregate is taken apart on *this* side, where both halves of the
    /// call are LLVM's and agree by construction, and the runtime gets an `i32`
    /// back with a pointer to write the payload through.
    ///
    /// `arity` is how many arguments the callee takes: a test takes none, a
    /// fiber's thunk takes its closure. One shim per arity, not per callee.
    pub fn tagged_trampoline(&mut self, arity: usize) -> FunctionValue<'ctx> {
        if let Some(f) = self.trampolines.get(&arity) {
            return *f;
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i32_type = self.ctx.i32_type();
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        params.extend(std::iter::repeat_n(BasicMetadataTypeEnum::from(ptr), arity));
        params.push(ptr.into());

        let f = self.module.add_function(
            &format!("kh$tagged_call{arity}"),
            i32_type.fn_type(&params, false),
            Some(Linkage::Internal),
        );
        self.trampolines.insert(arity, f);

        // Emitted at once rather than queued: it calls nothing that has to be
        // discovered first, and it borrows the builder for four instructions.
        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let args: Vec<BasicMetadataValueEnum<'ctx>> =
            (0..arity).filter_map(|i| f.get_nth_param(i as u32 + 1)).map(|v| v.into()).collect();
        let out = f
            .get_nth_param(arity as u32 + 1)
            .expect("somewhere to put the payload")
            .into_pointer_value();

        let callee_params: Vec<BasicMetadataTypeEnum<'ctx>> =
            std::iter::repeat_n(BasicMetadataTypeEnum::from(ptr), arity).collect();
        let callee_type = self.tagged_type().fn_type(&callee_params, false);
        let result = self
            .builder
            .build_indirect_call(callee_type, code, &args, "outcome")
            .expect("calling a fallible function")
            .try_as_basic_value()
            .basic()
            .expect("a fallible function returns a tagged value")
            .into_struct_value();

        let which = self
            .builder
            .build_extract_value(result, 0, "which")
            .expect("reading the tag")
            .into_int_value();
        let payload =
            self.builder.build_extract_value(result, 1, "payload").expect("reading the payload");
        self.builder.build_store(out, payload).expect("handing back the payload");
        self.builder.build_return(Some(&which)).expect("handing back the tag");

        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        f
    }

    /// A shim that calls an *infallible* closure taking no arguments but
    /// itself, and hands back its cancellation tag, with its answer as a word
    /// written through a pointer.
    ///
    /// The same shape as [`Backend::tagged_trampoline`], so the runtime calls
    /// every fiber's thunk one way. It exists for a different reason: the
    /// thunk's answer may be an `Int`, a `String`, a record or a `Float`, and
    /// those come back in different registers -- so the call is made here,
    /// where the type is known, and what crosses is the one word everything in
    /// this runtime fits in.
    ///
    /// **The tag is what lets a fiber with no error row say it was stopped.**
    /// Every closure that cannot raise returns `{ which, answer }`, and a
    /// non-zero `which` from one can only be a cancellation, which the runtime
    /// records as the fiber's outcome in place of the answer.
    ///
    /// One shim per answer type. `returns` is the thunk's answer type, `None`
    /// for `()`, whose pair still carries a word.
    pub fn answered_trampoline(&mut self, returns: Option<BasicTypeEnum<'ctx>>) -> FunctionValue<'ctx> {
        let key = (1, returns.map_or_else(|| "void".to_string(), |t| t.to_string()));
        if let Some(f) = self.plain_trampolines.get(&key) {
            return *f;
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i32_type = self.ctx.i32_type();
        let i64_type = self.ctx.i64_type();
        let f = self.module.add_function(
            &format!("kh$answered_call${}", self.plain_trampolines.len()),
            i32_type.fn_type(&[ptr.into(), ptr.into(), ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.plain_trampolines.insert(key, f);

        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let closure = f.get_nth_param(1).expect("the closure");
        let out = f.get_nth_param(2).expect("somewhere to put the answer").into_pointer_value();

        let answer_ty = returns.unwrap_or_else(|| i64_type.into());
        let pair_ty = self.ctx.struct_type(&[i32_type.into(), answer_ty], false);
        let pair = self
            .builder
            .build_indirect_call(pair_ty.fn_type(&[ptr.into()], false), code, &[closure.into()], "answer")
            .expect("calling an infallible closure")
            .try_as_basic_value()
            .basic()
            .expect("a closure hands back a pair")
            .into_struct_value();
        let which = self
            .builder
            .build_extract_value(pair, 0, "which")
            .expect("reading the tag")
            .into_int_value();
        let answer = self.builder.build_extract_value(pair, 1, "answer").expect("reading the answer");
        // A stopped thunk's answer half is a zero nobody reads, and turning an
        // inline answer into a word allocates the box it crosses in. So only
        // an answered thunk's word is made; a stopped one hands back zero,
        // which is also what a function answering nothing hands back.
        let answered = self.ctx.append_basic_block(f, "answered");
        let done = self.ctx.append_basic_block(f, "done");
        let stopped = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, which, i32_type.const_zero(), "stopped")
            .expect("testing the tag");
        self.builder.build_store(out, i64_type.const_zero()).expect("no answer yet");
        self.builder.build_conditional_branch(stopped, done, answered).expect("branching on the tag");
        self.builder.position_at_end(answered);
        if returns.is_some() {
            let word = self.to_word(answer);
            self.builder.build_store(out, word).expect("handing back the answer");
        }
        self.builder.build_unconditional_branch(done).expect("to the return");
        self.builder.position_at_end(done);
        self.builder.build_return(Some(&which)).expect("handing back the tag");

        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        f
    }

    /// The adapter that lets `symbol` be used as a closure.
    ///
    /// A named function and a closure have different shapes: the closure is
    /// called through a pointer with its own object as the first argument, and
    /// the named function has no such argument. Rather than give every function
    /// in the program that parameter — which would cost every ordinary call to
    /// pay for a feature it does not use — a one-line adapter is emitted for
    /// the functions actually used as values, and it forwards.
    pub fn thunk(&mut self, symbol: &str) -> Option<FunctionValue<'ctx>> {
        if let Some(f) = self.thunks.get(symbol) {
            return Some(*f);
        }
        let signature = self.signature_of(symbol)?;

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        for param in &signature.params {
            params.push(self.llvm_type(param)?.into());
        }
        // The adapter wears the same shape the real function does, evidence
        // and tag included, so forwarding is a straight pass-through. A
        // function value's convention follows its *type*, and its type says
        // what it needs and how it can fail.
        for (_, ty) in evidence_of(&signature) {
            params.push(self.llvm_type(&ty)?.into());
        }
        let fn_type = if can_raise(&signature) {
            self.tagged_type().fn_type(&params, false)
        } else {
            // A function value is called the way a closure is, and every
            // closure that cannot raise hands back a cancellation tag.
            self.plain_tagged_type(&signature.ret)?.fn_type(&params, false)
        };

        let f = self.module.add_function(
            &format!("kh$fnval${symbol}"),
            fn_type,
            Some(Linkage::Internal),
        );
        self.thunks.insert(symbol.to_string(), f);
        self.pending_thunks.push(symbol.to_string());
        Some(f)
    }

    /// Gives every queued adapter its body: call the real function, return it.
    pub(super) fn emit_pending_thunks(&mut self) {
        while let Some(symbol) = self.pending_thunks.pop() {
            let Some(f) = self.thunks.get(&symbol).copied() else { continue };
            let Ok(target) = self.callee(&symbol) else { continue };
            let Some(signature) = self.signature_of(&symbol) else { continue };

            let entry = self.ctx.append_basic_block(f, "entry");
            self.builder.position_at_end(entry);

            // Skip the closure argument: the adapter ignores it, because a
            // named function captures nothing. Everything after it — the
            // written parameters and then the evidence — forwards in order.
            let forwarded = signature.params.len() + evidence_of(&signature).len();
            let args: Vec<BasicMetadataValueEnum<'ctx>> = (0..forwarded)
                .filter_map(|i| f.get_nth_param(i as u32 + 1))
                .map(|v| v.into())
                .collect();
            let call =
                self.builder.build_call(target, &args, "forward").expect("forwarding a call");

            // A fallible target hands back the tagged pair and a tagged one
            // its cancellation pair, which the adapter returns unchanged — it
            // has nothing to add and nowhere to send an error of its own. A
            // target that cannot stop keeps its plain return, and the adapter
            // gives it the zero tag every closure call expects.
            if can_raise(&signature) || self.is_tagged(&symbol) {
                let value = call.try_as_basic_value().basic().expect("a tagged call returns a pair");
                self.builder.build_return(Some(&value)).expect("returning from an adapter");
                continue;
            }
            let shape = f.get_type().get_return_type().expect("a pair").into_struct_type();
            let answer = match call.try_as_basic_value().basic() {
                Some(value) if !matches!(signature.ret, Type::Unit | Type::Never) => value,
                _ => self.ctx.i64_type().const_zero().into(),
            };
            let pair = self
                .builder
                .build_insert_value(shape.const_zero(), answer, 1, "pair")
                .expect("the answer half");
            self.builder.build_return(Some(&pair)).expect("returning from an adapter");
        }
    }
}
