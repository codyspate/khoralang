//! `khora check --fix`: the `idiomatic` group's fixes, applied to a project.
//!
//! What each fix does to a line is `khora-lint`'s test. This is the part
//! between that and a file on disk: which findings are applied (only what the
//! manifest has at `warn` or `deny`), that the file is written and checked
//! again, and -- with a backend -- that the program prints what it printed
//! before.

use std::path::{Path, PathBuf};
use std::process::Command;

mod pinned;

/// Every fixable shape once, in a package named `shop`.
const SHAPES: &str = "module main;\n\n\
    import std::core::{print};\n\n\
    fn greet(name: String) -> String {\n  \"hello \" + name + \"!\"\n}\n\n\
    fn negative() -> Int {\n  return 0 - 1;\n}\n\n\
    fn flag(b: Bool) -> Bool { b == false }\n\n\
    fn nested(x: String) -> Bool { (\"a\" + x == \"a!\") == true }\n\n\
    pub fn main() -> () {\n  let twice = fn (x) => x * 2;\n  print(greet(\"world\"));\n  print(\"${twice(negative())}\");\n  print(\"${flag(false)}\");\n  print(\"${nested(\"!\")}\")\n}\n";

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

fn project(lints: &str, source: &str) -> Project {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let root = tmp.path().join("shop");
    std::fs::create_dir_all(root.join("src")).expect("a src directory");
    std::fs::write(
        root.join("khora.toml"),
        format!(
            "[package]\nname = \"shop\"\nversion = \"0.1.0\"\n\n{lints}\n[toolchain]\nversion = \"{}\"\n",
            khora_toolchain::RUNNING
        ),
    )
    .expect("a manifest");
    std::fs::write(root.join("src").join("main.kh"), source).expect("the source");
    Project { _tmp: tmp, root }
}

fn khora(at: &Path, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
    if let Some(archive) = pinned::runtime() {
        command.env("KHORA_RT_LIB", archive);
    }
    let out = command.args(args).current_dir(at).output().expect("running khora");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn source(p: &Project) -> String {
    std::fs::read_to_string(p.root.join("src").join("main.kh")).expect("the source")
}

const GROUP_ON: &str = "[lints.idiomatic]\n";

#[test]
fn fix_rewrites_the_file_and_checks_it_again() {
    let p = project(GROUP_ON, SHAPES);
    let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
    assert!(ok, "{out}");
    assert!(out.contains("fixed ") && out.contains("main.kh"), "names the file it changed:\n{out}");
    assert!(out.contains("checked "), "checks again after fixing:\n{out}");
    assert!(!out.contains("warning:"), "nothing is left to report:\n{out}");

    let after = source(&p);
    for expected in [
        "module shop::main;",
        "\"hello ${name}!\"",
        "  -1\n}",
        "{ !b }",
        "fn x => x * 2",
        // Two fixes, one inside the other: the outer is taken first and the
        // inner on the next pass, which is why `--fix` repeats.
        "{ (\"a${x}\" == \"a!\") }",
    ] {
        assert!(after.contains(expected), "expected `{expected}` in:\n{after}");
    }

    let (ok, again) = khora(&p.root, &["check", "--fix", "."]);
    assert!(ok, "{again}");
    assert!(again.contains("nothing to fix"), "{again}");
    assert_eq!(source(&p), after, "a second run changes nothing");
}

/// The group is off unless the manifest asks, and a lint the manifest
/// allows is never rewritten, even with the group on.
#[test]
fn fix_leaves_what_the_manifest_allows() {
    let off = project("", SHAPES);
    let (ok, out) = khora(&off.root, &["check", "--fix", "."]);
    assert!(ok, "{out}");
    assert!(out.contains("nothing to fix"), "{out}");
    assert_eq!(source(&off), SHAPES);

    let one_allowed = project("[lints]\nconcatenated-string = \"allow\"\n\n[lints.idiomatic]\n", SHAPES);
    let (ok, out) = khora(&one_allowed.root, &["check", "--fix", "."]);
    assert!(ok, "{out}");
    let after = source(&one_allowed);
    assert!(after.contains("\"hello \" + name + \"!\""), "an allowed lint is left alone:\n{after}");
    assert!(after.contains("{ !b }"), "the others are fixed:\n{after}");
}

/// **The fourth check: the two programs print the same.**
///
/// Each named case is one a plausible rewrite gets wrong: the `$` join, a `$`
/// before a hole, a non-ASCII piece, a nested hole holding a `}`, evaluation
/// order, a hole holding an interpolated string, a hole holding `{`,
/// `i64::MIN` spelled `0 - 9223372036854775807 - 1`, and `0.0 - 0.0`, which
/// is left alone because it is `+0.0` and `-0.0` is not. `a` and `yes` have
/// their types written, because a local without one is left alone (see
/// `the_review_probes_print_what_they_printed_before`). The chains holding a
/// call (`wrap(..)`, `noisy(..)`) keep their finding and get no fix, because
/// a call's result may be typed only by the `+`; they are here to show the
/// program still prints the same around them.
#[cfg(feature = "llvm")]
#[test]
fn a_fixed_program_prints_what_it_printed_before() {
    let program = "module main;\n\n\
        import std::core::{print};\n\n\
        fn noisy(tag: String) -> String {\n  print(\"ran ${tag}\");\n  tag\n}\n\n\
        fn wrap(s: String) -> String { s }\n\n\
        fn smallest() -> Int {\n  return 0 - 9223372036854775807 - 1;\n}\n\n\
        pub fn main() -> () {\n  \
        let a: String = \"x\";\n  \
        print(\"$\" + \"{a}\");\n  \
        print(\"$\" + a);\n  \
        print(\"café \" + a + \" é\");\n  \
        print(\"<\" + wrap(\"}\" + a) + \">\");\n  \
        print(\"[\" + wrap(\"b${a}c\") + \"]\");\n  \
        print(\"(\" + wrap(\"{\" + a) + \")\");\n  \
        print(noisy(\"1\") + \"-\" + noisy(\"2\"));\n  \
        print(\"${smallest()}\");\n  \
        let zero = 0.0 - 0.0;\n  \
        print(\"${1.0 / zero}\");\n  \
        let next = fn (x) => x + 1;\n  \
        print(\"${next(1)}\");\n  \
        let yes: Bool = true;\n  \
        let no = yes == false;\n  \
        let still = yes == true;\n  \
        print(\"${no} ${still}\")\n}\n";
    let p = project(GROUP_ON, program);
    let run = |p: &Project| {
        let (ok, out) = khora(&p.root, &["run", "."]);
        assert!(ok, "{out}");
        out.lines().filter(|l| !l.starts_with("warning") && !l.trim().is_empty()).map(str::to_string).collect::<Vec<_>>()
    };
    let before = run(&p);
    assert!(before.contains(&"${a}".to_string()), "{before:?}");
    assert!(before.contains(&"inf".to_string()), "{before:?}");

    let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
    assert!(ok, "{out}");
    let fixed = source(&p);
    // Every chain with no call and no untyped local in it is rewritten.
    for gone in ["\"café \" +", "fn (x)", "== false", "== true", "0 - 922"] {
        assert!(!fixed.contains(gone), "`{gone}` was left:\n{fixed}");
    }
    assert!(fixed.contains("noisy(\"1\") + \"-\" + noisy(\"2\")"), "a chain holding a call is left:\n{fixed}");
    assert!(fixed.contains("0.0 - 0.0"), "the float is left alone:\n{fixed}");

    let after = run(&p);
    assert_eq!(before, after, "the fixed program prints something else:\n{fixed}");
}

// --- the review's probes: a correct program stays correct ------------------------

/// `main.kh` and, beside it, the review's `m_importer` test file.
fn with_importer(p: &Project) {
    std::fs::write(
        p.root.join("src").join("main_test.kh"),
        "module shop::main_test;\n\nimport std::core::{assert};\nimport main::{helper};\n\ntest \"helper\" {\n  assert(helper() == 41);\n}\n",
    )
    .expect("the test file");
}

/// `m_importer`: a test file imports `main`, so renaming it would break the
/// test. `--fix` leaves the header as written and the package still checks.
#[test]
fn a_module_something_imports_is_not_renamed() {
    let program = "module main;\n\nimport std::core::{print};\n\npub fn helper() -> Int { 41 }\n\npub fn main() -> () {\n  print(\"${helper()}\");\n}\n";
    let p = project(GROUP_ON, program);
    with_importer(&p);
    let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
    assert!(ok, "{out}");
    assert!(source(&p).starts_with("module main;"), "{out}\n{}", source(&p));
    assert!(out.contains("[module-path]"), "still reported:\n{out}");
}

/// The review's statement-joining probes whose broken fix would not
/// type-check (`s_after_*`, `b_paren_*`): each is left as written, and the
/// package still checks.
#[test]
fn a_fix_that_would_join_a_unit_statement_is_not_made() {
    let bodies = [
        "fn f(c: Bool) -> Int {\n  if c { print(\"x\") } else { print(\"y\") }\n  0 - 1\n}",
        "fn f(n: Int) -> Int {\n  match n { 1 => print(\"one\"), _ => print(\"other\") }\n  0 - 5\n}",
        "fn f(n: Int) -> Int {\n  let mut i = 0;\n  while i < n { i = i + 1; }\n  0 - 5\n}",
        "fn f(n: Int) -> Int {\n  { print(\"blk\") }\n  0 - 5\n}",
        "fn g(c: Bool) -> Bool {\n  if c { print(\"x\") } else { print(\"y\") }\n  true == (c)\n}",
        "fn g(x: Int) -> Bool {\n  if x > 0 { print(\"x\") } else { print(\"y\") }\n  true == (x < 3)\n}",
    ];
    for body in bodies {
        let program = format!("module shop::main;\n\nimport std::core::{{print}};\n\n{body}\n\npub fn main() -> () {{}}\n");
        let p = project(GROUP_ON, &program);
        let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
        assert!(ok, "{body}\n{out}");
        let after = source(&p);
        assert!(after.contains("}\n  0 - ") || after.contains("}\n  true == ("), "left as written:\n{after}");
    }
}

/// **The review's silent wrong answers, and its unparseable and unbuildable
/// fixes, run.** Before and after `--fix`, the program prints the same:
/// `s_silent` (`-1` after an `if` whose value is an `Int`), `b_silent` (`(c)`
/// after an `if` whose value is a function), `c_nest2` (a string two levels
/// deep once moved into a hole), and `c_lambda_unused` (a closure nobody
/// calls, which `+` alone pins to `String`). And the second review's:
/// `c_alias` (the same through a `let`), `c_match_none` (a binding in an arm
/// that never matches), `b_lambda_unused` (`b == true` is all that makes `b`
/// a `Bool`), `c_for_empty` (a loop over an empty list), and
/// `b_lambda_unused_false`, whose fix `!b` still pins the type, so it is
/// made.
#[cfg(feature = "llvm")]
#[test]
fn the_review_probes_print_what_they_printed_before() {
    let program = "module main;\n\n\
        import std::core::{print, List, Step, Iterator, Option};\n\n\
        fn wrap(s: String) -> String { s }\n\n\
        fn g(s: String) -> String { s }\n\n\
        fn joined(c: Bool) -> Int {\n  if c { 10 } else { 20 }\n  0 - 1\n}\n\n\
        fn called(c: Bool, h: (Bool) -> Bool) -> Bool {\n  if c { h } else { h }\n  true == (c)\n}\n\n\
        pub fn main() -> () {\n  \
        print(\"${joined(true)}\");\n  \
        print(\"${called(true, fn b => !b)}\");\n  \
        print(\"[\" + wrap(\"${g(\"}\")}\") + \"]\");\n  \
        let shout = fn s => s + \"!\";\n  \
        let alias = fn s => {\n    let t = s;\n    t + \"!\"\n  };\n  \
        let o = Option::None;\n  \
        match o { Option::Some(s) => print(s + \"!\"), Option::None => print(\"none\") }\n  \
        let on = fn b => b == true;\n  \
        let off = fn b => b == false;\n  \
        for s in [] { print(s + \"!\"); }\n  \
        print(\"done\")\n}\n";
    let p = project(GROUP_ON, program);
    let run = |p: &Project| {
        let (ok, out) = khora(&p.root, &["run", "."]);
        assert!(ok, "{out}\n{}", source(p));
        out.lines().filter(|l| !l.starts_with("warning") && !l.trim().is_empty()).map(str::to_string).collect::<Vec<_>>()
    };
    let before = run(&p);
    assert_eq!(before.iter().take(3).cloned().collect::<Vec<_>>(), ["-1", "true", "[}]"], "{before:?}");
    let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
    assert!(ok, "{out}");
    let after = run(&p);
    assert_eq!(before, after, "the fixed program prints something else:\n{}", source(&p));
    assert!(source(&p).contains("let off = fn b => !b;"), "`!b` still pins the type, so it is fixed:\n{}", source(&p));
}

// --- writing: all of a pass, or none of it (review 2, N2 and N4) ------------------

/// A second file in `p`'s `src`.
fn beside(p: &Project, name: &str, text: &str) -> PathBuf {
    let path = p.root.join("src").join(name);
    std::fs::write(&path, text).expect("a second file");
    path
}

/// What `src` holds: every file's name and bytes, so a leftover temporary
/// file shows up as well as a changed one.
fn snapshot(p: &Project) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<(String, Vec<u8>)> = std::fs::read_dir(p.root.join("src"))
        .expect("src")
        .map(|e| e.expect("an entry").path())
        .map(|path| (path.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&path).expect("a file")))
        .collect();
    all.sort();
    all
}

/// The review's `ulimit -f` probe: a write that fails partway, as on a full
/// disk. `std::fs::write` in place left the first kilobyte of the file and
/// lost the rest; every file must be byte-identical afterwards.
#[cfg(unix)]
#[test]
fn a_write_that_fails_partway_leaves_every_file_as_it_was() {
    let pad: String = (0..120).map(|i| format!("fn pad{i}() -> Int {{ {i} }}\n")).collect();
    let program = format!("module main;\n\nimport std::core::{{print}};\n\npub fn main() -> () {{\n  print(\"a\" + \"b\");\n}}\n\n{pad}");
    assert!(program.len() > 3000);
    let p = project(GROUP_ON, &program);
    let before = snapshot(&p);
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!("ulimit -f 1; trap '' XFSZ; exec {} check --fix .", env!("CARGO_BIN_EXE_khora")))
        .current_dir(&p.root);
    let out = command.output().expect("running khora under a file-size limit");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "{text}");
    assert_eq!(snapshot(&p), before, "a file changed, or a temporary was left:\n{text}");
    assert!(text.contains("nothing was written"), "{text}");
}

/// The review's `w_readonly2`: two files with a fix, the second read-only. The
/// pass was verified as a set, so none of it is written.
#[test]
fn a_read_only_file_stops_the_whole_pass() {
    let p = project(GROUP_ON, "module main;\n\npub fn main() -> () {\n  let n = 0 - 1;\n}\n");
    let other = beside(&p, "zz.kh", "module shop::zz;\n\npub fn f() -> Int { 0 - 2 }\n");
    let mut permissions = std::fs::metadata(&other).expect("metadata").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&other, permissions).expect("read-only");
    let before = snapshot(&p);
    let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
    assert!(!ok, "{out}");
    assert_eq!(snapshot(&p), before, "{out}");
    assert!(out.contains("nothing was written") && out.contains("zz.kh"), "{out}");
}

/// A file that does not parse hides what a fix does to it, so one anywhere in
/// the package stops the pass, and says so.
#[test]
fn a_file_that_does_not_parse_stops_the_whole_pass() {
    let p = project(GROUP_ON, "module main;\n\npub fn main() -> () {\n  let n = 0 - 1;\n}\n");
    beside(&p, "broken.kh", "module shop::broken;\n\npub fn f( -> Int { 1 }\n");
    let before = snapshot(&p);
    let (_, out) = khora(&p.root, &["check", "--fix", "."]);
    assert_eq!(snapshot(&p), before, "{out}");
    assert!(out.contains("fix the parse errors first") && out.contains("broken.kh"), "{out}");
}

/// Review 3's `wp_group_writable_only`: mode `0464` lets the group write and
/// not the owner. Rust's `readonly()` means "no write bit at all", so it
/// called this writable, and the rename replaced a file its owner could not
/// write. Whether this process may write is what counts. (Skipped as root,
/// who may write anything.)
#[cfg(unix)]
#[test]
fn a_file_its_owner_may_not_write_stops_the_whole_pass() {
    use std::os::unix::fs::PermissionsExt;
    let p = project(GROUP_ON, "module main;\n\npub fn main() -> () {\n  let n = 0 - 1;\n}\n");
    let other = beside(&p, "zz.kh", "module shop::zz;\n\npub fn f() -> Int { 0 - 2 }\n");
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o464)).expect("0464");
    if std::fs::OpenOptions::new().write(true).open(&other).is_ok() {
        return; // root
    }
    let before = snapshot(&p);
    let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
    assert!(!ok, "{out}");
    assert_eq!(snapshot(&p), before, "{out}");
    assert!(out.contains("nothing was written") && out.contains("zz.kh"), "{out}");
}

/// A directory that cannot take the temporary file: the message names the
/// file it was writing, not only "Permission denied". (Skipped as root.)
#[cfg(unix)]
#[test]
fn a_staging_error_names_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let p = project(GROUP_ON, "module main;\n\npub fn main() -> () {\n  let n = 0 - 1;\n}\n");
    let src = p.root.join("src");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o555)).expect("read-only src");
    let probe = src.join(".probe");
    let writable = std::fs::write(&probe, "").is_ok();
    let _ = std::fs::remove_file(&probe);
    if !writable {
        let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).expect("restore src");
        assert!(!ok, "{out}");
        assert!(out.contains("nothing was written") && out.contains("main.kh"), "names the file:\n{out}");
    } else {
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).expect("restore src");
    }
}

// --- with labeled arguments and list literals -------------------------------------

/// A program meeting both: an unlabeled flag, `== true` in a labeled argument
/// (a typed parameter, and an untyped `let`), and each fixable lint inside
/// `if` and `for` list elements, where the element's value follows `=>`.
const MEETING: &str = "module main;\n\n\
    import std::core::{print, List, Step, Iterator};\n\n\
    fn show(n: Int, loud: Bool) -> String {\n  if loud { \"${n}!\" } else { \"${n}\" }\n}\n\n\
    fn parts(c: Bool, b: Bool, xs: List<Int>, s: String) -> List<String> {\n  \
    let g = fn () => true;\n  \
    let h = g();\n  \
    let direct = show(1, false);\n  \
    let typed = show(2, loud: b == true);\n  \
    let untyped = show(3, loud: h == true);\n  \
    let nums = [if c => 0 - 1, for x in xs => 0 - 2, if b == false => 0 - 3 else 4];\n  \
    let words = [if c => \"a \" + s + \"!\", for x in xs => \"n \" + s];\n  \
    let fns = [if c => fn (y) => y + 1, for x in xs => fn (z) => z * x];\n  \
    let blocks = [if c => {\n    if b { 10 } else { 20 }\n    0 - 5\n  }];\n  \
    [direct, typed, untyped, ..List::map(nums, fn n => \"${n}\"), ..words, ..List::map(fns, fn f => \"${f(10)}\"), ..List::map(blocks, fn n => \"${n}\")]\n}\n\n\
    pub fn main() -> () {\n  \
    for line in parts(true, true, [5, 6], \"s\") { print(line); }\n  \
    for line in parts(false, false, [], \"t\") { print(line); }\n}\n";

/// **Labeled calls, an unlabeled flag, and list elements.**
///
/// - `unlabeled-flag` is in the group, is reported, and has no fix: `show(1,
///   false)` is left as written.
/// - `loud: b == true` becomes `loud: b`; the label stays. `loud: h == true`
///   keeps its finding with no fix, because `h` is an untyped `let`.
/// - Inside `[..]`, an element's value follows `=>`, `,` or `else`, never a
///   `}`, and `-1` after any of those is a prefix minus: each `0 - n` becomes
///   `-n`, `b == false` in an `if` element's condition becomes `!b`, a
///   string chain becomes one literal, and `fn (y) =>` becomes `fn y =>`.
/// - A block element holds statements, so the statement-join guard applies
///   inside it: `0 - 5` after `if b { 10 } else { 20 }` with no `;` keeps its
///   finding and gets no fix, since `-5` there would subtract from the `if`.
/// - The fixed file checks, and a second `--fix` changes nothing.
#[test]
fn fixes_meet_labeled_arguments_and_list_elements() {
    let p = project(GROUP_ON, MEETING);
    let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
    assert!(ok, "{out}");
    let after = source(&p);
    for expected in [
        "let direct = show(1, false);",
        "let typed = show(2, loud: b);",
        "let untyped = show(3, loud: h == true);",
        "let nums = [if c => -1, for x in xs => -2, if !b => -3 else 4];",
        "let words = [if c => \"a ${s}!\", for x in xs => \"n ${s}\"];",
        "let fns = [if c => fn y => y + 1, for x in xs => fn z => z * x];",
        "    if b { 10 } else { 20 }\n    0 - 5\n",
    ] {
        assert!(after.contains(expected), "expected `{expected}` in:\n{after}\n{out}");
    }
    assert!(out.contains("[unlabeled-flag]"), "still reported:\n{out}");
    assert!(out.contains("[bool-comparison]"), "the untyped one is still reported:\n{out}");
    let (ok, again) = khora(&p.root, &["check", "--fix", "."]);
    assert!(ok, "{again}");
    assert_eq!(source(&p), after, "a second run changes nothing:\n{again}");
}

/// The same program prints the same before and after `--fix`, on either
/// backend.
#[cfg(feature = "llvm")]
#[test]
fn fixes_among_labeled_arguments_and_list_elements_print_the_same() {
    let p = project(GROUP_ON, MEETING);
    let run = |p: &Project| {
        let (ok, out) = khora(&p.root, &["run", "."]);
        assert!(ok, "{out}\n{}", source(p));
        out.lines().filter(|l| !l.starts_with("warning") && !l.trim().is_empty()).map(str::to_string).collect::<Vec<_>>()
    };
    let before = run(&p);
    assert!(before.contains(&"-1".to_string()) && before.contains(&"a s!".to_string()), "{before:?}");
    assert!(before.contains(&"-5".to_string()), "the block element is -5: {before:?}");
    let (ok, out) = khora(&p.root, &["check", "--fix", "."]);
    assert!(ok, "{out}");
    let after = run(&p);
    assert_eq!(before, after, "the fixed program prints something else:\n{}", source(&p));
}
