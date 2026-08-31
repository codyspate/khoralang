//! `khora run`, and paths that do not have to be typed.
//!
//! `run` is the first command a newcomer reaches for and the one a script
//! wraps, so what is tested here is mostly about *statuses and streams*: the
//! program's exit code has to arrive intact, and its output has to be the next
//! thing on the terminal.

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

mod pinned;

struct World {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

fn world(body: &str) -> World {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("a src directory");
    std::fs::write(project.join("khora.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n")
        .expect("a manifest");
    std::fs::write(project.join("src").join("main.kh"), body).expect("a source file");
    World { _tmp: tmp, home, project }
}

/// A program whose exit status is `answer`.
fn returning(answer: i64) -> String {
    format!("module app::main;\n\npub fn main() -> Int {{\n  {answer}\n}}\n")
}

/// Runs `khora` in `cwd`, returning the exit code and the merged output.
fn khora(w: &World, cwd: &Path, args: &[&str]) -> (Option<i32>, String) {
    khora_with(w, cwd, args, false)
}

/// The same, with the cache asked to show its working.
///
/// For the one test whose subject is the key. It prints a line per source, so
/// it is opt-in -- but the output is only ever read when a test fails, and a
/// key that moved is unreadable without it.
fn khora_explaining(w: &World, cwd: &Path, args: &[&str]) -> (Option<i32>, String) {
    khora_with(w, cwd, args, true)
}

fn khora_with(w: &World, cwd: &Path, args: &[&str], explain: bool) -> (Option<i32>, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
    if explain {
        command.env("KHORA_CACHE_EXPLAIN", "1");
    }
    // One archive for the whole test run. `pinned` says why a test has to
    // care.
    if let Some(archive) = pinned::runtime() {
        command.env("KHORA_RT_LIB", archive);
    }
    let out = command
        .args(args)
        .current_dir(cwd)
        .env("KHORA_HOME", &w.home)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code(), text)
}

/// The compiler, linker and runtime digests a run keyed on.
///
/// `KHORA_CACHE_EXPLAIN` prints them on one line. Empty when it is not on,
/// which compares equal and leaves the reuse assertion to speak.
fn toolchain_of(output: &str) -> &str {
    output.lines().find(|line| line.starts_with("khora: key from ")).unwrap_or_default()
}

#[test]
fn run_compiles_and_starts_the_program() {
    let w = world(&returning(0));

    let (code, output) = khora(&w, &w.project, &["run"]);
    assert_eq!(code, Some(0), "{output}");
    assert!(output.contains("running"), "{output}");
}

#[test]
fn the_programs_exit_status_becomes_ours() {
    // The whole point of a runner. `khora run` in a script has to behave the
    // way running the executable would, including when the program fails.
    let w = world(&returning(7));

    let (code, output) = khora(&w, &w.project, &["run"]);
    assert_eq!(code, Some(7), "{output}");
}

#[test]
fn a_program_that_does_not_compile_does_not_run() {
    let w = world("module app::main;\n\npub fn main() -> Int {\n  \"not an Int\"\n}\n");

    let (code, output) = khora(&w, &w.project, &["run"]);
    assert_ne!(code, Some(0), "{output}");
    assert!(!output.contains("running"), "it should not have started: {output}");
}

#[test]
fn the_second_run_comes_from_the_cache() {
    // Which is what makes this the command somebody reaches for rather than
    // one they wait on.
    let w = world(&returning(0));

    // **The first run is checked, and it was not.** This discarded its result
    // entirely, so a first run that failed stored nothing and the assertion
    // below blamed the second for a miss the first one caused -- reporting
    // `built ... [debug]` with no hint that anything had gone wrong earlier.
    // The test then reads as "the cache is flaky" when what it saw was one
    // failed build, which is a different problem in a different place.
    //
    // This has been intermittent under full-baseline load and passes in
    // isolation, which is exactly the shape a discarded exit status produces.
    let (first, first_output) = khora_explaining(&w, &w.project, &["run"]);
    assert_eq!(first, Some(0), "the first run has to succeed to store anything:\n{first_output}");
    assert!(
        first_output.contains("built"),
        "the first run should be the one that compiles:\n{first_output}"
    );

    let (code, output) = khora_explaining(&w, &w.project, &["run"]);
    assert_eq!(code, Some(0), "{output}");

    // **Checked before the reuse, because it is the thing that went wrong.**
    // This failed about one baseline in two, and the cache was right every
    // time: the two runs were handed *different runtime archives*, because the
    // pin was recomputed before each invocation and something rebuilt
    // `khora-rt` in between. Comparing the toolchain line the two runs printed
    // says so in one line, where "expected reused, got built" said nothing.
    // `tests/pinned` is where the fix lives.
    assert_eq!(
        toolchain_of(&first_output),
        toolchain_of(&output),
        "the toolchain moved between two runs, so the key was right to change.\n\
         first run:\n{first_output}\nsecond run:\n{output}"
    );

    assert!(
        output.contains("reused"),
        "the key moved between two runs of an unchanged tree, and it was not \
         the toolchain. The `source` lines below carry each path and its \
         digest; whichever pair differs is the input that moved.\n\n\
         first run:\n{first_output}\nsecond run:\n{output}"
    );
}

#[test]
fn arguments_after_a_double_dash_are_the_programs() {
    // They must not be parsed as `khora`'s. `--release` is a real flag of
    // ours, which makes it the one worth testing.
    let w = world(&returning(0));

    let (code, output) = khora(&w, &w.project, &["run", "--", "--release", "--no-cache"]);
    assert_eq!(code, Some(0), "{output}");
    assert!(output.contains("[debug]") || output.contains("reused"), "{output}");
    assert!(!output.contains("[release]"), "the program's flags leaked into ours: {output}");
}

#[test]
fn release_before_the_dashes_is_ours() {
    let w = world(&returning(0));

    let (code, output) = khora(&w, &w.project, &["run", "--release"]);
    assert_eq!(code, Some(0), "{output}");
    assert!(output.contains("[release]"), "{output}");
}

#[test]
fn a_workspace_root_has_no_one_program_to_run() {
    let w = world(&returning(0));
    let root = w.project.join("mono");
    std::fs::create_dir_all(root.join("packages").join("alpha").join("src"))
        .expect("a member");
    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = [\"packages/*\"]\n")
        .expect("a root manifest");
    std::fs::write(
        root.join("packages").join("alpha").join("khora.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
    )
    .expect("a member manifest");
    std::fs::write(
        root.join("packages").join("alpha").join("src").join("main.kh"),
        "module alpha::main;\n\npub fn main() -> Int {\n  0\n}\n",
    )
    .expect("a member source");

    let (code, output) = khora(&w, &root, &["run"]);
    assert_ne!(code, Some(0), "{output}");
    assert!(output.contains("no one program to run"), "{output}");
    assert!(output.contains("alpha"), "it should name the members: {output}");
}

#[test]
fn build_refuses_a_workspace_root_the_same_way() {
    // `build` used to pick whichever member's source contained `fn main(`
    // first and say nothing. `check` and `fmt` fan out because doing all of
    // them is a reading of "check the workspace"; building all of them into
    // one executable is not a reading of anything.
    let w = world(&returning(0));
    let root = w.project.join("mono");
    std::fs::create_dir_all(root.join("packages").join("alpha").join("src"))
        .expect("a member");
    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = [\"packages/*\"]\n")
        .expect("a root manifest");
    std::fs::write(
        root.join("packages").join("alpha").join("khora.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
    )
    .expect("a member manifest");
    std::fs::write(
        root.join("packages").join("alpha").join("src").join("main.kh"),
        "module alpha::main;\n\npub fn main() -> Int {\n  0\n}\n",
    )
    .expect("a member source");

    let (code, output) = khora(&w, &root, &["build"]);
    assert_ne!(code, Some(0), "{output}");
    assert!(output.contains("no one program to build"), "{output}");
}

#[test]
fn a_named_path_still_works() {
    // The point of making it optional is that it stays allowed.
    let w = world(&returning(3));

    let (code, output) = khora(&w, &w.project, &["run", "."]);
    assert_eq!(code, Some(3), "{output}");
}

#[test]
fn check_bare_and_check_dot_are_the_same_command() {
    // They were not: an empty list reached `collect_sources`, which
    // substitutes `.` there and nowhere else, so the bare form walked the
    // whole tree as one compilation while the explicit form fanned out over
    // the members.
    let w = world(&returning(0));
    let root = w.project.join("mono");
    for name in ["alpha", "beta"] {
        let member = root.join("packages").join(name);
        std::fs::create_dir_all(member.join("src")).expect("a member");
        std::fs::write(
            member.join("khora.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .expect("a member manifest");
        std::fs::write(
            member.join("src").join("lib.kh"),
            format!("module {name}::lib;\n\npub fn go() -> Int {{\n  1\n}}\n"),
        )
        .expect("a member source");
    }
    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = [\"packages/*\"]\n")
        .expect("a root manifest");

    let (bare_code, bare) = khora(&w, &root, &["check"]);
    let (dot_code, dot) = khora(&w, &root, &["check", "."]);
    assert_eq!(bare_code, dot_code, "bare:\n{bare}\ndot:\n{dot}");
    assert!(bare.contains("2 member(s) clean"), "bare:\n{bare}");
    assert!(dot.contains("2 member(s) clean"), "dot:\n{dot}");
}

#[test]
fn fmt_bare_and_fmt_dot_are_the_same_command() {
    let w = world(&returning(0));
    let root = w.project.join("mono");
    let member = root.join("packages").join("alpha");
    std::fs::create_dir_all(member.join("src")).expect("a member");
    std::fs::write(
        member.join("khora.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
    )
    .expect("a member manifest");
    std::fs::write(
        member.join("src").join("lib.kh"),
        "module alpha::lib;\n\npub fn go() -> Int {\n  1\n}\n",
    )
    .expect("a member source");
    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = [\"packages/*\"]\n")
        .expect("a root manifest");

    let (bare_code, bare) = khora(&w, &root, &["fmt", "--check"]);
    let (dot_code, dot) = khora(&w, &root, &["fmt", ".", "--check"]);
    assert_eq!(bare_code, dot_code, "bare:\n{bare}\ndot:\n{dot}");
    assert!(bare.contains("1 member(s) clean"), "bare:\n{bare}");
}

/// **`khora run some/package` runs the program where *you* are.**
///
/// Which is right — a relative path a program is given should mean what it
/// means to whoever typed it, and `cargo run` does the same. It is also a trap
/// for a package whose data sits beside it: `[permissions.fs]
/// read = ["./beside.txt"]` is written relative to the manifest, the program
/// opens `beside.txt`, the grant matches, and the file is not there. Nothing in
/// that sequence points at the working directory.
///
/// So a flag rather than a change of default.
#[test]
fn run_can_start_the_program_in_another_directory() {
    let w = world(
        "module app::main;\n\
         import std::core::{Result, attempt, print};\n\
         import std::fs::{FsRead};\n\n\
         pub fn main() -> Int {\n  \
           with { reads: FsRead::real() } {\n    \
             match attempt(fn () => reads.exists(\"beside.txt\")!) {\n      \
               Result::Ok(true) => print(\"found\"),\n      \
               Result::Ok(false) => print(\"missing\"),\n      \
               Result::Err(_e) => print(\"denied\"),\n    \
             };\n    \
             0\n  \
           }\n\
         }\n",
    );
    std::fs::write(
        w.project.join("khora.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [permissions.fs]\nread = [\"./beside.txt\"]\n",
    )
    .expect("a manifest");
    std::fs::write(w.project.join("beside.txt"), "here").expect("a file beside the package");

    // From the parent, the relative path is the parent's.
    let elsewhere = w.project.parent().expect("a parent").to_path_buf();
    let project = w.project.to_str().expect("a path").to_string();
    let (_, without) = khora(&w, &elsewhere, &["run", &project]);
    assert!(without.contains("missing"), "without --cwd it should not find it:\n{without}");

    let (_, with) = khora(&w, &elsewhere, &["run", &project, "--cwd", &project]);
    assert!(with.contains("found"), "with --cwd it should:\n{with}");
}

/// A directory that is not there is named, rather than becoming "the program
/// could not be started".
#[test]
fn run_says_which_cwd_is_missing() {
    let w = world(&returning(0));
    let (code, output) = khora(&w, &w.project, &["run", ".", "--cwd", "no-such-directory"]);
    assert_ne!(code, Some(0), "{output}");
    assert!(output.contains("no-such-directory"), "the message should name it:\n{output}");
}


/// A build leaves nothing among the sources.
///
/// **The rule this checks was written down in a `.gitignore` for four
/// revisions before anybody read it as a rule.** Output landed beside the
/// source it came from -- `src/main.exe`, `src/main.exe.o`, `src/main.pdb` --
/// so the first `git status` after a first build listed files nobody
/// recognised, and the repository grew a pattern per kind of file per tree
/// that gets built to hide them. Errata 61.
///
/// `run` and `build` are both here because they have to agree: they share a
/// cache entry and an output, and a `run` with a path of its own would rebuild
/// what `build` had just made.
#[test]
fn what_a_build_makes_goes_in_the_build_directory() {
    let w = world(&returning(0));

    let (code, output) = khora(&w, &w.project, &["build", "."]);
    assert_eq!(code, Some(0), "{output}");

    let program = w.project.join("build").join(format!("app{}", std::env::consts::EXE_SUFFIX));
    assert!(
        program.is_file(),
        "the program should be in build/, named after the package:\n{output}"
    );

    // Named after the package rather than after the file holding `main`, which
    // is the other half of the change: `build/app.exe`, not `build/main.exe`.
    assert!(output.contains("app"), "the line should name what it built:\n{output}");

    // Nothing beside the source. Anything but the `.kh` here is the bug.
    let strays: Vec<String> = std::fs::read_dir(w.project.join("src"))
        .expect("the source directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(".kh"))
        .collect();
    assert!(strays.is_empty(), "a build left {strays:?} among the sources");

    // And `run` finds the same file rather than building its own.
    let (code, again) = khora(&w, &w.project, &["run", "."]);
    assert_eq!(code, Some(0), "{again}");
    assert!(again.contains("reused"), "run should have found what build made:\n{again}");
}
