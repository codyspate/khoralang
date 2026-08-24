//! Where a program stops because it was wrong.
//!
//! Arithmetic that overflowed and an index outside its array. Both are the
//! same decision, taken in `docs/design/numbers.md`: a program that runs off
//! the end of its own array is wrong, and continuing with whatever was next in
//! memory is the least useful possible response.

use std::io::Write;

/// Reports arithmetic that did not fit, and stops.
///
/// Overflow traps in every build. A program that passes its tests and then
/// wraps in production is the failure worth spending a branch to prevent, and
/// two behaviours — one for testing, one for shipping — put the difference
/// exactly where it is most expensive to find. `docs/roadmap.md` 6.2.
///
/// `Int::wrapping_add` and its siblings are how you ask for the other thing,
/// in the places that genuinely want it: a hash, a checksum, a PRNG.
///
/// # Safety
///
/// `what` must point at `len` bytes naming the operation — generated code
/// passes a string literal in `.rodata`, live for the program's whole run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_overflow(what: *const u8, len: usize) -> ! {
    let bytes = if len == 0 { &[][..] } else {
        // SAFETY: the caller passes a string literal in `.rodata`, live for the
        // program's whole run.
        unsafe { std::slice::from_raw_parts(what, len) }
    };
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "khora: {} overflowed", String::from_utf8_lossy(bytes));
    let _ = std::io::stdout().flush();
    std::process::exit(134)
}

/// Reports an index that was not in range, and stops.
///
/// A trap rather than a wrapped value or a poisoned read, for the same reason
/// integer overflow traps: a program that reads past its own array is wrong,
/// and the useful thing to do is say where.
#[unsafe(no_mangle)]
pub extern "C" fn khora_bounds_fail(index: i64, len: i64) -> ! {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "khora: index {index} is outside an array of {len}");
    let _ = std::io::stdout().flush();
    std::process::exit(134)
}
