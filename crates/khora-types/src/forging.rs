//! A dependency making a capability it was not given.
//!
//! **What this prevents: a signature that lies about what a function
//! touches.** A capability row is the one place a reader can see that a
//! function reads files, takes the time or draws randomness. `FsRead::real()`
//! builds the handler that does it, out of nothing, so a function that calls
//! it needs no row at all: a dependency's `fn parse(text: String) -> Doc` could
//! read `~/.ssh` and its signature would still say it reads nothing. The
//! manifest's grants refuse some of that at run time, and a program that grants
//! `read = ["**"]` refuses none of it.
//!
//! **The rule:** a [`CONSTRUCTORS`] function -- `real()`, and `HttpClient`'s
//! other ways to build a real client -- may be named only in the package being
//! built, in any of its modules, and in `std`, which defines them. A
//! dependency gets a capability from whoever calls it, through a `with` row or
//! a parameter, and so its signature says what it reaches. The name is refused
//! wherever it is written, not only where it is called: `let f = Clock::real`
//! is the same door.
//!
//! **Not tests or benches.** A `test` or `bench` block has no caller and runs
//! only when its own package is the one being tested, so a dependency's test
//! is code the build in hand never runs -- and refusing it would make a package
//! that forges a clock in its own tests impossible to depend on. **Only the
//! block**: a helper function the tests call, in the same file or in a
//! `_test.kh` module, is a function like any other. Nothing stops the
//! package's own code calling it, and a `_test.kh` module is a naming habit,
//! not a rule the build enforces, so exempting either would be a door. The
//! helper takes the capability, and the test makes it.
//!
//! **What it does not deliver:** this is about capability constructors.
//! A dependency that may declare `extern fn` (`[permissions] extern`, which
//! under the default `allow` is every package) can call `getrandom` with no
//! row, and `std::net::socket` and `std::net::tls` dial out with no row;
//! `[permissions] network` is what limits those, at run time. The capability
//! types that are not machine authority -- `Random::seeded`, `Log::plain`,
//! `Tracer::none`, `Scope::root` -- stay open to everyone.
//!
//! **What it costs:** one walk of each body's expressions, in a file that is
//! a dependency's, looking for a two-segment path; the root's files and `std`
//! return before the walk.

use khora_db::{Db, SourceFile};
use khora_hir::body::Expr;
use khora_hir::{HirError, ModulePath, Resolution};
use khora_syntax::ast::AstNode;

/// A std effect whose handler reaches the machine, and the functions that
/// build one.
#[derive(Debug, Clone, Copy)]
pub struct Constructor {
    /// The module that declares the effect, as a program spells it.
    pub module: &'static str,
    /// The effect.
    pub effect: &'static str,
    /// The label to advise for it, the one the capabilities page's table
    /// gives: `std`'s own signatures use it, and `SecureRandom`, which no std
    /// signature asks for, has one that cannot collide with `Random`'s.
    pub label: &'static str,
    /// The functions of `impl Effect` that build a real handler.
    pub functions: &'static [&'static str],
}

/// Every capability a dependency may not build for itself.
///
/// **One table, so a new capability is one line here.** A std effect with a
/// public constructor that is not listed is refused by a test
/// (`khora-types/tests/forging.rs`, `every_constructor_in_std_is_decided`),
/// which makes the author decide which side it is on rather than leaving it
/// open by default.
pub const CONSTRUCTORS: [Constructor; 8] = [
    Constructor { module: "std::clock", effect: "Clock", label: "clock", functions: &["real"] },
    // `nonces`, not `random`: that is `Random`'s, and a function that needs
    // both -- `postgres`, which jitters its reconnects and signs a SCRAM
    // nonce -- cannot write one label twice in a row.
    Constructor { module: "std::crypto", effect: "SecureRandom", label: "nonces", functions: &["real"] },
    Constructor { module: "std::random", effect: "Random", label: "random", functions: &["real"] },
    Constructor { module: "std::env", effect: "Env", label: "env", functions: &["real"] },
    Constructor { module: "std::fs", effect: "FsRead", label: "reads", functions: &["real"] },
    Constructor { module: "std::fs", effect: "FsWrite", label: "writes", functions: &["real"] },
    Constructor { module: "std::process", effect: "Process", label: "process", functions: &["real"] },
    // **Every one of them, not only `real`.** `within` and the two `bounded`
    // forms differ from `real` in a deadline and a size, and each is a client
    // that connects; refusing only `real` would leave `HttpClient::within(1)`
    // as the same door with a different name on it.
    Constructor {
        module: "std::net::http",
        effect: "HttpClient",
        label: "client",
        functions: &["real", "within", "bounded", "bounded_within"],
    },
];

/// A `std` module that is half of another, which only `std` may name.
#[derive(Debug, Clone, Copy)]
pub struct Internal {
    /// The module, as a program would spell it.
    pub module: &'static str,
    /// The module a program uses instead, for the advice.
    pub instead: &'static str,
}

/// The modules of `std` that exist to be imported by `std` alone.
///
/// **What this prevents: a capability's checks, skipped by calling what is
/// underneath them.** A module that is one target's half of another --
/// `process_posix.kh` and `process_windows.kh` are `std::process::shell` --
/// has to be `pub` for the other half to import it, and the language has no
/// visibility narrower than that. So `std::process::shell::run` was a shell
/// with no `Process` row and no `[permissions] process` check, open to any
/// package, the root included. Naming one of these anywhere but `std` is an
/// error.
///
/// Found by looking for modules declared by more than one file, each selected
/// for a target, and `khora-types/tests/forging.rs` holds every such module to
/// a decision. `std::permissions::separator` reaches nothing -- it answers
/// whether `\` separates paths here -- and is on the list because it is the
/// same kind of thing, half of `std::permissions`, which a program should not
/// come to depend on. `std::net::socket` is declared once per target too and
/// is not here: it is the module a program uses, the raw network, which
/// postgres dials through and `[permissions] network` governs.
pub const STD_INTERNAL: [Internal; 2] = [
    Internal { module: "std::process::shell", instead: "`std::process` with a `Process` capability" },
    Internal { module: "std::permissions::separator", instead: "`std::permissions`, which asks it for this target" },
];

/// The internal module `module` is, or is inside.
fn internal(module: &str) -> Option<&'static Internal> {
    STD_INTERNAL
        .iter()
        .find(|i| module == i.module || module.strip_prefix(i.module).is_some_and(|rest| rest.starts_with("::")))
}

/// Whether `file` is one of the standard library's own.
///
/// **By where it is on disk, not by its `module` line**, which any package
/// may set to `std::..`, when a driver resolved the program: `khora check`,
/// every command that builds, and the editor all read `std` from
/// [`khora_db::standard_library`], so a file under that directory is `std`
/// and a file elsewhere is not, whatever it calls itself.
///
/// **By the `module` line when nothing was resolved** -- no
/// [`khora_db::DependencyFiles`] declared, as for a test harness that copies
/// `std`'s files into a scratch directory and hands them over itself. There
/// the path says nothing about which files are `std`, and the harness wrote
/// every file it compiles. A driver that runs untrusted code declares.
///
/// **Fails closed when something was resolved.** A file whose real path
/// cannot be worked out is not `std`'s, and with no standard library found
/// nothing is; see [`really_under`].
///
/// Costs a `canonicalize` of the file's path each time it is asked. The
/// internal-module rule asks only for a file that names an internal module;
/// the trap rules ask twice for every file in the compilation (whether it may
/// call `contain_with`, and whether its bodiless functions may be
/// intrinsics). Measured on `khora check` over `examples/` and `std`: within
/// run-to-run noise, so it is not cached.
pub(crate) fn is_standard_library(db: &dyn Db, file: SourceFile) -> bool {
    let by_module_line = || crate::imports::package_of(db, file) == "std";
    if khora_db::dependency_files(db).is_none() {
        return by_module_line();
    }
    static STD: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let std_dir = STD.get_or_init(|| khora_db::standard_library().and_then(|d| d.canonicalize().ok()));
    really_under(std_dir.as_deref(), file.path(db))
}

/// Whether `path`, with every link followed and every `..` gone, is inside
/// `dir`, itself already canonical.
///
/// **What this prevents: a spelling being taken for a place.**
/// `Path::starts_with` compares components and keeps `..`, so
/// `<std>/../../elsewhere` starts with `<std>`. A path that cannot be
/// canonicalized -- one whose real path is longer than `PATH_MAX`, reached
/// through short links, still opens -- is therefore **not** inside, rather
/// than compared as written. No directory (the standard library was not
/// found, or its own path could not be canonicalized) has nothing inside it.
fn really_under(dir: Option<&std::path::Path>, path: &std::path::Path) -> bool {
    let Some(dir) = dir else { return false };
    path.canonicalize().is_ok_and(|real| real.starts_with(dir))
}

/// Each place in `file` that names a [`STD_INTERNAL`] module, unless `file`
/// is `std`'s own: an import of it, or a qualified path into it.
pub(crate) fn internal_module_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let refusal = |module: &str, made: &Internal| {
        format!(
            "`{module}` is part of `std`'s own implementation, and only `std` may name it; use {} \
             instead",
            made.instead
        )
    };
    let mut found = Vec::new();
    let mut imported: Vec<&'static str> = Vec::new();
    for import in khora_db::parse(db, file).source_file().imports() {
        let Some(path) = import.path() else { continue };
        let Some(made) = internal(&path.text_path()) else { continue };
        imported.push(made.module);
        found.push(HirError { message: refusal(made.module, made), range: import.syntax().text_range() });
    }
    // A qualified path, `std::process::shell::run(..)`, which needs no import.
    // A name the file imported resolves to the module as well, and its import
    // has been told already: once per module is the useful count.
    let every = khora_hir::body::bodies(db, file).iter().chain(khora_hir::body::const_bodies(db, file));
    for (_, body) in every {
        for (id, expr) in body.exprs() {
            let module = match expr {
                Expr::Path(Resolution::Item { module, .. } | Resolution::Variant { module, .. }) => module,
                _ => continue,
            };
            let Some(made) = internal(&module.segments().join("::")) else { continue };
            if imported.contains(&made.module) {
                continue;
            }
            found.push(HirError { message: refusal(made.module, made), range: body.range(id) });
        }
    }
    // Asked last, and only when there is something to excuse: it touches the
    // file system.
    if found.is_empty() || is_standard_library(db, file) {
        return Vec::new();
    }
    found
}

/// The capability `module::effect::function` builds, if it builds one.
pub fn constructor(module: &ModulePath, effect: &str, function: &str) -> Option<&'static Constructor> {
    let written = module.segments().join("::");
    CONSTRUCTORS
        .iter()
        .find(|c| c.module == written && c.effect == effect && c.functions.contains(&function))
}

/// Whether code in `file` may build a real capability.
///
/// **The one place the answer lives**, so the plugin target can change it in
/// one: there, a plugin is a package being built and nothing it writes may
/// reach the machine except through its `pub extern fn` exports' arguments,
/// so this is where that target's exports become the entry points and the rest
/// of the plugin a dependency.
///
/// The answer for a native build: everything but a dependency's files. Which
/// files are a dependency's is the package resolver's to say, through
/// [`khora_db::DependencyFiles`], because a dependency picks its own module
/// path -- `module std::..` and `module app::..` are both open to it -- and so
/// the module path cannot be what decides. A driver that resolved nothing
/// falls back on the imports: the packages nothing else imports are the ones
/// being built, and `std` defines the constructors.
pub fn may_construct_capabilities(db: &dyn Db, file: SourceFile) -> bool {
    if let Some(declared) = khora_db::dependency_files(db) {
        return !declared.files(db).contains(&file);
    }
    let Some(root) = khora_db::source_root(db) else { return true };
    let package = crate::imports::package_of(db, file);
    package == "std" || crate::imports::root_packages(db, root).contains(&package)
}

/// Each place in `file` that names a capability's constructor it may not.
pub(crate) fn forged_capability_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    if may_construct_capabilities(db, file) {
        return Vec::new();
    }
    let homes = crate::type_homes(db, file);
    let package = crate::imports::package_of(db, file);
    let mut found = Vec::new();
    let every = khora_hir::body::bodies(db, file).iter().chain(khora_hir::body::const_bodies(db, file));
    for (name, body) in every {
        if khora_hir::is_test(name) {
            continue;
        }
        for (id, expr) in body.exprs() {
            // An alias, `import std::clock::{Clock as C}`, is still the
            // effect: `homes` maps the local spelling to the declaration, so
            // `C::real` is found and a type of the dependency's own that is
            // also called `Clock` is not.
            let Expr::Path(Resolution::TraitItem { owner, name: function }) = expr else { continue };
            let Some((module, effect)) = homes.of(owner) else { continue };
            let Some(made) = constructor(&module, &effect, function) else { continue };
            found.push(HirError {
                message: format!(
                    "`{owner}::{function}` makes a real `{effect}`, and only the package being built \
                     may make one; `{package}` is a dependency here. Take it from the caller \
                     instead, so this function's signature says it uses it: `with {{ {label}: \
                     {effect} }}` after the return type, or `{label}: {effect}` among the \
                     parameters. Only code inside a `test` block is exempt; a helper the tests \
                     call takes it the same way",
                    label = made.label,
                ),
                range: body.range(id),
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::really_under;

    /// A spelling that leaves the directory with `..` is outside it, whether
    /// or not anything is there: one that exists canonicalizes to its real
    /// place, and one that does not cannot be canonicalized and is outside
    /// every directory. The second stands in for the `PATH_MAX` case, which
    /// fails `canonicalize` the same way while still starting, as written,
    /// with `dir`.
    #[test]
    fn a_path_is_under_a_directory_only_by_where_it_really_is() {
        // Both directories are made here, so the test writes nowhere it does
        // not own: a CI runner's temporary directory is `/tmp`, whose parent
        // is not writable.
        let base = std::env::temp_dir().join(format!("khora_really_under_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("std");
        std::fs::create_dir_all(&dir).expect("the directory");
        let base = base.canonicalize().expect("its real path");
        let dir = base.join("std");
        let inside = dir.join("really_under_probe.kh");
        std::fs::write(&inside, "").expect("a file inside");
        assert!(really_under(Some(&dir), &inside));
        std::fs::write(base.join("really_under_outside.kh"), "").expect("a file beside the directory");
        assert!(!really_under(Some(&dir), &dir.join("..").join("really_under_outside.kh")));
        assert!(!really_under(Some(&dir), &dir.join("..").join("really_under_no_such_file.kh")));
        assert!(!really_under(None, &inside));
        let _ = std::fs::remove_dir_all(&base);
    }
}