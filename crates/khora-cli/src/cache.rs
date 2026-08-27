//! The build cache.
//!
//! `khora build` over inputs it has already built returns the artifact it
//! produced last time instead of producing it again. Roadmap 14.17.
//!
//! # Why this one is a proof and most are a bet
//!
//! Every build cache is a bet that the key captures everything that could
//! change the output. Turborepo hashes inputs and hopes; the usual way it
//! loses is a toolchain difference nobody hashed. 13.10 made
//! `KHORA_PROFILE=release` bit-for-bit reproducible — measured, not assumed —
//! so for that profile the bet is settleable: **the same key provably produces
//! the same bytes**, and the test that builds twice and compares proves it
//! rather than asserting it.
//!
//! So the key has to be honest, and the interesting part is what is in it
//! beyond the source:
//!
//! - **The compiler binary itself**, hashed, not `khora --version`. A version
//!   string is constant across every dev build out of `target/debug`, and a
//!   cache that served an artifact from the compiler you had ten minutes ago
//!   would be worse than no cache in exactly the repository that has to trust
//!   it most.
//! - **The linker binary.** Khora emits an object and a C driver links it, so
//!   the driver's bytes are in the output's. This is the difference the
//!   roadmap says other caches lose to.
//! - **The runtime archive.** Every executable links `khora-rt` statically.
//! - **The target triple, the profile, and whether debug information is on**,
//!   because `KHORA_DEBUG` overrides the profile in both directions.
//!
//! Hashing two large binaries on every build would cost more than it saves, so
//! a file's digest is memoised against its size and modification time. The
//! first build after a compiler changes pays for the hash; every one after it
//! pays for a `stat`.
//!
//! **A stat is not a content hash, and pretending otherwise is how a cache
//! goes wrong.** Two writes inside one filesystem timestamp tick are
//! indistinguishable, so a memo trusted on size and mtime alone can describe
//! contents that are no longer there. Git has the same problem and calls those
//! entries *racily clean*; the answer here is the same shape. The memo is only
//! believed when it was recorded **strictly after** the file it describes was
//! last written — the memo file's own mtime is the record of when the hash was
//! taken, and a subject that is not older than its record may have changed
//! since. In the steady state the compiler was built minutes ago and every
//! build pays for a `stat`; right after a rebuild, which is exactly when it
//! matters, the memo is distrusted and the file is read.
//!
//! A unit test in this module writes two different three-byte files in
//! microseconds, which is the degenerate case a size-and-mtime memo gets
//! wrong. It was written to check the claim and it failed, which is why the
//! rule above exists.
//!
//! # Debug information puts paths in the key
//!
//! A debug build embeds each source file's absolute path in DWARF or a PDB, so
//! two checkouts of identical content do not produce identical artifacts.
//! When debug information is on, the paths go into the key; when it is off
//! they do not, and two checkouts share an entry. That is not a special case
//! bolted on — it is the same rule as everything else here, which is that the
//! key holds exactly what the output depends on.
//!
//! Note the honest limit that follows: **`debug` is not bit-for-bit
//! reproducible on Windows** — 12.9 measured it, and what varies is inside
//! lld-link's PDB emission. A debug hit therefore returns *an* artifact built
//! from these inputs by this toolchain, which is what a fresh build would also
//! have given you, but not necessarily the same bytes as one run right now.
//! The release claim is the strong one.
//!
//! # A file that has just been linked is not always readable yet
//!
//! On Windows an executable the linker finished writing a moment ago is
//! routinely held open by something else — a virus scanner, the search
//! indexer — for long enough that opening it to read fails. Under a parallel
//! test run that stops being rare, and the symptom is the worst kind: a build
//! that silently does not go into the cache, and a next build that silently
//! misses.
//!
//! Every read of a freshly written artifact therefore retries for a fraction
//! of a second before giving up. That is an accommodation for one platform's
//! behaviour and not a workaround for a race of our own: nothing here writes a
//! file another part of this process is reading.
//!
//! # A cache never fails a build
//!
//! An unwritable directory, a corrupt entry, a linker that cannot be found:
//! every one of those is a miss and at most a warning. The moment a cache can
//! break a build is the moment people start passing `--no-cache` by reflex,
//! and then it may as well not exist.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// What a build is, for the purpose of naming its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An executable.
    Executable,
    /// A shared library, which also has a C header beside it.
    Library,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Executable => "exe",
            Kind::Library => "lib",
        }
    }
}

/// Everything a build's output depends on.
pub struct Inputs<'a> {
    /// Every source file the build compiles, with its path.
    pub sources: &'a [(PathBuf, String)],
    /// `debug` or `release`.
    pub profile: &'static str,
    /// Whether debug information is emitted.
    pub debug_info: bool,
    /// Executable or library.
    pub kind: Kind,
}

/// The cache directory, and the questions asked of it.
pub struct Cache {
    root: PathBuf,
}

/// Why a lookup did not answer.
///
/// **A cache that cannot say why it missed is a cache nobody can maintain.**
/// Set `KHORA_CACHE_EXPLAIN=1` and `khora build` prints the key and this. It
/// exists because a flaky test said "built" where it expected "reused" and
/// there was nothing else to read.
#[derive(Debug)]
pub enum Miss {
    /// Nothing has been built with this key.
    NoEntry,
    /// The entry is there and does not record what it holds.
    NoRecord,
    /// The entry records an artifact it does not have.
    NoArtifact,
    /// The artifact could not be read.
    Unreadable(String),
    /// The artifact is not the one the entry recorded.
    Mismatch,
}

impl std::fmt::Display for Miss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Miss::NoEntry => f.write_str("nothing has been built with this key"),
            Miss::NoRecord => f.write_str("the entry does not say what it holds"),
            Miss::NoArtifact => f.write_str("the entry has no artifact"),
            Miss::Unreadable(why) => write!(f, "the artifact could not be read: {why}"),
            Miss::Mismatch => f.write_str("the artifact is not what the entry recorded"),
        }
    }
}

/// One entry, found.
pub struct Hit {
    /// The artifact itself.
    pub artifact: PathBuf,
    /// The C header, for a library build.
    pub header: Option<PathBuf>,
    /// The key, for a message.
    pub key: String,
}

impl Cache {
    /// The cache under `$KHORA_HOME/cache`, or `~/.khora/cache`.
    pub fn open() -> Result<Cache> {
        // `home` already reads `KHORA_HOME`, which is how the tests point
        // every part of the toolchain at one scratch directory at once.
        let root = khora_toolchain::home()?.join("cache");
        std::fs::create_dir_all(root.join("build"))
            .with_context(|| format!("creating the build cache at {}", root.display()))?;
        std::fs::create_dir_all(root.join("ids"))?;
        Ok(Cache { root })
    }

    /// Where the cache lives, for `khora cache`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The key for a build, or `None` if the toolchain cannot be identified.
    ///
    /// `None` rather than an error: a build with no linker is about to fail
    /// for a reason the cache is not, and a cache that turns "no linker found"
    /// into "could not compute a cache key" has buried the message that
    /// matters.
    pub fn key(&self, inputs: &Inputs<'_>) -> Option<String> {
        let compiler = self.identity(&std::env::current_exe().ok()?)?;
        let linker = self.identity(&khora_codegen_llvm::toolchain::linker()?)?;
        // A library links the runtime too, so it is in both keys. A build that
        // cannot find the archive is about to fail; miss, and let it say so.
        let runtime = self.identity(&khora_codegen_llvm::toolchain::runtime_archive()?)?;

        let mut hasher = Sha256::new();
        let mut field = |bytes: &[u8]| {
            // Length-prefixed, so that two adjacent fields cannot be slid into
            // each other -- the same reason `khora_pkg::hash` does it.
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        };

        if Cache::explaining() {
            eprintln!(
                "khora: key from compiler {} linker {} runtime {} target {:?} profile {} \
                 debug {} kind {}",
                &compiler[..12],
                &linker[..12],
                &runtime[..12],
                khora_db::target_triple(),
                inputs.profile,
                inputs.debug_info,
                inputs.kind.name()
            );
        }
        field(b"khora-build-cache/1");
        field(compiler.as_bytes());
        field(linker.as_bytes());
        field(runtime.as_bytes());
        field(khora_db::target_triple().unwrap_or_default().as_bytes());
        field(inputs.profile.as_bytes());
        field(&[u8::from(inputs.debug_info)]);
        field(inputs.kind.name().as_bytes());

        // Sorted by content rather than by path, so the order does not depend
        // on where the checkout is. Paths join the key only when debug
        // information is on, because that is the only thing that puts them in
        // the output.
        let mut sources: Vec<&(PathBuf, String)> = inputs.sources.iter().collect();
        sources.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        field(&(sources.len() as u64).to_le_bytes());
        for (path, text) in sources {
            if Cache::explaining() {
                let mut one = Sha256::new();
                one.update(text.as_bytes());
                eprintln!(
                    "khora:   source {} {:x}",
                    absolute(path).display(),
                    one.finalize()
                );
            }
            if inputs.debug_info {
                // **Absolute.** `khora build .` hands the compiler relative
                // paths -- `.\src\main.kh` -- and a relative path is the same
                // string in every project, so hashing it would have made this
                // whole clause do nothing: two checkouts with identical
                // content would share a debug entry, and one would get the
                // other's paths inside its debug information. Found by two
                // separate tests reporting the same key.
                field(absolute(path).to_string_lossy().as_bytes());
            }
            field(text.as_bytes());
        }

        Some(format!("{:x}", hasher.finalize()))
    }

    /// The entry for `key`, if there is a sound one, and why not if not.
    ///
    /// The entry for `key`, if there is a sound one.
    ///
    /// The artifact is hashed and compared against what the entry recorded. A
    /// rename is atomic so a half-written entry should not exist, but "should
    /// not" is what a cache says right before it hands somebody a truncated
    /// binary, and the check costs milliseconds against a build.
    pub fn lookup(&self, key: &str) -> Result<Hit, Miss> {
        let entry = self.root.join("build").join(key);
        if !entry.is_dir() {
            return Err(Miss::NoEntry);
        }
        let artifact = entry.join("artifact");
        let Ok(recorded) = std::fs::read_to_string(entry.join("artifact.sha256")) else {
            return Err(Miss::NoRecord);
        };
        if !artifact.is_file() {
            return Err(Miss::NoArtifact);
        }
        match hash_file(&artifact) {
            Err(why) => return Err(Miss::Unreadable(format!("{why:#}"))),
            Ok(found) if found != recorded.trim() => return Err(Miss::Mismatch),
            Ok(_) => {}
        }
        let header = entry.join("header.h");
        Ok(Hit {
            artifact,
            header: header.is_file().then_some(header),
            key: key.to_string(),
        })
    }

    /// Whether `KHORA_CACHE_EXPLAIN` asked for the key and the verdict.
    pub fn explaining() -> bool {
        std::env::var_os("KHORA_CACHE_EXPLAIN").is_some_and(|value| value != "0")
    }

    /// Records a freshly built artifact under `key`.
    ///
    /// Built under a temporary name and renamed, so an interrupted store
    /// leaves a stray directory rather than an entry a later build trusts.
    /// Two processes racing both succeed: the loser's rename fails and the
    /// answer it wanted is what the winner put there.
    pub fn store(&self, key: &str, artifact: &Path, header: Option<&Path>) -> Result<()> {
        let destination = self.root.join("build").join(key);
        if destination.is_dir() {
            return Ok(());
        }
        let staged = self.root.join("build").join(format!(".tmp-{key}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staged);
        std::fs::create_dir_all(&staged)?;

        let stored = staged.join("artifact");
        copy(artifact, &stored)
            .with_context(|| format!("copying {} into the cache", artifact.display()))?;
        // **The copy is hashed, not the original.** Hashing the source and
        // storing the destination records a digest for bytes that are only
        // assumed to be the same ones, and a later lookup that compares them
        // and disagrees becomes a silent miss with nothing to read.
        std::fs::write(staged.join("artifact.sha256"), hash_file(&stored)?)?;
        if let Some(header) = header {
            if header.is_file() {
                copy(header, &staged.join("header.h"))?;
            }
        }

        // Retried, because **renaming a directory on Windows fails while
        // anything inside it is open** and the artifact was written seconds
        // ago -- see the module comment. Swallowed only when the destination
        // is now there, which is the lost-race case and the only one where
        // there is nothing to say; anything else is a cache that quietly
        // stopped working, which is how a build cache dies.
        if retrying(|| std::fs::rename(&staged, &destination)).is_err() {
            let _ = std::fs::remove_dir_all(&staged);
            if !destination.is_dir() {
                anyhow::bail!("could not move the entry into {}", destination.display());
            }
        }
        Ok(())
    }

    /// Puts a hit where the build was going to put its output.
    pub fn place(hit: &Hit, target: &Path) -> Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Copied rather than linked: the output belongs to whoever asked for
        // it, and a hard link would let `strip` reach into the cache.
        copy(&hit.artifact, target)
            .with_context(|| format!("writing {}", target.display()))?;
        if let Some(header) = &hit.header {
            copy(header, &target.with_extension("h"))?;
        }
        Ok(())
    }

    /// How many entries there are and what they occupy.
    pub fn size(&self) -> (usize, u64) {
        let mut entries = 0;
        let mut bytes = 0;
        let Ok(read) = std::fs::read_dir(self.root.join("build")) else { return (0, 0) };
        for entry in read.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            entries += 1;
            if let Ok(inner) = std::fs::read_dir(entry.path()) {
                for file in inner.flatten() {
                    bytes += file.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        (entries, bytes)
    }

    /// The keys the cache holds, for a diagnostic.
    pub fn keys(&self) -> Vec<String> {
        let Ok(read) = std::fs::read_dir(self.root.join("build")) else { return Vec::new() };
        let mut out: Vec<String> = read
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    /// Removes every entry, and the memoised digests with them.
    pub fn clear(&self) -> Result<()> {
        for name in ["build", "ids"] {
            let directory = self.root.join(name);
            if directory.is_dir() {
                std::fs::remove_dir_all(&directory)
                    .with_context(|| format!("clearing {}", directory.display()))?;
            }
            std::fs::create_dir_all(&directory)?;
        }
        Ok(())
    }

    /// A file's digest, remembered against its size and modification time.
    ///
    /// See the module comment for why the memo is only believed when it was
    /// written strictly after the file it describes.
    fn identity(&self, path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let written = modified(&meta);

        let mut naming = Sha256::new();
        naming.update(path.to_string_lossy().as_bytes());
        naming.update(meta.len().to_le_bytes());
        naming.update(written.to_le_bytes());
        let memo = self.root.join("ids").join(format!("{:x}", naming.finalize()));

        // Recorded *after* the subject was last written, or not believed. A
        // memo whose own mtime is not strictly greater describes a file that
        // may have changed inside the same tick.
        let recorded = std::fs::metadata(&memo).ok().map(|meta| modified(&meta));
        if let (Some(recorded), Ok(known)) = (recorded, std::fs::read_to_string(&memo)) {
            if recorded > written {
                return Some(known.trim().to_string());
            }
        }

        let digest = hash_file(path).ok()?;
        let _ = std::fs::write(&memo, &digest);
        Some(digest)
    }
}

/// A path as the filesystem knows it, for a key that must not depend on where
/// the command was run.
///
/// Canonicalized where possible, and joined onto the working directory when
/// not -- a file that cannot be canonicalized is one the build is about to
/// fail on, and the spelling is the clue.
fn absolute(path: &Path) -> PathBuf {
    if let Ok(found) = path.canonicalize() {
        return khora_manifest::readable(found);
    }
    match std::env::current_dir() {
        Ok(here) => here.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// When a file was last written, in nanoseconds since the epoch.
///
/// Zero for a filesystem that does not answer, which makes every memo about
/// that file untrusted -- the safe direction, and the one that costs only
/// time.
fn modified(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or(0)
}

/// The SHA-256 of a file, streamed rather than read whole.
fn hash_file(path: &Path) -> Result<String> {
    let mut file = retrying(|| std::fs::File::open(path))
        .with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("hashing {}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// `std::fs::copy`, patient about a file somebody else has open.
fn copy(from: &Path, to: &Path) -> Result<()> {
    retrying(|| std::fs::copy(from, to))
        .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    Ok(())
}

/// Runs `op` until it stops failing, for about a third of a second.
///
/// For the Windows behaviour the module comment describes. Short enough that a
/// genuine failure -- a path that is not there, a directory with no room --
/// still reports promptly, and long enough to outlast a scanner holding a
/// freshly linked executable.
fn retrying<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut waited = std::time::Duration::from_millis(5);
    for _ in 0..6 {
        match op() {
            Ok(value) => return Ok(value),
            Err(_) => std::thread::sleep(waited),
        }
        waited *= 2;
    }
    op()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> Cache {
        let root = std::env::temp_dir().join("khora-cache-unit").join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("build")).expect("a cache directory");
        std::fs::create_dir_all(root.join("ids")).expect("a memo directory");
        Cache { root }
    }

    /// The memo decides whether to *re-read* a file, never what is in it.
    ///
    /// This is the whole of why hashing the compiler against its size and
    /// modification time is not the shortcut it looks like, so it is worth a
    /// test rather than a paragraph.
    #[test]
    fn a_memoised_digest_follows_the_contents_and_not_the_stat() {
        let cache = scratch("memo");
        let file = cache.root.join("subject");
        std::fs::write(&file, b"one").expect("a file");

        let first = cache.identity(&file).expect("a digest");
        let again = cache.identity(&file).expect("a digest");
        assert_eq!(first, again, "an unchanged file keeps its digest");

        std::fs::write(&file, b"two").expect("a change");
        let changed = cache.identity(&file).expect("a digest");
        assert_ne!(first, changed, "changed contents must change the digest");

        std::fs::write(&file, b"one").expect("undone");
        assert_eq!(cache.identity(&file).expect("a digest"), first, "and change back");
    }

    /// A memo written before its subject is not believed.
    ///
    /// The record has to be newer than the thing it records, or it describes
    /// contents that may have been replaced inside one timestamp tick.
    #[test]
    fn a_memo_older_than_its_subject_is_ignored() {
        let cache = scratch("stale_memo");
        let file = cache.root.join("subject");
        std::fs::write(&file, b"first").expect("a file");
        let recorded = cache.identity(&file).expect("a digest");

        // Same length, so the memo's name is unchanged if the clock does not
        // move; the guard is what has to catch this.
        std::fs::write(&file, b"secnd").expect("a replacement");
        assert_ne!(cache.identity(&file).expect("a digest"), recorded);
    }

    #[test]
    fn a_file_that_is_not_there_has_no_identity() {
        let cache = scratch("missing");
        assert!(cache.identity(&cache.root.join("nothing")).is_none());
    }
}
