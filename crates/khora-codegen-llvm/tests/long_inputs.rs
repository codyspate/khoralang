#![cfg(feature = "llvm")]
#![cfg(unix)]

//! How long an input `std` walks before the stack runs out.
//!
//! **What this prevents: a program that dies of `SIGSEGV` because its input
//! was long.** Khora does not promise tail calls, so a function that takes the
//! next byte, line, chunk or entry by calling itself keeps a frame per item,
//! and the input decides how many. Each case below was such a function: a
//! file of 1,920 chunks, a directory of 1,528 names, a JSON array of 1,562
//! elements or a request path of 2,601 `/` ended the process under a 256 KB
//! stack, and under the default 8 MB the same shapes died at between 37,000
//! and 130,000 -- a 4 GB file, a directory of 52,000 files, a 1 MB request
//! body to a router that `holding` let accept one.
//!
//! Every program runs under a 256 KB stack limit, set with `setrlimit` in the
//! child before it starts, so a frame per item runs out within a few thousand
//! rather than after a hundred thousand, and each case asks for 20,000:
//! between five and eighteen times what the recursive version survived
//! (arguments are the exception, and say why).
//! Unix only, for the `setrlimit`. Both fiber backends, because the limit is
//! on the main thread and the program runs there on both.
//!
//! The exporter in `packages/otlp` runs on a fiber, whose stack is fixed at
//! 8 MB whatever the limit says, so its case drives it past the 20,068 spans
//! that 8 MB held.

use crate::harness;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// A stack small enough that a frame per item shows within a few thousand
/// items, and large enough for everything else these programs do.
const STACK: u64 = 256 * 1024;

/// How many items each case asks for: at least five times what the
/// recursive version survived under [`STACK`].
const ITEMS: usize = 20_000;

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// One program, one case per function, so it is compiled once.
///
/// Each case prints what it computed, so a rewrite that stopped early or
/// counted wrongly fails on the answer rather than passing because it did not
/// crash.
const PROGRAM: &str = r#"module demo::main;

import std::core::{List, Map, Option, Result, String, print};
import std::env::{Env};
import std::fs::{FsRead, IoError, extension, file_name, fold_chunks, fold_lines};
import std::json::{Json, JsonError, encode, parse, quote};
import std::net::http::{matches};
import std::permissions::{granted, granted_host};

fn parsed(text: String) -> String {
  match parse(text) {
    Result::Ok(value) => encode(value),
    Result::Err(why) => "error at ${Int::to_string(why.at)}: ${why.expected}",
  }
}

fn length(text: String) -> String { Int::to_string(String::byte_length(text)) }

fn nulls(n: Int) -> Json {
  let mut out = List::Nil;
  let mut i = 0;
  while i < n { out = List::Cons(Json::Null, out); i = i + 1; };
  Json::Array(out)
}

fn fields(n: Int) -> Json {
  let m: Map<String, Json> = Map::new();
  let mut i = 0;
  while i < n { Map::insert(m, "k${Int::to_string(i)}", Json::Null); i = i + 1; };
  Json::Object(m)
}

fn members(n: Int) -> String {
  let mut pieces = List::Nil;
  let mut i = 0;
  while i < n {
    pieces = List::Cons("\"k${Int::to_string(i)}\":1", pieces);
    i = i + 1;
  };
  "{" + String::join(pieces, ",") + "}"
}

fn yes(flag: Bool) -> String { if flag { "yes" } else { "no" } }

fn found<A>(value: Option<A>) -> String {
  match value { Option::Some(_v) => "some", Option::None => "none" }
}

/// Keys whose hashes agree in every bit the table uses, so they share one
/// bucket and every operation walks one chain as long as the map.
fn colliding(n: Int) -> String {
  let m: Map<Int, Int> = Map::new();
  let mut i = 0;
  while i < n { Map::insert(m, Int::shl(i, 48), i); i = i + 1; };
  // Replacing walks the chain to drop the old entry.
  Map::insert(m, 0, -1);
  let last = Option::unwrap_or(Map::get(m, Int::shl(n - 1, 48)), -2);
  Map::remove(m, 0);
  "${Int::to_string(Map::len(m))} ${Int::to_string(last)}"
}

fn run(case: String, n: Int, path: String) -> String
  with { env: Env, reads: FsRead }
  raises IoError
{
  match case {
    "chars" => Int::to_string(List::length(String::chars(String::repeat("é", n)))),
    "char_length" => Int::to_string(String::char_length(String::repeat("é", n))),
    "float" => found(Float::of_string(String::repeat("1", n))),
    "json_space" => parsed(String::repeat(" ", n) + "1"),
    "json_digits" => length(parsed(String::repeat("1", n))),
    "json_items" => length(parsed("[" + String::repeat("1,", n) + "1]")),
    "json_members" => length(parsed(members(n))),
    "quote" => length(quote(String::repeat("\n", n))),
    "encode_items" => length(encode(nulls(n))),
    "encode_fields" => length(encode(fields(n))),
    "file_name" => "${length(file_name(String::repeat("a", n)))} ${found(extension(String::repeat("a", n)))}",
    "granted" => "${yes(granted(List::Cons("**", List::Nil), String::repeat("a", n)))} ${yes(granted(List::Cons("*a", List::Nil), String::repeat("a", n)))}",
    "granted_host" => yes(granted_host(List::Cons("*", List::Nil), String::repeat("a", n))),
    "route" => "${found(matches("/", String::repeat("/", n)))} ${found(matches(String::repeat("/a", n), String::repeat("/a", n)))}",
    "arguments" => Int::to_string(List::length(env.arguments())),
    "chunks" => Int::to_string(fold_chunks(path, 0, fn (k, _chunk) => k + 1)!),
    "lines" => Int::to_string(fold_lines(path, 0, fn (k, _line) => k + 1)!),
    "read_dir" => Int::to_string(List::length(reads.read_dir(path)!)),
    "colliding" => colliding(n),
    _ => "no case ${case}",
  }
}

pub fn main() -> () raises IoError {
  with { env: Env::real(), reads: FsRead::real() } {
    let args = env.arguments();
    let case = Option::unwrap_or(List::nth(args, 1), "");
    let n = Option::unwrap_or(Int::of_string(Option::unwrap_or(List::nth(args, 2), "0")), 0);
    let path = Option::unwrap_or(List::nth(args, 3), "");
    print(run(case, n, path)!);
  }
}
"#;

/// The exporter with a client that answers every post, sending `n` spans.
///
/// Each batch of 64 is waited for before the next is started, so the
/// `dropping` queue never fills and every report reaches the exporter's
/// fiber: a test that let the queue drop reports would count fewer
/// iterations than it asked for.
const EXPORTER: &str = r#"module demo::main;

import std::core::{Channel, ChildFailed, List, Map, Option, Result, nursery, print};
import std::clock::{Clock};
import std::env::{Env};
import std::random::{Random};
import std::net::http::{Answer, HttpClient};
import std::trace::{Status, Tracer};
import otlp::exporter::{Exporter};

fn spans(n: Int, posted: Channel<Int>) -> Int with { tracer: Tracer } {
  let mut i = 0;
  let mut batches = 0;
  while i < n {
    let span = tracer.start("s", List::Nil);
    tracer.finish(span, Status::Ok);
    i = i + 1;
    if i % 64 == 0 {
      let _ = Channel::receive(posted);
      batches = batches + 1;
    };
  };
  batches
}

pub fn main() -> () raises ChildFailed {
  with { env: Env::real() } {
    let n = Option::unwrap_or(Int::of_string(Option::unwrap_or(List::nth(env.arguments(), 1), "0")), 0);
    let posted: Channel<Int> = Channel::bounded(4);
    let counting = handler for HttpClient {
      send: fn _call => {
        let _ = Channel::send(posted, 1);
        Result::Ok({ status: 200, headers: Map::new(), body: "" })
      },
    };
    with { client: counting } {
      let batches = nursery(fn () =>
        Exporter::running("demo", "http://collector", Clock::real(), Random::real(), fn tracer =>
          with { tracer: tracer } { spans(n, posted) }))!;
      print("${Int::to_string(batches)} batches");
    }
  }
}
"#;

/// Every `.kh` file under `roots`, except a package's `test`-block files,
/// which a `main` build has no entry point for, plus `main`.
fn sources(db: &KhoraDatabase, roots: &[PathBuf], dir: &Path, main: &str) -> Vec<SourceFile> {
    let mut out = Vec::new();
    let mut stack = roots.to_vec();
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
                && !path.to_string_lossy().ends_with("_test.kh")
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out.push(SourceFile::new(db, dir.join("main.kh"), main.to_string()));
    out
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn build(name: &str, roots: &[PathBuf], main: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("program");
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, roots, &dir, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("`{name}` did not build:\n  {}", messages.join("\n  "));
    }
    exe
}

/// One copy of [`PROGRAM`] per test, built where only that test looks.
///
/// **Not one build shared between tests.** nextest runs each test in a
/// process of its own, so a build shared through a `OnceLock` was redone by
/// every one of them into the same path, and a test that ran the program
/// while another replaced it got `Text file busy`, or no file at all.
fn program(test: &str) -> PathBuf {
    build(&format!("long_inputs_{test}"), &[repository().join("std")], PROGRAM)
}

/// A scratch directory of this test's own.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("long_inputs_fixtures")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// Runs `exe` with `args` under [`STACK`] on `backend`, and answers its
/// stdout, failing with the status and stderr if it did not exit cleanly.
fn run_limited(exe: &Path, backend: &str, args: &[String]) -> String {
    let mut command = Command::new(exe);
    // Cleared, because Linux gives the arguments and the environment together
    // a quarter of the stack limit, 64 KB here, and every byte of this
    // process's environment is one an argument cannot have.
    command
        .args(args)
        .env_clear()
        .env("KHORA_FIBERS", backend);
    // SAFETY: runs in the forked child before `exec`, and calls only
    // `setrlimit`, which is async-signal-safe and touches no memory the parent
    // shares; the limit is a stack value that outlives the call.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: STACK,
                rlim_max: STACK,
            };
            if libc::setrlimit(libc::RLIMIT_STACK, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().expect("the program should start");
    let stdout = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string();
    assert!(
        output.status.success(),
        "`{} {}` on `{backend}` ended with {:?}; it said: {}",
        exe.display(),
        args.first().map(String::as_str).unwrap_or(""),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// Runs one case of [`PROGRAM`] on both backends and checks its answer.
fn case(exe: &Path, name: &str, items: usize, path: &str, expected: &str) {
    for backend in BACKENDS {
        let args = vec![name.to_string(), items.to_string(), path.to_string()];
        let got = run_limited(exe, backend, &args);
        assert_eq!(got, expected, "case `{name}` on `{backend}`");
    }
}

/// `fold_chunks` over a file of many chunks, `fold_lines` over one chunk of
/// many lines, and `read_dir` over a directory of many entries.
///
/// The chunk file is sparse: 20,000 chunks of 64 KiB is 1.3 GB on paper and
/// nothing on disk. The lines are blank, so one chunk holds all of them and
/// the split inside a chunk is what is driven, not the fold across chunks.
#[test]
fn a_file_or_directory_of_any_size_is_read() {
    let exe = program("files");
    let dir = scratch("chunks");
    let path = dir.join("sparse");
    let file = std::fs::File::create(&path).expect("a file");
    file.set_len(ITEMS as u64 * 65536).expect("a sparse length");
    case(&exe, "chunks", 0, &path.to_string_lossy(), &ITEMS.to_string());

    let dir = scratch("lines");
    let path = dir.join("blank");
    std::fs::write(&path, "\n".repeat(ITEMS)).expect("a file");
    case(&exe, "lines", 0, &path.to_string_lossy(), &ITEMS.to_string());

    let dir = scratch("entries");
    for i in 0..ITEMS {
        std::fs::File::create(dir.join(format!("f{i}"))).expect("an entry");
    }
    case(&exe, "read_dir", 0, &dir.to_string_lossy(), &ITEMS.to_string());
}

/// `Env::arguments`, and the path functions of `std::fs` and
/// `std::permissions`, which see whatever a program was handed.
///
/// **10,000 arguments, not [`ITEMS`]**: under a 256 KB stack Linux refuses to
/// start a program with more than about 13,000 of them, because the arguments
/// get a quarter of the stack. The recursive version died at 3,400.
#[test]
fn any_number_of_arguments_and_a_path_of_any_length_are_read() {
    const ARGUMENTS: usize = 10_000;
    let exe = program("arguments");
    for backend in BACKENDS {
        let mut args = vec!["arguments".to_string(), "0".to_string(), String::new()];
        args.extend(std::iter::repeat_n("a".to_string(), ARGUMENTS));
        // argv[0] and the three above.
        assert_eq!(
            run_limited(&exe, backend, &args),
            (ARGUMENTS + 4).to_string(),
            "case `arguments` on `{backend}`"
        );
    }
    case(&exe, "file_name", ITEMS, "", &format!("{ITEMS} none"));
    case(&exe, "granted", ITEMS, "", "yes yes");
    case(&exe, "granted_host", ITEMS, "", "yes");
}

/// `String::chars`, `String::char_length`, `Float::of_string`, and a `Map`
/// whose keys all land in one bucket -- which is what a hash that ignores high
/// bits makes of keys that differ only there.
#[test]
fn core_takes_a_string_of_any_length_and_a_bucket_of_any_depth() {
    let exe = program("core");
    case(&exe, "chars", ITEMS, "", &ITEMS.to_string());
    case(&exe, "char_length", ITEMS, "", &ITEMS.to_string());
    case(&exe, "float", ITEMS, "", "some");
    case(
        &exe,
        "colliding",
        ITEMS,
        "",
        &format!("{} {}", ITEMS - 1, ITEMS - 1),
    );
}

#[test]
fn json_reads_and_writes_a_document_of_any_length() {
    let exe = program("json");
    case(&exe, "json_space", ITEMS, "", "1");
    case(&exe, "json_digits", ITEMS, "", &ITEMS.to_string());
    // `[1,1,...,1]`: ITEMS + 1 ones and ITEMS commas, and the brackets.
    case(&exe, "json_items", ITEMS, "", &(2 * ITEMS + 3).to_string());
    let members: usize = (0..ITEMS)
        .map(|i| format!("\"k{i}\":1").len())
        .sum::<usize>()
        + ITEMS
        - 1
        + 2;
    case(&exe, "json_members", ITEMS, "", &members.to_string());
    // `\n` is two bytes escaped, and the quotes.
    case(&exe, "quote", ITEMS, "", &(2 * ITEMS + 2).to_string());
    // `null` and a comma each, less the last comma, and the brackets.
    case(&exe, "encode_items", ITEMS, "", &(5 * ITEMS + 1).to_string());
    let fields: usize = (0..ITEMS)
        .map(|i| format!("\"k{i}\":null").len())
        .sum::<usize>()
        + ITEMS
        - 1
        + 2;
    case(&exe, "encode_fields", ITEMS, "", &fields.to_string());
}

/// `/` alone has an empty first segment at every step, and `/a/a/...` a
/// matching one: the two branches that took the next segment by recursing.
#[test]
fn a_route_matches_a_path_of_any_number_of_segments() {
    let exe = program("route");
    case(&exe, "route", ITEMS, "", "some some");
}

/// Five times the 20,068 spans an exporter's fiber held, each one a report
/// to the fiber that owns the batch.
#[test]
fn the_otlp_exporter_takes_any_number_of_spans() {
    const SPANS: usize = 100_000;
    let exe = build(
        "long_inputs_exporter",
        &[
            repository().join("std"),
            repository().join("packages").join("otlp").join("src"),
        ],
        EXPORTER,
    );
    for backend in BACKENDS {
        let got = run_limited(&exe, backend, &[SPANS.to_string()]);
        assert_eq!(got, format!("{} batches", SPANS / 64), "on `{backend}`");
    }
}
