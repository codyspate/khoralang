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
                let defer = self.be.rt.region_defer;
                self.be
                    .builder
                    .build_call(defer, &[region.into(), closure.into(), glue.into()], "")
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
                let boxed = self.be.ctx.bool_type().const_int(
                    u64::from(is_boxed(&value_ty)),
                    false,
                );
                let glue = self.be.drop_glue(&value_ty);
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
                let update = self.be.rt.shared_update;
                let word = self
                    .be
                    .builder
                    .build_call(update, &[handle.into(), closure.into(), shim.into()], "updated")
                    .expect("updating a shared cell")
                    .try_as_basic_value()
                    .basic()
                    .expect("an update gives back a word")
                    .into_int_value();
                // Both were lent for the call and neither was kept.
                self.drop(closure, &change_ty);
                self.release_unless_lent(*cell, handle, &cell_ty);
                Some(self.be.word_to_value(word, &value_ty))
            }
            ("modify", [cell, change]) => {
                let cell_ty = self.types.of(*cell).clone();
                let value_ty = self.shared_contents(site, &cell_ty, range)?;
                let answer_ty = self.types.of(site).clone();
                let change_ty = self.types.of(*change).clone();
                let handle = self.expr(*cell)?;
                let closure = self.expr(*change)?;
                let Some(shim) = self.be.modify_shim(&value_ty, &answer_ty) else {
                    return self.fail(
                        format!("`{answer_ty}` has no machine type, so it cannot be handed back"),
                        range,
                    );
                };
                let shim = shim.as_global_value().as_pointer_value();
                let slot = self
                    .be
                    .builder
                    .build_alloca(self.be.ctx.i64_type(), "answer")
                    .expect("somewhere for the answer");
                let modify = self.be.rt.shared_modify;
                self.be
                    .builder
                    .build_call(
                        modify,
                        &[handle.into(), closure.into(), shim.into(), slot.into()],
                        "modified",
                    )
                    .expect("modifying a shared cell");
                let word = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "answer")
                    .expect("reading the answer")
                    .into_int_value();
                self.drop(closure, &change_ty);
                self.release_unless_lent(*cell, handle, &cell_ty);
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
            ("bounded", [capacity]) => {
                // The element type is not in the arguments -- `bounded` takes a
                // number -- so it comes from what the call site was inferred
                // to produce, which is where `Channel<A>` is known.
                let held = self.channel_contents(site, &self.types.of(site).clone(), range)?;
                let room = self.expr(*capacity)?;
                let boxed =
                    self.be.ctx.bool_type().const_int(u64::from(is_boxed(&held)), false);
                let glue = self.be.drop_glue(&held);
                let open = self.be.rt.channel_open;
                Some(
                    self.be
                        .builder
                        .build_call(
                            open,
                            &[room.into(), boxed.into(), glue.into()],
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
                // releases it if the channel was closed. The handle was only
                // borrowed.
                self.release_unless_lent(*channel, handle, &channel_ty);
                Some(answered)
            }
            ("receive", [channel]) => {
                let channel_ty = self.types.of(*channel).clone();
                let held = self.channel_contents(site, &channel_ty, range)?;
                let handle = self.expr(*channel)?;

                // Somewhere for the runtime to put the word. A stack slot
                // rather than a return value because the call has two things
                // to say -- the value, and whether there was one -- and a
                // sentinel word would be a value some `A` could legitimately
                // be.
                let slot = self
                    .be
                    .builder
                    .build_alloca(self.be.ctx.i64_type(), "received")
                    .expect("a slot for the received word");
                let receive = self.be.rt.channel_receive;
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
                self.option_of_word(arrived, word, &held)
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

        self.be.builder.position_at_end(some_block);
        let value = self.be.word_to_value(word, &field_ty);
        let object = self.allocate(1, some_tag, "Some");
        self.store_field(object, 0, value, &field_ty);
        let some_value: BasicValueEnum<'ctx> = object.into();
        self.be.builder.build_unconditional_branch(after).expect("leaving the some arm");
        let some_end = self.be.builder.get_insert_block().expect("the some arm's end");

        self.be.builder.position_at_end(none_block);
        let none_value: BasicValueEnum<'ctx> =
            self.be.static_variant("Option", "None", none_tag).into();
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
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("spawn", [body]) => {
                // Whether the thunk returns the tagged pair, which is how a
                // fiber says it was cancelled or that it failed. Read from the
                // thunk's own type: a closure carries its error row, so this
                // is a fact about the value rather than a guess about it.
                let fallible = match self.types.of(*body) {
                    Type::Fn { raises, .. } => !matches!(
                        &**raises,
                        Type::Row { fields, tail } if fields.is_empty() && tail.is_none()
                    ),
                    _ => false,
                };
                // Handed over, not lent: the fiber releases the closure when
                // it finishes, so this gives up the reference the plan gave it.
                let closure = self.expr(*body)?;
                let glue = self.be.drop_glue(&Type::func(Vec::new(), Type::Unit));
                // Null for a thunk that cannot fail; otherwise the trampoline
                // that takes its tagged return apart on this side of the
                // boundary. See `Backend::tagged_trampoline`.
                let call = if fallible {
                    self.be.tagged_trampoline(1).as_global_value().as_pointer_value()
                } else {
                    self.be.null_pointer()
                };
                let spawn = self.be.rt.fiber_spawn;
                let fiber = self
                    .be
                    .builder
                    .build_call(spawn, &[closure.into(), glue.into(), call.into()], "fiber")
                    .expect("spawning a fiber")
                    .try_as_basic_value()
                    .basic()
                    .expect("a fiber handle is a value");
                Some(fiber)
            }
            ("join", [fiber]) | ("cancel", [fiber]) => {
                let ty = self.types.of(*fiber).clone();
                let handle = self.expr(*fiber)?;
                let call = if name == "join" {
                    self.be.rt.fiber_join
                } else {
                    self.be.rt.fiber_cancel
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
        let ordering = Type::adt("Ordering");
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

        let tag = runtime::load_tag(self.be.ctx, &self.be.builder, answer.into_pointer_value());
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
                self.be
                    .builder
                    .build_call(wait, &[handle.into()], "")
                    .expect("waiting for a nursery");
                self.release_unless_lent(*nursery, handle, &nursery_ty);
                Some(self.be.unit_value())
            }
            _ => self.fail(
                format!("`Fibers::{name}` is not a nursery operation the backend knows"),
                range,
            ),
        }
    }
}
