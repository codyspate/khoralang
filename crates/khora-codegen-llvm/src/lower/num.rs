//! Integers, floats, pointers, and what happens when arithmetic goes wrong.
//!
//! Overflow traps rather than wrapping — `docs/design/numbers.md` — so every
//! arithmetic operation on a sized integer is a checked one and a branch to a
//! trap that names the operation.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
    /// The bit and wrapping operations on `Int`.
    ///
    /// Methods rather than operators, for now. `^`, `&`, `|`, `<<` and `>>`
    /// are five new tokens and `>>` has to be told apart from the end of two
    /// nested type arguments; none of that is hard and none of it is what a
    /// hash function is waiting for.
    ///
    /// Wrapping arithmetic is here because ordinary arithmetic *traps* — see
    /// `checked_arithmetic`. A hash, a checksum and a PRNG are the places that
    /// genuinely want the other behaviour, and asking for it by name is how
    /// the trap stays the default without being in the way.
    /// The primitive integer operations, at whatever width the owner is.
    ///
    /// Three families, and the reason each exists:
    ///
    /// - **Wrapping arithmetic**, because ordinary arithmetic *traps* — see
    ///   `checked_arithmetic`. A hash, a checksum and a PRNG are the places
    ///   that genuinely want the other behaviour, and asking for it by name is
    ///   how the trap stays the default without being in the way.
    /// - **Bit operations**, which are what a hash is made of and what a wire
    ///   format is written in.
    /// - **Conversions**, which are always explicit, because there is no
    ///   implicit widening anywhere in the language and a narrowing that
    ///   happens on its own is how a length becomes 44.
    ///
    /// Every conversion goes through `Int`: `U8::of` and `U8::to_int` rather
    /// than a method for each of the forty-two ordered pairs. `U8` to `U32` is
    /// two steps, which is more to type and never wrong — and the pairs that
    /// deserve one step can be given one later without changing what these
    /// mean.
    pub(super) fn int_intrinsic(
        &mut self,
        (bits, signed): (u32, bool),
        owner: &str,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        // The conversions take one argument; everything else takes two.
        if name == "to_float" {
            let [only] = args else {
                return self.fail(format!("`{owner}::to_float` takes one number"), range);
            };
            let value = self.expr(*only)?.into_int_value();
            let converted = self
                .be
                .builder
                .build_signed_int_to_float(value, self.be.ctx.f64_type(), "to.float")
                .expect("converting an integer to a float");
            return Some(converted.into());
        }
        if matches!(name, "of" | "wrapping" | "to_int" | "wrapping_to_int") {
            let [only] = args else {
                return self.fail(format!("`{owner}::{name}` takes one argument"), range);
            };
            let value = self.expr(*only)?.into_int_value();
            return self.convert(value, (bits, signed), owner, name, range);
        }

        let [left, right] = args else {
            return self.fail(format!("`{owner}::{name}` takes two arguments"), range);
        };
        let l = self.expr(*left)?.into_int_value();
        let r = self.expr(*right)?.into_int_value();
        let b = &self.be.builder;
        let value = match name {
            "wrapping_add" => b.build_int_add(l, r, "wrapping.add"),
            "wrapping_sub" => b.build_int_sub(l, r, "wrapping.sub"),
            "wrapping_mul" => b.build_int_mul(l, r, "wrapping.mul"),
            "xor" => b.build_xor(l, r, "xor"),
            "and" => b.build_and(l, r, "and"),
            "or" => b.build_or(l, r, "or"),
            // Shifting by the width or more is undefined in LLVM, so the count
            // is masked. Silently, and deliberately: every shift would
            // otherwise need a branch, and there is no answer for `x << 8` on
            // a `U8` that is more right than any other.
            "shl" | "shr" => {
                let mask = self.be.int_width(bits).const_int(u64::from(bits - 1), false);
                let count = b.build_and(r, mask, "shift.count").expect("masking a shift");
                if name == "shl" {
                    b.build_left_shift(l, count, "shl")
                } else {
                    // Arithmetic for a signed type, so a negative number stays
                    // negative; logical for an unsigned one, which is what a
                    // hash wants and what `Int` could never express.
                    b.build_right_shift(l, count, signed, "shr")
                }
            }
            _ => {
                return self.fail(
                    format!("`{owner}::{name}` is not an integer operation the backend knows"),
                    range,
                )
            }
        };
        Some(value.expect("an integer operation").into())
    }

    /// One of the four conversions, between `Int` and a fixed-width type.
    ///
    /// `of` and `to_int` stop the program when the value does not fit, for the
    /// same reason `+` does: a number that silently becomes a different number
    /// is found in production rather than in a test. `wrapping` and
    /// `wrapping_to_int` are how to ask for truncation by name.
    pub(super) fn convert(
        &mut self,
        value: IntValue<'ctx>,
        (bits, signed): (u32, bool),
        owner: &str,
        name: &str,
        range: TextRange,
    ) -> Flow<'ctx> {
        let i64_type = self.be.ctx.i64_type();
        let narrow = self.be.int_width(bits);
        let b = &self.be.builder;
        match name {
            // Into the fixed-width type, truncating.
            "wrapping" => {
                Some(b.build_int_truncate_or_bit_cast(value, narrow, "wrapping").ok()?.into())
            }
            // Out of it, widening — which for everything but `U64` is exact,
            // and needs no check.
            "wrapping_to_int" => {
                let wide = if signed {
                    b.build_int_s_extend_or_bit_cast(value, i64_type, "to.int")
                } else {
                    b.build_int_z_extend_or_bit_cast(value, i64_type, "to.int")
                };
                Some(wide.ok()?.into())
            }
            "to_int" => {
                let wide = if signed {
                    b.build_int_s_extend_or_bit_cast(value, i64_type, "to.int")
                } else {
                    b.build_int_z_extend_or_bit_cast(value, i64_type, "to.int")
                }
                .expect("widening to Int");
                // Only `U64` can hold a number `Int` cannot, and it does so
                // exactly when the same bits read as signed are negative.
                if !signed && bits == 64 {
                    let zero = i64_type.const_zero();
                    let ok = self
                        .be
                        .builder
                        .build_int_compare(IntPredicate::SGE, wide, zero, "fits.int")
                        .expect("range-checking a U64");
                    self.guard(ok, &format!("converting {owner} to Int"));
                }
                Some(wide.into())
            }
            // Into the fixed-width type, checked. The check is a round trip:
            // narrow it, widen it back the way the target's signedness says,
            // and require the same number. That is one rule for all fourteen
            // combinations rather than fourteen bounds written by hand.
            _ => {
                let narrowed = b
                    .build_int_truncate_or_bit_cast(value, narrow, "narrowed")
                    .expect("narrowing to a fixed-width integer");
                let back = if signed {
                    self.be.builder.build_int_s_extend_or_bit_cast(narrowed, i64_type, "back")
                } else {
                    self.be.builder.build_int_z_extend_or_bit_cast(narrowed, i64_type, "back")
                }
                .expect("widening back");
                let ok = self
                    .be
                    .builder
                    .build_int_compare(IntPredicate::EQ, back, value, "fits")
                    .expect("comparing the round trip");
                self.guard(ok, &format!("converting Int to {owner}"));
                let _ = range;
                Some(narrowed.into())
            }
        }
    }

    /// Continues only if `ok`; otherwise stops the program saying `what`.
    pub(super) fn guard(&mut self, ok: IntValue<'ctx>, what: &str) {
        let good = self.block("in.range");
        let bad = self.block("out.of.range");
        self.be
            .builder
            .build_conditional_branch(ok, good, bad)
            .expect("branching on a range check");
        self.at(bad);
        self.trap(what);
        self.at(good);
    }

    /// `Float::to_int`: the whole part, and nothing rounded.
    ///
    /// **Truncates toward zero**, which is what C, Rust, Go and every machine
    /// instruction called "convert to integer" do — `2.9` is `2` and `-2.9` is
    /// `-2`. Rounding is a different question with four defensible answers, and
    /// a conversion that quietly picked one would be the wrong kind of
    /// surprise.
    ///
    /// A value too large for an `Int`, or a `NaN`, is *undefined* in LLVM. The
    /// saturating form is what makes it defined, and it is what this uses:
    /// out of range clamps to the nearest end, and a `NaN` is zero. Slower by
    /// one instruction and never nonsense.
    ///
    /// The other direction, `Int::to_float`, is exact for every integer up to
    /// 2^53 and rounds beyond it, which is IEEE's business rather than
    /// Khora's.
    pub(super) fn float_to_int(&mut self, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [only] = args else {
            return self.fail("`Float::to_int` takes one number", range);
        };
        let value = self.expr(*only)?.into_float_value();
        let converted = self
            .be
            .builder
            .build_float_to_signed_int(value, self.be.ctx.i64_type(), "to.int")
            .expect("converting a float to an integer");
        Some(converted.into())
    }

    /// The two things a `Ptr` can do, which is deliberately all of them.
    ///
    /// A `Ptr` is an opaque machine address that came from the other side of
    /// the C ABI. It cannot be dereferenced, offset, or made from a Khora
    /// value — the last is what keeps a dangling one impossible, because the
    /// only pointers that exist are ones a foreign library handed over and
    /// whose lifetimes are that library's business.
    ///
    /// `null` and `is_null` are here because a C function that fails by
    /// returning `NULL` is not a rare case, and because passing `NULL` where a
    /// library allows it is ordinary. `docs/design/ffi.md`.
    pub(super) fn ptr_intrinsic(&mut self, name: &str, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        match (name, args) {
            ("null", []) => Some(self.be.null_pointer().into()),
            ("is_null", [subject]) => {
                let value = self.expr(*subject)?.into_pointer_value();
                let answer = self
                    .be
                    .builder
                    .build_is_null(value, "is.null")
                    .expect("comparing a pointer against null");
                Some(answer.into())
            }
            _ => self.fail(format!("`Ptr::{name}` takes no arguments but `self`"), range),
        }
    }

    /// Arithmetic that stops the program rather than wrapping.
    ///
    /// LLVM's `with.overflow` intrinsics return the result and a flag in one
    /// go, so the check costs a branch the optimizer can usually see through
    /// and never a second computation.
    ///
    /// Trapping in *every* build is the decision: a program that passes its
    /// tests and then wraps in production is the failure worth this branch, and
    /// two behaviours put the difference where it is most expensive to find.
    /// The width and signedness of an integer type, or `None` if it is not one.
    ///
    /// Every arithmetic instruction needs both: LLVM's types carry the width
    /// but not the sign, so `U8` and `I8` are the same `i8` and differ only in
    /// which `div`, `shr`, overflow intrinsic and ordering predicate is asked
    /// for. Getting that wrong is silent, which is why it is read from one
    /// place.
    pub(super) fn int_shape(ty: &Type) -> Option<(u32, bool)> {
        match ty {
            Type::Int => Some((64, true)),
            Type::Fixed(kind) => Some((kind.bits.into(), kind.signed)),
            _ => None,
        }
    }

    /// Stops the program, saying what did not fit.
    ///
    /// The tail of an overflow check and of a narrowing conversion, which want
    /// the same three instructions and the same wording.
    pub(super) fn trap(&mut self, what: &str) {
        let text = self
            .be
            .builder
            .build_global_string_ptr(what, "overflow.what")
            .expect("naming the operation")
            .as_pointer_value();
        let len = self.be.ctx.i64_type().const_int(what.len() as u64, false);
        let report = self.be.rt.overflow;
        self.be
            .builder
            .build_call(report, &[text.into(), len.into()], "")
            .expect("reporting an overflow");
        self.be.builder.build_unreachable().expect("sealing after an overflow");
    }

    pub(super) fn checked_arithmetic(
        &mut self,
        intrinsic: &str,
        bits: u32,
        what: &str,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let checked = self.be.overflow_intrinsic(intrinsic, bits);
        let pair = self
            .be
            .builder
            .build_call(checked, &[left.into(), right.into()], "checked")
            .expect("checked arithmetic")
            .try_as_basic_value()
            .basic()
            .expect("the intrinsic returns a pair")
            .into_struct_value();
        let value = self
            .be
            .builder
            .build_extract_value(pair, 0, what)
            .expect("reading the result");
        let overflowed = self
            .be
            .builder
            .build_extract_value(pair, 1, "overflowed")
            .expect("reading the overflow flag")
            .into_int_value();

        let bad = self.block("overflow");
        let good = self.block("in.range");
        self.be
            .builder
            .build_conditional_branch(overflowed, bad, good)
            .expect("branching on the overflow flag");

        self.at(bad);
        self.trap(what);

        self.at(good);
        value
    }
}
