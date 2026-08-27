//! Adding a dependency to a manifest.
//!
//! `khora install <url>` is a convenience over editing `khora.toml` by hand,
//! and it is worth being clear that it is only that. There is no registry, so
//! there is no name to look a package up by; the URL is the name. What the
//! command buys is the three things a person gets wrong doing it manually:
//!
//! - **The package's real name.** A dependency's key has to match what the
//!   package calls itself, and the only way to know that is to look. Guessing
//!   from the last segment of a URL is right often enough to be a trap.
//! - **Whether it is offered at all.** A repository says which of the things in
//!   it are libraries with `publish = true`; finding that out before the entry
//!   is written is better than a failed build afterwards.
//! - **Where in the repository it is.** A git URL names a repository, so a
//!   package in a subdirectory needs `subdir`, and forgetting it produces a
//!   confusing error about the wrong package.
//!
//! The inspection here is a second shallow fetch of one commit -- `resolve`
//! fetches again afterwards to do the real work. That is a little wasteful and
//! deliberately so: the alternative is writing a lockfile entry by hand here so
//! the resolver's store lookup hits, which would put the store's invariants in
//! two places to save a fetch of one commit with no history.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use khora_manifest::Manifest;

use crate::{fetch, Store};

/// What `install` did, for the caller to report.
pub struct Installed {
    /// The name the package calls itself, which is the key that was written.
    pub name: String,
    /// Its version, for the sake of saying so.
    pub version: String,
    /// The commit `rev` named, resolved before anything was written down.
    pub revision: String,
    /// What the manifest edit came to.
    pub outcome: Outcome,
}

/// What writing the dependency in amounted to.
///
/// `Unchanged` is worth distinguishing from `Updated`: running the same install
/// twice is a common thing to do, and reporting it as a change teaches people
/// to distrust the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// There was no entry of this name.
    Added,
    /// There was one, naming something else.
    Updated,
    /// There was one, saying exactly this.
    Unchanged,
}

impl Outcome {
    /// The past-tense verb to report it with.
    pub fn verb(self) -> &'static str {
        match self {
            Outcome::Added => "added",
            Outcome::Updated => "updated",
            Outcome::Unchanged => "already had",
        }
    }
}

/// Adds a git dependency to the manifest at `manifest_path`.
///
/// Fetches first and writes second, so a URL that does not offer a package
/// leaves the manifest untouched rather than needing to be undone.
pub fn install(
    manifest_path: &Path,
    url: &str,
    rev: &str,
    subdir: Option<&str>,
    store: &Store,
) -> Result<Installed> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    // Parsed only to fail early on a manifest this cannot safely edit.
    Manifest::load(manifest_path).map_err(|e| anyhow::anyhow!("{e}"))?;

    let revision = fetch::resolve_revision(url, rev)?;
    let offered = inspect(url, &revision, subdir, store)?;

    let (edited, outcome) = with_dependency(&text, &offered.name, url, rev, subdir);
    if outcome != Outcome::Unchanged {
        std::fs::write(manifest_path, edited)
            .with_context(|| format!("writing {}", manifest_path.display()))?;
    }

    Ok(Installed { name: offered.name, version: offered.version, revision, outcome })
}

struct Offered {
    name: String,
    version: String,
}

/// Fetches the revision and reads what the package says about itself.
fn inspect(url: &str, revision: &str, subdir: Option<&str>, store: &Store) -> Result<Offered> {
    let staged = store.staging("install")?;
    fetch::checkout(url, revision, &staged)?;

    let root: PathBuf = match subdir {
        Some(inner) => staged.join(inner),
        None => staged.clone(),
    };
    let manifest = root.join("khora.toml");
    if !manifest.is_file() {
        let hint = match subdir {
            Some(inner) => format!(
                "there is no `{inner}/khora.toml` in {url} at {revision}. Check the \
                 `--subdir` path"
            ),
            None => format!(
                "there is no `khora.toml` at the root of {url}. A git URL names a \
                 repository, not a package -- if the package is in a subdirectory, say \
                 which with `--subdir`"
            ),
        };
        let _ = std::fs::remove_dir_all(&staged);
        bail!("{hint}");
    }

    // Read before the checkout goes away, and *loaded* rather than parsed:
    // with `--subdir` this is a member of a monorepo, and its `version` may
    // live in the workspace root two directories up. The root is in the same
    // checkout, which is the only moment it is reachable.
    let parsed = Manifest::load(&manifest).map_err(|e| anyhow::anyhow!("{e}"));
    let _ = std::fs::remove_dir_all(&staged);
    let parsed = parsed?;
    let Some(package) = parsed.manifest.package else {
        bail!(
            "{} is a workspace root rather than a package: it has a `[workspace]` table and \
             no `[package]` one.\n\
             Depend on one of its members instead -- the URL needs a `subdir` naming which.",
            manifest.display()
        );
    };

    if package.publish != Some(true) {
        bail!(
            "`{}` does not offer itself as a package: its `khora.toml` has no \
             `publish = true` under `[package]`.\n\
             That flag is how a repository says which of the things in it are libraries -- \
             this one may be an application, or unfinished. Ask its author to add it, or \
             depend on a working copy with `path` if it is yours.",
            package.name
        );
    }

    Ok(Offered { name: package.name, version: package.version })
}

/// Returns the manifest with the dependency written in, and what that came to.
///
/// Textual rather than a TOML round trip on purpose: a manifest is a file a
/// person wrote, with their comments and their ordering in it, and reformatting
/// the whole thing to add one line is not a fair trade.
fn with_dependency(
    text: &str,
    name: &str,
    url: &str,
    rev: &str,
    subdir: Option<&str>,
) -> (String, Outcome) {
    let entry = match subdir {
        Some(inner) => {
            format!("{name} = {{ git = \"{url}\", rev = \"{rev}\", subdir = \"{inner}\" }}")
        }
        None => format!("{name} = {{ git = \"{url}\", rev = \"{rev}\" }}"),
    };

    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();

    let Some(header) = lines.iter().position(|l| l.trim() == "[dependencies]") else {
        // No table yet. Append one, with a blank line before it unless the file
        // already ends in one.
        let mut out = text.trim_end_matches(['\n', '\r']).to_string();
        out.push_str(newline);
        out.push_str(newline);
        out.push_str("[dependencies]");
        out.push_str(newline);
        out.push_str(&entry);
        out.push_str(newline);
        return (out, Outcome::Added);
    };

    // The table runs to the next header, or to the end.
    let end = lines[header + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |offset| header + 1 + offset);

    // An entry already there is replaced in place, which keeps the ordering and
    // is what somebody installing a different revision means.
    for line in lines[header + 1..end].iter_mut() {
        if key_of(line) == Some(name) {
            let same = line.trim() == entry;
            *line = entry;
            let outcome = if same { Outcome::Unchanged } else { Outcome::Updated };
            return (lines.join(newline) + newline, outcome);
        }
    }

    // Otherwise after the last non-blank line of the table, so a blank line
    // separating it from what follows stays where it was.
    let at = lines[header + 1..end]
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(header + 1, |offset| header + offset + 2);
    lines.insert(at, entry);
    (lines.join(newline) + newline, Outcome::Added)
}

/// The key a `name = ...` line assigns to, ignoring comments and blanks.
fn key_of(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim().trim_matches('"');
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::{with_dependency, Outcome};

    #[test]
    fn a_missing_table_is_created() {
        let (out, outcome) =
            with_dependency("[package]\nname = \"app\"\n", "pg", "u", "main", None);
        assert_eq!(outcome, Outcome::Added);
        assert_eq!(out, "[package]\nname = \"app\"\n\n[dependencies]\npg = { git = \"u\", rev = \"main\" }\n");
    }

    #[test]
    fn an_existing_table_is_added_to_and_the_comments_survive() {
        let before = "[package]\nname = \"app\"\n\n\
                      # what this needs\n[dependencies]\na = { path = \"../a\" }\n\n\
                      [permissions]\nextern = []\n";
        let (out, outcome) = with_dependency(before, "pg", "u", "main", Some("packages/pg"));
        assert_eq!(outcome, Outcome::Added);
        assert!(out.contains("# what this needs"), "comments should survive: {out}");
        assert!(out.contains("a = { path = \"../a\" }"), "so should the other entry: {out}");
        assert!(out.contains("[permissions]"), "and everything after: {out}");
        let deps = out.find("[dependencies]").expect("the table");
        let pg = out.find("pg = ").expect("the new entry");
        let perms = out.find("[permissions]").expect("the next table");
        assert!(deps < pg && pg < perms, "the entry belongs inside the table: {out}");
        assert!(out.contains("subdir = \"packages/pg\""));
    }

    /// Installing again at a different revision replaces rather than duplicates
    /// -- two entries of one name is a TOML error, so appending would produce a
    /// manifest that no longer parses.
    #[test]
    fn installing_over_an_entry_replaces_it() {
        let before = "[package]\nname = \"app\"\n\n[dependencies]\npg = { git = \"u\", rev = \"old\" }\n";
        let (out, outcome) = with_dependency(before, "pg", "u", "new", None);
        assert_eq!(outcome, Outcome::Updated);
        assert_eq!(out.matches("pg = ").count(), 1, "not twice: {out}");
        assert!(out.contains("rev = \"new\""));
    }

    #[test]
    fn windows_line_endings_stay_windows() {
        let (out, _) =
            with_dependency("[package]\r\nname = \"app\"\r\n", "pg", "u", "main", None);
        assert!(!out.contains('\n') || out.contains("\r\n"));
        assert_eq!(out.matches('\n').count(), out.matches("\r\n").count());
    }

    /// Running the same install twice is a common thing to do, and it should
    /// say so rather than claim a change.
    #[test]
    fn installing_the_same_entry_twice_changes_nothing() {
        let before = "[package]\nname = \"app\"\n";
        let (once, _) = with_dependency(before, "pg", "u", "main", None);
        let (twice, outcome) = with_dependency(&once, "pg", "u", "main", None);
        assert_eq!(outcome, Outcome::Unchanged);
        assert_eq!(once, twice);
    }
}
