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
            let held = self.field_types(&result_ty, "Ok").unwrap_or_else(|| ok_info.fields.clone());
            let (_, words) = self.be.field_layout(&held);
            let object = self.allocate(words, ok_tag, &result_name);
            let field = held.first().cloned().unwrap_or(Type::Unit);
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
        let ok_held = self.field_types(&result_ty, "Ok").unwrap_or_else(|| ok_info.fields.clone());
        let ok_field = ok_held.first().cloned().unwrap_or(Type::Unit);
        let value = self.be.word_to_value(word, &ok_field);
        let (_, ok_words) = self.be.field_layout(&ok_held);
        let object = self.allocate(ok_words, ok_tag, &result_name);
        self.store_field(object, 0, value, &ok_field);
        self.store_result(slot, object.into());
        self.br(merge);

        // Two things travel this channel without being errors, and `catch`
        // already routes both of them back to propagating — see
        // `lower_catch`, where a `_` arm sends `CANCELLED_WHICH` and
        // `FAILED_WHICH` to `onward` by name. `attempt` is the *other* total
        // handler and it did not, which is a hole in the same rule:
        // `effect-runtime.md` §6 says a cancellation cannot be swallowed
        // because nothing a program writes names it, and `attempt` names
        // nothing at all.
        //
        // It was worse than a swallow. A cancellation carries no payload, so
        // the word is zero, and the `Err` built from it held a null typed as
        // the body's error — `Result::Err(problem)` whose `problem.show()`
        // reads through it. Found by 13.3, whose rolled-back transaction ran
        // fallible work in a finalizer and got back `Err` from a body that had
        // been cancelled rather than having failed.
        self.at(failed);
        let escape = self.block("attempt.onward");
        let erred = self.block("attempt.raised");
        let cancelled = self.be.ctx.i32_type().const_int(runtime::CANCELLED_WHICH, false);
        let aborted = self.be.ctx.i32_type().const_int(runtime::FAILED_WHICH, false);
        let is_cancel = self
            .be
            .builder
            .build_int_compare(IntPredicate::EQ, which, cancelled, "attempt.cancelled")
            .expect("testing for a cancellation");
        let is_abort = self
            .be
            .builder
            .build_int_compare(IntPredicate::EQ, which, aborted, "attempt.aborted")
            .expect("testing for a failed assertion");
        let not_an_error = self
            .be
            .builder
            .build_or(is_cancel, is_abort, "attempt.escapes")
            .expect("either of the two");
        self.be
            .builder
            .build_conditional_branch(not_an_error, escape, erred)
            .expect("branching on the tag");

        self.at(escape);
        self.leave_with(which, word);

        self.at(erred);
        let err_held =
            self.field_types(&result_ty, "Err").unwrap_or_else(|| err_info.fields.clone());
        let err_field = err_held.first().cloned().unwrap_or(Type::Unit);
        let error = self.be.word_to_value(word, &err_field);
        let (_, err_words) = self.be.field_layout(&err_held);
        let object = self.allocate(err_words, err_tag, &result_name);
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

        // Counted before the condition is lowered, so a nested `assert` in a
        // closure cannot renumber the one it is inside.
        self.asserts += 1;
        let ordinal = self.asserts;

        let held = self.expr(condition)?.into_int_value();
        let failed = self.block("assert.failed");
        let held_ok = self.block("assert.ok");
        self.be
            .builder
            .build_conditional_branch(held, held_ok, failed)
            .expect("branching on an assertion");

        self.at(failed);
        // **Which one.** A failing test used to say only that it had failed,
        // so finding out which of six assertions it was meant deleting them
        // one at a time.
        let say = self.be.rt.assert_failed;
        let ordinal = self.be.ctx.i32_type().const_int(u64::from(ordinal), false);
        // **The line, as an immediate.** It is known here and costs a constant
        // in the call, so it works in a release build exactly as it does in a
        // debug one — which was the objection to reporting one at all, on the
        // belief that a line has to come from debug information. It does not.
        let line = self.be.ctx.i32_type().const_int(u64::from(line_of(&self.be.source, range)), false);
        self.be
            .builder
            .build_call(say, &[ordinal.into(), line.into()], "")
            .expect("reporting which assertion failed");
        let which = self.be.ctx.i32_type().const_int(runtime::FAILED_WHICH, false);
        let none = self.be.ctx.i64_type().const_zero();
        self.leave_with(which, none);

        self.at(held_ok);
        Some(self.be.unit_value())
    }

    /// `assert_that(condition, message)` — an assertion that says what it saw.
    ///
    /// **The same branch, plus a sentence.** `assert` reports an ordinal and a
    /// line, which says *which* assertion failed and not *why*; recovering the
    /// value meant adding a `print` and building again, and on a program that
    /// links `std` that is the better part of a minute for a fact the test
    /// already had in its hand.
    ///
    /// The message is only evaluated on the failing path. It is built by string
    /// interpolation at the call site -- `assert_that(ok, "port ${l.port}")` --
    /// so it costs nothing at all in a passing test, which is every test almost
    /// every time.
    ///
    /// **A message rather than `assert_eq`'s two rendered values.** An
    /// `assert_eq<A: Show>` was the other candidate and is the worse trade: the
    /// `Show` bound excludes types a test may legitimately compare, and
    /// "left/right" is the wrong sentence whenever the useful thing to say is
    /// neither operand -- which key was missing, which input produced this.
    /// Interpolation already exists, already composes, and needs no bound.
    pub(super) fn assert_that(
        &mut self,
        condition: ExprId,
        message: ExprId,
        range: TextRange,
    ) -> Flow<'ctx> {
        if !khora_hir::is_test(&self.owner) {
            return self.fail(
                "`assert_that` is only allowed inside a `test` block; elsewhere, `raise` says \
                 the same thing and says where it goes"
                    .to_string(),
                range,
            );
        }

        // Counted with `assert`'s, and before the condition is lowered, so the
        // ordinals a reader sees are the order the assertions are written in
        // whichever form they take.
        self.asserts += 1;
        let ordinal = self.asserts;

        let held = self.expr(condition)?.into_int_value();
        let failed = self.block("assert.failed");
        let held_ok = self.block("assert.ok");
        self.be
            .builder
            .build_conditional_branch(held, held_ok, failed)
            .expect("branching on an assertion");

        self.at(failed);
        // **Built on this side of the branch**, which is the whole reason the
        // message is an expression rather than a string the caller formats
        // first: a passing assertion never allocates it.
        let text = self.expr(message)?.into_pointer_value();
        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, text, STRING_LEN_FIELD);
        let length = self
            .be
            .builder
            .build_load(self.be.ctx.i64_type(), length_slot, "assert.len")
            .expect("reading the message length");
        let bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            text,
            STRING_BYTES_OFFSET,
            "assert.bytes",
        );

        let say = self.be.rt.assert_that_failed;
        let ordinal = self.be.ctx.i32_type().const_int(u64::from(ordinal), false);
        let line = self
            .be
            .ctx
            .i32_type()
            .const_int(u64::from(line_of(&self.be.source, range)), false);
        self.be
            .builder
            .build_call(
                say,
                &[ordinal.into(), line.into(), bytes.into(), length.into()],
                "",
            )
            .expect("reporting which assertion failed");
        // The message is this frame's to release, and the frame is leaving.
        self.drop(text.into(), &Type::Str);
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
            ty @ Type::Adt { .. } => {
                let ty = ty.clone();
                self.be.error_id(&ty)
            }
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

    /// Returns `{ which, answer }` from an infallible function that carries a
    /// cancellation tag. `answer` is at the function's own answer type, or an
    /// `i64` zero for `()`.
    pub(super) fn return_plain_tagged(&mut self, which: IntValue<'ctx>, answer: BasicValueEnum<'ctx>) {
        let shape = self.plain_pair_type();
        let value = self
            .be
            .builder
            .build_insert_value(shape.get_undef(), which, 0, "t.which")
            .expect("setting the tag");
        let value = self
            .be
            .builder
            .build_insert_value(value, answer, 1, "t.answer")
            .expect("setting the answer");
        self.be
            .builder
            .build_return(Some(&value.into_struct_value()))
            .expect("returning a tagged answer");
    }

    /// The ordinary way out of a tagged infallible function: its answer, with
    /// a zero tag. `()` and `Never` travel as the `i64` zero.
    pub(super) fn return_plain_ok(&mut self, value: BasicValueEnum<'ctx>) {
        let answer = match self.ret {
            Type::Unit | Type::Never => self.be.ctx.i64_type().const_zero().into(),
            _ => value,
        };
        let ok = self.be.ctx.i32_type().const_zero();
        self.return_plain_tagged(ok, answer);
    }

    /// The pair this function returns. Only asked of a tagged function.
    pub(super) fn plain_pair_type(&self) -> inkwell::types::StructType<'ctx> {
        self.function
            .get_type()
            .get_return_type()
            .expect("a tagged function returns a pair")
            .into_struct_type()
    }

    /// A zero of this function's answer type: the answer half of a pair whose
    /// tag says there is no answer. Nobody reads it.
    fn plain_answer_zero(&self) -> BasicValueEnum<'ctx> {
        let field = self.plain_pair_type().get_field_type_at_index(1).expect("an answer field");
        match field {
            BasicTypeEnum::PointerType(p) => p.const_null().into(),
            BasicTypeEnum::IntType(i) => i.const_zero().into(),
            BasicTypeEnum::FloatType(f) => f.const_zero().into(),
            BasicTypeEnum::StructType(s) => s.const_zero().into(),
            BasicTypeEnum::ArrayType(a) => a.const_zero().into(),
            BasicTypeEnum::VectorType(v) => v.const_zero().into(),
            BasicTypeEnum::ScalableVectorType(v) => v.const_zero().into(),
        }
    }

    /// Leaves on `which` if it is not 0, and carries on if it is.
    ///
    /// For a runtime call that hands back a change function's cancellation
    /// tag rather than the pair a Khora callee returns: `Shared::update` and
    /// `modify`. The same branch [`Self::split_cancelled`] emits, without an
    /// answer half to take.
    pub(super) fn leave_if_stopped(&mut self, which: IntValue<'ctx>) {
        let stopped = self.raised(which);
        let stop = self.block("t.cancelled");
        let carry_on = self.block("t.ok");
        self.be
            .builder
            .build_conditional_branch(stopped, stop, carry_on)
            .expect("branching on the tag");
        self.at(stop);
        let none = self.be.ctx.i64_type().const_zero();
        self.leave_with(which, none);
        self.at(carry_on);
    }

    /// The branch after a call to a tagged infallible Khora function: unwind
    /// if it came back cancelled, take the answer if it did not.
    ///
    /// The branch `split_tagged` emits, with the answer at its own type. It
    /// needs no `raises` clause, because every frame that reaches it has a way
    /// out: a fallible one's row, a tagged one's pair, or a `catch`, which
    /// sends the cancellation on rather than handling it.
    pub(super) fn split_cancelled(&mut self, result: BasicValueEnum<'ctx>, ret: &Type) -> BasicValueEnum<'ctx> {
        let pair = result.into_struct_value();
        let which = self
            .be
            .builder
            .build_extract_value(pair, 0, "t.which")
            .expect("reading the tag")
            .into_int_value();
        let answer = self
            .be
            .builder
            .build_extract_value(pair, 1, "t.answer")
            .expect("reading the answer");
        self.leave_if_stopped(which);
        match ret {
            Type::Unit | Type::Never => self.be.unit_value(),
            _ => answer,
        }
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

    /// Whether this frame has a way out for a cancellation: a `raises` row or
    /// a cancellation tag.
    ///
    /// **False only in a function [`crate::backend::can_stop`] pruned**, and
    /// such a function has no cancellation point to emit, by the definition of
    /// pruned. So a false here that mattered would be a disagreement between
    /// the analysis and the lowering, and the cost of one is a cancellation
    /// observed a frame later rather than a miscompile.
    pub(super) fn can_leave_on_a_cancel(&self) -> bool {
        (self.raises || self.tagged) && !self.aborted
    }

    /// Leaves at a cancellation point if a cancellation is pending.
    ///
    /// Emitted wherever this function can hand a cancellation on, which is
    /// every function that can reach a cancellation point:
    /// [`Self::can_leave_on_a_cancel`].
    ///
    /// **Behind [`Self::poll`], so a `!` in a loop is not a call per trip.**
    /// Only the count of cancelled fibers is consulted here, not the pool
    /// half, because a `!` has no safepoint to take.
    pub(super) fn check_cancellation(&mut self, range: TextRange) {
        if !self.can_leave_on_a_cancel() {
            return;
        }
        let _ = range;
        let (slow, carry_on) = self.poll(Some(runtime::POLL_CANCELLED));
        self.at(slow);
        self.ask_about_cancellation();
        self.br(carry_on);
        self.at(carry_on);
    }

    /// [`Self::check_cancellation`] after a runtime export that gives up on a
    /// cancel, asked only when `result` is the answer a give-up can be.
    /// `super::calls::GaveUp` says which that is. The answer is a machine
    /// word or a raw pointer the runtime owns, so leaving drops nothing.
    pub(super) fn check_cancellation_on(
        &mut self,
        gave_up: super::calls::GaveUp,
        result: Option<BasicValueEnum<'ctx>>,
        range: TextRange,
    ) {
        use super::calls::GaveUp;
        if !self.can_leave_on_a_cancel() {
            return;
        }
        let failed = match (gave_up, result) {
            (GaveUp::Always, _) => None,
            (GaveUp::Negative, Some(BasicValueEnum::IntValue(n))) => Some(
                self.be
                    .builder
                    .build_int_compare(IntPredicate::SLT, n, n.get_type().const_zero(), "gave.up")
                    .expect("testing for a failure"),
            ),
            (GaveUp::Null, Some(BasicValueEnum::PointerValue(p))) => Some(
                self.be.builder.build_is_null(p, "gave.up").expect("testing for a null"),
            ),
            // An export on the list declared with an answer of a different
            // shape: checking every answer would drop a real one, so this
            // asks nothing, and the fiber stops a step later.
            (GaveUp::Negative | GaveUp::Null, _) => return,
        };
        let Some(failed) = failed else {
            self.check_cancellation(range);
            return;
        };
        let ask = self.block("gave.up.ask");
        let carry_on = self.block("gave.up.no");
        self.be
            .builder
            .build_conditional_branch(failed, ask, carry_on)
            .expect("branching on the failure value");
        self.at(ask);
        self.check_cancellation(range);
        self.br(carry_on);
        self.at(carry_on);
    }

    /// Calls `khora_cancelled` and leaves if it says so. The slow half of
    /// every cancellation check; the caller has already decided to ask.
    pub(super) fn ask_about_cancellation(&mut self) {
        let asker = self.be.rt.cancelled;
        self.ask_about_cancellation_with(asker);
    }

    /// The same, asking `asker`, which answers 1 when this frame should stop.
    /// `khora_back_edge` is the other one: the safepoint and this question in
    /// one call.
    pub(super) fn ask_about_cancellation_with(&mut self, asker: FunctionValue<'ctx>) {
        let asked = self
            .be
            .builder
            .build_call(asker, &[], "cancelled")
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

    /// A cancellation point on the path where a blocking channel operation
    /// gave up.
    ///
    /// `Channel::send` and `Channel::receive` are cancellation points, but
    /// only when they come back **empty-handed**: a receive that got a value
    /// hands it over, and the fiber stops at its next cancellation point
    /// instead. Unwinding while holding the value would drop it on the floor
    /// -- a message taken off the channel and seen by nobody.
    ///
    /// The runtime keeps the other half of that bargain: it checks the
    /// cancellation flag only once it has established there is nothing to
    /// take, so a send racing the cancellation still wins.
    pub(super) fn cancelled_empty_handed(&mut self, moved: IntValue<'ctx>, range: TextRange) {
        if !self.can_leave_on_a_cancel() {
            return;
        }
        let empty = self.block("moved.not");
        let carry_on = self.block("moved.yes");
        self.be
            .builder
            .build_conditional_branch(moved, carry_on, empty)
            .expect("branching on whether the channel moved");

        self.at(empty);
        self.check_cancellation(range);
        self.br(carry_on);

        self.at(carry_on);
    }

    /// Sends an error on from the block it was found in.
    ///
    /// Out of the function, releasing the whole frame — or, inside a `catch`,
    /// into that `catch`'s handler, releasing only what the operand opened.
    /// The frame stays alive in the second case, which is the entire
    /// difference between handling an error and propagating one.
    pub(super) fn leave_with(&mut self, which: IntValue<'ctx>, word: IntValue<'ctx>) {
        // **A reuse token held here is freed on the way out.** An arm that
        // may build in its matched cell takes the cell at its head and spends
        // it at its constructor; a cancellation point between the two -- the
        // recursive call in `Cons(h, walk(t))` is one -- leaves with the
        // token in hand, and the token is memory no counter and no owner can
        // see. Only a cancellation reaches here holding one: an arm that can
        // leave on an error, a `break` or a `return` is never given a token
        // (`khora_perceus` `may_leave_early`). Emitted on the leaving path
        // only, so the path that reaches the constructor pays nothing.
        if let Some((_, token)) = self.reuse.clone() {
            let free_reuse = self.be.rt.free_reuse;
            self.be
                .builder
                .build_call(free_reuse, &[token.into()], "")
                .expect("freeing a reuse token on the way out");
        }
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
            // **A tagged frame hands the cancellation on.** Every Khora
            // function that can reach a cancellation point returns a tag,
            // whatever its row, so this is the ordinary way out of one: release
            // the whole frame -- the regions among it, so their finalizers run
            // -- and return the tag. The answer half is never read, because
            // the caller branches on the tag first. An *error* cannot get
            // here: the checker has ruled out an unhandled one in a function
            // with no row.
            None if !self.raises && self.tagged => {
                self.unwind_to(0);
                let answer = self.plain_answer_zero();
                self.return_plain_tagged(which, answer);
            }
            // **Nowhere left: a frame `can_stop` pruned, with no `catch`
            // around this point.** Refused rather than emitted, because
            // nothing correct can be emitted here: the frame has no tag to
            // hand a cancellation on with and no row to hand an error on with.
            //
            // Nothing sound calls this. A pruned frame contains no
            // cancellation point and calls nothing tagged, so no
            // `CANCELLED_WHICH` exists in it; the checker has ruled out an
            // unhandled error in a function with no row; and `assert`, the
            // one source of `FAILED_WHICH`, is only allowed in a test, which
            // has a row. A `catch`'s fall-through was the one site that
            // emitted this path anyway, and [`Self::lower_catch`] seals it
            // itself in such a frame.
            //
            // A call that arrives here is `can_stop` and the lowering
            // disagreeing about the frame. It used to become a call that
            // returned a zero nobody computed, or ended the process; a
            // compiler panic is the direction that cannot ship. A frame that
            // has already failed to lower is exempt: the error it reported is
            // the one to show.
            None if !self.raises => {
                assert!(
                    self.aborted,
                    "`{}`: a frame with no `raises` row and no cancellation tag has nowhere to \
                     send an error or a cancellation; `can_stop` pruned a frame the lowering \
                     leaves from",
                    self.owner
                );
                self.be.builder.build_unreachable().expect("sealing a frame that already failed");
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
        if !self.raises && !self.tagged && self.catches.is_empty() {
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

    /// [`Self::split_tagged`] for a word some structure keeps: the answer is
    /// read with [`Backend::reload_kept`] rather than taken over.
    ///
    /// A fiber's stored answer is the case. The read is on the answer's side
    /// of the branch only, because on the other side the word may be the zero
    /// a stopped child leaves, and counting what a kept value holds loads
    /// through the word.
    pub(super) fn split_tagged_kept(
        &mut self,
        result: BasicValueEnum<'ctx>,
        ret: &Type,
        range: TextRange,
    ) -> Flow<'ctx> {
        if !self.raises && !self.tagged && self.catches.is_empty() {
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
        Some(self.be.reload_kept(word, ret))
    }
}

/// The one-based line `at` starts on, or 0 when there is no source to count in.
///
/// Counting newlines rather than keeping a table: an `assert` is rare, this
/// runs once per one at compile time, and a table would have to be built for
/// every file whether or not it had any.
fn line_of(source: &str, at: TextRange) -> u32 {
    if source.is_empty() {
        return 0;
    }
    let offset = usize::from(at.start()).min(source.len());
    u32::try_from(source[..offset].bytes().filter(|b| *b == b'\n').count() + 1)
        .unwrap_or(0)
}
