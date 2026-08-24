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
    hay_len: usize,
    needle: *const u8,
    needle_len: usize,
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
            std::slice::from_raw_parts(hay, hay_len),
            std::slice::from_raw_parts(needle, needle_len),
        )
    };
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
    a_len: usize,
    b: *const u8,
    b_len: usize,
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
    unsafe { std::slice::from_raw_parts(a, a_len) == std::slice::from_raw_parts(b, b_len) }
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
