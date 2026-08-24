//! `khora.lock`: what the last resolution decided.
//!
//! Committed, read on every build, and rewritten only when the manifest asks
//! for something it does not already answer. The point is that two people, and
//! a CI machine, and this machine six months from now, all compile the same
//! bytes.
//!
//! # What is pinned, and what is not
//!
//! A git dependency is pinned twice over: to a full commit id, and to the
//! SHA-256 of the tree that commit produced. The second is not redundant. A
//! commit id says what a server *said* it was serving; the content hash says
//! what arrived. If a rewritten history, a compromised mirror or a broken
//! transfer ever makes those disagree, the build stops and says so instead of
//! compiling something nobody wrote.
//!
//! A `path` dependency is recorded but not hashed. It is a directory somebody
//! is editing, and pinning its contents would mean a lockfile change on every
//! save — which trains people to stop reading lockfile diffs, and a lockfile
//! nobody reads is not a security property.
//!
//! # Format
//!
//! TOML, one `[[package]]` per resolved package, sorted by name so that a diff
//! shows what changed rather than how the resolver happened to walk.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::hash::ContentHash;
use crate::source::Source;

/// The name of the file, everywhere.
pub const LOCKFILE: &str = "khora.lock";

/// A parsed `khora.lock`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    /// Bumped only for a change that an older toolchain cannot read.
    ///
    /// A reader that does not know a version refuses the file rather than
    /// guessing, because guessing here means building the wrong source.
    pub version: u32,
    #[serde(default, rename = "package")]
    pub packages: Vec<LockedPackage>,
}

/// The version this toolchain writes.
pub const FORMAT_VERSION: u32 = 1;

/// One resolved package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPackage {
    pub name: String,
    /// `git`, or `path`.
    pub source: String,
    /// The repository URL, for a git package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The full commit id — never a tag or a branch, which can move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The directory, for a path package, relative to the manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// SHA-256 of the package's visible files. Absent for a path package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    /// What this package itself depends on, by name. Sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

impl Lockfile {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: Lockfile =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

        if parsed.version > FORMAT_VERSION {
            bail!(
                "{} is version {}, and this toolchain understands up to {}. Upgrade Khora \
                 rather than deleting the lockfile — it was written by something that knew \
                 more than this does",
                path.display(),
                parsed.version,
                FORMAT_VERSION
            );
        }
        Ok(parsed)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .context("serialising the lockfile")?;
        std::fs::write(path, format!("{HEADER}{text}"))
            .with_context(|| format!("writing {}", path.display()))
    }

    /// The entry for `name`, if it has one.
    pub fn get(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// The pinned checksum for a source, if this lockfile pins one.
    ///
    /// Returns `None` both when the package is absent and when it is a path
    /// dependency, which is right: neither is a mismatch, both mean "no opinion".
    pub fn pinned(&self, name: &str, source: &Source) -> Option<ContentHash> {
        let entry = self.get(name)?;
        match source {
            Source::Git { url, .. } if entry.url.as_deref() == Some(url.as_str()) => {
                entry.checksum.as_deref().map(ContentHash::from_hex)
            }
            _ => None,
        }
    }

    /// The revision a git dependency was locked to, if the URL still matches.
    pub fn pinned_revision(&self, name: &str, url: &str) -> Option<&str> {
        let entry = self.get(name)?;
        (entry.url.as_deref() == Some(url)).then_some(entry.revision.as_deref())?
    }

    /// Sorts, so that a diff shows a change rather than a walk order.
    pub fn normalise(&mut self) {
        self.version = FORMAT_VERSION;
        for package in &mut self.packages {
            package.dependencies.sort();
            package.dependencies.dedup();
        }
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

const HEADER: &str = "\
# Written by Khora. Commit this file.
#
# It records the exact source every dependency resolved to, so that this
# project builds the same way on another machine and in six months. Editing it
# by hand is not useful: the next resolution rewrites what it disagrees with,
# and a checksum you change yourself only makes the build refuse to start.

";

/// Builds a lockfile entry.
pub fn entry(
    name: &str,
    source: &Source,
    checksum: Option<&ContentHash>,
    dependencies: BTreeMap<String, ()>,
) -> LockedPackage {
    match source {
        Source::Git { url, rev } => LockedPackage {
            name: name.to_string(),
            source: "git".into(),
            url: Some(url.clone()),
            revision: Some(rev.clone()),
            path: None,
            checksum: checksum.map(|c| c.to_string()),
            dependencies: dependencies.into_keys().collect(),
        },
        Source::Path(p) => LockedPackage {
            name: name.to_string(),
            source: "path".into(),
            url: None,
            revision: None,
            path: Some(p.display().to_string().replace('\\', "/")),
            checksum: None,
            dependencies: dependencies.into_keys().collect(),
        },
    }
}
