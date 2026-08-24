//! Getting a package's files onto this machine.
//!
//! # Why `git` the program, and not a git library
//!
//! Linking one would add a large dependency and a second implementation of
//! authentication, and it would still be worse at the job: whoever runs this
//! already has git configured — credentials, SSH agents, proxies, `insteadOf`
//! rewrites, whatever their company does — and shelling out inherits all of it
//! for free. The cost is that git has to be on the path, which is true of
//! everyone who obtained Khora, and an error message says so plainly.
//!
//! The checkout is deliberately shallow and detached. Nothing here wants
//! history, and a working copy with a branch checked out is a working copy
//! somebody might expect to be able to commit in.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// The full commit id `rev` names in `url`.
///
/// Resolved before anything is written down, so a lockfile never records a
/// branch or a tag — both of which can be moved, which would make a locked
/// build mean something different tomorrow.
pub fn resolve_revision(url: &str, rev: &str) -> Result<String> {
    // Already a full id? `ls-remote` will not find it, and asking is pointless.
    if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(rev.to_string());
    }

    let out = git(&["ls-remote", url, rev], None)
        .with_context(|| format!("asking {url} what `{rev}` is"))?;
    let Some(line) = out.lines().next() else {
        bail!("{url} has no `{rev}`. Check the tag or branch name")
    };
    let Some(id) = line.split_whitespace().next() else {
        bail!("`git ls-remote {url} {rev}` answered `{line}`, which has no commit id in it")
    };
    Ok(id.to_string())
}

/// Checks `rev` out of `url` into `into`, which must already exist and be empty.
pub fn checkout(url: &str, rev: &str, into: &Path) -> Result<()> {
    git(&["init", "--quiet"], Some(into))?;
    git(&["remote", "add", "origin", url], Some(into))?;

    // `--depth 1` of one revision: no history, no other branches. A tag or a
    // branch name would work here too, but `rev` is a full commit id by the
    // time it arrives, because `resolve_revision` ran first.
    git(&["fetch", "--quiet", "--depth", "1", "origin", rev], Some(into))
        .with_context(|| format!("fetching {rev} from {url}"))?;
    git(&["checkout", "--quiet", "--detach", "FETCH_HEAD"], Some(into))
        .with_context(|| format!("checking out {rev}"))?;

    // The store hashes what is here, and `.git` holds a different hash of the
    // same content — plus pack files whose bytes depend on how the fetch went.
    // Leaving it would make one revision hash differently on two machines.
    std::fs::remove_dir_all(into.join(".git"))
        .with_context(|| format!("removing the .git directory under {}", into.display()))?;
    Ok(())
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let output = command.output().map_err(|e| {
        anyhow::anyhow!(
            "could not run `git`: {e}. Khora resolves git dependencies by running it, so \
             it has to be on the path"
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`git {}` failed:\n{}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
