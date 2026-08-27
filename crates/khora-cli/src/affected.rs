//! Which members a diff can reach.
//!
//! `khora check . --since main` checks the members the change could have
//! broken and says which ones it skipped. In a monorepo that is the difference
//! between a check somebody runs and one they learn to skip.
//!
//! # Why this is exact and not a guess
//!
//! Nx and Turborepo infer the graph from imports and are approximately right;
//! Bazel is exactly right and makes you declare everything by hand. Khora gets
//! the exact answer for free, because the resolver already knows which
//! packages a member reaches — it had to, in order to hand the compiler a set
//! of directories. `khora_pkg::resolve` returns that, so "which members does
//! this diff affect" is a query against something a build computes anyway.
//!
//! At *file* granularity, which is where this stops. A change inside a package
//! a member depends on marks that member affected, even if the member never
//! reaches the module that changed. Going finer means asking the compiler
//! which modules a package actually reaches, which it also knows;
//! `TypeMap::reachable` holds part of the answer. Roadmap 14.16 stops here
//! because file granularity is already exact about the thing that goes wrong
//! in practice — a shared package changing under you — and module granularity
//! is a refinement with the same shape.
//!
//! # When the answer is "everything"
//!
//! A change that is not inside any member and not inside anything a member
//! depends on — the compiler, `std`, the root manifest, a script — marks
//! *every* member affected. That is deliberate and it is the rule that makes
//! the feature safe to trust: a tool that answers "nothing was affected"
//! because it did not recognise the file is worse than no tool.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// What a diff selected, and what it left out.
pub struct Selection {
    /// The members to run, in the order the workspace lists them.
    pub members: Vec<PathBuf>,
    /// The members left out, for the report.
    pub skipped: Vec<PathBuf>,
    /// Why everything was selected, when everything was.
    ///
    /// `None` when the diff was actually narrowed. Some when a changed file
    /// belongs to nothing the workspace knows about, in which case naming it
    /// is the difference between "the tool is being careful" and "the tool is
    /// broken".
    pub everything_because: Option<PathBuf>,
}

/// The members `since` can reach, out of `members`.
///
/// `since` is anything `git diff` accepts: a branch, a tag, a commit.
pub fn select(root: &Path, members: &[PathBuf], since: &str) -> Result<Selection> {
    let changed = changed_files(root, since)?;

    // A member's own directory, and every directory it compiles. The second
    // half is the part a heuristic would have to guess at.
    let mut reach: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for member in members {
        let mut directories = vec![absolute(&member.clone())];
        if let Ok(store) = khora_pkg::Store::open() {
            if let Ok(resolution) = khora_pkg::resolve(&member.join("khora.toml"), &store, false) {
                directories.extend(resolution.directories().into_iter().map(|d| absolute(&d)));
            }
        }
        reach.push((member.clone(), directories));
    }

    let mut selected: BTreeSet<PathBuf> = BTreeSet::new();
    for file in &changed {
        let mut claimed = false;
        for (member, directories) in &reach {
            if directories.iter().any(|directory| file.starts_with(directory)) {
                selected.insert(member.clone());
                claimed = true;
            }
        }
        if !claimed {
            // Nothing in the workspace owns this file, so nothing in the
            // workspace can prove it is unaffected.
            return Ok(Selection {
                members: members.to_vec(),
                skipped: Vec::new(),
                everything_because: Some(file.clone()),
            });
        }
    }

    let chosen: Vec<PathBuf> =
        members.iter().filter(|member| selected.contains(*member)).cloned().collect();
    let skipped: Vec<PathBuf> =
        members.iter().filter(|member| !selected.contains(*member)).cloned().collect();
    Ok(Selection { members: chosen, skipped, everything_because: None })
}

/// Every file that differs from `since`, as an absolute path.
///
/// Three questions asked of git, because one does not cover it: what changed
/// against the revision, what is staged but uncommitted, and what is not
/// tracked at all. A new file nobody has added yet is exactly the change most
/// likely to be the one being tested.
fn changed_files(root: &Path, since: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for args in [
        vec!["diff", "--name-only", since, "--"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let result = Command::new("git")
            .args(&args)
            .current_dir(root)
            .output()
            .with_context(|| "running git, which `--since` needs in order to know what changed")?;
        if !result.status.success() {
            bail!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&result.stderr).trim()
            );
        }
        for line in String::from_utf8_lossy(&result.stdout).lines() {
            let line = line.trim();
            if !line.is_empty() {
                out.push(absolute(&root.join(line)));
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// A path in the one spelling `starts_with` can compare.
///
/// Both sides have to be canonical or neither comparison works: the members
/// come from a manifest as `.\examples\core_demo` and git answers with
/// `examples/core_demo/src/main.kh`. A file that does not exist -- deleted in
/// the working tree, which `git diff` reports -- cannot be canonicalized, so
/// it falls back to joining, which is right for a deleted file whose
/// *directory* still says which member owned it.
fn absolute(path: &Path) -> PathBuf {
    if let Ok(found) = path.canonicalize() {
        return khora_manifest::readable(found);
    }
    match (path.parent().and_then(|p| p.canonicalize().ok()), path.file_name()) {
        (Some(parent), Some(name)) => khora_manifest::readable(parent).join(name),
        _ => path.to_path_buf(),
    }
}
