//! Lint groups: the files, the refusals, and the precedence.
//!
//! **A group that is wrong must never become a group that is empty.** Each
//! refusal here stands in for a manifest that would otherwise have looked
//! configured and switched nothing on, so every one asserts the message names
//! the file and the key -- the two things a reader needs to fix it.
//!
//! The built-in `idiomatic` group has no members yet, so the tests use groups
//! of their own, built from real lints, in scratch directories.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_lint::groups::{self, Group, GroupError};
use khora_lint::{Levels, DANGLING_EXPRESSION, LINTS, UNDOCUMENTED_EXPORT, UNKNOWN_ALLOW, UNUSED_BINDING, UNUSED_IMPORT};
use khora_manifest::{LintLevel, Manifest};

/// A scratch directory, emptied.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lint-groups").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).expect("writing a fixture");
}

/// A group file's text.
fn group_file(name: &str, members: &str) -> String {
    format!("[group]\nname = \"{name}\"\ndescription = \"test\"\n\n[group.lints]\n{members}")
}

/// `strict`: two warn-by-default lints made louder, and one off-by-default
/// lint switched on.
const STRICT: &str = "unused-binding = \"deny\"\nunused-import = \"warn\"\nundocumented-export = \"warn\"\n";

/// A package whose manifest has `lints` (the text after `[package]`) and the
/// `strict` group declared, plus any extra files.
fn package(name: &str, lints: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = scratch(name);
    write(&dir.join("lints/strict.toml"), &group_file("strict", STRICT));
    for (path, text) in files {
        write(&dir.join(path), text);
    }
    write(
        &dir.join("khora.toml"),
        &format!(
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lint-groups]\nstrict = \"lints/strict.toml\"\n\n{lints}"
        ),
    );
    dir.join("khora.toml")
}

/// The levels for the manifest at `path`, with `built_in` as the toolchain's.
fn levels_with(path: &Path, built_in: Vec<Group>) -> Result<Levels, GroupError> {
    let parsed = Manifest::load(path).expect("the manifest loads");
    Levels::new(Some((&parsed.manifest, path)), built_in)
}

fn resolve(path: &Path) -> Result<Levels, GroupError> {
    levels_with(path, Vec::new())
}

fn refused(path: &Path) -> GroupError {
    match resolve(path) {
        Ok(_) => panic!("expected {} to be refused", path.display()),
        Err(why) => why,
    }
}

/// Asserts `why` names `file` and `key`, and says `says`.
fn names(why: &GroupError, file: &Path, key: &str, says: &str) {
    let text = why.to_string();
    assert_eq!(why.file, file, "the file: {text}");
    assert_eq!(why.key, key, "the key: {text}");
    assert!(text.contains(&file.display().to_string()), "the message names the file: {text}");
    assert!(text.contains(key), "the message names the key: {text}");
    assert!(text.contains(says), "the message says `{says}`: {text}");
}

// ---- precedence ------------------------------------------------------------

#[test]
fn precedence_step_5_the_base_default_when_no_group_is_on() {
    let path = package("base", "", &[]);
    let levels = resolve(&path).unwrap();
    // Declared is not enabled: the members stay at their base defaults.
    assert_eq!(khora_lint::level(&levels, UNDOCUMENTED_EXPORT), LintLevel::Allow);
    assert_eq!(khora_lint::level(&levels, UNUSED_BINDING), LintLevel::Warn);
}

#[test]
fn precedence_step_4_each_member_runs_at_its_in_group_default() {
    // No `level`: no blanket. Each member at the level the group file gives it.
    let path = package("in_group", "[lints.strict]\n", &[]);
    let levels = resolve(&path).unwrap();
    assert_eq!(khora_lint::level(&levels, UNUSED_BINDING), LintLevel::Deny);
    assert_eq!(khora_lint::level(&levels, UNUSED_IMPORT), LintLevel::Warn);
    assert_eq!(khora_lint::level(&levels, UNDOCUMENTED_EXPORT), LintLevel::Warn);
    // A lint outside the group is untouched.
    assert_eq!(khora_lint::level(&levels, DANGLING_EXPRESSION), LintLevel::Warn);
}

#[test]
fn precedence_step_3_an_explicit_level_sets_every_member() {
    let path = package("explicit", "[lints.strict]\nlevel = \"allow\"\n", &[]);
    let levels = resolve(&path).unwrap();
    assert_eq!(khora_lint::level(&levels, UNUSED_BINDING), LintLevel::Allow, "beats the in-group deny");
    assert_eq!(khora_lint::level(&levels, UNUSED_IMPORT), LintLevel::Allow, "beats the base warn");
    assert_eq!(khora_lint::level(&levels, DANGLING_EXPRESSION), LintLevel::Warn, "not a member");

    let path = package("explicit_deny", "[lints.strict]\nlevel = \"deny\"\n", &[]);
    let levels = resolve(&path).unwrap();
    assert_eq!(khora_lint::level(&levels, UNDOCUMENTED_EXPORT), LintLevel::Deny);
}

#[test]
fn precedence_step_2_a_lint_entry_beats_the_group_whatever_the_order() {
    // Both orders: the entry above the group's table and below it.
    for (name, lints) in [
        ("entry_first", "[lints]\nunused-binding = \"warn\"\n\n[lints.strict]\nlevel = \"deny\"\n"),
        ("entry_last", "[lints.strict]\nlevel = \"deny\"\n\n[lints]\nunused-binding = \"warn\"\n"),
    ] {
        let path = package(name, lints, &[]);
        let levels = resolve(&path).unwrap();
        assert_eq!(khora_lint::level(&levels, UNUSED_BINDING), LintLevel::Warn, "{name}");
        assert_eq!(khora_lint::level(&levels, UNUSED_IMPORT), LintLevel::Deny, "{name}");
    }
    // And against an in-group default, with no explicit level.
    let path = package("entry_vs_in_group", "[lints]\nunused-binding = \"allow\"\n\n[lints.strict]\n", &[]);
    assert_eq!(khora_lint::level(&resolve(&path).unwrap(), UNUSED_BINDING), LintLevel::Allow);
}

/// What a file reports under `levels`, as (lint, level).
fn reported(source: &str, levels: &Levels) -> Vec<(&'static str, LintLevel, String)> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "t.kh".into(), source.to_string());
    SourceRoot::new(&db, vec![file]);
    khora_lint::reported(&db, file, levels)
        .into_iter()
        .map(|(finding, level)| (finding.lint, level, finding.message))
        .collect()
}

const UNUSED_LOCAL: &str = "module t;\n\npub fn main() -> Int {\n  let a = 1;\n  0\n}\n";
const UNUSED_LOCAL_ALLOWED: &str =
    "module t;\n\npub fn main() -> Int {\n  // @klint allow unused-binding\n  let a = 1;\n  0\n}\n";

#[test]
fn precedence_step_1_the_pragma_beats_everything() {
    let path = package("pragma", "[lints]\nunused-binding = \"deny\"\n\n[lints.strict]\nlevel = \"deny\"\n", &[]);
    let levels = resolve(&path).unwrap();
    // The control: the lint fires, and at `deny`.
    let found = reported(UNUSED_LOCAL, &levels);
    assert!(found.iter().any(|(lint, level, _)| *lint == UNUSED_BINDING && *level == LintLevel::Deny), "{found:?}");
    let found = reported(UNUSED_LOCAL_ALLOWED, &levels);
    assert!(found.iter().all(|(lint, _, _)| *lint != UNUSED_BINDING), "{found:?}");
}

#[test]
fn the_group_decides_what_is_reported() {
    // Through `reported`, the one path both the CLI and the editor take: a
    // group's `allow` drops the finding, which a resolver the reporter
    // ignored would not.
    let path = package("reported_allow", "[lints.strict]\nlevel = \"allow\"\n", &[]);
    let found = reported(UNUSED_LOCAL, &resolve(&path).unwrap());
    assert!(found.iter().all(|(lint, _, _)| *lint != UNUSED_BINDING), "{found:?}");
    let path = package("reported_on", "[lints.strict]\n", &[]);
    let found = reported(UNUSED_LOCAL, &resolve(&path).unwrap());
    assert!(found.iter().any(|(lint, level, _)| *lint == UNUSED_BINDING && *level == LintLevel::Deny), "{found:?}");
}

// ---- the manifest's `[lints.<group>]` table --------------------------------

#[test]
fn a_reserved_key_is_refused_for_a_later_version() {
    for key in ["exclude", "rules", "extends"] {
        let path = package(&format!("reserved_{key}"), &format!("[lints.strict]\n{key} = []\n"), &[]);
        names(&refused(&path), &path, &format!("lints.strict.{key}"), "reserved for a later version");
    }
}

#[test]
fn an_unknown_key_in_a_group_table_is_an_error() {
    let path = package("unknown_key", "[lints.strict]\nlevle = \"deny\"\n", &[]);
    names(&refused(&path), &path, "lints.strict.levle", "takes `level`");
}

#[test]
fn enabled_is_refused_and_names_level() {
    let path = package("enabled", "[lints.strict]\nenabled = true\n", &[]);
    names(&refused(&path), &path, "lints.strict.enabled", "`level = \"allow\"`");
}

#[test]
fn the_string_form_is_refused_and_shows_the_table() {
    let path = package("string_form", "[lints]\nstrict = \"deny\"\n", &[]);
    names(&refused(&path), &path, "lints.strict", "[lints.strict]\n    level = \"deny\"");
}

#[test]
fn a_lint_table_still_needs_a_level() {
    // `level` is optional in the manifest because a group's table may omit
    // it. A lint's may not, and nothing else would say so.
    let path = package("lint_no_level", "[lints.unused-binding]\n", &[]);
    names(&refused(&path), &path, "lints.unused-binding", "needs a `level`");
}

#[test]
fn a_package_group_name_is_refused_as_not_yet() {
    let path = package("pkg_in_lints", "[lints.\"acme::strict\"]\n", &[]);
    names(&refused(&path), &path, "lints.acme::strict", "not yet");
}

// ---- the group files --------------------------------------------------------

#[test]
fn a_malformed_group_file_is_refused() {
    let path = package("malformed", "[lints.strict]\n", &[("lints/strict.toml", "[group\nname = ")]);
    let file = path.parent().unwrap().join("lints/strict.toml");
    let why = refused(&path);
    assert_eq!(why.file, file, "{why}");
    let text = why.to_string();
    assert!(text.contains("is not a lint group file"), "{why}");
    // Where in the file, as a bad `khora.toml` gets: `toml` stops at line 1,
    // after the unclosed `[group`. And no empty backticks for a key there is
    // none of.
    assert!(text.contains(&format!("{}:1:", file.display())), "a line and column: {text}");
    assert!(!text.contains("``"), "no empty key: {text}");
}

#[test]
fn an_unquoted_package_group_member_is_told_to_quote_it_and_not_yet() {
    // `acme::strict` bare is not a TOML key at all, so the parse fails before
    // the "not yet" check can see it. The message still has to say what the
    // reader meant, not only that the file does not parse.
    let text = group_file("strict", "acme::strict = \"warn\"\n");
    let path = package("pkg_member_bare", "", &[("lints/strict.toml", &text)]);
    let why = refused(&path);
    let text = why.to_string();
    assert!(text.contains("not yet"), "{text}");
    assert!(text.contains("\"acme::strict\""), "shows the quoted form: {text}");
}

#[test]
fn a_group_file_setting_that_does_not_exist_is_refused() {
    let text = format!("{}\n", group_file("strict", STRICT)).replace("description", "descripton");
    let path = package("bad_setting", "", &[("lints/strict.toml", &text)]);
    let file = path.parent().unwrap().join("lints/strict.toml");
    names(&refused(&path), &file, "group.descripton", "not a group setting");
}

#[test]
fn an_unknown_member_is_refused() {
    let text = group_file("strict", "unused-bindings = \"deny\"\n");
    let path = package("unknown_member", "", &[("lints/strict.toml", &text)]);
    let file = path.parent().unwrap().join("lints/strict.toml");
    names(&refused(&path), &file, "group.lints.unused-bindings", "not a built-in lint");
}

#[test]
fn a_group_inside_a_group_is_refused() {
    let text = group_file("outer", "strict = \"warn\"\n");
    let path = package("nested", "", &[("lints/outer.toml", &text)]);
    let dir = path.parent().unwrap();
    write(
        &path,
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lint-groups]\nstrict = \"lints/strict.toml\"\n\
         outer = \"lints/outer.toml\"\n",
    );
    names(&refused(&path), &dir.join("lints/outer.toml"), "group.lints.strict", "never another group");
}

#[test]
fn a_package_group_member_is_refused_as_not_yet() {
    let text = group_file("strict", "\"acme::strict\" = \"warn\"\n");
    let path = package("pkg_member", "", &[("lints/strict.toml", &text)]);
    let file = path.parent().unwrap().join("lints/strict.toml");
    names(&refused(&path), &file, "group.lints.acme::strict", "not yet");
}

#[test]
fn a_file_that_names_another_group_is_refused() {
    let path = package("wrong_name", "", &[("lints/strict.toml", &group_file("lax", STRICT))]);
    let file = path.parent().unwrap().join("lints/strict.toml");
    names(&refused(&path), &file, "group.name", "`lax`");
}

#[test]
fn a_group_file_that_is_missing_is_refused() {
    let path = package("missing_file", "", &[]);
    std::fs::remove_file(path.parent().unwrap().join("lints/strict.toml")).unwrap();
    let why = refused(&path);
    assert!(why.to_string().contains("cannot be read"), "{why}");
}

// ---- names -------------------------------------------------------------------

/// A fixture toolchain `std` whose `lints/` holds `files`.
fn fixture_std(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let std = scratch(name).join("std");
    std::fs::create_dir_all(std.join("lints")).unwrap();
    for (file, text) in files {
        write(&std.join("lints").join(file), text);
    }
    std
}

#[test]
fn a_local_group_cannot_shadow_a_built_in_one() {
    let std = fixture_std("shadow_std", &[("strict.toml", &group_file("strict", "unused-import = \"deny\"\n"))]);
    let built_in = groups::built_in(Some(&std)).unwrap();
    let path = package("shadow", "", &[]);
    let why = levels_with(&path, built_in).expect_err("a local group named like a built-in one");
    names(&why, &path, "lint-groups.strict", "is a built-in group");
}

#[test]
fn no_group_is_named_like_a_lint() {
    // Built-in: the shipped files, and a fixture that tries it.
    let shipped = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../std");
    for group in groups::built_in(Some(&shipped)).expect("the shipped groups load") {
        assert!(!LINTS.contains(&group.name.as_str()), "`{}` is a lint", group.name);
    }
    let std = fixture_std("lint_named_std", &[("unused-import.toml", &group_file("unused-import", ""))]);
    let built_in = groups::built_in(Some(&std)).unwrap();
    let path = package("lint_named_built_in", "", &[]);
    let why = levels_with(&path, built_in).expect_err("a built-in group named like a lint");
    // Joined a component at a time, as the loader builds it: a path spelled
    // `lints/unused-import.toml` compares equal on Windows but displays with
    // a `/` where the message has a `\`.
    names(&why, &std.join("lints").join("unused-import.toml"), "group.name", "is a lint");

    // Local.
    let path = package(
        "lint_named_local",
        "",
        &[("lints/unused-import.toml", &group_file("unused-import", ""))],
    );
    write(
        &path,
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lint-groups]\nunused-import = \"lints/unused-import.toml\"\n",
    );
    names(&refused(&path), &path, "lint-groups.unused-import", "is a lint");
}

#[test]
fn a_local_group_named_for_a_package_is_refused_as_not_yet() {
    let path = package("pkg_local", "", &[]);
    write(
        &path,
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lint-groups]\n\"acme::strict\" = \"lints/strict.toml\"\n",
    );
    names(&refused(&path), &path, "lint-groups.acme::strict", "not yet");
}

// ---- two groups ----------------------------------------------------------------

#[test]
fn two_enabled_groups_that_disagree_are_an_error_naming_both() {
    let lax = group_file("lax", "unused-binding = \"warn\"\n");
    let path = package("conflict", "", &[("lints/lax.toml", &lax)]);
    let manifest = |lints: &str| {
        format!(
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lint-groups]\nstrict = \"lints/strict.toml\"\n\
             lax = \"lints/lax.toml\"\n\n{lints}"
        )
    };
    write(&path, &manifest("[lints.strict]\n\n[lints.lax]\n"));
    let why = refused(&path);
    let text = why.to_string();
    for says in ["`strict`", "`lax`", "unused-binding", "strict.toml", "lax.toml"] {
        assert!(text.contains(says), "names {says}: {text}");
    }
    // The key is the line that settles it, not the whole `[lints]` table.
    assert_eq!(why.key, "lints.unused-binding", "{text}");

    // Two explicit levels that disagree are the same error, in either order.
    // `strict` holds `unused-binding` at `deny` and `lax` at `warn`; the
    // explicit levels below are the other way round, so the error cannot come
    // from the in-group defaults.
    for lints in [
        "[lints.strict]\nlevel = \"warn\"\n\n[lints.lax]\nlevel = \"deny\"\n",
        "[lints.lax]\nlevel = \"deny\"\n\n[lints.strict]\nlevel = \"warn\"\n",
    ] {
        write(&path, &manifest(lints));
        let why = refused(&path);
        let text = why.to_string();
        for says in ["`strict`", "`lax`", "unused-binding", "`warn`", "`deny`"] {
            assert!(text.contains(says), "{lints}: names {says}: {text}");
        }
    }

    // One per-lint entry settles it.
    write(&path, &manifest("[lints]\nunused-binding = \"warn\"\n\n[lints.strict]\n\n[lints.lax]\n"));
    assert_eq!(khora_lint::level(&resolve(&path).unwrap(), UNUSED_BINDING), LintLevel::Warn);

    // Guard: two groups that agree, or one of them off, are not a conflict.
    write(&path, &manifest("[lints.strict]\n"));
    assert!(resolve(&path).is_ok(), "only enabled groups can conflict");
}

/// **One group's explicit `level` decides a lint another enabled group holds
/// only by its in-group default** (the owner's ruling). `a` holds
/// `unused-binding` at `deny`; `b` holds it at `warn` and is written with
/// `level = "allow"`. The explicit level wins, so the lint is `allow`, and
/// no error -- in both TOML orders. A per-lint entry is how to keep `a`'s
/// `deny`.
#[test]
fn an_explicit_level_decides_a_lint_another_group_holds_by_default() {
    let path = package(
        "explicit_beats_default",
        "",
        &[
            ("lints/a.toml", &group_file("a", "unused-binding = \"deny\"\n")),
            ("lints/b.toml", &group_file("b", "unused-binding = \"warn\"\n")),
        ],
    );
    let manifest = |lints: &str| {
        format!(
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lint-groups]\na = \"lints/a.toml\"\n\
             b = \"lints/b.toml\"\n\n{lints}"
        )
    };
    for lints in [
        "[lints.a]\n\n[lints.b]\nlevel = \"allow\"\n",
        "[lints.b]\nlevel = \"allow\"\n\n[lints.a]\n",
    ] {
        write(&path, &manifest(lints));
        let levels = resolve(&path).unwrap_or_else(|why| panic!("{lints}: not a conflict: {why}"));
        assert_eq!(khora_lint::level(&levels, UNUSED_BINDING), LintLevel::Allow, "{lints}");
    }
    write(&path, &manifest("[lints]\nunused-binding = \"deny\"\n\n[lints.a]\n\n[lints.b]\nlevel = \"allow\"\n"));
    assert_eq!(khora_lint::level(&resolve(&path).unwrap(), UNUSED_BINDING), LintLevel::Deny);
}

/// **A built-in group file that is broken fails every check**, never becomes
/// an empty group. A `built_in` that skipped files it could not read would
/// switch a project's group off with no word, the failure the whole
/// mechanism is built to refuse.
#[test]
fn a_broken_built_in_group_file_is_an_error_naming_it() {
    let std = fixture_std("broken_built_in_std", &[("broken.toml", "[group\nname = ")]);
    let why = groups::built_in(Some(&std)).expect_err("a malformed built-in group file");
    assert_eq!(why.file, std.join("lints/broken.toml"), "{why}");
    assert!(why.to_string().contains("broken.toml"), "{why}");
}

// ---- the built-in group -------------------------------------------------------

#[test]
fn the_built_in_groups_are_the_files_beside_std() {
    let shipped = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let found = groups::built_in(Some(&shipped)).expect("the shipped groups load");
    let idiomatic = found.iter().find(|g| g.name == "idiomatic").expect("`idiomatic` ships");
    assert!(idiomatic.built_in);

    // A built-in group is used exactly the way a local one is.
    let std = fixture_std("built_in_used_std", &[("tidy.toml", &group_file("tidy", "unused-import = \"deny\"\n"))]);
    let path = package("built_in_used", "[lints.tidy]\n", &[]);
    let levels = levels_with(&path, groups::built_in(Some(&std)).unwrap()).unwrap();
    assert_eq!(khora_lint::level(&levels, UNUSED_IMPORT), LintLevel::Deny);
}

// ---- unknown-allow ---------------------------------------------------------------

#[test]
fn a_pragma_naming_a_group_is_told_it_is_a_group() {
    let path = package("pragma_group", "", &[]);
    let levels = resolve(&path).unwrap();
    let source = "module t;\n\npub fn main() -> Int {\n  // @klint allow strict\n  let a = 1;\n  0\n}\n";
    let found = reported(source, &levels);
    let (_, _, message) = found.iter().find(|(lint, _, _)| *lint == UNKNOWN_ALLOW).expect("unknown-allow fires");
    assert!(message.contains("`strict` is a group"), "{message}");
    assert!(message.contains("unused-binding"), "it lists the members: {message}");
}
