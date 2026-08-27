//! Finding a workspace, and the members it holds.
//!
//! A workspace is a `khora.toml` with a `[workspace]` table. Its members are
//! directories, each with a `khora.toml` of its own, and every command that
//! takes a package takes a workspace root instead by running over all of them.
//!
//! # Why the patterns are not globs
//!
//! `members = ["packages/*", "examples/*"]` is what a monorepo actually
//! writes, and a trailing `*` matching one level of directory covers it. `**`,
//! character classes and brace expansion are a syntax to document, a
//! dependency to carry, and a set of edge cases to get subtly wrong — for a
//! feature whose whole job is "which directories". A member that does not fit
//! is listed by name, which reads better than a clever pattern anyway.
//!
//! # What is not a member
//!
//! A directory with no `khora.toml`. Silently, and that is deliberate:
//! `examples/*` should not break because somebody left a `notes/` directory
//! there. `exclude` exists for the directory that *does* have a manifest and
//! should still be left out.

use std::path::{Path, PathBuf};

use crate::{Manifest, ManifestError};

/// A workspace root and the members under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The directory holding the root `khora.toml`.
    pub root: PathBuf,
    /// Each member's directory, in the order the patterns named them.
    pub members: Vec<PathBuf>,
}

/// Reads the workspace rooted at `manifest`, if that manifest is a root.
///
/// `Ok(None)` for an ordinary package manifest, which is not an error — most
/// manifests are not workspace roots and every caller has to cope with that
/// anyway.
pub fn read(manifest: &Path) -> Result<Option<Workspace>, ManifestError> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|why| ManifestError::io(&format!("reading {}", manifest.display()), &why))?;
    let parsed = Manifest::parse(&text)?.manifest;
    let Some(table) = parsed.workspace else { return Ok(None) };

    let root = manifest.parent().unwrap_or(Path::new(".")).to_path_buf();
    let excluded: Vec<PathBuf> =
        table.exclude.iter().map(|entry| root.join(entry)).collect();

    let mut members: Vec<PathBuf> = Vec::new();
    for pattern in &table.members {
        for candidate in expand(&root, pattern) {
            if excluded.iter().any(|skip| same(skip, &candidate)) {
                continue;
            }
            if !candidate.join("khora.toml").is_file() {
                continue;
            }
            if !members.iter().any(|seen| same(seen, &candidate)) {
                members.push(candidate);
            }
        }
    }

    Ok(Some(Workspace { root, members }))
}

/// The workspace `start` belongs to, searching upwards.
///
/// **Upwards from the member, not downwards from the root**, which is what
/// lets `khora check` inside `examples/core_demo` know it is in a workspace
/// without being told. The first `khora.toml` with a `[workspace]` table wins;
/// a member's own manifest is passed over rather than stopping the walk.
pub fn enclosing(start: &Path) -> Option<Workspace> {
    let mut here = if start.is_dir() { Some(start) } else { start.parent() };
    while let Some(directory) = here {
        let candidate = directory.join("khora.toml");
        if candidate.is_file() {
            if let Ok(Some(found)) = read(&candidate) {
                return Some(found);
            }
        }
        here = directory.parent();
    }
    None
}

/// The directories one pattern names.
fn expand(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let Some(prefix) = pattern.strip_suffix("/*").or_else(|| pattern.strip_suffix("*")) else {
        let named = root.join(pattern);
        return if named.is_dir() { vec![named] } else { Vec::new() };
    };
    let parent = root.join(prefix.trim_end_matches('/'));
    let Ok(entries) = std::fs::read_dir(&parent) else { return Vec::new() };

    let mut found: Vec<PathBuf> =
        entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
    // The filesystem's order is not an order. Sorted, so that a command over a
    // workspace reports its members the same way twice.
    found.sort();
    found
}

/// Whether two paths name the same directory.
///
/// By `canonicalize` where it works, because `examples/core_demo` and
/// `./examples/core_demo/` are the same member written twice and a workspace
/// that listed both should not run it twice.
fn same(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => left == right,
    }
}
