//! Native backend: HIR plus a reference-counting plan, to LLVM IR, to an
//! executable.
//!
//! Roadmap phase 2.5, and the last stage of the vertical slice. [`compile`]
//! takes a file that has already been parsed, lowered and checked by the
//! crates upstream and produces a linked native binary.
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

pub mod toolchain;

#[cfg(feature = "llvm")]
pub mod spike;

#[cfg(feature = "llvm")]
mod backend;
#[cfg(feature = "llvm")]
mod lower;
#[cfg(feature = "llvm")]
mod runtime;

#[cfg(feature = "llvm")]
pub use backend::{compile, compile_tests, verify_for_target};
