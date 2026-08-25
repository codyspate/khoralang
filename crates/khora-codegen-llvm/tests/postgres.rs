#![cfg(feature = "llvm")]

//! The Postgres driver, against a server that answers.
//!
//! **There is no PostgreSQL on the machine this was written on**, so the server
//! is here: eighty lines of Rust that speak enough of the protocol to complete
//! a handshake and answer one query. That is a weaker claim than talking to the
//! real thing and a much stronger one than testing nothing, and it has a
//! property a real server does not — it can assert about the bytes the *driver*
//! sent, which is the half a live connection cannot see.
//!
//! `packages/postgres/src/wire_test.kh` covers the encoding itself, in Khora,
//! byte for byte. This covers the conversation: startup, authentication, a
//! query, rows, and the `ReadyForQuery` that ends every exchange including a
//! failed one.
//!
//! # Against the real thing
//!
//! ```text
//! docker compose -f packages/postgres/docker-compose.yml up -d
//! KHORA_POSTGRES=1 cargo test -p khora-codegen-llvm --features llvm --test postgres
//! ```
//!
//! [`against_a_real_server`] then runs, and it is the one that can find what a
//! fake cannot: a real server's parameter list, its error format, its idea of
//! what an `int4` looks like as text. Without the variable it is skipped with
//! a message rather than failing, because a suite that needs Docker to pass is
//! a suite that does not run.

mod harness;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std` and the postgres package, plus the program.
fn sources(db: &KhoraDatabase, dir: &std::path::Path, main: &str) -> Vec<SourceFile> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut out = Vec::new();
    let mut stack = vec![root.join("std"), root.join("packages").join("postgres").join("src")];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
                // The package's own tests are `test` blocks, which a `main`
                // build has no entry point for.
                && !path.ends_with("wire_test.kh")
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

// --- a server that speaks just enough --------------------------------------

/// Reads one frontend message: a type byte, a length that counts itself, and a
/// payload.
fn read_message(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut kind = [0u8; 1];
    stream.read_exact(&mut kind).expect("a type byte");
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).expect("a length");
    let length = i32::from_be_bytes(length) as usize;
    let mut payload = vec![0u8; length - 4];
    stream.read_exact(&mut payload).expect("a payload");
    (kind[0], payload)
}

/// Writes one backend message.
fn write_message(stream: &mut TcpStream, kind: u8, payload: &[u8]) {
    let mut out = vec![kind];
    out.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    out.extend_from_slice(payload);
    stream.write_all(&out).expect("the message");
}

fn cstring(text: &str) -> Vec<u8> {
    let mut out = text.as_bytes().to_vec();
    out.push(0);
    out
}

/// What the driver said, so a test can assert about it.
struct Heard {
    startup: Vec<u8>,
    query: String,
}

/// Accepts one connection, completes a handshake, answers one query.
///
/// `auth` is the authentication method to demand: 0 for none, 3 for cleartext,
/// 10 for SCRAM — which this does not implement and the driver should refuse.
fn serve(listener: TcpListener, auth: i32, rows: Vec<Vec<Option<&'static str>>>) -> Heard {
    let (mut stream, _) = listener.accept().expect("a connection");
    // **A deadline, so a client that never speaks fails the test instead of
    // hanging it.** Without this a bug on the Khora side showed up as "has
    // been running for over 60 seconds" with no output at all, which says
    // nothing about which side is stuck.
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .expect("a read deadline");

    // The startup message has no type byte: a length, then the payload.
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).expect("a startup length");
    let mut startup = vec![0u8; i32::from_be_bytes(length) as usize - 4];
    stream.read_exact(&mut startup).expect("a startup payload");

    if auth != 0 {
        write_message(&mut stream, b'R', &auth.to_be_bytes());
        if auth == 3 {
            // 'p' PasswordMessage.
            let (kind, _) = read_message(&mut stream);
            assert_eq!(kind, b'p', "cleartext auth should be answered with a password");
        } else {
            // Nothing else is implemented; the driver is expected to give up,
            // so there is nothing more to read.
            return Heard { startup, query: String::new() };
        }
    }
    write_message(&mut stream, b'R', &0i32.to_be_bytes());

    // A parameter and a key, because a real server sends them and a driver
    // that choked on what it did not recognise would break on the next
    // server version.
    let mut parameter = cstring("server_version");
    parameter.extend_from_slice(&cstring("16.0"));
    write_message(&mut stream, b'S', &parameter);
    write_message(&mut stream, b'K', &[0, 0, 0, 1, 0, 0, 0, 2]);
    write_message(&mut stream, b'Z', b"I");

    // 'Q' Query.
    let (kind, payload) = read_message(&mut stream);
    assert_eq!(kind, b'Q', "the driver should send a simple query");
    let query = String::from_utf8_lossy(&payload[..payload.len() - 1]).into_owned();

    // 'T' RowDescription: two columns, `id` as int4 and `name` as text.
    let mut description = (2i16).to_be_bytes().to_vec();
    for (name, oid) in [("id", 23i32), ("name", 25i32)] {
        description.extend_from_slice(&cstring(name));
        description.extend_from_slice(&0i32.to_be_bytes()); // table oid
        description.extend_from_slice(&0i16.to_be_bytes()); // column number
        description.extend_from_slice(&oid.to_be_bytes());
        description.extend_from_slice(&(-1i16).to_be_bytes()); // type size
        description.extend_from_slice(&(-1i32).to_be_bytes()); // modifier
        description.extend_from_slice(&0i16.to_be_bytes()); // text format
    }
    write_message(&mut stream, b'T', &description);

    let count = rows.len();
    for row in &rows {
        let mut data = (row.len() as i16).to_be_bytes().to_vec();
        for value in row {
            match value {
                // -1 is NULL, which is not a value of length zero.
                None => data.extend_from_slice(&(-1i32).to_be_bytes()),
                Some(text) => {
                    data.extend_from_slice(&(text.len() as i32).to_be_bytes());
                    data.extend_from_slice(text.as_bytes());
                }
            }
        }
        write_message(&mut stream, b'D', &data);
    }

    write_message(&mut stream, b'C', &cstring(&format!("SELECT {count}")));
    write_message(&mut stream, b'Z', b"I");
    Heard { startup, query }
}

/// The Khora side: connect, query, print what came back.
fn program(port: u16, sql: &str, secret: &str) -> String {
    format!(
        "module demo::main;
import std::core::{{List, Option, Result, print}};
import std::db::{{Cell, Row}};
import postgres::conn::{{Answer, PgError, close, open, run}};

fn show_cell(c: Cell) -> String {{
  match c {{
    Cell::Null => \"null\",
    Cell::Text(t) => \"text:\" + t,
    Cell::Number(n) => \"number:\" + Int::to_string(n),
    Cell::Flag(b) => \"flag\",
    Cell::Money(m) => \"money\",
  }}
}}

fn show_row(cells: List<Cell>) -> String {{
  match cells {{
    List::Nil => \"\",
    List::Cons(head, tail) => match tail {{
      List::Nil => show_cell(head),
      List::Cons(_, _) => show_cell(head) + \",\" + show_row(tail),
    }},
  }}
}}

fn main() -> Int {{
  match open(\"127.0.0.1\", {port}, \"bob\", \"shop\", \"{secret}\") {{
    Result::Err(why) => {{
      match why {{
        PgError::Unreachable(m) => print(\"unreachable: \" + m),
        PgError::Refused(m) => print(\"refused: \" + m),
        PgError::Closed(m) => print(\"closed: \" + m),
        PgError::Unsupported(m) => print(\"unsupported: \" + m),
      }};
      1
    }},
    Result::Ok(c) => {{
      match run(c, \"{sql}\") {{
        Result::Err(why) => {{
          match why {{
            PgError::Unreachable(m) => print(\"unreachable: \" + m),
            PgError::Refused(m) => print(\"refused: \" + m),
            PgError::Closed(m) => print(\"closed: \" + m),
            PgError::Unsupported(m) => print(\"unsupported: \" + m),
          }};
          close(c);
          1
        }},
        Result::Ok(answer) => {{
          print(\"tag:\" + answer.tag);
          let mut rest = answer.rows;
          let mut going = true;
          while going {{
            match rest {{
              List::Nil => going = false,
              List::Cons(row, tail) => {{
                print(show_row(row.cells));
                rest = tail
              }},
            }}
          }};
          close(c);
          0
        }},
      }}
    }},
  }}
}}
"
    )
}

/// `name` gives each test its own build directory.
///
/// **Not a detail.** Every one of these called `build("postgres_client", ..)`,
/// and cargo runs a binary's tests on several threads: four tests compiled
/// four different programs to one path at once. One failed, three hung, and
/// the failure looked like a driver deadlock rather than what it was.
fn run_against(
    name: &str,
    auth: i32,
    rows: Vec<Vec<Option<&'static str>>>,
    sql: &str,
    secret: &str,
) -> (String, Heard, Option<i32>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let server = std::thread::spawn(move || serve(listener, auth, rows));

    let exe = build(name, &program(port, sql, secret));
    let ran = std::process::Command::new(&exe).output().expect("the program should run");
    let heard = server.join().expect("the server");
    (
        String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n"),
        heard,
        ran.status.code(),
    )
}

// --- the tests -------------------------------------------------------------

/// **The whole conversation.** Connect, handshake, query, rows out.
#[test]
fn khora_talks_to_a_postgres_server() {
    let (out, heard, code) = run_against(
        "pg_conversation",
        0,
        vec![
            vec![Some("1"), Some("ada")],
            vec![Some("2"), None],
        ],
        "select id, name from people",
        "",
    );

    assert_eq!(code, Some(0), "the client should succeed: {out}");
    assert_eq!(
        out,
        "tag:SELECT 2\nnumber:1,text:ada\nnumber:2,null\n",
        "two rows, an int4 as a Number, a text as Text, and NULL as Null"
    );
    assert_eq!(heard.query, "select id, name from people");

    // The startup message: version three, then `user` and `database`.
    assert_eq!(&heard.startup[..4], &[0, 3, 0, 0], "protocol version 3.0");
    let rest = String::from_utf8_lossy(&heard.startup[4..]);
    assert!(rest.contains("user"), "{rest}");
    assert!(rest.contains("bob"), "{rest}");
    assert!(rest.contains("shop"), "{rest}");
}

/// A `NULL` is not an empty string, and the two arrive as different lengths on
/// the wire: -1 against 0.
#[test]
fn null_and_empty_are_told_apart() {
    let (out, _, code) =
        run_against("pg_null", 0, vec![vec![None, Some("")]], "select a, b", "");
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(out, "tag:SELECT 1\nnull,text:\n");
}

/// Cleartext authentication: the server asks, the driver answers, the exchange
/// completes.
#[test]
fn a_password_is_sent_when_the_server_asks_for_one() {
    let (out, _, code) =
        run_against("pg_password", 3, vec![vec![Some("7"), Some("ok")]], "select 1", "hunter2");
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("number:7"), "{out}");
}

/// **A method the driver has not got is refused by name.**
///
/// SCRAM-SHA-256 is the default on PostgreSQL 14 and later, so a stock install
/// lands here — and "connection failed" would send somebody looking at their
/// network. The message says which method, why it cannot be answered, and what
/// to change.
#[test]
fn scram_is_refused_with_something_useful_to_read() {
    let (out, _, code) = run_against("pg_scram", 10, vec![], "select 1", "hunter2");
    assert_eq!(code, Some(1), "{out}");
    assert!(out.starts_with("unsupported: "), "{out}");
    assert!(out.contains("SCRAM-SHA-256"), "it names the method: {out}");
    assert!(out.contains("PostgreSQL 14"), "and why it is what you hit: {out}");
}

// --- and against PostgreSQL itself ----------------------------------------

/// The same conversation, against a real server.
///
/// **Everything above proves the driver agrees with my reading of the
/// protocol.** This proves it agrees with PostgreSQL, which is a different
/// claim and the one that matters — the fake server was written from the same
/// understanding as the driver, so the two can be wrong together.
///
/// Skipped without `KHORA_POSTGRES`. `packages/postgres/docker-compose.yml`
/// brings one up on 5433, deliberately not 5432, so a database somebody
/// already runs cannot be used by accident.
#[test]
fn against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }

    // A plain string: nothing is interpolated here, and `format!` over a
    // literal is its own lint.
    let main = "module demo::main;
import std::core::{List, Option, Result, print};
import std::db::{Cell, Row};
import postgres::conn::{Answer, PgError, close, open, run};

fn show_cell(c: Cell) -> String {
  match c {
    Cell::Null => \"null\",
    Cell::Text(t) => \"text:\" + t,
    Cell::Number(n) => \"number:\" + Int::to_string(n),
    Cell::Flag(b) => if b { \"flag:t\" } else { \"flag:f\" },
    Cell::Money(m) => \"money\",
  }
}

fn show_row(cells: List<Cell>) -> String {
  match cells {
    List::Nil => \"\",
    List::Cons(head, tail) => match tail {
      List::Nil => show_cell(head),
      List::Cons(_, _) => show_cell(head) + \",\" + show_row(tail),
    },
  }
}

fn main() -> Int {
  match open(\"127.0.0.1\", 5433, \"khora\", \"khora\", \"khora\") {
    Result::Err(why) => {
      match why {
        PgError::Unreachable(m) => print(\"unreachable: \" + m),
        PgError::Refused(m) => print(\"refused: \" + m),
        PgError::Closed(m) => print(\"closed: \" + m),
        PgError::Unsupported(m) => print(\"unsupported: \" + m),
      };
      1
    },
    Result::Ok(c) => {
      // Every shape the driver claims to map: an int4, a text, a bool, and a
      // NULL that is not an empty string.
      match run(c, \"select 42::int4, 'ada'::text, true, null::text\") {
        Result::Err(why) => {
          match why {
            PgError::Refused(m) => print(\"refused: \" + m),
            PgError::Unreachable(m) => print(\"unreachable: \" + m),
            PgError::Closed(m) => print(\"closed: \" + m),
            PgError::Unsupported(m) => print(\"unsupported: \" + m),
          };
          close(c);
          1
        },
        Result::Ok(answer) => {
          let mut rest = answer.rows;
          let mut going = true;
          while going {
            match rest {
              List::Nil => going = false,
              List::Cons(row, tail) => {
                print(show_row(row.cells));
                rest = tail
              },
            }
          };
          // A statement that fails has to leave the connection usable, which
          // is what `ReadyForQuery` after an error is for. If this second
          // query hangs or returns nothing, the stream desynchronised.
          match run(c, \"select * from a_table_that_is_not_there\") {
            Result::Err(_) => print(\"rejected\"),
            Result::Ok(_) => print(\"NOT rejected\"),
          };
          match run(c, \"select 7::int4\") {
            Result::Ok(after) => {
              let mut r2 = after.rows;
              match r2 {
                List::Nil => print(\"no row after the error\"),
                List::Cons(row, _) => print(show_row(row.cells)),
              }
            },
            Result::Err(_) => print(\"the connection did not survive\"),
          };
          close(c);
          0
        },
      }
    },
  }
}
";

    let exe = build("postgres_real", main);
    let ran = std::process::Command::new(&exe).output().expect("the program should run");
    let out = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert_eq!(ran.status.code(), Some(0), "the client should succeed: {out}");
    assert_eq!(
        out,
        "number:42,text:ada,flag:t,null\nrejected\nnumber:7\n",
        "a real server's own text for each type, an error that does not break \
         the connection, and a query after it"
    );
}
