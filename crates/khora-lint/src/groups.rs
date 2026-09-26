//! Lint groups, and the one answer to "how loud is this lint".
//!
//! A group is a TOML file naming some built-in lints and the level each runs
//! at when the group is switched on:
//!
//! ```toml
//! [group]
//! name = "strict"
//! description = "What this project holds itself to."
//!
//! [group.lints]
//! unused-binding = "deny"
//! ```
//!
//! The groups that ship with the toolchain are the files in `lints/` beside
//! `std`; a project adds its own under `[lint-groups]` in its manifest. **Both
//! go through the same reader**, so the built-in `idiomatic` group exercises
//! the path a project's group takes, and there is no second list of groups in
//! the compiler to disagree with the files.
//!
//! A group combines lints and never defines one. That is what keeps a group
//! file harmless: the worst it can do is make a real lint louder.
//!
//! # Precedence
//!
//! Most specific first, and TOML order never matters:
//!
//! 1. the pragma, `// @klint allow <lint>`, which [`crate::findings`] applies
//!    before any level is asked for;
//! 2. the lint's own entry in `[lints]`;
//! 3. an enabled group's explicit `level`;
//! 4. the lint's default inside an enabled group;
//! 5. the lint's base default, [`crate::default_level`].
//!
//! Where two enabled groups disagree at the step that decides, that is an
//! error naming both, fixed by a line at step 2. **There is no "last one
//! wins"**: the order of tables in a manifest is not something a reader
//! checks, so a rule keyed on it would decide a build on a detail nobody sees.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use khora_manifest::{LintLevel, Manifest};

use crate::{default_level, LINTS};

/// Keys a group's `[lints.<group>]` table will take in a later version.
///
/// Refused rather than ignored: a manifest written for that version and read
/// by this one would otherwise get none of what it asked for, silently.
pub const RESERVED_KEYS: &[&str] = &["exclude", "rules", "extends"];

/// A lint group read from its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The name `[lints.<name>]` switches it on by.
    pub name: String,
    /// One line on what the group is for.
    pub description: String,
    /// Each member, with the level it runs at when the group is on.
    pub members: BTreeMap<String, LintLevel>,
    /// The file it came from, for every message about it.
    pub file: PathBuf,
    /// Whether it ships with the toolchain rather than the project.
    pub built_in: bool,
}

/// A group file, or a `[lints]` entry, that cannot be used.
///
/// Always names the file and the key, because both kinds of file are edited
/// by hand and "invalid lint group" with no place to look is a search. A file
/// that does not parse has no key yet; it gets the line and column `toml`
/// reported instead, as a bad `khora.toml` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupError {
    /// The file the mistake is in.
    pub file: PathBuf,
    /// The key, dotted from the top of that file; empty when the file did
    /// not parse far enough to have one.
    pub key: String,
    /// Where in the file, one-based line and column, when that is known.
    pub position: Option<(usize, usize)>,
    /// What is wrong and what to write instead.
    pub message: String,
}

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file.display())?;
        if let Some((line, column)) = self.position {
            write!(f, ":{line}:{column}")?;
        }
        if !self.key.is_empty() {
            write!(f, ": `{}`", self.key)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for GroupError {}

fn error(file: &Path, key: impl Into<String>, message: impl Into<String>) -> GroupError {
    GroupError { file: file.to_path_buf(), key: key.into(), position: None, message: message.into() }
}

/// One-based line and column of byte `at` in `text`.
fn line_and_column(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |newline| newline + 1);
    (line, before[start..].chars().count() + 1)
}

/// A group file that does not parse, placed where `toml` stopped.
///
/// **A `package::group` member written bare is the one parse error with a
/// better answer than the parser's.** `acme::strict = "warn"` is not a TOML
/// key, so the reader meant a package's group and got "invalid unquoted key".
/// Said here as what it is -- not yet supported -- with the quoted spelling,
/// so the next edit is not a quoting fix that then meets "not yet" anyway.
fn unparsable(text: &str, file: &Path, why: &toml::de::Error) -> GroupError {
    let position = why.span().map(|span| line_and_column(text, span.start));
    if let Some((line, _)) = position {
        let written = text.lines().nth(line - 1).unwrap_or_default();
        if let Some((key, _)) = written.split_once('=') {
            let key = key.trim();
            if key.contains("::") && !key.starts_with('"') {
                let mut refused = package_group(file, &format!("group.lints.{key}"), key);
                refused.message.push_str(&format!(
                    ". (A key with `::` in it is written quoted in TOML: `\"{key}\"`.)"
                ));
                refused.position = position;
                return refused;
            }
        }
    }
    GroupError {
        file: file.to_path_buf(),
        key: String::new(),
        position,
        message: format!("is not a lint group file: {}", why.message()),
    }
}

/// The groups that ship with the toolchain: every `*.toml` in `lints/`
/// beside `std`.
///
/// No `std` at all is no built-in groups, which is what a test database with
/// its own sources wants. A `lints/` directory that exists and cannot be read
/// is an error, for the reason a manifest that does not load is one: an empty
/// answer would switch a project's groups off without a word.
pub fn built_in(std: Option<&Path>) -> Result<Vec<Group>, GroupError> {
    let Some(std) = std else { return Ok(Vec::new()) };
    let directory = std.join("lints");
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(&directory)
        .map_err(|why| error(&directory, "", format!("cannot be read: {why}")))?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "toml"))
        .collect();
    // Sorted so that which of two bad files is reported does not depend on
    // the order the filesystem lists them in.
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        out.push(read(&file, &stem, true)?);
    }
    Ok(out)
}

/// Reads one group file, expected to define the group `name`.
pub fn read(file: &Path, name: &str, built_in: bool) -> Result<Group, GroupError> {
    let text = std::fs::read_to_string(file)
        .map_err(|why| error(file, "", format!("the group `{name}` cannot be read: {why}")))?;
    parse(&text, file, name, built_in)
}

/// Parses a group file's text; [`read`] without the filesystem.
pub fn parse(text: &str, file: &Path, name: &str, built_in: bool) -> Result<Group, GroupError> {
    not_a_package_group(file, name, name)?;
    let table: toml::Table = toml::from_str(text).map_err(|why| unparsable(text, file, &why))?;

    let mut group = None;
    for (key, value) in &table {
        match key.as_str() {
            "group" => group = Some(value),
            other => {
                return Err(error(
                    file,
                    other,
                    "a group file has one table, `[group]`, with `[group.lints]` inside it",
                ))
            }
        }
    }
    let Some(group) = group.and_then(toml::Value::as_table) else {
        return Err(error(file, "group", "a group file needs a `[group]` table"));
    };

    let mut declared = None;
    let mut description = String::new();
    let mut members = BTreeMap::new();
    for (key, value) in group {
        let at = format!("group.{key}");
        match key.as_str() {
            "name" => {
                let Some(text) = value.as_str() else {
                    return Err(error(file, at, "is a string, the group's name"));
                };
                declared = Some(text.to_string());
            }
            "description" => {
                let Some(text) = value.as_str() else {
                    return Err(error(file, at, "is a string, one line on what the group is for"));
                };
                description = text.to_string();
            }
            "lints" => {
                let Some(lints) = value.as_table() else {
                    return Err(error(
                        file,
                        at,
                        "is a table of lint names, each with the level it runs at in this group",
                    ));
                };
                for (lint, level) in lints {
                    let at = format!("group.lints.{lint}");
                    not_a_package_group(file, &at, lint)?;
                    let level = level.as_str().and_then(LintLevel::from_name).ok_or_else(|| {
                        error(file, &at, "is the level this lint runs at in the group: `allow`, `warn` or `deny`")
                    })?;
                    members.insert(lint.clone(), level);
                }
            }
            _ => {
                return Err(error(
                    file,
                    at,
                    "is not a group setting. A group has `name`, `description` and `[group.lints]`",
                ))
            }
        }
    }
    let Some(declared) = declared else {
        return Err(error(file, "group.name", "is missing: a group says its own name"));
    };
    if declared != name {
        let why = if built_in {
            format!("says `{declared}`, and a built-in group is named by its file, `{name}.toml`")
        } else {
            format!("says `{declared}`, and the manifest declares this file as `{name}`. Make them agree")
        };
        return Err(error(file, "group.name", why));
    }
    Ok(Group { name: declared, description, members, file: file.to_path_buf(), built_in })
}

/// Refuses a member that is not a built-in lint, naming what it is instead.
///
/// Here rather than in [`parse`], because telling a nested group from a typo
/// needs every group's name, and one file cannot know the others.
fn check_members(group: &Group, registry: &BTreeMap<String, Group>) -> Result<(), GroupError> {
    for member in group.members.keys() {
        let at = format!("group.lints.{member}");
        if registry.contains_key(member) {
            return Err(error(
                &group.file,
                at,
                format!("`{member}` is a group, and a group holds lints, never another group. List its lints here instead"),
            ));
        }
        if !LINTS.contains(&member.as_str()) {
            return Err(error(
                &group.file,
                at,
                format!(
                    "`{member}` is not a built-in lint, and a group holds only those. The lints are {}",
                    LINTS.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

/// Refuses `package::group`, which is a name form kept for later.
fn not_a_package_group(file: &Path, at: &str, name: &str) -> Result<(), GroupError> {
    if name.contains("::") {
        return Err(package_group(file, at, name));
    }
    Ok(())
}

/// The refusal of a `package::group` name.
fn package_group(file: &Path, at: &str, name: &str) -> GroupError {
    error(
        file,
        at,
        format!(
            "`{name}` names a group published in a package, and those are not yet \
             supported. Copy the group file into this project and declare it under \
             `[lint-groups]`"
        ),
    )
}

/// How loud each lint is for one package: the manifest's `[lints]` resolved
/// against the groups it can see.
///
/// Built once per manifest, and every conflict and refusal is found then, not
/// when a lint happens to fire: a policy that is only wrong for a lint the
/// code does not trip yet is still wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Levels {
    resolved: BTreeMap<&'static str, LintLevel>,
    /// Every group this package can see, enabled or not, with its members:
    /// what `unknown-allow` needs to say a pragma named a group.
    groups: BTreeMap<String, Vec<String>>,
    unknown: Vec<String>,
}

impl Levels {
    /// Resolves the manifest at `path` against the built-in groups, or just
    /// the defaults when there is no manifest.
    ///
    /// `built_in` is [`built_in`]'s answer, passed in so that the language
    /// server can read the files once. The built-in groups are checked even
    /// with no manifest, so a broken toolchain is reported by every command
    /// rather than only by the projects that switch the group on.
    pub fn new(manifest: Option<(&Manifest, &Path)>, built_in: Vec<Group>) -> Result<Levels, GroupError> {
        let mut registry: BTreeMap<String, Group> = BTreeMap::new();
        for group in built_in {
            if LINTS.contains(&group.name.as_str()) {
                return Err(error(
                    &group.file,
                    "group.name",
                    format!("`{}` is a lint, so a group cannot take the name", group.name),
                ));
            }
            registry.insert(group.name.clone(), group);
        }
        let empty = khora_manifest::Lints::default();
        let (lints, path) = match manifest {
            Some((manifest, path)) => {
                local_groups(manifest, path, &mut registry)?;
                (&manifest.lints, path)
            }
            None => (&empty, Path::new("khora.toml")),
        };
        for group in registry.values() {
            check_members(group, &registry)?;
        }

        let mut entries: BTreeMap<&'static str, LintLevel> = BTreeMap::new();
        // Each enabled group, with its explicit `level` if it has one.
        let mut enabled: Vec<(&Group, Option<LintLevel>)> = Vec::new();
        let mut unknown = Vec::new();
        for (name, entry) in lints {
            let at = format!("lints.{name}");
            not_a_package_group(path, &at, name)?;
            if let Some(group) = registry.get(name) {
                enabled.push((group, group_entry(path, &at, name, entry)?));
            } else if let Some(lint) = LINTS.iter().find(|lint| **lint == name) {
                let Some(level) = entry.level else {
                    return Err(error(
                        path,
                        at,
                        "a lint's table needs a `level`: `allow`, `warn` or `deny`",
                    ));
                };
                entries.insert(lint, level);
            } else {
                unknown.push(name.clone());
            }
        }

        let mut resolved = BTreeMap::new();
        for lint in LINTS {
            let level = match entries.get(lint) {
                Some(level) => *level,
                None => from_groups(path, lint, &enabled)?.unwrap_or_else(|| default_level(lint)),
            };
            resolved.insert(*lint, level);
        }
        let groups = registry
            .into_iter()
            .map(|(name, group)| (name, group.members.into_keys().collect()))
            .collect();
        Ok(Levels { resolved, groups, unknown })
    }

    /// The names in `[lints]` that are neither a lint nor a group.
    pub fn unknown(&self) -> &[String] {
        &self.unknown
    }

    /// The members of `name`, if it is a group this package can see.
    pub fn group(&self, name: &str) -> Option<&[String]> {
        self.groups.get(name).map(Vec::as_slice)
    }

    /// The groups this package can see, by name.
    pub fn group_names(&self) -> impl Iterator<Item = &str> {
        self.groups.keys().map(String::as_str)
    }
}

/// Adds the manifest's `[lint-groups]` to `registry`.
fn local_groups(
    manifest: &Manifest,
    path: &Path,
    registry: &mut BTreeMap<String, Group>,
) -> Result<(), GroupError> {
    let directory = path.parent().unwrap_or(Path::new("."));
    for (name, file) in &manifest.lint_groups {
        let at = format!("lint-groups.{name}");
        not_a_package_group(path, &at, name)?;
        if LINTS.contains(&name.as_str()) {
            return Err(error(path, at, format!("`{name}` is a lint, so a group cannot take the name")));
        }
        if let Some(shadowed) = registry.get(name) {
            return Err(error(
                path,
                at,
                format!(
                    "`{name}` is a built-in group ({}), and a project's group cannot take its \
                     name. Pick another",
                    shadowed.file.display()
                ),
            ));
        }
        let group = read(&directory.join(file), name, false)?;
        registry.insert(name.clone(), group);
    }
    Ok(())
}

/// The explicit level of a group's `[lints.<group>]` table.
fn group_entry(
    path: &Path,
    at: &str,
    name: &str,
    entry: &khora_manifest::Lint,
) -> Result<Option<LintLevel>, GroupError> {
    if entry.bare {
        let level = entry.level.map(LintLevel::as_str).unwrap_or("warn");
        return Err(error(
            path,
            at,
            format!(
                "`{name}` is a lint group, and a group is always a table:\n\n    \
                 [lints.{name}]\n    level = \"{level}\"\n\nWith no `level`, each lint in the \
                 group runs at its own level; `level` sets all of them"
            ),
        ));
    }
    // The first key is enough to report: a table with a stray key is wrong
    // whichever one it is, and the reader fixes them one message at a time.
    if let Some(key) = entry.options.keys().next() {
        let at = format!("{at}.{key}");
        if key == "enabled" {
            return Err(error(
                path,
                at,
                format!(
                    "is not a group setting. Writing `[lints.{name}]` switches the group on, \
                     and `level = \"allow\"` switches it off"
                ),
            ));
        }
        if RESERVED_KEYS.contains(&key.as_str()) {
            return Err(error(path, at, "is reserved for a later version"));
        }
        return Err(error(path, at, "is not a group setting. A group's table takes `level`"));
    }
    Ok(entry.level)
}

/// The level the enabled groups give `lint`, if any holds it: steps 3 and 4.
///
/// **Step 3 is one step across every enabled group** (the owner's ruling). If
/// any group holding `lint` has an explicit `level`, the explicit levels
/// decide, and another group's in-group default for the lint is not
/// consulted. So `[lints.b] level = "allow"` makes `lint` `allow` even when an
/// enabled `a` holds it at `deny`, silently; the reference page says so with
/// that example. Only proposals at the same step can conflict: two explicit
/// levels, or, when none is written, two in-group defaults.
fn from_groups(
    path: &Path,
    lint: &'static str,
    enabled: &[(&Group, Option<LintLevel>)],
) -> Result<Option<LintLevel>, GroupError> {
    let holding: Vec<&(&Group, Option<LintLevel>)> =
        enabled.iter().filter(|(group, _)| group.members.contains_key(lint)).collect();
    let explicit: Vec<(&Group, LintLevel)> =
        holding.iter().filter_map(|(group, level)| level.map(|level| (*group, level))).collect();
    let decided: Vec<(&Group, LintLevel)> = if explicit.is_empty() {
        holding.iter().map(|(group, _)| (*group, group.members[lint])).collect()
    } else {
        explicit
    };
    let Some((first, level)) = decided.first() else { return Ok(None) };
    if let Some((other, theirs)) = decided.iter().find(|(_, theirs)| theirs != level) {
        let how = |group: &Group, level: LintLevel| {
            format!("`{}` ({}) says `{level}`", group.name, group.file.display())
        };
        // Keyed on the line that settles it, the per-lint entry, rather
        // than on the whole `[lints]` table.
        return Err(error(
            path,
            format!("lints.{lint}"),
            format!(
                "two enabled groups disagree about `{lint}`: {} and {}. Say which with a line \
                 of its own, `{lint} = \"...\"`, under `[lints]`",
                how(first, *level),
                how(other, *theirs)
            ),
        ));
    }
    Ok(Some(*level))
}

/// How loud `lint` is under `levels`: steps 2 to 5 of the precedence above.
///
/// **The one resolver.** The CLI and the language server each had a
/// `levels.get(..).unwrap_or_else(default_level)` of their own, and a group is
/// exactly the rule two copies would implement differently -- the editor then
/// says a line is fine and the build fails on it. The pragma is not here
/// because it is not a level: [`crate::findings`] has already dropped what it
/// allows.
pub fn level(levels: &Levels, lint: &str) -> LintLevel {
    levels.resolved.get(lint).copied().unwrap_or_else(|| default_level(lint))
}
