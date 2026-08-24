//! Statements, blocks, and everything that changes where control goes.
//!
//! Assignment, `let` — including destructuring a pattern that cannot fail —
//! `if`, `while`, `loop`, `break`, `continue` and `return`. Each of them can
//! leave a scope, which is why each of them talks to the cleanup stack in
//! `rc`.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// Assignment to a `let mut` binding.
    ///
    /// The target is deliberately **not** lowered as an expression. The plan
    /// records a `dup` for it — its walk sees a local read on the left of an
    /// `=` and cannot tell it apart from a use — and honouring that would take
    /// a reference nobody ever releases. What the assignment owes instead is
    /// the *old* value's release, which the plan has no place to record.
    pub(super) fn assign(&mut self, target: ExprId, value: ExprId, range: TextRange) -> Flow<'ctx> {
        if let Expr::Field { base, name } = self.body.expr(target).clone() {
            return self.assign_field(base, &name, value, range);
        }
        let Expr::Local(local) = self.body.expr(target).clone() else {
            return self.fail("this expression cannot be assigned to", range);
        };
        let ty = self.types.local(local).clone();
        let Some(slot) = self.slots.get(&local).copied() else {
            return self.fail("this binding has no storage, which is a compiler bug", range);
        };

        let new = self.expr(value)?;

        if is_boxed(&ty) {
            let llvm_ty = self.be.llvm_type(&ty).expect("a boxed type is a pointer");
            let old = self
                .be
                .builder
                .build_load(llvm_ty, slot, "overwritten")
                .expect("reading the overwritten value");
            self.be.builder.build_store(slot, new).expect("assigning");
            // After the store, so that `s = s` — where the read already
            // duplicated the reference — cannot free what it just stored.
            self.drop(old, &ty);
        } else {
            self.be.builder.build_store(slot, new).expect("assigning");
        }
        Some(self.be.unit_value())
    }

    pub(super) fn lower_block(&mut self, id: ExprId, stmts: &[Stmt], tail: Option<ExprId>) -> Flow<'ctx> {
        let cleanups: Vec<Cleanup<'ctx>> =
            self.plan.drops_for(id).iter().map(|l| Cleanup::Local(*l)).collect();
        self.scopes.push(cleanups);

        for stmt in stmts {
            let reached = match stmt {
                Stmt::Let { pat, init, .. } => self.lower_let(*pat, *init),
                Stmt::Expr(e) => {
                    let ty = self.types.of(*e).clone();
                    match self.expr(*e) {
                        // A statement's value is discarded, and a discarded
                        // boxed value is a leak the plan does not cover: it
                        // records releases for *bindings*, and this was never
                        // bound to anything.
                        Some(value) => {
                            self.drop(value, &ty);
                            true
                        }
                        None => false,
                    }
                }
            };
            if !reached {
                // Control left through a `return` or a `break`, which released
                // this scope on the way past. Nothing more to emit here.
                self.scopes.pop();
                return None;
            }
        }

        let value = match tail {
            Some(tail) => self.expr(tail),
            None => Some(self.be.unit_value()),
        };
        match value {
            Some(value) => {
                // Releases come after the tail is evaluated: a tail that reads
                // one of these locals has already duplicated it.
                self.leave_scope();
                Some(value)
            }
            None => {
                self.scopes.pop();
                None
            }
        }
    }

    /// Returns whether control continues past the statement.
    pub(super) fn lower_let(&mut self, pat: PatId, init: Option<ExprId>) -> bool {
        let Some(init) = init else {
            // `let x;` leaves the zeroed slot alone. Reading it before an
            // assignment is a front-end question, not a backend one.
            return true;
        };
        let ty = self.types.of(init).clone();
        let range = self.body.range(init);
        let Some(value) = self.expr(init) else { return false };

        match self.body.pat(pat).clone() {
            Pat::Bind(local) => match self.slots.get(&local).copied() {
                Some(slot) => {
                    self.be.builder.build_store(slot, value).expect("binding a let");
                    true
                }
                None => {
                    self.fail("this binding has no storage, which is a compiler bug", range);
                    false
                }
            },
            // `let _ = f()` still owns what `f` returned.
            Pat::Wildcard | Pat::Missing => {
                self.drop(value, &ty);
                true
            }
            // **Only a pattern that cannot fail.** A `let` has nowhere to
            // send a value that does not match, so `let (a, b) = pair` is
            // allowed and `let Option::Some(x) = o` is not — the second needs a
            // `match`, and saying so is more useful than refusing both.
            other if self.destructures_irrefutably(pat, &ty) => {
                let _ = other;
                self.bind_pattern(pat, value, &ty);
                // The bindings are projections into the object, exactly as a
                // `match` arm's are: `bind_pattern` stores the loaded field and
                // nothing has taken a reference for it. One copy each makes
                // them the owning locals the plan already believes they are —
                // it put every one of them in this block's release list.
                self.own_projected(pat);
                // And the container itself was owned by this expression.
                self.drop(value, &ty);
                true
            }
            _ => {
                self.fail(
                    "this pattern can fail, so it needs a `match` rather than a `let` — a \
                     `let` has nowhere to send a value that does not match",
                    range,
                );
                false
            }
        }
    }

    /// Whether a `let` can take this pattern apart without a way to fail.
    ///
    /// A tuple always matches, so its elements decide. A constructor matches
    /// only when its type has no other case — which is what a record is, and
    /// what makes `let Wrapper(x) = w` safe and `let Option::Some(x) = o` not.
    pub(super) fn destructures_irrefutably(&self, pat: PatId, ty: &Type) -> bool {
        match self.body.pat(pat) {
            Pat::Bind(_) | Pat::Wildcard | Pat::Missing => true,
            Pat::Tuple(fields) => {
                let Type::Tuple(items) = ty else { return false };
                fields.len() == items.len()
                    && fields
                        .clone()
                        .iter()
                        .zip(items)
                        .all(|(p, t)| self.destructures_irrefutably(*p, t))
            }
            Pat::TupleStruct { fields, .. } => {
                let variants = self.be.instantiated_variants(ty);
                if variants.len() != 1 {
                    return false;
                }
                let only = &variants[0];
                fields.len() == only.fields.len()
                    && fields
                        .clone()
                        .iter()
                        .zip(&only.fields)
                        .all(|(p, t)| self.destructures_irrefutably(*p, t))
            }
            Pat::Literal(_) | Pat::Path(_) => false,
        }
    }

    /// Gives the bindings a `let` pattern introduced ownership of what they
    /// point at, the way [`Self::own_arm_bindings`] does for a `match` arm.
    pub(super) fn own_projected(&mut self, pat: PatId) {
        let mut locals = Vec::new();
        self.bound_locals(pat, &mut locals);
        for local in locals {
            let ty = self.types.local(local).clone();
            if !is_boxed(&ty) {
                continue;
            }
            let Some(slot) = self.slots.get(&local).copied() else { continue };
            let Some(llvm_ty) = self.be.llvm_type(&ty) else { continue };
            let value = self
                .be
                .builder
                .build_load(llvm_ty, slot, "bound")
                .expect("loading a destructured binding");
            self.dup(value);
        }
    }

    pub(super) fn bound_locals(&self, pat: PatId, found: &mut Vec<LocalId>) {
        match self.body.pat(pat) {
            Pat::Bind(local) => found.push(*local),
            Pat::Tuple(fields) | Pat::TupleStruct { fields, .. } => {
                for field in fields.clone() {
                    self.bound_locals(field, found);
                }
            }
            _ => {}
        }
    }

    pub(super) fn lower_if(
        &mut self,
        id: ExprId,
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    ) -> Flow<'ctx> {
        let condition = self.expr(condition)?.into_int_value();

        let then_block = self.block("if.then");
        let else_block = self.block("if.else");
        let merge = self.block("if.end");
        self.be
            .builder
            .build_conditional_branch(condition, then_block, else_block)
            .expect("an if branch");

        let result_ty = self.types.of(id).clone();
        let slot = self.result_slot(&result_ty);
        let mut reached = 0;

        self.at(then_block);
        self.release_at_arm(then_branch);
        if let Some(value) = self.expr(then_branch) {
            self.store_result(slot, value);
            self.br(merge);
            reached += 1;
        }

        self.at(else_block);
        let value = match else_branch {
            Some(else_branch) => {
                self.release_at_arm(else_branch);
                self.expr(else_branch)
            }
            // An `if` without `else` is `()` on the missing side, which the
            // checker has already required of the other side too.
            None => Some(self.be.unit_value()),
        };
        if let Some(value) = value {
            self.store_result(slot, value);
            self.br(merge);
            reached += 1;
        }

        self.at(merge);
        if reached == 0 {
            self.be.builder.build_unreachable().expect("sealing an unreachable join");
            return None;
        }
        Some(self.load_result(slot, &result_ty))
    }

    pub(super) fn lower_while(&mut self, condition: ExprId, body: ExprId) -> Flow<'ctx> {
        let head = self.block("while.head");
        let body_block = self.block("while.body");
        let exit = self.block("while.end");
        self.br(head);

        self.at(head);
        let Some(test) = self.expr(condition) else {
            // A condition that never returns makes the loop and everything
            // after it dead. Seal the blocks so the IR stays well formed.
            self.at(body_block);
            self.be.builder.build_unreachable().expect("sealing a dead loop body");
            self.at(exit);
            self.be.builder.build_unreachable().expect("sealing a dead loop exit");
            return None;
        };
        self.be
            .builder
            .build_conditional_branch(test.into_int_value(), body_block, exit)
            .expect("a loop test");

        self.loops.push(LoopFrame {
            continue_to: head,
            break_to: exit,
            scope_depth: self.scopes.len(),
            breaks: 0,
        });
        self.at(body_block);
        let body_ty = self.types.of(body).clone();
        if let Some(value) = self.expr(body) {
            self.drop(value, &body_ty);
            // The back-edge. After the body's drops, so a fiber that yields
            // here is not holding a reference it was about to release.
            self.safepoint();
            self.br(head);
        }
        self.loops.pop();

        self.at(exit);
        Some(self.be.unit_value())
    }

    pub(super) fn lower_loop(&mut self, body: ExprId) -> Flow<'ctx> {
        let body_block = self.block("loop.body");
        let exit = self.block("loop.end");
        self.br(body_block);

        self.loops.push(LoopFrame {
            continue_to: body_block,
            break_to: exit,
            scope_depth: self.scopes.len(),
            breaks: 0,
        });
        self.at(body_block);
        let body_ty = self.types.of(body).clone();
        if let Some(value) = self.expr(body) {
            self.drop(value, &body_ty);
            self.safepoint();
            self.br(body_block);
        }
        let frame = self.loops.pop().expect("the frame just pushed");

        self.at(exit);
        if frame.breaks == 0 {
            // Nothing branches out, so the loop never finishes and the code
            // after it cannot run.
            self.be.builder.build_unreachable().expect("sealing an endless loop");
            return None;
        }
        Some(self.be.unit_value())
    }

    pub(super) fn lower_break(&mut self, value: Option<ExprId>, range: TextRange) -> Flow<'ctx> {
        if value.is_some() {
            return self.fail(
                "`break` with a value is not supported yet: a `loop`'s type is not inferred in \
                 phase 2, so there is nothing for the value to flow into",
                range,
            );
        }
        let Some(frame) = self.loops.last() else {
            return self.fail("`break` outside a loop", range);
        };
        let (target, depth) = (frame.break_to, frame.scope_depth);
        self.unwind_to(depth);
        self.loops.last_mut().expect("checked above").breaks += 1;
        self.br(target);
        None
    }

    pub(super) fn lower_continue(&mut self, range: TextRange) -> Flow<'ctx> {
        let Some(frame) = self.loops.last() else {
            return self.fail("`continue` outside a loop", range);
        };
        let (target, depth) = (frame.continue_to, frame.scope_depth);
        self.unwind_to(depth);
        self.br(target);
        None
    }

    /// An early `return` from a fallible function is the ok case: it carries a
    /// value, not an error, and still has to wear the tag.
    pub(super) fn return_value(&mut self, value: BasicValueEnum<'ctx>) {
        if self.raises {
            self.return_ok(value);
            return;
        }
        match self.ret {
            Type::Unit => {
                self.be.builder.build_return(None).expect("returning unit");
            }
            _ => {
                self.be.builder.build_return(Some(&value)).expect("returning a value");
            }
        }
    }

    pub(super) fn lower_return(&mut self, value: Option<ExprId>) -> Flow<'ctx> {
        let value = match value {
            Some(expr) => Some(self.expr(expr)?),
            None => None,
        };
        // Every scope, not just the innermost: a `return` leaves the whole
        // frame, and the parameters are released by the outermost one.
        self.unwind_to(0);

        let value = value.unwrap_or_else(|| self.be.unit_value());
        self.return_value(value);
        None
    }
}
