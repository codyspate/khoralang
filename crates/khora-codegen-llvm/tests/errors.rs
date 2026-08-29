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

mod harness;

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
/// comment exists to draw.
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
