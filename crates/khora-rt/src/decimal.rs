//! The one decimal operation that needs more than sixty-four bits.
//!
//! `std::decimal` is a scaled integer and almost all of it is written in Khora:
//! adding, subtracting, comparing and multiplying are `Int` arithmetic with the
//! scales lined up, and they trap on overflow like every other number in the
//! language. Division cannot be, for two reasons that arrive together.
//!
//! It has no exact answer in general — one third has no finite decimal form —
//! so it takes a scale and a rounding mode rather than pretending. And getting
//! to that scale means computing `left * 10^n / right`, whose numerator
//! overflows a sixty-four bit integer for perfectly ordinary money: a hundred
//! pounds divided to eight places wants twenty-six digits on the way to an
//! answer that needs ten.
//!
//! So the intermediate is a Rust `i128`, which is wide enough for anything an
//! `Int` significand can produce, and what comes back is the one number that
//! fits. Khora never sees the hundred and twenty-eight bits and does not need a
//! type for them.
//!
//! **And when the answer does not fit, this stops the program.** It used to
//! saturate, on a written-down argument that "a total that quietly became a
//! different total is the failure this type exists to prevent" -- which is a
//! description of what saturating does. `Decimal::divide(10d, 1d, 18,
//! HalfEven)` answered `9.223372036854775807`, in range, printed to the right
//! number of places, and balancing against itself. Somebody writing a
//! reconciler found it eight minutes in. `add`, `sub` and `mul` all trap;
//! there was never a reason for division to be the one that lies.
//!
//! The second thing here is comparison. `Eq` and `Ord` used to bring both
//! operands to a common scale in Khora, which multiplies, which traps -- so
//! comparing a hundred million against a rate to twelve places stopped the
//! program. Comparing needs no such thing: it is a question about order, not a
//! value that has to exist, and the widths it needs are here already.

/// Ten to the `power`, as a wide integer, or `None` past the widest one.
///
/// `i128` reaches `10^38`. A caller asking for more is asking for a scale no
/// answer could be written at, and gets a stopped program rather than a
/// number that came from a clamp.
fn ten_to(power: u32) -> Option<i128> {
    10i128.checked_pow(power)
}

/// Stops the program, saying which operation did not fit.
///
/// The same report generated code makes for `Int` arithmetic, so a decimal
/// that does not fit and a multiplication that does not fit read the same way
/// and land in the same place.
fn overflowed(what: &str) -> ! {
    // SAFETY: `what` is a string literal in `.rodata`, live for the whole run,
    // and `len` is its own length.
    unsafe { crate::trap::khora_overflow(what.as_ptr(), what.len() as u64) }
}

/// `left / right`, to `scale` decimal places.
///
/// `mode` is 0 for half-to-even, 1 for half-away-from-zero, 2 for truncation;
/// `std::decimal::Rounding` is the enumeration and this is its encoding, kept
/// as an integer because `docs/design/ffi.md` §1 says an aggregate must not
/// cross and a tag is not one.
///
/// Returns zero when `right` is zero. **`std::decimal` never calls it that
/// way** — it answers `None` before getting here, because dividing by a
/// quantity that turned out to be zero is a thing data does rather than a bug
/// — and this is the answer for a caller that arrives from somewhere else.
///
/// **Stops the program when the answer does not fit.** Dividing a large
/// number by a small one at a large scale produces digits no sixty-four bit
/// significand can hold, and there is no honest number to hand back: the
/// nearest one is a different total, and a different total is the failure this
/// type exists to prevent. `add`, `sub` and `mul` all stop for the same reason
/// and `docs/design/numbers.md` argues the case once for all four.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_divide(
    left: i64,
    left_scale: i64,
    right: i64,
    right_scale: i64,
    scale: i64,
    mode: i64,
) -> i64 {
    if right == 0 {
        return 0;
    }
    let scale = scale.clamp(0, 38) as u32;
    let left_scale = left_scale.clamp(0, 38) as u32;
    let right_scale = right_scale.clamp(0, 38) as u32;

    // `left / right` already carries a scale of `left_scale - right_scale`, so
    // the numerator only has to make up the difference to the scale asked for.
    // Working it out this way rather than lining the operands up first keeps
    // the intermediate as small as the question allows.
    //
    // Every step is checked, including this one: a scale of 38 against an
    // operand scaled the other way asks for `10^76`, which is past `i128` as
    // surely as the answer is past `i64`.
    let wanted = scale as i64 + right_scale as i64 - left_scale as i64;
    let (numerator, denominator) = if wanted >= 0 {
        let step = ten_to(wanted as u32).unwrap_or_else(|| overflowed(WHAT));
        (
            (left as i128).checked_mul(step).unwrap_or_else(|| overflowed(WHAT)),
            right as i128,
        )
    } else {
        let step = ten_to((-wanted) as u32).unwrap_or_else(|| overflowed(WHAT));
        (
            left as i128,
            (right as i128).checked_mul(step).unwrap_or_else(|| overflowed(WHAT)),
        )
    };
    if denominator == 0 {
        // Only reachable by scaling a non-zero divisor into nothing, which the
        // checked multiply above already rules out. Belt and braces, because
        // the alternative is a division by zero in the next line.
        return 0;
    }

    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder == 0 {
        return fits(quotient);
    }

    // Which way the tie goes is the whole of the mode, and the comparison is
    // doubled to avoid a division that would round on its own.
    let negative = (numerator < 0) != (denominator < 0);
    let twice = (remainder.saturating_mul(2)).abs();
    let magnitude = denominator.abs();
    let away = match mode {
        // Half to even: only step away from zero on a tie when the quotient
        // would otherwise be odd. This is what accounting standards specify,
        // because rounding every tie upward biases a long column of figures.
        0 => twice > magnitude || (twice == magnitude && quotient % 2 != 0),
        1 => twice >= magnitude,
        // Towards zero: truncation, which is what the division already did.
        _ => false,
    };
    if away {
        fits(if negative { quotient - 1 } else { quotient + 1 })
    } else {
        fits(quotient)
    }
}

/// What generated code calls a division that did not fit, so that the trap
/// reads like the ones `Int` arithmetic produces.
const WHAT: &str = "Decimal division";

/// The answer as an `Int`, or a stopped program.
fn fits(value: i128) -> i64 {
    i64::try_from(value).unwrap_or_else(|_| overflowed(WHAT))
}

/// Where `left` sits relative to `right`: `-1`, `0` or `1`.
///
/// **Comparison must not be able to stop the program.** `std::decimal` used to
/// answer this by bringing both operands to a common scale, which multiplies,
/// which traps -- so `100000000.00d == 0.000000000001d`, two perfectly legal
/// numbers, halted a reconciler. An `Eq` that stops is a worse trap than one
/// that surprises, and it is the exact promise the type is sold on:
/// `std/decimal.kh` says this is the whole reason `Float` cannot have an `Eq`
/// and this can.
///
/// Nothing has to be representable for one number to be bigger than another,
/// and this never needs it to be. Different signs settle it with no
/// arithmetic. Otherwise only the operand at the *smaller* scale is scaled up,
/// so at most one side can run past `i128` -- and a side that does is larger
/// in magnitude than one that fits, which is the answer rather than a failure.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_cmp(
    left: i64,
    left_scale: i64,
    right: i64,
    right_scale: i64,
) -> i64 {
    let (left_sign, right_sign) = (left.signum(), right.signum());
    if left_sign != right_sign {
        return if left_sign < right_sign { -1 } else { 1 };
    }
    if left_sign == 0 {
        return 0;
    }

    let left_scale = left_scale.clamp(0, 38) as u32;
    let right_scale = right_scale.clamp(0, 38) as u32;
    let common = left_scale.max(right_scale);

    // The one already at the common scale is multiplied by `10^0`, so it
    // cannot fail. Only the other one can, and only by being enormous.
    let scaled = |value: i64, scale: u32| -> Option<i128> {
        ten_to(common - scale).and_then(|step| (value as i128).checked_mul(step))
    };
    match (scaled(left, left_scale), scaled(right, right_scale)) {
        (Some(l), Some(r)) => {
            if l < r {
                -1
            } else if l == r {
                0
            } else {
                1
            }
        }
        // Ran past `i128` on the left, so the left is the larger magnitude.
        // Both operands have the same sign, which decides what larger means.
        (None, Some(_)) => left_sign,
        (Some(_), None) => -left_sign,
        // Unreachable: the operand at the common scale is never scaled.
        (None, None) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quotient that is exact comes back exact, whatever the mode.
    #[test]
    fn an_exact_quotient_does_not_round() {
        // 1.00 / 4 = 0.25
        for mode in 0..3 {
            assert_eq!(khora_decimal_divide(100, 2, 4, 0, 2, mode), 25);
        }
    }

    /// **The reason half-to-even is the default.** Two ties in opposite
    /// directions cancel instead of both going up.
    #[test]
    fn half_to_even_does_not_bias_a_column() {
        // 0.125 -> 0.12, 0.135 -> 0.14: the even neighbour each time.
        assert_eq!(khora_decimal_divide(125, 3, 1, 0, 2, 0), 12);
        assert_eq!(khora_decimal_divide(135, 3, 1, 0, 2, 0), 14);
        // Half away from zero takes both upward, which over a ledger is money.
        assert_eq!(khora_decimal_divide(125, 3, 1, 0, 2, 1), 13);
        assert_eq!(khora_decimal_divide(135, 3, 1, 0, 2, 1), 14);
        // Truncation takes both down.
        assert_eq!(khora_decimal_divide(125, 3, 1, 0, 2, 2), 12);
        assert_eq!(khora_decimal_divide(135, 3, 1, 0, 2, 2), 13);
    }

    /// Negative numbers round away from zero, not downward.
    #[test]
    fn a_negative_tie_goes_away_from_zero() {
        assert_eq!(khora_decimal_divide(-125, 3, 1, 0, 2, 1), -13);
        assert_eq!(khora_decimal_divide(-135, 3, 1, 0, 2, 2), -13);
    }

    /// **The case the sixty-four bit path could not do.** A hundred pounds to
    /// eight places needs a twenty-six digit numerator.
    #[test]
    fn the_intermediate_is_wider_than_the_answer() {
        // 100.00 / 3 at eight places = 33.33333333
        assert_eq!(khora_decimal_divide(10_000, 2, 3, 0, 8, 0), 3_333_333_333);
    }

    /// One third, to as many places as an `Int` can hold.
    #[test]
    fn a_recurring_quotient_rounds_at_the_scale_it_was_given() {
        assert_eq!(khora_decimal_divide(1, 0, 3, 0, 4, 0), 3_333);
        assert_eq!(khora_decimal_divide(2, 0, 3, 0, 4, 0), 6_667);
    }

    /// Dividing by zero answers zero rather than faulting; `std::decimal`
    /// answers `None` before it ever gets here.
    #[test]
    fn dividing_by_zero_is_answered_not_faulted() {
        assert_eq!(khora_decimal_divide(1, 0, 0, 0, 2, 0), 0);
    }

    /// **The largest answer that fits still comes back exact**, which is the
    /// half of the overflow question a unit test can ask.
    ///
    /// The other half stops the process, so it is asked where a stopped
    /// process is an expected outcome: `tests/decimal.rs` in the code
    /// generator runs the program and checks that it ended.
    #[test]
    fn an_answer_at_the_edge_of_the_significand_is_exact() {
        // Right up against the top, undivided and unscaled.
        assert_eq!(khora_decimal_divide(i64::MAX, 0, 1, 0, 0, 0), i64::MAX);
        assert_eq!(khora_decimal_divide(i64::MIN, 0, 1, 0, 0, 0), i64::MIN);
        // And one place further out, reached by dividing rather than by scaling.
        assert_eq!(khora_decimal_divide(i64::MAX, 0, 10, 0, 1, 2), i64::MAX);
    }

    /// **Comparison answers for numbers arithmetic could not bring together.**
    ///
    /// A hundred million against a rate to twelve places wants a common scale
    /// of twelve, which is a multiplication by `10^12` that no `Int` survives.
    /// Ordering them needs no such number to exist.
    #[test]
    fn comparing_does_not_need_a_common_scale_to_be_representable() {
        // 100000000.00 against 0.000000000001
        assert_eq!(khora_decimal_cmp(10_000_000_000, 2, 1, 12), 1);
        assert_eq!(khora_decimal_cmp(1, 12, 10_000_000_000, 2), -1);
        // The same, on the other side of zero, where the order reverses.
        assert_eq!(khora_decimal_cmp(-10_000_000_000, 2, -1, 12), -1);
        assert_eq!(khora_decimal_cmp(-1, 12, -10_000_000_000, 2), 1);
        // As far apart as the scales go.
        assert_eq!(khora_decimal_cmp(i64::MAX, 0, 1, 38), 1);
        assert_eq!(khora_decimal_cmp(1, 38, i64::MAX, 0), -1);
    }

    /// The same number written at two scales is one number.
    #[test]
    fn comparing_is_by_value_and_not_by_representation() {
        // 1.5 and 1.50 and 1.500000
        assert_eq!(khora_decimal_cmp(15, 1, 150, 2), 0);
        assert_eq!(khora_decimal_cmp(1_500_000, 6, 15, 1), 0);
        // Zero is zero at every scale, including against itself.
        assert_eq!(khora_decimal_cmp(0, 0, 0, 38), 0);
        assert_eq!(khora_decimal_cmp(0, 12, 0, 0), 0);
    }

    /// Sign settles it before any scaling is attempted, which is both the fast
    /// path and the one case where the scales cannot matter.
    #[test]
    fn a_sign_decides_on_its_own() {
        assert_eq!(khora_decimal_cmp(-1, 38, i64::MAX, 0), -1);
        assert_eq!(khora_decimal_cmp(1, 38, -1, 0), 1);
        assert_eq!(khora_decimal_cmp(0, 0, -1, 38), 1);
        assert_eq!(khora_decimal_cmp(-1, 38, 0, 0), -1);
    }

    /// Ordinary comparisons, at the same scale and at neighbouring ones.
    #[test]
    fn comparing_orders_the_way_the_numbers_read() {
        assert_eq!(khora_decimal_cmp(100, 2, 200, 2), -1);
        assert_eq!(khora_decimal_cmp(200, 2, 100, 2), 1);
        // 1.01 against 1.0009
        assert_eq!(khora_decimal_cmp(101, 2, 10_009, 4), 1);
        // -1.01 against -1.0009
        assert_eq!(khora_decimal_cmp(-101, 2, -10_009, 4), -1);
    }

    /// The operands' own scales are respected, not just the target.
    #[test]
    fn the_operands_scales_are_part_of_the_question() {
        // 1.5 / 0.5 = 3
        assert_eq!(khora_decimal_divide(15, 1, 5, 1, 0, 0), 3);
        // 0.05 / 0.5 = 0.1
        assert_eq!(khora_decimal_divide(5, 2, 5, 1, 2, 0), 10);
    }
}
