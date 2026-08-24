//! Where `dup` and `drop` are emitted, and the scopes that decide it.
//!
//! `khora-perceus` decides *what* — which reads copy, which take, what a block
//! releases, which arm releases early. This emits it, and owns the one thing
//! the plan cannot express: a stack of cleanups that a `return`, a `break` or a
//! raise unwinds through. `docs/design/reuse.md`.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// Adds a reference, for a value that must outlive the expression that
    /// produced it.
    ///
    /// **Emitted inline rather than called.** The refcount is the first word of
    /// the header, so this is a null test and one atomic add — against a call
    /// into `khora_dup`, which does exactly that and nothing else. An HTTP
    /// request parse performs 280 reference-count operations against 50
    /// allocations, so the call was a large fraction of what counting cost.
    /// `docs/design/reuse.md` §3.
    ///
    /// The runtime's `khora_dup` stays, because `khora-rt` is a C ABI anything
    /// may link against, and it is still what a hand-written extern uses.
    pub(super) fn dup(&mut self, value: BasicValueEnum<'ctx>) {
        let object = value.into_pointer_value();
        let bump = self.block("dup.bump");
        let done = self.block("dup.done");
        let null = self.be.ctx.ptr_type(AddressSpace::default()).const_null();
        let is_null = self
            .be
            .builder
            .build_int_compare(IntPredicate::EQ, object, null, "dup.null")
            .expect("testing a pointer for null");
        self.be
            .builder
            .build_conditional_branch(is_null, done, bump)
            .expect("a dup's null guard");

        self.at(bump);
        self.adjust_count(object, 1);
        self.br(done);
        self.at(done);
    }

    /// Releases a reference. A no-op for a machine word.
    ///
    /// The decrement is inline like [`Self::dup`]'s add, and only the *last*
    /// reference pays for a call — to `khora_drop_last`, which holds the fence,
    /// the field-dropping callback and the deallocation. The common case is a
    /// decrement and a branch that is not taken.
    pub(super) fn drop(&mut self, value: BasicValueEnum<'ctx>, ty: &Type) {
        if !is_boxed(ty) {
            return;
        }
        let glue = self.be.drop_glue(ty);
        let object = value.into_pointer_value();
        let release = self.block("drop.release");
        let last = self.block("drop.last");
        let done = self.block("drop.done");

        let null = self.be.ctx.ptr_type(AddressSpace::default()).const_null();
        let is_null = self
            .be
            .builder
            .build_int_compare(IntPredicate::EQ, object, null, "drop.null")
            .expect("testing a pointer for null");
        self.be
            .builder
            .build_conditional_branch(is_null, done, release)
            .expect("a drop's null guard");

        self.at(release);
        let previous = self.adjust_count(object, -1);
        // Zero goes to the slow path too, which is where the already-zero abort
        // lives: a second branch here would be paid by every drop in the
        // program to catch something that must never happen.
        let survives = self
            .be
            .builder
            .build_int_compare(
                IntPredicate::UGT,
                previous,
                self.be.ctx.i64_type().const_int(1, false),
                "drop.survives",
            )
            .expect("comparing a refcount");
        self.be
            .builder
            .build_conditional_branch(survives, done, last)
            .expect("a drop's last-reference branch");

        self.at(last);
        let drop_last = self.be.rt.drop_last;
        self.be
            .builder
            .build_call(drop_last, &[object.into(), glue.into(), previous.into()], "")
            .expect("releasing the last reference");
        self.br(done);
        self.at(done);
    }

    /// Adds `by` to an object's refcount, returning what was there before.
    ///
    /// **Atomic only where two threads are possible.** A program that never
    /// mentions `Fiber::spawn` has one thread for its whole life, and its
    /// reference counts are then ordinary arithmetic: a load, an add and a
    /// store, with no lock prefix and nothing for the processor to order. Worth
    /// 7% of an HTTP request parse and 10% of a browser's — D10's escape
    /// analysis in the case where there is nothing to escape to.
    /// `docs/design/reuse.md` §4.
    ///
    /// Where threads are possible the orderings are the ones `khora_dup` and
    /// `khora_drop` argued for: relaxed to add, because the caller already owns
    /// a reference and nothing is being published; release to subtract, pairing
    /// with the acquire fence inside `khora_drop_last`.
    pub(super) fn adjust_count(&mut self, object: PointerValue<'ctx>, by: i64) -> IntValue<'ctx> {
        let i64t = self.be.ctx.i64_type();
        let one = i64t.const_int(1, false);
        if self.be.single_threaded {
            let previous = self
                .be
                .builder
                .build_load(i64t, object, "rc")
                .expect("loading a refcount")
                .into_int_value();
            let next = if by > 0 {
                self.be.builder.build_int_add(previous, one, "rc.up")
            } else {
                self.be.builder.build_int_sub(previous, one, "rc.down")
            }
            .expect("adjusting a refcount");
            self.be.builder.build_store(object, next).expect("storing a refcount");
            return previous;
        }
        let (op, ordering) = if by > 0 {
            (AtomicRMWBinOp::Add, AtomicOrdering::Monotonic)
        } else {
            (AtomicRMWBinOp::Sub, AtomicOrdering::Release)
        };
        self.be
            .builder
            .build_atomicrmw(op, object, one, ordering)
            .expect("adjusting a refcount")
    }

    /// Releases everything owned by scopes at or above `depth`, innermost
    /// first, without popping them.
    ///
    /// Not popping is the point: this runs at a `return` or a `break`, which
    /// leaves the scopes on one path while the lowering of the enclosing
    /// expression carries on building the others.
    pub(super) fn unwind_to(&mut self, depth: usize) {
        for level in (depth..self.scopes.len()).rev() {
            for cleanup in self.scopes[level].clone().into_iter().rev() {
                self.release(cleanup);
            }
        }
    }

    /// Releases and pops the innermost scope, on the path that reaches its end.
    pub(super) fn leave_scope(&mut self) {
        let scope = self.scopes.pop().unwrap_or_default();
        for cleanup in scope.into_iter().rev() {
            self.release(cleanup);
        }
    }

    /// Releases what this branch arm owes on entry.
    ///
    /// Where one arm of a branch takes a binding's reference, every other arm
    /// has to release it, or the count never reaches zero on those paths. The
    /// planner grants an arm release only to an arm that does not mention the
    /// binding at all, so the head of the arm is a safe place for it — see
    /// `RcPlan::arm_drops`.
    pub(super) fn release_at_arm(&mut self, arm: ExprId) {
        for local in self.plan.arm_drops_for(arm).to_vec() {
            self.release(Cleanup::Local(local));
        }
    }

    pub(super) fn release(&mut self, cleanup: Cleanup<'ctx>) {
        match cleanup {
            Cleanup::Local(local) => {
                let ty = self.types.local(local).clone();
                let Some(slot) = self.slots.get(&local).copied() else { return };
                let Some(llvm_ty) = self.be.llvm_type(&ty) else { return };
                let value = self
                    .be
                    .builder
                    .build_load(llvm_ty, slot, "released")
                    .expect("loading a local to release");
                self.drop(value, &ty);

                // Null the slot afterwards. A scope inside a loop is left once
                // per iteration, and `break` on a later iteration leaves it
                // again before the binding has been reached — which would drop
                // the previous iteration's freed pointer a second time. The
                // runtime's null tolerance turns that into a no-op, but only if
                // the slot is actually null.
                let zero = self.be.zero_value(&ty);
                self.be.builder.build_store(slot, zero).expect("clearing a released slot");
            }
            Cleanup::Temp(value, ty) => self.drop(value, &ty),
        }
    }
}
