//! Trampolines: how a Khora function is called from Rust.
//!
//! A tagged return is a 16-byte aggregate, and how one of those comes back is a
//! decision LLVM and rustc make separately — errata 35. So nothing structured
//! crosses: the runtime is handed a function that takes scalars and writes the
//! payload through a pointer.

use super::*;

impl<'ctx> Backend<'ctx> {
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
            match &signature.ret {
                Type::Unit => self.ctx.void_type().fn_type(&params, false),
                other => self.llvm_type(other)?.fn_type(&params, false),
            }
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

            // A fallible target hands back the tagged pair, which the adapter
            // returns unchanged — it has nothing to add and nowhere to send an
            // error of its own.
            let returns_value = can_raise(&signature) || signature.ret != Type::Unit;
            match call.try_as_basic_value().basic().filter(|_| returns_value) {
                Some(value) => {
                    self.builder
                        .build_return(Some(&value))
                        .expect("returning from an adapter");
                }
                None => {
                    self.builder.build_return(None).expect("returning from an adapter");
                }
            }
        }
    }
}
