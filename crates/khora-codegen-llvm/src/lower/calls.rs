//! Calls, and the evidence that travels with them.
//!
//! What a call *is* depends on what the callee turns out to be: a constructor,
//! an intrinsic the backend fills in, a closure, or an ordinary named function.
//! Deciding that is `call`, and it decides once — a method somebody wrote wins
//! over one the backend implements, and the test is that nothing else filled it
//! in first.
//!
//! Capabilities are extra arguments appended here, per
//! `docs/design/effect-runtime.md` §2, which is why installing a handler needs
//! no runtime machinery at all.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    pub(super) fn call(
        &mut self,
        site: ExprId,
        callee: ExprId,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match self.body.expr(callee).clone() {
            Expr::Path(khora_hir::Resolution::Variant { module, type_name, name }) => {
                self.construct(site, Some(&module), &type_name, &name, args, range)
            }
            Expr::Path(khora_hir::Resolution::TraitItem { owner, name }) => {
                // **A method somebody wrote wins over one the backend
                // implements.** An intrinsic is a *declaration the backend
                // fills in*, so the test is that nothing else filled it in
                // first — `Int::to_string` is written in `std::core`, and
                // keying `Int::` on the owner alone sent it to the two-argument
                // integer operations and asked a `String` to be an `i64`.
                //
                // The same rule `attempt` already needed, applied where it
                // belongs: once, before any of them.
                if let Some(symbol) = self.mono.callee(&self.owner.clone(), callee) {
                    if self.be.is_defined(&symbol) {
                        return self.call_named(&symbol, site, args, range);
                    }
                }
                if owner == runtime::REGION_TYPE {
                    return self.region_intrinsic(&name, args, range);
                }
                if owner == runtime::FIBER_TYPE {
                    return self.fiber_intrinsic(&name, args, range);
                }
                if owner == runtime::FIBERS_TYPE {
                    return self.nursery_intrinsic(&name, args, range);
                }
                if owner == runtime::SHARED_FN_TYPE {
                    return self.shared_fn_intrinsic(site, &name, args, range);
                }
                if owner == runtime::SHARED_TYPE {
                    return self.shared_intrinsic(site, &name, args, range);
                }
                if owner == runtime::CHANNEL_TYPE {
                    return self.channel_intrinsic(site, &name, args, range);
                }
                if owner == runtime::ARRAY_TYPE {
                    return self.array_intrinsic(site, &name, args, range);
                }
                if let Some(shape) = int_owner(&owner) {
                    return self.int_intrinsic(shape, &owner, &name, args, range);
                }
                if owner == "String" && name == "with_data" {
                    return self.with_data(site, args, range);
                }
                if owner == "String" && name == "with_c_string" {
                    return self.with_c_string(site, args, range);
                }
                if owner == "String" && name == "from_bytes" {
                    return self.string_from_bytes(args, range);
                }
                if owner == "Float" && name == "to_int" {
                    return self.float_to_int(args, range);
                }
                if owner == "String"
                    && matches!(name.as_str(), "bytes" | "byte" | "byte_length" | "slice" | "find")
                {
                    return self.string_intrinsic(&name, args, range);
                }
                if owner == "Ptr" && matches!(name.as_str(), "null" | "is_null") {
                    return self.ptr_intrinsic(&name, args, range);
                }
                match self.mono.callee(&self.owner.clone(), callee) {
                    Some(symbol) => self.call_named(&symbol, site, args, range),
                    None => self.fail(
                        format!("`{name}` was not resolved to an impl; that is a compiler bug"),
                        range,
                    ),
                }
            }
            Expr::Path(khora_hir::Resolution::Item { name, .. }) => {
                // A generic callee resolves to the specialization this call
                // site asked for; a concrete one keeps its own name.
                let symbol = self
                    .mono
                    .callee(&self.owner.clone(), callee)
                    .unwrap_or_else(|| name.clone());

                // An intrinsic is a *declaration the backend implements*, so
                // the test is that nothing else does. A program with its own
                // `attempt` — the tests in this repository have one — gets its
                // own, and the name means what it was written to mean.
                let is_intrinsic = !self.be.is_defined(&symbol) && args.len() == 1;
                if is_intrinsic && name == "print" {
                    self.print(args[0], range)
                } else if is_intrinsic && name == "assert" {
                    self.assert(args[0], range)
                } else if is_intrinsic && name == "attempt" {
                    self.attempt(site, args[0], range)
                } else {
                    self.call_named(&symbol, site, args, range)
                }
            }
            // `a.show()` — the receiver becomes the first argument, and which
            // impl runs was settled by monomorphization.
            //
            // Unless the callee is a *field* holding a function, which wins
            // over a method of the same name (D2). The checker decided that
            // already, and recorded it by typing the field access as a
            // function; monomorphization has nothing for such a site.
            Expr::Field { base, .. } => match self.mono.callee(&self.owner.clone(), callee) {
                Some(symbol) => {
                    let mut all = vec![base];
                    all.extend_from_slice(args);
                    self.call_named(&symbol, site, &all, range)
                }
                None if matches!(self.types.of(callee), Type::Fn { .. }) => {
                    let shape = FnShape::of(self.types.of(callee))
                        .expect("guarded by the match arm");
                    self.call_closure(site, callee, &shape, args, range)
                }
                None => self.fail(
                    "this method call was not resolved to an impl; that is a compiler bug",
                    range,
                ),
            },
            // A value of function type: a closure, called indirectly.
            _ if matches!(self.types.of(callee), Type::Fn { .. }) => {
                let shape =
                    FnShape::of(self.types.of(callee)).expect("guarded by the match arm");
                self.call_closure(site, callee, &shape, args, range)
            }
            _ => self.fail(
                "only a named function or a constructor can be called; there are no function \
                 values until closures land",
                range,
            ),
        }
    }

    /// Releases an argument, unless the plan passed it as a borrow.
    ///
    /// These intrinsics only *look at* their receiver — the runtime keeps the
    /// finalizer, not the region; the closure, not the cell. They were handed
    /// an owned reference anyway and dropped it here, which cancels out and
    /// costs two atomic operations. Where the plan could see that the argument
    /// is a binding that outlives the call, it now makes no reference and this
    /// releases nothing. `khora_perceus::borrowed_arguments`.
    ///
    /// Still a real release for anything else, because a temporary handed to a
    /// borrowing call is owned by nobody: `Region::defer(Region::open(), f)`
    /// has to be released by somebody and there is no binding to do it.
    pub(super) fn release_unless_lent(&mut self, arg: ExprId, value: BasicValueEnum<'ctx>, ty: &Type) {
        if self.plan.borrowed.contains(&arg) {
            return;
        }
        self.drop(value, ty);
    }

    /// The capabilities a call needs, read out of the caller's own bindings.
    ///
    /// A label is in scope because the caller declared it in its own `with`
    /// clause or bound it in a `with` block — both are locals by the time
    /// lowering runs, which is why installation needs no runtime of its own.
    pub(super) fn evidence_for(
        &mut self,
        name: &str,
        site: ExprId,
        range: TextRange,
    ) -> Option<Vec<BasicValueEnum<'ctx>>> {
        let signature = self.be.signature_of(name)?;
        self.evidence_from_row(&signature.requires, name, site, range)
    }

    /// The same, given a row rather than a signature.
    ///
    /// This is what a call *through a value* uses: the requirement is part of
    /// the callee's type, so the evidence a closure is handed is worked out
    /// exactly as it is for a direct call — same labels, same order, same
    /// ownership.
    pub(super) fn evidence_from_row(
        &mut self,
        requires: &Type,
        name: &str,
        site: ExprId,
        range: TextRange,
    ) -> Option<Vec<BasicValueEnum<'ctx>>> {
        let Type::Row { fields: wanted, .. } = requires else { return Some(Vec::new()) };
        if wanted.is_empty() {
            return Some(Vec::new());
        }

        let wanted = wanted.clone();
        let mut out = Vec::with_capacity(wanted.len());
        for (label, ty) in wanted {
            // By name first, then by type. A capability installed as
            // `with MyDatabase { .. }` is bound under the path that was
            // written, so no lookup by label finds it; the checker matched it
            // to this label and recorded which binding won.
            // `docs/design/capability-installation.md`.
            let found = self.body.capability_at(site, &label).or_else(|| {
                self.body
                    .by_type_at(site)
                    .into_iter()
                    .find(|local| self.types.local(*local) == &ty)
            });
            let Some(local) = found else {
                // Not a binding this body can name, but possibly one it was
                // handed: a `with 'r` clause forwards capabilities it has no
                // name for. Passed on as it arrived, with a `dup` to match the
                // ownership every other argument has.
                if let Some(value) = self.incoming.get(&label).copied() {
                    if is_boxed(&ty) {
                        self.dup(value);
                    }
                    out.push(value);
                    continue;
                }
                self.fail(
                    format!("`{name}` needs the capability `{label}`, which is not in scope"),
                    range,
                );
                return None;
            };
            let (Some(slot), Some(llvm_ty)) =
                (self.slots.get(&local).copied(), self.be.llvm_type(&ty))
            else {
                self.fail(format!("`{label}` has no storage, which is a compiler bug"), range);
                return None;
            };
            let value = self
                .be
                .builder
                .build_load(llvm_ty, slot, &format!("{label}.evidence"))
                .expect("reading a capability");
            // Passed owned, as every other argument is: the callee's plan
            // releases it where its body ends, so the caller hands over a
            // reference of its own rather than lending the one it holds.
            if is_boxed(&ty) {
                self.dup(value);
            }
            out.push(value);
        }
        Some(out)
    }

    pub(super) fn call_named(
        &mut self,
        name: &str,
        site: ExprId,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        let Some(signature) = self.be.signature_of(name) else {
            return self.fail(format!("`{name}` has no signature to call through"), range);
        };
        let function = match self.be.callee(name) {
            Ok(function) => function,
            Err(message) => return self.fail(message, range),
        };

        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.expr(*arg)?.into());
        }
        // Then the capabilities, which the source never writes: the row said
        // which and in what order.
        //
        // Except across the C ABI, where a `with` clause is a *permission*
        // rather than an argument — a foreign function has no use for a Khora
        // record of closures, and requiring one it never receives is how the
        // boundary is governed. Decision 3 in `docs/design/ffi.md`; the
        // checker has already charged the row to this frame either way.
        if self.be.is_defined(name) {
            for capability in self.evidence_for(name, site, range)? {
                values.push(capability.into());
            }
        }

        let call = self.be.builder.build_call(function, &values, "call").expect("a call");
        let result = call.try_as_basic_value().basic();

        // A fallible callee handed back `{ raised, payload }`. Splitting it is
        // the branch `!` marks, and the error path is where this frame's
        // bindings are released on the way out.
        if can_raise(&signature) {
            let result = result.expect("a fallible call returns a tagged value");
            return self.split_tagged(result, &signature.ret, range);
        }

        Some(match signature.ret {
            Type::Unit => self.be.unit_value(),
            _ => result.unwrap_or_else(|| self.be.unit_value()),
        })
    }
}
