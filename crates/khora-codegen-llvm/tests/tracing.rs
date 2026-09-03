#![cfg(feature = "llvm")]

//! `std::trace`: a span knows what it is inside.
//!
//! **Every trace used to be one span deep.** `Tracer::start` builds a `Span`
//! from the name it is given and nothing else — an effect operation runs where
//! the effect is *performed*, so it cannot see the `around` above it on the
//! stack — and so a nested `around` began a second trace rather than a child
//! span. `Span::parent` was written `0` at every call site in the repository,
//! and the OTLP exporter carried a comment saying it started a fresh trace per
//! span. A collector showed one request as a dozen unrelated one-span traces.
//!
//! The module's own header had been claiming the opposite since it was
//! written: that propagation — *a span's parent surviving a spawn, a steal, a
//! wake and a cancellation* — was the half of tracing that belongs in `std`
//! because it is a property of the scheduler rather than of a library.
//!
//! So these tests assert relationships between spans, never one span in
//! isolation. A test that checked a single span's fields would have passed
//! against the version with no parents at all.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_sources(db: &KhoraDatabase) -> Vec<SourceFile> {
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
    out
}

fn run(name: &str, main: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let mut files = std_sources(&db);
    files.push(SourceFile::new(&db, dir.join("main.kh"), main.to_string()));
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let output = Command::new(&exe).output().expect("the program should run");
    assert!(
        output.status.success(),
        "the program failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

/// A tracer that prints what each span was started inside.
///
/// Ids come from a counter rather than `Random` so that the expected output is
/// a fixed string: a test whose assertion is a regular expression over random
/// hex is a test of the regular expression. A root invents a trace id from its
/// own span id, so two roots are visibly two traces.
const TRACER: &str = r#"
fn recording(counter: Shared<Int>) -> Tracer {
  handler for Tracer {
    start: fn (name, _attributes) => {
      let id = Shared::update(counter, fn n => n + 1);
      match current() {
        Option::Some(inside) => {
          print(name + " span=" + Int::to_string(id)
            + " parent=" + Int::to_string(inside.span)
            + " trace=" + Int::to_string(inside.trace_high));
          {
            context: {
              trace_high: inside.trace_high,
              trace_low: inside.trace_low,
              span: id,
              sampled: inside.sampled,
            },
            parent: inside.span,
            name: name,
          }
        },
        Option::None => {
          print(name + " span=" + Int::to_string(id)
            + " parent=0 trace=" + Int::to_string(900 + id));
          {
            context: { trace_high: 900 + id, trace_low: 0, span: id, sampled: true },
            parent: 0,
            name: name,
          }
        },
      }
    },
    finish: fn (_span, _status) => (),
    event: fn (_span, _name, _attributes) => (),
  }
}
"#;

fn program(body: &str) -> String {
    format!(
        "module demo::main;

import std::core::{{Fiber, List, Option, Shared}};
import std::trace::{{Context, Span, Status, Tracer, around, current}};

fn print(value: String);
{TRACER}
{body}
"
    )
}

/// **The point of the change, in one assertion.** The inner span is inside the
/// outer one, and both are in the same trace.
#[test]
fn a_nested_span_is_a_child_of_the_one_around_it() {
    let out = run(
        "trace_nested",
        &program(
            "pub fn main() -> Int {
  let counter = Shared::of(0);
  let tracer = recording(counter);
  around(tracer, \"outer\", fn () =>
    around(tracer, \"inner\", fn () => ()));
  0
}",
        ),
    );
    assert_eq!(
        out,
        "outer span=1 parent=0 trace=901\n\
         inner span=2 parent=1 trace=901\n",
        "the inner span should name the outer as its parent, in the outer's trace"
    );
}

/// Leaving a span puts back the one that was there, so two spans opened one
/// after the other are siblings rather than a chain.
#[test]
fn leaving_a_span_puts_the_enclosing_one_back() {
    let out = run(
        "trace_siblings",
        &program(
            "pub fn main() -> Int {
  let counter = Shared::of(0);
  let tracer = recording(counter);
  around(tracer, \"outer\", fn () => {
    around(tracer, \"first\", fn () => ());
    around(tracer, \"second\", fn () => ());
  });
  0
}",
        ),
    );
    assert_eq!(
        out,
        "outer span=1 parent=0 trace=901\n\
         first span=2 parent=1 trace=901\n\
         second span=3 parent=1 trace=901\n",
        "`second` is a sibling of `first`, not its child"
    );
}

/// At the top there is no current span, so the first `around` starts a trace.
///
/// Two spans in a row at the top are two traces, which is what makes the
/// restore above observable: if leaving did not clear the slot, the second
/// would be a child of the first.
#[test]
fn two_spans_at_the_top_are_two_traces() {
    let out = run(
        "trace_roots",
        &program(
            "pub fn main() -> Int {
  let counter = Shared::of(0);
  let tracer = recording(counter);
  around(tracer, \"one\", fn () => ());
  around(tracer, \"two\", fn () => ());
  0
}",
        ),
    );
    assert_eq!(
        out,
        "one span=1 parent=0 trace=901\n\
         two span=2 parent=0 trace=902\n",
        "each should begin its own trace"
    );
}

/// **The case the reference-service item is about.** A request that fans out
/// into fibers is one trace, not one per fiber.
///
/// The tracer is passed as a *value* rather than installed as a capability,
/// because a capability does not cross a fiber boundary. What crosses is the
/// span, and it crosses because the runtime copies it into the child at the
/// moment the child is created.
#[test]
fn a_spawned_fiber_stays_in_its_spawners_trace() {
    let out = run(
        "trace_spawn",
        &program(
            "pub fn main() -> Int {
  let counter = Shared::of(0);
  let tracer = recording(counter);
  around(tracer, \"request\", fn () => {
    let child = Fiber::spawn(fn () => around(tracer, \"worker\", fn () => ()));
    Fiber::wait(child);
  });
  0
}",
        ),
    );
    assert_eq!(
        out,
        "request span=1 parent=0 trace=901\n\
         worker span=2 parent=1 trace=901\n",
        "the spawned fiber's span belongs to the request's trace"
    );
}

/// A span left through a failure still puts the enclosing one back.
///
/// Without this the slot would keep pointing at a span that has ended, and
/// every later span in the fiber would be a child of a corpse — which is worse
/// than the bug this replaced, because it looks right.
#[test]
fn a_span_that_failed_still_restores_the_one_around_it() {
    let out = run(
        "trace_failed",
        &program(
            "type Refused = { why: String };

fn doomed(tracer: Tracer) -> () raises Refused {
  around(tracer, \"doomed\", fn () => raise({ why: \"no\" }))!
}

pub fn main() -> Int {
  let counter = Shared::of(0);
  let tracer = recording(counter);
  around(tracer, \"outer\", fn () => {
    doomed(tracer)! catch { _ => () };
    around(tracer, \"after\", fn () => ());
  });
  0
}",
        ),
    );
    assert_eq!(
        out,
        "outer span=1 parent=0 trace=901\n\
         doomed span=2 parent=1 trace=901\n\
         after span=3 parent=1 trace=901\n",
        "`after` should be the outer span's child, not the failed span's"
    );
}
