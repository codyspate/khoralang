#![cfg(feature = "llvm")]

//! Two profiles, and what each one promises.
//!
//! `docs/design/profiles.md` decides what they are. What is checked here is the
//! part a person is entitled to rely on:
//!
//! - **They agree.** A release build answers what a debug build answers, which
//!   is the only reason optimizing is allowed to be a flag rather than a
//!   rewrite.
//! - **Release optimizes.** Otherwise the flag is a rename of `KHORA_DEBUG`.
//! - **Release is reproducible**, without anybody setting a variable — 12.9
//!   measured that `KHORA_DEBUG=0 khora build` is bit-for-bit reproducible on
//!   Windows and that a build with debug information is not, so the profile
//!   that ships has to be the one without.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_codegen_llvm::Profile;
use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// A program with something to optimize and something to say.
///
/// The arithmetic is foldable, the helper is inlinable, and the loop is real
/// work — so an optimizer has somewhere to go, and the answer is still a
/// number a test can compare.
const WORK: &str = "module t;

fn print(value: Int);

fn scale(n: Int) -> Int { n * 3 + 1 }

fn total(upto: Int) -> Int {
  let mut i = 0;
  let mut sum = 0;
  while i < upto {
    sum = sum + scale(i);
    i = i + 1
  };
  sum
}

fn main() -> Int {
  print(total(1000));
  print(scale(7));
  0
}
";

/// Compiles `WORK` under `profile` and returns what it printed.
fn built_and_run(name: &str, profile: Profile) -> (String, Vec<u8>) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), WORK.to_string());
    let root = SourceRoot::new(&db, vec![file]);
    if let Err(errors) = khora_codegen_llvm::compile_with(&db, root, &exe, profile) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    let printed = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");

    let mut object = exe.into_os_string();
    object.push(".o");
    (printed, std::fs::read(PathBuf::from(object)).expect("an object"))
}

/// **The claim that makes a profile a flag rather than a fork.** Optimizing
/// changes how long a program takes and nothing about what it says.
#[test]
fn the_two_profiles_agree_about_the_answer() {
    let (debug, debug_object) = built_and_run("profile_debug", Profile::Debug);
    let (release, release_object) = built_and_run("profile_release", Profile::Release);

    assert_eq!(debug, "1499500\n22\n", "the sum of 3i+1 below 1000, and 3*7+1");
    assert_eq!(release, debug, "a release build must answer what a debug build answers");
    assert_ne!(
        debug_object, release_object,
        "if the objects are identical the pipeline did not run, and `--release` is a rename"
    );
}

/// And release is *smaller*, here, which is the cheapest evidence that the
/// pipeline actually ran rather than being asked to.
///
/// Not a general law — inlining grows code, and a program built around it
/// would go the other way. It holds for this one because the debug object
/// carries every unoptimized allocation and every unfolded constant, and the
/// margin is large enough that the assertion is not measuring noise.
#[test]
fn release_is_the_smaller_object_for_this_program() {
    let (_, debug_object) = built_and_run("profile_size_debug", Profile::Debug);
    let (_, release_object) = built_and_run("profile_size_release", Profile::Release);
    assert!(
        release_object.len() < debug_object.len(),
        "release {} bytes, debug {} bytes",
        release_object.len(),
        debug_object.len()
    );
}

/// **Release is reproducible with nothing set**, which is the whole reason the
/// profile owns the debug-information decision.
///
/// Across processes, like its neighbour in `reproducible.rs`: within one
/// process every `HashSet` shares a seed, so an in-process comparison cannot
/// see the failure this one can. The **executable** is compared as well as the
/// object, because the artifact somebody ships is the executable, and on
/// Windows it is exactly debug information that made relinking unrepeatable.
#[test]
fn a_release_build_is_reproducible() {
    let Some(khora) = std::env::var_os("CARGO_BIN_EXE_khora") else {
        eprintln!("no khora binary for the cross-process check; skipping");
        return;
    };

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("profile_reproducible");
    harness::ensure_runtime();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    std::fs::write(dir.join("main.kh"), WORK).expect("a program");

    let mut artifacts = Vec::new();
    for run in 0..2 {
        let out = dir.join(format!("run{run}.exe"));
        let built = Command::new(&khora)
            .arg("build")
            .arg(dir.join("main.kh"))
            .arg("-o")
            .arg(&out)
            .arg("--release")
            // Deliberately nothing in the environment. The profile is the
            // whole of the instruction.
            .env_remove("KHORA_DEBUG")
            .env_remove("KHORA_PROFILE")
            .output()
            .expect("could not run khora");
        assert!(
            built.status.success(),
            "the build failed:\n{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let mut object = out.clone().into_os_string();
        object.push(".o");
        artifacts.push((
            std::fs::read(PathBuf::from(object)).expect("an object"),
            std::fs::read(&out).expect("an executable"),
        ));
    }
    assert!(artifacts[0].0 == artifacts[1].0, "two release objects differ");
    assert!(
        artifacts[0].1 == artifacts[1].1,
        "two release executables differ, and release is the profile that ships"
    );
}

/// `--release` says it, and the line a person reads says which they got.
///
/// Small, and worth pinning: a build that silently used the wrong profile is
/// indistinguishable from one that used the right one until something is
/// measured or debugged.
#[test]
fn the_build_line_names_the_profile() {
    let Some(khora) = std::env::var_os("CARGO_BIN_EXE_khora") else {
        eprintln!("no khora binary; skipping");
        return;
    };

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("profile_named");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    std::fs::write(dir.join("main.kh"), WORK).expect("a program");

    let say = |args: &[&str], env: Option<(&str, &str)>| -> String {
        let mut cmd = Command::new(&khora);
        cmd.arg("build")
            .arg(dir.join("main.kh"))
            .arg("-o")
            .arg(dir.join("named.exe"))
            .args(args)
            .env_remove("KHORA_DEBUG")
            .env_remove("KHORA_PROFILE");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("could not run khora");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    assert!(say(&[], None).contains("[debug]"), "the default profile is debug");
    assert!(say(&["--release"], None).contains("[release]"), "`--release` selects it");
    assert!(
        say(&[], Some(("KHORA_PROFILE", "release"))).contains("[release]"),
        "and so does the variable, which is how `test` and `bench` say it"
    );
}
