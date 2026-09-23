//! The intrinsics that are effects: regions, fibers, nurseries, shared cells.
//!
//! Each is a *declaration the backend fills in* rather than a function written
//! in Khora, and each is here for the same reason: the runtime has to be told
//! how to release what it was handed, and only code generation knows the drop
//! glue for a static type.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// `Region::open` and `Region::defer`.
    ///
    /// Intrinsics rather than externs for one reason: `defer` has to hand the
    /// runtime the closure's *drop routine* alongside the closure. That routine
    /// is generated — one shared function switching on the site tag — so
    /// nothing but the code generator knows the pointer, and a Khora
    /// declaration has nowhere to write it. Everything else about a region is
    /// an ordinary reference-counted object.
    pub(super) fn region_intrinsic(
        &mut self,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            // The program's own region. A reference, like any other: the
            // binding that takes it releases it, and the entry point releases
            // the one the runtime keeps once `main` has returned.
            ("root", []) => {
                let root = self.be.rt.region_root;
                let region = self
                    .be
                    .builder
                    .build_call(root, &[], "region.root")
                    .expect("taking the root region")
                    .try_as_basic_value()
                    .basic()
                    .expect("a region is a value");
                Some(region)
            }
            ("open", []) => {
                let open = self.be.rt.region_open;
                let region = self
                    .be
                    .builder
                    .build_call(open, &[], "region")
                    .expect("opening a region")
                    .try_as_basic_value()
                    .basic()
                    .expect("a region is a value");
                Some(region)
            }
            ("defer", [region_arg, finalizer]) => {
                let region_ty = self.types.of(*region_arg).clone();
                let region = self.expr(*region_arg)?;
                let closure = self.expr(*finalizer)?;

                // Both arrive owned, because the reference-counting plan reads
                // this as the ordinary call it is written as. The runtime keeps
                // the closure — it releases it after calling it — and only
                // borrows the region, so the region's reference is given back
                // here rather than leaked. Getting this backwards is a region
                // whose count never reaches zero and finalizers that never run.
                let glue = self.be.drop_glue(&Type::func(Vec::new(), Type::Unit));
                // How the runtime calls it: a finalizer is a `() -> ()`
                // closure, which hands back a cancellation tag and a word.
                let call = self.be.answered_trampoline(None).as_global_value().as_pointer_value();
                let defer = self.be.rt.region_defer;
                self.be
                    .builder
                    .build_call(defer, &[region.into(), closure.into(), glue.into(), call.into()], "")
                    .expect("deferring a finalizer");
                self.release_unless_lent(*region_arg, region, &region_ty);
                Some(self.be.unit_value())
            }
            _ => self.fail(
                format!("`Region::{name}` is not a region operation the backend knows"),
                range,
            ),
        }
    }

    /// `Shared::of`, `get`, `set` and `update`.
    ///
    /// Intrinsics because the value lives behind a lock the runtime owns, and
    /// generated code cannot reach through one. What crosses is the value as a
    /// single word, plus — once, when the cell is opened — how to release it,
    /// since the runtime cannot know `A`. `docs/design/shared.md`.
    pub(super) fn shared_intrinsic(
        &mut self,
        site: ExprId,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("of", [value]) => {
                let value_ty = self.types.of(*value).clone();
                let held = self.expr(*value)?;
                let word = self.be.to_word(held);
                let boxed = self
                    .be
                    .ctx
                    .bool_type()
                    .const_int(u64::from(self.be.counted_across(&value_ty)), false);
                let glue = self.be.holding_glue(&value_ty);
                let open = self.be.rt.shared_open;
                Some(
                    self.be
                        .builder
                        .build_call(open, &[word.into(), boxed.into(), glue.into()], "shared")
                        .expect("opening a shared cell")
                        .try_as_basic_value()
                        .basic()
                        .expect("a cell is a value"),
                )
            }
            ("get", [cell]) => {
                let cell_ty = self.types.of(*cell).clone();
                let value_ty = self.shared_contents(site, &cell_ty, range)?;
                let handle = self.expr(*cell)?;
                let get = self.be.rt.shared_get;
                let word = self
                    .be
                    .builder
                    .build_call(get, &[handle.into()], "read")
                    .expect("reading a shared cell")
                    .try_as_basic_value()
                    .basic()
                    .expect("a read gives back a word")
                    .into_int_value();
                self.release_unless_lent(*cell, handle, &cell_ty);
                Some(self.be.word_to_value(word, &value_ty))
            }
            ("set", [cell, value]) => {
                let cell_ty = self.types.of(*cell).clone();
                let handle = self.expr(*cell)?;
                let held = self.expr(*value)?;
                let word = self.be.to_word(held);
                let set = self.be.rt.shared_set;
                self.be
                    .builder
                    .build_call(set, &[handle.into(), word.into()], "")
                    .expect("writing a shared cell");
                // The value was handed over; the handle was only borrowed.
                self.release_unless_lent(*cell, handle, &cell_ty);
                Some(self.be.unit_value())
            }
            ("update", [cell, change]) => {
                let cell_ty = self.types.of(*cell).clone();
                let value_ty = self.shared_contents(site, &cell_ty, range)?;
                let change_ty = self.types.of(*change).clone();
                let handle = self.expr(*cell)?;
                let closure = self.expr(*change)?;
                let Some(shim) = self.be.change_shim(&value_ty) else {
                    return self.fail(
                        format!("`{value_ty}` has no machine type, so it cannot be shared"),
                        range,
                    );
                };
                let shim = shim.as_global_value().as_pointer_value();
                let slot = self.entry_slot(self.be.ctx.i64_type().into(), "updated");
                let update = self.be.rt.shared_update;
                let which = self
                    .be
                    .builder
                    .build_call(
                        update,
                        &[handle.into(), closure.into(), shim.into(), slot.into()],
                        "updated",
                    )
                    .expect("updating a shared cell")
                    .try_as_basic_value()
                    .basic()
                    .expect("an update gives back a tag")
                    .into_int_value();
                // Both were lent for the call and neither was kept -- on
                // either path, so before the branch.
                self.drop(closure, &change_ty);
                self.release_unless_lent(*cell, handle, &cell_ty);
                self.leave_if_stopped(which);
                let word = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "updated")
                    .expect("reading the new value")
                    .into_int_value();
                Some(self.be.word_to_value(word, &value_ty))
            }
            ("modify", [cell, change]) => {
                let cell_ty = self.types.of(*cell).clone();
                let value_ty = self.shared_contents(site, &cell_ty, range)?;
                let answer_ty = self.types.of(site).clone();
                let change_ty = self.types.of(*change).clone();
                let handle = self.expr(*cell)?;
                let closure = self.expr(*change)?;
                // **The carrier's own type, taken from the change function's
                // signature.** `Changed` is an ordinary record, so at a scalar
                // instantiation it is held inline and the shim is handed an
                // aggregate rather than a pointer. Naming it `Changed` with no
                // arguments answers that question wrong in both directions.
                let carrier = match &change_ty {
                    Type::Fn { ret, .. } => (**ret).clone(),
                    _ => Type::adt("Changed"),
                };
                let Some(shim) = self.be.modify_shim(&carrier, &value_ty, &answer_ty) else {
                    return self.fail(
                        format!("`{answer_ty}` has no machine type, so it cannot be handed back"),
                        range,
                    );
                };
                let shim = shim.as_global_value().as_pointer_value();
                let slot = self.entry_slot(self.be.ctx.i64_type().into(), "answer");
                let modify = self.be.rt.shared_modify;
                let which = self
                    .be
                    .builder
                    .build_call(
                        modify,
                        &[handle.into(), closure.into(), shim.into(), slot.into()],
                        "modified",
                    )
                    .expect("modifying a shared cell")
                    .try_as_basic_value()
                    .basic()
                    .expect("a modify gives back a tag")
                    .into_int_value();
                self.drop(closure, &change_ty);
                self.release_unless_lent(*cell, handle, &cell_ty);
                self.leave_if_stopped(which);
                let word = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "answer")
                    .expect("reading the answer")
                    .into_int_value();
                Some(self.be.word_to_value(word, &answer_ty))
            }
            _ => self.fail(
                format!("`Shared::{name}` is not an operation the backend knows"),
                range,
            ),
        }
    }

    /// `Channel::bounded`, `send`, `receive`, `close` and `depth`.
    ///
    /// Intrinsics for the same reason `Shared`'s are: the values sit in a queue
    /// the runtime owns behind a lock, and generated code cannot reach through
    /// one. What crosses is the value as a single word, plus -- once, when the
    /// channel is opened -- how to release it, since the runtime cannot know
    /// `A`. `docs/design/channels.md`.
    pub(super) fn channel_intrinsic(
        &mut self,
        site: ExprId,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            // The three constructors differ by one word: what a send does
            // when the queue is full. Written as one arm rather than three
            // near-copies, because the only thing that varies is the number.
            ("bounded", [capacity]) | ("dropping", [capacity]) | ("sliding", [capacity]) => {
                let strategy = match name {
                    "dropping" => 1,
                    "sliding" => 2,
                    _ => 0,
                };
                // The element type is not in the arguments -- these take a
                // number -- so it comes from what the call site was inferred
                // to produce, which is where `Channel<A>` is known.
                let held = self.channel_contents(site, &self.types.of(site).clone(), range)?;
                let room = self.expr(*capacity)?;
                let when_full = self.be.ctx.i64_type().const_int(strategy, false);
                let boxed =
                    self.be.ctx.bool_type().const_int(u64::from(self.be.counted_across(&held)), false);
                let glue = self.be.holding_glue(&held);
                let open = self.be.rt.channel_open;
                Some(
                    self.be
                        .builder
                        .build_call(
                            open,
                            &[room.into(), when_full.into(), boxed.into(), glue.into()],
                            "channel",
                        )
                        .expect("opening a channel")
                        .try_as_basic_value()
                        .basic()
                        .expect("a channel is a value"),
                )
            }
            ("send", [channel, value]) => {
                let channel_ty = self.types.of(*channel).clone();
                let handle = self.expr(*channel)?;
                let held = self.expr(*value)?;
                let word = self.be.to_word(held);
                let send = self.be.rt.channel_send;
                let answered = self
                    .be
                    .builder
                    .build_call(send, &[handle.into(), word.into()], "sent")
                    .expect("sending on a channel")
                    .try_as_basic_value()
                    .basic()
                    .expect("a send answers");
                // The value was handed over -- the queue owns it now, and
                // releases it if the channel was closed, or if this fiber was
                // cancelled before it could find room. The handle was only
                // borrowed.
                self.release_unless_lent(*channel, handle, &channel_ty);
                self.cancelled_empty_handed(answered.into_int_value(), range);
                Some(answered)
            }
            // `receive` waits and `poll` does not; everything else about the
            // two is identical, down to the stack slot, so they share an arm
            // and differ in which runtime function is called.
            ("receive", [channel]) | ("poll", [channel]) => {
                let channel_ty = self.types.of(*channel).clone();
                let held = self.channel_contents(site, &channel_ty, range)?;
                let handle = self.expr(*channel)?;

                // Somewhere for the runtime to put the word. A stack slot
                // rather than a return value because the call has two things
                // to say -- the value, and whether there was one -- and a
                // sentinel word would be a value some `A` could legitimately
                // be.
                let slot = self.entry_slot(self.be.ctx.i64_type().into(), "received");
                let receive = if name == "poll" {
                    self.be.rt.channel_poll
                } else {
                    self.be.rt.channel_receive
                };
                let arrived = self
                    .be
                    .builder
                    .build_call(receive, &[handle.into(), slot.into()], "arrived")
                    .expect("receiving from a channel")
                    .try_as_basic_value()
                    .basic()
                    .expect("a receive answers")
                    .into_int_value();
                let word = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "word")
                    .expect("reading the received word")
                    .into_int_value();
                self.release_unless_lent(*channel, handle, &channel_ty);
                // `poll` never waits, so it can never be the thing a
                // cancellation is stuck behind and carries no row.
                if name != "poll" {
                    self.cancelled_empty_handed(arrived, range);
                }
                let answer_ty = self.types.of(site).clone();
                self.option_of_word(arrived, word, &held, &answer_ty)
            }
            ("close", [channel]) => {
                let channel_ty = self.types.of(*channel).clone();
                let handle = self.expr(*channel)?;
                let close = self.be.rt.channel_close;
                self.be
                    .builder
                    .build_call(close, &[handle.into()], "")
                    .expect("closing a channel");
                self.release_unless_lent(*channel, handle, &channel_ty);
                Some(self.be.unit_value())
            }
            ("depth", [channel]) => {
                let channel_ty = self.types.of(*channel).clone();
                let handle = self.expr(*channel)?;
                let depth = self.be.rt.channel_depth;
                let answer = self
                    .be
                    .builder
                    .build_call(depth, &[handle.into()], "depth")
                    .expect("asking a channel its depth")
                    .try_as_basic_value()
                    .basic()
                    .expect("a depth is a number");
                self.release_unless_lent(*channel, handle, &channel_ty);
                Some(answer)
            }
            _ => self.fail(
                format!("`Channel::{name}` is not a channel operation the backend knows"),
                range,
            ),
        }
    }

    /// `Option::Some(word)` when `present`, and `Option::None` otherwise.
    ///
    /// The first intrinsic to build an ADT out of a value the runtime produced
    /// rather than out of expressions the source wrote. Everything returning an
    /// `Option` until now -- `Vector::get`, `String::index_of` -- was written in
    /// Khora over an intrinsic that could not fail, and a channel cannot be:
    /// "closed and drained" is an answer no `A` can stand in for.
    fn option_of_word(
        &mut self,
        present: inkwell::values::IntValue<'ctx>,
        word: inkwell::values::IntValue<'ctx>,
        held: &Type,
        option_ty: &Type,
    ) -> Flow<'ctx> {
        let (some_tag, _) = self.be.variant_in(None, "Option", "Some")?;
        let (none_tag, _) = self.be.variant_in(None, "Option", "None")?;

        // **`held`, and never `Option::Some`'s declared field.** That field is
        // the type parameter `A` as written in `std::core`, not the type this
        // instantiation carries -- so using it built the payload and chose its
        // drop routine for a type that does not exist at runtime. The symptom
        // was a released object calling an unrelated function as its glue,
        // three frames from anything to do with channels.
        let field_ty = held.clone();

        let function = self
            .be
            .builder
            .get_insert_block()
            .and_then(|block| block.get_parent())
            .expect("a function to build in");
        let some_block = self.be.ctx.append_basic_block(function, "received.some");
        let none_block = self.be.ctx.append_basic_block(function, "received.none");
        let after = self.be.ctx.append_basic_block(function, "received.end");

        self.be
            .builder
            .build_conditional_branch(present, some_block, none_block)
            .expect("branching on whether a value arrived");

        // **An `Option` the program holds inline is built here too.** This is
        // the one place an ADT is made out of a runtime answer rather than out
        // of an expression, so it is also the one place that would go on
        // allocating after every other constructor stopped -- and the reader
        // on the far side of the `phi` reads a tag out of a register.
        let inline = self.be.unboxed_type(option_ty).filter(|_| self.be.unboxed.holds(option_ty));

        self.be.builder.position_at_end(some_block);
        let value = self.be.word_to_value(word, &field_ty);
        let some_value: BasicValueEnum<'ctx> = match inline {
            Some(shape) => {
                let tagged = self
                    .be
                    .builder
                    .build_insert_value(
                        shape.const_zero(),
                        self.be.ctx.i32_type().const_int(u64::from(some_tag), false),
                        0,
                        "some.case",
                    )
                    .expect("writing an inline tag");
                self.be.write_inline(tagged, option_ty, 0, value).into_struct_value().into()
            }
            None => {
                let (_, words) = self.be.field_layout(std::slice::from_ref(&field_ty));
                let object = self.allocate(words, some_tag, "Some");
                self.store_field(object, 0, value, &field_ty);
                object.into()
            }
        };
        self.be.builder.build_unconditional_branch(after).expect("leaving the some arm");
        let some_end = self.be.builder.get_insert_block().expect("the some arm's end");

        self.be.builder.position_at_end(none_block);
        let none_value: BasicValueEnum<'ctx> = match inline {
            // Nothing carried, so nothing but the tag is written: the fields
            // beside it are never read on this arm.
            Some(shape) => self
                .be
                .builder
                .build_insert_value(
                    shape.const_zero(),
                    self.be.ctx.i32_type().const_int(u64::from(none_tag), false),
                    0,
                    "none.case",
                )
                .expect("writing an inline tag")
                .into_struct_value()
                .into(),
            None => self.be.static_variant("Option", "None", none_tag).into(),
        };
        self.be.builder.build_unconditional_branch(after).expect("leaving the none arm");
        let none_end = self.be.builder.get_insert_block().expect("the none arm's end");

        self.be.builder.position_at_end(after);
        let merged = self
            .be
            .builder
            .build_phi(some_value.get_type(), "received.option")
            .expect("merging the two arms");
        merged.add_incoming(&[(&some_value, some_end), (&none_value, none_end)]);
        Some(merged.as_basic_value())
    }

    /// `Outcome::Answered(word)` when the fiber answered, `Outcome::Stopped`
    /// when it was stopped.
    ///
    /// `which` is the tag `khora_fiber_outcome` reported. The caller has
    /// already taken any *error* out of it, so the only two values that reach
    /// here are `STOPPED_WHICH` and an ordinary answer.
    ///
    /// **`answers`, and never `Outcome::Answered`'s declared field.** That
    /// field is the type parameter `A` as written in `std::core`, not the type
    /// this instantiation carries. `option_of_word` records what using it cost
    /// once: the payload was built and its drop routine chosen for a type that
    /// does not exist at run time, and the symptom was a released object
    /// calling an unrelated function as its glue, three frames from anything
    /// to do with the call that produced it. `Outcome::Answered(A)` has
    /// exactly that shape.
    fn outcome_of_word(
        &mut self,
        which: inkwell::values::IntValue<'ctx>,
        word: inkwell::values::IntValue<'ctx>,
        answers: &Type,
        outcome_ty: &Type,
    ) -> Flow<'ctx> {
        let (answered_tag, _) = self.be.variant_in(None, "Outcome", "Answered")?;
        let (stopped_tag, _) = self.be.variant_in(None, "Outcome", "Stopped")?;

        let field_ty = answers.clone();

        let stopped_which = self.be.ctx.i32_type().const_int(runtime::STOPPED_WHICH, false);
        let was_stopped = self
            .be
            .builder
            .build_int_compare(IntPredicate::EQ, which, stopped_which, "outcome.was.stopped")
            .expect("testing whether the fiber was stopped");

        let function = self
            .be
            .builder
            .get_insert_block()
            .and_then(|block| block.get_parent())
            .expect("a function to build in");
        let stopped_block = self.be.ctx.append_basic_block(function, "outcome.stopped");
        let answered_block = self.be.ctx.append_basic_block(function, "outcome.answered");
        let after = self.be.ctx.append_basic_block(function, "outcome.end");

        self.be
            .builder
            .build_conditional_branch(was_stopped, stopped_block, answered_block)
            .expect("branching on what the fiber ended as");

        // Held inline where the type allows it, for the reason `option_of_word`
        // gives: this is one of the few places an ADT is made from a runtime
        // answer rather than from an expression, so it is one of the few that
        // would go on allocating after every other constructor stopped.
        let inline = self.be.unboxed_type(outcome_ty).filter(|_| self.be.unboxed.holds(outcome_ty));

        self.be.builder.position_at_end(answered_block);
        // **The retain belongs here and not before the branch.** Both callers
        // funnel through this block for the answered case, including the
        // empty-row early return, so this covers every path that reads the
        // word -- and the stopped arm, where the word is a zero, never reaches
        // it. `retain_spilled` loads the fields out of the word before it calls
        // the walk, so on a null that load is the crash rather than a no-op.
        if self.be.unboxed.holds(&field_ty) {
            self.retain_spilled(word, &field_ty);
        }
        let value = self.be.word_to_value(word, &field_ty);
        let answered_value: BasicValueEnum<'ctx> = match inline {
            Some(shape) => {
                let tagged = self
                    .be
                    .builder
                    .build_insert_value(
                        shape.const_zero(),
                        self.be.ctx.i32_type().const_int(u64::from(answered_tag), false),
                        0,
                        "answered.case",
                    )
                    .expect("writing an inline tag");
                self.be.write_inline(tagged, outcome_ty, 0, value).into_struct_value().into()
            }
            None => {
                let (_, words) = self.be.field_layout(std::slice::from_ref(&field_ty));
                let object = self.allocate(words, answered_tag, "Answered");
                self.store_field(object, 0, value, &field_ty);
                object.into()
            }
        };
        self.be.builder.build_unconditional_branch(after).expect("leaving the answered arm");
        let answered_end = self.be.builder.get_insert_block().expect("the answered arm's end");

        self.be.builder.position_at_end(stopped_block);
        let stopped_value: BasicValueEnum<'ctx> = match inline {
            // Nothing carried, so nothing but the tag is written: the fields
            // beside it are never read on this arm.
            Some(shape) => self
                .be
                .builder
                .build_insert_value(
                    shape.const_zero(),
                    self.be.ctx.i32_type().const_int(u64::from(stopped_tag), false),
                    0,
                    "stopped.case",
                )
                .expect("writing an inline tag")
                .into_struct_value()
                .into(),
            None => self.be.static_variant("Outcome", "Stopped", stopped_tag).into(),
        };
        self.be.builder.build_unconditional_branch(after).expect("leaving the stopped arm");
        let stopped_end = self.be.builder.get_insert_block().expect("the stopped arm's end");

        self.be.builder.position_at_end(after);
        let merged = self
            .be
            .builder
            .build_phi(answered_value.get_type(), "outcome")
            .expect("merging the two arms");
        merged.add_incoming(&[(&answered_value, answered_end), (&stopped_value, stopped_end)]);
        Some(merged.as_basic_value())
    }

    /// What a `Fiber<A, 'r>` answers, and what it can raise.
    ///
    /// From the handle's own type where it has one, and otherwise from what
    /// the call site was inferred to produce -- the same two places
    /// `channel_contents` looks, and for the same reason: `spawn` is given a
    /// thunk rather than an `A`, so the argument does not always say.
    ///
    /// The row is the second argument, and it is why the row is on the type at
    /// all: it is what lets a join of an infallible fiber compile to a load
    /// rather than to a branch and a `raises` clause nobody wanted.
    pub(super) fn fiber_parts(
        &mut self,
        site: ExprId,
        fiber: &Type,
        range: TextRange,
    ) -> Option<(Type, Type)> {
        for candidate in [fiber, &self.types.of(site).clone()] {
            if let Type::Adt { name, args, .. } = candidate {
                if name == runtime::FIBER_TYPE {
                    if let [answers, raised, ..] = &args[..] {
                        return Some((answers.clone(), raised.clone()));
                    }
                }
            }
        }
        self.fail(format!("`{fiber}` is not a fiber, so it cannot be joined"), range);
        None
    }

    /// What a `Channel<A>` carries, at this instantiation.
    pub(super) fn channel_contents(
        &mut self,
        site: ExprId,
        channel: &Type,
        range: TextRange,
    ) -> Option<Type> {
        if let Type::Adt { name, args, .. } = channel {
            if name == runtime::CHANNEL_TYPE {
                if let Some(first) = args.first() {
                    return Some(first.clone());
                }
            }
        }
        // A `bounded` whose receiver type is not known, but whose result is:
        // the call site says `Channel<A>` even when the argument is a number.
        if let Type::Adt { name, args, .. } = &self.types.of(site).clone() {
            if name == runtime::CHANNEL_TYPE {
                if let Some(first) = args.first() {
                    return Some(first.clone());
                }
            }
        }
        // Same shape as `shared_contents`: report through `fail` for the
        // diagnostic and answer `None`, because the caller wants a type and
        // `fail` answers with a value.
        self.fail(format!("`{channel}` is not a channel"), range);
        None
    }

    /// What a `Shared<A>` holds, at this instantiation.
    pub(super) fn shared_contents(&mut self, site: ExprId, cell: &Type, range: TextRange) -> Option<Type> {
        if let Type::Adt { name, args, .. } = cell {
            if name == runtime::SHARED_TYPE {
                if let Some(first) = args.first() {
                    return Some(first.clone());
                }
            }
        }
        // A `get` whose result type is known even when the receiver's is not:
        // the two are the same type said twice, so either will do.
        let result = self.types.of(site).clone();
        if !matches!(result, Type::Unknown) {
            return Some(result);
        }
        self.fail(format!("`{cell}` is not a shared cell"), range);
        None
    }

    /// `SharedFn::of` and `SharedFn::call`.
    ///
    /// **The wrapper is not there at runtime.** A `SharedFn<A, B, 'e>` *is* the
    /// closure — `of` returns its argument untouched and `call` is an ordinary
    /// closure call — because the whole of what the wrapper does happened in
    /// the checker, at the one line where the captures were visible. Paying for
    /// a proof at runtime would be paying twice.
    ///
    /// The shape `call` needs is read off the wrapper's own type arguments,
    /// which monomorphization has already made concrete: `SharedFn<A, B, 'e>`
    /// says the closure takes an `A`, gives back a `B` and fails with `'e`.
    pub(super) fn shared_fn_intrinsic(
        &mut self,
        site: ExprId,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("of", [closure]) => Some(self.expr(*closure)?),
            ("call", [wrapper, argument]) => {
                let wrapped = self.types.of(*wrapper).clone();
                let Type::Adt { name: owner, args: parameters, .. } = &wrapped else {
                    return self.fail(format!("`{wrapped}` is not a `SharedFn`"), range);
                };
                if owner != runtime::SHARED_FN_TYPE || parameters.len() < 3 {
                    return self.fail(format!("`{wrapped}` is not a `SharedFn`"), range);
                }
                let signature = FnShape {
                    params: vec![parameters[0].clone()],
                    ret: parameters[1].clone(),
                    // Always empty: a closure captures the capabilities it
                    // uses, so there is nothing left for a caller to supply.
                    requires: Type::empty_row(),
                    raises: parameters[2].clone(),
                };
                let closure = self.expr(*wrapper)?.into_pointer_value();
                let given = vec![self.expr(*argument)?];
                let invoked =
                    self.invoke_closure_at(site, *wrapper, closure, &signature, given, range)?;
                let ret = signature.ret.clone();
                self.after_invoke(invoked, &ret, range)
            }
            _ => self.fail(
                format!("`SharedFn::{name}` is not an operation the backend knows"),
                range,
            ),
        }
    }

    /// `Fiber::spawn`, `Fiber::join` and `Fiber::cancel`.
    ///
    /// Intrinsics for the same reason the region ones are: `spawn` hands the
    /// runtime a closure, and the runtime has to be told how to release it
    /// when the fiber finishes. Everything else about a fiber handle is an
    /// ordinary reference-counted object — including that releasing it joins,
    /// which is where structured concurrency comes from.
    pub(super) fn fiber_intrinsic(
        &mut self,
        site: ExprId,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("spawn", [body]) => {
                // Whether the thunk returns the tagged pair, which is how a
                // fiber says it was cancelled or that it failed, and what it
                // answers when it does not. Both read from the thunk's own
                // type: a closure carries its rows, so these are facts about
                // the value rather than guesses about it.
                let (fallible, answers) = match self.types.of(*body) {
                    Type::Fn { raises, ret, .. } => (
                        !matches!(
                            &**raises,
                            Type::Row { fields, tail } if fields.is_empty() && tail.is_none()
                        ),
                        (**ret).clone(),
                    ),
                    _ => (false, Type::Unit),
                };
                // Handed over, not lent: the fiber releases the closure when
                // it finishes, so this gives up the reference the plan gave it.
                let closure = self.expr(*body)?;
                let glue = self.be.drop_glue(&Type::func(Vec::new(), Type::Unit));
                // **One trampoline shape for every thunk.** A fallible thunk
                // hands back its tag and writes its word through a pointer; an
                // infallible one hands back a cancellation tag and its answer,
                // which the answered trampoline turns into the same two
                // things. So the runtime reads a stopped fiber the same way
                // whatever its row.
                let call = if fallible {
                    self.be.tagged_trampoline(1).as_global_value().as_pointer_value()
                } else {
                    // `Unit` is carried as an `i64` zero in the pair, so the
                    // shim invents the word rather than converting one.
                    let returns = match answers {
                        Type::Unit | Type::Never => None,
                        ref other => self.be.llvm_type(other),
                    };
                    self.be.answered_trampoline(returns).as_global_value().as_pointer_value()
                };
                let plain = self.be.null_pointer();
                // How to let go of an answer nobody joined. An *error* is
                // always a boxed `Adt`, which the runtime knows; only the
                // successful word needs describing.
                let boxed =
                    self.be.ctx.bool_type().const_int(u64::from(self.be.counted_across(&answers)), false);
                let value_glue = self.be.holding_glue(&answers);
                let spawn = self.be.rt.fiber_spawn;
                let fiber = self
                    .be
                    .builder
                    .build_call(
                        spawn,
                        &[
                            closure.into(),
                            glue.into(),
                            call.into(),
                            plain.into(),
                            boxed.into(),
                            value_glue.into(),
                        ],
                        "fiber",
                    )
                    .expect("spawning a fiber")
                    .try_as_basic_value()
                    .basic()
                    .expect("a fiber handle is a value");
                Some(fiber)
            }
            ("cancel", [fiber]) | ("detach", [fiber]) | ("abort", [fiber]) => {
                let ty = self.types.of(*fiber).clone();
                let handle = self.expr(*fiber)?;
                let call = match name {
                    "detach" => self.be.rt.fiber_detach,
                    "abort" => self.be.rt.fiber_force,
                    _ => self.be.rt.fiber_cancel,
                };
                self.be
                    .builder
                    .build_call(call, &[handle.into()], "")
                    .expect("acting on a fiber");
                // Borrowed, not consumed — the handle is still the caller's,
                // and the plan handed this frame an owned reference.
                self.release_unless_lent(*fiber, handle, &ty);
                Some(self.be.unit_value())
            }
            ("cancel_within", [fiber, millis]) => {
                let ty = self.types.of(*fiber).clone();
                let handle = self.expr(*fiber)?;
                let millis = self.expr(*millis)?;
                self.be
                    .builder
                    .build_call(self.be.rt.fiber_cancel_within, &[handle.into(), millis.into()], "")
                    .expect("cancelling a fiber with a deadline");
                self.release_unless_lent(*fiber, handle, &ty);
                Some(self.be.unit_value())
            }
            ("wait", [fiber]) => {
                // **A cancellation point, unlike `cancel` and `detach`.** A
                // waiter parked here observed nothing until the child finished
                // on its own, so a `main` ending in a wait could not be
                // stopped at all (roadmap §16.7). The runtime answers
                // `CANCELLED_WHICH` when the *waiter* was asked to stop, and
                // the branch below is the `!` that unwinds on it -- which is
                // why `Fiber::wait` carries a `raises 'er` row.
                let ty = self.types.of(*fiber).clone();
                let handle = self.expr(*fiber)?;
                let which = self
                    .be
                    .builder
                    .build_call(self.be.rt.fiber_wait, &[handle.into()], "wait.which")
                    .expect("waiting for a fiber")
                    .try_as_basic_value()
                    .basic()
                    .expect("a wait answers")
                    .into_int_value();
                // Borrowed, like `cancel`: waiting does not consume the handle.
                self.release_unless_lent(*fiber, handle, &ty);
                // No answer travels with it, so the word is the unit every
                // other `()`-returning fallible call carries.
                let word = self.be.ctx.i64_type().const_zero();
                let tagged = self.be.tagged_of(which, word);
                self.split_tagged(tagged, &Type::Unit, range)
            }
            ("finished", [fiber]) => {
                let ty = self.types.of(*fiber).clone();
                let handle = self.expr(*fiber)?;
                let answer = self
                    .be
                    .builder
                    .build_call(self.be.rt.fiber_finished, &[handle.into()], "fiber.finished")
                    .expect("asking whether a fiber has finished")
                    .try_as_basic_value()
                    .basic()
                    .expect("a bool is a value");
                // Borrowed, like `wait` and `cancel`: asking does not consume.
                self.release_unless_lent(*fiber, handle, &ty);
                Some(answer)
            }
            ("cancelled", [fiber]) => {
                // **The one question about a cancelled fiber that does not
                // unwind the asker.** `join` on a cancelled fiber answers
                // `CANCELLED_WHICH`, which is in no row and so no `catch` can
                // name -- at the entry point that ends the program at 130. This
                // is a `Bool` and an ordinary call, with no tag to split on.
                //
                // No branch on the fiber's row, unlike `join`: what comes back
                // is a fact about the fiber rather than its answer, so a fiber
                // whose row is empty and one that can fail are lowered the same
                // way. That is what keeps this out of the `announce` gate's
                // way -- the runtime reads the fiber's own flag, and nothing
                // here reads the stored word at all.
                let ty = self.types.of(*fiber).clone();
                let handle = self.expr(*fiber)?;
                let answer = self
                    .be
                    .builder
                    .build_call(self.be.rt.fiber_cancelled, &[handle.into()], "fiber.cancelled")
                    .expect("asking whether a fiber was cancelled")
                    .try_as_basic_value()
                    .basic()
                    .expect("a bool is a value");
                // Borrowed, like `finished`: a supervisor asks and keeps
                // supervising, so consuming the handle would make this unusable
                // in the loop it exists for.
                self.release_unless_lent(*fiber, handle, &ty);
                Some(answer)
            }
            ("outcome", [fiber]) => {
                // **`join`'s lowering with the cancellation kept rather than
                // raised.** Everything up to the branch is the same, for the
                // same reasons -- the stack slot, the retain on an inline
                // answer, the borrow-aware release -- and the difference is
                // what happens to a child that was stopped: `join` lets
                // `CANCELLED_WHICH` travel out as a raise no `catch` can name,
                // and this builds `Outcome::Stopped` instead.
                let ty = self.types.of(*fiber).clone();
                let (answers, _) = self.fiber_parts(site, &ty, range)?;
                let handle = self.expr(*fiber)?;

                let slot = self.entry_slot(self.be.ctx.i64_type().into(), "outcome.answer");
                let which = self
                    .be
                    .builder
                    .build_call(
                        self.be.rt.fiber_outcome,
                        &[handle.into(), slot.into()],
                        "outcome.which",
                    )
                    .expect("asking a fiber what it ended as")
                    .try_as_basic_value()
                    .basic()
                    .expect("an outcome answers")
                    .into_int_value();
                let word = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "outcome.word")
                    .expect("reading the answered word")
                    .into_int_value();

                // **The same double free `join` documents, and it applies here
                // unchanged.** An answer held inline crosses as a word
                // pointing at the box it was spilled into; reading the fields
                // back out copies whatever they hold into this frame without
                // counting it, and then this frame and the fiber's stored
                // answer release the same pointer.
                //
                // Before the branch, because `Answered` is built on one arm
                // and the empty-row path returns without reaching either --
                // both read the word, so a retain on one of them is a retain
                // on half the paths that need it.
                //
                // **On the answered arm only, because a stopped word is a
                // zero.** `retain_spilled` is not a runtime test: it decides
                // from the *type* whether a walk exists, then loads the fields
                // out of the word to hand them to it. On the stopped arm that
                // load is from the null page, and the program dies -- reported
                // by the stack guard as "the stack ran out", three frames from
                // anything to do with fibers, which is the same misdirection
                // `option_of_word`'s comment was written about. It bites only
                // when the answer is held inline *and* owns a counted field,
                // which is the one case the retain exists for.
                self.release_unless_lent(*fiber, handle, &ty);

                let outcome_ty = self.types.of(site).clone();
                // A child *failure* is still this frame's to re-raise, which
                // is the whole of what `raises 'er` on this method means.
                // `split_tagged` is the branch every fallible call emits, and
                // `STOPPED_WHICH` is not an error tag -- so the stopped case
                // is taken out of `which` first, and what reaches
                // `split_tagged` is the answer-or-error pair `join` would have
                // had.
                //
                // Whatever the fiber's row: a `which` of `CANCELLED_WHICH`
                // says *this* frame was stopped while it waited, and that
                // leaves through the same branch.
                let stopped = self.be.ctx.i32_type().const_int(runtime::STOPPED_WHICH, false);
                let was_stopped = self
                    .be
                    .builder
                    .build_int_compare(IntPredicate::EQ, which, stopped, "outcome.stopped")
                    .expect("testing for a stopped child");
                let raise_it = self.block("outcome.raised");
                let build_it = self.block("outcome.built");
                self.be
                    .builder
                    .build_conditional_branch(was_stopped, build_it, raise_it)
                    .expect("branching on whether the child was stopped");

                // Not stopped: an answer, the child's error, or a cancellation
                // of this frame. The last two leave here. **The word is not
                // converted on this path**: an answer held inline crosses in a
                // box that converting it frees, and `outcome_of_word` converts
                // it on the far side -- converting it here as well freed the
                // box twice.
                self.at(raise_it);
                let left = self.raised(which);
                let leave = self.block("outcome.leave");
                self.be
                    .builder
                    .build_conditional_branch(left, leave, build_it)
                    .expect("branching on the tag");
                self.at(leave);
                self.leave_with(which, word);

                self.at(build_it);
                self.outcome_of_word(which, word, &answers, &outcome_ty)
            }
            ("join", [fiber]) => {
                let ty = self.types.of(*fiber).clone();
                let (answers, _) = self.fiber_parts(site, &ty, range)?;
                let handle = self.expr(*fiber)?;

                // A stack slot rather than a return value, for the reason
                // every other tagged call across this boundary uses one: two
                // things come back and a 16-byte aggregate is a thing LLVM and
                // rustc lay out separately.
                let slot = self.entry_slot(self.be.ctx.i64_type().into(), "answer");
                let join = self.be.rt.fiber_join;
                let which = self
                    .be
                    .builder
                    .build_call(join, &[handle.into(), slot.into()], "which")
                    .expect("joining a fiber")
                    .try_as_basic_value()
                    .basic()
                    .expect("a join answers")
                    .into_int_value();
                let word = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "word")
                    .expect("reading the joined word")
                    .into_int_value();

                // **A joined inline value has to be retained on the way out.**
                //
                // The answer to a `join` is one word, and for a type held
                // inline that word is a box the value crossed in. The runtime
                // hands out a reference to the *box* per joiner and keeps its
                // own, which is what makes joining twice joining once. But a
                // reference to the box is not a reference to what the box
                // holds: reading the value back out copies a `String` pointer
                // into this frame without counting it, and then two owners --
                // this frame and the fiber's stored answer -- release the same
                // pointer. The abort lands on whichever gets there second, as
                // a double free somewhere with no evidence of a fiber in it.
                //
                // A `Channel` has no matching bug because `receive` *moves*
                // the value out of the queue, so the count is right by there
                // being one owner throughout.
                //
                // Both exits below read the word, so this is not on either of
                // their branches: a fiber whose row is empty returns straight
                // out of `word_to_value` and would otherwise take the same
                // uncounted copy.
                if self.be.unboxed.holds(&answers) {
                    self.retain_spilled(word, &answers);
                }
                self.release_unless_lent(*fiber, handle, &ty);

                // **The child's failure becomes this frame's**, which is
                // what `join` re-raising means: the two halves are already in
                // the shape `split_tagged` wants, so the branch and the
                // unwinding are the ones every fallible call already emits.
                //
                // **Whatever the fiber's row.** A child with no row can still
                // be stopped -- every infallible thunk hands back a
                // cancellation tag -- and a joiner handed a stopped child's
                // zero would compute with an answer nobody produced. So the
                // join unwinds on it instead, as it does on a cancellation of
                // the joiner itself.
                let tagged = self.be.tagged_of(which, word);
                self.split_tagged(tagged, &answers, range)
            }
            _ => self.fail(
                format!("`Fiber::{name}` is not a fiber operation the backend knows"),
                range,
            ),
        }
    }

    /// Which of `<`, `>`, `<=`, `>=` an `Ordering` answers.
    ///
    /// `Ord::cmp` hands back a three-way answer, because one comparison should
    /// decide all four operators rather than four calls deciding them
    /// separately — `docs/design` has the argument at `Ordering`'s
    /// declaration. Reading it here is a tag comparison.
    ///
    /// `<=` is *not* `Less`-or-`Equal` spelled out; it is "not `Greater`". Two
    /// tests rather than one, and the same answer, so the cheaper one wins.
    ///
    /// The `Ordering` is a heap object like any other nullary variant, and it
    /// is released here — one allocation per comparison, which is exactly what
    /// phase 9's reuse analysis exists to remove and is not worth a special
    /// case before then.
    pub(super) fn read_ordering(
        &mut self,
        op: BinOp,
        answer: BasicValueEnum<'ctx>,
        range: TextRange,
    ) -> Flow<'ctx> {
        // Named with its home, because whether an `Ordering` is a register
        // or a pointer is answered from the declaration and not from the name.
        let ordering = self.be.named_type("Ordering", Some("Less"));
        let (Some((less, _)), Some((greater, _))) = (
            self.be.variant_of("Ordering", "Less"),
            self.be.variant_of("Ordering", "Greater"),
        ) else {
            self.drop(answer, &ordering);
            return self.fail(
                "`Ord::cmp` produces an `Ordering`, which has `Less`, `Equal` and `Greater`",
                range,
            );
        };

        let tag = self.case_of(answer, &ordering);
        let against = self.be.ctx.i32_type().const_int(
            u64::from(if matches!(op, BinOp::Lt | BinOp::Ge) { less } else { greater }),
            false,
        );
        // `<` is "is Less"; `>=` is "is not Less"; `>` is "is Greater"; `<=` is
        // "is not Greater". One tag read and one comparison for all four.
        let predicate = if matches!(op, BinOp::Lt | BinOp::Gt) {
            IntPredicate::EQ
        } else {
            IntPredicate::NE
        };
        let decided = self
            .be
            .builder
            .build_int_compare(predicate, tag, against, "ordered")
            .expect("reading an `Ordering`");
        self.drop(answer, &ordering);
        Some(decided.into())
    }

    /// `Fibers::open`, `Fibers::adopt` and `Fibers::wait`.
    ///
    /// A nursery holds fiber handles, and adopting one grows the list — which
    /// is why the list lives in the runtime and this is an intrinsic rather
    /// than an extern. `adopt` takes the handle's reference; the nursery
    /// releases it once the fiber has been waited for.
    pub(super) fn nursery_intrinsic(
        &mut self,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("open", []) => {
                let open = self.be.rt.fibers_open;
                let fibers = self
                    .be
                    .builder
                    .build_call(open, &[], "fibers")
                    .expect("opening a nursery")
                    .try_as_basic_value()
                    .basic()
                    .expect("a nursery is a value");
                Some(fibers)
            }
            ("bounded", [limit]) => {
                let cap = self.expr(*limit)?;
                let open = self.be.rt.fibers_bounded;
                Some(
                    self.be
                        .builder
                        .build_call(open, &[cap.into()], "fibers")
                        .expect("opening a bounded nursery")
                        .try_as_basic_value()
                        .basic()
                        .expect("a nursery is a value"),
                )
            }
            ("adopt", [nursery, fiber]) => {
                let nursery_ty = self.types.of(*nursery).clone();
                let handle = self.expr(*nursery)?;
                let child = self.expr(*fiber)?;
                let adopt = self.be.rt.fibers_adopt;
                self.be
                    .builder
                    .build_call(adopt, &[handle.into(), child.into()], "")
                    .expect("adopting a fiber");
                // The nursery keeps the fiber's reference and only borrows its
                // own, so exactly one of the two is given back.
                self.release_unless_lent(*nursery, handle, &nursery_ty);
                Some(self.be.unit_value())
            }
            ("wait", [nursery]) => {
                let nursery_ty = self.types.of(*nursery).clone();
                let handle = self.expr(*nursery)?;
                let wait = self.be.rt.fibers_wait;
                // How many children ended with an error, which `std` turns
                // into a `ChildFailed`. The runtime cannot raise it itself:
                // every child's error has a type of its own and a nursery
                // holds them as bare handles, so a count is what it has.
                let failed = self
                    .be
                    .builder
                    .build_call(wait, &[handle.into()], "failed")
                    .expect("waiting for a nursery")
                    .try_as_basic_value()
                    .basic()
                    .expect("a count is a value");
                self.release_unless_lent(*nursery, handle, &nursery_ty);
                Some(failed)
            }
            _ => self.fail(
                format!("`Fibers::{name}` is not a nursery operation the backend knows"),
                range,
            ),
        }
    }
}
