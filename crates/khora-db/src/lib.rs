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
#[salsa::input]
pub struct SourceFile {
    #[returns(ref)]
    pub path: PathBuf,
    #[returns(deref)]
    pub text: String,
}

/// The set of files that make up a compilation.
///
/// Module and import resolution will read this rather than touching the
/// filesystem, so that adding or removing a file is an ordinary input change
/// and invalidates exactly the queries that depended on the file set.
#[salsa::input]
pub struct SourceRoot {
    #[returns(deref)]
    pub files: Vec<SourceFile>,
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
