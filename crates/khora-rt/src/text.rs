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

/// Writes `value` into `into` as text, and says how many bytes that took.
///
/// The shortest form that reads back as the same number, which is Rust's
/// `{}` and is what a reader means by "the number". Khora cannot produce it
/// itself: shortest-round-trip formatting is Ryū or Grisu, a table and a
/// thousand lines, and it is exactly the kind of thing
/// `docs/design/ecosystem.md` says to bind rather than write twice.
///
/// Returns the length needed when `capacity` is too small and writes nothing,
/// so a caller can size a buffer in two calls rather than guessing. 32 bytes
/// is always enough for an `f64`, which is why nobody will make the second
/// call.
///
/// # Safety
///
/// `into` must address `capacity` writable bytes, or be null with a zero
/// `capacity`.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_float_text(value: f64, into: *mut u8, capacity: i64) -> i64 {
    let text = format!("{value}");
    let bytes = text.as_bytes();
    if capacity < bytes.len() as i64 || into.is_null() {
        return bytes.len() as i64;
    }
    // SAFETY: the contract above, and the length was just checked against it.
    unsafe { into.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
    bytes.len() as i64
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
}
