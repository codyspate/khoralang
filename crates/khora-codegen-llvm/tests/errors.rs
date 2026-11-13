#![cfg(feature = "llvm")]

//! `std`'s own error types, printed and compared.
//!
//! Every one of them is something a program raises, catches and then has to
//! say something about — and until this batch, saying it meant a `match` per
//! call site. Logging why an HTTP call failed took a hand-written seven-arm
//! match in one of the four review programs, and `Show` was the whole of what
//! was missing.
//!
//! `Eq` comes with it because the thing a *test* does with an error is compare
//! it to the one it expected.
//!
//! Compiled against the real `std` on purpose, unlike `derive.rs`, which is
//! self-contained: the question here is not what a derive expands to but
//! whether these particular types carry the impls, which is a fact about
//! `std/` and has to be read from `std/`.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std`, plus the program under test.
///
/// The whole tree rather than a hand-written list: `std::net::http` reaches
/// the sockets and the TLS shims, those reach `std::time`, and a list of what
/// an import needs today is a list that is wrong after the next commit.
/// `selected_for_target` is what keeps the Windows sockets out of a Linux
/// build.
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

/// Compiles `body` as the whole of `main`, runs it, and gives back its output.
fn run(name: &str, body: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let main = format!(
        "module demo::main;
import std::core::{{Eq, Show, print}};
import std::net::http::{{CallError, HttpError, Response}};
import std::env::{{EnvError}};
import std::fs::{{IoError}};
import std::db::{{DbError}};

fn same(yes: Bool) -> String {{ if yes {{ \"same\" }} else {{ \"different\" }} }}

fn main() -> () {{
{body}
}}
"
    );

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, &main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// **A failure can say what it was without a `match` at the call site.**
///
/// Derived, so the case name is there — which is what a reader of a log needs
/// to tell `Unreachable` (DNS, a firewall, a service that is down) from
/// `Denied` (a line in `khora.toml`), the distinction `CallError`'s own doc
/// comment exists to draw. `CallError`'s is written out, to say more about a
/// `TimedOut`, and must print every other case exactly as the derive did.
#[test]
fn every_std_error_can_say_what_it_is() {
    let out = run(
        "errors_show",
        r#"  print(CallError::TooLarge(4096).show());
  print(CallError::Denied("db.internal:5432").show());
  print(HttpError::BindFailed(8080).show());
  print(HttpError::MalformedRequest("no request line").show());
  print(EnvError::Denied("DATABASE_URL").show());
  print(IoError::NotFound("data/rates.csv").show());
  print(IoError::Denied("/etc/shadow").show());
  // The line an interpolated log actually writes.
  print("call failed: ${CallError::Closed("api.example.com")}");"#,
    );

    assert_eq!(
        out,
        "CallError::TooLarge(4096)\n\
         CallError::Denied(db.internal:5432)\n\
         HttpError::BindFailed(8080)\n\
         HttpError::MalformedRequest(no request line)\n\
         EnvError::Denied(DATABASE_URL)\n\
         IoError::NotFound(data/rates.csv)\n\
         IoError::Denied(/etc/shadow)\n\
         call failed: CallError::Closed(api.example.com)\n"
    );
}

/// **A `CallError::TimedOut` says what its number can be, in words true for
/// whoever reads it.** The value cannot say whether the call had a deadline:
/// `real()`, `bounded(limit)` and `within(30000)` end at a 30000 ms deadline,
/// and a `within(0)` client, which has none, ends at 30000 ms of silence
/// between reads, and all four are `TimedOut(30000)`. So the line names both
/// readings, and at 30000 says that number is the default for each. Any
/// other number is the caller's own deadline and gets no parenthesis.
#[test]
fn a_timeout_at_the_default_deadline_says_so() {
    let out = run(
        "errors_timed_out_show",
        r#"  print(CallError::TimedOut(30000).show());
  print(CallError::TimedOut(1500).show());"#,
    );

    assert_eq!(
        out,
        "CallError::TimedOut(30000): no answer within 30000 ms \
         (30000 ms is the default deadline `HttpClient::real()` and `HttpClient::bounded` give, \
         and what `default_answer_timeout` returns). That is the call's deadline, or, for a client \
         with no deadline (`HttpClient::within(0)`), the limit on silence between reads; \
         `HttpClient::within(ms)` or `HttpClient::bounded_within(limit, ms)` sets the deadline\n\
         CallError::TimedOut(1500): no answer within 1500 ms. That is the call's deadline, or, for \
         a client with no deadline (`HttpClient::within(0)`), the limit on silence between reads; \
         `HttpClient::within(ms)` or `HttpClient::bounded_within(limit, ms)` sets the deadline\n"
    );
}

/// **`DbError` keeps its hand-written `Show` and derives only `Eq`.**
///
/// A derived `Show` gives `DbError::Rejected(...)`, which is right for the
/// types above, whose cases a reader has to tell apart. This one goes into a
/// log line a person reads, and "rejected: duplicate key" is the sentence they
/// want rather than a constructor. The derive was refused where it would have
/// collided, which is how this was noticed.
#[test]
fn a_db_error_reads_as_a_sentence() {
    let out = run(
        "errors_db_show",
        r#"  print(DbError::Rejected("duplicate key").show());
  print(DbError::Disconnected("server closed the connection").show());
  print(DbError::RolledBack("deadlock detected").show());"#,
    );

    assert_eq!(
        out,
        "rejected: duplicate key\n\
         disconnected: server closed the connection\n\
         rolled back: deadlock detected\n"
    );
}

/// **What a test does with an error is compare it to the one it expected.**
///
/// Both halves matter: the same case with the same payload is equal, and two
/// different cases carrying the same string are not — which is the comparison
/// that would go wrong if `Eq` were written by hand and one arm forgotten.
#[test]
fn two_errors_can_be_compared() {
    let out = run(
        "errors_eq",
        r#"  print(same(CallError::TooLarge(1) == CallError::TooLarge(1)));
  print(same(CallError::TooLarge(1) == CallError::TooLarge(2)));
  print(same(CallError::BadUrl("x") == CallError::Unreachable("x")));
  print(same(IoError::NotFound("a") == IoError::NotFound("a")));
  print(same(IoError::NotFound("a") == IoError::Denied("a")));
  print(same(EnvError::Denied("A") == EnvError::Denied("A")));
  print(same(HttpError::BindFailed(80) == HttpError::BindFailed(80)));
  print(same(DbError::Rejected("x") == DbError::Rejected("x")));
  print(same(DbError::Rejected("x") == DbError::RolledBack("x")));"#,
    );

    assert_eq!(
        out,
        "same\ndifferent\ndifferent\nsame\ndifferent\nsame\nsame\nsame\ndifferent\n"
    );
}


/// **`Unknown` on the wire about a status the server deliberately chose.**
///
/// That happened twice: the link shortener answered a redirect and got
/// `HTTP/1.1 302 Unknown`, and later a service under load answered 503 and got
/// the same. The phrase is advisory — every client reads the number — but it
/// reads as a bug to whoever is holding the packet capture, and it was.
///
/// The whole of 5xx is here now, because that is the family a server sends
/// about *itself*, and those are the lines somebody reads at three in the
/// morning.
#[test]
fn every_status_a_service_sends_has_a_phrase() {
    let out = run(
        "errors_reason",
        r#"  // The ones a service under load or behind a proxy sends about itself.
  print(Response::reason(503));
  print(Response::reason(502));
  print(Response::reason(504));
  print(Response::reason(501));
  // And the client-side ones a real API answers with.
  print(Response::reason(202));
  print(Response::reason(402));
  print(Response::reason(406));
  print(Response::reason(410));
  print(Response::reason(415));
  print(Response::reason(428));
  print(Response::reason(431));
  // The ones that were already right stay right.
  print(Response::reason(200));
  print(Response::reason(302));
  print(Response::reason(413));
  // And something nobody has a phrase for is still honest about it.
  print(Response::reason(599));"#,
    );

    assert_eq!(
        out,
        "Service Unavailable\n\
         Bad Gateway\n\
         Gateway Timeout\n\
         Not Implemented\n\
         Accepted\n\
         Payment Required\n\
         Not Acceptable\n\
         Gone\n\
         Unsupported Media Type\n\
         Precondition Required\n\
         Request Header Fields Too Large\n\
         OK\n\
         Found\n\
         Payload Too Large\n\
         Unknown\n"
    );
}

/// Compiles `program` whole, runs it on both fiber backends, and gives back
/// each backend's exit status and standard error.
fn escaped(name: &str, program: &str) -> Vec<(&'static str, Option<i32>, String)> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, program));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }
    ["threads", "scheduler"]
        .into_iter()
        .map(|backend| {
            let out = Command::new(&exe)
                .env("KHORA_FIBERS", backend)
                .output()
                .expect("the program should run");
            (backend, out.status.code(), String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"))
        })
        .collect()
}

/// **An error that reaches `main` is named, then printed with its value when
/// its type has `Show`**, and named alone when it has none.
///
/// The name alone (`SocketError`) said what kind of thing went wrong and not
/// which: a refused port, a host that did not resolve and a denied grant all
/// printed the same line. The value is the type's own `Show`, the one `${e}`
/// prints, on a line of its own after the name.
///
/// A failure with context prints its `show()` on the `failure:` line, and its
/// first line stays the name.
#[test]
fn an_error_that_reaches_main_prints_its_value() {
    let note = "note: `main` has nowhere to hand an error, so the program ends here with \
                status 1. Catch it in `main`, or return a `Result`\n";

    let shown = escaped(
        "escaped_shown",
        r#"module demo::main;
import std::net::socket::{SocketError};

pub fn main() -> () raises SocketError {
  raise SocketError::Denied("0.0.0.0:80")
}
"#,
    );
    for (backend, code, err) in shown {
        assert_eq!(code, Some(1), "`{backend}`: {err}");
        assert_eq!(
            err,
            format!(
                "khora: `SocketError` reached the entry point and nothing handled it\n  \
                 value: SocketError::Denied(0.0.0.0:80)\n{note}"
            ),
            "`{backend}`"
        );
    }

    let bare = escaped(
        "escaped_bare",
        r#"module demo::main;

pub type Bare = | Oops;

pub fn main() -> () raises Bare {
  raise Bare::Oops
}
"#,
    );
    for (backend, code, err) in bare {
        assert_eq!(code, Some(1), "`{backend}`: {err}");
        assert_eq!(
            err,
            format!("khora: `Bare` reached the entry point and nothing handled it\n{note}"),
            "`{backend}`"
        );
    }

    let noted = escaped(
        "escaped_noted",
        r#"module demo::main;
import std::core::{Raised, Result, Show, attempt};

derive(Show)
pub type ShipError = | Embargo(country: String);

fn step() -> Int raises ShipError { raise ShipError::Embargo("XX") }

pub fn main() -> () raises ShipError {
  match attempt(fn () => step()!) {
    Result::Ok(_) => (),
    Result::Err(e) => raise Raised::context(e, "shipping order 42"),
  }
}
"#,
    );
    for (backend, code, err) in noted {
        assert_eq!(code, Some(1), "`{backend}`: {err}");
        assert_eq!(
            err,
            format!(
                "khora: `ShipError` reached the entry point and nothing handled it\n\
                 {note}failure: shipping order 42: ShipError::Embargo(XX)\n"
            ),
            "`{backend}`"
        );
    }
}

/// **A `Show` that traps still leaves the name of what reached `main`.**
///
/// The name is written, and flushed, before the type's `Show` runs. A `Show`
/// is user code: one that overflowed ended the program with only the trap's
/// own line, and the fact that mattered -- which error nobody handled -- was
/// never printed. The trap's exit status is the trap's; the line before it is
/// the one this guards.
#[test]
fn an_error_whose_show_traps_is_still_named() {
    let out = escaped(
        "escaped_show_traps",
        r#"module demo::main;
import std::core::{Show};

pub type Boom = | Bang(n: Int);

impl Show for Boom {
  fn show(self) -> String { match self { Boom::Bang(n) => "${n + 9223372036854775800}" } }
}

pub fn main() -> () raises Boom { raise Boom::Bang(100) }
"#,
    );
    for (backend, code, err) in out {
        assert_ne!(code, Some(0), "`{backend}`: {err}");
        assert!(
            err.starts_with("khora: `Boom` reached the entry point and nothing handled it\n"),
            "`{backend}`: the name line first, before the `Show` ran:\n{err}"
        );
        assert!(err.contains("overflowed"), "`{backend}`: the trap's own line too:\n{err}");
    }
}

/// **The value is one line, however its `Show` reads, and a bounded one.**
///
/// The value comes from user data -- a host out of a request, a body -- and
/// went to standard error as written: a newline in it forged a second
/// `khora:` line in a log, an escape sequence drove the terminal, and a
/// 4 MB value was printed whole. Control characters are written as escapes
/// and the value is cut at 4096 bytes, saying how much more there was.
#[test]
fn an_escaped_value_is_one_bounded_line() {
    let note = "note: `main` has nowhere to hand an error, so the program ends here with \
                status 1. Catch it in `main`, or return a `Result`\n";

    let controls = escaped(
        "escaped_controls",
        r#"module demo::main;
import std::core::{Show};

derive(Show)
pub type HostError = | Unresolved(host: String);

pub fn main() -> () raises HostError {
  raise HostError::Unresolved("evil.example\nkhora: all good\r\u{1b}[2J\u{7f}\tend")
}
"#,
    );
    for (backend, code, err) in controls {
        assert_eq!(code, Some(1), "`{backend}`: {err}");
        assert_eq!(
            err,
            format!(
                "khora: `HostError` reached the entry point and nothing handled it\n  \
                 value: HostError::Unresolved(evil.example\\nkhora: all good\\r\\u{{1b}}[2J\\u{{7f}}\tend)\n{note}"
            ),
            "`{backend}`"
        );
    }

    let huge = escaped(
        "escaped_huge",
        r#"module demo::main;
import std::core::{Show};

derive(Show)
pub type Huge = | Body(text: String);

fn grow(s: String, n: Int) -> String { if n == 0 { s } else { grow("${s}${s}", n - 1) } }

pub fn main() -> () raises Huge { raise Huge::Body(grow("é", 19)) }
"#,
    );
    // `Huge::Body(` is 11 bytes, then 524288 two-byte characters and `)`:
    // 1048588 bytes. Byte 4096 is the first half of a character, so the cut
    // is made one byte earlier, at 4095 (11 + 2042 characters), and says
    // 1048588 - 4095 bytes more.
    let kept = format!("Huge::Body({}", "é".repeat(2042));
    assert_eq!(kept.len(), 4095);
    for (backend, code, err) in huge {
        assert_eq!(code, Some(1), "`{backend}`: {} bytes", err.len());
        let expected = format!(
            "khora: `Huge` reached the entry point and nothing handled it\n  \
             value: {kept}… ({} bytes more)\n{note}",
            1_048_588 - 4095
        );
        assert!(err == expected, "`{backend}`: {} bytes of stderr, expected {}", err.len(), expected.len());
    }
}
