//! Native backend.
//!
//! Not implemented yet, and the `llvm` feature is off by default because
//! `inkwell` needs a matching LLVM installation that this workspace does not
//! assume. Planned shape:
//!
//! - Lower reference-counted HIR to LLVM IR via `inkwell`.
//! - C FFI shims for the BLAS / GGML kernels behind `std.ai`.
//! - Link with `lld` to a static executable
//!   (`x86_64-unknown-linux-musl`, `aarch64-apple-darwin`).
