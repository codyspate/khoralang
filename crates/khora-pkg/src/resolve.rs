//! Turning a manifest into a set of directories to compile.
//!
//! Breadth-first from the root package, following each dependency's own
//! manifest, so a dependency's dependencies are found without anybody
//! restating them.
//!
//! # What this deliberately does not do
//!
//! **There is no version solving.** Every source names exactly one thing — a
//! commit id, or a directory — so there is nothing to choose between. Two
//! packages asking for different revisions of a third is therefore an error
//! here rather than a resolution problem, and the error names both askers,
//! which is the thing a person needs in order to fix it. When a registry
//! arrives this is where the solver goes, and the diamond case is exactly what
//! it will have to answer.
//!
//! **Nothing is fetched twice.** A package is keyed by name, and a second
//! request for a name already resolved is checked for agreement rather than
//! re-fetched.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use khora_manifest::Manifest;

use crate::fetch;
use crate::hash::ContentHash;
use crate::lock::{self, Lockfile};
use crate::source::Source;
use crate::store::Store;

/// One package, resolved to a directory on disk.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub name: String,
    pub source: Source,
    /// Where its files are: in the store for a git package, in place for a path
    /// one.
    pub directory: PathBuf,
    pub checksum: Option<ContentHash>,
    /// Which packages asked for it, for an error message that names them.
    pub requested_by: Vec<String>,
}

/// Everything a build needs, plus the lockfile that records it.
#[derive(Debug)]
pub struct Resolution {
    pub packages: Vec<Resolved>,
    pub lockfile: Lockfile,
    /// Whether the lockfile differs from the one that was read.
    pub changed: bool,
}

impl Resolution {
    /// The directories to hand a compilation, dependencies before the root.
    pub fn directories(&self) -> Vec<PathBuf> {
        self.packages.iter().map(|p| p.directory.clone()).collect()
    }
}

/// Resolves the dependencies of the manifest at `manifest_path`.
///
/// `locked` refuses to change the lockfile, which is what CI wants: a build
/// that would need a new resolution is a build whose lockfile was not committed.
pub fn resolve(manifest_path: &Path, store: &Store, locked: bool) -> Result<Resolution> {
    let root_dir = manifest_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let lock_path = root_dir.join(lock::LOCKFILE);
    let existing =
        if lock_path.is_file() { Lockfile::read(&lock_path)? } else { Lockfile::default() };

    let root = read_manifest(manifest_path)?;
    let root_name = root.package.name.clone();

    let mut resolved: BTreeMap<String, Resolved> = BTreeMap::new();
    let mut edges: BTreeMap<String, BTreeMap<String, ()>> = BTreeMap::new();
    let mut queue: VecDeque<(String, Manifest, PathBuf)> =
        VecDeque::from([(root_name.clone(), root, root_dir.clone())]);

    while let Some((holder, manifest, base)) = queue.pop_front() {
        for (name, dependency) in &manifest.dependencies {
            let source = Source::of(name, dependency, &base)?;
            edges.entry(holder.clone()).or_default().insert(name.clone(), ());

            if let Some(before) = resolved.get_mut(name) {
                if before.source != source {
                    bail!(
                        "`{name}` is asked for twice and differently:\n  \
                         {} wants {}\n  {holder} wants {}\n\
                         There is no version solver yet, so the two have to agree",
                        before.requested_by.join(", "),
                        before.source,
                        source
                    );
                }
                before.requested_by.push(holder.clone());
                continue;
            }

            let package = acquire(name, &source, store, &existing, locked)
                .with_context(|| format!("resolving `{name}`, asked for by `{holder}`"))?;

            // Its own manifest is what says what *it* needs.
            let child = package.directory.join("khora.toml");
            if child.is_file() {
                let parsed = read_manifest(&child)?;
                if parsed.package.name != *name {
                    bail!(
                        "`{holder}` asks for `{name}`, but the package at {} calls itself \
                         `{}`. A dependency's name has to be the name it answers to",
                        package.directory.display(),
                        parsed.package.name
                    );
                }
                queue.push_back((name.clone(), parsed, package.directory.clone()));
            }

            resolved.insert(name.clone(), Resolved { requested_by: vec![holder.clone()], ..package });
        }
    }

    let mut lockfile = Lockfile { version: lock::FORMAT_VERSION, packages: Vec::new() };
    for (name, package) in &resolved {
        lockfile.packages.push(lock::entry(
            name,
            &package.source,
            package.checksum.as_ref(),
            edges.get(name).cloned().unwrap_or_default(),
        ));
    }
    lockfile.normalise();

    let mut before = existing;
    before.normalise();
    let changed = before != lockfile;

    if changed && locked {
        bail!(
            "the lockfile does not match the manifest, and `--locked` says not to change \
             it. Run the build without `--locked` and commit the result"
        );
    }
    if changed {
        lockfile.write(&lock_path)?;
    }

    Ok(Resolution { packages: resolved.into_values().collect(), lockfile, changed })
}

/// Gets one package's files onto disk, verifying anything the lockfile pins.
fn acquire(
    name: &str,
    source: &Source,
    store: &Store,
    locked_to: &Lockfile,
    locked: bool,
) -> Result<Resolved> {
    match source {
        Source::Path(directory) => {
            if !directory.is_dir() {
                bail!(
                    "`{name}` points at {}, which is not a directory",
                    directory.display()
                );
            }
            Ok(Resolved {
                name: name.to_string(),
                source: source.clone(),
                directory: directory.clone(),
                checksum: None,
                requested_by: Vec::new(),
            })
        }
        Source::Git { url, rev, subdir } => {
            // A lockfile's revision wins over the manifest's, which is the
            // whole point of having one: `rev = "main"` must keep meaning the
            // commit it meant when it was locked.
            let wanted = match locked_to.pinned_revision(name, url) {
                Some(pinned) => pinned.to_string(),
                None if locked => bail!(
                    "`{name}` is not in the lockfile and `--locked` says not to add it"
                ),
                None => fetch::resolve_revision(url, rev)?,
            };

            // Already in the store under a hash the lockfile pins? Then there
            // is nothing to fetch, and this is the common case.
            if let Some(pinned) = locked_to.pinned(name, &Source::Git {
                url: url.clone(),
                rev: wanted.clone(),
                subdir: subdir.clone(),
            }) {
                if store.contains(&pinned) {
                    let root = within(store.path_of(&pinned), subdir.as_deref());
                    published(name, &root)?;
                    return Ok(Resolved {
                        name: name.to_string(),
                        source: Source::Git {
                            url: url.clone(),
                            rev: wanted,
                            subdir: subdir.clone(),
                        },
                        directory: root,
                        checksum: Some(pinned),
                        requested_by: Vec::new(),
                    });
                }
            }

            let staged = store.staging(name)?;
            fetch::checkout(url, &wanted, &staged)?;
            let (checksum, directory) = store.insert(&staged)?;

            // The commit id says what the server claimed. This says what
            // arrived. They disagreeing is the case the lockfile exists for.
            if let Some(pinned) = locked_to.pinned(
                name,
                &Source::Git {
                    url: url.clone(),
                    rev: wanted.clone(),
                    subdir: subdir.clone(),
                },
            ) {
                if pinned != checksum {
                    bail!(
                        "`{name}` at {wanted} does not hash to what the lockfile records.\n  \
                         locked:   {pinned}\n  arrived:  {checksum}\n\
                         The same commit id produced different bytes, which should be \
                         impossible. Do not build this until you know why"
                    );
                }
            }

            let root = within(directory, subdir.as_deref());
            published(name, &root)?;
            Ok(Resolved {
                name: name.to_string(),
                source: Source::Git {
                    url: url.clone(),
                    rev: wanted,
                    subdir: subdir.clone(),
                },
                directory: root,
                checksum: Some(checksum),
                requested_by: Vec::new(),
            })
        }
    }
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed = Manifest::parse(&text)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(parsed.manifest)
}

/// The package's own root inside a checkout.
///
/// A git URL names a repository. Where the two coincide there is nothing to
/// do; where they do not, this is the difference.
fn within(checkout: std::path::PathBuf, subdir: Option<&str>) -> std::path::PathBuf {
    match subdir {
        Some(inner) => checkout.join(inner),
        None => checkout,
    }
}

/// Refuses a package that has not offered itself.
///
/// **An intent marker, not a permission**, and the message says so. Anybody can
/// set `publish`, and anybody can point a `path` dependency at anything — what
/// this prevents is depending on somebody's application, or their unfinished
/// experiment, because it happened to be in a repository you fetched.
///
/// Absent means no. Publishing here is *passive* — a pushed repository is
/// already fetchable — so the active choice is the one that should be written
/// down. That is the opposite of Cargo's default and for the opposite reason:
/// publishing to a registry is an act somebody performs, and opting out is the
/// right shape there.
///
/// A `path` dependency never reaches this. That is your own working copy.
fn published(name: &str, root: &Path) -> Result<()> {
    let manifest = root.join("khora.toml");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        bail!(
            "`{name}` has no `khora.toml` at {}. A git dependency names a repository, and \
             the package inside it may need `subdir` to say where",
            root.display()
        )
    };
    let parsed = khora_manifest::Manifest::parse(&text)
        .map_err(|e| anyhow::anyhow!("reading `{name}`'s manifest: {e}"))?;
    if parsed.manifest.package.publish == Some(true) {
        return Ok(());
    }
    bail!(
        "`{name}` does not offer itself as a package: its `khora.toml` has no \
         `publish = true` under `[package]`.\n\
         That flag is how a repository says which of the things in it are libraries — this \
         one may be an application, or unfinished. Ask its author to add it, or depend on a \
         working copy with `path` if it is yours."
    )
}
