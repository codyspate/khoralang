//! `khora release`: what changed, and what the next version should be.
//!
//! Roadmap 14.20. `docs/design/releasing.md` is the argument; this is the
//! implementation of the four decisions it settles.
//!
//! # What it does not do
//!
//! **It never tags and it never pushes.** `.github/workflows/release.yml`
//! deliberately puts a person between "built" and "visible": a draft is
//! created, the workflow fills it, somebody looks, somebody presses Publish.
//! A tool that tagged would be a tool that could publish a mistake at three in
//! the morning, which is the thing that flow exists to prevent.
//!
//! So this reports, and on request writes one number into one manifest. The
//! tag stays a `git tag` a person types.
//!
//! # One version for the repository
//!
//! Members share `[workspace.package] version`, and this keeps them in step.
//! Independent per-member versions need a registry to mean anything: a Khora
//! dependency is a git URL and a revision, so a consumer pins a tag, and **one
//! repository tag already names exactly one state of every member**. Tags like
//! `postgres-v0.3.0` would have to be learned by the resolver, the installer
//! and every consumer's `rev`, in exchange for a distinction nothing can
//! currently observe. Revisit when there is a registry.
//!
//! # What counts as a change
//!
//! 14.16's selection, unchanged — including its rule that a changed file
//! belonging to no member and to nothing a member depends on selects
//! **everything**. A release tool that quietly decided otherwise would be
//! wrong in the most expensive direction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use khora_manifest::Version;

use crate::affected;

/// Which part of the version a release moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Major,
    Minor,
    Patch,
}

impl Step {
    /// `version`, moved.
    fn applied(self, version: &Version) -> Version {
        let mut next = version.clone();
        next.pre = None;
        match self {
            Step::Major => {
                next.major += 1;
                next.minor = 0;
                next.patch = 0;
            }
            Step::Minor => {
                next.minor += 1;
                next.patch = 0;
            }
            Step::Patch => next.patch += 1,
        }
        next
    }
}

/// Reports what a release would contain, and optionally writes the version.
pub fn release(
    path: &Path,
    since: &str,
    step: Option<Step>,
    notes: Option<&Path>,
    members: &[PathBuf],
) -> Result<bool> {
    let manifest = path.join("khora.toml");
    let parsed = khora_manifest::Manifest::load(&manifest).map_err(|e| anyhow::anyhow!("{e}"))?;
    let current = parsed
        .manifest
        .workspace
        .as_ref()
        .and_then(|table| table.package.as_ref())
        .and_then(|shared| shared.version.clone())
        .with_context(|| {
            format!(
                "{} has no `[workspace.package] version`, so there is one version per member \
                 and nothing for this to move. `docs/design/releasing.md` says why the \
                 lockstep shape is the one supported",
                manifest.display()
            )
        })?;
    let version = Version::parse(&current)
        .map_err(|why| anyhow::anyhow!("`{current}` is not a version: {why}"))?;

    let selection = affected::select(path, members, since)?;
    let commits = commits_by_member(path, since, members)?;

    println!("{} member(s), {} changed since {since}", members.len(), selection.members.len());
    println!();

    if let Some(file) = &selection.everything_because {
        println!(
            "  every member, because {} is outside all of them",
            file.display()
        );
        println!("  and outside anything they depend on.");
        println!();
    }

    if selection.members.is_empty() {
        println!("  nothing has changed. There is nothing to release.");
        return Ok(true);
    }

    println!("  changed");
    for member in &selection.members {
        let how_many = commits.get(member).map_or(0, Vec::len);
        println!("    {:<40} {} commit(s)", member.display(), how_many);
    }
    if !selection.skipped.is_empty() {
        println!();
        println!("  unchanged");
        for member in &selection.skipped {
            println!("    {}", member.display());
        }
    }

    println!();
    println!("  version   {version}");
    match step {
        None => {
            println!("  next      you choose: --major, --minor or --patch");
            println!();
            // Not a default, and not inferred. `docs/design/compatibility.md`
            // is explicit that a bug fix is not automatically a patch release:
            // if a program could reasonably have been written against the old
            // behaviour, correcting it is major however wrong it was. That is
            // a judgement about observable behaviour, and a tool that guessed
            // would be guessing about the one thing it cannot see.
            println!("  Which one is a judgement about observable behaviour, so this does not");
            println!("  guess. `docs/design/compatibility.md` has the rule, including that a");
            println!("  bug fix is not automatically a patch.");
        }
        Some(step) => {
            let next = step.applied(&version);
            println!("  next      {next}");
            write_version(&manifest, &current, &next.to_string())?;
            println!();
            println!("  written to {}", manifest.display());
            println!("  Nothing is tagged. `git tag v{next}` when the notes are done.");
        }
    }

    if let Some(out) = notes {
        let step = step.with_context(|| {
            "writing notes needs to know the version, so pass --major, --minor or --patch too"
        })?;
        let next = step.applied(&version);
        write_notes(out, &next.to_string(), since, &selection.members, &commits)?;
        println!();
        println!("  notes drafted in {}", out.display());
        println!("  They are a draft. See the empty section it left you.");
    }

    Ok(true)
}

/// The commit subjects touching each member since `since`.
fn commits_by_member(
    root: &Path,
    since: &str,
    members: &[PathBuf],
) -> Result<BTreeMap<PathBuf, Vec<String>>> {
    let mut out: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for member in members {
        let subjects = git(
            root,
            &[
                "log",
                "--format=%s",
                &format!("{since}..HEAD"),
                "--",
                &member.to_string_lossy(),
            ],
        )?;
        let list: Vec<String> =
            subjects.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
        if !list.is_empty() {
            out.insert(member.clone(), list);
        }
    }
    Ok(out)
}

/// Rewrites the one `version = "..."` under `[workspace.package]`.
///
/// Textually, and deliberately: re-serializing the manifest would reformat a
/// file full of comments that were written to be read, and a release that
/// reflowed the reasoning in `khora.toml` would be a bad trade for one number.
fn write_version(manifest: &Path, from: &str, to: &str) -> Result<()> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let needle = format!("version = \"{from}\"");
    let hits = text.matches(&needle).count();
    if hits != 1 {
        bail!(
            "expected exactly one `{needle}` in {}, found {hits}. Edit it by hand; a release \
             tool guessing which one you meant is worse than one that stops",
            manifest.display()
        );
    }
    std::fs::write(manifest, text.replace(&needle, &format!("version = \"{to}\"")))
        .with_context(|| format!("writing {}", manifest.display()))
}

/// Writes the notes skeleton.
///
/// **A skeleton, and it says so.** `docs/design/compatibility.md` requires that
/// before 1.0 every change altering what a valid program does is named in the
/// notes, with the old behaviour and the new one. That is prose, and no tool
/// writes it. What a tool can do is group the commit subjects and leave a
/// required section empty — an empty required section is the only honest thing
/// it can say, and it says "you are not done".
fn write_notes(
    out: &Path,
    version: &str,
    since: &str,
    changed: &[PathBuf],
    commits: &BTreeMap<PathBuf, Vec<String>>,
) -> Result<()> {
    let mut text = format!("# {version}\n\n## Behaviour changes\n\n");
    text.push_str(
        "<!-- Required before 1.0: every change that alters what a valid program does,\n\
         \x20    with the old behaviour and the new one. `docs/design/compatibility.md`.\n\
         \x20    An empty section here means the release is not ready, not that there\n\
         \x20    were none -- say \"none\" if there were none. -->\n\n",
    );
    text.push_str(&format!("## What changed, by member (since {since})\n\n"));
    for member in changed {
        let Some(subjects) = commits.get(member) else { continue };
        text.push_str(&format!("### {}\n\n", member.display()));
        for subject in subjects {
            text.push_str(&format!("- {subject}\n"));
        }
        text.push('\n');
    }
    text.push_str(
        "<!-- Commit subjects, grouped. They are a starting point: a subject line\n\
         \x20    says what changed and not what it changed *from*, which is what the\n\
         \x20    section above needs. -->\n",
    );
    std::fs::write(out, text).with_context(|| format!("writing {}", out.display()))
}

/// Runs git in `root` and returns its output.
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let done = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .context("running git, which `khora release` needs in order to know what changed")?;
    if !done.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&done.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&done.stdout).into_owned())
}
