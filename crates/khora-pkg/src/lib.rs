//! Dependency resolution, the lockfile, the package store and the task runner.
//!
//! What turns a directory of `.kh` files into a package somebody else can
//! depend on. Roadmap phase 10.2.
//!
//! The shape is conventional on purpose — a manifest names dependencies, a
//! resolver turns those into exact sources, a lockfile records what it decided,
//! and a content-addressed store holds the results. Two things are worth
//! knowing before reading further:
//!
//! - **There is no version solver, because there is nothing to solve.** Every
//!   source names one exact thing: a commit id, or a directory. A registry is
//!   where versions and therefore solving arrive, and `resolve` is where that
//!   goes.
//! - **A git package is pinned twice**, to a commit id and to the SHA-256 of
//!   what that commit produced. The second is not redundant with the first;
//!   `lock` explains why.
//!
//! Nothing here compiles anything. It answers "which directories", and the
//! compiler is handed the answer.

mod fetch;
mod hash;
mod lock;
mod resolve;
mod source;
mod store;
pub mod tasks;

pub use hash::{tree as hash_tree, ContentHash};
pub use lock::{Lockfile, LockedPackage, FORMAT_VERSION, LOCKFILE};
pub use resolve::{resolve, Resolution, Resolved};
pub use source::Source;
pub use store::Store;
