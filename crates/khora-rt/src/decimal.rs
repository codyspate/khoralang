//! Exact decimal arithmetic, in the width money actually needs.
//!
//! `std::decimal` is a scaled integer: a significand counted in steps of
//! `10^-scale`. That significand used to be an `Int` — sixty-four bits — on the
//! argument that eighteen significant digits is every currency amount anybody
//! transacts.
//!
//! **True about amounts, false about arithmetic on them.** `add` brings both
//! operands to the larger scale before adding, so
//!
//! ```khora
//! notional = 100000000.00d   // significand 10_000_000_000, scale 2
//! rate     = 0.000000000001d // scale 12
//! ```
//!
//! needs the notional at scale twelve — `1e10 × 10^10 = 1e20` against a ceiling
//! of `9.22e18`. It trapped. A hundred million dollars plus a twelve-decimal
//! rate is unremarkable finance and did not fit, and `mul` is worse because it
//! adds the scales, which is what makes it exact.
//!
//! So the significand is a hundred and twenty-eight bits: every fiat
//! computation including that one, `NUMERIC` as real schemas declare it, and an
//! eighteen-decimal token balance, where sixty-four bits topped out at 9.2
//! whole units.
//!
//! # Why all of it moved here
//!
//! Khora has no 128-bit integer and is not getting one — `IntKind` is 8, 16, 32
//! and 64, and widening the language to carry a library type would be a large
//! change for a small gain. So the significand crosses as **two `Int` fields**
//! and every operation on it happens here, where it is an `i128` and nobody has
//! to see the halves.
//!
//! # How a 128-bit answer comes back
//!
//! Not whole. `docs/design/ffi.md` §1: only scalars and pointers cross between
//! generated code and the runtime, because how a 16-byte aggregate returns is a
//! decision LLVM makes for a struct type and rustc makes for a `repr(C)` one —
//! and on x86-64 Windows they disagree silently. Errata 35, and it cost a day.
//!
//! So an operation **returns the low word and leaves the high one in
//! [`SPARE`]**, which the caller takes with [`khora_decimal_high`] on the next
//! line. Two calls rather than one, and no aggregate near the boundary.
//!
//! **The state is per thread, so the condition is exact**: between the two calls
//! there must be no other decimal operation and no chance for the scheduler to
//! take the worker back. Both hold structurally — every wrapper in
//! `std::decimal` reads the high half on the very next line, and the only
//! safepoint generated code emits is a loop back-edge (`lower.rs`'s
//! `back_edge`), which cannot fall between two adjacent `let`s. The same shape
//! `khora_spawn_capture` and `khora_spawn_take` use, for the same reason.

use std::cell::Cell;

thread_local! {
    /// The parts of the last answer that did not fit in a return value:
    /// `(high, scale)`.
    ///
    /// `scale` is read only after [`khora_decimal_parse`], the one operation
    /// whose scale the caller cannot work out for itself — `add` takes the
    /// larger of two, `mul` takes their sum, and `divide` was told.
    static SPARE: Cell<(i64, i64)> = const { Cell::new((0, 0)) };

    /// The low word of the last parse, which cannot travel in that function's
    /// return value because it has to say whether the text was a number.
    static LOW: Cell<i64> = const { Cell::new(0) };
}

/// The significand, from the two words it crossed in.
fn wide(hi: i64, lo: i64) -> i128 {
    ((hi as i128) << 64) | (lo as u64 as i128)
}

/// Records what does not fit in a return value, and answers with the low word.
fn deliver(value: i128, scale: i64) -> i64 {
    SPARE.with(|spare| spare.set(((value >> 64) as i64, scale)));
    value as u64 as i64
}

/// The high word of the last answer. Read on the line after the operation.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_high() -> i64 {
    SPARE.with(|spare| spare.get().0)
}

/// The scale of the last answer, for the one caller that cannot know it.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_scale() -> i64 {
    SPARE.with(|spare| spare.get().1)
}

/// The low word of the last parse.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_low() -> i64 {
    LOW.with(|low| low.get())
}

/// Stops the program, naming the operation that did not fit.
///
/// The same report generated code makes for `Int` arithmetic, so a decimal
/// that overflows and a multiplication that overflows read the same way.
///
/// **A trap rather than a wrong number.** Division used to saturate, on a
/// written-down argument that "a total that quietly became a different total is
/// the failure this type exists to prevent" — which is a description of
/// saturating. `numbers.md` §"Overflow traps, in every build" is the argument
/// and decimals were never outside it.
fn overflowed(what: &str) -> ! {
    // SAFETY: `what` is a string literal in `.rodata`, live for the whole run.
    unsafe { crate::trap::khora_overflow(what.as_ptr(), what.len() as u64) }
}

/// Ten to the `power`, when that is a number a significand can hold.
///
/// `None` outside nought to thirty-eight, which is every power that fits — and
/// **the difference of two scales rather than a scale is what gets asked**, so
/// a number at forty places compared against one at forty-one asks for `10^1`
/// and not `10^41`.
///
/// Clamping the scales themselves instead was a bug for the eight minutes it
/// existed: it made `divide(x, y, 100, HalfEven)` compute to thirty-eight
/// places and label the answer as having a hundred, which is a wrong number
/// wearing the right hat. There is no representable answer at a hundred
/// places, and saying so is the only honest option.
fn ten_to(power: i64) -> Option<i128> {
    if !(0..=38).contains(&power) {
        return None;
    }
    10i128.checked_pow(power as u32)
}

/// `value` moved to a scale `by` places larger, or a stopped program.
fn raised(value: i128, by: i64, what: &str) -> i128 {
    let step = ten_to(by).unwrap_or_else(|| overflowed(what));
    value.checked_mul(step).unwrap_or_else(|| overflowed(what))
}

/// Both operands at the larger of their two scales.
///
/// The step that made sixty-four bits too narrow, and the reason the width
/// changed: the operands fit and their aligned forms did not.
fn aligned(a: i128, a_scale: i64, b: i128, b_scale: i64, what: &str) -> (i128, i128, i64) {
    let scale = a_scale.max(b_scale);
    let a = raised(a, scale - a_scale, what);
    let b = raised(b, scale - b_scale, what);
    (a, b, scale)
}

/// `left + right`, at the larger of their scales.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_add(
    a_hi: i64,
    a_lo: i64,
    a_scale: i64,
    b_hi: i64,
    b_lo: i64,
    b_scale: i64,
) -> i64 {
    const WHAT: &str = "Decimal addition";
    let (a, b, scale) = aligned(wide(a_hi, a_lo), a_scale, wide(b_hi, b_lo), b_scale, WHAT);
    deliver(a.checked_add(b).unwrap_or_else(|| overflowed(WHAT)), scale)
}

/// `left - right`, at the larger of their scales.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_sub(
    a_hi: i64,
    a_lo: i64,
    a_scale: i64,
    b_hi: i64,
    b_lo: i64,
    b_scale: i64,
) -> i64 {
    const WHAT: &str = "Decimal subtraction";
    let (a, b, scale) = aligned(wide(a_hi, a_lo), a_scale, wide(b_hi, b_lo), b_scale, WHAT);
    deliver(a.checked_sub(b).unwrap_or_else(|| overflowed(WHAT)), scale)
}

/// `left * right`. **The scales add**, which the caller works out itself.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_mul(a_hi: i64, a_lo: i64, b_hi: i64, b_lo: i64) -> i64 {
    const WHAT: &str = "Decimal multiplication";
    let (a, b) = (wide(a_hi, a_lo), wide(b_hi, b_lo));
    deliver(a.checked_mul(b).unwrap_or_else(|| overflowed(WHAT)), 0)
}

/// The same number at a larger scale, exactly, or a stopped program.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_rescale(hi: i64, lo: i64, from: i64, to: i64) -> i64 {
    const WHAT: &str = "Decimal rescaling";
    let value = wide(hi, lo);
    if to <= from {
        return deliver(value, from);
    }
    deliver(raised(value, to - from, WHAT), to)
}

/// The whole part, towards zero, as an `Int`.
///
/// Traps when the whole part is wider than sixty-four bits: this is the one
/// place a `Decimal` becomes an `Int` and may not fit in one.
///
/// Past thirty-eight places the answer is zero rather than a trap, and it is
/// the true answer: no significand reaches `10^39`, so every digit of one
/// written that far down is behind the point.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_truncated(hi: i64, lo: i64, scale: i64) -> i64 {
    let value = wide(hi, lo);
    let Some(step) = ten_to(scale.max(0)) else { return 0 };
    i64::try_from(value / step).unwrap_or_else(|_| overflowed("Decimal truncation"))
}

/// Whether the significand is negative, which its digits do not say.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_is_negative(hi: i64, lo: i64) -> i64 {
    i64::from(wide(hi, lo) < 0)
}

/// Where `left` sits relative to `right`: `-1`, `0` or `1`.
///
/// **Comparison must not be able to stop the program**, which is why this does
/// not go through `aligned`. It used to, so `==` on a hundred million and a
/// rate to twelve places asked for a multiplication no significand survived —
/// an equality that traps, in the type sold on the fact that `Float`'s cannot
/// be trusted.
///
/// Different signs settle it with no arithmetic. Otherwise only the operand at
/// the *smaller* scale is raised, so at most one side can run past `i128` — and
/// a side that does is larger in magnitude than one that fits, which is the
/// answer rather than a failure.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_cmp(
    a_hi: i64,
    a_lo: i64,
    a_scale: i64,
    b_hi: i64,
    b_lo: i64,
    b_scale: i64,
) -> i64 {
    let (a, b) = (wide(a_hi, a_lo), wide(b_hi, b_lo));
    let (a_sign, b_sign) = (a.signum(), b.signum());
    if a_sign != b_sign {
        return if a_sign < b_sign { -1 } else { 1 };
    }
    if a_sign == 0 {
        return 0;
    }

    let common = a_scale.max(b_scale);
    let raise = |value: i128, from: i64| -> Option<i128> {
        ten_to(common - from).and_then(|step| value.checked_mul(step))
    };
    match (raise(a, a_scale), raise(b, b_scale)) {
        (Some(a), Some(b)) => {
            if a < b {
                -1
            } else if a == b {
                0
            } else {
                1
            }
        }
        // Ran past `i128` on one side, so that side is the larger magnitude;
        // both have the same sign, which decides what larger means.
        (None, Some(_)) => a_sign as i64,
        (Some(_), None) => -(a_sign as i64),
        // Unreachable: the operand already at the common scale is not raised.
        (None, None) => 0,
    }
}

/// `a * b`, in two hundred and fifty-six bits.
///
/// Four partial products of sixty-four bit halves, each of which fits a `u128`
/// on its own. Needed because dividing to a scale computes `left * 10^n /
/// right`, and with a 128-bit significand that numerator is wider than any
/// integer this language or Rust has.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    const MASK: u128 = u64::MAX as u128;
    let (a1, a0) = (a >> 64, a & MASK);
    let (b1, b0) = (b >> 64, b & MASK);

    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let p11 = a1 * b1;

    let middle = (p00 >> 64) + (p01 & MASK) + (p10 & MASK);
    let low = (p00 & MASK) | (middle << 64);
    let high = p11 + (p01 >> 64) + (p10 >> 64) + (middle >> 64);
    (high, low)
}

/// `(high, low) / divisor`, and the remainder, when the quotient fits 128 bits.
///
/// `None` when it does not, which the caller reports as an overflow: there is
/// no honest 128-bit answer to give.
///
/// Shift and subtract, one bit at a time, because it is short enough to read
/// and division is already the expensive operation. Knuth's algorithm D is
/// faster and is a page of code with a famous off-by-one.
fn div_wide(high: u128, low: u128, divisor: u128) -> Option<(u128, u128)> {
    if divisor == 0 || high >= divisor {
        return None;
    }
    let mut remainder = high;
    let mut quotient: u128 = 0;
    let mut bit = 128;
    while bit > 0 {
        bit -= 1;
        // The bit shifted off the top is worth 2^128, which is larger than any
        // divisor, so it forces the subtraction on its own.
        let carried = remainder >> 127;
        remainder = (remainder << 1) | ((low >> bit) & 1);
        if carried == 1 || remainder >= divisor {
            remainder = remainder.wrapping_sub(divisor);
            quotient |= 1u128 << bit;
        }
    }
    Some((quotient, remainder))
}

/// A magnitude and a sign as an `i128`, or `None` if it does not fit one.
fn signed_from(magnitude: u128, negative: bool) -> Option<i128> {
    if negative {
        if magnitude > (i128::MAX as u128) + 1 {
            return None;
        }
        Some((magnitude as i128).wrapping_neg())
    } else {
        i128::try_from(magnitude).ok()
    }
}

/// `left / right`, to `scale` decimal places.
///
/// `mode` is 0 for half-to-even, 1 for half-away-from-zero, 2 for truncation;
/// `std::decimal::Rounding` is the enumeration and this is its encoding, kept
/// as an integer because an aggregate must not cross.
///
/// Returns zero when `right` is zero. **`std::decimal` never calls it that
/// way** — it answers `None` first, because dividing by a quantity that turned
/// out to be zero is a thing data does rather than a bug.
///
/// **Stops the program when the answer does not fit.** Asking for more digits
/// than a significand holds leaves no honest number to hand back: the nearest
/// one is a different total, and a different total is the failure this type
/// exists to prevent.
#[unsafe(no_mangle)]
pub extern "C" fn khora_decimal_divide(
    a_hi: i64,
    a_lo: i64,
    a_scale: i64,
    b_hi: i64,
    b_lo: i64,
    b_scale: i64,
    scale: i64,
    mode: i64,
) -> i64 {
    const WHAT: &str = "Decimal division";
    let (left, right) = (wide(a_hi, a_lo), wide(b_hi, b_lo));
    let scale = scale.max(0);
    if right == 0 {
        return deliver(0, scale);
    }

    // `left / right` already carries a scale of `left_scale - right_scale`, so
    // the numerator only has to make up the difference to the scale asked for.
    // Working it out this way rather than lining the operands up first keeps
    // the intermediate as small as the question allows.
    let wanted = scale + b_scale - a_scale;
    let negative = (left < 0) != (right < 0);
    let numerator = left.unsigned_abs();
    let mut denominator = right.unsigned_abs();
    let (high, low) = if wanted >= 0 {
        let step = ten_to(wanted).unwrap_or_else(|| overflowed(WHAT)) as u128;
        mul_wide(numerator, step)
    } else {
        let step = ten_to(-wanted).unwrap_or_else(|| overflowed(WHAT)) as u128;
        denominator = denominator.checked_mul(step).unwrap_or_else(|| overflowed(WHAT));
        (0, numerator)
    };

    let Some((quotient, remainder)) = div_wide(high, low, denominator) else {
        overflowed(WHAT)
    };

    // Which way a tie goes is the whole of the mode, and the comparison is
    // doubled to avoid a division that would round on its own.
    let twice = remainder.checked_mul(2);
    let away = match mode {
        // Half to even: step away from zero on a tie only when the quotient
        // would otherwise be odd. What accounting standards specify, because
        // rounding every tie upward biases a long column of figures.
        0 => match twice {
            Some(twice) => twice > denominator || (twice == denominator && quotient % 2 == 1),
            // Doubling ran past 128 bits, so the remainder is more than half.
            None => true,
        },
        1 => match twice {
            Some(twice) => twice >= denominator,
            None => true,
        },
        // Towards zero: truncation, which the division already did.
        _ => false,
    };

    let magnitude = if away && remainder != 0 {
        quotient.checked_add(1).unwrap_or_else(|| overflowed(WHAT))
    } else {
        quotient
    };
    deliver(signed_from(magnitude, negative).unwrap_or_else(|| overflowed(WHAT)), scale)
}

/// The significand's digits, without a sign or a point, into `into`.
///
/// Answers how many bytes the digits need whether or not they were written —
/// the contract `khora_float_text` has, so `String::with_data` drives both. The
/// caller places the point and the sign, because where they go is the scale's
/// business and the scale is a Khora field.
///
/// # Safety
///
/// `into` must address `capacity` writable bytes, or be null with a zero
/// `capacity`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_decimal_digits(
    hi: i64,
    lo: i64,
    into: *mut u8,
    capacity: i64,
) -> i64 {
    let text = wide(hi, lo).unsigned_abs().to_string();
    let bytes = text.as_bytes();
    if capacity < bytes.len() as i64 || into.is_null() {
        return bytes.len() as i64;
    }
    // SAFETY: the contract above, and the length was just checked against it.
    unsafe { into.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
    bytes.len() as i64
}

/// Reads a decimal out of text: `1` if it is one, `0` if it is not.
///
/// The significand and the scale are left where [`khora_decimal_high`],
/// [`khora_decimal_low`] and [`khora_decimal_scale`] find them, because the
/// return value has to say whether it parsed at all.
///
/// Accepts a leading sign, digits, and at most one point. **Rejects everything
/// else on purpose**, including the exponent notation a `Float` takes: a number
/// arriving as `1e-3` is a measurement that has been through a float somewhere,
/// and taking it here would launder that history.
///
/// A numeral too long for the significand is a rejection, not a stopped
/// program. This is the function that reads numbers out of files somebody else
/// wrote, and one long cell must not kill the process.
///
/// # Safety
///
/// `text` must address `len` readable bytes, or be null with a zero `len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_decimal_parse(text: *const u8, len: i64) -> i64 {
    if text.is_null() || len <= 0 {
        return 0;
    }
    // SAFETY: the caller's contract, above.
    let bytes = unsafe { std::slice::from_raw_parts(text, len as usize) };

    let (negative, rest) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    if rest.is_empty() {
        return 0;
    }

    let mut magnitude: u128 = 0;
    let mut point: Option<usize> = None;
    for (at, byte) in rest.iter().enumerate() {
        match byte {
            b'.' if point.is_none() => point = Some(at),
            // A second point is a rejection rather than a silently different
            // number.
            b'.' => return 0,
            b'0'..=b'9' => {
                let digit = u128::from(byte - b'0');
                let Some(grown) = magnitude.checked_mul(10).and_then(|v| v.checked_add(digit))
                else {
                    return 0;
                };
                magnitude = grown;
            }
            _ => return 0,
        }
    }
    // A lone sign, or a lone point, is not a number.
    if point == Some(rest.len() - 1) && rest.len() == 1 {
        return 0;
    }

    let scale = match point {
        Some(at) => (rest.len() - at - 1) as i64,
        None => 0,
    };
    let Some(value) = signed_from(magnitude, negative) else { return 0 };
    SPARE.with(|spare| spare.set(((value >> 64) as i64, scale)));
    LOW.with(|low| low.set(value as u64 as i64));
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two words a `Decimal` crosses in.
    fn split(value: i128) -> (i64, i64) {
        ((value >> 64) as i64, value as u64 as i64)
    }

    /// The whole answer, from a call that returned half of it.
    fn whole(low: i64) -> i128 {
        wide(khora_decimal_high(), low)
    }

    fn add(a: i128, a_scale: i64, b: i128, b_scale: i64) -> i128 {
        let ((a_hi, a_lo), (b_hi, b_lo)) = (split(a), split(b));
        whole(khora_decimal_add(a_hi, a_lo, a_scale, b_hi, b_lo, b_scale))
    }

    fn divide(a: i128, a_scale: i64, b: i128, b_scale: i64, scale: i64, mode: i64) -> i128 {
        let ((a_hi, a_lo), (b_hi, b_lo)) = (split(a), split(b));
        whole(khora_decimal_divide(a_hi, a_lo, a_scale, b_hi, b_lo, b_scale, scale, mode))
    }

    fn cmp(a: i128, a_scale: i64, b: i128, b_scale: i64) -> i64 {
        let ((a_hi, a_lo), (b_hi, b_lo)) = (split(a), split(b));
        khora_decimal_cmp(a_hi, a_lo, a_scale, b_hi, b_lo, b_scale)
    }

    /// **The addition sixty-four bits could not do**, and the reason the width
    /// changed: a hundred million against a rate to twelve places.
    #[test]
    fn a_wide_alignment_no_longer_overflows() {
        let total = add(10_000_000_000, 2, 1, 12);
        assert_eq!(total, 100_000_000_000_000_000_001);
        // Twenty-one digits — seven more than an `Int` significand had room
        // for, and the pair is ordinary finance.
        assert!(total > i64::MAX as i128);
    }

    /// A quotient that is exact comes back exact, whatever the mode.
    #[test]
    fn an_exact_quotient_does_not_round() {
        for mode in 0..3 {
            assert_eq!(divide(100, 2, 4, 0, 2, mode), 25, "1.00 / 4 at mode {mode}");
        }
    }

    /// Half to even does not bias a column of figures; the other two modes do
    /// what they say.
    #[test]
    fn the_rounding_modes_differ_where_it_matters() {
        assert_eq!(divide(125, 3, 1, 0, 2, 0), 12);
        assert_eq!(divide(135, 3, 1, 0, 2, 0), 14);
        assert_eq!(divide(125, 3, 1, 0, 2, 1), 13);
        assert_eq!(divide(135, 3, 1, 0, 2, 1), 14);
        assert_eq!(divide(125, 3, 1, 0, 2, 2), 12);
        assert_eq!(divide(135, 3, 1, 0, 2, 2), 13);
    }

    /// A negative tie rounds away from zero, not towards it.
    #[test]
    fn a_negative_tie_goes_away_from_zero() {
        assert_eq!(divide(-125, 3, 1, 0, 2, 1), -13);
    }

    /// **The intermediate is wider than the answer**, which is why division is
    /// here: a hundred pounds to eight places wants twenty-six digits on the
    /// way to an answer needing ten, and with a 128-bit significand it wants
    /// two hundred and fifty-six.
    #[test]
    fn the_intermediate_is_wider_than_the_answer() {
        assert_eq!(divide(10_000, 2, 3, 0, 8, 2), 3_333_333_333);
        let big = i128::MAX / 4;
        assert_eq!(divide(big, 0, 1, 0, 0, 2), big);
    }

    /// Dividing by zero is answered rather than faulted, because the caller
    /// answers `None` before it gets here.
    #[test]
    fn dividing_by_zero_is_answered_not_faulted() {
        assert_eq!(divide(1, 0, 0, 0, 2, 0), 0);
    }

    /// The operands' own scales are part of the question, not just the target.
    #[test]
    fn the_operands_scales_are_part_of_the_question() {
        assert_eq!(divide(15, 1, 5, 1, 0, 0), 3);
        assert_eq!(divide(100_000_000, 2, 1, 5, 0, 0), 100_000_000_000);
    }

    /// **Comparison answers for numbers arithmetic could not bring together.**
    #[test]
    fn comparing_does_not_need_a_common_scale_to_be_representable() {
        assert_eq!(cmp(10_000_000_000, 2, 1, 12), 1);
        assert_eq!(cmp(1, 12, 10_000_000_000, 2), -1);
        assert_eq!(cmp(-10_000_000_000, 2, -1, 12), -1);
        assert_eq!(cmp(i128::MAX, 0, 1, 38), 1);
    }

    /// The same number written at two scales is one number.
    #[test]
    fn comparing_is_by_value_and_not_by_representation() {
        assert_eq!(cmp(15, 1, 150, 2), 0);
        assert_eq!(cmp(1_500_000, 6, 15, 1), 0);
        assert_eq!(cmp(0, 0, 0, 38), 0);
    }

    /// Sign settles it before any scaling is attempted.
    #[test]
    fn a_sign_decides_on_its_own() {
        assert_eq!(cmp(-1, 38, i128::MAX, 0), -1);
        assert_eq!(cmp(1, 38, -1, 0), 1);
        assert_eq!(cmp(0, 0, -1, 38), 1);
    }

    /// The widening multiply, against products a `u128` can check on its own.
    #[test]
    fn a_widening_multiply_agrees_with_the_narrow_one() {
        for (a, b) in [(0u128, 0u128), (1, 1), (7, 9), (u64::MAX as u128, u64::MAX as u128)] {
            let (high, low) = mul_wide(a, b);
            assert_eq!(high, 0, "{a} * {b} fits 128 bits");
            assert_eq!(low, a * b);
        }
        // And one that does not fit, checked by dividing it back.
        let (high, low) = mul_wide(u128::MAX, 2);
        assert_eq!(div_wide(high, low, 2), Some((u128::MAX, 0)));
    }

    /// A quotient that will not fit 128 bits is refused rather than wrapped.
    #[test]
    fn a_quotient_too_wide_to_hold_is_refused() {
        let (high, low) = mul_wide(u128::MAX, u128::MAX);
        assert_eq!(div_wide(high, low, 1), None);
        assert_eq!(div_wide(0, 1, 0), None, "and so is a zero divisor");
    }

    /// Text in, and the parts out.
    #[test]
    fn reading_a_numeral_gives_the_significand_and_the_scale() {
        let read = |text: &str| -> Option<(i128, i64)> {
            // SAFETY: a live slice for the duration of the call.
            let ok = unsafe { khora_decimal_parse(text.as_ptr(), text.len() as i64) };
            (ok == 1).then(|| {
                (wide(khora_decimal_high(), khora_decimal_low()), khora_decimal_scale())
            })
        };

        assert_eq!(read("12.34"), Some((1234, 2)));
        assert_eq!(read("-0.05"), Some((-5, 2)));
        assert_eq!(read("7"), Some((7, 0)));
        // Twenty-seven digits, which sixty-four bits could not hold and this
        // can — the CSV cell that used to stop the program.
        assert_eq!(
            read("1234567890123456789012345.00"),
            Some((123_456_789_012_345_678_901_234_500, 2))
        );
        // The rejections, none of which stops the program.
        assert_eq!(read("1.2.3"), None);
        assert_eq!(read("1e-3"), None);
        assert_eq!(read(""), None);
        assert_eq!(read("-"), None);
        assert_eq!(read("12a"), None);
        assert_eq!(read("9999999999999999999999999999999999999999999"), None);
    }

    /// The digits come back as the magnitude alone; the caller places the sign
    /// and the point.
    #[test]
    fn the_digits_are_the_magnitude_alone() {
        let text = |value: i128| -> String {
            let (hi, lo) = split(value);
            let mut room = vec![0u8; 64];
            // SAFETY: a live buffer of the length being promised.
            let written =
                unsafe { khora_decimal_digits(hi, lo, room.as_mut_ptr(), room.len() as i64) };
            String::from_utf8(room[..written as usize].to_vec()).expect("digits")
        };

        assert_eq!(text(1234), "1234");
        assert_eq!(text(-1234), "1234", "the sign is the caller's to place");
        assert_eq!(text(0), "0");
        assert_eq!(text(i128::MAX), "170141183460469231731687303715884105727");
        // The most negative significand, whose magnitude is one larger.
        assert_eq!(text(i128::MIN), "170141183460469231731687303715884105728");
    }

    /// **A scale nothing can represent is refused rather than quietly met.**
    ///
    /// The clamp this replaced computed to thirty-eight places and returned the
    /// answer labelled as having a hundred, which is a wrong number wearing the
    /// right hat.
    #[test]
    fn a_scale_no_significand_reaches_is_not_quietly_answered() {
        assert_eq!(ten_to(38), Some(100_000_000_000_000_000_000_000_000_000_000_000_000));
        assert_eq!(ten_to(39), None, "past every power that fits");
        assert_eq!(ten_to(-1), None);
        // But a *difference* of scales is what callers ask for, so two numbers
        // written far past thirty-eight places still compare.
        assert_eq!(cmp(15, 41, 150, 42), 0);
        assert_eq!(cmp(15, 41, 15, 42), 1);
    }

    /// A `Decimal` whose whole part is wider than an `Int` cannot become one.
    #[test]
    fn truncating_answers_what_fits() {
        let (hi, lo) = split(123_456);
        assert_eq!(khora_decimal_truncated(hi, lo, 2), 1234);
        assert_eq!(khora_decimal_truncated(hi, lo, 0), 123_456);
        // Past thirty-eight places there is no whole part to find, and the
        // clamp used to answer `1` for the largest significand at forty.
        let (hi, lo) = split(1);
        assert_eq!(khora_decimal_truncated(hi, lo, 38), 0);
        let (hi, lo) = split(i128::MAX);
        assert_eq!(khora_decimal_truncated(hi, lo, 40), 0);
    }
}
