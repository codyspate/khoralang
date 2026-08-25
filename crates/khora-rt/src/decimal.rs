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

/// Ten to the `power`, as a wide integer. Saturates rather than wrapping.
fn ten_to(power: u32) -> i128 {
    10i128.checked_pow(power).unwrap_or(i128::MAX)
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
/// The result can still overflow an `Int`: dividing a large number by a small
/// one at a large scale produces digits that no sixty-four bit significand can
/// hold. That saturates rather than wrapping, on the same reasoning as
/// everywhere else — a total that quietly became a different total is the
/// failure this type exists to prevent.
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
    let wanted = scale as i64 + right_scale as i64 - left_scale as i64;
    let (numerator, denominator) = if wanted >= 0 {
        (left as i128 * ten_to(wanted as u32), right as i128)
    } else {
        (left as i128, right as i128 * ten_to((-wanted) as u32))
    };

    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder == 0 {
        return saturate(quotient);
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
        saturate(if negative { quotient - 1 } else { quotient + 1 })
    } else {
        saturate(quotient)
    }
}

/// The nearest `i64`, rather than a wrapped one.
fn saturate(value: i128) -> i64 {
    if value > i64::MAX as i128 {
        i64::MAX
    } else if value < i64::MIN as i128 {
        i64::MIN
    } else {
        value as i64
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

    /// A result too large for the significand saturates rather than wrapping.
    #[test]
    fn an_answer_that_cannot_fit_saturates() {
        assert_eq!(khora_decimal_divide(i64::MAX, 0, 1, 0, 4, 0), i64::MAX);
        assert_eq!(khora_decimal_divide(i64::MIN, 0, 1, 0, 4, 0), i64::MIN);
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
