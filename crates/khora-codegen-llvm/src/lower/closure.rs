//! Closures: building them, and calling through one.
//!
//! A closure is an ordinary heap object holding a function pointer and its
//! captures, under the same header as everything else — which is why it is
//! reference counted like everything else and needs no special release.
//!
//! A lifted lambda takes its own closure object as its first argument, and that
//! is also how a closure calls itself without capturing itself. The cycle that
//! would otherwise be is `docs/design/memory.md` §3.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// Allocates the closure object for a lambda expression.
    ///
    /// Field 0 holds the lifted function's address and the captures follow, all
    /// under the ordinary object header — so a closure is dup'ed, dropped and
    /// counted by exactly the machinery every other heap value already uses.
    ///
    /// Which captures those are comes from the *site*, not from the lambda
    /// expression: lowering finds the names the body reads, and the checker
    /// adds the capabilities it uses without naming. The site is where the two
    /// were put together, and it is the only list this may read.
    pub(super) fn make_closure(&mut self, id: ExprId, range: TextRange) -> Flow<'ctx> {
        let owner = self.owner.clone();
        let Some(site) = self.be.closure_at(&owner, id).cloned() else {
            return self.fail("this closure was never declared, which is a compiler bug", range);
        };
        let Some(tag) = self.be.closure_tag(&owner, id) else {
            return self.fail("this closure has no tag, which is a compiler bug", range);
        };
        let Some(function) = self.be.definition(&site.symbol) else {
            return self.fail("this closure has no lifted function, which is a compiler bug", range);
        };

        // Sized from the *site*, which is the list the fields are written
        // from. Sizing it from the lowering's list instead worked for as long
        // as the two agreed, and wrote past the end of the object the moment
        // one of them grew.
        let fields = CLOSURE_CAPTURE_BASE + site.captures.len();
        let alloc = self.be.rt.alloc;
        let object = self
            .be
            .builder
            .build_call(
                alloc,
                &[
                    self.be.ctx.i64_type().const_int(FIELD_WORD * fields as u64, false).into(),
                    self.be.ctx.i32_type().const_int(tag as u64, false).into(),
                ],
                "closure.obj",
            )
            .expect("allocating a closure")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();

        let code = function.as_global_value().as_pointer_value();
        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, 0);
        self.be.builder.build_store(slot, code).expect("storing a closure's code pointer");

        for (index, (local, ty)) in site.captures.iter().enumerate() {
            let Some(from) = self.slots.get(local).copied() else { continue };
            let Some(llvm_ty) = self.be.llvm_type(ty) else { continue };
            let value = self
                .be
                .builder
                .build_load(llvm_ty, from, "capture")
                .expect("reading a captured local");
            // The closure outlives this expression and now holds its own
            // reference. This is the one place a capture is counted; the
            // closure's drop glue is the matching release.
            if is_boxed(ty) {
                self.dup(value);
            }
            self.store_field(object, index + CLOSURE_CAPTURE_BASE, value, ty);
        }

        Some(object.into())
    }

    /// Wraps a named function in a closure object so it can be passed along.
    pub(super) fn function_value(&mut self, symbol: &str, range: TextRange) -> Flow<'ctx> {
        let Some(thunk) = self.be.thunk(symbol) else {
            return self.fail(
                format!("`{symbol}` has a signature the backend cannot represent"),
                range,
            );
        };

        let alloc = self.be.rt.alloc;
        let object = self
            .be
            .builder
            .build_call(
                alloc,
                &[
                    self.be.ctx.i64_type().const_int(FIELD_WORD, false).into(),
                    // Any tag: an adapter captures nothing, so the shared
                    // closure `drop_fields` has no case for it and the default
                    // arm — which releases nothing — is the correct one.
                    self.be.ctx.i32_type().const_int(CLOSURE_ADAPTER_TAG, false).into(),
                ],
                "fnval.obj",
            )
            .expect("allocating a function value")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();

        let code = thunk.as_global_value().as_pointer_value();
        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, 0);
        self.be.builder.build_store(slot, code).expect("storing an adapter pointer");
        Some(object.into())
    }

    /// Calls a closure value: load its code pointer and call through it.
    pub(super) fn invoke_closure(
        &mut self,
        site: ExprId,
        callee: ExprId,
        signature: &FnShape,
        args: &[ExprId],
        range: TextRange,
    ) -> Option<Invoked<'ctx>> {
        // The callee before the arguments, which is the order the source is
        // written in and the order the reference-counting plan was made for.
        let closure = self.expr(callee)?.into_pointer_value();
        let mut given = Vec::with_capacity(args.len());
        for arg in args {
            given.push(self.expr(*arg)?);
        }
        self.invoke_closure_at(site, callee, closure, signature, given, range)
    }

    /// The same, given the closure and its arguments as values.
    ///
    /// What an intrinsic that calls back into Khora needs: `Array::with_data`
    /// hands its body a pointer and a length that no expression in the source
    /// produced. Split out rather than written twice, because two places
    /// building the same argument list is how the two come to disagree — see
    /// errata 33.
    pub(super) fn invoke_closure_at(
        &mut self,
        site: ExprId,
        callee: ExprId,
        closure: PointerValue<'ctx>,
        signature: &FnShape,
        given: Vec<BasicValueEnum<'ctx>>,
        range: TextRange,
    ) -> Option<Invoked<'ctx>> {
        let FnShape { params, ret, requires, raises } = signature;

        if given.len() != params.len() {
            self.fail(
                format!(
                    "this call takes {} argument(s), but {} were given",
                    params.len(),
                    given.len()
                ),
                range,
            )?;
        }
        let mut values: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = vec![closure.into()];
        for value in given {
            values.push(value.into());
        }
        // Evidence is appended in label order, exactly as a direct call
        // appends it — the closure's shape follows its *type* the way a named
        // function's follows its signature.
        // A name for the diagnostic if the callee has one; a closure often
        // does not, and "this call" is honest about that.
        let label = match self.body.expr(callee) {
            Expr::Local(local) => self.body.local(*local).name.clone(),
            Expr::Field { name, .. } => name.clone(),
            _ => "this call".to_string(),
        };
        for value in self.evidence_from_row(requires, &label, site, range)? {
            values.push(value.into());
        }

        let ptr = self.be.ctx.ptr_type(AddressSpace::default());
        let mut param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        for param in params {
            let Some(ty) = self.be.llvm_type(param) else {
                self.fail("a closure parameter has no machine type", range)?;
                unreachable!("`fail` returns None")
            };
            param_types.push(ty.into());
        }
        for (_, ty) in row_fields(requires) {
            let Some(ty) = self.be.llvm_type(&ty) else {
                self.fail("a capability has no machine type", range)?;
                unreachable!("`fail` returns None")
            };
            param_types.push(ty.into());
        }
        let fallible = !row_is_empty(raises);
        let fn_type = if fallible {
            self.be.tagged_type().fn_type(&param_types, false)
        } else {
            match ret {
                Type::Unit => self.be.ctx.void_type().fn_type(&param_types, false),
                other => match self.be.llvm_type(other) {
                    Some(ty) => ty.fn_type(&param_types, false),
                    None => {
                        self.fail("a closure's result has no machine type", range)?;
                        unreachable!("`fail` returns None")
                    }
                },
            }
        };

        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, closure, 0);
        let code = self
            .be
            .builder
            .build_load(ptr, slot, "closure.code")
            .expect("loading a closure's code pointer")
            .into_pointer_value();

        // The call site owns a reference to the closure — reading a local
        // dup'ed it, and a lambda written in place was born owned — and the
        // callee only borrows it. So it has to be released here, on *every*
        // way out of this expression, which is what makes it a scope rather
        // than a line after the call: a fallible callee can leave through the
        // branch below, and that path never reaches the line.
        //
        // A closure calling *itself* is the exception. Its own name is the
        // argument it was called through, which it borrows; releasing that
        // would decrement a count this frame never took, and free the closure
        // out from under the caller still running in it.
        let owned = !matches!(self.body.expr(callee), Expr::LambdaSelf);
        let callee_ty = self.types.of(callee).clone();
        self.scopes.push(if owned && is_boxed(&callee_ty) {
            vec![Cleanup::Temp(closure.into(), callee_ty)]
        } else {
            Vec::new()
        });

        let call = self
            .be
            .builder
            .build_indirect_call(fn_type, code, &values, "closure.call")
            .expect("calling a closure");

        Some(Invoked { raw: call.try_as_basic_value().basic(), fallible })
    }

    /// Calls a closure and propagates whatever it raised.
    ///
    /// The ordinary reading of `f(x)`: an error leaves through the branch `!`
    /// marks. [`Lower::attempt`] is the other reading, and the two differ only
    /// in what they do with the tag.
    pub(super) fn call_closure(
        &mut self,
        site: ExprId,
        callee: ExprId,
        signature: &FnShape,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        let invoked = self.invoke_closure(site, callee, signature, args, range)?;
        self.after_invoke(invoked, &signature.ret, range)
    }

    /// What to do with a closure call that has happened: split its tag if it
    /// had one, then close the scope holding the closure's reference.
    ///
    /// Split out because `SharedFn::call` invokes a closure the source never
    /// wrote as a call, and two places deciding what a tagged return means is
    /// how the two come to disagree.
    pub(super) fn after_invoke(
        &mut self,
        invoked: Invoked<'ctx>,
        ret: &Type,
        range: TextRange,
    ) -> Flow<'ctx> {
        let Invoked { raw, fallible } = invoked;
        let result = if fallible {
            let tagged = raw.expect("a fallible closure returns a tagged value");
            self.split_tagged(tagged, ret, range)?
        } else {
            match ret {
                Type::Unit => self.be.unit_value(),
                _ => raw.unwrap_or_else(|| self.be.unit_value()),
            }
        };

        self.leave_scope();
        Some(result)
    }
}
