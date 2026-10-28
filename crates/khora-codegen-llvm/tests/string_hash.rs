#![cfg(feature = "llvm")]

//! `Map`'s string hash is keyed per process.
//!
//! **What this prevents: a client choosing keys that all land in one bucket.**
//! With an unkeyed hash, anybody can compute ahead of time a set of strings
//! that collide, and a `Map` built from them -- a JSON object's members, a
//! query string, a header map -- costs time in the square of its size. The key
//! is the process's own secret, so a set that collides in one run does not
//! collide in the next.
//!
//! The colliding keys below are real collisions of the hash `std` used before
//! it was keyed (FNV-1a over the bytes, 32 bits), built the way an attacker
//! would build them: a birthday search finds two short blocks that take the
//! hash from one state to the same state, and sixteen such pairs, chosen
//! independently, give 2^16 strings with one hash.

use crate::harness;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus the program under test.
fn sources(db: &KhoraDatabase, dir: &std::path::Path, main: &str) -> Vec<SourceFile> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out.push(SourceFile::new(db, dir.join("main.kh"), main.to_string()));
    out
}

fn build(name: &str, main: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }
    exe
}

/// Runs `exe`, and kills it if it is still going after `limit`.
///
/// `None` for a program that had to be killed: a quadratic map does finish,
/// eventually, and the test is about whether it finishes in reasonable time.
fn run_within(exe: &std::path::Path, limit: Duration) -> Option<String> {
    let mut child = Command::new(exe)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the program should start");
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("waiting on the program") {
            let mut out = String::new();
            use std::io::Read;
            child.stdout.take().expect("piped").read_to_string(&mut out).expect("its output");
            assert!(status.success(), "the program exited with {:?}: {out}", status.code());
            return Some(out.replace("\r\n", "\n"));
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One step of the unkeyed hash: FNV-1a over `bytes`, from `state`.
fn fnv1a(state: u32, bytes: &[u8]) -> u32 {
    bytes.iter().fold(state, |h, b| (h ^ u32::from(*b)).wrapping_mul(16_777_619))
}

/// Sixteen pairs of five-letter blocks. Each pair takes the unkeyed hash from
/// the state the pairs before it left to one common state, so any choice of
/// one block from every pair gives the same final hash.
fn colliding_blocks() -> Vec<(String, String)> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let block = |mut n: u64| -> Vec<u8> {
        (0..5)
            .map(|_| {
                let c = ALPHABET[(n % 36) as usize];
                n /= 36;
                c
            })
            .collect()
    };
    let mut state = 2_166_136_261u32;
    let mut pairs = Vec::new();
    for _ in 0..16 {
        let mut seen: HashMap<u32, Vec<u8>> = HashMap::new();
        let mut n = 0u64;
        let (first, second) = loop {
            let candidate = block(n);
            n += 1;
            let h = fnv1a(state, &candidate);
            match seen.get(&h) {
                Some(earlier) => break (earlier.clone(), candidate),
                None => {
                    seen.insert(h, candidate);
                }
            }
        };
        state = fnv1a(state, &first);
        pairs.push((
            String::from_utf8(first).expect("ascii"),
            String::from_utf8(second).expect("ascii"),
        ));
    }
    pairs
}

/// A Khora function answering the `i`th colliding key, for `i` below 2^16.
fn key_function(pairs: &[(String, String)]) -> String {
    let mut out = String::from("fn colliding(i: Int) -> String {\n  let mut key = \"\";\n");
    for (bit, (zero, one)) in pairs.iter().enumerate() {
        out.push_str(&format!(
            "  key = key + (if (i / {}) % 2 == 0 {{ \"{zero}\" }} else {{ \"{one}\" }});\n",
            1u64 << bit
        ));
    }
    out.push_str("  key\n}\n");
    out
}

/// The generator above is right: every key it builds has one unkeyed hash.
/// Checked here in Rust, so that a red test below cannot be a broken fixture.
#[test]
fn the_fixture_keys_collide_under_the_unkeyed_hash() {
    let pairs = colliding_blocks();
    let hashes: std::collections::HashSet<u32> = (0..1000u32)
        .map(|i| {
            let key: String = pairs
                .iter()
                .enumerate()
                .map(|(bit, (zero, one))| if (i >> bit) & 1 == 0 { zero.as_str() } else { one.as_str() })
                .collect();
            fnv1a(2_166_136_261, key.as_bytes())
        })
        .collect();
    assert_eq!(hashes.len(), 1, "the fixture's keys should share one unkeyed hash");
}

/// Two runs of one program hash one string differently.
///
/// The seed is the process's, read once from the operating system, so this is
/// the property an attacker runs into: a set of keys worked out against one
/// run says nothing about the next. Two 64-bit draws agree by chance about
/// once in 2^64 runs.
#[test]
fn two_runs_hash_a_string_differently() {
    let exe = build(
        "string_hash_two_runs",
        "module main;
import std::core::{Hash, print};

pub fn main() -> () {
  let key = \"the same key\";
  print(\"${Hash::hash(key)}\");
  print(\"${Hash::hash(key)}\");
}
",
    );
    let first = run_within(&exe, Duration::from_secs(60)).expect("the program should finish");
    let second = run_within(&exe, Duration::from_secs(60)).expect("the program should finish");
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 2, "{first}");
    // Within one process the hash is a function, or `Map` could not find a key.
    assert_eq!(lines[0], lines[1], "one process hashed one string two ways");
    assert_ne!(first, second, "two runs hashed one string the same way");
}

/// Keys built to collide under the unkeyed hash spread out under the keyed one.
///
/// A count of distinct hashes rather than a timing, so it cannot be flaky:
/// unkeyed, all 50,000 share one hash, and keyed, a repeat is a 64-bit
/// coincidence.
#[test]
fn keys_built_to_collide_have_distinct_hashes() {
    let exe = build(
        "string_hash_distinct",
        &format!(
            "module main;
import std::core::{{Hash, Map, print}};

{}
pub fn main() -> () {{
  let seen: Map<Int, Bool> = Map::new();
  let mut i = 0;
  while i < 50000 {{
    Map::insert(seen, Hash::hash(colliding(i)), true);
    i = i + 1;
  }};
  print(\"${{Map::len(seen)}}\");
}}
",
            key_function(&colliding_blocks())
        ),
    );
    let out = run_within(&exe, Duration::from_secs(120)).expect("the program should finish");
    let distinct: i64 = out.trim().parse().expect("a count");
    assert!(distinct > 49_990, "50,000 keys built to collide had only {distinct} distinct hashes");
}

/// The attack itself: 50,000 colliding keys into one `Map`, in reasonable time.
///
/// Unkeyed, every insert walks a chain as long as the map, which is about 1.25
/// billion string comparisons. Keyed, it is an ordinary map of 50,000 entries,
/// which takes well under a second in a debug build; the limit is far above
/// that so a slow machine does not fail it, and far below the unkeyed time.
#[test]
fn fifty_thousand_colliding_keys_insert_in_reasonable_time() {
    let exe = build(
        "string_hash_flood",
        &format!(
            "module main;
import std::core::{{Map, Option, print}};

{}
pub fn main() -> () {{
  let flooded: Map<String, Int> = Map::new();
  let mut i = 0;
  while i < 50000 {{
    Map::insert(flooded, colliding(i), i);
    i = i + 1;
  }};
  let last = Option::unwrap_or(Map::get(flooded, colliding(49999)), 0 - 1);
  print(\"${{Map::len(flooded)}} ${{last}}\");
}}
",
            key_function(&colliding_blocks())
        ),
    );
    let out = run_within(&exe, Duration::from_secs(20))
        .expect("50,000 colliding keys took more than 20 seconds to insert");
    assert_eq!(out, "50000 49999\n");
}
