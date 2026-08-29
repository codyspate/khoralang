//! The expression dispatch, and the leaves.
//!
//! One arm per `Expr`, most of them a line handing off to a neighbouring
//! module. Literals and local reads are here because they have nowhere else to
//! be — and a local read is where a take clears its slot, which is the one
//! piece of reference counting that could not live in `rc`.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    pub(super) fn expr(&mut self, id: ExprId) -> Flow<'ctx> {
        if self.aborted {
            return None;
        }
        let range = self.body.range(id);
        // **The one place a source position is attached to code.** Every
        // expression passes through here, so setting the debug location once
        // at the top covers whatever the arm below emits — including the arms
        // that emit nothing themselves and hand off to a neighbouring module.
        self.be.at(range);
        match self.body.expr(id).clone() {
            Expr::Unit => Some(self.be.unit_value()),
            Expr::Literal(lit) => {
                let ty = self.types.of(id).clone();
                self.literal(lit, &ty, range)
            }
            Expr::Local(local) => self.read_local(id, local, range),
            Expr::Path(resolution) => self.path(id, &resolution, range),
            Expr::Call { callee, args } => self.call(id, callee, &args, range),
            Expr::Binary { op, lhs, rhs } => self.binary(id, op, lhs, rhs, range),
            Expr::Unary { op, operand } => self.unary(op, operand, range),
            Expr::Assign { target, value } => self.assign(target, value, range),
            Expr::Block { stmts, tail } => self.lower_block(id, &stmts, tail),
            Expr::If { condition, then_branch, else_branch } => {
                self.lower_if(id, condition, then_branch, else_branch)
            }
            Expr::Match { scrutinee, arms } => self.lower_match(id, scrutinee, &arms, range),
            Expr::Catch { inner, arms } => self.lower_catch(id, inner, &arms, range),
            Expr::While { condition, body } => self.lower_while(condition, body),
            Expr::Loop { body } => self.lower_loop(id, body),
            Expr::Break(value) => self.lower_break(value, range),
            Expr::Continue => self.lower_continue(range),
            Expr::Return(value) => self.lower_return(value),
            Expr::Record { fields, .. } => self.build_record(id, &fields, range),
            Expr::Raise(error) => self.lower_raise(error, range),
            // `!` is the identity on values. The branch it stands for is
            // emitted by the call underneath, which knows it is marked.
            // `!` is a cancellation point as well as an error branch. The
            // check comes *before* the call, so a cancelled computation stops
            // rather than doing work it is about to throw away — and before
            // the arguments are evaluated, so there is nothing half-built to
            // leak on the way out.
            Expr::Try(inner) => {
                self.check_cancellation(range);
                self.expr(inner)
            }
            Expr::Lambda { .. } => self.make_closure(id, range),
            // Parameter 0 of a lifted lambda *is* the closure, and it is live
            // for the duration of the call because the caller holds it. No
            // capture, no reference count, no cycle.
            Expr::LambdaSelf => match self.function.get_nth_param(0) {
                Some(closure) => Some(closure),
                None => self.fail(
                    "a closure referred to itself outside a closure, which is a compiler bug",
                    range,
                ),
            },
            Expr::Field { base, name } => self.read_field(base, &name, range),
            Expr::Tuple(items) => self.build_tuple(id, &items, range),
            // The checker already rejected these, so reaching one means
            // `compile` ran with diagnostics it should have refused.
            Expr::Missing | Expr::Unresolved(_) => {
                self.fail("this expression did not survive the front end", range)
            }
        }
    }

    /// A literal, at the width the checker gave it.
    ///
    /// `ty` matters because an integer literal is not always an `Int`: the
    /// checker types it from context, so the `56` in `U8::wrapping_add(b, 56)`
    /// is a `U8` and has to be an `i8` here. Emitting `i64` regardless is a
    /// mismatch LLVM catches and the checker never would.
    pub(super) fn literal(&mut self, lit: Literal, ty: &Type, range: TextRange) -> Flow<'ctx> {
        match lit {
            Literal::Int(text) => match parse_int(&text) {
                Some(value) => {
                    let bits = Self::int_shape(ty).map_or(64, |(bits, _)| bits);
                    // `sign_extend` is false because `value` is already the
                    // exact bit pattern; LLVM would otherwise re-extend a
                    // negative literal that has none of its bits to spare.
                    Some(self.be.int_width(bits).const_int(value as u64, false).into())
                }
                None => self.fail(format!("`{text}` does not fit in an `Int`"), range),
            },
            Literal::Bool(value) => {
                Some(self.be.ctx.bool_type().const_int(value as u64, false).into())
            }
            Literal::Str(text) => self.string_literal(&text),
            Literal::Float(text) => {
                let Ok(value) = text.parse::<f64>() else {
                    return self.fail(format!("`{text}` is not a number this target can hold"), range);
                };
                Some(self.be.ctx.f64_type().const_float(value).into())
            }
        }
    }

    /// The `String` for a literal, which is built once for the whole program.
    ///
    /// Layout, since the runtime does not impose one: the ordinary object
    /// header, then field 0 is the byte length and the bytes follow it
    /// immediately.
    ///
    /// **A static, not an allocation.** This used to call `khora_alloc` and
    /// memcpy the bytes every time the literal was *evaluated* — so
    /// `Response::rendered` allocated a dozen small strings per response for
    /// its `"Content-Type: "` and friends, `fn alphabet() -> String { ".." }`
    /// allocated on every call, and a literal inside a loop allocated on every
    /// turn. A string is immutable, so one object can serve every mention.
    ///
    /// The reference count starts enormous rather than at one. Nothing has to
    /// know a static from a heap object that way: `khora_dup` and `khora_drop`
    /// treat it like anything else, and the count cannot reach zero inside the
    /// lifetime of any real program, so it is never handed to a free that would
    /// not understand it. The alternative — a check on the hot path of every
    /// drop — costs every object to protect these.
    pub(super) fn string_literal(&mut self, text: &str) -> Flow<'ctx> {
        Some(self.be.static_string(text).into())
    }

    pub(super) fn read_local(&mut self, id: ExprId, local: LocalId, range: TextRange) -> Flow<'ctx> {
        let ty = self.types.local(local).clone();
        let Some(slot) = self.slots.get(&local).copied() else {
            let name = self.body.local(local).name.clone();
            return self.fail(format!("`{name}` has no storage, which is a compiler bug"), range);
        };
        let Some(llvm_ty) = self.be.llvm_type(&ty) else {
            let name = self.body.local(local).name.clone();
            return self.fail(format!("`{name}` has a type the backend cannot represent"), range);
        };

        let value =
            self.be.builder.build_load(llvm_ty, slot, "load").expect("reading a local").to_owned();

        // The plan decides this, not the type: the value outlives the read, so
        // it needs its own reference.
        if self.plan.needs_dup(id) {
            self.dup(value);
            return Some(value);
        }

        // No copy. The plan says whether that is because this read *takes* the
        // binding — its last use, handed the reference the binding was holding
        // — or for one of the other reasons a read needs no copy of its own.
        //
        // **A take in a body that can unwind clears the slot.** The block still
        // lists the binding among what it releases, because a `raise` passing
        // through before this point has to release it; clearing the slot is
        // what stops the block releasing it again after. It makes "has this
        // been handed on" a question the slot answers at run time rather than
        // one the lowering position would have to answer at compile time — and
        // the lowering position cannot, because two paths reach it in different
        // states. `docs/design/reuse.md` §1.
        //
        // Where nothing can unwind the answer is static, the block never lists
        // the binding at all, and this store would be dead. That is the common
        // case and it keeps costing nothing.
        if self.plan.unwinds && self.plan.takes.contains(&id) {
            let zero = self.be.zero_value(&ty);
            self.be.builder.build_store(slot, zero).expect("clearing a taken slot");
        }
        Some(value)
    }

    pub(super) fn path(
        &mut self,
        id: ExprId,
        resolution: &khora_hir::Resolution,
        range: TextRange,
    ) -> Flow<'ctx> {
        match resolution {
            // A constructor with no payload is still an allocation: it has a
            // tag, and a tag lives in a header. Interning the nullary cases
            // would need the refcount to be saturating, which is a phase 6
            // conversation.
            khora_hir::Resolution::Variant { module, type_name, name } => {
                let (home, owner, case) = (module.clone(), type_name.clone(), name.clone());
                self.construct(id, Some(&home), &owner, &case, &[], range)
            }
            // A named function used as a value becomes a closure that
            // captures nothing and forwards to it.
            khora_hir::Resolution::Item { name, .. }
                if matches!(self.types.of(id), Type::Fn { .. }) =>
            {
                let symbol =
                    self.mono.callee(&self.owner.clone(), id).unwrap_or_else(|| name.clone());
                self.function_value(&symbol, range)
            }
            khora_hir::Resolution::Item { name, .. } => self.fail(
                format!("`{name}` is not a value; only functions and constructors have one"),
                range,
            ),
            // `Applicative::pure(x)` in value position: the same wrapper a
            // named function gets, around whichever impl was selected.
            khora_hir::Resolution::TraitItem { .. } => match self.mono.callee(&self.owner.clone(), id) {
                Some(symbol) => self.function_value(&symbol, range),
                None => self.fail(
                    "this trait function was not resolved to an impl; that is a compiler bug",
                    range,
                ),
            },
            khora_hir::Resolution::Unsupported(what) => self.fail(what.to_string(), range),
        }
    }
}
