//! Applying fixes to a compilation, and refusing the ones that break it.
//!
//! **A fix is applied by somebody who did not read it**, so the case analysis
//! on each lint is not the last word. This is the backstop behind all of
//! them: a pass of fixes is kept only if the fixed file still parses and the
//! whole compilation has no error it did not have before. Four shapes got
//! past the lints' own reasoning before this existed -- a string two levels
//! deep in a hole, an importer of a renamed module in another file, a `-1`
//! that joined the statement before it, a lambda parameter left untyped --
//! and the first two of those this would have caught on its own.
//!
//! What it cannot catch: a fix that compiles and changes what the program
//! does. `-1` after an `if` whose value is an `Int` compiles; that one is the
//! lint's to refuse (`idiomatic::joins_the_statement_before`), and the
//! same-output tests are what hold it.
//!
//! What it costs: every pass type-checks the compilation twice, once as it
//! was and once as fixed, incrementally in one database. A fix that breaks
//! something costs one more check per fix in its file, to name it.

use std::path::{Path, PathBuf};

use khora_db::{Db, KhoraDatabase, Setter, SourceFile};

use crate::idiomatic::{apply, select, Fix};

/// A fix offered for a file, with the lint that offered it.
pub type Offered = (&'static str, Fix);

/// What one pass did.
#[derive(Debug, Default)]
pub struct Pass {
    /// Each file the pass changed.
    pub changed: Vec<Changed>,
    /// Each fix withheld, and why.
    pub refused: Vec<Refused>,
}

/// A file the pass rewrote.
#[derive(Debug)]
pub struct Changed {
    /// The file.
    pub path: PathBuf,
    /// Its new text.
    pub text: String,
    /// The fixes that made it, as edits against the old text.
    pub fixes: Vec<Fix>,
}

/// Fixes that were computed and not applied.
#[derive(Debug)]
pub struct Refused {
    /// The file they were for.
    pub path: PathBuf,
    /// The lints that offered them.
    pub lints: Vec<&'static str>,
    /// What applying them would have done.
    pub why: String,
}

/// How many errors `file` has: its parse errors, or, when it parses, its type
/// errors. The same split `khora check` reports.
fn errors(db: &dyn Db, file: SourceFile) -> usize {
    let parse = khora_db::parse(db, file);
    if !parse.errors().is_empty() {
        return parse.errors().len();
    }
    khora_types::diagnostics(db, file).len()
}

/// One file's worth of fixes, being tried.
struct Candidate {
    file: SourceFile,
    old: String,
    taken: Vec<Offered>,
}

impl Candidate {
    fn text(&self) -> String {
        let fixes: Vec<&Fix> = self.taken.iter().map(|(_, fix)| fix).collect();
        apply(&self.old, &fixes).0
    }

    fn lints(&self) -> Vec<&'static str> {
        let mut lints: Vec<&'static str> = self.taken.iter().map(|(lint, _)| *lint).collect();
        lints.sort_unstable();
        lints.dedup();
        lints
    }
}

/// One pass of fixes over `files`, verified; `db` holds them, and is left as
/// it was found.
///
/// `offer` says which fixes a file gets -- the CLI and the language server
/// pass [`crate::reported`]'s, under the package's levels. Only files `owned`
/// accepts, and only those with no error before the pass, are fixed. For each:
/// the fixes [`select`] takes are applied; if the result does not parse, the
/// file is refused. Then every file in the compilation is checked with all
/// the new texts in place. If any file has more errors than before, the fixes
/// are tried one at a time to name the ones that break something; those are
/// refused, and the rest are verified again. If the rest still break it
/// together, none are applied.
pub fn pass(
    db: &mut KhoraDatabase,
    files: &[SourceFile],
    owned: &dyn Fn(&Path) -> bool,
    offer: &dyn Fn(&KhoraDatabase, SourceFile) -> Vec<Offered>,
) -> Pass {
    let before: Vec<usize> = files.iter().map(|file| errors(db, *file)).collect();
    let mut out = Pass::default();
    let mut candidates = Vec::new();
    for (at, file) in files.iter().enumerate() {
        if before[at] != 0 || !owned(file.path(db)) {
            continue;
        }
        let offered = offer(db, *file);
        let all: Vec<&Fix> = offered.iter().map(|(_, fix)| fix).collect();
        let chosen = select(&all);
        let taken: Vec<Offered> =
            offered.iter().filter(|(_, fix)| chosen.iter().any(|c| std::ptr::eq(*c, fix))).cloned().collect();
        let candidate = Candidate { file: *file, old: file.text(db).to_string(), taken };
        if candidate.taken.is_empty() {
            continue;
        }
        let text = candidate.text();
        if text == candidate.old {
            continue;
        }
        if !khora_syntax::parse(&text).errors().is_empty() {
            out.refused.push(Refused {
                path: file.path(db).clone(),
                lints: candidate.lints(),
                why: "the fixed file would not parse".to_string(),
            });
            continue;
        }
        candidates.push(candidate);
    }

    // The files a set of candidates would leave with more errors than before.
    let worse = |db: &mut KhoraDatabase, trying: &[(&Candidate, String)]| -> Vec<PathBuf> {
        for (candidate, text) in trying {
            candidate.file.set_text(db).to(text.clone());
        }
        let broken = files
            .iter()
            .zip(&before)
            .filter(|(file, had)| errors(db, **file) > **had)
            .map(|(file, _)| file.path(db).clone())
            .collect();
        for (candidate, _) in trying {
            candidate.file.set_text(db).to(candidate.old.clone());
        }
        broken
    };

    loop {
        let trying: Vec<(&Candidate, String)> = candidates.iter().map(|c| (c, c.text())).collect();
        let broken = worse(db, &trying);
        if broken.is_empty() {
            break;
        }
        let named = broken.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
        let mut kept = Vec::new();
        let mut any_named = false;
        for candidate in std::mem::take(&mut candidates) {
            let (bad, good): (Vec<Offered>, Vec<Offered>) = candidate.taken.iter().cloned().partition(|one| {
                let alone = apply(&candidate.old, &[&one.1]).0;
                !worse(db, &[(&candidate, alone)]).is_empty()
            });
            if !bad.is_empty() {
                any_named = true;
                let path = candidate.file.path(db).clone();
                let mut lints: Vec<&'static str> = bad.iter().map(|(lint, _)| *lint).collect();
                lints.dedup();
                out.refused.push(Refused { path, lints, why: format!("the fix leaves errors in {named}") });
            }
            if !good.is_empty() {
                kept.push(Candidate { taken: good, ..candidate });
            }
        }
        if !any_named {
            // Each is harmless alone and they break something together.
            for candidate in &kept {
                out.refused.push(Refused {
                    path: candidate.file.path(db).clone(),
                    lints: candidate.lints(),
                    why: format!("together with the other fixes in this pass, it leaves errors in {named}"),
                });
            }
            kept.clear();
        }
        candidates = kept;
    }

    for candidate in candidates {
        let text = candidate.text();
        out.changed.push(Changed {
            path: candidate.file.path(db).clone(),
            text,
            fixes: candidate.taken.into_iter().map(|(_, fix)| fix).collect(),
        });
    }
    out
}
