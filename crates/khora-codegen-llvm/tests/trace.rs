#![cfg(feature = "llvm")]

//! Trace context, compiled and run.
//!
//! The part of `std/trace.kh` worth testing hard is the wire format, because
//! it is the only part another system reads. A `traceparent` that round-trips
//! differently from how it arrived is two services disagreeing about which
//! trace they are in, and that failure is invisible until somebody looks at a
//! dashboard and finds half a request.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn run(name: &str, body: &str) -> String {
    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, List, Option, Show, print}};
import std::trace::{{Context, Span, Status, Tracer, around, number, text}};

fn shown(value: Option<Context>) -> String {{
  match value {{
    Option::Some(context) => context.show(),
    Option::None => "None",
  }}
}}

fn main() -> () {{
{body}
}}
"#
    );

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("trace.kh"), std_source("trace.kh")),
        SourceFile::new(&db, dir.join("main.kh"), main),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// A header is rendered in the shape the specification names, and reading it
/// back gives the same context.
#[test]
fn a_traceparent_round_trips() {
    let out = run(
        "trace_roundtrip",
        r#"  let context = { trace_high: 81985529216486895, trace_low: 81985529216486895, span: 4822678189205111, sampled: true };
  let header = Context::to_traceparent(context);
  print(header);
  print(shown(Context::of_traceparent(header)));
  match Context::of_traceparent(header) {
    Option::Some(back) => print(if back == context { "same" } else { "different" }),
    Option::None => print("unreadable"),
  }"#,
    );
    assert_eq!(
        out,
        "00-0123456789abcdef0123456789abcdef-0011223344556677-01\n\
         00-0123456789abcdef0123456789abcdef-0011223344556677-01\n\
         same\n"
    );
}

/// The sampled flag survives the trip, and it is the low bit of the last byte.
#[test]
fn the_sampled_flag_is_carried() {
    let out = run(
        "trace_sampled",
        r#"  let off = { trace_high: 1, trace_low: 2, span: 3, sampled: false };
  print(Context::to_traceparent(off));
  print(shown(Context::of_traceparent(Context::to_traceparent(off))));"#,
    );
    assert_eq!(
        out,
        "00-00000000000000010000000000000002-0000000000000003-00\n\
         00-00000000000000010000000000000002-0000000000000003-00\n"
    );
}

/// **Anything not fully understood is refused.** A header half-read is a trace
/// joined to the wrong parent, which is worse than starting a fresh one.
#[test]
fn a_malformed_header_is_refused() {
    let out = run(
        "trace_malformed",
        r#"  print(shown(Context::of_traceparent("")));
  print(shown(Context::of_traceparent("00-0123456789abcdef0123456789abcdef-0011223344556677")));
  print(shown(Context::of_traceparent("01-0123456789abcdef0123456789abcdef-0011223344556677-01")));
  print(shown(Context::of_traceparent("00-0123456789ABCDEF0123456789abcdef-0011223344556677-01")));
  print(shown(Context::of_traceparent("00-0123456789abcdef0123456789abcdef_0011223344556677-01")));
  print(shown(Context::of_traceparent("00-zzzz456789abcdef0123456789abcdef-0011223344556677-01")));"#,
    );
    assert_eq!(
        out,
        "None\nNone\nNone\nNone\nNone\nNone\n",
        "short, wrong version, uppercase, wrong separator and non-hex are all refused"
    );
}

/// A zero context is not a trace, which is what lets zero mean "none" without
/// a separate flag.
#[test]
fn an_empty_context_is_not_valid() {
    let out = run(
        "trace_valid",
        r#"  print(Context::is_valid(Context::none()).show());
  print(Context::is_valid({ trace_high: 0, trace_low: 1, span: 0, sampled: false }).show());
  print(Context::is_valid({ trace_high: 0, trace_low: 1, span: 2, sampled: false }).show());"#,
    );
    assert_eq!(out, "false\nfalse\ntrue\n");
}

/// **The no-op tracer costs nothing and still composes.** `around` runs the
/// body and gives back its answer, whatever the tracer does or does not do.
#[test]
fn the_default_tracer_records_nothing_and_stays_out_of_the_way() {
    let out = run(
        "trace_none",
        r#"  let tracer = Tracer::none();
  let answer = around(tracer, "work", fn () => 6 * 7);
  print(Int::to_string(answer));
  let span = tracer.start("outer", List::Cons(text("route", "/health"), List::Nil));
  print(span.name);
  print(Context::is_valid(span.context).show());
  tracer.event(span, "cache.miss", List::Cons(number("size", 3), List::Nil));
  tracer.finish(span, Status::Ok);
  print("done");"#,
    );
    assert_eq!(out, "42\nouter\nfalse\ndone\n");
}
