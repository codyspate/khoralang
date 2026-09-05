#![cfg(feature = "llvm")]

//! Trace context, compiled and run.
//!
//! The part of `std/trace.kh` worth testing hard is the wire format, because
//! it is the only part another system reads. A `traceparent` that round-trips
//! differently from how it arrived is two services disagreeing about which
//! trace they are in, and that failure is invisible until somebody looks at a
//! dashboard and finds half a request.

use crate::harness;

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
    run_with(name, "", body)
}

/// [`run`], with extra items above `main`.
///
/// A test that needs a recording tracer needs a function to build one, and a
/// handler is not an expression a `main` body can be squeezed around.
fn run_with(name: &str, items: &str, body: &str) -> String {
    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, Fiber, List, Option, Result, Show, attempt, print}};
import std::trace::{{Context, Span, Status, Tracer, around, around_result, number, text}};

fn shown(value: Option<Context>) -> String {{
  match value {{
    Option::Some(context) => context.show(),
    Option::None => "None",
  }}
}}

{items}

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

// --- a span that is left open reads as still running ------------------------

/// A tracer that says what it was told, as it is told.
///
/// **The printed order is the record**, for the same reason `tests/db.rs`
/// gives: the thing being asserted is that `finish` happened at all, and it is
/// visible in the transcript rather than decoded from a count.
const RECORDING: &str = r#"extern fn khora_cancel();

pub type Oops = | Bad;

/// A fallible call, so that `!` marks a cancellation point.
fn mark() -> Int raises Oops { 1 }

/// `Status` has no `Show`, and giving it one is a decision about `std` rather
/// than about this test.
fn said(status: Status) -> String {
  match status {
    Status::Ok => "ok",
    Status::Failed(why) => "failed: " + why,
  }
}

/// A body that raises. Named, so its return type pins `around`'s `A` — a
/// `raise` on its own determines nothing.
fn boom(tracer: Tracer) -> Int raises Oops {
  around(tracer, "work", fn () => { raise Oops::Bad })!
}

/// A body that stops itself in the middle. On a fiber, so the cancellation is
/// absorbed by the fiber's root instead of ending the program with 130.
fn stopped(tracer: Tracer) -> () raises Oops {
  around(tracer, "work", fn () => {
    khora_cancel();
    mark()!;
    print("the body carried on, which is wrong");
    0
  })!;
  print("the span returned, which is wrong");
}

fn recording() -> Tracer {
  handler for Tracer {
    start: fn (name, _attributes) => {
      print("start " + name);
      {
        context: { trace_high: 0, trace_low: 0, span: 1, sampled: true },
        parent: 0,
        name: name,
      }
    },
    finish: fn (span, status) => print("finish " + span.name + " " + said(status)),
    event: fn (_span, _name, _attributes) => (),
  }
}
"#;

/// The ordinary path is unchanged: the span is finished once, as `Ok`.
#[test]
fn a_body_that_returns_finishes_its_span_once() {
    let out = run_with(
        "trace_around_ok",
        RECORDING,
        r#"  let tracer = recording();
  print(Int::to_string(around(tracer, "work", fn () => 7)));"#,
    );
    assert_eq!(out, "start work\nfinish work ok\n7\n");
}

/// **The case this exists for.** A body that raises still finishes its span.
///
/// It used to be `start`, `body()`, `finish` in a row, which closes the span
/// exactly when nothing goes wrong — and a trace with a span that was never
/// closed is read as one that is *still running*, which is the most misleading
/// thing a dashboard can be told about a request that failed.
#[test]
fn a_body_that_raises_still_finishes_its_span() {
    let out = run_with(
        "trace_around_raise",
        RECORDING,
        r#"  let tracer = recording();
  match attempt(fn () => boom(tracer)!) {
    Result::Ok(_) => print("returned, which is wrong"),
    Result::Err(_) => print("raised"),
  }"#,
    );
    assert_eq!(
        out,
        "start work\nfinish work failed: did not complete\nraised\n",
        "a span must close on the way out, however it leaves"
    );
}

/// And a body whose fiber is cancelled, which is the way out that no `match`
/// written in `around` could ever see.
#[test]
fn a_cancelled_body_still_finishes_its_span() {
    let out = run_with(
        "trace_around_cancel",
        RECORDING,
        r#"  let tracer = recording();
  let f = Fiber::spawn(fn () => stopped(tracer)!);
  // `wait`, not `join`: this needs the ordering and not the answer, and a
  // cancelled fiber has no answer to give -- a join would have nothing to
  // hand back and would unwind this frame along with it.
  Fiber::wait(f);
  print("the parent carried on");"#,
    );
    // Nothing after the span's close runs *in the fiber* — the cancellation
    // carries on past it to the fiber's root, which absorbs it. The parent is
    // untouched, which is what makes cancellation per-fiber rather than a way
    // to stop a program.
    assert_eq!(
        out,
        "start work\nfinish work failed: did not complete\nthe parent carried on\n",
        "the span closes, the fiber stops, and the parent is untouched"
    );
}

/// `around_result` reads the error and puts its text on the span, which is the
/// whole difference between the two.
#[test]
fn around_result_reports_what_went_wrong() {
    let out = run_with(
        "trace_around_result",
        RECORDING,
        r#"  let tracer = recording();
  let answer: Result<Int, String> =
    around_result(tracer, "work", fn () => Result::Err("no room"));
  match answer {
    Result::Ok(_) => print("succeeded, which is wrong"),
    Result::Err(why) => print(why),
  }"#,
    );
    assert_eq!(out, "start work\nfinish work failed: no room\nno room\n");
}
