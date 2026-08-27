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
//!
//! # One lock for a workspace
//!
//! A workspace resolves as one graph, into one `khora.lock` at the root. Two
//! members cannot then be quietly holding two revisions of a shared
//! dependency: the second one to ask hits the same "asked for twice and
//! differently" error that two packages in one graph already did, and the
//! single-version rule is most of what makes a monorepo coherent rather than a
//! directory of projects. Roadmap 14.15.
//!
//! The cost is real and worth stating: resolving *any* member resolves every
//! member, so a member with no dependencies of its own still pays for the
//! fetches of one that has them, and a member whose manifest does not parse
//! breaks the others. That is the price of the graph being one graph, and
//! Cargo charges it too.
//!
//! What comes *back* is still only what the asking member reaches. The lock
//! covers the workspace; the compilation does not.

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
    /// The name its manifest declares.
    pub name: String,
    /// Where it came from, and what the lockfile records.
    pub source: Source,
    /// Where its files are: in the store for a git package, in place for a path
    /// one.
    pub directory: PathBuf,
    /// What the contents hashed to. `None` for a path dependency, which is a
    /// working copy and is expected to change.
    pub checksum: Option<ContentHash>,
    /// Which packages asked for it, for an error message that names them.
    pub requested_by: Vec<String>,
}

/// Everything a build needs, plus the lockfile that records it.
#[derive(Debug)]
pub struct Resolution {
    /// Everything the build compiles, dependencies included.
    pub packages: Vec<Resolved>,
    /// What should be on disk after this resolution.
    pub lockfile: Lockfile,
    /// Whether the lockfile differs from the one that was read.
    pub changed: bool,
    /// Member lockfiles that are no longer read, because the root holds the
    /// only one now.
    ///
    /// Returned rather than deleted, and rather than ignored. Deleting a
    /// committed file is not this function's call to make, and a lockfile that
    /// silently stopped being read is the kind of thing somebody finds out
    /// about during an incident.
    pub stray_locks: Vec<PathBuf>,
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

    // The workspace this manifest belongs to, if it belongs to one. Being
    // *under* a root is not being *in* it, which is the same rule inheritance
    // uses: a directory the root does not list keeps its own lockfile.
    let workspace = khora_manifest::enclosing(&root_dir)
        .filter(|found| same(&found.root, &root_dir) || found.members.iter().any(|m| same(m, &root_dir)));

    let lock_dir = workspace.as_ref().map_or_else(|| root_dir.clone(), |found| found.root.clone());
    let lock_path = lock_dir.join(lock::LOCKFILE);
    let existing =
        if lock_path.is_file() { Lockfile::read(&lock_path)? } else { Lockfile::default() };

    let root = read_manifest(manifest_path)?;
    let root_name = holder_name(&root, &root_dir);

    let mut resolved: BTreeMap<String, Resolved> = BTreeMap::new();
    let mut edges: BTreeMap<String, BTreeMap<String, ()>> = BTreeMap::new();
    let mut queue: VecDeque<(String, Manifest, PathBuf)> =
        VecDeque::from([(root_name.clone(), root, root_dir.clone())]);

    // Every member seeds the queue, so the lock describes the workspace rather
    // than whichever member happened to be built.
    let mut stray_locks = Vec::new();
    if let Some(found) = &workspace {
        for member in &found.members {
            let stray = member.join(lock::LOCKFILE);
            if !same(member, &lock_dir) && stray.is_file() {
                stray_locks.push(stray);
            }
            if same(member, &root_dir) {
                continue;
            }
            let manifest = member.join("khora.toml");
            let parsed = read_manifest(&manifest).with_context(|| {
                format!(
                    "resolving the workspace at {}: every member is resolved into the one \
                     lockfile, so this manifest has to parse even to build a different member",
                    found.root.display()
                )
            })?;
            queue.push_back((holder_name(&parsed, member), parsed, member.clone()));
        }
    }

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
                let Some(declared) = parsed.package() else {
                    bail!(
                        "`{holder}` asks for `{name}`, but {} is a workspace root rather \
                         than a package. Point at one of its members with `subdir`",
                        package.directory.display()
                    );
                };
                if declared.name != *name {
                    bail!(
                        "`{holder}` asks for `{name}`, but the package at {} calls itself \
                         `{}`. A dependency's name has to be the name it answers to",
                        package.directory.display(),
                        declared.name
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
            &lock_dir,
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

    // **The lock covers the workspace; the compilation does not.** Handing a
    // member every package in the workspace would compile its siblings'
    // dependencies into it, and a package that builds only because a sibling
    // happens to depend on something is a package that stops building the day
    // the sibling stops.
    let wanted = reachable(&root_name, &edges);
    let packages =
        resolved.into_iter().filter(|(name, _)| wanted.contains(name)).map(|(_, p)| p).collect();

    Ok(Resolution { packages, lockfile, changed, stray_locks })
}

/// The name a manifest goes by when saying who asked for a dependency.
///
/// A workspace root has no package name, and the name is only ever used to say
/// *who asked* -- for which the directory is a perfectly good answer and better
/// than inventing one.
fn holder_name(manifest: &Manifest, directory: &Path) -> String {
    match manifest.package() {
        Some(package) => package.name.clone(),
        None => directory
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "the workspace".to_string()),
    }
}

/// Every package `start` reaches, following the graph the resolution built.
fn reachable(
    start: &str,
    edges: &BTreeMap<String, BTreeMap<String, ()>>,
) -> std::collections::BTreeSet<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = VecDeque::from([start.to_string()]);
    while let Some(here) = queue.pop_front() {
        for name in edges.get(&here).into_iter().flatten().map(|(name, ())| name) {
            if seen.insert(name.clone()) {
                queue.push_back(name.clone());
            }
        }
    }
    seen
}

/// Whether two paths name the same directory.
fn same(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => left == right,
    }
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
    let parsed = Manifest::load(path).map_err(|e| anyhow::anyhow!("{e}"))?;
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
    if !manifest.is_file() {
        bail!(
            "`{name}` has no `khora.toml` at {}. A git dependency names a repository, and \
             the package inside it may need `subdir` to say where",
            root.display()
        )
    }
    let parsed = khora_manifest::Manifest::load(&manifest)
        .map_err(|e| anyhow::anyhow!("reading `{name}`'s manifest: {e}"))?;
    if parsed.manifest.package().is_some_and(|p| p.publish == Some(true)) {
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
