//! The value an integer literal names, in whichever base it was written.
//!
//! **One reading for every pass.** The type checker, the code generator and
//! the const-generic reader each parsed literal text themselves, and only the
//! code generator knew about a radix prefix -- so a literal one pass read as
//! 255 another refused outright. Every pass reads the text through here, and
//! the parser refuses what this refuses, so nothing later meets a literal it
//! cannot read.

/// Why an integer literal's text names no number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntLiteralError {
    /// A character that is not a digit of the literal's base: the `2` in
    /// `0b12`, the `G` in `0xG`.
    Digit(char),
    /// A prefix and nothing after it but separators: `0x`, `0b_`.
    Empty,
    /// More than 128 bits. No integer type comes close, so the type checker
    /// reports it against whatever type the literal was meant to be.
    TooLarge,
}

/// The base a literal was written in, by its prefix: `0x`, `0b`, `0o`, or
/// none.
pub fn int_literal_base(written: &str) -> Base {
    let mut bytes = written.bytes();
    if bytes.next() != Some(b'0') {
        return Base::Decimal;
    }
    match bytes.next() {
        Some(b'x' | b'X') => Base::Hexadecimal,
        Some(b'o' | b'O') => Base::Octal,
        Some(b'b' | b'B') => Base::Binary,
        _ => Base::Decimal,
    }
}

/// The four bases an integer literal may be written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Binary,
    Octal,
    Decimal,
    Hexadecimal,
}

impl Base {
    /// How many values one digit has.
    pub fn radix(self) -> u32 {
        match self {
            Base::Binary => 2,
            Base::Octal => 8,
            Base::Decimal => 10,
            Base::Hexadecimal => 16,
        }
    }

    /// The base's name with its article, as a message uses it: "`8` is not an
    /// octal digit", "`2` is not a binary digit". Spelled per base rather
    /// than guessed from the first letter, so a base added later says which
    /// article it takes.
    pub fn with_article(self) -> &'static str {
        match self {
            Base::Binary => "a binary",
            Base::Octal => "an octal",
            Base::Decimal => "a decimal",
            Base::Hexadecimal => "a hexadecimal",
        }
    }

    /// The prefix's length in the text: two for a based literal, none for a
    /// decimal one.
    fn prefix_len(self) -> usize {
        match self {
            Base::Decimal => 0,
            Base::Binary | Base::Octal | Base::Hexadecimal => 2,
        }
    }
}

/// The non-negative number `written` names, with `_` separators ignored.
///
/// A `u128` so that every fixed-width type's range, `U64`'s included, is a
/// question the caller can ask of the answer; a negative literal is a negation
/// applied to one of these, and the type checker folds the two.
pub fn int_literal_value(written: &str) -> Result<u128, IntLiteralError> {
    let base = int_literal_base(written);
    let radix = base.radix();
    let mut value: u128 = 0;
    let mut digits = 0usize;
    let mut too_large = false;
    for c in written[base.prefix_len()..].chars() {
        if c == '_' {
            continue;
        }
        let Some(digit) = c.to_digit(radix) else { return Err(IntLiteralError::Digit(c)) };
        digits += 1;
        // Read on past an overflow rather than stopping, so a bad digit
        // later in the literal is still the error reported: it is the one
        // the reader can fix by looking.
        match value.checked_mul(u128::from(radix)).and_then(|v| v.checked_add(u128::from(digit))) {
            Some(next) => value = next,
            None => too_large = true,
        }
    }
    if digits == 0 {
        Err(IntLiteralError::Empty)
    } else if too_large {
        Err(IntLiteralError::TooLarge)
    } else {
        Ok(value)
    }
}

/// [`int_literal_value`], with a leading `-` allowed and applied.
///
/// **Source never spells one** -- `-5` parses as a negation of `5` -- but the
/// front end writes them: a `Decimal` literal desugars to `Literal::Int`
/// halves, and the high half of a negative one is `-1`. A reader that
/// refused the sign refused every negative `Decimal` literal.
pub fn int_literal_signed(written: &str) -> Result<i128, IntLiteralError> {
    let (negative, digits) = match written.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, written),
    };
    let magnitude = int_literal_value(digits)?;
    if negative {
        // `i128::MIN`'s magnitude is one past `i128::MAX`, so it is the one
        // negative value that has to be built rather than negated.
        if magnitude == i128::MIN.unsigned_abs() {
            return Ok(i128::MIN);
        }
        i128::try_from(magnitude).map(|v| -v).map_err(|_| IntLiteralError::TooLarge)
    } else {
        i128::try_from(magnitude).map_err(|_| IntLiteralError::TooLarge)
    }
}
