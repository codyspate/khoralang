//! Native backend.
//!
//! Real code generation is not implemented yet — see `docs/roadmap.md` Phase 2.
//! What exists today is the toolchain plumbing and a spike that proves the
//! whole emit-link-run path works on this host, so Phase 2 can assume it.
//!
//! LLVM is an optional dependency: building without `--features llvm` needs no
//! LLVM installation at all, which keeps `cargo test` green for anyone working
//! on the front end. See `docs/llvm-setup.md`.

pub mod toolchain;

#[cfg(feature = "llvm")]
pub mod spike;
