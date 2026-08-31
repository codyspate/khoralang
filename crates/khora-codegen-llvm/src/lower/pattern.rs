//! `match`, `catch`, and the tests a pattern compiles to.
//!
//! Dispatch has two shapes and the second earns its place: when every arm is an
//! unguarded constructor pattern the tag goes into one LLVM `switch`, and
//! anything else becomes a chain of tests in written order, which is the only
//! shape that gets fallthrough right.
//!
//! This is also where an arm takes ownership of what it bound and where the
//! scrutinee is released — early enough that the arm's own constructor can be
//! built in the cell it matched. `docs/design/reuse.md` §2.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// Lowers a `match`.
    ///
    /// # Who owns the scrutinee
    ///
    /// The plan `dup`s a boxed scrutinee at the read and records nothing for
    /// the arm bindings, which is the right call: the bindings *borrow* fields
    /// out of the scrutinee, so releasing them would free something the parent
    /// still points at. What follows is that the `match` itself owns the
    /// scrutinee for the whole of the arm, and releases it afterwards — after
    /// the body, because an arm that returns a binding has to `dup` it out
    /// first, and before the value escapes, because nothing else will.
    ///
    /// It goes on the scope stack rather than being dropped at the join, so a
    /// `return` inside an arm releases it too.
    pub(super) fn lower_match(
        &mut self,
        id: ExprId,
        scrutinee: ExprId,
        arms: &[MatchArm],
        range: TextRange,
    ) -> Flow<'ctx> {
        let scrutinee_ty = self.types.of(scrutinee).clone();
        let value = self.expr(scrutinee)?;

        // **The scrutinee is released at the head of each arm**, not here, so
        // that its count can reach zero before the arm's own constructor —
        // which is the whole prerequisite for handing that memory over.
        // `docs/design/reuse.md` §2.
        //
        // The scope is still pushed and still holds it, because a *guard* runs
        // before any of that: a guard that raises, breaks or continues unwinds
        // through here and has to release it. `emit_arms` empties this level
        // for the length of each arm's body, which is the only stretch where
        // the release has already happened.
        self.scopes.push(if is_boxed(&scrutinee_ty) {
            vec![Cleanup::Temp(value, scrutinee_ty.clone())]
        } else {
            Vec::new()
        });

        let result_ty = self.types.of(id).clone();
        let slot = self.result_slot(&result_ty);
        let merge = self.block("match.end");
        let on = Scrutinee {
            value,
            ty: scrutinee_ty.clone(),
            released_by_arms: true,
        };
        let reached = self.emit_arms(arms, &on, slot, merge, range);

        // Popped, not released: every edge that reaches the join released the
        // scrutinee itself.
        self.scopes.pop();
        self.at(merge);
        if reached == 0 {
            self.be.builder.build_unreachable().expect("sealing an unreachable join");
            return None;
        }
        Some(self.load_result(slot, &result_ty))
    }

    /// `f()! catch { .. }` — the same branch `!` already emits, with the
    /// handled error types diverted to arms instead of returned onward.
    ///
    /// The dispatch is two levels. `which` says which error *type* arrived, and
    /// a type nobody named falls through to the ordinary propagate path, so a
    /// partial `catch` costs one extra `switch` and nothing else. Within a
    /// named type the arms dispatch on the object tag exactly as a `match`
    /// does, which is why they share `emit_arms`.
    ///
    /// **A `_` arm takes the fall-through instead.** It has no error type, so
    /// grouping the arms by the type they name skipped it entirely and the
    /// switch's default still propagated — the checker said the function could
    /// not fail while the code still took the unhandled path, which for a
    /// program with no `raises` clause meant walking into `unreachable`.
    ///
    /// Two things it must not swallow. A **cancellation** is not an error and
    /// is in nobody's row, so it keeps the propagate path by an explicit case;
    /// a `_` that stopped one would break every nursery. A **test failure**
    /// travels the same channel to the runner, and goes the same way.
    ///
    /// Releasing what it caught is [`Backend::release_error`]: there is no
    /// static type here to take drop glue from, so the id does it at runtime.
    pub(super) fn lower_catch(
        &mut self,
        id: ExprId,
        inner: ExprId,
        arms: &[MatchArm],
        range: TextRange,
    ) -> Flow<'ctx> {
        let result_ty = self.types.of(id).clone();
        let slot = self.result_slot(&result_ty);
        let merge = self.block("catch.end");

        // The phis have to exist before the operand is lowered, because each
        // `!` inside it adds an edge as it is emitted.
        let entry = self.here();
        let handler = self.block("catch.raised");
        self.at(handler);
        let which = self
            .be
            .builder
            .build_phi(self.be.ctx.i32_type(), "which")
            .expect("the error type that arrived");
        let word = self
            .be
            .builder
            .build_phi(self.be.ctx.i64_type(), "error")
            .expect("the error that arrived");
        self.at(entry);

        let depth = self.scopes.len();
        self.catches.push(CatchFrame { handler, which, word, scope_depth: depth });
        let value = self.expr(inner);
        self.catches.pop();

        let mut reached = 0;
        if let Some(value) = value {
            self.store_result(slot, value);
            self.br(merge);
            reached += 1;
        }

        // An operand that cannot raise leaves the handler with no way in. The
        // checker reports that as an error of its own, so this only has to
        // emit something a verifier will accept.
        if which.count_incoming() == 0 {
            self.at(handler);
            self.be.builder.build_unreachable().expect("sealing an unreachable handler");
            return self.join(merge, reached, slot, &result_ty);
        }

        // Group the arms by the error type they name, keeping written order so
        // the emitted blocks read in the order the source does.
        let mut caught: Vec<(String, Vec<MatchArm>)> = Vec::new();
        let mut everything: Option<MatchArm> = None;
        for arm in arms {
            let Some(owner) = self.owner_of(arm.pat) else {
                // A binding takes everything a `_` does. The difference is on
                // the way out: `_` has no name to release the error under, and
                // this does.
                if matches!(
                    self.body.pat(arm.pat),
                    khora_hir::body::Pat::Wildcard | khora_hir::body::Pat::Bind(_)
                ) {
                    everything = Some(arm.clone());
                }
                continue;
            };
            match caught.iter_mut().find(|(name, _)| name == &owner) {
                Some((_, mine)) => mine.push(arm.clone()),
                None => caught.push((owner, vec![arm.clone()])),
            }
        }

        let onward = self.block("catch.onward");
        let cases: Vec<(inkwell::values::IntValue<'ctx>, BasicBlock<'ctx>)> = caught
            .iter()
            .map(|(owner, _)| {
                let id = self.be.error_id(owner);
                let tag = self.be.ctx.i32_type().const_int(u64::from(id), false);
                (tag, self.block(&format!("catch.{owner}")))
            })
            .collect();

        // Where anything the arms did not name goes. Without a `_` that is
        // still the propagate path; with one it is the arm, and the two things
        // that travel this channel without being errors are routed back to
        // propagating by name.
        let (fallthrough, mut escapes) = match &everything {
            None => (onward, Vec::new()),
            Some(_) => (
                self.block("catch.rest"),
                vec![runtime::CANCELLED_WHICH, runtime::FAILED_WHICH],
            ),
        };
        let mut cases = cases;
        for escape in escapes.drain(..) {
            cases.push((self.be.ctx.i32_type().const_int(escape, false), onward));
        }

        self.at(handler);
        let which = which.as_basic_value().into_int_value();
        let word = word.as_basic_value().into_int_value();
        self.be
            .builder
            .build_switch(which, fallthrough, &cases)
            .expect("dispatching on the error type");

        // Not ours: release the frame and hand it to whoever is next. Nested
        // `catch`es chain here, since `leave_with` looks at the stack again
        // and this runs with the inner frame already popped.
        self.at(onward);
        self.leave_with(which, word);

        for ((owner, mine), (_, block)) in caught.iter().zip(&cases) {
            self.at(*block);
            let error_ty = Type::adt(owner);
            let error = self.be.word_to_value(word, &error_ty);

            // The raising frame moved the error into its return, so this frame
            // owns it. The arms borrow their bindings out of it, exactly as a
            // `match` borrows out of a temporary scrutinee, and it is released
            // on the way to the join.
            let released = self.block(&format!("catch.{owner}.done"));
            self.scopes.push(vec![Cleanup::Temp(error, error_ty.clone())]);
            let on = Scrutinee {
                value: error,
                ty: error_ty.clone(),
                released_by_arms: false,
            };
            let reached_here = self.emit_arms(mine, &on, slot, released, range);
            let scope = self.scopes.pop().unwrap_or_default();

            self.at(released);
            if reached_here == 0 {
                self.be.builder.build_unreachable().expect("sealing a diverging handler");
                continue;
            }
            for cleanup in scope.into_iter().rev() {
                self.release(cleanup);
            }
            self.br(merge);
            reached += 1;
        }

        if let Some(arm) = everything {
            self.at(fallthrough);

            // **A bound arm is the named path with the type read elsewhere.**
            // A constructor arm knows what it caught because it says so; a
            // binding knows because the operand raises one type and the
            // checker gave the local that type. From there it is the same
            // shape: own the error, let the arm read it, release it on the way
            // to the join.
            if let khora_hir::body::Pat::Bind(local) = self.body.pat(arm.pat) {
                let error_ty = self.types.local(*local).clone();
                let error = self.be.word_to_value(word, &error_ty);
                let released = self.block("catch.bound.done");
                self.scopes.push(vec![Cleanup::Temp(error, error_ty.clone())]);
                let on = Scrutinee {
                    value: error,
                    ty: error_ty,
                    released_by_arms: false,
                };
                let mine = [arm.clone()];
                let reached_here = self.emit_arms(&mine, &on, slot, released, range);
                let scope = self.scopes.pop().unwrap_or_default();

                self.at(released);
                if reached_here == 0 {
                    self.be.builder.build_unreachable().expect("sealing a diverging handler");
                    return self.join(merge, reached, slot, &result_ty);
                }
                for cleanup in scope.into_iter().rev() {
                    self.release(cleanup);
                }
                self.br(merge);
                reached += 1;
                return self.join(merge, reached, slot, &result_ty);
            }

            // Released first, by id, because after this there is no handle on
            // it: the arm binds nothing — a `_` has no name to bind under —
            // so this is the only place the error can be let go of.
            let releaser = self.be.release_error();
            self.be
                .builder
                .build_call(releaser, &[which.into(), word.into()], "")
                .expect("releasing a caught error of unknown type");
            if let Some(guard) = arm.guard {
                // A guard on a `_` would need somewhere to send a false, and
                // the error has been released by now. The checker does not
                // offer one; this is here so a future one is not silent.
                let _ = guard;
            }
            if let Some(value) = self.expr(arm.body) {
                self.store_result(slot, value);
                self.br(merge);
                reached += 1;
            }
        }

        self.join(merge, reached, slot, &result_ty)
    }

    /// Arrives at a join block, or seals it if nothing reaches it.
    pub(super) fn join(
        &mut self,
        merge: BasicBlock<'ctx>,
        reached: usize,
        slot: Option<PointerValue<'ctx>>,
        ty: &Type,
    ) -> Flow<'ctx> {
        self.at(merge);
        if reached == 0 {
            self.be.builder.build_unreachable().expect("sealing an unreachable join");
            return None;
        }
        Some(self.load_result(slot, ty))
    }

    /// The error type a `catch` arm names, by its constructor.
    pub(super) fn owner_of(&self, pat: khora_hir::body::PatId) -> Option<String> {
        match self.body.pat(pat) {
            khora_hir::body::Pat::Path(r)
            | khora_hir::body::Pat::TupleStruct { resolution: r, .. } => match r {
                khora_hir::Resolution::Variant { type_name, .. } => Some(type_name.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Emits the arms of a `match` or a `catch` over `value` and returns how
    /// many of them reach `merge`.
    ///
    /// Shared because a `catch` arm is a `match` arm in every respect except
    /// what it is matching on: one error type's variants rather than a
    /// scrutinee's. None is zero if every arm diverges, and the caller has to
    /// seal `merge` rather than join to it.
    pub(super) fn emit_arms(
        &mut self,
        arms: &[MatchArm],
        on: &Scrutinee<'ctx>,
        slot: Option<PointerValue<'ctx>>,
        merge: BasicBlock<'ctx>,
        range: TextRange,
    ) -> usize {
        let Scrutinee { value, ty, released_by_arms: releases_scrutinee } = on;
        let (value, releases_scrutinee) = (*value, *releases_scrutinee);
        // One pair of blocks per arm: bindings and guard first, then the body,
        // so a failing guard can jump on without the body ever being entered.
        let mut binds = Vec::with_capacity(arms.len());
        let mut bodies = Vec::with_capacity(arms.len());
        for index in 0..arms.len() {
            binds.push(self.block(&format!("arm{index}.bind")));
            bodies.push(self.block(&format!("arm{index}.body")));
        }

        self.dispatch(arms, value, ty, &binds, range);

        // The scope holding the scrutinee, which each arm body empties while it
        // runs. See `lower_match`.
        let held = self.scopes.len().saturating_sub(1);

        // **A guard on the last arm can fail**, and then no arm ran and so no
        // arm released. That edge gets a block of its own rather than going
        // straight to the join.
        let unguarded = if releases_scrutinee && arms.last().is_some_and(|a| a.guard.is_some()) {
            let current = self.here();
            let block = self.block("match.unguarded");
            self.at(block);
            self.drop(value, ty);
            self.br(merge);
            self.at(current);
            block
        } else {
            merge
        };

        let mut reached = 0;
        for (index, arm) in arms.iter().enumerate() {
            self.at(binds[index]);
            self.bind_pattern(arm.pat, value, ty);
            match arm.guard {
                Some(guard) => {
                    // A guard is checked with the bindings in scope and, if it
                    // fails, hands the value to the next arm untouched.
                    let next = binds.get(index + 1).copied().unwrap_or(unguarded);
                    match self.expr(guard) {
                        Some(test) => {
                            self.be
                                .builder
                                .build_conditional_branch(
                                    test.into_int_value(),
                                    bodies[index],
                                    next,
                                )
                                .expect("a guard branch");
                        }
                        // A guard that never returns — `if (return 0)` — leaves
                        // the body with no way in. Seal it, or the block sits
                        // there unterminated and fails verification a long way
                        // from the guard that caused it.
                        None => {
                            self.at(bodies[index]);
                            self.be
                                .builder
                                .build_unreachable()
                                .expect("sealing an unenterable arm");
                            continue;
                        }
                    }
                }
                None => self.br(bodies[index]),
            }

            self.at(bodies[index]);
            self.release_at_arm(arm.body);
            self.own_arm_bindings(arm.body);
            // Copies taken, so the scrutinee can go. From here to the end of
            // the arm it is no longer this frame's to release, which is what
            // emptying its scope level says to anything that unwinds out.
            let holding = if releases_scrutinee {
                let holding = std::mem::take(&mut self.scopes[held]);
                self.release_scrutinee(value, ty, arm.body);
                holding
            } else {
                Vec::new()
            };
            let produced = self.expr(arm.body);
            if releases_scrutinee {
                self.scopes[held] = holding;
                self.discard_unspent_token();
            }
            if let Some(value) = produced {
                self.store_result(slot, value);
                self.leave_scope();
                self.br(merge);
                reached += 1;
            } else {
                // The arm diverged, so nothing reaches the release. Whatever
                // left the frame released along its own path.
                self.scopes.pop();
            }
        }
        reached
    }

    /// Releases the scrutinee at an arm's head, keeping the cell if the arm
    /// can build its result there.
    ///
    /// An ordinary `khora_drop` unless the planner promised this arm a reuse,
    /// in which case `khora_drop_reuse` does the same release and hands back
    /// the memory when the reference it dropped was the last one. Null when it
    /// was not, and then the constructor allocates as usual — the decision is
    /// one comparison at run time rather than a proof at compile time, which
    /// is what makes it worth trying at all. `docs/design/reuse.md` §2.
    pub(super) fn release_scrutinee(&mut self, value: BasicValueEnum<'ctx>, ty: &Type, arm: ExprId) {
        let Some(site) = self.plan.reuse_site(arm).filter(|_| is_boxed(ty)) else {
            self.drop(value, ty);
            return;
        };
        let glue = self.be.drop_glue(ty);
        let drop_reuse = self.be.rt.drop_reuse;
        let token = self
            .be
            .builder
            .build_call(drop_reuse, &[value.into(), glue.into()], "reuse.token")
            .expect("releasing a scrutinee for reuse")
            .try_as_basic_value()
            .basic()
            .expect("khora_drop_reuse returns a pointer")
            .into_pointer_value();
        self.reuse = Some((site, token));
    }

    /// Frees a token the arm did not spend.
    ///
    /// Unreachable by the planner's rule — the arm's body *is* the constructor
    /// — and emitted anyway, because the failure mode of being wrong about
    /// that is memory nothing owns and no counter is watching.
    pub(super) fn discard_unspent_token(&mut self) {
        let Some((_, token)) = self.reuse.take() else { return };
        let free_reuse = self.be.rt.free_reuse;
        self.be
            .builder
            .build_call(free_reuse, &[token.into()], "")
            .expect("discarding an unspent reuse token");
    }

    /// Gives a `match` arm ownership of what its pattern bound.
    ///
    /// `bind_pattern` stores the loaded field straight into the slot, so until
    /// this runs the binding is a borrowed view into the scrutinee's payload
    /// and is only valid while the `match` holds the scrutinee. One copy on the
    /// way in makes the arm an owner, and the scope pushed here releases what
    /// the body did not hand on.
    ///
    /// Pushes a scope in every case, so the caller can pop unconditionally.
    pub(super) fn own_arm_bindings(&mut self, body: ExprId) {
        let mut cleanups = Vec::new();
        for local in self.plan.arm_binds_for(body).to_vec() {
            let ty = self.types.local(local).clone();
            let Some(slot) = self.slots.get(&local).copied() else { continue };
            let Some(llvm_ty) = self.be.llvm_type(&ty) else { continue };
            let value = self
                .be
                .builder
                .build_load(llvm_ty, slot, "arm.bound")
                .expect("loading an arm binding to copy");
            self.dup(value);
            // A binding the body hands on is released by whoever took it.
            if !self.plan.moved.contains(&local) {
                cleanups.push(Cleanup::Local(local));
            }
        }
        self.scopes.push(cleanups);
    }

    /// Branches to the first arm whose pattern applies.
    ///
    /// Two shapes, and the difference is worth the second code path. When every
    /// arm is an unguarded constructor pattern with irrefutable fields — the
    /// overwhelmingly common `match` — the tag goes straight into an LLVM
    /// `switch`, which is a jump table. Anything else (guards, literal
    /// patterns, nested constructors, a non-ADT scrutinee) becomes a chain of
    /// tests, tried in written order, which is the only shape that gets
    /// fallthrough right.
    pub(super) fn dispatch(
        &mut self,
        arms: &[MatchArm],
        value: BasicValueEnum<'ctx>,
        scrutinee_ty: &Type,
        binds: &[BasicBlock<'ctx>],
        range: TextRange,
    ) {
        if arms.is_empty() {
            self.fail("a `match` needs at least one arm", range);
            return;
        }

        if let Some(plan) = self.switch_plan(arms, scrutinee_ty) {
            let tag = runtime::load_tag(self.be.ctx, &self.be.builder, value.into_pointer_value());
            let mut cases = Vec::new();
            let mut default = None;
            for (index, entry) in plan.into_iter().enumerate() {
                match entry {
                    Some(tag_value) => {
                        let case = self.be.ctx.i32_type().const_int(tag_value as u64, false);
                        cases.push((case, binds[index]));
                    }
                    None => default = Some(binds[index]),
                }
            }
            let default = default.unwrap_or_else(|| self.unmatched_block());
            self.be.builder.build_switch(tag, default, &cases).expect("a tag switch");
            return;
        }

        let tests: Vec<BasicBlock<'ctx>> =
            (0..arms.len()).map(|i| self.block(&format!("arm{i}.test"))).collect();
        let unmatched = self.unmatched_block();
        self.br(tests[0]);

        for (index, arm) in arms.iter().enumerate() {
            let next = tests.get(index + 1).copied().unwrap_or(unmatched);
            self.at(tests[index]);
            self.test_pattern(arm.pat, value, scrutinee_ty, binds[index], next);
        }
    }

    /// A tag per arm, or `None` for an arm that matches anything, when the
    /// whole `match` can dispatch through one `switch`.
    pub(super) fn switch_plan(&self, arms: &[MatchArm], scrutinee_ty: &Type) -> Option<Vec<Option<u32>>> {
        if !matches!(scrutinee_ty, Type::Adt { .. }) {
            return None;
        }

        let mut plan = Vec::with_capacity(arms.len());
        let mut seen: Vec<u32> = Vec::new();
        let mut catch_all = false;

        for arm in arms {
            if arm.guard.is_some() {
                return None;
            }
            // Anything after a catch-all is unreachable, which the checker
            // rejects; refusing here too keeps this from having to model it.
            if catch_all {
                return None;
            }
            match self.body.pat(arm.pat) {
                Pat::Wildcard | Pat::Bind(_) => {
                    catch_all = true;
                    plan.push(None);
                }
                Pat::Path(resolution) => {
                    let tag = self.tag_of(resolution)?;
                    if seen.contains(&tag) {
                        return None;
                    }
                    seen.push(tag);
                    plan.push(Some(tag));
                }
                Pat::TupleStruct { resolution, fields } => {
                    // A refutable field pattern needs a test the switch has
                    // nowhere to put.
                    if !fields.iter().all(|f| self.is_irrefutable(*f)) {
                        return None;
                    }
                    let tag = self.tag_of(resolution)?;
                    if seen.contains(&tag) {
                        return None;
                    }
                    seen.push(tag);
                    plan.push(Some(tag));
                }
                _ => return None,
            }
        }
        Some(plan)
    }

    pub(super) fn tag_of(&self, resolution: &khora_hir::Resolution) -> Option<u32> {
        match resolution {
            khora_hir::Resolution::Variant { module, type_name, name } => {
                self.be.variant_in(Some(module), type_name, name).map(|(tag, _)| tag)
            }
            _ => None,
        }
    }

    /// Whether this pattern can fail to match, ignoring what it binds.
    ///
    /// A tuple has one shape, so a tuple of names tests nothing — which is what
    /// lets `test_fields` skip the field entirely rather than emitting a
    /// comparison against a value it would always accept.
    pub(super) fn is_irrefutable(&self, pat: PatId) -> bool {
        match self.body.pat(pat) {
            Pat::Wildcard | Pat::Bind(_) | Pat::Missing => true,
            Pat::Tuple(fields) => {
                fields.clone().iter().all(|field| self.is_irrefutable(*field))
            }
            _ => false,
        }
    }

    /// A block for "no arm applied".
    ///
    /// Exhaustiveness checking says this cannot happen, so `unreachable` alone
    /// would be correct — and would make a bug in that checker into undefined
    /// behavior rather than a crash. A trap first costs one instruction on a
    /// path nothing takes.
    pub(super) fn unmatched_block(&mut self) -> BasicBlock<'ctx> {
        let current = self.here();
        let block = self.block("match.unmatched");
        self.at(block);
        let trap = self.be.rt.trap;
        self.be.builder.build_call(trap, &[], "").expect("a trap");
        self.be.builder.build_unreachable().expect("sealing the trap");
        self.at(current);
        block
    }

    /// Emits the tests a pattern requires, ending the current block.
    ///
    /// Chained rather than combined into one condition, because the tests are
    /// not independent: reading field 0 of a `Cons` is only safe once the tag
    /// says it *is* a `Cons`, and a `Nil` has no field 0 to read.
    pub(super) fn test_pattern(
        &mut self,
        pat: PatId,
        value: BasicValueEnum<'ctx>,
        ty: &Type,
        success: BasicBlock<'ctx>,
        failure: BasicBlock<'ctx>,
    ) {
        let range = TextRange::empty(0.into());
        match self.body.pat(pat).clone() {
            Pat::Wildcard | Pat::Bind(_) | Pat::Missing => self.br(success),
            Pat::Literal(Literal::Int(text)) => {
                let Some(literal) = parse_int(&text) else {
                    self.fail(format!("`{text}` does not fit in an `Int`"), range);
                    return;
                };
                let expected = self.be.ctx.i64_type().const_int(literal as u64, false);
                self.branch_on_equal(value.into_int_value(), expected, success, failure);
            }
            Pat::Literal(Literal::Bool(expected)) => {
                let expected = self.be.ctx.bool_type().const_int(expected as u64, false);
                self.branch_on_equal(value.into_int_value(), expected, success, failure);
            }
            // **A literal pattern is an equality test**, which is what D14
            // decided. It parsed, it type-checked, and then it failed here —
            // accepted through two phases and refused in the third, which is
            // the one behaviour that was clearly wrong. `khora_str_eq` already
            // existed and `==` already compiled; the decision tree simply had
            // no case for it.
            Pat::Literal(Literal::Str(text)) => {
                let Some(expected) = self.string_literal(&text) else { return };
                // Borrowed on both sides. The scrutinee belongs to the `match`
                // and the literal is a static with an immortal count, so a test
                // that released either would be wrong in one direction and
                // pointless in the other.
                let equal = self.strings_equal(value, expected);
                let zero = self.be.ctx.i8_type().const_zero();
                let test = self
                    .be
                    .builder
                    .build_int_compare(IntPredicate::NE, equal, zero, "matches")
                    .expect("narrowing a C boolean");
                self.be
                    .builder
                    .build_conditional_branch(test, success, failure)
                    .expect("a string pattern test");
            }
            Pat::Literal(Literal::Float(text)) => {
                let Ok(literal) = text.parse::<f64>() else {
                    self.fail(format!("`{text}` is not a number this target can hold"), range);
                    return;
                };
                // Ordered equality, so a `NaN` scrutinee matches no literal —
                // including a `NaN` one. That is what `==` on floats already
                // does here and what IEEE 754 says; a pattern behaving
                // differently from the operator would be worse than either.
                let expected = self.be.ctx.f64_type().const_float(literal);
                let test = self
                    .be
                    .builder
                    .build_float_compare(
                        inkwell::FloatPredicate::OEQ,
                        value.into_float_value(),
                        expected,
                        "matches",
                    )
                    .expect("a float pattern test");
                self.be
                    .builder
                    .build_conditional_branch(test, success, failure)
                    .expect("a float pattern branch");
            }
            Pat::Path(resolution) => {
                let Some(tag) = self.tag_of(&resolution) else {
                    self.fail("this pattern does not name a constructor", range);
                    return;
                };
                let loaded =
                    runtime::load_tag(self.be.ctx, &self.be.builder, value.into_pointer_value());
                let expected = self.be.ctx.i32_type().const_int(tag as u64, false);
                self.branch_on_equal(loaded, expected, success, failure);
            }
            Pat::TupleStruct { resolution, fields } => {
                let Some((tag, info)) = self.variant_of(&resolution) else {
                    self.fail("this pattern does not name a constructor", range);
                    return;
                };
                let info = self.at_this_instantiation(ty, info);
                let object = value.into_pointer_value();
                let loaded = runtime::load_tag(self.be.ctx, &self.be.builder, object);
                let expected = self.be.ctx.i32_type().const_int(tag as u64, false);
                let matched = self.block("case");
                self.branch_on_equal(loaded, expected, matched, failure);

                self.at(matched);
                self.test_fields(object, &info, &fields, 0, success, failure);
            }
            // **No tag to test.** A tuple has one shape, so matching one is
            // only its elements — and whether *they* match is `test_fields`,
            // the same walk a constructor's payload gets. The layout comes from
            // the type rather than from a declaration, because a tuple has no
            // declaration to look one up in.
            Pat::Tuple(fields) => {
                let Some(info) = self.be.instantiated_variants(ty).into_iter().next() else {
                    self.fail(format!("`{ty}` is not a tuple"), range);
                    return;
                };
                let object = value.into_pointer_value();
                self.test_fields(object, &info, &fields, 0, success, failure);
            }
        }
    }

    /// A variant's fields with *this* use's type arguments substituted in.
    ///
    /// `variant_of` answers from the declaration, where `Cons(head: A, ..)`
    /// carries an `A`. A pattern that descends into that field needs to know
    /// what `A` is here — `List<(Int, String)>` makes it a tuple, and a
    /// parameter is not a shape anything can be loaded at.
    ///
    /// `bind_pattern` used to work around this by preferring the bound local's
    /// recorded type, which is right for a leaf and has nothing to say about a
    /// nested pattern. Both callers know the type of the value they are
    /// matching now, so the substitution can just be done.
    pub(super) fn at_this_instantiation(&self, ty: &Type, declared: VariantInfo) -> VariantInfo {
        self.be
            .instantiated_variants(ty)
            .into_iter()
            .find(|v| v.name == declared.name)
            .unwrap_or(declared)
    }

    pub(super) fn test_fields(
        &mut self,
        object: PointerValue<'ctx>,
        info: &VariantInfo,
        fields: &[PatId],
        index: usize,
        success: BasicBlock<'ctx>,
        failure: BasicBlock<'ctx>,
    ) {
        if index >= fields.len() {
            self.br(success);
            return;
        }
        if self.is_irrefutable(fields[index]) {
            self.test_fields(object, info, fields, index + 1, success, failure);
            return;
        }

        let field_ty = info.fields.get(index).cloned().unwrap_or(Type::Unknown);
        let value = self.load_field(object, index, &field_ty);
        let next = self.block("field.next");
        self.test_pattern(fields[index], value, &field_ty, next, failure);
        self.at(next);
        self.test_fields(object, info, fields, index + 1, success, failure);
    }

    pub(super) fn branch_on_equal(
        &mut self,
        value: inkwell::values::IntValue<'ctx>,
        expected: inkwell::values::IntValue<'ctx>,
        success: BasicBlock<'ctx>,
        failure: BasicBlock<'ctx>,
    ) {
        let test = self
            .be
            .builder
            .build_int_compare(IntPredicate::EQ, value, expected, "matches")
            .expect("a pattern test");
        self.be
            .builder
            .build_conditional_branch(test, success, failure)
            .expect("a pattern branch");
    }

    /// Writes a pattern's bindings into their slots.
    ///
    /// No `dup` anywhere: a binding borrows out of the scrutinee, which the
    /// `match` owns for the duration of the arm. A read of the binding is what
    /// duplicates, and the plan records that read.
    pub(super) fn bind_pattern(&mut self, pat: PatId, value: BasicValueEnum<'ctx>, ty: &Type) {
        match self.body.pat(pat).clone() {
            Pat::Bind(local) => {
                if let Some(slot) = self.slots.get(&local).copied() {
                    self.be.builder.build_store(slot, value).expect("binding a pattern");
                }
            }
            Pat::TupleStruct { resolution, fields } => {
                let Some((_, info)) = self.variant_of(&resolution) else { return };
                let info = self.at_this_instantiation(ty, info);
                let object = value.into_pointer_value();
                for (index, field) in fields.iter().enumerate() {
                    // **The binding's own type, not the variant's declared
                    // one.** `Option::Some(value: A)` declares `A`, and at
                    // `Option<Bool>` the declared type is still `A` — which has
                    // no machine type, so the field came back as an `i64`.
                    //
                    // That was right by accident for everything word-sized and
                    // a *stack overflow* for anything narrower: `v` in
                    // `Option::Some(v)` at `Bool` is a one-byte slot, and
                    // storing eight bytes into it wrote over whatever the
                    // frame put next — which was the scrutinee, so the object
                    // was never released. Errata 44.
                    //
                    // The checker recorded the specialized type of every bound
                    // local, so the leaf knows exactly what it is. A nested
                    // pattern keeps the declared type, which is a pointer for
                    // anything a pattern can descend into.
                    let field_ty = match self.body.pat(*field) {
                        Pat::Bind(local) => self.types.local(*local).clone(),
                        _ => info.fields.get(index).cloned().unwrap_or(Type::Unknown),
                    };
                    let loaded = self.load_field(object, index, &field_ty);
                    self.bind_pattern(*field, loaded, &field_ty);
                }
            }
            // The elements come from the type, positionally. A tuple has no
            // declaration, so there is nothing else they could come from — and
            // nothing else they need to, since a tuple's shape is its type.
            Pat::Tuple(fields) => {
                let Some(info) = self.be.instantiated_variants(ty).into_iter().next() else {
                    return;
                };
                let object = value.into_pointer_value();
                for (index, field) in fields.iter().enumerate() {
                    let field_ty = info.fields.get(index).cloned().unwrap_or(Type::Unknown);
                    let loaded = self.load_field(object, index, &field_ty);
                    self.bind_pattern(*field, loaded, &field_ty);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => {}
        }
    }

    pub(super) fn variant_of(&self, resolution: &khora_hir::Resolution) -> Option<(u32, VariantInfo)> {
        match resolution {
            khora_hir::Resolution::Variant { module, type_name, name } => {
                self.be.variant_in(Some(module), type_name, name)
            }
            _ => None,
        }
    }
}
