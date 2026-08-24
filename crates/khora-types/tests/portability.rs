//! The standard library type-checks for every target, from any host.
//!
//! **Platform files are the one part of `std` a developer cannot compile.**
//! `khora-db::selected_for_target` picks by file-name suffix, so building on
//! Windows never looks at `socket_linux.kh` or `socket_macos.kh` — and until
//! this existed, a type error in one of them was found by whoever next built on
//! that platform, which for macOS meant nobody.
//!
//! Type checking is host-independent: it is the *linker* that needs the target,
//! and nothing here links. So the whole matrix can be checked from wherever the
//! suite happens to run, and a CI job on one machine covers the other two.
//!
//! What this does not cover is whether the numbers are right — a `sockaddr_in`
//! with the family in the wrong byte type-checks perfectly. That needs a real
//! machine, which is what the CI matrix is for.

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every target `selected_for_target` knows how to choose between.
const TARGETS: [&str; 3] = ["windows", "linux", "macos"];

fn std_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")
}

/// Every `.kh` file of `std` that belongs in a build for `target`.
fn sources_for(target: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![std_root()];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, target)
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn the_standard_library_checks_for_every_target() {
    for target in TARGETS {
        let db = KhoraDatabase::new();
        let files: Vec<SourceFile> = sources_for(target)
            .into_iter()
            .map(|path| {
                let text = std::fs::read_to_string(&path).expect("readable");
                SourceFile::new(&db, path, text)
            })
            .collect();
        assert!(!files.is_empty(), "no sources selected for `{target}`");
        SourceRoot::new(&db, files.clone());

        let mut complaints: Vec<String> = Vec::new();
        for file in &files {
            let parsed = khora_db::parse(&db, *file);
            for error in parsed.errors() {
                complaints.push(format!("{}: {}", file.path(&db).display(), error.message));
            }
            for error in khora_types::diagnostics(&db, *file).iter() {
                complaints.push(format!("{}: {}", file.path(&db).display(), error.message));
            }
        }
        assert!(
            complaints.is_empty(),
            "`std` does not check for `{target}`:\n  {}",
            complaints.join("\n  ")
        );
    }
}

/// Every target gets a socket module, which is the one platform-specific piece
/// of `std` an application actually needs.
///
/// Stated separately from the check above because the failure reads differently:
/// a missing file is not a type error, it is `cannot find module
/// `std::net::socket`` in somebody's program a long way from here.
#[test]
fn every_target_has_sockets() {
    for target in TARGETS {
        let selected = sources_for(target);
        assert!(
            selected.iter().any(|p| {
                p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with("socket"))
            }),
            "no socket implementation is selected for `{target}`"
        );
    }
}
