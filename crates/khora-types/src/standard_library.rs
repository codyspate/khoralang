//! Whether a file is one of the standard library's own.
//!
//! **What this prevents: two "only the standard library may" rules
//! disagreeing about what the standard library is.** `contain_with`, and the
//! intrinsics code generation lowers by name (`attempt`, `within_deadline`,
//! `contain_run`), are sound only when the standard library calls or
//! declares them. Each of those rules asks this one function, and it asks
//! the package resolver: [`crate::forging::is_standard_library`], the same
//! answer that keeps `std`'s internal modules to `std`.
//!
//! **By where the file is on disk, not by its `module` line.** A package
//! that names a module `std::anything`, or a dependency that calls itself
//! `std`, is not the standard library here, since its files are not under
//! the toolchain's `std` directory. `khora check`, every command that builds,
//! and the editor declare what they resolved, so this is the answer a
//! program meets. A caller that resolved nothing (a test harness handing
//! over `std`'s files itself) gets forging's fallback, the `module` line.

use khora_db::{Db, SourceFile};

/// Whether `file` is one of the standard library's own files.
///
/// See the module documentation for how it is answered.
pub(crate) fn is_std_file(db: &dyn Db, file: SourceFile) -> bool {
    crate::forging::is_standard_library(db, file)
}
