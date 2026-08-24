//! Failure, cancellation, and the tagged return that carries both.
//!
//! A fallible function returns `{ which, payload }`. Zero is a value, and the
//! two reserved values are an error and a cancellation — which travels the same
//! channel deliberately, so that a `catch` that names error constructors cannot
//! swallow one. `docs/design/effect-runtime.md` §6.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// The `attempt` intrinsic: run a computation and make its failure a value.
    ///
    /// The tagged return is already "an error or a value"; this is the same
    /// thing with a name the type system can see. An intrinsic rather than a
    /// library function because catching *whatever* a body raises is not
    /// something `catch` can express — `catch` names constructors, and this
    /// names none.
    ///
    /// It is what makes retrying possible at all: a policy that runs a
    /// computation again cannot know what the computation failed with.
    pub(super) fn attempt(&mut self, site: ExprId, body: ExprId, range: TextRange) -> Flow<'ctx> {
        let Some(shape) = FnShape::of(self.types.of(body)) else {
            return self.fail("`attempt` takes a function to run", range);
        };
        let result_ty = self.types.of(site).clone();
        let Type::Adt { name: result_name, .. } = result_ty.clone() else {
            return self.fail("`attempt` produces a `Result`", range);
        };
        let (Some((ok_tag, ok_info)), Some((err_tag, err_info))) = (
            self.be.variant_of(&result_name, "Ok").map(|(t, i)| (t, i.clone())),
            self.be.variant_of(&result_name, "Err").map(|(t, i)| (t, i.clone())),
        ) else {
            return self.fail("`attempt` produces a `Result`, which has `Ok` and `Err`", range);
        };

        let Invoked { raw, fallible } = self.invoke_closure(site, body, &shape, &[], range)?;
        let slot = self.result_slot(&result_ty);

        if !fallible {
            // Nothing to catch. Still a `Result`, because the *type* says so:
            // a caller reading one should not have to know whether the body it
            // passed happened to be infallible.
            let value = raw.unwrap_or_else(|| self.be.unit_value());
            let object = self.allocate(ok_info.fields.len(), ok_tag, &result_name);
            let field = ok_info.fields.first().cloned().unwrap_or(Type::Unit);
            self.store_field(object, 0, value, &field);
            self.leave_scope();
            return Some(object.into());
        }

        let tagged = raw.expect("a fallible closure returns a tagged value");
        let (which, word) = self.read_tagged(tagged);
        let raised = self.raised(which);
        let failed = self.block("attempt.err");
        let succeeded = self.block("attempt.ok");
        let merge = self.block("attempt.end");
        self.be
            .builder
            .build_conditional_branch(raised, failed, succeeded)
            .expect("branching on the tag");

        self.at(succeeded);
        let ok_field = ok_info.fields.first().cloned().unwrap_or(Type::Unit);
        let value = self.be.word_to_value(word, &ok_field);
        let object = self.allocate(ok_info.fields.len(), ok_tag, &result_name);
        self.store_field(object, 0, value, &ok_field);
        self.store_result(slot, object.into());
        self.br(merge);

        self.at(failed);
        let err_field = err_info.fields.first().cloned().unwrap_or(Type::Unit);
        let error = self.be.word_to_value(word, &err_field);
        let object = self.allocate(err_info.fields.len(), err_tag, &result_name);
        self.store_field(object, 0, error, &err_field);
        self.store_result(slot, object.into());
        self.br(merge);

        self.at(merge);
        self.leave_scope();
        Some(self.load_result(slot, &result_ty))
    }

    /// The `assert` intrinsic.
    ///
    /// A false assertion leaves the test the way a raise leaves a function:
    /// release what this frame owns, and return with a tag. The tag is
    /// reserved, so no `catch` can name a failed assertion and only the runner
    /// reads it.
    ///
    /// Only inside a test, and inside one it needs no `!`. That is the one
    /// place the mark rule bends, and it is bounded here rather than in the
    /// checker so that the bend is impossible to reach from ordinary code.
    pub(super) fn assert(&mut self, condition: ExprId, range: TextRange) -> Flow<'ctx> {
        if !khora_hir::is_test(&self.owner) {
            return self.fail(
                "`assert` is only allowed inside a `test` block; elsewhere, `raise` says the \
                 same thing and says where it goes"
                    .to_string(),
                range,
            );
        }

        let held = self.expr(condition)?.into_int_value();
        let failed = self.block("assert.failed");
        let held_ok = self.block("assert.ok");
        self.be
            .builder
            .build_conditional_branch(held, held_ok, failed)
            .expect("branching on an assertion");

        self.at(failed);
        let which = self.be.ctx.i32_type().const_int(runtime::FAILED_WHICH, false);
        let none = self.be.ctx.i64_type().const_zero();
        self.leave_with(which, none);

        self.at(held_ok);
        Some(self.be.unit_value())
    }

    /// `raise e` — leave the function carrying the error.
    ///
    /// Everything the frame owns is released first, exactly as an early
    /// `return` releases it. A raise *is* a return, with a tag.
    pub(super) fn lower_raise(&mut self, error: ExprId, range: TextRange) -> Flow<'ctx> {
        // An enclosing `catch` is the other place an error can go, so a
        // function with no `raises` clause may still contain a `raise` — as
        // long as something between here and the signature handles it. The
        // checker has already decided that; this only has to agree.
        if !self.raises && self.catches.is_empty() {
            return self.fail(
                "this function has no `raises` clause, so it cannot raise",
                range,
            );
        }
        // Which error type this is comes from the checker's record, not from
        // the expression's shape: `raise e` may raise a bound variable whose
        // type only inference knows.
        let which = match self.types.of(error) {
            Type::Adt { name, .. } => self.be.error_id(&name.clone()),
            other => {
                let other = other.clone();
                return self
                    .fail(format!("`{other}` is not an error type, so it cannot be raised"), range);
            }
        };
        let value = self.expr(error)?;
        let which = self.be.ctx.i32_type().const_int(u64::from(which), false);
        let word = self.be.to_word(value);
        self.leave_with(which, word);
        None
    }

    /// Returns a value from a fallible function without raising.
    pub(super) fn return_ok(&mut self, payload: BasicValueEnum<'ctx>) {
        let none = self.be.ctx.i32_type().const_zero();
        self.return_tagged(none, payload);
    }

    /// Returns `{ which, payload }` from a fallible function.
    ///
    /// `which` is 0 to return normally and otherwise the error's type id. It
    /// is a value rather than a constant because propagating an error onward
    /// passes through whatever id arrived, which no frame in the middle knows.
    pub(super) fn return_tagged(&mut self, which: IntValue<'ctx>, payload: BasicValueEnum<'ctx>) {
        let tagged = self.be.tagged_type();
        let word = self.be.to_word(payload);

        let value = self
            .be
            .builder
            .build_insert_value(tagged.get_undef(), which, 0, "tagged.which")
            .expect("setting the tag");
        let value = self
            .be
            .builder
            .build_insert_value(value, word, 1, "tagged")
            .expect("setting the payload");
        self.be
            .builder
            .build_return(Some(&value.into_struct_value()))
            .expect("returning a tagged value");
    }

    /// Takes a tagged return apart into its `which` and its payload word.
    pub(super) fn read_tagged(
        &mut self,
        result: BasicValueEnum<'ctx>,
    ) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let aggregate = result.into_struct_value();
        let which = self
            .be
            .builder
            .build_extract_value(aggregate, 0, "which")
            .expect("reading the tag")
            .into_int_value();
        let word = self
            .be
            .builder
            .build_extract_value(aggregate, 1, "payload")
            .expect("reading the payload")
            .into_int_value();
        (which, word)
    }

    /// Whether a `which` says the call raised — that is, whether it is not 0.
    pub(super) fn raised(&mut self, which: IntValue<'ctx>) -> IntValue<'ctx> {
        let none = self.be.ctx.i32_type().const_zero();
        self.be
            .builder
            .build_int_compare(IntPredicate::NE, which, none, "raised")
            .expect("testing the tag")
    }

    /// Leaves at a cancellation point if a cancellation is pending.
    ///
    /// Emitted only where this function can return a tagged value, because
    /// that is the only channel a cancellation can travel on. A function with
    /// no error channel cannot report one and does not need to: the flag is
    /// the state of record, and the caller's next cancellation point sees it.
    /// `docs/design/effect-runtime.md` §6.
    pub(super) fn check_cancellation(&mut self, range: TextRange) {
        if !self.raises || self.aborted {
            return;
        }
        let _ = range;

        let asked = self
            .be
            .builder
            .build_call(self.be.rt.cancelled, &[], "cancelled")
            .expect("reading the cancellation flag")
            .try_as_basic_value()
            .basic()
            .expect("a flag is a value")
            .into_int_value();
        let zero = self.be.ctx.i8_type().const_zero();
        let pending = self
            .be
            .builder
            .build_int_compare(IntPredicate::NE, asked, zero, "cancel.pending")
            .expect("testing the cancellation flag");

        let stop = self.block("cancel.stop");
        let carry_on = self.block("cancel.no");
        self.be
            .builder
            .build_conditional_branch(pending, stop, carry_on)
            .expect("branching on the cancellation flag");

        // The same way out an error takes: release what this frame owns — the
        // regions among it, so their finalizers run — and hand the tag on.
        self.at(stop);
        let which = self.be.ctx.i32_type().const_int(runtime::CANCELLED_WHICH, false);
        let none = self.be.ctx.i64_type().const_zero();
        self.leave_with(which, none);

        self.at(carry_on);
    }

    /// Sends an error on from the block it was found in.
    ///
    /// Out of the function, releasing the whole frame — or, inside a `catch`,
    /// into that `catch`'s handler, releasing only what the operand opened.
    /// The frame stays alive in the second case, which is the entire
    /// difference between handling an error and propagating one.
    pub(super) fn leave_with(&mut self, which: IntValue<'ctx>, word: IntValue<'ctx>) {
        match self.catches.last() {
            Some(frame) => {
                let (handler, depth) = (frame.handler, frame.scope_depth);
                let (which_phi, word_phi) = (frame.which, frame.word);
                self.unwind_to(depth);
                let from = self.here();
                which_phi.add_incoming(&[(&which, from)]);
                word_phi.add_incoming(&[(&word, from)]);
                self.br(handler);
            }
            // Nowhere left: no enclosing `catch` and no `raises` clause.
            //
            // For an *error* the checker guarantees this is unreachable — a
            // total `catch` still emits its fall-through, and that is the only
            // way to get here. For a *cancellation* it is reachable, because a
            // cancellation is not in any row and so nothing the checker looked
            // at ruled it out. There is no frame between here and the entry
            // point that could carry it, so the entry point's outcome is
            // produced here instead. `docs/design/effect-runtime.md` §6.
            None if !self.raises => {
                let cancelled = self.be.ctx.i32_type().const_int(runtime::CANCELLED_WHICH, false);
                let is_cancel = self
                    .be
                    .builder
                    .build_int_compare(IntPredicate::EQ, which, cancelled, "cancelled")
                    .expect("testing for a cancellation");
                let stop = self.block("cancel.nowhere");
                let sealed = self.block("error.impossible");
                self.be
                    .builder
                    .build_conditional_branch(is_cancel, stop, sealed)
                    .expect("branching on the tag");

                self.at(stop);
                let cancel_stop = self.be.rt.cancel_stop;
                self.be.builder.build_call(cancel_stop, &[], "").expect("stopping");
                self.be.builder.build_unreachable().expect("sealing after a stop");

                self.at(sealed);
                self.be.builder.build_unreachable().expect("sealing an unhandled error");
            }
            None => {
                self.unwind_to(0);
                let error = self.be.word_to_value(word, &Type::Str);
                self.return_tagged(which, error);
            }
        }
    }

    /// Splits a fallible call's result: propagate the error, or take the value.
    ///
    /// This is the branch `!` marks. On the error path every binding this frame
    /// owns is released and the error is returned onward, which is the whole of
    /// unwinding — no tables, no personality routine.
    pub(super) fn split_tagged(
        &mut self,
        result: BasicValueEnum<'ctx>,
        ret: &Type,
        range: TextRange,
    ) -> Flow<'ctx> {
        if !self.raises && self.catches.is_empty() {
            return self.fail(
                "this call can leave the function, but the function has no `raises` clause",
                range,
            );
        }

        let (which, word) = self.read_tagged(result);

        let propagate = self.block("raised");
        let continue_to = self.block("ok");
        let raised = self.raised(which);
        self.be
            .builder
            .build_conditional_branch(raised, propagate, continue_to)
            .expect("branching on the tag");

        self.at(propagate);
        self.leave_with(which, word);

        self.at(continue_to);
        Some(self.be.word_to_value(word, ret))
    }
}
