//! What a package's contents hash to.
//!
//! The number that goes in `khora.lock` and names a directory in the store, so
//! two things have to be true of it: the same tree must hash the same on every
//! machine, and any change to any file the compiler reads must change it.
//!
//! # What is hashed, and why the details matter
//!
//! Every file under the package root that a build can see: `khora.toml` and
//! every `.kh` file. Not `.git`, not `target`, not a README — a change to those
//! is not a change to the package, and treating it as one means a lockfile that
//! churns for reasons nobody can act on.
//!
//! Three details keep it reproducible across platforms, and each is a bug
//! somebody would otherwise find later:
//!
//! - **Paths are hashed too, separated from the contents by a byte that cannot
//!   appear in either.** Hashing only file contents lets a rename go unnoticed,
//!   and concatenating path and contents without a separator lets `ab` + `c`
//!   collide with `a` + `bc`.
//! - **Paths are normalised to forward slashes.** Otherwise the same package
//!   hashes differently on Windows, which would mean a lockfile nobody can
//!   share.
//! - **Files are sorted by that normalised path**, because directory order is
//!   whatever the filesystem feels like.
//!
//! Contents are hashed as raw bytes. Khora source is pinned to LF by
//! `.gitattributes`, so a checkout is byte-identical everywhere; normalising
//! here as well would hide a genuine difference in a file that is not source.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// A package's content hash, as it appears in `khora.lock`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash(String);

impl ContentHash {
    /// Takes an already-computed digest, such as one read from a lockfile.
    pub fn from_hex(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    /// The whole digest, which is also the directory name in the store.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first twelve characters, for messages a person reads.
    pub fn short(&self) -> &str {
        &self.0[..12.min(self.0.len())]
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hashes every file in `root` that a build can see.
pub fn tree(root: &Path) -> Result<ContentHash> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();

    let mut hasher = Sha256::new();
    for relative in &files {
        let bytes = std::fs::read(root.join(relative))
            .with_context(|| format!("reading {relative} under {}", root.display()))?;

        // The length prefix is what makes this injective: without it a file
        // named `a` holding `bc` hashes the same as one named `ab` holding `c`.
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(ContentHash(format!("{:x}", hasher.finalize())))
}

/// Whether a build can see this file.
fn is_visible(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some("kh") => true,
        _ => path.file_name().is_some_and(|n| n == "khora.toml"),
    }
}

/// Whether to descend into a directory.
///
/// `.git` holds a different hash of the same content and would make the answer
/// depend on how the checkout was made; `target` holds build output, which is
/// derived from what is being hashed.
fn is_traversable(name: &std::ffi::OsStr) -> bool {
    name != ".git" && name != "target"
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading the directory {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(is_traversable) {
                collect(root, &path, out)?;
            }
        } else if is_visible(&path) {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push(relative);
        }
    }
    Ok(())
}
