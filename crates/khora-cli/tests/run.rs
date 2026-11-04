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
    std::fs::write(project.join("khora.toml"), pinned("[package]\nname = \"app\"\nversion = \"0.1.0\"\n"))
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
    std::fs::write(root.join("khora.toml"), pinned("[workspace]\nmembers = [\"packages/*\"]\n"))
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
    std::fs::write(root.join("khora.toml"), pinned("[workspace]\nmembers = [\"packages/*\"]\n"))
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
    std::fs::write(root.join("khora.toml"), pinned("[workspace]\nmembers = [\"packages/*\"]\n"))
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
    std::fs::write(root.join("khora.toml"), pinned("[workspace]\nmembers = [\"packages/*\"]\n"))
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
        pinned("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [permissions.fs]\nread = [\"./beside.txt\"]\n"),
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
/// recognized, and the repository grew a pattern per kind of file per tree
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

/// A manifest with the `[toolchain]` table every project now needs.
///
/// **A pin is required, and these fixtures live in the system temporary
/// directory** -- unlike the suites under `CARGO_TARGET_TMPDIR`, which sit
/// inside this repository and inherit its pin by the same upward walk a real
/// project uses. There is nothing above these, so they carry their own.
///
/// It names the version under test. That is the binary these tests are about to
/// run, so the pin resolves to "the one already running" and no handover
/// happens -- which is what keeps this a fixture detail rather than a thing the
/// tests have to think about.
fn pinned(manifest: &str) -> String {
    format!("{manifest}\n[toolchain]\nversion = \"{}\"\n", khora_toolchain::RUNNING)
}

/// **`[permissions.fs]` was advisory for any program holding `Process`.**
///
/// With `read = ["./data/**"]`, `read_text("/etc/hostname")` was refused and
/// `checked_output("cat", ["/etc/hostname"])` returned the file — so every
/// path grant in the table was one subprocess away from meaning nothing. A
/// grant that another program can be asked to walk around is not a grant.
///
/// End to end rather than against the matcher, because the thing that was
/// missing was the *category*: the manifest key, the rendered
/// `std::permissions::grants::process`, and the handler that consults it.
/// Roadmap 16, "the `process` permission category".
#[test]
fn a_subprocess_cannot_read_what_the_manifest_denies() {
    let w = world(
        "module app::main;\n\
         import std::core::{List, print};\n\
         import std::process::{Process, ProcessError, checked_output};\n\n\
         pub fn main() -> Int {\n  \
           with { process: Process::real() } {\n    \
             print(checked_output(\"echo\", [\"reached\"])! catch {\n      \
               ProcessError::Denied(c) => \"denied ${c}\",\n      \
               ProcessError::NotStarted(c) => \"not started ${c}\",\n      \
               ProcessError::NotText(c) => \"not text ${c}\",\n      \
               ProcessError::Failed(c, _s) => \"failed ${c}\",\n    \
             });\n    \
             0\n  \
           }\n\
         }\n",
    );

    // Granted by name: the program runs and its output comes back.
    std::fs::write(
        w.project.join("khora.toml"),
        pinned(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [permissions]\nprocess = [\"echo\"]\n",
        ),
    )
    .expect("a manifest");
    let (_, allowed) = khora(&w, &w.project, &["run", ".", "--no-cache"]);
    assert!(allowed.contains("reached"), "a granted program should run:\n{allowed}");

    // A table that grants something else does not grant this.
    std::fs::write(
        w.project.join("khora.toml"),
        pinned(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [permissions]\nprocess = [\"git\"]\n",
        ),
    )
    .expect("a manifest");
    let (_, refused) = khora(&w, &w.project, &["run", ".", "--no-cache"]);
    assert!(refused.contains("denied echo"), "and one it does not name should not:\n{refused}");

    // And `default = "deny"` denies it without the key being written at all,
    // which is the case 16.1 exists for.
    std::fs::write(
        w.project.join("khora.toml"),
        pinned(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [permissions]\ndefault = \"deny\"\n",
        ),
    )
    .expect("a manifest");
    let (_, denied) = khora(&w, &w.project, &["run", ".", "--no-cache"]);
    assert!(denied.contains("denied echo"), "`default = \"deny\"` covers it too:\n{denied}");
}

/// **Nothing of the toolchain's own reaches the program's stdout.**
///
/// `khora run --help` claims the command is usable in a script the way running
/// the executable is. It was not: `reused ... from the cache` and `running
/// ...` went to stdout, so `khora run . | head -1` read the toolchain's
/// progress rather than the program's first line, and `2>/dev/null` did not
/// help because none of it was on stderr.
#[test]
fn build_chatter_stays_off_the_programs_stdout() {
    let w = world(
        "module app::main;\n\nimport std::core::{print};\n\n\
         pub fn main() -> Int {\n  print(\"first line\");\n  0\n}\n",
    );

    // Twice, because the two runs print different things -- a real build the
    // first time and a cache hit the second -- and both were on stdout.
    for attempt in 1..=2 {
        let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
        if let Some(archive) = pinned::runtime() {
            command.env("KHORA_RT_LIB", archive);
        }
        let out = command
            .args(["run", "."])
            .current_dir(&w.project)
            .env("KHORA_HOME", &w.home)
            .output()
            .expect("could not run `khora`");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(0), "run {attempt}:\n{stdout}{stderr}");
        assert_eq!(
            stdout.lines().next(),
            Some("first line"),
            "run {attempt}: the program's first line has to be the first line of stdout.\n\
             stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            !stdout.contains("running "),
            "run {attempt}: toolchain progress on stdout:\n{stdout}"
        );
        assert!(
            !stdout.contains("from the cache"),
            "run {attempt}: cache progress on stdout:\n{stdout}"
        );
    }
}

/// A verdict belongs to the test that earned it, across files.
///
/// **Keys are numbered per file, and the lookup ignored the module.** The nth
/// `test` block of one file and the nth of another both have key `#test$n`, so
/// a search by key alone returned whichever instance was found first, and every
/// verdict after the collision landed on the wrong name: a test whose body is
/// `assert(2 == 2)` was reported `FAILED`, another vanished from the report,
/// and the totals described a suite that did not exist.
///
/// **Driven through the CLI on a real package, not through `compile_tests`
/// directly.** An in-process fixture with two hand-built `SourceFile`s does
/// *not* reproduce this: the collision depends on the order instances are
/// found in, and a real build has `std` in the root ahead of the program. A
/// unit test that passes either way would have been worse than none.
///
/// The same lookup serves `khora bench`, where it reported a `Dict` insertion
/// ladder getting six times *faster* as it grew.
#[test]
fn a_verdict_belongs_to_the_test_that_earned_it() {
    let w = world(
        "module app::main;\n\
         \n\
         import std::core::{assert};\n\
         \n\
         test \"A in main passes\" { assert(1 == 1); }\n\
         \n\
         test \"B in main fails\" { assert(1 == 2); }\n\
         \n\
         pub fn main() -> Int { 0 }\n",
    );
    std::fs::write(
        w.project.join("src").join("other.kh"),
        "module app::other;\n\
         \n\
         import std::core::{assert};\n\
         \n\
         test \"C in other passes\" { assert(1 == 1); }\n\
         \n\
         test \"D in other cannot fail\" { assert(2 == 2); }\n",
    )
    .expect("a second source file");

    let (_code, text) = khora(&w, &w.project, &["test", "."]);

    assert!(
        text.contains("test A in main passes ... ok"),
        "a passing test should be reported, and as passing:\n{text}"
    );
    assert!(
        text.contains("test B in main fails ... FAILED"),
        "the one failing body is the one that fails:\n{text}"
    );
    assert!(
        text.contains("test C in other passes ... ok"),
        "the second file's tests keep their own verdicts:\n{text}"
    );
    assert!(
        text.contains("test D in other cannot fail ... ok"),
        "`assert(2 == 2)` cannot fail, whatever file it is in:\n{text}"
    );
    assert!(
        text.contains("3 passed, 1 failed"),
        "the totals should match the bodies:\n{text}"
    );
}

/// **`khora test` runs the package's own `test` blocks, and no dependency's.**
///
/// A dependency's files are in the compilation because the package uses its
/// code, and its `test` blocks used to come along: they ran under *this*
/// package's manifest, so a library's suite passed or failed by who tested
/// it. `postgres`'s wire tests dial loopback, and inside a service granted
/// only its database port they were refused and then waited for ever. The
/// dependency's own run is unaffected and is asserted too, so the fix is not
/// a test that stopped running anywhere.
///
/// The dependency's test would also fail if it ran here, which is the half a
/// count alone cannot show: the dependent's run reporting `1 passed, 0
/// failed` means the failing block was not in it.
#[test]
fn a_dependencys_test_blocks_are_not_this_packages() {
    let w = world(
        "module app::main;\n\
         \n\
         import std::core::{assert};\n\
         import greet::lib::{greeting};\n\
         \n\
         test \"the app's own test\" { assert(greeting() == 7); }\n\
         \n\
         pub fn main() -> Int { greeting() }\n",
    );
    let lib = w.project.parent().expect("a parent").join("greet");
    std::fs::create_dir_all(lib.join("src")).expect("a library");
    std::fs::write(
        lib.join("khora.toml"),
        pinned("[package]\nname = \"greet\"\nversion = \"0.1.0\"\n"),
    )
    .expect("a manifest");
    std::fs::write(
        lib.join("src").join("lib.kh"),
        "module greet::lib;\n\
         \n\
         import std::core::{assert};\n\
         \n\
         pub fn greeting() -> Int { 7 }\n\
         \n\
         test \"the library's own test\" { assert(greeting() == 7); }\n\
         \n\
         test \"a library test that fails under its dependent\" { assert(greeting() == 8); }\n",
    )
    .expect("a library source");
    std::fs::write(
        w.project.join("khora.toml"),
        pinned(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ngreet = { path = \"../greet\" }\n",
        ),
    )
    .expect("a manifest");

    let (code, text) = khora(&w, &w.project, &["test", "."]);
    assert!(text.contains("test the app's own test ... ok"), "the app's test should run:\n{text}");
    assert!(
        !text.contains("library"),
        "a dependency's test blocks must not run in its dependent's suite:\n{text}"
    );
    assert!(text.contains("1 passed, 0 failed"), "only the app's test:\n{text}");
    assert_eq!(code, Some(0), "{text}");

    // And the library's suite is still the library's, run where it lives.
    let (code, text) = khora(&w, &lib, &["test", "."]);
    assert!(text.contains("test the library's own test ... ok"), "{text}");
    assert!(text.contains("1 passed, 1 failed"), "the library runs both of its own:\n{text}");
    assert_eq!(code, Some(1), "{text}");
}

// ---------------------------------------------------------------------------
// Which `test` blocks are a package's own, in each layout a package can have.
//
// The reviewer's `p1` rows: the dependency vendored inside the package, a
// link that resolves into it, and a `src` that is itself a link out -- each
// got the own set wrong when "own" was a path prefix -- and the layouts that
// were right, which stay right. Every test here runs `khora test` with the
// cache off, so the set is decided by the source, not by an earlier run.

const OWN_LIB: &str = "module greet::lib;\n\nimport std::core::{assert};\n\npub fn greeting() -> Int { 7 }\n\ntest \"LIB test\" { assert(greeting() == 7); }\n";
const OWN_APP: &str = "module app::main;\n\nimport std::core::{assert};\nimport greet::lib::{greeting};\n\ntest \"APP test\" { assert(greeting() == 7); }\n\npub fn main() -> Int { greeting() - 7 }\n";

/// A scratch directory for one layout, with `KHORA_HOME` inside it.
fn layout() -> World {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let home = tmp.path().join("home");
    let project = tmp.path().join("app");
    World { _tmp: tmp, home, project }
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(path, text).expect("a file");
}

/// The library `greet` at `dir`.
fn greet_at(dir: &Path) {
    put(&dir.join("khora.toml"), &pinned("[package]\nname = \"greet\"\nversion = \"0.1.0\"\n"));
    put(&dir.join("src").join("lib.kh"), OWN_LIB);
}

/// The app at `dir`, depending on `greet` at `dependency` (relative).
fn app_at(dir: &Path, dependency: &str) {
    put(
        &dir.join("khora.toml"),
        &pinned(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ngreet = {{ path = \"{dependency}\" }}\n"
        )),
    );
    put(&dir.join("src").join("main.kh"), OWN_APP);
}

/// The test names a run printed, sorted.
fn ran(output: &str) -> Vec<String> {
    let mut names: Vec<String> = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("test "))
        .filter_map(|line| line.split(" ...").next())
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

/// `khora test <args> --no-cache` in `cwd`, and the tests it ran.
fn tested(w: &World, cwd: &Path, args: &[&str]) -> (Option<i32>, Vec<String>, String) {
    let mut all = vec!["test"];
    all.extend_from_slice(args);
    all.push("--no-cache");
    let (code, text) = khora(w, cwd, &all);
    let names = ran(&text);
    (code, names, text)
}

/// **M1: a dependency vendored inside the package is still a dependency.**
/// Its directory is under the package's, and a path-prefix "own" ran its
/// tests in the dependent's suite under the dependent's manifest -- the
/// ledger_service hang, reached by vendoring postgres instead of placing it
/// beside the service. Three spellings of "inside": at the package root, in
/// `src`, and through a link whose target is inside.
// Links are made with `std::os::unix::fs::symlink`.
#[cfg(unix)]
#[test]
fn a_dependency_inside_the_package_is_not_its_own() {
    for (label, dependency, vendored) in [
        ("vendor", "vendor/greet", "vendor/greet"),
        ("src/vendor", "src/vendor/greet", "src/vendor/greet"),
    ] {
        let w = layout();
        app_at(&w.project, dependency);
        greet_at(&w.project.join(vendored));
        let (code, names, text) = tested(&w, &w.project, &["."]);
        assert_eq!(names, ["APP test"], "{label}: only the app's own test:\n{text}");
        assert_eq!(code, Some(0), "{label}: {text}");
    }

    // `../greet` is a link to a directory inside the app.
    let w = layout();
    app_at(&w.project, "../greet");
    greet_at(&w.project.join("third_party").join("greet"));
    std::os::unix::fs::symlink(
        w.project.join("third_party").join("greet"),
        w.project.parent().expect("a parent").join("greet"),
    )
    .expect("a link");
    let (code, names, text) = tested(&w, &w.project, &["."]);
    assert_eq!(names, ["APP test"], "a linked dependency resolving into the app:\n{text}");
    assert_eq!(code, Some(0), "{text}");
}

/// **M2: a `src` that is a link out of the package keeps its tests.** Its
/// files resolve outside the package's directory, so a path prefix found
/// none of them its own and the run was a green `no tests`.
// Links are made with `std::os::unix::fs::symlink`.
#[cfg(unix)]
#[test]
fn a_linked_src_is_still_the_packages() {
    let w = layout();
    let outside = w.project.parent().expect("a parent").join("real");
    app_at(&outside, "../greet");
    greet_at(&w.project.parent().expect("a parent").join("greet"));
    std::fs::create_dir_all(&w.project).expect("the app");
    std::fs::rename(outside.join("khora.toml"), w.project.join("khora.toml")).expect("the manifest");
    std::os::unix::fs::symlink(outside.join("src"), w.project.join("src")).expect("a link");
    let (code, names, text) = tested(&w, &w.project, &["."]);
    assert_eq!(names, ["APP test"], "the package's own test, through its linked src:\n{text}");
    assert_eq!(code, Some(0), "{text}");
}

/// **The layouts that were right stay right.** A sibling dependency; the
/// package reached through a linked alias, by name and from inside; a named
/// file; a `tests/` directory beside `src`; a `src/bin` program; a nested
/// package that is not a dependency, and that package tested by itself.
// Links are made with `std::os::unix::fs::symlink`.
#[cfg(unix)]
#[test]
fn the_own_set_in_the_layouts_that_were_already_right() {
    let w = layout();
    let root = w.project.parent().expect("a parent").to_path_buf();
    app_at(&w.project, "../greet");
    greet_at(&root.join("greet"));
    put(
        &w.project.join("src").join("other.kh"),
        "module app::other;\n\nimport std::core::{assert};\n\ntest \"OTHER test\" { assert(1 == 1); }\n",
    );
    put(
        &w.project.join("tests").join("extra.kh"),
        "module app::extra;\n\nimport std::core::{assert};\nimport greet::lib::{greeting};\n\ntest \"TESTS-DIR test\" { assert(greeting() == 7); }\n",
    );
    put(
        &w.project.join("src").join("bin").join("tool.kh"),
        "module app::tool;\n\nimport std::core::{assert};\nimport greet::lib::{greeting};\n\ntest \"BIN test\" { assert(greeting() == 7); }\n\npub fn main() -> Int { 0 }\n",
    );
    put(&w.project.join("scratch").join("khora.toml"), &pinned("[package]\nname = \"scratch\"\nversion = \"0.1.0\"\n"));
    put(
        &w.project.join("scratch").join("src").join("main.kh"),
        "module scratch::main;\n\nimport std::core::{assert};\n\ntest \"NESTED test\" { assert(true); }\n\npub fn main() -> Int { 0 }\n",
    );
    std::os::unix::fs::symlink(&w.project, root.join("alias")).expect("a link");

    let whole = ["APP test", "OTHER test", "TESTS-DIR test"];
    for (cwd, args) in [
        (w.project.clone(), vec!["."]),
        (root.clone(), vec!["alias"]),
        (root.join("alias"), vec!["."]),
        (w.project.clone(), vec!["src/main.kh"]),
    ] {
        let (code, names, text) = tested(&w, &cwd, &args);
        assert_eq!(names, whole, "{} {args:?}: the app's own tests, and no LIB, BIN or NESTED:\n{text}", cwd.display());
        assert_eq!(code, Some(0), "{text}");
    }
    let (_, names, text) = tested(&w, &w.project.join("scratch"), &["."]);
    assert_eq!(names, ["NESTED test"], "the nested package's own:\n{text}");
}

/// **A workspace root runs each member's own tests once.** The member that
/// depends on another ran that member's tests a second time, inside its own
/// suite.
#[test]
fn a_workspace_runs_each_members_tests_once() {
    let w = layout();
    let root = w.project.parent().expect("a parent").to_path_buf();
    put(&root.join("khora.toml"), &pinned("[workspace]\nmembers = [\"greet\", \"app\"]\n"));
    greet_at(&root.join("greet"));
    app_at(&w.project, "../greet");
    let (code, names, text) = tested(&w, &root, &["."]);
    assert_eq!(names, ["APP test", "LIB test"], "each member's own, once:\n{text}");
    assert_eq!(code, Some(0), "{text}");
}

/// **M3: which files are the package's is part of the harness's cache key.**
///
/// Two packages with identical source texts: A is an app with `greet` beside
/// it as a dependency, B holds both files itself. In the release profile no
/// path is in the key, so the texts alone cannot tell them apart -- and B's
/// own tests include `greet`'s, where A's do not. Without the own set in the
/// key, B is served A's harness and silently skips a test. A again reuses
/// its own entry.
#[test]
fn the_package_boundary_is_in_the_harness_key() {
    let w = layout();
    let root = w.project.parent().expect("a parent").to_path_buf();
    let a = root.join("A").join("app");
    app_at(&a, "../greet");
    greet_at(&root.join("A").join("greet"));
    let b = root.join("B").join("app");
    put(&b.join("khora.toml"), &pinned("[package]\nname = \"app\"\nversion = \"0.1.0\"\n"));
    put(&b.join("src").join("main.kh"), OWN_APP);
    put(&b.join("src").join("greet").join("lib.kh"), OWN_LIB);

    let release = |cwd: &Path| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_khora"));
        if let Some(archive) = pinned::runtime() {
            command.env("KHORA_RT_LIB", archive);
        }
        let out = command
            .args(["test", "."])
            .current_dir(cwd)
            .env("KHORA_HOME", &w.home)
            .env("KHORA_PROFILE", "release")
            .output()
            .expect("could not run `khora`");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        text
    };

    let first = release(&a);
    assert_eq!(ran(&first), ["APP test"], "{first}");
    let second = release(&b);
    assert!(!second.contains("from the cache"), "B must not be served A's harness:\n{second}");
    assert_eq!(ran(&second), ["APP test", "LIB test"], "B's own tests include the library's:\n{second}");
    let third = release(&a);
    assert!(third.contains("from the cache"), "A's own entry is still there:\n{third}");
    assert_eq!(ran(&third), ["APP test"], "{third}");
}

#[cfg(unix)]
const OWN_SCRIPT: &str = "module script;\n\nimport std::core::{assert};\n\ntest \"SCRIPT test\" { assert(1 == 1); }\n\npub fn main() -> Int { 0 }\n";

/// **A named file is the package's, however it is reached.** `bin/s.kh` is a
/// link to a file inside the app, and no package encloses `bin`; a rule that
/// asked where the real file lives found the app's manifest above it and
/// dropped the file the command named, for a green `no tests`. A
/// manifest-less directory linked the same way did the same.
#[cfg(unix)]
#[test]
fn a_named_link_into_a_package_keeps_its_tests() {
    let w = layout();
    let root = w.project.parent().expect("a parent").to_path_buf();
    app_at(&w.project, "../greet");
    greet_at(&root.join("greet"));
    put(&w.project.join("tools").join("s.kh"), OWN_SCRIPT);
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    std::os::unix::fs::symlink(w.project.join("tools").join("s.kh"), root.join("bin").join("s.kh")).expect("a link");
    std::os::unix::fs::symlink(w.project.join("tools"), root.join("loose")).expect("a link");

    for args in [["bin/s.kh"], ["loose"]] {
        let (code, names, text) = tested(&w, &root, &args);
        assert_eq!(names, ["SCRIPT test"], "{args:?}: the named file's own test:\n{text}");
        assert_eq!(code, Some(0), "{text}");
    }
    // And the harness went beside what was named, not into the toolchain's
    // `std`, which is where the first input sorted.
    assert!(root.join("bin").join("khora-tests").exists(), "the harness beside the named file");
}

/// **A dependency whose `src` is a link into the app is still the
/// dependency.** Its manifest sits beside the app and its sources live in
/// the app's `third_party`. Both walks reach them, and they are the
/// dependency's: the app does not run them under its own manifest, and the
/// dependency, tested by itself, still has them.
#[cfg(unix)]
#[test]
fn a_dependencys_linked_src_stays_the_dependencys() {
    let w = layout();
    let root = w.project.parent().expect("a parent").to_path_buf();
    app_at(&w.project, "../greet");
    put(&root.join("greet").join("khora.toml"), &pinned("[package]\nname = \"greet\"\nversion = \"0.1.0\"\n"));
    put(&w.project.join("third_party").join("greet_src").join("lib.kh"), OWN_LIB);
    std::os::unix::fs::symlink(w.project.join("third_party").join("greet_src"), root.join("greet").join("src"))
        .expect("a link");

    let (code, names, text) = tested(&w, &w.project, &["."]);
    assert_eq!(names, ["APP test"], "the app's own, and not the dependency's:\n{text}");
    assert_eq!(code, Some(0), "{text}");
    let (code, names, text) = tested(&w, &root.join("greet"), &["."]);
    assert_eq!(names, ["LIB test"], "the dependency's own, through its linked src:\n{text}");
    assert_eq!(code, Some(0), "{text}");
}

/// **A `src` linked into some other package's directory is still this
/// package's.** The real files sit under `shared`, a package that is not a
/// dependency, and nothing but the app's walk reaches them.
#[cfg(unix)]
#[test]
fn a_src_linked_into_another_package_is_still_the_packages() {
    let w = layout();
    let root = w.project.parent().expect("a parent").to_path_buf();
    greet_at(&root.join("greet"));
    put(&root.join("shared").join("khora.toml"), &pinned("[package]\nname = \"shared\"\nversion = \"0.1.0\"\n"));
    put(&root.join("shared").join("src").join("lib.kh"), "module shared::lib;\n\npub fn s() -> Int { 1 }\n");
    put(&root.join("shared").join("appsrc_holder").join("src").join("main.kh"), OWN_APP);
    std::fs::create_dir_all(&w.project).expect("the app");
    put(
        &w.project.join("khora.toml"),
        &pinned("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ngreet = { path = \"../greet\" }\n"),
    );
    std::os::unix::fs::symlink(root.join("shared").join("appsrc_holder").join("src"), w.project.join("src"))
        .expect("a link");

    let (code, names, text) = tested(&w, &w.project, &["."]);
    assert_eq!(names, ["APP test"], "the app's own test, through a src inside another package:\n{text}");
    assert_eq!(code, Some(0), "{text}");
}

/// **A workspace member nested in another member and depended on by it runs
/// its tests once, as itself.** `app/greet` is both a member and `app`'s
/// path dependency.
#[test]
fn a_nested_member_that_is_a_dependency_runs_once() {
    let w = layout();
    let root = w.project.parent().expect("a parent").to_path_buf();
    put(&root.join("khora.toml"), &pinned("[workspace]\nmembers = [\"app\", \"app/greet\"]\n"));
    app_at(&w.project, "greet");
    greet_at(&w.project.join("greet"));

    let (code, names, text) = tested(&w, &root, &["."]);
    assert_eq!(names, ["APP test", "LIB test"], "each member's own, once:\n{text}");
    assert_eq!(code, Some(0), "{text}");
    let (_, names, text) = tested(&w, &w.project, &["."]);
    assert_eq!(names, ["APP test"], "{text}");
    let (_, names, text) = tested(&w, &w.project.join("greet"), &["."]);
    assert_eq!(names, ["LIB test"], "{text}");
}
