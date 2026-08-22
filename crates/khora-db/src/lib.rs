//! The incremental query database.
//!
//! Every compiler pass downstream of parsing is a salsa query hanging off this
//! crate. That is a deliberate structural choice, recorded as decision A3 in
//! `docs/roadmap.md`: retrofitting incrementality means rewriting every pass's
//! signature and ownership model, and §6.5 wants sub-15 ms LSP responses, which
//! is not something that can be bolted on afterwards.
//!
//! `khora-syntax` stays salsa-free. It is a pure function from text to tree,
//! and this crate is what makes it incremental. Keeping that boundary means the
//! parser can be tested, fuzzed and reused without dragging a database along.
//!
//! ```
//! use khora_db::{Db, KhoraDatabase, SourceFile, parse};
//!
//! let db = KhoraDatabase::new();
//! let file = SourceFile::new(&db, "app.kh".into(), "module app;".to_string());
//! assert!(parse(&db, file).errors().is_empty());
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use salsa::Setter;

/// A source file, and the identity of one.
///
/// Salsa inputs are `Copy` handles into the database, so this doubles as the
/// `FileId` the rest of the compiler passes around. The text lives in the
/// database, not in the handle.
#[salsa::input(debug)]
pub struct SourceFile {
    #[returns(ref)]
    pub path: PathBuf,
    #[returns(deref)]
    pub text: String,
}

/// The set of files that make up a compilation.
///
/// Module and import resolution read this rather than touching the filesystem,
/// so that adding or removing a file is an ordinary input change and
/// invalidates exactly the queries that depended on the file set.
///
/// A **singleton**: there is one compilation per database, and resolving an
/// import from inside a per-file query needs the file set without every query
/// between here and there growing a parameter to carry it. `SourceRoot::get`
/// answers with the one that was created.
#[salsa::input(debug, singleton)]
pub struct SourceRoot {
    #[returns(deref)]
    pub files: Vec<SourceFile>,
}

/// The compilation's file set, or an empty one if nothing declared it.
///
/// Tests that check a single file in isolation never build a root, and asking
/// them to would be noise; they simply see no other modules.
pub fn source_root(db: &dyn Db) -> Option<SourceRoot> {
    SourceRoot::try_get(db)
}

/// The database interface queries are written against.
///
/// Queries take `&dyn Db` rather than the concrete database so that tests and
/// the eventual language server can supply their own implementations.
#[salsa::db]
pub trait Db: salsa::Database {}

/// Parses one file.
///
/// Returned by reference: a `Parse` owns a whole green tree, and callers
/// overwhelmingly want to walk it rather than own it.
#[salsa::tracked(returns(ref))]
pub fn parse(db: &dyn Db, file: SourceFile) -> khora_syntax::Parse {
    khora_syntax::parse(file.text(db))
}

/// Records which queries actually executed.
///
/// Incrementality is invisible when it works, which makes it easy to break
/// without noticing. This turns "did that recompute?" into something a test can
/// assert on.
#[derive(Clone, Default)]
pub struct QueryLog(Arc<Mutex<Vec<String>>>);

impl QueryLog {
    /// Drains the recorded executions.
    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    /// Number of executions recorded since the last [`QueryLog::take`].
    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for QueryLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("QueryLog").field(&*self.0.lock().unwrap()).finish()
    }
}

#[salsa::db]
#[derive(Clone)]
pub struct KhoraDatabase {
    storage: salsa::Storage<Self>,
}

impl KhoraDatabase {
    pub fn new() -> Self {
        KhoraDatabase { storage: salsa::Storage::new(None) }
    }

    /// A database that records every query execution into the returned log.
    ///
    /// Used by the incrementality tests, and by tracing when we want to see
    /// what an edit actually invalidated.
    pub fn logged() -> (Self, QueryLog) {
        let log = QueryLog::default();
        let sink = log.clone();
        let db = KhoraDatabase {
            storage: salsa::Storage::new(Some(Box::new(move |event: salsa::Event| {
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    sink.0.lock().unwrap().push(format!("{database_key:?}"));
                }
            }))),
        };
        (db, log)
    }
}

impl Default for KhoraDatabase {
    fn default() -> Self {
        Self::new()
    }
}

#[salsa::db]
impl salsa::Database for KhoraDatabase {}

#[salsa::db]
impl Db for KhoraDatabase {}

/// Every target suffix a file name may carry, and which targets it selects.
///
/// Ordered longest-first is not needed — the suffixes are disjoint — but they
/// are listed the way a reader would group them.
const TARGET_SUFFIXES: [(&str, &[&str]); 4] = [
    ("_windows", &["windows"]),
    ("_linux", &["linux"]),
    ("_macos", &["macos"]),
    // The two that share almost everything. A file that works on both says so
    // once rather than existing twice.
    ("_posix", &["linux", "macos"]),
];

/// Whether a source file belongs in a build for `target`.
///
/// **The rule is in the file's name**, as it is in Go: `socket_windows.kh` is
/// compiled only on Windows, `socket_posix.kh` only on Linux and macOS, and
/// `socket.kh` always. Two files may then declare the same module, because at
/// most one of them is ever in the build.
///
/// A name rather than syntax, and deliberately. An `#[if(windows)]` attribute
/// would put two targets' code in one file, which means every reader reads
/// both and the compiler parses both — and the moment a third target appears,
/// so does a nest of conditions. A suffix keeps each target's version whole
/// and readable on its own, and makes "which files did this build use?" a
/// question `ls` can answer.
///
/// What it does not do is let a *fragment* differ. That is on purpose: if two
/// targets share ninety per cent of a file, the ten per cent belongs behind a
/// function they both call, which is what a reader would want anyway.
pub fn selected_for_target(path: &std::path::Path, target: &str) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return true;
    };
    match TARGET_SUFFIXES.iter().find(|(suffix, _)| stem.ends_with(suffix)) {
        Some((_, targets)) => targets.contains(&target),
        None => true,
    }
}

/// The target this compiler is running on, named as a file suffix would be.
///
/// Khora generates for the host triple, so the host's name is the target's —
/// the same assumption the code generator already makes about word size.
pub fn host_target() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// The standard library's source directory, if it can be found.
///
/// **`std` is not a dependency anybody declares.** It is found beside the
/// compiler, the way `rustc` finds its sysroot and `go` finds `GOROOT`, so a
/// program that has never written a manifest still has one. A line every
/// package repeats and no package can get wrong is not a line worth writing.
///
/// Searched rather than configured, in the same order and for the same reasons
/// as the runtime archive:
///
/// 1. `KHORA_STD`, for a packaged toolchain or an unusual layout.
/// 2. Beside the running executable, and one directory up — which covers both
///    an installed `khora` and a `cargo test` binary in `target/*/deps`.
/// 3. This workspace's own `std/`, for a compiler still sitting in the tree it
///    was built from.
///
/// `None` when nothing plausible is on disk, so the caller can decide whether
/// that is fatal. It is not always: a test that hands the database its own
/// sources needs no standard library at all.
pub fn standard_library() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("KHORA_STD") {
        let path = std::path::PathBuf::from(explicit);
        if path.is_dir() {
            return Some(path);
        }
    }

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("std"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("std"));
            }
        }
    }
    // Baked in at build time, so this only helps a compiler still in its own
    // tree — which is exactly where the probes above miss.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace) = manifest.parent().and_then(|c| c.parent()) {
        candidates.push(workspace.join("std"));
    }

    candidates.into_iter().find(|p| p.is_dir())
}
