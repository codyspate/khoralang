//! Perceus reference counting and in-place reuse.
//!
//! Not implemented yet. Planned shape:
//!
//! - Insert precise `dup` / `drop` at lexical scope boundaries over a linear
//!   HIR, so no tracing collector is needed.
//! - Reuse analysis: when a variant is dropped and another of the same shape is
//!   allocated on the same path, fuse `drop` + `alloc` into `reuse`. This is
//!   what makes a pure `match`-and-rebuild loop run in place (FBIP).
