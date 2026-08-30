//! Binary and unary operators.
//!
//! Mostly one instruction each. The two that are not: string comparison, which
//! is a runtime call over the bytes, and `&&`/`||`, which are branches rather
//! than operators because they must not evaluate the right side.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    pub(super) fn binary(
        &mut self,
        site: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        range: TextRange,
    ) -> Flow<'ctx> {
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.logical(op, lhs, rhs);
        }

        let operand_ty = self.types.of(lhs).clone();
        let left = self.expr(lhs)?;
        let right = self.expr(rhs)?;

        match op {
            BinOp::Add if matches!(operand_ty, Type::Str) => self.concat(left, right),
            // IEEE arithmetic does not overflow — it reaches infinity — so
            // there is nothing to trap on and nothing to check.
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div
                if matches!(operand_ty, Type::Float) =>
            {
                let (l, r) = (left.into_float_value(), right.into_float_value());
                let b = &self.be.builder;
                let value = match op {
                    BinOp::Add => b.build_float_add(l, r, "fadd"),
                    BinOp::Sub => b.build_float_sub(l, r, "fsub"),
                    BinOp::Mul => b.build_float_mul(l, r, "fmul"),
                    _ => b.build_float_div(l, r, "fdiv"),
                };
                Some(value.expect("floating-point arithmetic").into())
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let (l, r) = (left.into_int_value(), right.into_int_value());
                let (bits, signed) =
                    Self::int_shape(&operand_ty).expect("arithmetic on an integer");
                // A `U8` addition traps at 255, not at 2^63: the check is only
                // worth anything if it is the *type's* range being checked.
                let sign = if signed { 's' } else { 'u' };
                let (verb, what) = match op {
                    BinOp::Add => ("add", "addition"),
                    BinOp::Sub => ("sub", "subtraction"),
                    _ => ("mul", "multiplication"),
                };
                let intrinsic = format!("llvm.{sign}{verb}.with.overflow.i{bits}");
                let what = format!("{operand_ty} {what} overflowed");
                Some(self.checked_arithmetic(&intrinsic, bits, &what, l, r))
            }
            BinOp::Div | BinOp::Rem => {
                let (l, r) = (left.into_int_value(), right.into_int_value());
                let (bits, signed) =
                    Self::int_shape(&operand_ty).expect("arithmetic on an integer");
                let width = self.be.int_width(bits);

                // Both ways an integer division can go wrong are *undefined* in
                // LLVM, and what they do on hardware is a fault with no message
                // attached — a bare 0xC0000094 or a SIGFPE, which says nothing
                // about which line or which value. Checked for the same reason
                // overflow is: the program is wrong either way, and the useful
                // thing to do is say so.
                let nonzero = self
                    .be
                    .builder
                    .build_int_compare(IntPredicate::NE, r, width.const_zero(), "nonzero")
                    .expect("comparing a divisor against zero");
                self.guard(nonzero, &format!("{operand_ty} division by zero"));

                // The other one only exists for a signed type, and only for one
                // pair of values: the minimum over minus one, whose quotient is
                // one past the maximum. Unsigned division cannot overflow.
                if signed {
                    let min = width.const_int(1u64 << (bits - 1), false);
                    let minus_one = width.const_all_ones();
                    let b = &self.be.builder;
                    let is_min = b
                        .build_int_compare(IntPredicate::EQ, l, min, "is.min")
                        .expect("comparing against the minimum");
                    let is_neg_one = b
                        .build_int_compare(IntPredicate::EQ, r, minus_one, "is.neg.one")
                        .expect("comparing against minus one");
                    let both = b.build_and(is_min, is_neg_one, "overflows").expect("both");
                    let ok = b.build_not(both, "in.range").expect("negating");
                    self.guard(ok, &format!("{operand_ty} division overflowed"));
                }

                let b = &self.be.builder;
                let value = match (op, signed) {
                    (BinOp::Div, true) => b.build_int_signed_div(l, r, "div"),
                    (BinOp::Div, false) => b.build_int_unsigned_div(l, r, "div"),
                    (_, true) => b.build_int_signed_rem(l, r, "rem"),
                    (_, false) => b.build_int_unsigned_rem(l, r, "rem"),
                };
                Some(value.expect("integer arithmetic").into())
            }
            _ => self.compare(site, op, left, right, &operand_ty, range),
        }
    }

    /// `a == b` on two strings, by content.
    ///
    /// Both operands are owned here — they were evaluated for this comparison —
    /// so both are released once the answer is in hand.
    pub(super) fn compare_strings(
        &mut self,
        op: BinOp,
        left: BasicValueEnum<'ctx>,
        right: BasicValueEnum<'ctx>,
    ) -> Flow<'ctx> {
        let equal = self.strings_equal(left, right);

        self.drop(left, &Type::Str);
        self.drop(right, &Type::Str);

        // The runtime answers in a C `_Bool`, one byte; Khora's `Bool` is an
        // `i1`, so the answer is narrowed by asking whether the byte is set.
        let zero = self.be.ctx.i8_type().const_zero();
        let predicate = match op {
            BinOp::Eq => IntPredicate::NE,
            _ => IntPredicate::EQ,
        };
        Some(
            self.be
                .builder
                .build_int_compare(predicate, equal, zero, "str.cmp")
                .expect("narrowing a C boolean")
                .into(),
        )
    }

    /// `khora_str_eq` over two strings, as the C `_Bool` the runtime returns.
    ///
    /// **Borrows both**, unlike [`Self::compare_strings`], which owns them.
    ///
    /// A literal pattern compares against a scrutinee the `match` still holds
    /// and a static literal nothing owns, so neither may be released here.
    pub(super) fn strings_equal(
        &mut self,
        left: BasicValueEnum<'ctx>,
        right: BasicValueEnum<'ctx>,
    ) -> IntValue<'ctx> {
        let mut parts = Vec::with_capacity(4);
        for value in [left, right] {
            let object = value.into_pointer_value();
            let length_slot =
                runtime::field_pointer(self.be.ctx, &self.be.builder, object, STRING_LEN_FIELD);
            let length = self
                .be
                .builder
                .build_load(self.be.ctx.i64_type(), length_slot, "str.len")
                .expect("reading a string length");
            let bytes = runtime::byte_offset(
                self.be.ctx,
                &self.be.builder,
                object,
                STRING_BYTES_OFFSET,
                "str.bytes",
            );
            parts.push(bytes.into());
            parts.push(length.into());
        }

        self.be
            .builder
            .build_call(self.be.rt.str_eq, &parts, "str.eq")
            .expect("comparing two strings")
            .try_as_basic_value()
            .basic()
            .expect("khora_str_eq returns a _Bool")
            .into_int_value()
    }

    pub(super) fn compare(
        &mut self,
        site: ExprId,
        op: BinOp,
        left: BasicValueEnum<'ctx>,
        right: BasicValueEnum<'ctx>,
        operand_ty: &Type,
        range: TextRange,
    ) -> Flow<'ctx> {
        // `Bool` is an `i1`, where "less than" means `false < true`, so its
        // ordering is unsigned — signing an `i1` comparison inverts it — and an
        // unsigned integer is unsigned for the obvious reason. `255 < 0` being
        // true is exactly the bug this prevents.
        let signed = match Self::int_shape(operand_ty) {
            Some((_, signed)) => signed,
            None => !matches!(operand_ty, Type::Bool),
        };
        let predicate = match op {
            BinOp::Eq => IntPredicate::EQ,
            BinOp::Ne => IntPredicate::NE,
            BinOp::Lt if signed => IntPredicate::SLT,
            BinOp::Lt => IntPredicate::ULT,
            BinOp::Gt if signed => IntPredicate::SGT,
            BinOp::Gt => IntPredicate::UGT,
            BinOp::Le if signed => IntPredicate::SLE,
            BinOp::Le => IntPredicate::ULE,
            BinOp::Ge if signed => IntPredicate::SGE,
            _ => IntPredicate::UGE,
        };

        // IEEE comparison, which is what every reader expects `==` on floats
        // to mean and exactly why `Float` implements neither `Eq` nor `Ord`:
        // `NaN == NaN` is false, and a law-abiding equivalence cannot say so.
        // The *operator* is primitive; the *trait* is for lawful equality.
        // `docs/design/numbers.md`.
        if matches!(operand_ty, Type::Float) {
            use inkwell::FloatPredicate;
            // Ordered, so every comparison involving a NaN is false — `<`, and
            // `==` too. `!=` is the one that is unordered, so that `x != x` is
            // true for a NaN, which is the other half of the same convention.
            let predicate = match op {
                BinOp::Eq => FloatPredicate::OEQ,
                BinOp::Ne => FloatPredicate::UNE,
                BinOp::Lt => FloatPredicate::OLT,
                BinOp::Gt => FloatPredicate::OGT,
                BinOp::Le => FloatPredicate::OLE,
                _ => FloatPredicate::OGE,
            };
            let value = self
                .be
                .builder
                .build_float_compare(
                    predicate,
                    left.into_float_value(),
                    right.into_float_value(),
                    "fcmp",
                )
                .expect("comparing two floats");
            return Some(value.into());
        }

        // Strings compare by their bytes, not by their address: two `"a"`
        // literals are separate allocations and a pointer comparison would call
        // them different.
        if matches!(operand_ty, Type::Str) && matches!(op, BinOp::Eq | BinOp::Ne) {
            return self.compare_strings(op, left, right);
        }

        // Anything with a shape decides for itself what comparison means, in an
        // `Eq` or `Ord` impl the checker already resolved and monomorphization
        // already emitted. The operator is one thing whichever type it is used
        // on: a machine instruction where that is the answer, and a call where
        // the answer is a question only the type can settle.
        if !matches!(operand_ty, Type::Int | Type::Fixed(_) | Type::Bool | Type::Unit) {
            if let Some(symbol) = self.mono.callee(&self.owner.clone(), site) {
                let function = match self.be.callee(&symbol) {
                    Ok(function) => function,
                    Err(message) => return self.fail(message, range),
                };
                let answer = self
                    .be
                    .builder
                    .build_call(function, &[left.into(), right.into()], "compare")
                    .expect("calling a comparison impl")
                    .try_as_basic_value()
                    .basic()
                    .expect("a comparison returns a value");

                return match op {
                    // `!=` is `==` negated. Asking a type for both would be
                    // asking it to be consistent about something it cannot get
                    // wrong here.
                    BinOp::Eq => Some(answer),
                    BinOp::Ne => Some(
                        self.be
                            .builder
                            .build_not(answer.into_int_value(), "ne")
                            .expect("negating an equality")
                            .into(),
                    ),
                    _ => self.read_ordering(op, answer, range),
                };
            }
        }

        if !matches!(operand_ty, Type::Int | Type::Fixed(_) | Type::Bool | Type::Unit) {
            self.drop(left, operand_ty);
            self.drop(right, operand_ty);
            // **Name the operator that was written, and the reason it did
            // not resolve.** This said the same thing whatever the operator
            // was: somebody who wrote `==` was told that `<` needs an `Ord`
            // impl and that `==` reaches an `Eq` impl — which is what they
            // had just done, and what had just failed. The real cause was
            // `Eq` not being imported at the comparison, and the message
            // pointed away from it.
            let (needed, operators) = match op {
                BinOp::Eq | BinOp::Ne => ("Eq", "`==` or `!=`"),
                _ => ("Ord", "`<`, `>`, `<=` or `>=`"),
            };
            return self.fail(
                format!(
                    "two `{operand_ty}` values cannot be compared with {operators}: no \
                     `{needed}` impl was reachable here. Either there is no \
                     `impl {needed} for {operand_ty}`, or `{needed}` is not imported in \
                     this module — an operator only reaches an impl whose trait is in \
                     scope where the comparison is written"
                ),
                range,
            );
        }

        let value = self
            .be
            .builder
            .build_int_compare(predicate, left.into_int_value(), right.into_int_value(), "cmp")
            .expect("a comparison");
        Some(value.into())
    }

    /// `&&` and `||`, short-circuiting.
    ///
    /// Written with a `phi` rather than a slot because both incoming values are
    /// already in registers and the join has no other work to do.
    pub(super) fn logical(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId) -> Flow<'ctx> {
        let left = self.expr(lhs)?;
        let entry = self.here();
        let rhs_block = self.block("logic.rhs");
        let merge = self.block("logic.end");

        let condition = left.into_int_value();
        match op {
            BinOp::And => self.be.builder.build_conditional_branch(condition, rhs_block, merge),
            _ => self.be.builder.build_conditional_branch(condition, merge, rhs_block),
        }
        .expect("a short-circuit branch");

        self.at(rhs_block);
        let right = self.expr(rhs);
        let rhs_end = self.here();
        if right.is_some() {
            self.br(merge);
        }

        self.at(merge);
        let bool_type = self.be.ctx.bool_type();
        let phi = self.be.builder.build_phi(bool_type, "logic").expect("a phi");
        // The short-circuit edge carries the answer the operator already knows:
        // `false` for a failed `&&`, `true` for a satisfied `||`.
        let shortcut = bool_type.const_int(u64::from(matches!(op, BinOp::Or)), false);
        phi.add_incoming(&[(&shortcut, entry)]);
        if let Some(right) = right {
            phi.add_incoming(&[(&right, rhs_end)]);
        }
        Some(phi.as_basic_value())
    }

    pub(super) fn unary(&mut self, op: UnOp, operand: ExprId, _range: TextRange) -> Flow<'ctx> {
        let ty = self.types.of(operand).clone();
        let value = self.expr(operand)?;
        if matches!(op, UnOp::Neg) && matches!(ty, Type::Float) {
            let negated = self
                .be
                .builder
                .build_float_neg(value.into_float_value(), "fneg")
                .expect("negating a float");
            return Some(negated.into());
        }
        let value = value.into_int_value();
        let result = match op {
            // Not checked, unlike `-` the binary operator: the one value that
            // cannot be negated is the type's minimum, and the only way to
            // write it is as a negated literal, which the checker folds into
            // the constant before it ever reaches here.
            UnOp::Neg => self.be.builder.build_int_neg(value, "neg"),
            // On an `i1`, `not` is `xor 1`, which is exactly logical negation.
            UnOp::Not => self.be.builder.build_not(value, "not"),
        };
        Some(result.expect("a unary operator").into())
    }
}
