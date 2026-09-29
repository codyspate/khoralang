//! `std` is written the one way: no `idiomatic` finding anywhere in it.
//!
//! **`std` has no manifest**, so no `[lints]` table governs it the way one
//! governs a package, and `cargo nextest` never runs `khora check std`. A
//! rule the gate does not enforce is a preference rather than a standard, and
//! `std` is the code people copy. So this reads every `std` file the host
//! compiles, as one compilation, and fails on any finding of a lint in the
//! group. The fix is `khora check --fix std` from the repository root, whose
//! manifest switches the group on; what it cannot fix, it names.
//!
//! What it does not see: a file selected only for another target
//! (`socket_macos.kh` on Linux). CI runs this on each OS it builds on.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a readable directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh")
            && khora_db::selected_for_target(&path, khora_db::host_target())
        {
            out.push(path);
        }
    }
}

#[test]
fn std_has_no_idiomatic_finding() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let mut paths = Vec::new();
    sources(&root, &mut paths);
    paths.sort();
    assert!(paths.len() > 10, "expected to find `std`, found {} files", paths.len());

    let db = KhoraDatabase::new();
    let files: Vec<SourceFile> = paths
        .iter()
        .map(|path| SourceFile::new(&db, path.clone(), std::fs::read_to_string(path).expect("readable")))
        .collect();
    SourceRoot::new(&db, files.clone());

    let mut found = Vec::new();
    for file in &files {
        let errors = khora_types::diagnostics(&db, *file);
        assert!(errors.is_empty(), "{} does not check: {errors:?}", file.path(&db).display());
        let text = file.text(&db);
        for finding in khora_lint::findings(&db, *file) {
            if !khora_lint::idiomatic::ALL.contains(&finding.lint) {
                continue;
            }
            let line = text[..usize::from(finding.range.start())].matches('\n').count() + 1;
            let name = file.path(&db).strip_prefix(&root).unwrap_or(file.path(&db)).display().to_string();
            found.push(format!("std/{name}:{line} [{}] {}", finding.lint, finding.message));
        }
    }
    assert!(
        found.is_empty(),
        "{} `idiomatic` finding(s) in `std`:\n  {}\n\nRun `khora check --fix std` from the repository root.",
        found.len(),
        found.join("\n  ")
    );
}
