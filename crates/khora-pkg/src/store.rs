//! The content-addressed package store.
//!
//! One directory per content hash under `~/.khora/store`, holding an extracted
//! package. Two properties earn the design:
//!
//! - **A directory in the store is immutable.** Its name is a hash of what is
//!   inside it, so writing to one after it exists would make the name a lie.
//!   Nothing here offers a way to.
//! - **Two packages that are byte-identical are one directory**, whatever they
//!   are called and wherever they came from. A diamond dependency on the same
//!   revision costs one checkout.
//!
//! Population is atomic by rename: the tree is built under a temporary name and
//! moved into place once complete. An interrupted fetch therefore leaves a stray
//! `.tmp-*` directory rather than a half-populated hash that later builds would
//! trust. Two processes racing to populate the same hash both succeed — the
//! second's rename fails, and the answer it wanted is what the first put there.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::hash::{self, ContentHash};

/// Where extracted packages live.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// The default store, `~/.khora/store`, or `$KHORA_HOME/store`.
    pub fn open() -> Result<Self> {
        let base = match std::env::var_os("KHORA_HOME") {
            Some(home) => PathBuf::from(home),
            None => home_directory()?.join(".khora"),
        };
        Self::at(base.join("store"))
    }

    /// A store at an explicit path, for tests and for a vendored build.
    pub fn at(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating the package store at {}", root.display()))?;
        Ok(Self { root })
    }

    /// The directory the store keeps its contents under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where a hash's contents are, whether or not they are there yet.
    pub fn path_of(&self, hash: &ContentHash) -> PathBuf {
        self.root.join(hash.as_str())
    }

    /// Whether these contents have already been fetched.
    pub fn contains(&self, hash: &ContentHash) -> bool {
        self.path_of(hash).is_dir()
    }

    /// Moves a populated directory into the store under its own content hash.
    ///
    /// Returns where it ended up. `staged` is consumed either way: on success
    /// by the rename, on a lost race by being removed.
    pub fn insert(&self, staged: &Path) -> Result<(ContentHash, PathBuf)> {
        let hash = hash::tree(staged)?;
        let destination = self.path_of(&hash);

        if destination.is_dir() {
            // Somebody got here first, and by construction they wrote the same
            // bytes. Theirs is as good as ours.
            let _ = std::fs::remove_dir_all(staged);
            return Ok((hash, destination));
        }

        match std::fs::rename(staged, &destination) {
            Ok(()) => Ok((hash, destination)),
            // Lost the race between the check above and the rename.
            Err(_) if destination.is_dir() => {
                let _ = std::fs::remove_dir_all(staged);
                Ok((hash, destination))
            }
            Err(e) => Err(e).with_context(|| {
                format!("moving {} into the store at {}", staged.display(), destination.display())
            }),
        }
    }

    /// A fresh directory to build a package in before [`Store::insert`].
    ///
    /// Named for the operation rather than randomly, and inside the store so
    /// the rename is on one filesystem. A rename across devices is a copy, and
    /// a copy is not atomic.
    pub fn staging(&self, label: &str) -> Result<PathBuf> {
        let safe: String =
            label.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        for attempt in 0..1_000 {
            let candidate = self.root.join(format!(".tmp-{safe}-{attempt}"));
            if !candidate.exists() {
                std::fs::create_dir_all(&candidate).with_context(|| {
                    format!("creating a staging directory at {}", candidate.display())
                })?;
                return Ok(candidate);
            }
        }
        bail!("a thousand staging directories for `{label}` already exist under {}", self.root.display())
    }
}

fn home_directory() -> Result<PathBuf> {
    // No `dirs` dependency for one lookup that is two variables wide.
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match home {
        Some(h) => Ok(h),
        None => bail!(
            "no home directory: neither HOME nor USERPROFILE is set. Set KHORA_HOME to \
             say where the package store should live"
        ),
    }
}
