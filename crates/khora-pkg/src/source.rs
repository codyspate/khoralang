//! Where a package comes from.
//!
//! Three kinds, and only two of them work today. A `path` is a directory on
//! this machine; a `git` dependency is a repository and a revision inside it; a
//! `version` needs a registry, which does not exist, and says so in a sentence
//! rather than resolving to nothing.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use khora_manifest::Dependency;

/// Where a package's files come from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// A directory, resolved relative to the manifest that named it.
    ///
    /// Not hashed into the lockfile, and that is deliberate — see
    /// [`crate::lock`]. A path dependency is a working copy somebody is
    /// editing, and pinning its contents would mean re-locking on every save.
    Path(PathBuf),
    /// A git repository at a particular revision.
    ///
    /// `rev` is whatever `git rev-parse` accepts and is resolved to a full
    /// commit id during resolution, so a lockfile never holds a name that can
    /// move.
    Git { url: String, rev: String },
}

impl Source {
    /// Reads a `[dependencies]` entry.
    ///
    /// `base` is the directory holding the manifest, so a relative `path` means
    /// what it looks like it means.
    pub fn of(name: &str, dependency: &Dependency, base: &Path) -> Result<Self> {
        let git = dependency.git.as_deref();
        let path = dependency.path.as_deref();
        let version = dependency.version.as_deref();

        match (git, path, version) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => bail!(
                "`{name}` gives more than one source. Say exactly one of `git`, `path` \
                 or `version`"
            ),
            (Some(url), None, None) => {
                // A git dependency with no revision is not reproducible, and
                // silently taking the default branch is how a build stops
                // meaning anything. `rev` may be a tag or a branch here; it is
                // resolved to a commit id before it reaches a lockfile.
                let Some(rev) = dependency.rev.as_deref().or(dependency.tag.as_deref()) else {
                    bail!(
                        "`{name}` is a git dependency with no `rev` or `tag`, so what it \
                         resolves to would change under you. Name one"
                    )
                };
                Ok(Source::Git { url: url.to_string(), rev: rev.to_string() })
            }
            (None, Some(relative), None) => Ok(Source::Path(base.join(relative))),
            (None, None, Some(_)) => bail!(
                "`{name}` is declared with a version, and resolving one needs a registry \
                 that does not exist yet. Point it at a `git` or `path` source for now"
            ),
            (None, None, None) => bail!(
                "`{name}` says none of `git`, `path` or `version`, so there is nothing \
                 to resolve"
            ),
        }
    }

    /// Whether the lockfile pins this source's contents.
    ///
    /// A path dependency is a directory somebody is editing; hashing it would
    /// make every save a lockfile change.
    pub fn is_pinned(&self) -> bool {
        matches!(self, Source::Git { .. })
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Path(p) => write!(f, "path {}", p.display()),
            Source::Git { url, rev } => write!(f, "git {url}#{rev}"),
        }
    }
}
