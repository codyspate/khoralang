//! `khora test` and `khora bench` through the build cache.
//!
//! **What these prevent: a test run that reports on code it did not build.** A
//! cached harness that is served after an input changed runs the old tests
//! against the old program and says they passed. So every input the key is
//! meant to hold gets a test here that changes that input alone and requires
//! the next run to build, and the source test goes further and makes the
//! change one that the stale binary would pass and a fresh one fails.
//!
//! Each test builds for real, in its own `KHORA_HOME`, against a runtime
//! archive pinned for the whole process -- `pinned` says why.

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

mod pinned;

struct World {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    /// Every run so far, for an assertion message.
    story: std::cell::RefCell<String>,
}

/// The tree's own `std`, named explicitly so that a copied compiler finds it.
fn tree_std() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")
}

fn world(main: &str) -> World {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("a src directory");
    let manifest = format!(
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[toolchain]\nversion = \"{}\"\n",
        khora_toolchain::RUNNING
    );
    std::fs::write(project.join("khora.toml"), manifest).expect("a manifest");
    std::fs::write(project.join("src").join("main.kh"), main).expect("a source file");
    World { _tmp: tmp, home, project, story: std::cell::RefCell::new(String::new()) }
}

/// A package with one function, one test of it and one bench of it.
fn package(answer: i64, expected: i64) -> String {
    format!(
        "module app::main;\n\nimport std::core::{{assert_that, print}};\n\n\
         pub fn answer() -> Int {{ {answer} }}\n\n\
         pub fn main() {{ print(\"the program ran\"); }}\n\n\
         test \"the answer\" {{\n  assert_that(answer() == {expected}, \"the answer is {expected}\");\n}}\n\n\
         bench \"answering\" {{\n  answer();\n}}\n"
    )
}

/// Runs `khora` with `args`, `KHORA_CACHE_EXPLAIN` on, and `env` on top.
fn run_with(w: &World, compiler: &Path, env: &[(&str, &str)], args: &[&str]) -> (bool, String) {
    let mut command = Command::new(compiler);
    if let Some(pinned) = pinned::runtime() {
        command.env("KHORA_RT_LIB", pinned);
    }
    command
        .args(args)
        .current_dir(&w.project)
        .env("KHORA_HOME", &w.home)
        .env("KHORA_STD", tree_std())
        .env("KHORA_CACHE_EXPLAIN", "1")
        .env_remove("KHORA_PROFILE")
        .env_remove("KHORA_DEBUG")
        .env_remove("KHORA_TARGET")
        .env_remove("KHORA_FIBERS");
    for (name, value) in env {
        command.env(name, value);
    }
    let out = command.output().expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    w.story.borrow_mut().push_str(&format!("\n--- {env:?} khora {}\n{text}", args.join(" ")));
    (out.status.success(), text)
}

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_khora"))
}

fn test(w: &World, env: &[(&str, &str)]) -> (bool, String) {
    run_with(w, &compiler(), env, &["test", "."])
}

/// Whether a run's harness came from the cache.
fn reused(output: &str) -> bool {
    output.contains("reused ") && output.contains("from the cache")
}

/// Runs `khora test` once to fill the cache, then checks that a second run
/// with nothing changed is served from it.
fn warmed(w: &World) {
    let (ok, first) = test(w, &[]);
    assert!(ok && !reused(&first), "the first run builds: {}", w.story.borrow());
    let (ok, second) = test(w, &[]);
    assert!(ok, "{}", w.story.borrow());
    assert!(reused(&second), "an unchanged second run is reused: {}", w.story.borrow());
    assert!(second.contains("1 passed"), "and the reused harness runs the tests: {second}");
}

#[test]
fn a_second_khora_test_with_nothing_changed_is_reused() {
    let w = world(&package(41, 41));
    warmed(&w);
}

/// **The one that matters most**: an edit the stale harness would pass.
///
/// The new source makes the test fail. A harness served from the old key
/// would still say `1 passed`, so this is red for a key that ignored the
/// source, not merely for one that reported the wrong word.
#[test]
fn an_edited_source_is_rebuilt_and_its_tests_are_the_ones_that_run() {
    let w = world(&package(41, 41));
    warmed(&w);

    std::fs::write(w.project.join("src").join("main.kh"), package(40, 41)).expect("an edit");
    let (ok, output) = test(&w, &[]);
    assert!(!reused(&output), "an edited package must be rebuilt: {}", w.story.borrow());
    assert!(!ok && output.contains("1 failed"), "the edit breaks the test: {}", w.story.borrow());
}

/// A change to `std` is a change to the program, though no file of the
/// package moved.
#[test]
fn an_edited_standard_library_is_rebuilt() {
    let w = world(&package(41, 41));
    let copy = w.home.parent().expect("the scratch directory").join("std");
    copy_tree(&tree_std(), &copy);
    let std_env = [("KHORA_STD", copy.to_str().expect("a path"))];

    let (ok, first) = test(&w, &std_env);
    assert!(ok && !reused(&first), "{}", w.story.borrow());
    let (_, second) = test(&w, &std_env);
    assert!(reused(&second), "{}", w.story.borrow());

    let core = copy.join("core.kh");
    let mut text = std::fs::read_to_string(&core).expect("std's core");
    text.push_str("\n// an edit to std\n");
    std::fs::write(&core, text).expect("an edit to std");
    let (ok, third) = test(&w, &std_env);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&third), "an edited std must be rebuilt: {}", w.story.borrow());
}

/// A different compiler binary is a different toolchain, even at the same
/// version.
#[test]
fn a_different_compiler_is_rebuilt() {
    let w = world(&package(41, 41));
    warmed(&w);

    // The same compiler with one byte more: it runs the same, and it is not
    // the same file, which is all the key can know about a compiler.
    let other = w
        .home
        .parent()
        .expect("the scratch directory")
        .join(format!("other-khora{}", std::env::consts::EXE_SUFFIX));
    let mut bytes = std::fs::read(compiler()).expect("the compiler");
    bytes.push(0);
    std::fs::write(&other, bytes).expect("a copy of the compiler");
    make_executable(&other);

    let (ok, output) = run_with(&w, &other, &[], &["test", "."]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&output), "another compiler must rebuild: {}", w.story.borrow());
}

/// The profile is in the key, with debug information held equal.
///
/// `KHORA_DEBUG=1` keeps debug information on in release, so that the profile
/// is the only input that differs; without it, release also turns debug
/// information off and the test could not tell which of the two the key held.
#[test]
fn the_profile_is_in_the_key() {
    let w = world(&package(41, 41));
    warmed(&w);

    let (ok, output) = test(&w, &[("KHORA_PROFILE", "release"), ("KHORA_DEBUG", "1")]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&output), "a release harness is not a debug one: {}", w.story.borrow());
}

/// Debug information is in the key, with the profile held equal.
#[test]
fn debug_information_is_in_the_key() {
    let w = world(&package(41, 41));
    warmed(&w);

    let (ok, output) = test(&w, &[("KHORA_DEBUG", "0")]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&output), "debug information off is another build: {}", w.story.borrow());
}

/// The target triple is in the key. The host's own triple, spelled out, is
/// the one target that both changes the key and still runs here.
#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_target_is_in_the_key() {
    let w = world(&package(41, 41));
    warmed(&w);

    let (ok, output) = test(&w, &[("KHORA_TARGET", "x86_64-unknown-linux-gnu")]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&output), "a named target is another build: {}", w.story.borrow());
}

/// **Which fiber backend a program gets by default is in the runtime
/// archive**, and a different archive rebuilds.
///
/// `KHORA_FIBERS` itself is read when the harness starts rather than when it
/// is built, so its default is the archive's `fiber::on_the_scheduler` and
/// nothing in generated code. The archive is hashed into the key whole; this
/// changes it by one appended archive member and requires a rebuild.
///
/// Not on macOS: its linker refuses an archive member that is not a Mach-O
/// object, and there is no member every linker ignores. The key is the same
/// code on every platform.
#[test]
#[cfg(not(target_os = "macos"))]
fn a_different_runtime_archive_is_rebuilt() {
    let Some(pinned) = pinned::runtime() else {
        panic!("no runtime archive beside the compiler to test against");
    };
    let w = world(&package(41, 41));
    warmed(&w);

    let other = w.home.parent().expect("the scratch directory").join(pinned.file_name().expect("a name"));
    let mut bytes = std::fs::read(&pinned).expect("the runtime archive");
    bytes.extend_from_slice(&padding_member());
    std::fs::write(&other, bytes).expect("a changed archive");

    let (ok, output) = test(&w, &[("KHORA_RT_LIB", other.to_str().expect("a path"))]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&output), "another runtime must rebuild: {}", w.story.borrow());
}

/// One cached runner serves both fiber backends, because the backend is
/// chosen when it runs.
#[test]
fn a_cached_harness_runs_on_either_fiber_backend() {
    let w = world(&package(41, 41));
    warmed(&w);
    for backend in ["threads", "scheduler"] {
        let (ok, output) = test(&w, &[("KHORA_FIBERS", backend)]);
        assert!(ok && reused(&output), "{backend}: {}", w.story.borrow());
        assert!(output.contains("1 passed"), "{backend}: {output}");
    }
}

/// The program, the test runner and the bench runner are three artifacts from
/// one set of sources, and none is served for another.
#[test]
fn a_program_a_test_runner_and_a_bench_runner_are_kept_apart() {
    let w = world(&package(41, 41));

    let (ok, built) = run_with(&w, &compiler(), &[], &["build", "."]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&built), "{}", w.story.borrow());

    let (ok, tested) = test(&w, &[]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&tested), "the program is not the test runner: {}", w.story.borrow());
    assert!(tested.contains("1 passed"), "{}", w.story.borrow());
    assert!(!tested.contains("the program ran"), "{}", w.story.borrow());

    let (ok, benched) = run_with(&w, &compiler(), &[], &["bench", "."]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&benched), "the test runner is not the bench runner: {}", w.story.borrow());
    assert!(benched.contains("answering"), "{}", w.story.borrow());

    let (ok, again) = run_with(&w, &compiler(), &[], &["bench", "."]);
    assert!(ok && reused(&again), "and a second bench run is reused: {}", w.story.borrow());
}

/// A program in a scratch package outside the repository, with a manifest that
/// permits its own `extern`s and links the archive in `vendor` if one is named.
fn native_package(main: &str, link: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("a src directory");
    let mut manifest = format!(
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[toolchain]\nversion = \"{}\"\n\n\
         [permissions]\nextern = [\"app\"]\n",
        khora_toolchain::RUNNING
    );
    if let Some(name) = link {
        manifest.push_str(&format!("\n[build]\nlink = [\"{name}\"]\nlink-search = [\"../vendor\"]\n"));
    }
    std::fs::write(project.join("khora.toml"), manifest).expect("a manifest");
    std::fs::write(project.join("src").join("main.kh"), main).expect("a source file");
    (tmp, project)
}

/// A C archive in `<scratch>/vendor`, built from `source`, named for `-l<name>`.
fn c_archive(scratch: &Path, name: &str, source: &str) {
    let vendor = scratch.join("vendor");
    std::fs::create_dir_all(&vendor).expect("a vendor directory");
    let c = vendor.join(format!("{name}.c"));
    std::fs::write(&c, source).expect("the C source");
    let object = vendor.join(format!("{name}.o"));
    let clang = khora_codegen_llvm::toolchain::linker().expect("a C compiler");
    let compiled =
        Command::new(&clang).arg("-c").arg(&c).arg("-o").arg(&object).output().expect("running clang");
    assert!(compiled.status.success(), "compiling the C archive: {}", String::from_utf8_lossy(&compiled.stderr));
    let archive = vendor.join(if cfg!(windows) { format!("{name}.lib") } else { format!("lib{name}.a") });
    let ar = khora_codegen_llvm::toolchain::tool("llvm-ar")
        .or_else(|| khora_codegen_llvm::toolchain::tool("ar"))
        .unwrap_or_else(|| PathBuf::from("ar"));
    let archived = Command::new(&ar).arg("rcs").arg(&archive).arg(&object).output().expect("running ar");
    assert!(archived.status.success(), "archiving: {}", String::from_utf8_lossy(&archived.stderr));
}

/// **A C library that calls back into the program by name.**
///
/// The program's `main` calls `twice_through_c`, which is C; the C function
/// calls `price`, which is the program's `pub extern fn`. No Khora function
/// calls `price`, so the only route to it is the symbol table: a build that
/// emits only what `main` reaches through Khora calls links with `price`
/// undefined. That is the shape of every C callback API.
#[test]
fn a_c_library_can_call_back_into_a_program_by_name() {
    let (tmp, project) = native_package(
        "module app::main;\n\n\
         import std::core::{print};\n\n\
         extern fn twice_through_c(n: Int) -> Int;\n\n\
         /// Doubles `n`. The C library calls this, and nothing in Khora does.\n\
         pub extern fn price(n: Int) -> Int {\n  n * 2\n}\n\n\
         pub fn main() -> Int {\n  print(\"${twice_through_c(21)}\");\n  0\n}\n",
        Some("callback"),
    );
    c_archive(
        tmp.path(),
        "callback",
        "long price(long n);\nlong twice_through_c(long n) { return price(n); }\n",
    );
    let built = Command::new(compiler())
        .args(["build", ".", "--no-cache"])
        .current_dir(&project)
        .env("KHORA_HOME", tmp.path().join("home"))
        .env("KHORA_STD", tree_std())
        .output()
        .expect("could not run `khora`");
    assert!(
        built.status.success(),
        "a program whose export only C calls must link:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let exe = project.join("build").join(format!("app{}", std::env::consts::EXE_SUFFIX));
    let ran = Command::new(&exe).output().expect("running the program");
    assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), "42", "C reached `price` by name");
}

/// Rewrites the project's manifest with `extra` after the pin.
fn manifest(w: &World, pin: &str, extra: &str) {
    let text = format!(
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[toolchain]\nversion = \"{pin}\"\n{extra}"
    );
    std::fs::write(w.project.join("khora.toml"), text).expect("a manifest");
}

/// **`[build] link` names are in the key, though no archive is found.**
///
/// `m` is the C maths library, which the linker finds in a system directory,
/// so nothing under `link-search` can be hashed for it. Adding and removing
/// the line must each rebuild; a key holding only the archives found reused
/// the runner across both. With a library whose symbols the program uses
/// (zlib), that reuse is a runner that only links while the stale entry lasts.
///
/// Unix only: Windows has no `m` library to link.
#[test]
#[cfg(unix)]
fn a_build_link_name_is_in_the_key() {
    let w = world(&package(41, 41));
    warmed(&w);

    manifest(&w, khora_toolchain::RUNNING, "\n[build]\nlink = [\"m\"]\n");
    let (ok, with) = test(&w, &[]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&with), "linking another library is another build: {}", w.story.borrow());

    manifest(&w, khora_toolchain::RUNNING, "");
    let (ok, without) = test(&w, &[]);
    assert!(ok, "{}", w.story.borrow());
    assert!(reused(&without), "and the first entry is the one served after it: {}", w.story.borrow());
    // The earlier entry, not the one that links `m`: what proves that is the
    // middle run having missed, which is asserted above.
}

/// `[build] link-search` is in the key, though it finds nothing.
#[test]
fn a_build_link_search_directory_is_in_the_key() {
    let w = world(&package(41, 41));
    std::fs::create_dir_all(w.project.join("vendor")).expect("a search directory");
    warmed(&w);

    manifest(&w, khora_toolchain::RUNNING, "\n[build]\nlink-search = [\"vendor\"]\n");
    let (ok, output) = test(&w, &[]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&output), "another search path is another link: {}", w.story.borrow());
}

/// `khora build` is keyed on `[build] link` the same way, through the same
/// `cache::Inputs`. Unix only, like the test above.
#[test]
#[cfg(unix)]
fn a_build_link_name_is_in_the_programs_key_too() {
    let w = world(&package(41, 41));
    let (ok, first) = run_with(&w, &compiler(), &[], &["build", "."]);
    assert!(ok && !reused(&first), "{}", w.story.borrow());

    manifest(&w, khora_toolchain::RUNNING, "\n[build]\nlink = [\"m\"]\n");
    let (ok, second) = run_with(&w, &compiler(), &[], &["build", "."]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&second), "the program links another library: {}", w.story.borrow());
}

/// `khora test --no-cache` builds the runner even when the cache has it, and
/// still files what it built.
#[test]
fn no_cache_rebuilds_the_test_runner() {
    let w = world(&package(41, 41));
    warmed(&w);

    let (ok, output) = run_with(&w, &compiler(), &[], &["test", ".", "--no-cache"]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&output), "--no-cache must build: {}", w.story.borrow());
    assert!(output.contains("1 passed"), "{output}");

    let (ok, bench) = run_with(&w, &compiler(), &[], &["bench", ".", "--no-cache"]);
    assert!(ok && !reused(&bench), "and so must `bench --no-cache`: {}", w.story.borrow());
}

/// A dependency's source is part of the program, and so of the key.
#[test]
fn an_edited_dependency_is_rebuilt() {
    let w = world(
        "module app::main;\n\nimport std::core::{assert_that};\nimport dep::lib::{seed};\n\n\
         test \"the seed\" {\n  assert_that(seed() == 7, \"the seed is 7\");\n}\n",
    );
    let dep = w.home.parent().expect("the scratch directory").join("dep");
    std::fs::create_dir_all(dep.join("src")).expect("a dependency");
    std::fs::write(
        dep.join("khora.toml"),
        format!(
            "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n\n[toolchain]\nversion = \"{}\"\n",
            khora_toolchain::RUNNING
        ),
    )
    .expect("the dependency's manifest");
    let seed = |n: i64| format!("module dep::lib;\n\n/// The seed.\npub fn seed() -> Int {{ {n} }}\n");
    std::fs::write(dep.join("src").join("lib.kh"), seed(7)).expect("the dependency's source");
    manifest(&w, khora_toolchain::RUNNING, "\n[dependencies]\ndep = { path = \"../dep\" }\n");
    warmed(&w);

    std::fs::write(dep.join("src").join("lib.kh"), seed(8)).expect("an edit to the dependency");
    let (ok, output) = test(&w, &[]);
    assert!(!reused(&output), "an edited dependency must rebuild: {}", w.story.borrow());
    assert!(!ok && output.contains("1 failed"), "and the edit is what runs: {}", w.story.borrow());
}

/// `[permissions]` is compiled into the program, so a change to it rebuilds,
/// and the rebuilt runner is the one that enforces it.
#[test]
fn a_permissions_change_is_rebuilt() {
    let w = world(
        "module app::main;\n\nimport std::core::{assert_that, print};\nimport std::env::{Env, EnvError};\n\n\
         fn read_home() -> Bool raises EnvError {\n\
         \x20 let got = with { env: Env::real() } { env.variable(\"HOME\")! };\n\
         \x20 got.is_some()\n}\n\n\
         test \"env read\" {\n\
         \x20 let ok = read_home()! catch { _ => false };\n\
         \x20 print(\"TEST env ${ok}\");\n\
         \x20 assert_that(true, \"ok\");\n}\n",
    );
    let allow = |w: &World, granted: &str| {
        manifest(w, khora_toolchain::RUNNING, &format!("\n[permissions]\nenv = [{granted}]\n"));
    };
    allow(&w, "\"HOME\"");
    let (ok, first) = test(&w, &[("HOME", "/nowhere")]);
    assert!(ok && first.contains("TEST env true"), "{}", w.story.borrow());

    allow(&w, "");
    let (ok, denied) = test(&w, &[("HOME", "/nowhere")]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&denied), "narrower permissions are another build: {}", w.story.borrow());
    assert!(denied.contains("TEST env false"), "and the narrower runner runs: {}", w.story.borrow());
}

/// **A `[toolchain]` pin that names another compiler** builds with that
/// compiler, so its key is that compiler's.
///
/// The other compiler is this one plus a byte, registered under a version of
/// its own: the pin hands the build to it, and the key it computes names its
/// own binary. Pinning back is the first entry again.
#[test]
fn a_toolchain_pin_to_another_compiler_is_rebuilt() {
    let w = world(&package(41, 41));
    warmed(&w);

    let other = w
        .home
        .parent()
        .expect("the scratch directory")
        .join(format!("pinned-khora{}", std::env::consts::EXE_SUFFIX));
    let mut bytes = std::fs::read(compiler()).expect("the compiler");
    bytes.push(0);
    std::fs::write(&other, bytes).expect("a copy of the compiler");
    make_executable(&other);
    let (ok, linked) =
        run_with(&w, &compiler(), &[], &["toolchain", "link", "9.9.8", other.to_str().expect("a path")]);
    assert!(ok, "registering the other compiler: {linked}");

    manifest(&w, "9.9.8", "");
    let (ok, output) = test(&w, &[]);
    assert!(ok, "{}", w.story.borrow());
    assert!(!reused(&output), "a build by another compiler must not reuse: {}", w.story.borrow());

    manifest(&w, khora_toolchain::RUNNING, "");
    let (ok, back) = test(&w, &[]);
    assert!(ok && reused(&back), "pinned back, the first entry serves: {}", w.story.borrow());
}

/// An `ar` member that links as nothing: a header naming a two-byte text file.
#[cfg(not(target_os = "macos"))]
fn padding_member() -> Vec<u8> {
    let mut header = format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", "khora-pad/", 0, 0, 0, 644, 2);
    assert_eq!(header.len(), 60, "an ar member header is sixty bytes");
    header.push_str("x\n");
    header.into_bytes()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a directory");
    for entry in std::fs::read_dir(from).expect("a readable directory").flatten() {
        let path = entry.path();
        let into = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &into);
        } else {
            std::fs::copy(&path, &into).expect("a copy");
        }
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
