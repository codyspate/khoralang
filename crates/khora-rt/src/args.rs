//! The command line, as the process received it.

use std::ffi::CString;
use std::sync::OnceLock;

/// What the process was invoked with, converted once and owned here.
///
/// **The one thing a program cannot ask the operating system for.** Every other
/// piece of its environment has a function — `getenv`, `time`, `fopen` — and
/// the arguments arrive once, through `main`, and are gone. So the generated
/// `main` hands them here before it does anything else, and this holds them for
/// whatever asks later.
///
/// **Owned rather than borrowed from `argv`, because `argv` is not UTF-8.** On
/// Windows the `argv` a C `main` receives is the command line in the machine's
/// ANSI code page, so `khora run app -- café` handed Khora bytes that are not a
/// `String` — and `String::from_bytes` traps on those, taking the process down
/// with `these bytes are not UTF-8, so they are not a String` before the
/// program's first line. `arguments()` has no failure row and so no check a
/// caller could make first: every Khora command-line program was one accented
/// character away from a 134. Storing converted copies is what makes the
/// operation total, which is what its type already claims.
static ARGS: OnceLock<Vec<CString>> = OnceLock::new();

/// Records `argc` and `argv`. Called by generated code, first thing in `main`.
///
/// # Safety
///
/// `argv` must be the `main` argument of the same name: `argc` pointers to
/// NUL-terminated strings, live for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_args_set(argc: i32, argv: *const *const u8) {
    // SAFETY: the caller's contract.
    let _ = ARGS.set(unsafe { collect(argc, argv) });
}

/// The arguments as text, asked for the way Windows has to be asked.
///
/// **The wide command line, not the one the parameters describe.**
/// `GetCommandLineW` — what `args_os` reads — is the line as it was actually
/// typed, in UTF-16. The `argv` beside it is that same line squeezed through
/// the machine's ANSI code page, which has no room for most of Unicode, so the
/// parameters are ignored here. The conversion is exact for anything a person
/// can type.
///
/// # Safety
///
/// As [`khora_args_set`]; nothing here reads the pointer.
#[cfg(windows)]
unsafe fn collect(_argc: i32, _argv: *const *const u8) -> Vec<CString> {
    std::env::args_os().map(|arg| owned(&arg.to_string_lossy())).collect()
}

/// The arguments as text, from the vector `main` was handed.
///
/// Nothing promises those bytes are UTF-8 either, so they are converted the
/// same way — lossily, and deliberately. An argument with a stray byte in it
/// becomes a replacement character rather than the end of the process, and a
/// program that cares can look for one.
///
/// # Safety
///
/// As [`khora_args_set`].
#[cfg(not(windows))]
unsafe fn collect(argc: i32, argv: *const *const u8) -> Vec<CString> {
    if argv.is_null() {
        return Vec::new();
    }
    (0..argc.max(0) as usize)
        .map(|index| {
            // SAFETY: the caller's contract — `argc` live pointers, each to a
            // NUL-terminated string.
            let text = unsafe { std::ffi::CStr::from_ptr(argv.add(index).read().cast()) };
            owned(&String::from_utf8_lossy(text.to_bytes()))
        })
        .collect()
}

/// One argument, as bytes Khora can read.
///
/// An interior NUL cannot come from a command line — every form of it is
/// NUL-terminated — so the error case is unreachable, and truncating at the NUL
/// is what a C caller would have seen anyway.
fn owned(text: &str) -> CString {
    CString::new(text).unwrap_or_else(|bad| {
        let bytes = bad.into_vec();
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        CString::new(&bytes[..end]).expect("no NUL before the first NUL")
    })
}

/// How many arguments there were, the program's own name included.
#[unsafe(no_mangle)]
pub extern "C" fn khora_arg_count() -> i64 {
    ARGS.get().map_or(0, |args| args.len() as i64)
}

/// The `index`th argument, as a pointer to its NUL-terminated bytes.
///
/// Null when there is no such argument. What Khora does with it is read its
/// length with `strlen` and copy it with `memcpy`, both of which are ISO C —
/// so the only thing that had to be added here is the part C has no function
/// for. The bytes live as long as the process, which is what lets a pointer
/// cross: `docs/design/ffi.md` §1 wants scalars and pointers and nothing else.
#[unsafe(no_mangle)]
pub extern "C" fn khora_arg(index: i64) -> *const u8 {
    let Some(args) = ARGS.get() else { return std::ptr::null() };
    if index < 0 {
        return std::ptr::null();
    }
    match args.get(index as usize) {
        Some(arg) => arg.as_ptr().cast(),
        None => std::ptr::null(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever the platform hands over comes back as something Khora can read.
    ///
    /// The count is not asserted: this runs inside a test binary whose own
    /// command line is the one being read, and `cargo nextest` supplies
    /// arguments of its own. What matters is that every one of them is text,
    /// because the alternative used to be a trap.
    #[test]
    fn every_argument_is_utf_8_by_the_time_khora_sees_it() {
        // SAFETY: a well-formed empty vector; on Windows the parameters are
        // ignored, and elsewhere a count of nought reads nothing.
        unsafe { khora_args_set(0, std::ptr::null()) };

        for index in 0..khora_arg_count() {
            let pointer = khora_arg(index);
            assert!(!pointer.is_null(), "argument {index} is inside the count");
            // SAFETY: `khora_arg` returns a pointer into a `CString` this
            // module owns for the life of the process.
            let text = unsafe { std::ffi::CStr::from_ptr(pointer.cast()) };
            assert!(
                std::str::from_utf8(text.to_bytes()).is_ok(),
                "argument {index} is not text: {:?}",
                text.to_bytes()
            );
        }
    }

    /// Past the end is null rather than anything else.
    #[test]
    fn an_index_outside_the_vector_is_null() {
        // SAFETY: as above.
        unsafe { khora_args_set(0, std::ptr::null()) };
        assert!(khora_arg(-1).is_null(), "before the start");
        assert!(khora_arg(khora_arg_count()).is_null(), "one past the end");
        assert!(khora_arg(i64::MAX).is_null(), "far past the end");
    }
}
