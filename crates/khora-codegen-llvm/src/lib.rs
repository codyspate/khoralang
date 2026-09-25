//! Native backend: HIR plus a reference-counting plan, to LLVM IR, to an
//! executable.
//!
//! [`compile`] takes a file that has already been parsed, lowered and checked
//! by the crates upstream, and produces a linked native binary.
//!
//! # What runs, and in what order
//!
//! 1. `khora_types::diagnostics` — if the program does not check, nothing is
//!    emitted. Code generation assumes a well-typed program everywhere and
//!    would otherwise turn a type error into a miscompilation.
//! 2. `khora_hir::body::bodies` and `khora_perceus::rc_plans` — the IR and the
//!    `dup`/`drop` placement, walked together.
//! 3. One LLVM module, verified, written as an object file.
//! 4. `clang` from the pinned toolchain links it with `khora-rt`.
//!
//! # Optional dependency
//!
//! LLVM is behind the `llvm` feature: building without it needs no LLVM
//! installation at all, which keeps `cargo test` green for anyone working on
//! the front end. Only [`toolchain`] is unconditional, because it is just path
//! arithmetic. See `docs/llvm-setup.md`.

#![deny(missing_docs)]

pub mod toolchain;

#[cfg(feature = "llvm")]
pub mod spike;

#[cfg(feature = "llvm")]
mod backend;
// Gated like the rest of it. `mod debug` went in unconditional and nothing
// noticed, because every check that runs here passes `--features llvm` — the
// front-end build the feature exists to keep working is the one it broke.
#[cfg(feature = "llvm")]
mod debug;
#[cfg(feature = "llvm")]
mod lower;
#[cfg(feature = "llvm")]
mod runtime;
// Gated with its only consumer. `backend` is the whole of what reads a phase
// timer and is `llvm`-only, so without the feature this module compiled with
// nothing reaching it -- five dead-code warnings, which `-D warnings` turns
// into a failed build. Nobody saw it locally because every local build passes
// `--features llvm`; the no-backend configuration is a thing only CI builds.
#[cfg(feature = "llvm")]
mod timings;

/// Whether small values are laid out flat rather than behind a header.
///
/// **On unless `KHORA_UNBOXED=0` says otherwise**, and the switch is here
/// rather than read at each use so that the compiler and the build cache
/// cannot disagree about it -- a cache keyed on a different answer from the
/// one the compiler used hands back an artifact built the other way, which
/// `docs/errata.md` 85 is the cost of.
///
/// Anything but `0` is on, including the `1` that used to be how it was turned
/// on, so a script that sets it keeps working.
pub fn unboxing_enabled() -> bool {
    !matches!(std::env::var("KHORA_UNBOXED").as_deref(), Ok("0"))
}

thread_local! {
    static PLAIN_COUNTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// **For tests only: count references without atomics in a program that
/// spawns.** Programs built this way are unsound and corrupt their own heap.
///
/// It exists so that the crossings fixture can be shown to fail. That
/// fixture sends values to other fibers by every route the runtime has, and
/// it passes when their counts are atomic. A fixture that also passes with
/// plain counts could not catch a crossing that was missed. So the
/// fixture's own test builds it this way and requires it to go wrong.
///
/// Per thread rather than global. A test binary compiles on several threads
/// at once, and a global switch would give an unrelated test plain counts.
/// There is no environment variable for it, so no user can set it by
/// accident.
#[doc(hidden)]
pub fn force_plain_counts_on_this_thread(plain: bool) {
    PLAIN_COUNTS.with(|p| p.set(plain));
}

/// Whether [`force_plain_counts_on_this_thread`] is on for this thread.
#[cfg(feature = "llvm")]
pub(crate) fn plain_counts_forced() -> bool {
    PLAIN_COUNTS.with(|p| p.get())
}

#[cfg(feature = "llvm")]
pub use backend::{
    compile, compile_benches, compile_library, compile_library_with, compile_tests, compile_with,
    verify_for_target,
};
pub use toolchain::Profile;
pub use toolchain::{set_natives, Natives};
