//! `khora release`.
//!
//! Roadmap 14.20. The dangerous behaviours of a release tool are the ones it
//! does *not* have — it must not tag, must not push, and must not decide the
//! semver level for you — so several of these assert an absence. The rest are
//! about not mangling a manifest full of comments that were written to be
//! read.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs git in `at` with an identity on the command line.
fn git(args: &[&str], at: &Path) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(at)
        .output()
        .expect("git on the path");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A committed two-member workspace, tagged `v0.4.0`, with one member edited
/// since the tag.
fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("release").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");

    std::fs::write(
        root.join("khora.toml"),
        "# A comment that was written to be read.\n[workspace]\nmembers = [\"packages/*\"]\n\n\
         [workspace.package]\nversion = \"0.4.0\"\nedition = \"2026\"\n",
    )
    .expect("the root manifest");
    for member in ["alpha", "beta"] {
        let at = root.join("packages").join(member);
        std::fs::create_dir_all(at.join("src")).expect("a member");
        std::fs::write(
            at.join("khora.toml"),
            format!("[package]\nname = \"{member}\"\nversion.workspace = true\n"),
        )
        .expect("a member manifest");
        std::fs::write(
            at.join("src").join("lib.kh"),
            format!("module {member}::lib;\n\npub fn go() -> Int {{\n  1\n}}\n"),
        )
        .expect("a member source");
    }

    git(&["init", "--quiet", "-b", "main"], &root);
    git(&["add", "-A"], &root);
    git(&["commit", "--quiet", "-m", "start"], &root);
    git(&["tag", "v0.4.0"], &root);

    std::fs::write(
        root.join("packages").join("alpha").join("src").join("lib.kh"),
        "module alpha::lib;\n\npub fn go() -> Int {\n  2\n}\n",
    )
    .expect("an edit");
    git(&["commit", "--quiet", "-am", "alpha: go returns two now"], &root);
    root
}

fn run(at: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(at)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn manifest(root: &Path) -> String {
    std::fs::read_to_string(root.join("khora.toml")).expect("the root manifest")
}

#[test]
fn it_says_which_members_changed_and_which_did_not() {
    let root = fixture("changed");

    let (ok, output) = run(&root, &["release", "--since", "v0.4.0"]);
    assert!(ok, "{output}");
    assert!(output.contains("1 changed since v0.4.0"), "{output}");
    assert!(output.contains("alpha"), "{output}");
    assert!(output.contains("unchanged"), "{output}");
    assert!(output.contains("beta"), "the unchanged one is named too: {output}");
}

#[test]
fn it_does_not_choose_the_level_for_you() {
    // `docs/design/compatibility.md` is explicit that a bug fix is not
    // automatically a patch release. Which level a change is, is a judgement
    // about observable behaviour, and a tool that guessed would be guessing
    // about the one thing it cannot see.
    let root = fixture("no_guess");

    let (ok, output) = run(&root, &["release", "--since", "v0.4.0"]);
    assert!(ok, "{output}");
    assert!(output.contains("you choose"), "{output}");
    assert!(manifest(&root).contains("0.4.0"), "nothing should be written without a level");
}

#[test]
fn a_level_writes_the_version_and_nothing_else() {
    let root = fixture("write");

    let (ok, output) = run(&root, &["release", "--since", "v0.4.0", "--minor"]);
    assert!(ok, "{output}");
    assert!(output.contains("0.5.0"), "{output}");

    let text = manifest(&root);
    assert!(text.contains("version = \"0.5.0\""), "{text}");
    assert!(
        text.contains("# A comment that was written to be read."),
        "re-serializing would have eaten the comments: {text}"
    );
    assert!(text.contains("edition = \"2026\""), "{text}");
}

#[test]
fn the_levels_move_what_they_say() {
    for (flag, expected) in [("--major", "1.0.0"), ("--minor", "0.5.0"), ("--patch", "0.4.1")] {
        let root = fixture(&format!("level{}", flag.trim_start_matches('-')));
        let (ok, output) = run(&root, &["release", "--since", "v0.4.0", flag]);
        assert!(ok, "{output}");
        assert!(
            manifest(&root).contains(&format!("version = \"{expected}\"")),
            "{flag} should give {expected}: {}",
            manifest(&root)
        );
    }
}

#[test]
fn it_never_tags() {
    // The one behaviour worth being certain about. `release.yml` puts a person
    // between "built" and "visible" deliberately.
    let root = fixture("no_tag");
    run(&root, &["release", "--since", "v0.4.0", "--minor"]);

    let out = Command::new("git")
        .args(["tag", "--list"])
        .current_dir(&root)
        .output()
        .expect("git");
    let tags = String::from_utf8_lossy(&out.stdout);
    assert_eq!(tags.trim(), "v0.4.0", "a tag was created: {tags}");
}

#[test]
fn it_says_a_tag_is_still_yours_to_make() {
    let root = fixture("says_tag");
    let (_, output) = run(&root, &["release", "--since", "v0.4.0", "--patch"]);
    assert!(output.contains("Nothing is tagged"), "{output}");
    assert!(output.contains("git tag v0.4.1"), "and says what to type: {output}");
}

#[test]
fn the_notes_leave_the_required_section_empty() {
    // The pre-1.0 rule wants every behaviour change described in both
    // directions. A commit subject says what changed, not what it changed
    // *from*, so the tool cannot fill this in — and an empty required section
    // is the only honest thing it can say.
    let root = fixture("notes");

    let (ok, output) =
        run(&root, &["release", "--since", "v0.4.0", "--minor", "--notes", "NOTES.md"]);
    assert!(ok, "{output}");

    let notes = std::fs::read_to_string(root.join("NOTES.md")).expect("the notes");
    assert!(notes.starts_with("# 0.5.0"), "{notes}");
    assert!(notes.contains("## Behaviour changes"), "{notes}");
    assert!(notes.contains("alpha: go returns two now"), "the subjects are grouped: {notes}");
    assert!(
        notes.contains("not ready"),
        "an empty section has to say it means unfinished: {notes}"
    );
    assert!(output.contains("They are a draft"), "{output}");
}

#[test]
fn notes_without_a_level_says_what_is_missing() {
    let root = fixture("notes_no_level");

    let (ok, output) = run(&root, &["release", "--since", "v0.4.0", "--notes", "NOTES.md"]);
    assert!(!ok, "{output}");
    assert!(output.contains("--major, --minor or --patch"), "{output}");
}

#[test]
fn nothing_changed_is_not_a_release() {
    let root = fixture("nothing");
    git(&["tag", "v0.4.1"], &root);

    let (ok, output) = run(&root, &["release", "--since", "v0.4.1"]);
    assert!(ok, "having nothing to do is not a failure: {output}");
    assert!(output.contains("nothing to release"), "{output}");
}

#[test]
fn a_change_the_workspace_does_not_own_selects_everything() {
    // 14.16's rule, unchanged and load-bearing here: a release tool that
    // quietly decided a compiler change affected nothing would be wrong in the
    // most expensive direction.
    let root = fixture("everything");
    std::fs::write(root.join("build.sh"), "#!/bin/sh\necho hello\n").expect("a stray file");
    git(&["add", "-A"], &root);
    git(&["commit", "--quiet", "-m", "a script nobody owns"], &root);

    let (ok, output) = run(&root, &["release", "--since", "v0.4.0"]);
    assert!(ok, "{output}");
    assert!(output.contains("every member"), "{output}");
    assert!(output.contains("build.sh"), "and names the file: {output}");
}

#[test]
fn a_package_that_is_not_a_workspace_is_refused() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("release").join("solo");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a directory");
    std::fs::write(root.join("khora.toml"), "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n")
        .expect("a manifest");

    let (ok, output) = run(&root, &["release", "--since", "HEAD"]);
    assert!(!ok, "{output}");
    assert!(output.contains("not a workspace root"), "{output}");
}
