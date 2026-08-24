//! The command line, as the process received it.

use crate::counters::COUNTER_ORDER;
use std::sync::atomic::AtomicUsize;

/// What the process was invoked with, as the C runtime handed it over.
///
/// **The one thing a program cannot ask the operating system for.** Every other
/// piece of its environment has a function — `getenv`, `time`, `fopen` — and
/// the arguments arrive once, through `main`, and are gone. So the generated
/// `main` hands them here before it does anything else, and this holds them for
/// whatever asks later.
///
/// `Relaxed` because they are written once, before any Khora code runs, and
/// only read afterwards; there is no ordering for anything else to depend on.
static ARG_COUNT: AtomicUsize = AtomicUsize::new(0);
static ARG_VECTOR: AtomicUsize = AtomicUsize::new(0);

/// Records `argc` and `argv`. Called by generated code, first thing in `main`.
///
/// # Safety
///
/// `argv` must be the `main` argument of the same name: `argc` pointers to
/// NUL-terminated strings, live for the process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_args_set(argc: i32, argv: *const *const u8) {
    ARG_COUNT.store(argc.max(0) as usize, COUNTER_ORDER);
    ARG_VECTOR.store(argv as usize, COUNTER_ORDER);
}

/// How many arguments there were, the program's own name included.
#[unsafe(no_mangle)]
pub extern "C" fn khora_arg_count() -> i64 {
    ARG_COUNT.load(COUNTER_ORDER) as i64
}

/// The `index`th argument, as a pointer to its NUL-terminated bytes.
///
/// Null when there is no such argument. What Khora does with it is read its
/// length with `strlen` and copy it with `memcpy`, both of which are ISO C —
/// so the only thing that had to be added here is the part C has no function
/// for.
#[unsafe(no_mangle)]
pub extern "C" fn khora_arg(index: i64) -> *const u8 {
    if index < 0 || index >= khora_arg_count() {
        return std::ptr::null();
    }
    let argv = ARG_VECTOR.load(COUNTER_ORDER) as *const *const u8;
    if argv.is_null() {
        return std::ptr::null();
    }
    // SAFETY: `argv` came from `main` per `khora_args_set`'s contract, and the
    // index was just checked against the count recorded with it.
    unsafe { *argv.add(index as usize) }
}
