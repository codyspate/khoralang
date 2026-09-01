//! Two compile-time inputs that cargo would otherwise not know about.
//!
//! `RUNNING` reads `KHORA_RELEASE` with `option_env!`, which is resolved at
//! compile time — so without `rerun-if-env-changed` the binary keeps reporting
//! whatever it was first built with, silently, and it looks like the packaging
//! script not working.
//!
//! `KHORA_TARGET` is the triple this build is *for*, which release archives are
//! named with. `std::env::consts` cannot produce it: it has no way to tell
//! `x86_64-pc-windows-msvc` from `x86_64-pc-windows-gnu`, and those are
//! different downloads. Cargo hands it to a build script and nowhere else.
//!
//! `KHORA_VERSION_LINE` is what `khora --version` prints. **A bare version is
//! not enough to act on a bug report**: two builds of `0.1.0` from either side
//! of a fix report the same thing, and a report against "0.1.0" from a
//! development checkout names a compiler nobody else has. So the commit and
//! the target travel with it.
fn main() {
    println!("cargo:rerun-if-env-changed=KHORA_RELEASE");
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=KHORA_TARGET={target}");

    let version = std::env::var("KHORA_RELEASE")
        .ok()
        .filter(|named| !named.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());

    // **`git` is not a build dependency.** A release is packaged from a
    // checkout and a user may be building from an unpacked archive with no
    // repository at all, so a missing commit is an ordinary outcome and not a
    // failure. `unknown` is honest and still leaves the version and the target.
    let commit = commit().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KHORA_VERSION_LINE={version} ({commit}) {target}");
}

/// The short commit this was built from, with a `-dirty` marker if the tree had
/// uncommitted changes.
///
/// The marker matters more than it looks: most bug reports during a release
/// candidate come from people building the compiler, and "0.1.0 (a1b2c3d)"
/// against a tree that is not `a1b2c3d` is a report nobody can reproduce.
fn commit() -> Option<String> {
    // Rebuild when HEAD moves. `git rev-parse --git-path HEAD` resolves a
    // worktree's own HEAD, which a plain `.git/HEAD` does not.
    for path in ["HEAD", "logs/HEAD"] {
        if let Some(found) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={found}");
        }
    }
    let short = git(&["rev-parse", "--short", "HEAD"])?;
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|out| !out.is_empty());
    Some(if dirty { format!("{short}-dirty") } else { short })
}

/// One `git` command, or `None` for any reason at all.
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
