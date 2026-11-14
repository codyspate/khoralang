//! Strings and bytes: comparison, search, validation, formatting.
//!
//! A Khora `String` is an ordinary counted object whose first field is a length
//! and whose bytes follow it, so most string work is generated code walking
//! that layout. What lives here is the handful of operations that are either
//! too slow written in Khora — `khora_str_find` is `memmem`, and the Khora
//! version was six hundred times slower — or need a Rust library to be correct
//! at all, like UTF-8 validation and float formatting.

use super::*;

/// Whether `len` bytes starting at `data` are well-formed UTF-8.
///
/// The runtime's job because the answer is a table nobody should write twice,
/// and Rust's standard library already has it. Note what crosses: a pointer
/// and a length, and a `_Bool` back — the boundary rule holds here as it does
/// everywhere else.
///
/// # Safety
///
/// `data` must be null with a zero `len`, or address `len` initialized bytes
/// that stay live for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_utf8_valid(data: *const u8, len: i64) -> bool {
    if len <= 0 {
        return true;
    }
    if data.is_null() {
        return false;
    }
    // SAFETY: the contract above.
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
    std::str::from_utf8(bytes).is_ok()
}

/// Adds up `len` bytes starting at `data`.
///
/// A testing aid, and specifically a *foreign function* one: it is here so that
/// `Array::with_data` and `String::with_data` can be tested against something
/// that actually reads through the pointer they lend. A test that only checks
/// the pointer is non-null would pass just as well if the pointer addressed
/// the wrong place.
///
/// # Safety
///
/// `data` must be null with a zero `len`, or address `len` initialized bytes
/// that stay live for the call — which is exactly what a borrow guarantees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_sum_bytes(data: *const u8, len: i64) -> i64 {
    if data.is_null() || len <= 0 {
        return 0;
    }
    // SAFETY: the contract above.
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
    bytes.iter().map(|b| i64::from(*b)).sum()
}

/// Where `needle` first occurs in `hay`, or -1.
///
/// **A runtime call because the Khora version was 600 times slower.** Written
/// as a loop over `String::byte`, finding a six-byte word in eighty bytes took
/// 3,180 nanoseconds — a function call and a bounds check per byte, per
/// candidate position. `memmem` does it in single digits, and the request
/// parser calls it several times for every request a server answers.
///
/// An empty needle is found at zero, which is what every other language says
/// and what makes `split_once` on an empty separator terminate.
///
/// # Safety
///
/// Both pointers must address at least their stated length in readable bytes,
/// or be null with a length of zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_str_find(
    hay: *const u8,
    hay_len: u64,
    needle: *const u8,
    needle_len: u64,
) -> i64 {
    if needle_len == 0 {
        return 0;
    }
    if needle_len > hay_len || hay.is_null() || needle.is_null() {
        return -1;
    }
    // SAFETY: the caller guarantees both lengths are readable.
    let (hay, needle) = unsafe {
        (
            std::slice::from_raw_parts(hay, hay_len as usize),
            std::slice::from_raw_parts(needle, needle_len as usize),
        )
    };
    // Narrowed once, here, rather than at every index below. The ABI is
    // fixed-width — see the note on this function's signature — and a slice
    // index is a `usize`, so exactly one conversion is the right number.
    let (hay_len, needle_len) = (hay.len(), needle.len());
    // The first byte narrows the candidates before anything longer is compared,
    // which is the whole of why this is not the loop it replaced.
    let first = needle[0];
    let last = hay_len - needle_len;
    let mut at = 0;
    while at <= last {
        match hay[at..=last].iter().position(|b| *b == first) {
            None => return -1,
            Some(step) => {
                let here = at + step;
                if &hay[here..here + needle_len] == needle {
                    return here as i64;
                }
                at = here + 1;
            }
        }
    }
    -1
}

/// Whether two strings hold the same bytes.
///
/// Takes bytes and lengths rather than object pointers, matching
/// [`khora_print_str`]: the header layout is the code generator's business, and
/// the runtime stays a function of the data it is handed.
///
/// A null pointer is only valid with a zero length, which is how an
/// uninitialized slot compares equal to `""` and to itself.
///
/// # Safety
///
/// Each pointer must be null or address `len` initialized bytes that stay live
/// and unmodified for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_str_eq(
    a: *const u8,
    a_len: u64,
    b: *const u8,
    b_len: u64,
) -> bool {
    if a_len != b_len {
        return false;
    }
    if a_len == 0 {
        return true;
    }
    if a.is_null() || b.is_null() {
        fatal("string comparison of a null pointer with a non-zero length");
    }
    // SAFETY: the caller guarantees `len` initialized bytes at each pointer,
    // live and unmodified for this call, and each length came from an
    // allocation so is far below `isize::MAX`.
    unsafe { std::slice::from_raw_parts(a, a_len as usize) == std::slice::from_raw_parts(b, b_len as usize) }
}

/// The byte order of two strings: -1, 0 or 1.
///
/// **What this prevents: sorting text a byte at a time in Khora.** `String`'s
/// `cmp` was a loop that bounds-checked two strings and polled for
/// cancellation at every byte; sorting the thirteen rows of the fortunes page
/// by their text was a fifth of the request. This is one `memcmp` and a length
/// compare, and gives the same order: bytes compared unsigned, so 0x80 comes
/// after `z`, and a string that is a prefix of another comes first.
///
/// # Safety
///
/// As [`khora_str_eq`]: each pointer must be null or address `len`
/// initialized bytes that stay live and unmodified for the call. A length is
/// never negative.
#[unsafe(no_mangle)]
// SHARE: reads two lent byte buffers; takes no Khora object.
pub unsafe extern "C" fn khora_str_cmp(a: *const u8, a_len: i64, b: *const u8, b_len: i64) -> i64 {
    // SAFETY: a length of zero never reads its pointer, which may be null for
    // an uninitialized slot; any other length is the caller's guarantee of
    // that many live bytes.
    let mine: &[u8] = if a_len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(a, a_len as usize) } };
    // SAFETY: as above.
    let theirs: &[u8] = if b_len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(b, b_len as usize) } };
    match mine.cmp(theirs) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// The process's key for hashing text, drawn from the operating system once.
///
/// **What this prevents: a client choosing `Map` keys that all collide.** An
/// unkeyed string hash is a public function, so anybody can work out offline a
/// set of strings that share one bucket, and a map built from them -- a JSON
/// object's members, a query string, a header map -- costs time in the square
/// of its size. With a key nobody outside the process knows, a set that
/// collides in one run is ordinary keys in the next.
///
/// `RandomState` is the door Rust's standard library already opens onto the
/// operating system's random source for exactly this job, so this adds no
/// dependency. One `RandomState` for the whole process, because a hash must be
/// the same function in every fiber or a map could not find its own keys.
///
/// What it costs: the iteration order of a `Map` keyed by text differs from
/// run to run, which the type's documentation says. On a target with no
/// entropy source (`wasm32-unknown-unknown`), Rust keys it from two addresses,
/// which is weaker and says so in Rust's own source.
fn text_hash_key() -> &'static std::collections::hash_map::RandomState {
    static KEY: std::sync::OnceLock<std::collections::hash_map::RandomState> =
        std::sync::OnceLock::new();
    KEY.get_or_init(std::collections::hash_map::RandomState::new)
}

/// Draws the text-hash key now, so the read happens at startup rather than on
/// whichever request first hashes a string.
pub(crate) fn draw_text_hash_key() {
    let _ = text_hash_key();
}

/// The keyed hash of `len` bytes: Rust's default hasher, SipHash, under the
/// process's key.
///
/// The hash `impl Hash for String` answers, and so the one `Map` buckets text
/// by. SipHash is what Rust's own `HashMap` uses for the same reason: without
/// its key it cannot be predicted, and FNV-1a, which this replaced, has no key.
/// It costs a runtime call per hash, where FNV-1a was a loop in Khora.
///
/// # Safety
///
/// As [`khora_str_eq`]: the pointer must be null with a zero length, or
/// address `len` initialized bytes that stay live and unmodified for the call.
#[unsafe(no_mangle)]
// SHARE: reads a lent byte buffer; takes no Khora object.
pub unsafe extern "C" fn khora_hash_bytes(data: *const u8, len: i64) -> i64 {
    use std::hash::{BuildHasher, Hasher};

    // SAFETY: a length of zero never reads its pointer, which may be null for
    // an empty string; any other length is the caller's guarantee of that many
    // live bytes.
    let bytes: &[u8] = if len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(data, len as usize) } };
    let mut hasher = text_hash_key().build_hasher();
    hasher.write(bytes);
    hasher.finish() as i64
}

/// Writes `value` into `into` as text, and says how many bytes that took.
///
/// The shortest digits that read back as the same number, which is what a
/// reader means by "the number". Khora cannot produce them itself:
/// shortest-round-trip formatting is Ryū or Grisu, a table and a thousand
/// lines, and it is exactly the kind of thing `docs/design/ecosystem.md` says
/// to bind rather than write twice.
///
/// **Plain from `1e-6` up to below `1e21`, with an exponent outside**:
/// `0.000001`, `123456789012345680000`, then `1e-7`, `1e21`, `5e-324`. It is
/// JavaScript's `Number.prototype.toString` rule. Rust's `{}`, which this was,
/// never writes an exponent, so `1e300` came out as 301 digits and `5e-324`
/// as 327 characters, and `Float::to_string`, which allows 32 bytes, answered
/// `"?"` for them. The longest text is 25 bytes, a 17-digit value just above
/// `1e-6` such as `-0.0000010000000000000002`. The exponent is written as `e300` and
/// `e-7`, with no `+`, which `Float::of_string` and JSON both read.
///
/// Returns the length needed when `capacity` is too small and writes nothing,
/// so a caller can size a buffer in two calls rather than guessing.
///
/// # Safety
///
/// `into` must address `capacity` writable bytes, or be null with a zero
/// `capacity`.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_float_text(value: f64, into: *mut u8, capacity: i64) -> i64 {
    let magnitude = value.abs();
    let text = if magnitude != 0.0 && magnitude.is_finite() && !(1e-6..1e21).contains(&magnitude) {
        format!("{value:e}")
    } else {
        format!("{value}")
    };
    let bytes = text.as_bytes();
    if capacity < bytes.len() as i64 || into.is_null() {
        return bytes.len() as i64;
    }
    // SAFETY: the contract above, and the length was just checked against it.
    unsafe { into.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
    bytes.len() as i64
}

/// The double nearest the number `text` spells, or `NaN` if it does not spell
/// one in `Float::of_string`'s grammar.
///
/// **Bound for the reason `khora_float_text` is: correctly rounded reading is
/// Eisel-Lemire with a big-number fallback, not a loop.** `Float::of_string`
/// was that loop -- digits accumulated in doubles, scaled by a power of ten
/// built the same way, a rounding at every step -- and it read about four
/// doubles in five one unit in the last place off (`1.118` as
/// `1.1179999999999999`), and `5e-324` as zero. Rust's `str::parse::<f64>`
/// is correctly rounded, as Go's, JavaScript's and Python's readers are.
///
/// **The grammar is checked here first, because Rust's parser accepts more**:
/// `inf`, `infinity` and `nan` in any case, a leading `+`, `1.`, `.5`. Each of
/// those is a number nobody meant arriving from a configuration file, and
/// `Float::of_string` refuses them. What passes is JSON's shape, plus the
/// leading zeros `of_string` has always taken: an optional `-`, digits, an
/// optional `.` and digits, an optional `e` or `E` with an optional sign and
/// digits, and nothing else.
///
/// `NaN` is the refusal because no accepted text can produce it: the grammar
/// has no name for it, and a parse of digits gives a finite number, zero or
/// an infinity. A second return channel would cost the Khora side a buffer.
///
/// # Safety
///
/// `text` must address `len` readable bytes, or be null with a zero `len`.
#[unsafe(no_mangle)]
// SHARE: reads bytes from a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_float_parse(text: *const u8, len: i64) -> f64 {
    if text.is_null() || len <= 0 {
        return f64::NAN;
    }
    // SAFETY: the caller's contract, above, and `len` was just checked positive.
    let bytes = unsafe { std::slice::from_raw_parts(text, len as usize) };
    if !is_number_shape(bytes) {
        return f64::NAN;
    }
    // The shape is ASCII, so the bytes are UTF-8 and the parse sees what was
    // checked; neither fallback can be reached.
    std::str::from_utf8(bytes).ok().and_then(|text| text.parse().ok()).unwrap_or(f64::NAN)
}

/// Whether `bytes` is a number `Float::of_string` accepts. See
/// [`khora_float_parse`] for the grammar and why it is narrower than Rust's.
fn is_number_shape(bytes: &[u8]) -> bool {
    let digits_from = |at: usize| at + bytes[at..].iter().take_while(|b| b.is_ascii_digit()).count();
    let mut at = usize::from(bytes.first() == Some(&b'-'));
    let whole = digits_from(at);
    if whole == at {
        return false;
    }
    at = whole;
    if bytes.get(at) == Some(&b'.') {
        let fraction = digits_from(at + 1);
        if fraction == at + 1 {
            return false;
        }
        at = fraction;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let exponent = digits_from(at);
        if exponent == at {
            return false;
        }
        at = exponent;
    }
    at == bytes.len()
}

/// The number to exactly `places` decimal places.
///
/// **Here rather than in Khora, and not for the reason `khora_float_text` is.**
/// That one is bound because shortest-round-trip formatting is Ryū and a
/// thousand lines. This one is bound because the obvious Khora version --
/// multiply by `10^places`, round, divide back -- does its rounding in binary
/// floating point, which is the exact error `std::decimal` exists to avoid.
///
/// A percentage that renders as `33.33` on one machine and `33.34` on another
/// is the bug, and it is introduced by the arithmetic rather than by the
/// formatting.
///
/// Rust's `{:.*}` rounds the *decimal expansion of the double*, which is what
/// C's `printf`, Go's `strconv` and every other language's fixed formatter do,
/// so a Khora program and its neighbors agree about `0.125` at two places.
///
/// `places` is clamped to nought through thirty. A double carries about
/// seventeen significant digits, so beyond that the extra characters are an
/// artifact of the binary value rather than information -- and an unclamped
/// count is an allocation a caller can ask for by accident.
///
/// Same contract as the shortest form: answers the length needed and writes
/// nothing when `capacity` is too small.
///
/// # Safety
///
/// `into` must address `capacity` writable bytes, or be null with a zero
/// `capacity`.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_float_fixed(
    value: f64,
    places: i64,
    into: *mut u8,
    capacity: i64,
) -> i64 {
    let places = places.clamp(0, 30) as usize;
    let text = format!("{value:.places$}");
    let bytes = text.as_bytes();
    if capacity < bytes.len() as i64 || into.is_null() {
        return bytes.len() as i64;
    }
    // SAFETY: the contract above, and the length was just checked against it.
    unsafe { into.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
    bytes.len() as i64
}

/// Writes `len` bytes of text into `into` with `&`, `<`, `>`, `"` and `'`
/// replaced by HTML entities, and says how many bytes that took.
///
/// **What this prevents: escaping a page a byte at a time in Khora.** A loop
/// over `String::byte` pays a call, a bounds check and a safepoint per byte,
/// and builds a string per run it keeps; on the fortunes page that was about
/// five hundred loop iterations and two dozen allocations per request, for
/// text that is almost all plain. This scans once and copies the plain runs
/// whole, which is what `Bun.escapeHTML` does.
///
/// The entities are `&amp;`, `&lt;`, `&gt;`, `&quot;` and `&apos;`: the ones
/// TechEmpower's reference page uses, so a page built with this matches it
/// byte for byte. `&apos;` is HTML5 and XML; an HTML 4 reader would show it
/// literally, which is a cost accepted rather than overlooked. Every other byte
/// is copied as it is, so UTF-8 passes through: all five are ASCII, and no
/// byte of a multi-byte character is below 0x80.
///
/// The same contract as [`khora_float_text`]: when `capacity` is too small,
/// or `into` is null, nothing is written and the length needed is returned,
/// so a caller measures with a null `into`, allocates exactly, and calls
/// again. An answer equal to `len` means there was nothing to escape, so the
/// caller can keep the original rather than copy it.
///
/// # Safety
///
/// `from` must be null with a zero `len`, or address `len` initialized bytes
/// that stay live for the call. `into` must be null, or address `capacity`
/// writable bytes that do not overlap `from`'s.
#[unsafe(no_mangle)]
// SHARE: reads one lent byte buffer and writes another; takes no Khora object.
pub unsafe extern "C" fn khora_html_escape(from: *const u8, len: i64, into: *mut u8, capacity: i64) -> i64 {
    let text: &[u8] = if len <= 0 || from.is_null() {
        &[]
    } else {
        // SAFETY: a positive `len` with a non-null `from` is the caller's
        // guarantee of that many live, initialized bytes.
        unsafe { std::slice::from_raw_parts(from, len as usize) }
    };
    let needed: usize = text.iter().map(|&byte| html_entity(byte).map_or(1, <[u8]>::len)).sum();
    if into.is_null() || capacity < needed as i64 {
        return needed as i64;
    }
    // SAFETY: `into` is non-null and addresses `capacity` writable bytes by
    // the caller's guarantee, `capacity >= needed` was just checked, and the
    // two buffers do not overlap, so a shared and a mutable slice may coexist.
    let out = unsafe { std::slice::from_raw_parts_mut(into, needed) };
    let mut at = 0;
    let mut run = 0;
    for (here, &byte) in text.iter().enumerate() {
        if let Some(entity) = html_entity(byte) {
            let plain = &text[run..here];
            out[at..at + plain.len()].copy_from_slice(plain);
            at += plain.len();
            out[at..at + entity.len()].copy_from_slice(entity);
            at += entity.len();
            run = here + 1;
        }
    }
    let plain = &text[run..];
    out[at..at + plain.len()].copy_from_slice(plain);
    needed as i64
}

/// The entity `byte` becomes in HTML text, or `None` if it stands for itself.
fn html_entity(byte: u8) -> Option<&'static [u8]> {
    match byte {
        b'&' => Some(b"&amp;"),
        b'<' => Some(b"&lt;"),
        b'>' => Some(b"&gt;"),
        b'"' => Some(b"&quot;"),
        b'\'' => Some(b"&apos;"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escaped(text: &str) -> String {
        // SAFETY: a live string's bytes, and a null `into`, which only measures.
        let needed = unsafe { khora_html_escape(text.as_ptr(), text.len() as i64, std::ptr::null_mut(), 0) };
        let mut out = vec![0u8; needed as usize];
        // SAFETY: as above, and `out` is exactly `needed` writable bytes that
        // do not overlap the text.
        let wrote = unsafe { khora_html_escape(text.as_ptr(), text.len() as i64, out.as_mut_ptr(), needed) };
        assert_eq!(wrote, needed);
        String::from_utf8(out).expect("escaping keeps UTF-8")
    }

    #[test]
    fn html_escape_replaces_the_five_and_copies_the_rest() {
        assert_eq!(escaped(""), "");
        assert_eq!(escaped("plain é 日本"), "plain é 日本");
        assert_eq!(escaped("<>&\"'"), "&lt;&gt;&amp;&quot;&apos;");
        assert_eq!(escaped("a<b>c&d\"e'f"), "a&lt;b&gt;c&amp;d&quot;e&apos;f");
    }

    /// **Too little room writes nothing.** A caller that measured, and then
    /// passed a buffer one short, must get the length back and an untouched
    /// buffer, not a partial page.
    #[test]
    fn html_escape_with_too_little_room_writes_nothing() {
        let text = "a<b";
        let mut out = [b'?'; 5];
        // SAFETY: `out` is five writable bytes, one fewer than `a&lt;b` needs.
        let needed = unsafe { khora_html_escape(text.as_ptr(), 3, out.as_mut_ptr(), 5) };
        assert_eq!(needed, 6);
        assert_eq!(&out, b"?????");
    }

    fn float_text(value: f64) -> String {
        let mut out = [0u8; 64];
        // SAFETY: `out` is 64 writable bytes, and its length is what is passed.
        let wrote = unsafe { khora_float_text(value, out.as_mut_ptr(), out.len() as i64) };
        assert!(wrote <= 32, "{value:e} took {wrote} bytes, past `Float::to_string`'s 32");
        String::from_utf8(out[..wrote as usize].to_vec()).expect("ASCII")
    }

    /// **Every double fits `Float::to_string`'s 32 bytes, as the shortest
    /// text that reads back.** Rust's `{}` never writes an exponent, so `1e300`
    /// was 301 digits and `5e-324` 327 characters, and `to_string` answered
    /// `"?"` for both. The cut is JavaScript's `Number.prototype.toString`'s:
    /// plain from `1e-6` up to below `1e21`, an exponent outside.
    #[test]
    fn a_double_is_written_plain_inside_the_ordinary_range_and_with_an_exponent_outside() {
        for (value, text) in [
            (1e300, "1e300"),
            (1.7976931348623157e308, "1.7976931348623157e308"),
            (1e-300, "1e-300"),
            (5e-324, "5e-324"),
            (-5e-324, "-5e-324"),
            (2.2250738585072014e-308, "2.2250738585072014e-308"),
            (1e21, "1e21"),
            (-1e21, "-1e21"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (0.000001, "0.000001"),
            (0.1, "0.1"),
            (1.0, "1"),
            (0.0, "0"),
            (-0.0, "-0"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "NaN"),
        ] {
            assert_eq!(float_text(value), text, "{value:e}");
        }
    }

    /// **Every finite double reads back to the same bits**, through Rust's
    /// correctly rounded parse, which `Float::of_string` binds. 10,000 bit
    /// patterns from a fixed seed, every exponent range in proportion to how
    /// many doubles it holds.
    #[test]
    fn every_double_is_written_in_32_bytes_and_reads_back_exactly() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut checked = 0;
        while checked < 10_000 {
            // xorshift64*: fixed, so a red run names a value that stays red.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let value = f64::from_bits(state.wrapping_mul(0x2545_f491_4f6c_dd1d));
            if !value.is_finite() {
                continue;
            }
            let text = float_text(value);
            let back: f64 = text.parse().unwrap_or_else(|_| panic!("{text} does not parse"));
            assert_eq!(back.to_bits(), value.to_bits(), "{value:e} was written {text}");
            checked += 1;
        }
    }

    /// **The longest text is 25 bytes, as documented**: a negative 17-digit
    /// value just above `1e-6` is written plain, `-0.0000010000000000000002`.
    /// The doc said 24 and named a value that is 24; nothing measured it.
    #[test]
    fn the_longest_text_is_the_one_the_doc_names() {
        let longest = float_text(-1.0000000000000002e-6);
        assert_eq!(longest, "-0.0000010000000000000002");
        assert_eq!(longest.len(), 25);
        assert_eq!(float_text(-2.2250738585072014e-308).len(), 24);
    }

    fn float_parse(text: &str) -> Option<f64> {
        // SAFETY: `text` is `len` readable bytes for the length of the call.
        let value = unsafe { khora_float_parse(text.as_ptr(), text.len() as i64) };
        (!value.is_nan()).then_some(value)
    }

    /// **The grammar `Float::of_string` has always had, no wider**, now that
    /// Rust's parser does the reading. Every row of "refused" is text Rust's
    /// `str::parse::<f64>` takes, and that was refused before this bound it:
    /// a configuration value that read `inf` or `+1` would otherwise become a
    /// number nobody wrote.
    #[test]
    fn a_number_is_read_in_of_strings_grammar_and_no_other() {
        let accepted: &[(&str, f64)] = &[
            ("0", 0.0),
            ("-0", -0.0),
            ("007", 7.0),
            ("1.5", 1.5),
            ("-1.5", -1.5),
            ("1e3", 1000.0),
            ("1E3", 1000.0),
            ("1e+3", 1000.0),
            ("1e-3", 0.001),
            ("1.25e2", 125.0),
            ("1e400", f64::INFINITY),
            ("-1e400", f64::NEG_INFINITY),
            ("1e-400", 0.0),
            ("1.118", 1.118),
            ("5e-324", 5e-324),
            ("1e00000000000000000000003", 1000.0),
        ];
        for &(text, want) in accepted {
            let got = float_parse(text).unwrap_or_else(|| panic!("`{text}` was refused"));
            assert_eq!(got.to_bits(), want.to_bits(), "`{text}` read as {got:e}");
        }
        let refused = [
            "", "-", "+1", ".5", "1.", "-.5", "1.e3", "1e", "1e+", "1e-", "e3", "inf", "-inf", "Infinity",
            "infinity", "INF", "nan", "NaN", "-nan", "1_0", "0x10", " 1", "1 ", "1.2.3", "1e3.5", "--1", "1-",
            "١", "1e3e3",
        ];
        for text in refused {
            assert_eq!(float_parse(text), None, "`{text}` was accepted");
        }
    }

    /// `Float::of_string`'s grammar as its Khora scanner implemented it,
    /// transcribed stage by stage: digits, an optional point and digits, an
    /// optional `e`. Kept as the oracle the fuzz below holds the bound reader
    /// to, so the two cannot drift apart by one of them being edited.
    fn scanner_accepts(bytes: &[u8]) -> bool {
        let digits = |from: usize| {
            let mut at = from;
            while at < bytes.len() && bytes[at].is_ascii_digit() {
                at += 1;
            }
            at
        };
        if bytes.is_empty() {
            return false;
        }
        let start = usize::from(bytes[0] == b'-');
        let whole = digits(start);
        if whole == start {
            return false;
        }
        let mut at = whole;
        if at < bytes.len() && bytes[at] == b'.' {
            let fraction = digits(at + 1);
            if fraction == at + 1 {
                return false;
            }
            at = fraction;
        }
        if at >= bytes.len() {
            return true;
        }
        if bytes[at] != b'e' && bytes[at] != b'E' {
            return false;
        }
        let signed = at + 1;
        let from = if signed < bytes.len() && (bytes[signed] == b'-' || bytes[signed] == b'+') { signed + 1 } else { signed };
        let end = digits(from);
        end == bytes.len() && end != from
    }

    /// **The bound reader refuses exactly what the scanner refused**, on
    /// 200,000 random strings of up to eight characters from the alphabet
    /// that matters: digits, `-`, `+`, `.`, `e`, `E`, `_`, `i`, `n`, `f`,
    /// `a`, a space. A table names the cases somebody thought of; this finds
    /// the ones nobody did.
    #[test]
    fn the_bound_reader_accepts_what_the_scanner_accepted_and_nothing_else() {
        let alphabet = b"0123456789-+.eE_infa ";
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut agreed_accepting = 0;
        for _ in 0..200_000 {
            let length = (next() % 9) as usize;
            let text: Vec<u8> = (0..length).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            let text = String::from_utf8(text).expect("ASCII");
            let accepted = float_parse(&text).is_some();
            assert_eq!(accepted, scanner_accepts(text.as_bytes()), "`{text}`: bound reader accepted = {accepted}");
            agreed_accepting += usize::from(accepted);
        }
        assert!(agreed_accepting > 1_000, "only {agreed_accepting} accepted strings were tried");
    }
}
