//! Writing to standard output.
//!
//! One function per shape rather than one that formats, because the code
//! generator knows the static type at the call site and a runtime that has to
//! ask is a runtime doing the type checker's job again.

use super::*;
use std::io::Write;

//
// Each of these takes the lock once and emits the value and its newline
// together, so two prints cannot interleave halfway through a line. Rust's
// stdout is line buffered, so the trailing newline also flushes: generated
// programs exit through a C `main` without running Rust's shutdown, and
// anything still sitting in a buffer at that point would simply be lost.
//
// Write errors are discarded. A closed or full stdout is not a condition a
// Khora program can react to, and turning it into an abort would make `print`
// the most dangerous statement in the language.

/// Prints an `Int` and a newline to stdout.
#[unsafe(no_mangle)]
pub extern "C" fn khora_print_int(value: i64) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
}

/// Prints a `Bool` and a newline to stdout, spelled as Khora spells it.
///
/// Takes a C `_Bool`, so generated code must pass a byte that is exactly 0 or
/// 1 — see the module documentation.
#[unsafe(no_mangle)]
pub extern "C" fn khora_print_bool(value: bool) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(if value { b"true\n" } else { b"false\n" });
}

/// Prints `len` bytes from `ptr`, then a newline, to stdout.
///
/// Takes a raw pointer and a length rather than a heap object, so the code
/// generator can pass a string literal in `.rodata` directly, without boxing
/// it. The bytes are written through unvalidated: Khora `String`s are UTF-8 by
/// construction, so validating here would charge every print for a property the
/// type system already guarantees.
///
/// # Safety
///
/// If `len` is non-zero, `ptr` must point at `len` initialized bytes that stay
/// valid and unmodified for the duration of the call. A zero `len` ignores
/// `ptr` entirely, so the empty string may be passed as null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_print_str(ptr: *const u8, len: usize) {
    let mut out = std::io::stdout().lock();
    if len != 0 {
        if ptr.is_null() {
            fatal("print of a null string with a non-zero length");
        }
        // SAFETY: the caller guarantees `len` initialized bytes at `ptr`, live
        // and unmodified for this call, and `len` is far below `isize::MAX`
        // because it came from an allocation.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let _ = out.write_all(bytes);
    }
    let _ = out.write_all(b"\n");
}

/// Writes a float and a newline.
///
/// Rust's shortest round-tripping form, which is what a reader wants: `0.1`
/// prints as `0.1` rather than as the seventeen digits that are literally
/// stored, and any two distinct doubles still print differently.
#[unsafe(no_mangle)]
pub extern "C" fn khora_print_float(value: f64) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
}
