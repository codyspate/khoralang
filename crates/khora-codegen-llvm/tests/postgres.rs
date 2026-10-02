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
//! KHORA_POSTGRES=1 cargo test -p khora-codegen-llvm --features llvm --test suite -- postgres::
//! ```
//!
//! [`against_a_real_server`] then runs, and it is the one that can find what a
//! fake cannot: a real server's parameter list, its error format, its idea of
//! what an `int4` looks like as text. Without the variable it is skipped with
//! a message rather than failing, because a suite that needs Docker to pass is
//! a suite that does not run.

use crate::harness;

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
    // that choked on what it did not recognize would break on the next
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

/// **An authentication method the driver has not got is refused by name.**
///
/// MD5 is what lands here now: `ring` does not carry it, deliberately, because
/// nothing new should be using it. The message says which method, why it
/// cannot be answered, and what to change — "connection failed" would send
/// somebody looking at their network.
///
/// **SCRAM used to be tested here and deliberately is not any more.** The fake
/// server would have to implement the whole exchange to answer method 10, and
/// a fake written from the same reading of the RFC as the driver proves only
/// that the two agree with each other. What settles SCRAM is
/// `against_a_real_server`, where PostgreSQL implements the document
/// independently and rejects a wrong proof on its own authority.
#[test]
fn an_unknown_method_is_refused_with_something_useful_to_read() {
    let (out, _, code) = run_against("pg_md5", 5, vec![], "select 1", "hunter2");
    assert_eq!(code, Some(1), "{out}");
    assert!(out.starts_with("unsupported: "), "{out}");
    assert!(out.contains("MD5"), "it names the method: {out}");
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

/// Bound parameters, against a real server, including a value that would end
/// the statement if it were concatenated into it.
///
/// The fake server cannot check this. It could be taught to echo a `Bind`, but
/// what is being tested is that *PostgreSQL* treats the value as a value --
/// that `'; drop table` arrives as eleven characters of text and not as SQL --
/// and only PostgreSQL can answer that.
///
/// Skipped without `KHORA_POSTGRES`, like its neighbor.
#[test]
fn bound_parameters_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }

    let main = "module demo::main;
import std::core::{List, Option, Result, print};
import std::db::{Cell, Row};
import postgres::conn::{Answer, Connection, PgError, ask, close, open};

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

fn first(answer: Answer) -> String {
  match answer.rows {
    List::Nil => \"no rows\",
    List::Cons(row, _) => show_row(row.cells),
  }
}

fn one(c: Cell) -> List<Cell> { List::Cons(c, List::Nil) }

fn say(c: Connection, sql: String, values: List<Cell>) -> () {
  match ask(c, sql, values) {
    Result::Ok(answer) => print(first(answer)),
    Result::Err(why) => match why {
      PgError::Refused(m) => print(\"refused: \" + m),
      PgError::Unreachable(m) => print(\"unreachable: \" + m),
      PgError::Closed(m) => print(\"closed: \" + m),
      PgError::Unsupported(m) => print(\"unsupported: \" + m),
    },
  }
}

fn main() -> Int {
  match open(\"127.0.0.1\", 5433, \"khora\", \"khora\", \"khora\") {
    Result::Err(_) => 1,
    Result::Ok(c) => {
      say(c, \"select $1::int4\", one(Cell::Number(42)));
      say(c, \"select $1::text\", one(Cell::Text(\"ada\")));
      say(c, \"select $1::bool\", one(Cell::Flag(true)));
      say(c, \"select $1::text\", one(Cell::Null));
      say(c, \"select $1::int4 + $2::int4, $1::int4\",
        List::Cons(Cell::Number(3), List::Cons(Cell::Number(4), List::Nil)));
      say(c, \"select $1::text\", one(Cell::Text(\"'; --\")));
      say(c, \"select $1::text is null\", one(Cell::Text(\"\")));
      say(c, \"select $1::text is null\", one(Cell::Null));
      say(c, \"select $1::int4\", one(Cell::Text(\"not a number\")));
      say(c, \"select $1::int4\", one(Cell::Number(7)));
      close(c);
      0
    },
  }
}
";

    let exe = build("postgres_bound", main);
    let ran = std::process::Command::new(&exe).output().expect("the program should run");
    let out = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert_eq!(ran.status.code(), Some(0), "the client should succeed: {out}");

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.first().copied(), Some("number:42"), "{out}");
    assert_eq!(lines.get(1).copied(), Some("text:ada"), "{out}");
    assert_eq!(lines.get(2).copied(), Some("flag:t"), "{out}");
    assert_eq!(lines.get(3).copied(), Some("null"), "{out}");
    assert_eq!(lines.get(4).copied(), Some("number:7,number:3"), "$1 used twice: {out}");
    assert_eq!(
        lines.get(5).copied(),
        Some("text:'; --"),
        "the value came back as a value, character for character: {out}"
    );
    assert_eq!(lines.get(6).copied(), Some("flag:f"), "'' is not null: {out}");
    assert_eq!(lines.get(7).copied(), Some("flag:t"), "NULL is: {out}");
    assert!(
        lines.get(8).is_some_and(|l| l.starts_with("refused:")),
        "a bad value is the server's error, not a hang: {out}"
    );
    assert_eq!(
        lines.get(9).copied(),
        Some("number:7"),
        "and the connection survived it: {out}"
    );
}

/// The `Db` capability, against a real server, through a transaction.
///
/// This is the whole point of the driver: `std::db::transaction` is written
/// against `Db` and has never heard of PostgreSQL, and it commits and rolls
/// back correctly anyway. It also exercises the arrangement that makes it
/// possible -- one fiber owns the connection, the handler holds a channel --
/// which is what `docs/design/channels.md` exists for.
///
/// Skipped without `KHORA_POSTGRES`, like its neighbors.
#[test]
fn the_db_capability_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }

    let main = "module demo::main;
import std::core::{Channel, Fiber, Fibers, List, Option, Result, print};
import std::db::{Cell, Db, DbError, Row, transaction};
import postgres::db::{Request, Settings, over, serve};

fn one(c: Cell) -> List<Cell> { List::Cons(c, List::Nil) }

fn show(answer: Result<List<Row>, DbError>) -> String {
  match answer {
    Result::Err(why) => match why {
      DbError::Rejected(m) => \"rejected: \" + m,
      DbError::Disconnected(m) => \"disconnected: \" + m,
      DbError::RolledBack(m) => \"rolled back: \" + m,
    },
    Result::Ok(rows) => match rows {
      List::Nil => \"no rows\",
      List::Cons(row, _) => match row.cells {
        List::Nil => \"no cells\",
        List::Cons(cell, _) => match cell {
          Cell::Number(n) => Int::to_string(n),
          Cell::Text(t) => t,
          Cell::Flag(b) => if b { \"t\" } else { \"f\" },
          Cell::Null => \"null\",
          Cell::Money(_) => \"money\",
        },
      },
    },
  }
}

fn insert(n: Int) -> Result<Int, DbError>
  with { db: Db }
{
  db.execute(\"insert into kept (n) values ($1)\", one(Cell::Number(n)))
}

fn good() -> Result<Int, DbError>
  with { db: Db }
{
  insert(1)
}

fn bad() -> Result<Int, DbError>
  with { db: Db }
{
  match insert(2) {
    Result::Err(why) => Result::Err(why),
    Result::Ok(_) => match db.query(\"select * from nowhere\", List::Nil) {
      Result::Err(why) => Result::Err(why),
      Result::Ok(_) => Result::Ok(0),
    },
  }
}

fn keeps() -> ()
  with { db: Db }
{
  match transaction(fn () => good()) {
    Result::Ok(_) => print(\"committed\"),
    Result::Err(_) => print(\"NOT committed\"),
  };
  print(show(db.query(\"select count(*)::int4 from kept\", List::Nil)));
}

fn discards() -> ()
  with { db: Db }
{
  match transaction(fn () => bad()) {
    Result::Err(DbError::RolledBack(_)) => print(\"rolled back\"),
    Result::Err(_) => print(\"failed, but not as a rollback\"),
    Result::Ok(_) => print(\"NOT rolled back\"),
  };
  print(show(db.query(\"select count(*)::int4 from kept\", List::Nil)));
}

fn work(requests: Channel<Request>) -> () {
  with { db: over(requests) } {
    match db.execute(\"drop table if exists kept\", List::Nil) {
      Result::Ok(_) => (),
      Result::Err(_) => print(\"could not drop\"),
    };
    match db.execute(\"create table kept (n int4)\", List::Nil) {
      Result::Ok(_) => (),
      Result::Err(_) => print(\"could not create\"),
    };
    keeps();
    discards();
  }
}

fn main() -> Int {
  let settings: Settings = {
    host: \"127.0.0.1\",
    port: 5433,
    user: \"khora\",
    database: \"khora\",
    secret: \"khora\",
  };
  let requests: Channel<Request> = Channel::bounded(4);
  let crew = Fibers::open();
  Fibers::adopt(crew, Fiber::spawn(fn () => serve(settings, requests)));
  work(requests);
  Channel::close(requests);
  let _stopped = Fibers::wait(crew);
  0
}
";

    let exe = build("postgres_capability", main);
    let ran = std::process::Command::new(&exe).output().expect("the program should run");
    let out = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert_eq!(ran.status.code(), Some(0), "the client should succeed: {out}");
    assert_eq!(
        out,
        "committed\n1\nrolled back\n1\n",
        "the first transaction's row survives and the second's does not: {out}"
    );
}

/// A pool of two connections serving four fibers, against a real server.
///
/// Two claims. **Every fiber gets served** even though there are twice as many
/// of them as there are connections -- the fifth request does not fail and
/// does not queue against the database, the *fiber* waits. And **a lease
/// survives a body that leaves badly**: the third fiber's work raises, and the
/// pool still has both connections afterwards, because `with_db` registers the
/// return with `Scope` before the body runs.
#[test]
fn a_pool_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }

    let main = "module demo::main;
import std::core::{Channel, Fibers, List, Result, print};
import std::db::{Cell, Db, DbError, Row};
import postgres::db::{Settings};
import postgres::pool::{Pool, close, open, with_db, idle_count};

extern fn khora_sleep(millis: Int) -> ();
extern fn khora_monotonic_millis() -> Int;

pub type Oops = | Failed;

fn number(answer: Result<List<Row>, DbError>) -> Int {
  match answer {
    Result::Err(_) => 0 - 1,
    Result::Ok(rows) => match rows {
      List::Nil => 0 - 2,
      List::Cons(row, _) => match row.cells {
        List::Nil => 0 - 3,
        List::Cons(cell, _) => match cell {
          Cell::Number(n) => n,
          Cell::Null => 0 - 4,
          Cell::Text(_) => 0 - 5,
          Cell::Flag(_) => 0 - 6,
          Cell::Money(_) => 0 - 7,
        },
      },
    },
  }
}

fn add(a: Int, b: Int) -> Int
  with { db: Db }
{
  number(db.query(\"select ($1::int4 + $2::int4)\",
    List::Cons(Cell::Number(a), List::Cons(Cell::Number(b), List::Nil))))
}

fn one_job(pool: Pool, n: Int) -> () {
  match with_db(pool, fn () => add(n, n)) {
    Result::Ok(total) => print(Int::to_string(total)),
    Result::Err(_) => print(\"no connection\"),
  };
}

fn fail_while_leased() -> ()
  with { db: Db }
  raises Oops
{
  raise Oops::Failed
}

fn bad_job(pool: Pool) -> () raises Oops {
  with_db(pool, fail_while_leased)!;
}

fn run_jobs(pool: Pool) -> () {
  one_job(pool, 1);
  one_job(pool, 2);
  catch_bad(pool);
  one_job(pool, 3);
}

fn catch_bad(pool: Pool) -> () {
  bad_job(pool)! catch { Oops::Failed => print(\"raised\") };
}

fn main() -> Int {
  let settings: Settings = {
    host: \"127.0.0.1\",
    port: 5433,
    user: \"khora\",
    database: \"khora\",
    secret: \"khora\",
  };
  let crew = Fibers::open();
  let pool = open(crew, settings, 2);
  run_jobs(pool);
  // A lease ends when its serving fiber reads the give-back, just after
  // `with_db` returns, so the count is read once it has settled.
  let deadline = khora_monotonic_millis() + 5000;
  while idle_count(pool) != 2 && khora_monotonic_millis() < deadline { khora_sleep(5) };
  print(Int::to_string(idle_count(pool)));
  close(pool);
  let _stopped = Fibers::wait(crew);
  0
}
";

    let exe = build("postgres_pool", main);
    let ran = std::process::Command::new(&exe).output().expect("the program should run");
    let out = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert_eq!(ran.status.code(), Some(0), "the client should succeed: {out}");
    assert_eq!(
        out,
        "2\n4\nraised\n6\n2\n",
        "three sums, a caught raise, and both connections back in the pool: {out}"
    );
}

/// **13.3, against the server that has to believe it.** A fiber canceled
/// inside a transaction leaves nothing behind.
#[test]
fn a_canceled_transaction_leaves_nothing_behind() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }

    let main = r#"module demo::main;
import std::core::{Fiber, Fibers, List, Option, Result, Show, print};
import std::db::{Cell, Db, DbError, Row, transaction};
import postgres::db::{Settings};
import postgres::pool::{Pool, close as close_pool, open as open_pool, with_db};

extern fn khora_cancel();

pub type Oops = | Bad;

fn mark() -> Int raises Oops { 1 }

fn settings() -> Settings {
  {
    host: "127.0.0.1",
    port: 5433,
    user: "khora",
    database: "khora",
    secret: "khora",
  }
}

fn one(c: Cell) -> List<Cell> { List::Cons(c, List::Nil) }

fn interrupted_transaction() -> Result<Int, DbError>
  with { db: Db }
  raises Oops
{
  transaction(fn () => {
    db.execute("insert into khora_cancel_probe (note) values ($1)",
      one(Cell::Text("should not survive")));
    khora_cancel();
    mark()!;
    Result::Ok(1)
  })!
}

fn interrupted(pool: Pool) -> () raises Oops {
  with_db(pool, interrupted_transaction)!;
  print("the fiber carried on, which is wrong");
}

fn committed_transaction() -> Result<Int, DbError>
  with { db: Db }
{
  transaction(fn () => {
    db.execute("insert into khora_cancel_probe (note) values ($1)",
      one(Cell::Text("should survive")));
    Result::Ok(1)
  })
}

fn committed(pool: Pool) -> () {
  match with_db(pool, committed_transaction) {
    Result::Err(problem) => print("no connection: " + problem.show()),
    Result::Ok(inner) => match inner {
      Result::Ok(_) => print("wrote one"),
      Result::Err(problem) => print("did not write: " + problem.show()),
    },
  }
}

fn count_rows() -> Result<List<Row>, DbError>
  with { db: Db }
{
  db.query("select count(*)::int4 from khora_cancel_probe", List::Nil)
}

fn count(pool: Pool) -> () {
  match with_db(pool, count_rows) {
    Result::Err(problem) => print("no connection: " + problem.show()),
    Result::Ok(inner) => match inner {
      Result::Err(problem) => print("query failed: " + problem.show()),
      Result::Ok(rows) => match rows {
        List::Nil => print("no rows"),
        List::Cons(row, _) => match Row::cell(row, 0) {
          Option::Some(cell) => print("rows: " + cell.show()),
          Option::None => print("no cell"),
        },
      },
    },
  }
}

fn prepare_schema() -> ()
  with { db: Db }
{
  let _ = db.execute("drop table if exists khora_cancel_probe", List::Nil);
  let _ = db.execute(
    "create table khora_cancel_probe (id serial primary key, note text not null)",
    List::Nil,
  );
}

fn schema(pool: Pool) -> () {
  with_db(pool, prepare_schema);
}

fn main() -> Int {
  let crew = Fibers::open();
  let pool = open_pool(crew, settings(), 2);
  schema(pool);

  let f = Fiber::spawn(fn () => interrupted(pool)!);
  // `wait`, not `join`: the child is stopped, and joining a stopped child
  // stops the joiner. `catch` is for the row `wait` carries from the child.
  Fiber::wait(f)! catch { Oops::Bad => () };
  print("the parent carried on");

  committed(pool);
  count(pool);

  close_pool(pool);
  0
}
"#;

    let exe = build("postgres_canceled_transaction", main);
    let ran = std::process::Command::new(&exe).output().expect("the program should run");
    let out = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert_eq!(ran.status.code(), Some(0), "the program should end cleanly: {out}");
    assert_eq!(
        out,
        "the parent carried on\nwrote one\nrows: 1\n",
        "the canceled insert must be gone and the committed one must be there"
    );
}

// --- a lease handed over at the moment its waiter is canceled --------------

/// How one watched run ended.
struct Watched {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    /// Whether the watchdog had to kill it. A pool that has lost a connection
    /// shows up as this: `close` waits for a serving fiber nobody will stop.
    hung: bool,
}

/// Runs `exe` on fiber backend `backend`, killing it after `patience`.
fn run_watched(exe: &std::path::Path, backend: &str, patience: std::time::Duration) -> Watched {
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = std::time::Instant::now();
    let mut hung = false;
    while child.try_wait().expect("waiting").is_none() {
        if started.elapsed() > patience {
            let _ = child.kill();
            hung = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let status = child.wait().expect("reaping");
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_string(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    Watched {
        stdout: stdout.replace("\r\n", "\n"),
        stderr: stderr.replace("\r\n", "\n"),
        code: status.code(),
        hung,
    }
}

/// Settings for a server that accepts every connection and says nothing
/// after the handshake, on a port of its own for this test.
///
/// **A pool lends only connections that opened**, so a pool test needs
/// something that accepts. Nothing here queries: the bodies under test do
/// not touch the database, and the pool's own `Check` before each lease is a
/// read that finds nothing waiting, which is the healthy answer.
fn a_quiet_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("an address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            std::thread::spawn(move || {
                let mut length = [0u8; 4];
                if stream.read_exact(&mut length).is_err() {
                    return;
                }
                let mut startup = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
                if stream.read_exact(&mut startup).is_err() {
                    return;
                }
                let mut hello = framed(b'R', &0i32.to_be_bytes());
                hello.extend(framed(b'Z', b"I"));
                if stream.write_all(&hello).is_err() {
                    return;
                }
                // Until the driver hangs up.
                let mut sink = [0u8; 64];
                while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
            });
        }
    });
    format!("{{ host: \"127.0.0.1\", port: {port}, user: \"khora\", database: \"khora\", secret: \"khora\" }}")
}

/// Settings for the real server `KHORA_POSTGRES` promises.
const REAL: &str = "{ host: \"127.0.0.1\", port: 5433, user: \"khora\", database: \"khora\", secret: \"khora\" }";

/// A pool of one. Each trial takes the connection itself in a lease whose
/// body waits to be told to finish, parks a waiter in `with_db`, then ends
/// the first lease and cancels the waiter straight after, so the cancel lands
/// while the waiter is being handed the connection or just after. Whichever
/// it is, the connection must end up back in the pool, and `close` must
/// return.
fn handover_program(settings: &str, leased: &str) -> String {
    format!(
        "module demo::main;
import std::core::{{Channel, Fiber, Fibers, List, Option, Result, print}};
import std::db::{{Db, DbError, Row}};
import postgres::db::{{Settings}};
import postgres::pool::{{Pool, close, open, with_db, idle_count}};

extern fn khora_sleep(millis: Int) -> ();

fn leased() -> Int
  with {{ db: Db }}
{{
  {leased}
}}

/// How many connections are idle once the pool has settled at `want`, or
/// what it settled at after five seconds. A lease ends when its give-back
/// runs, which is as `with_db` returns.
fn settled(pool: Pool, want: Int) -> Int {{
  let mut left = 5000;
  while idle_count(pool) != want && left > 0 {{
    khora_sleep(1);
    left = left - 1
  }};
  idle_count(pool)
}}

fn lease(pool: Pool) -> () {{
  let _ = with_db(pool, leased);
  ()
}}

fn wait_for(go: Channel<Int>) -> Int with {{ db: Db }} {{
  match Channel::receive(go) {{ Option::Some(n) => n, Option::None => 0 }}
}}

/// Holds the pool's connection until `go` is sent to.
fn hold(pool: Pool, holding: Channel<Int>, go: Channel<Int>) -> () {{
  let _ = with_db(pool, fn () => {{
    Channel::send(holding, 1);
    wait_for(go)
  }});
  ()
}}

fn main() -> Int {{
  let settings: Settings = {settings};
  let crew = Fibers::open();
  let pool = open(crew, settings, 1);
  let mut trial = 0;
  let mut lost = 0 - 1;
  while trial < 200 && lost < 0 {{
    let holding: Channel<Int> = Channel::bounded(1);
    let go: Channel<Int> = Channel::bounded(1);
    let holder = Fiber::spawn(fn () => hold(pool, holding, go));
    let _ = Channel::receive(holding);
    let waiter = Fiber::spawn(fn () => lease(pool));
    khora_sleep(1 + trial % 3);
    Channel::send(go, 1);
    Fiber::wait(holder);
    Fiber::cancel(waiter);
    Fiber::wait(waiter);
    if settled(pool, 1) != 1 {{ lost = trial }} else {{}};
    trial = trial + 1
  }};
  if lost >= 0 {{
    print(\"lost the connection at trial \" + Int::to_string(lost));
  }} else {{
    print(\"kept it through 200 trials\");
    close(pool);
    print(\"closed\");
  }};
  0
}}
"
    )
}

/// Runs a handover program on both backends and asserts the lease survived.
///
/// Both backends are run before anything is asserted, so a failure reports
/// what each of them did.
fn assert_the_lease_comes_back(name: &str, source: &str) {
    let exe = build(name, source);
    let ran: Vec<(&str, Watched)> = ["threads", "scheduler"]
        .into_iter()
        .map(|backend| (backend, run_watched(&exe, backend, std::time::Duration::from_secs(60))))
        .collect();
    let seen: Vec<String> =
        ran.iter().map(|(backend, r)| format!("{backend}: {:?}, hung {}", r.stdout, r.hung)).collect();
    for (backend, ran) in &ran {
        assert!(
            !ran.hung,
            "{backend} hung, which is what a lost connection does to `close`: {seen:?}"
        );
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout, "kept it through 200 trials\nclosed\n",
            "a waiter canceled as it was handed the connection must give it back: {seen:?}"
        );
    }
}

/// **A waiter canceled as the connection reaches it gives the connection
/// back.** The receive hands the connection over and does not look at the
/// cancel, which is right: a receive that got a value never drops it. What
/// came after it was the hole -- the give-back was registered inside `scoped`
/// and `acquire`, each entered through a cancellation check, so a cancel
/// taken there unwound holding a connection no finalizer knew about. The
/// pool shrank by one and `close` then waited for ever.
///
/// No server is needed: the lease goes round the pool whether or not its
/// connection opened. Both backends; before the fix this lost the connection
/// at the first trial on threads and within ten on the scheduler.
#[test]
fn a_waiter_canceled_at_the_hand_over_gives_the_connection_back() {
    assert_the_lease_comes_back("pool_handover", &handover_program(&a_quiet_server(), "0"));
}

/// The same, with the waiter's body querying a real server.
///
/// Skipped without `KHORA_POSTGRES`, like its neighbors.
#[test]
fn a_waiter_canceled_at_the_hand_over_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }
    let leased = "match db.query(\"select 1\", List::Nil) {
    Result::Ok(_) => 1,
    Result::Err(_) => 0,
  }";
    assert_the_lease_comes_back("pool_handover_real", &handover_program(REAL, leased));
}

/// Every way out of `with_db` gives the lease back, and nobody is starved.
///
/// A pool of two, no server. In order: a body that returns; a body that
/// raises; a body canceled while it runs; a waiter canceled while it is
/// still parked for a connection; and eight fibers taking twenty-five leases
/// each, all of which must be served. The pool must hold both connections
/// after each, and `close` must return.
///
/// **Not a regression test for the hand-over**: none of these cancels lands
/// in the gap that one guards, so this is green with or without that fix. It
/// pins the paths the fix rewrote, which a wrong fix would break.
#[test]
fn a_pool_gives_every_lease_back_however_the_body_ends() {
    let quiet = a_quiet_server();
    let main = format!(
        "module demo::main;
import std::core::{{Channel, Fiber, Fibers, Option, Result, print}};
import std::db::{{Db}};
import postgres::db::{{Settings}};
import postgres::pool::{{Pool, close, open, with_db, idle_count}};

extern fn khora_sleep(millis: Int) -> ();

pub type Oops = | Failed;

fn seven() -> Int with {{ db: Db }} {{ 7 }}

fn fail() -> Int with {{ db: Db }} raises Oops {{ raise Oops::Failed }}

fn served_wrongly() -> Int with {{ db: Db }} {{
  print(\"a canceled waiter was served, which is wrong\");
  0
}}

fn stuck(entered: Channel<Int>, never: Channel<Int>) -> Int with {{ db: Db }} {{
  Channel::send(entered, 1);
  match Channel::receive(never) {{
    Option::Some(n) => n,
    Option::None => 0,
  }}
}}

/// How many slots are idle once the pool has settled at `want`, or what it
/// settled at after five seconds. A lease ends when its serving fiber reads
/// the give-back, which is after `with_db` has returned.
fn settled(pool: Pool, want: Int) -> Int {{
  let mut left = 5000;
  while idle_count(pool) != want && left > 0 {{
    khora_sleep(1);
    left = left - 1
  }};
  idle_count(pool)
}}

fn idle(pool: Pool) -> String {{ \"idle \" + Int::to_string(settled(pool, 2)) }}

fn succeed(pool: Pool) -> Int {{
  match with_db(pool, seven) {{
    Result::Ok(n) => n,
    Result::Err(_) => 0 - 1,
  }}
}}

fn raising(pool: Pool) -> () raises Oops {{
  with_db(pool, fail)!;
}}

fn caught(pool: Pool) -> () {{
  raising(pool)! catch {{ Oops::Failed => print(\"raised: \" + idle(pool)) }};
}}

fn canceled_inside(pool: Pool) -> () {{
  let entered: Channel<Int> = Channel::bounded(1);
  let never: Channel<Int> = Channel::bounded(1);
  let f = Fiber::spawn(fn () => {{
    let _ = with_db(pool, fn () => stuck(entered, never));
    print(\"the canceled body carried on, which is wrong\");
  }});
  let _ = Channel::receive(entered);
  Fiber::cancel(f);
  Fiber::wait(f);
  print(\"canceled while leased: \" + idle(pool));
}}

fn wait_for(go: Channel<Int>) -> Int with {{ db: Db }} {{
  match Channel::receive(go) {{ Option::Some(n) => n, Option::None => 0 }}
}}

/// Holds one of the pool's connections until `go` is sent to.
fn hold(pool: Pool, holding: Channel<Int>, go: Channel<Int>) -> () {{
  let _ = with_db(pool, fn () => {{
    Channel::send(holding, 1);
    wait_for(go)
  }});
  ()
}}

fn canceled_waiting(pool: Pool) -> () {{
  let holding: Channel<Int> = Channel::bounded(2);
  let go: Channel<Int> = Channel::bounded(2);
  let a = Fiber::spawn(fn () => hold(pool, holding, go));
  let b = Fiber::spawn(fn () => hold(pool, holding, go));
  let _ = Channel::receive(holding);
  let _ = Channel::receive(holding);
  let f = Fiber::spawn(fn () => {{
    let _ = with_db(pool, served_wrongly);
    ()
  }});
  khora_sleep(20);
  Fiber::cancel(f);
  Fiber::wait(f);
  Channel::send(go, 1);
  Channel::send(go, 1);
  Fiber::wait(a);
  Fiber::wait(b);
  print(\"canceled while waiting: \" + idle(pool));
}}

fn worker(pool: Pool, done: Channel<Int>) -> () {{
  let mut n = 0;
  let mut served = 0;
  while n < 25 {{
    if succeed(pool) == 7 {{ served = served + 1 }} else {{}};
    n = n + 1
  }};
  Channel::send(done, served);
  ()
}}

fn shared_out(pool: Pool) -> () {{
  let done: Channel<Int> = Channel::bounded(8);
  let mut spawned = 0;
  while spawned < 8 {{
    let _ = Fiber::spawn(fn () => worker(pool, done));
    spawned = spawned + 1
  }};
  let mut total = 0;
  let mut heard = 0;
  while heard < 8 {{
    match Channel::receive(done) {{
      Option::Some(n) => total = total + n,
      Option::None => (),
    }};
    heard = heard + 1
  }};
  print(\"served \" + Int::to_string(total) + \" of 200: \" + idle(pool));
}}

fn main() -> Int {{
  let settings: Settings = {quiet};
  let crew = Fibers::open();
  let pool = open(crew, settings, 2);
  print(\"returned \" + Int::to_string(succeed(pool)) + \": \" + idle(pool));
  caught(pool);
  canceled_inside(pool);
  canceled_waiting(pool);
  shared_out(pool);
  close(pool);
  print(\"closed\");
  0
}}
"
    );
    let exe = build("pool_every_way_out", &main);
    for backend in ["threads", "scheduler"] {
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(60));
        assert!(!ran.hung, "{backend}: the program hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout,
            "returned 7: idle 2\n\
             raised: idle 2\n\
             canceled while leased: idle 2\n\
             canceled while waiting: idle 2\n\
             served 200 of 200: idle 2\n\
             closed\n",
            "{backend}"
        );
    }
}

// --- a lease that outlives `close` -----------------------------------------

/// **A lease still out when `close` is called ends, and `close` then
/// returns.** `close` waits for every slot's fiber, and a slot's fiber waits
/// for its connection to come home; a lease that ends after `close` began
/// must send it there rather than into a pool nobody will lend from again.
///
/// A pool of one: a fiber holds the connection, `close` starts in another,
/// and must still be waiting 100 ms later; the lease then ends, and `close`
/// must return.
#[test]
fn a_lease_ended_after_close_lets_close_return() {
    let main = format!(
        "module demo::main;
import std::core::{{Channel, Fiber, Fibers, Option, print}};
import std::db::{{Db}};
import postgres::db::{{Settings}};
import postgres::pool::{{Pool, close, open, idle_count, with_db}};

extern fn khora_sleep(millis: Int) -> ();

fn settled(pool: Pool, want: Int) -> Int {{
  let mut left = 5000;
  while idle_count(pool) != want && left > 0 {{
    khora_sleep(1);
    left = left - 1
  }};
  idle_count(pool)
}}

fn wait_for(go: Channel<Int>) -> Int with {{ db: Db }} {{
  match Channel::receive(go) {{ Option::Some(n) => n, Option::None => 0 }}
}}

fn hold(pool: Pool, holding: Channel<Int>, go: Channel<Int>) -> () {{
  let _ = with_db(pool, fn () => {{
    Channel::send(holding, 1);
    wait_for(go)
  }});
  ()
}}

fn main() -> Int {{
  let settings: Settings = {quiet};
  let a = open(Fibers::open(), settings, 1);
  print(\"before: \" + Int::to_string(settled(a, 1)));
  let holding: Channel<Int> = Channel::bounded(1);
  let go: Channel<Int> = Channel::bounded(1);
  let holder = Fiber::spawn(fn () => hold(a, holding, go));
  let _ = Channel::receive(holding);
  let done: Channel<Int> = Channel::bounded(1);
  let closer = Fiber::spawn(fn () => {{
    close(a);
    Channel::send(done, 1);
    ()
  }});
  khora_sleep(100);
  print(\"closed while the lease is out: \" + (if Channel::depth(done) == 1 {{ \"yes\" }} else {{ \"no\" }}));
  Channel::send(go, 1);
  Fiber::wait(holder);
  Fiber::wait(closer);
  print(\"closed after the lease ended\");
  0
}}
",
        quiet = a_quiet_server()
    );
    let exe = build("pool_lease_outlives_close", &main);
    for backend in ["threads", "scheduler"] {
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(60));
        assert!(!ran.hung, "{backend}: `close` never returned: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout,
            "before: 1\nclosed while the lease is out: no\nclosed after the lease ended\n",
            "{backend}"
        );
    }
}

// --- a reply cut off in the middle -------------------------------------------

/// How long the scripted server stalls in the middle of the first reply.
const STALL_MS: u64 = 1000;

/// The receive deadline the program puts on its connections: a tenth of the
/// stall, so even a loaded machine reads the stall as a failed read.
const DEADLINE_MS: u64 = 100;

/// One frontend message, or `None` once the driver has hung up.
fn next_frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut kind = [0u8; 1];
    stream.read_exact(&mut kind).ok()?;
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).ok()?;
    let mut payload = vec![0u8; (i32::from_be_bytes(length) as usize).checked_sub(4)?];
    stream.read_exact(&mut payload).ok()?;
    Some((kind[0], payload))
}

/// The statements a fake server's connection has been asked to parse, by
/// name, so a `Bind` of a prepared statement finds its SQL.
///
/// **What this prevents: a fake server that answers a prepared statement's
/// second run as if its SQL were empty.** The driver parses a statement once
/// per connection and afterwards sends only `Bind`, `Execute` and `Sync`, so
/// the SQL is in the `Parse` of an earlier request, not this one.
#[derive(Default)]
struct Statements(std::collections::HashMap<String, String>);

impl Statements {
    /// The SQL of a `Parse` payload (name, SQL, parameter types), remembered
    /// under its name.
    fn parse(&mut self, payload: &[u8]) -> String {
        let parts: Vec<&[u8]> = payload.split(|b| *b == 0).collect();
        let name = String::from_utf8_lossy(parts.first().copied().unwrap_or(&[])).into_owned();
        let sql = String::from_utf8_lossy(parts.get(1).copied().unwrap_or(&[])).into_owned();
        self.0.insert(name, sql.clone());
        sql
    }

    /// The SQL of the statement a `Bind` payload (portal, statement, ...)
    /// names, if this connection parsed it.
    fn bound(&self, payload: &[u8]) -> Option<String> {
        let name = payload.split(|b| *b == 0).nth(1)?;
        self.0.get(String::from_utf8_lossy(name).as_ref()).cloned()
    }
}

/// One backend message as bytes, so a reply can be cut wherever a test likes.
fn framed(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Answers `select N` with one `int4` row holding `N`, on every connection it
/// is given, until the driver hangs up.
///
/// **The first reply of all is cut in the middle of its `DataRow`**, and the
/// rest follows [`STALL_MS`] later on the same connection, the way a slow
/// server or a lost segment delivers it. Nothing is closed: whatever the
/// driver sends next on that connection is read and answered after the late
/// half.
///
/// Each reply sent whole is noted in `heard` as `answered N`, and the
/// connection that was cut notes `the cut connection ended` when the driver
/// hangs up on it, so a test can say in which order the two happened.
fn answer_numbers(
    mut stream: TcpStream,
    cut: &std::sync::atomic::AtomicBool,
    heard: &std::sync::Mutex<Vec<String>>,
) {
    let mut this_one_was_cut = false;
    answer_numbers_until_hung_up(&mut stream, cut, heard, &mut this_one_was_cut);
    if this_one_was_cut {
        heard.lock().expect("the notes").push("the cut connection ended".to_string());
    }
}

fn answer_numbers_until_hung_up(
    stream: &mut TcpStream,
    cut: &std::sync::atomic::AtomicBool,
    heard: &std::sync::Mutex<Vec<String>>,
    this_one_was_cut: &mut bool,
) {
    let _ = stream.set_nodelay(true);
    let mut length = [0u8; 4];
    if stream.read_exact(&mut length).is_err() {
        return;
    }
    let mut startup = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
    if stream.read_exact(&mut startup).is_err() {
        return;
    }
    let mut hello = framed(b'R', &0i32.to_be_bytes());
    hello.extend(framed(b'Z', b"I"));
    if stream.write_all(&hello).is_err() {
        return;
    }
    let mut statements = Statements::default();
    loop {
        // One request: the extended protocol's frames up to `Sync`, or one
        // simple `Query`. The SQL is in `Parse`, or in the `Parse` of an
        // earlier request for the statement `Bind` names.
        let mut sql = String::new();
        loop {
            let Some((kind, payload)) = next_frame(stream) else { return };
            let text = |skip: usize| {
                let parts: Vec<&[u8]> = payload.split(|b| *b == 0).collect();
                String::from_utf8_lossy(parts.get(skip).copied().unwrap_or(&[])).into_owned()
            };
            match kind {
                b'X' => return,
                b'P' => sql = statements.parse(&payload),
                b'B' => {
                    if let Some(known) = statements.bound(&payload) {
                        sql = known;
                    }
                }
                b'Q' => {
                    sql = text(0);
                    break;
                }
                b'S' => break,
                _ => {}
            }
        }
        let n = sql.trim().trim_start_matches("select ").trim().to_string();

        let mut description = 1i16.to_be_bytes().to_vec();
        description.extend_from_slice(&cstring("n"));
        description.extend_from_slice(&0i32.to_be_bytes());
        description.extend_from_slice(&0i16.to_be_bytes());
        description.extend_from_slice(&23i32.to_be_bytes());
        description.extend_from_slice(&4i16.to_be_bytes());
        description.extend_from_slice(&(-1i32).to_be_bytes());
        description.extend_from_slice(&0i16.to_be_bytes());
        let mut row = 1i16.to_be_bytes().to_vec();
        row.extend_from_slice(&(n.len() as i32).to_be_bytes());
        row.extend_from_slice(n.as_bytes());

        let mut reply = framed(b'T', &description);
        let middle = reply.len() + 3;
        reply.extend(framed(b'D', &row));
        reply.extend(framed(b'C', &cstring("SELECT 1")));
        reply.extend(framed(b'Z', b"I"));

        let sent = if cut.swap(true, std::sync::atomic::Ordering::SeqCst) {
            stream.write_all(&reply)
        } else {
            *this_one_was_cut = true;
            stream.write_all(&reply[..middle]).and_then(|_| {
                std::thread::sleep(std::time::Duration::from_millis(STALL_MS));
                stream.write_all(&reply[middle..])
            })
        };
        if sent.is_err() {
            return;
        }
        if !*this_one_was_cut || n != "1" {
            heard.lock().expect("the notes").push(format!("answered {n}"));
        }
    }
}

/// The cut-reply program, run against [`answer_numbers`] on `backend`: what
/// it printed, and what the server saw, in order.
///
/// A pool of two; the first reply of all is cut and stalled, with a receive
/// deadline much shorter than the stall. Asks 1, sleeps past the stall so the
/// late half has arrived, asks 2 to 6 one at a time, and closes the pool.
///
/// The deadline is set on every descriptor a connection could be given,
/// before the pool opens, because the driver does not hand its socket out.
fn run_the_cut_reply(name: &str, backend: &str) -> (Watched, Vec<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let cut = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = {
        let (cut, heard) = (cut.clone(), heard.clone());
        std::thread::spawn(move || {
            // A pool of two opens two connections.
            let mut served = Vec::new();
            for _ in 0..2 {
                let Ok((stream, _)) = listener.accept() else { break };
                let (cut, heard) = (cut.clone(), heard.clone());
                served.push(std::thread::spawn(move || answer_numbers(stream, &cut, &heard)));
            }
            for s in served {
                let _ = s.join();
            }
        })
    };
    let exe = build(&format!("{name}_{backend}"), &cut_reply_program().replace("PORT", &port.to_string()));
    let ran = run_watched(&exe, backend, std::time::Duration::from_secs(60));
    server.join().expect("the scripted server");
    let heard = heard.lock().expect("the notes").clone();
    (ran, heard)
}

fn cut_reply_program() -> String {
    format!(
        "module demo::main;
import std::core::{{Fibers, List, Result, print}};
import std::db::{{Cell, Db, DbError, Row}};
import postgres::db::{{Settings}};
import postgres::pool::{{Pool, Reconnect, close, open_with, with_db}};

extern fn khora_net_set_timeout(handle: I32, millis: Int) -> I32;
extern fn khora_sleep(millis: Int) -> ();

fn shown(rows: List<Row>) -> String {{
  match rows {{
    List::Nil => \"no rows\",
    List::Cons(row, _) => match row.cells {{
      List::Cons(Cell::Number(n), _) => Int::to_string(n),
      _ => \"a row of another shape\",
    }},
  }}
}}

fn ask(pool: Pool, n: Int) -> () {{
  let sql = \"select \" + Int::to_string(n);
  let said = match with_db(pool, fn () => db.query(sql, List::Nil)) {{
    Result::Err(_) => \"no connection\",
    Result::Ok(Result::Err(why)) => match why {{
      DbError::Disconnected(_) => \"disconnected\",
      DbError::Rejected(m) => \"rejected: \" + m,
      DbError::RolledBack(m) => \"rolled back: \" + m,
    }},
    Result::Ok(Result::Ok(rows)) => shown(rows),
  }};
  print(\"asked \" + Int::to_string(n) + \": \" + said);
}}

fn main() -> Int {{
  let mut fd = 3;
  while fd < 4096 {{
    khora_net_set_timeout(I32::of(fd), {DEADLINE_MS});
    fd = fd + 1
  }};
  let settings: Settings = {{
    host: \"127.0.0.1\", port: PORT, user: \"khora\", database: \"khora\", secret: \"\",
  }};
  let crew = Fibers::open();
  // No handshake bound. `open_within` bounds the handshake with a receive
  // deadline on the socket and clears it once the handshake is over, which
  // clears the deadline set above too: the stall is then waited out, every
  // caller gets its own answer, and this program no longer cuts anything.
  // Any harness that cuts replies by a deadline set before the pool opens
  // needs `handshake: 0` for the same reason.
  let usual = Reconnect::default();
  let pool = open_with(crew, settings, 2, {{ fast: usual.fast, slow: usual.slow, handshake: 0 }});
  ask(pool, 1);
  // Past the stall, so the late half of the first reply has arrived.
  khora_sleep({STALL_MS} + 500);
  let mut n = 2;
  while n <= 6 {{
    ask(pool, n);
    n = n + 1
  }};
  close(pool);
  print(\"closed\");
  0
}}
"
    )
}

/// **A reply cut off by a failed read never becomes the next caller's
/// answer.** The rest of that reply is still on its way when the read gives
/// up, so a driver that goes on using the connection hands it to whoever
/// asks next, and every later caller on that connection gets the answer
/// before theirs.
///
/// The first caller must be told `Disconnected`; after the late half has
/// arrived, every statement must get its own number, and `close` must
/// return. The cut connection is closed and its slot reconnected before it
/// is lent again, so no later caller is answered `Disconnected` either.
/// Before the crosstalk fix, `asked 3` got 1 and `asked 5` got 3; before
/// reconnect, both were answered `Disconnected`.
///
/// Not on Windows, where a socket is a handle and not a small number.
#[test]
fn a_reply_cut_off_mid_stream_is_never_another_callers_answer() {
    if cfg!(windows) {
        eprintln!("skipping: the deadline is set by descriptor number, and a Windows socket is a handle");
        return;
    }
    for backend in ["threads", "scheduler"] {
        let (ran, _) = run_the_cut_reply("postgres_cut_reply", backend);
        assert!(!ran.hung, "{backend}: the program hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout,
            "asked 1: disconnected\n\
             asked 2: 2\n\
             asked 3: 3\n\
             asked 4: 4\n\
             asked 5: 5\n\
             asked 6: 6\n\
             closed\n",
            "{backend}: a caller must get its own number, never another's, and a cut connection must be replaced"
        );
    }
}

/// **The connection whose reply was cut off is hung up at once, not kept
/// open until the pool closes.**
///
/// Refusing it is not the same as dropping it. A connection that answers
/// every later request `Disconnected` but is never closed holds a socket, a
/// serving fiber and a server backend for the life of the pool, and the
/// server goes on writing the rest of a reply nobody will read. So the
/// server here must see the cut connection end before it is asked anything
/// else: the driver learns the reply was cut at the deadline, 100 ms in, and
/// the next request comes 1.5 s later.
///
/// With the connection only refused, the hang-up came when the pool closed,
/// after every other answer.
#[test]
fn a_connection_whose_reply_was_cut_off_is_closed_straight_away() {
    if cfg!(windows) {
        eprintln!("skipping: the deadline is set by descriptor number, and a Windows socket is a handle");
        return;
    }
    for backend in ["threads", "scheduler"] {
        let (ran, heard) = run_the_cut_reply("postgres_cut_closed", backend);
        assert!(!ran.hung, "{backend}: the program hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            heard.first().map(String::as_str),
            Some("the cut connection ended"),
            "{backend}: the server answered something before the driver hung up on the cut connection: {heard:?}"
        );
    }
}

// --- a request bigger than the socket buffer -------------------------------

/// Answers each extended-protocol request with one `int4` row: the length of
/// its first bound parameter, or 7 when it has none.
///
/// **It reads every byte of every frame before it answers**, the way a real
/// server does, and gives up after ten seconds of silence. A driver that sent
/// part of a frame therefore gets no answer, and then a closed connection.
fn answer_parameter_lengths(mut stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let mut length = [0u8; 4];
    if stream.read_exact(&mut length).is_err() {
        return;
    }
    let mut startup = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
    if stream.read_exact(&mut startup).is_err() {
        return;
    }
    let mut hello = framed(b'R', &0i32.to_be_bytes());
    hello.extend(framed(b'Z', b"I"));
    if stream.write_all(&hello).is_err() {
        return;
    }
    loop {
        let mut answer = 7usize;
        loop {
            let Some((kind, payload)) = next_frame(&mut stream) else { return };
            match kind {
                b'X' => return,
                // Bind: portal, statement, the format codes, then the values,
                // each a length and its bytes.
                b'B' => {
                    let mut at = payload.iter().position(|b| *b == 0).map_or(0, |p| p + 1);
                    at += payload[at..].iter().position(|b| *b == 0).map_or(0, |p| p + 1);
                    let formats = i16::from_be_bytes([payload[at], payload[at + 1]]) as usize;
                    at += 2 + 2 * formats;
                    let values = i16::from_be_bytes([payload[at], payload[at + 1]]);
                    at += 2;
                    if values > 0 {
                        let bytes = &payload[at..at + 4];
                        answer = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
                    }
                }
                b'S' => break,
                _ => {}
            }
        }
        let n = answer.to_string();
        let mut description = 1i16.to_be_bytes().to_vec();
        description.extend_from_slice(&cstring("n"));
        description.extend_from_slice(&0i32.to_be_bytes());
        description.extend_from_slice(&0i16.to_be_bytes());
        description.extend_from_slice(&23i32.to_be_bytes());
        description.extend_from_slice(&4i16.to_be_bytes());
        description.extend_from_slice(&(-1i32).to_be_bytes());
        description.extend_from_slice(&0i16.to_be_bytes());
        let mut row = 1i16.to_be_bytes().to_vec();
        row.extend_from_slice(&(n.len() as i32).to_be_bytes());
        row.extend_from_slice(n.as_bytes());
        let mut reply = framed(b'T', &description);
        reply.extend(framed(b'D', &row));
        reply.extend(framed(b'C', &cstring("SELECT 1")));
        reply.extend(framed(b'Z', b"I"));
        if stream.write_all(&reply).is_err() {
            return;
        }
    }
}

/// **A parameter bigger than the socket buffer arrives whole.**
///
/// A non-blocking `send` takes what fits in the kernel's buffer, about
/// 2.6 MB on Linux loopback, and reports that count. The driver counted any
/// count that was not negative as the whole request, so a 4 MiB parameter
/// went out as its first 2.6 MB. The server waited for the rest of the frame
/// and the driver waited for its reply: a hang, or with a receive deadline a
/// `Disconnected`.
///
/// A pool of one, a 4 MiB text parameter, then a small query on the same
/// connection. The first must come back as its own length and the second as
/// 7. Both backends.
#[test]
fn a_parameter_larger_than_the_socket_buffer_goes_whole() {
    let main = "module demo::main;
import std::core::{Fibers, List, Result, print};
import std::db::{Cell, Db, DbError, Row};
import postgres::db::{Settings};
import postgres::pool::{Pool, close, open, with_db};

fn said(r: Result<Result<List<Row>, DbError>, DbError>) -> String {
  match r {
    Result::Err(_) => \"no connection\",
    Result::Ok(Result::Err(why)) => match why {
      DbError::Disconnected(m) => \"disconnected: \" + m,
      DbError::Rejected(m) => \"rejected: \" + m,
      DbError::RolledBack(m) => \"rolled back: \" + m,
    },
    Result::Ok(Result::Ok(rows)) => match rows {
      List::Cons(row, _) => match row.cells {
        List::Cons(Cell::Number(n), _) => Int::to_string(n),
        _ => \"a row of another shape\",
      },
      List::Nil => \"no rows\",
    },
  }
}

fn big(n: Int) -> String {
  let mut s = \"x\";
  while String::byte_length(s) < n {
    s = s + s
  };
  s
}

fn main() -> Int {
  let settings: Settings = {
    host: \"127.0.0.1\", port: PORT, user: \"khora\", database: \"khora\", secret: \"\",
  };
  let crew = Fibers::open();
  let pool = open(crew, settings, 1);
  let payload = big(4194304);
  print(\"big: \" + said(with_db(pool, fn () =>
    db.query(\"select length($1)\", List::Cons(Cell::Text(payload), List::Nil)))));
  print(\"then: \" + said(with_db(pool, fn () => db.query(\"select 7\", List::Nil))));
  close(pool);
  print(\"closed\");
  0
}
";
    for backend in ["threads", "scheduler"] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let server = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                answer_parameter_lengths(stream);
            }
        });
        let exe = build(
            &format!("postgres_big_parameter_{backend}"),
            &main.replace("PORT", &port.to_string()),
        );
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(60));
        server.join().expect("the scripted server");
        assert!(!ran.hung, "{backend}: the program hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout, "big: 4194304\nthen: 7\nclosed\n",
            "{backend}: a request bigger than the socket buffer must reach the server whole"
        );
    }
}

// --- a cancel between `BEGIN` and the rollback -----------------------------
//
// **What these prevent: a write answered `Ok` that is never committed.** A
// fiber canceled after `transaction` sent `BEGIN` -- including while it
// waits for the `BEGIN`'s own reply -- must not unwind with the server inside
// a transaction and nothing registered to end it. Otherwise `with_db` hands
// the connection back that way, and the next borrower's autocommit
// statements run inside the leftover transaction: answered `Ok`, and rolled
// back when that transaction ends.

/// What [`record_queries`] heard, and how the conversation ended.
struct Recorded {
    /// Every simple query, in the order it arrived.
    heard: Vec<String>,
    /// `Terminate` for a clean close; otherwise the I/O error or the message
    /// this server has no answer for, as text.
    ended: String,
}

/// A server that records every simple query and answers each one, holding
/// its answer to the first `BEGIN` until the program says it has canceled.
///
/// **What the hold is for: the cancel has to land while `BEGIN` waits for
/// its reply**, so that the server is inside a transaction the fiber was never
/// told about. The server tells the program on `control` when `BEGIN` has
/// arrived; the program cancels, says so on `control`, and only then is
/// `BEGIN` answered. It used to stall the reply a fixed second and have the
/// program cancel after a fixed 200 ms; on a slow macOS runner the whole
/// transaction was over before the cancel, and the server heard `BEGIN`,
/// `COMMIT`.
///
/// The reply cannot wait for the rollback instead: the pool's serving fiber
/// owns the socket and sends nothing more until `BEGIN` is answered.
///
/// **Never panics once it has a connection.** A panic in this thread reached
/// the test only as "the server: Any", which named neither the error nor what
/// had been said before it. Every failure ends the conversation instead, and
/// is reported in [`Recorded::ended`] beside the queries heard up to it.
fn record_queries(listener: TcpListener, control: TcpListener) -> Recorded {
    let (mut stream, _) = listener.accept().expect("a connection");
    let mut heard = Vec::new();
    let ended = match converse(&mut stream, Hold::UntilSpokenTo(control), "BEGIN", &mut heard) {
        Ok(()) => "Terminate".to_string(),
        Err(why) => why,
    };
    Recorded { heard, ended }
}

/// How [`converse`] holds its answer to the statement it was told to hold.
enum Hold {
    /// Sleep this long, then answer.
    For(std::time::Duration),
    /// Say `B` on the first connection to this listener, then answer only
    /// once the program has written a byte back on it.
    UntilSpokenTo(TcpListener),
}

/// [`record_queries`], stalling the first `statement` a fixed time rather
/// than holding the first `BEGIN`.
fn record_queries_stalling(
    listener: TcpListener,
    stall: std::time::Duration,
    statement: &str,
) -> Recorded {
    let (mut stream, _) = listener.accept().expect("a connection");
    let mut heard = Vec::new();
    let ended = match converse(&mut stream, Hold::For(stall), statement, &mut heard) {
        Ok(()) => "Terminate".to_string(),
        Err(why) => why,
    };
    Recorded { heard, ended }
}

/// Reads one frontend message, naming the step that failed.
fn next_message(stream: &mut TcpStream) -> Result<(u8, Vec<u8>), String> {
    let step = |what: &'static str| move |e: std::io::Error| format!("{what}: {e}");
    let mut kind = [0u8; 1];
    stream.read_exact(&mut kind).map_err(step("reading a message type"))?;
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).map_err(step("reading a message length"))?;
    let mut payload = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
    stream.read_exact(&mut payload).map_err(step("reading a message payload"))?;
    Ok((kind[0], payload))
}

/// The body of [`record_queries`], with every failure as an `Err` naming
/// the step it happened at.
fn converse(
    stream: &mut TcpStream,
    hold: Hold,
    statement: &str,
    heard: &mut Vec<String>,
) -> Result<(), String> {
    let step = |what: &'static str| move |e: std::io::Error| format!("{what}: {e}");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .map_err(step("setting a read deadline"))?;
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).map_err(step("reading the startup length"))?;
    let mut startup = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
    stream.read_exact(&mut startup).map_err(step("reading the startup payload"))?;
    send(stream, b'R', &0i32.to_be_bytes()).map_err(step("writing AuthenticationOk"))?;
    send(stream, b'Z', b"I").map_err(step("writing the first ReadyForQuery"))?;

    let mut hold = Some(hold);
    loop {
        let (kind, payload) = next_message(stream)?;
        match kind {
            b'Q' => {
                let sql = String::from_utf8_lossy(payload.strip_suffix(&[0]).unwrap_or(&payload))
                    .into_owned();
                heard.push(sql.clone());
                if sql == statement {
                    match hold.take() {
                        None => {}
                        Some(Hold::For(stall)) => std::thread::sleep(stall),
                        Some(Hold::UntilSpokenTo(control)) => {
                            let (mut told, _) = control.accept().map_err(step("accepting the control connection"))?;
                            told.set_read_timeout(Some(std::time::Duration::from_secs(20)))
                                .map_err(step("setting the control deadline"))?;
                            told.write_all(b"B").map_err(step("saying BEGIN has arrived"))?;
                            let mut canceled = [0u8; 1];
                            told.read_exact(&mut canceled).map_err(step("waiting to hear the cancel was delivered"))?;
                        }
                    }
                }
                send(stream, b'C', &cstring(&sql)).map_err(step("writing CommandComplete"))?;
                send(stream, b'Z', b"I").map_err(step("writing ReadyForQuery"))?;
            }
            // 'X' Terminate: the pool closed.
            b'X' => return Ok(()),
            other => return Err(format!("the driver sent {:?}, which this server does not answer", other as char)),
        }
    }
}

/// [`write_message`], answering the write's error instead of panicking on it.
fn send(stream: &mut TcpStream, kind: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut out = vec![kind];
    out.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    out.extend_from_slice(payload);
    stream.write_all(&out)
}

/// **A cancel while `BEGIN` waits for its reply is followed by a `ROLLBACK`
/// on the wire, before the connection's next borrower speaks.**
///
/// No real server: the fake one holds its answer to the first `BEGIN` until
/// the program has canceled the transaction's fiber, and the program then
/// runs one more transaction on the same pooled connection. What the server
/// heard, in order, is the assertion.
#[test]
fn a_cancel_while_begin_is_answered_puts_a_rollback_on_the_wire() {
    // The fake answers one connection, and the port is in the program, so
    // each backend gets a server and a build of its own.
    for backend in ["threads", "scheduler"] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let control = TcpListener::bind("127.0.0.1:0").expect("a control port");
        let told = control.local_addr().expect("an address").port();
        let server = std::thread::spawn(move || record_queries(listener, control));
        // The port is in the name as well, so two copies of this test running
        // at once (a stress run) never compile to one path.
        let exe = build(&format!("pg_cancel_in_begin_{backend}_{port}"), &cancel_in_begin_program(port, told));
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(30));
        let recorded = server.join().expect("the server thread does not panic once connected");
        assert!(!ran.hung, "{backend}: the program hung: {:?}", ran.stdout);
        assert_eq!(
            ran.stdout, "the next transaction committed\n",
            "{backend}: stderr {}; the server heard {:?} and ended with {}",
            ran.stderr, recorded.heard, recorded.ended
        );
        assert_eq!(
            (recorded.heard.as_slice(), recorded.ended.as_str()),
            (["BEGIN", "ROLLBACK", "BEGIN", "COMMIT"].map(String::from).as_slice(), "Terminate"),
            "{backend}: a transaction canceled while its BEGIN was answered must roll back \
             before the connection is lent again, and the pool must close the connection cleanly"
        );
    }
}

/// The program for [`a_cancel_while_begin_is_answered_puts_a_rollback_on_the_wire`]:
/// cancel a transaction once the server says its `BEGIN` has arrived, then
/// run another on the same pooled connection.
fn cancel_in_begin_program(port: u16, told: u16) -> String {
    format!(
        "module demo::main;
import std::core::{{Array, Fiber, Fibers, Result, print}};
import std::db::{{Db, DbError, transaction}};
import std::net::socket::{{start, connect_to, receive, transmit, shut}};
import postgres::db::{{Settings}};
import postgres::pool::{{Pool, close, open, with_db}};

fn empty() -> Result<Int, DbError> with {{ db: Db }} {{
  transaction(fn () => Result::Ok(1))
}}

fn main() -> Int {{
  let settings: Settings = {{ host: \"127.0.0.1\", port: {port}, user: \"khora\", database: \"khora\", secret: \"\" }};
  if start() {{}} else {{ print(\"no sockets\") }};
  let crew = Fibers::open();
  let pool = open(crew, settings, 1);
  let f = Fiber::spawn(fn () => {{ let _ = with_db(pool, empty); () }});
  let control = connect_to(\"127.0.0.1\", {told});
  let one: Array<U8> = Array::new(1, 0);
  let _ = receive(control, one);
  Fiber::cancel(f);
  let _ = transmit(control, \"c\");
  shut(control);
  Fiber::wait(f);
  match with_db(pool, empty) {{
    Result::Ok(_) => print(\"the next transaction committed\"),
    Result::Err(_) => print(\"the next transaction failed\"),
  }};
  close(pool);
  0
}}
"
    )
}

/// The program for [`a_canceled_transaction_never_costs_a_write_against_a_real_server`].
///
/// Two phases on a pool of one, each 100 trials of: a fiber looping
/// `with_db(transaction(..))`, canceled after 1 to 4 ms.
///
/// - **Left open.** Then `SAVEPOINT` on the pool's connection, which the
///   server refuses outside a transaction block: accepted means the
///   connection came back inside one.
/// - **Lost.** Then an autocommit `insert` answered `Ok`, and a transaction
///   whose body fails. Inside a leftover transaction the failed one's
///   `ROLLBACK` takes the insert with it, so every leftover costs a row.
///   Afterwards the pool is closed and a second pool counts the rows.
const LOST_WRITES: &str = r#"module demo::main;
import std::core::{Fiber, Fibers, List, Result, print};
import std::db::{Cell, Db, DbError, Row, transaction};
import postgres::db::{Settings};
import postgres::pool::{Pool, close, open, with_db};

extern fn khora_sleep(millis: Int) -> ();

fn empty() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => Result::Ok(1))
}

fn churn(pool: Pool) -> () {
  let mut going = true;
  while going {
    let _ = with_db(pool, empty);
    ()
  }
}

fn canceled_churn(pool: Pool, trial: Int) -> () {
  let f = Fiber::spawn(fn () => churn(pool));
  khora_sleep(1 + trial % 4);
  Fiber::cancel(f);
  Fiber::wait(f);
}

fn left_open() -> Int with { db: Db } {
  match db.execute("savepoint tx_gap_probe", List::Nil) {
    Result::Ok(_) => { let _ = db.rollback(); 1 },
    Result::Err(_) => 0,
  }
}

fn insert(n: Int) -> Int with { db: Db } {
  match db.execute("insert into tx_gap_regression (n) values ($1)", List::Cons(Cell::Number(n), List::Nil)) {
    Result::Ok(_) => 1,
    Result::Err(_) => 0,
  }
}

fn refused() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => Result::Err(DbError::Rejected("the body failed")))
}

fn fresh_table() -> () with { db: Db } {
  let _ = db.execute("drop table if exists tx_gap_regression", List::Nil);
  let _ = db.execute("create table tx_gap_regression (n int4)", List::Nil);
}

fn counted() -> Int with { db: Db } {
  match db.query("select count(*)::int4 from tx_gap_regression", List::Nil) {
    Result::Ok(List::Cons(row, _)) => match row.cells {
      List::Cons(Cell::Number(n), _) => n,
      _ => 0 - 1,
    },
    _ => 0 - 1,
  }
}

fn main() -> Int {
  let settings: Settings = { host: "127.0.0.1", port: 5433, user: "khora", database: "khora", secret: "khora" };
  let crew = Fibers::open();
  let pool = open(crew, settings, 1);
  let _ = with_db(pool, fresh_table);

  let mut trial = 0;
  let mut open_after = 0;
  while trial < 100 {
    canceled_churn(pool, trial);
    open_after = open_after + match with_db(pool, left_open) { Result::Ok(n) => n, Result::Err(_) => 0 };
    trial = trial + 1
  };

  trial = 0;
  let mut told_ok = 0;
  while trial < 100 {
    canceled_churn(pool, trial);
    told_ok = told_ok + match with_db(pool, fn () => insert(trial)) { Result::Ok(n) => n, Result::Err(_) => 0 };
    let _ = with_db(pool, refused);
    trial = trial + 1
  };
  close(pool);

  let crew2 = Fibers::open();
  let again = open(crew2, settings, 1);
  let there = match with_db(again, counted) { Result::Ok(n) => n, Result::Err(_) => 0 - 2 };
  close(again);
  print("left open " + Int::to_string(open_after) + " of 100");
  print("writes answered Ok " + Int::to_string(told_ok) + ", present " + Int::to_string(there));
  0
}
"#;

/// **A canceled transaction never hands its connection back inside the
/// transaction, and never costs a later caller a write it was told had
/// succeeded.** The regression test for a cancel landing between `BEGIN`
/// and the rollback's registration, against the server that has to believe
/// it; [`LOST_WRITES`] says how each is observed.
///
/// Skipped without `KHORA_POSTGRES`, like its neighbors.
#[test]
fn a_canceled_transaction_never_costs_a_write_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }
    let exe = build("pg_lost_writes", LOST_WRITES);
    // Both backends run before anything is asserted, so a failure shows both.
    let ran: Vec<(&str, Watched)> = ["threads", "scheduler"]
        .into_iter()
        .map(|backend| (backend, run_watched(&exe, backend, std::time::Duration::from_secs(120))))
        .collect();
    let seen: Vec<String> =
        ran.iter().map(|(backend, r)| format!("{backend}: {:?}, hung {}", r.stdout, r.hung)).collect();
    for (backend, ran) in &ran {
        assert!(!ran.hung, "{backend} hung: {seen:?}");
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout, "left open 0 of 100\nwrites answered Ok 100, present 100\n",
            "a canceled transaction must neither leave its connection inside a transaction \
             nor cost a later write: {seen:?}"
        );
    }
}

/// **The driver answers a `ROLLBACK` with no transaction open with `Ok`.**
///
/// What this prevents: a pool losing a healthy connection on every cancel
/// that lands before `transaction`'s `BEGIN` goes out. `transaction`
/// registers its rollback first, so such a cancel sends a `ROLLBACK` the
/// server has no transaction for. PostgreSQL answers it with a warning
/// (`NoticeResponse`) and `CommandComplete`, not an error. A driver that read
/// the warning as a failure would make `undo` call `broken`, and the pool
/// would close the connection.
///
/// Checked twice on one connection, with a statement between them, so the
/// connection is shown still usable after the stray rollback. Skipped
/// without `KHORA_POSTGRES`, like its neighbors.
#[test]
fn a_rollback_with_no_transaction_open_is_ok_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }
    let main = format!(
        "module demo::main;
import std::core::{{Fibers, List, Result, print}};
import std::db::{{Db, DbError}};
import postgres::db::{{Settings}};
import postgres::pool::{{close, open, with_db}};

fn said(answer: Result<(), DbError>) -> String {{
  match answer {{
    Result::Ok(_) => \"ok\",
    Result::Err(problem) => problem.show(),
  }}
}}

fn stray() -> () with {{ db: Db }} {{
  print(\"first stray rollback: \" + said(db.rollback()));
  match db.query(\"select 1\", List::Nil) {{
    Result::Ok(_) => print(\"the connection still answers\"),
    Result::Err(problem) => print(\"the connection failed: \" + problem.show()),
  }};
  print(\"second stray rollback: \" + said(db.rollback()));
}}

fn main() -> Int {{
  let settings: Settings = {REAL};
  let crew = Fibers::open();
  let pool = open(crew, settings, 1);
  match with_db(pool, stray) {{
    Result::Ok(_) => (),
    Result::Err(problem) => print(\"no lease: \" + problem.show()),
  }};
  close(pool);
  0
}}
"
    );
    let exe = build("pg_stray_rollback", &main);
    for backend in ["threads", "scheduler"] {
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(30));
        assert!(!ran.hung, "{backend}: the program hung: {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout,
            "first stray rollback: ok\nthe connection still answers\nsecond stray rollback: ok\n",
            "{backend}: a ROLLBACK with no transaction open must be answered Ok"
        );
    }
}

// --- reconnect, shrink and grow back ----------------------------------------
//
// **What these prevent: a pool that goes on lending a connection it has
// lost, and a pool that hangs its callers once it has none.** Each program
// drives [`Scripted`] with statements it understands as commands, so the
// order of events is the program's own and no test sleeps on the scheduler's
// timing: `kill others` hangs up every other connection, and `down N` hangs
// up every connection and turns new ones away for `N` ms (`-1`: for good).

/// A fake server a program can break on purpose. See the section comment.
struct Scripted {
    port: u16,
    state: std::sync::Arc<ScriptedState>,
}

struct ScriptedState {
    /// Until when connections are turned away: accepted, their startup read,
    /// and then closed, the way a server that is restarting answers.
    refusing_until: std::sync::Mutex<Option<std::time::Instant>>,
    /// Until when new connections finish the handshake and are then sent a
    /// `DataRow` nobody asked for, so every check before a lease fails.
    sour_until: std::sync::Mutex<Option<std::time::Instant>>,
    /// Every connection being served, to hang up on.
    open: std::sync::Mutex<Vec<(usize, TcpStream)>>,
    /// When each connection attempt arrived, and whether it was turned away.
    attempts: std::sync::Mutex<Vec<(std::time::Instant, bool)>>,
    next: std::sync::atomic::AtomicUsize,
}

impl Scripted {
    fn start() -> Scripted {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let state = std::sync::Arc::new(ScriptedState {
            refusing_until: std::sync::Mutex::new(None),
            sour_until: std::sync::Mutex::new(None),
            open: std::sync::Mutex::new(Vec::new()),
            attempts: std::sync::Mutex::new(Vec::new()),
            next: std::sync::atomic::AtomicUsize::new(0),
        });
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let now = std::time::Instant::now();
                let refusing = shared
                    .refusing_until
                    .lock()
                    .expect("the switch")
                    .is_some_and(|until| now < until);
                shared.attempts.lock().expect("the attempts").push((now, refusing));
                let state = shared.clone();
                if refusing {
                    std::thread::spawn(move || turn_away(stream));
                } else {
                    std::thread::spawn(move || state.serve(stream));
                }
            }
        });
        Scripted { port, state }
    }

    fn settings(&self) -> String {
        format!(
            "{{ host: \"127.0.0.1\", port: {}, user: \"khora\", database: \"khora\", secret: \"\" }}",
            self.port
        )
    }

    /// The gaps between connection attempts that were turned away, in order.
    fn refused_gaps(&self) -> Vec<std::time::Duration> {
        let attempts = self.state.attempts.lock().expect("the attempts");
        let refused: Vec<std::time::Instant> =
            attempts.iter().filter(|(_, turned)| *turned).map(|(at, _)| *at).collect();
        refused.windows(2).map(|w| w[1] - w[0]).collect()
    }
}

/// Reads the startup message, so the driver's one write lands, and hangs up.
fn turn_away(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let mut length = [0u8; 4];
    if stream.read_exact(&mut length).is_ok() {
        let mut startup = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
        let _ = stream.read_exact(&mut startup);
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

impl ScriptedState {
    fn serve(&self, mut stream: TcpStream) {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = stream.set_nodelay(true);
        if let Ok(clone) = stream.try_clone() {
            self.open.lock().expect("the connections").push((id, clone));
        }
        let mut length = [0u8; 4];
        if stream.read_exact(&mut length).is_err() {
            return;
        }
        let mut startup = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
        if stream.read_exact(&mut startup).is_err() {
            return;
        }
        let mut hello = framed(b'R', &0i32.to_be_bytes());
        hello.extend(framed(b'Z', b"I"));
        let sour = self
            .sour_until
            .lock()
            .expect("the switch")
            .is_some_and(|until| std::time::Instant::now() < until);
        if sour {
            // Not kept in `open`: thousands of these arrive, and each is
            // over when the pool hangs up on it.
            self.open.lock().expect("the connections").retain(|(other, _)| *other != id);
            hello.extend(framed(b'D', &[0, 1, 0, 0, 0, 2, b'9', b'9']));
            let _ = stream.write_all(&hello);
            while next_frame(&mut stream).is_some() {}
            return;
        }
        if stream.write_all(&hello).is_err() {
            return;
        }
        let mut statements = Statements::default();
        loop {
            let mut sql = String::new();
            loop {
                let Some((kind, payload)) = next_frame(&mut stream) else { return };
                let text = |skip: usize| {
                    let parts: Vec<&[u8]> = payload.split(|b| *b == 0).collect();
                    String::from_utf8_lossy(parts.get(skip).copied().unwrap_or(&[])).into_owned()
                };
                match kind {
                    b'X' => return,
                    b'P' => sql = statements.parse(&payload),
                    b'B' => {
                        if let Some(known) = statements.bound(&payload) {
                            sql = known;
                        }
                    }
                    b'Q' => {
                        sql = text(0);
                        break;
                    }
                    b'S' => break,
                    _ => {}
                }
            }
            let sql = sql.trim().to_string();
            let (n, then) = if sql == "kill others" {
                ("0".to_string(), Then::KillOthers)
            } else if sql.trim() == "rst others" {
                ("0".to_string(), Then::ResetOthers)
            } else if sql.trim() == "notify others" {
                ("0".to_string(), Then::NotifyOthers)
            } else if sql.trim() == "error others" {
                ("0".to_string(), Then::ErrorOthers)
            } else if sql.trim() == "half-error others" {
                ("0".to_string(), Then::HalfErrorOthers)
            } else if sql.trim() == "conns" {
                let accepted = self.attempts.lock().expect("the attempts").len();
                (accepted.to_string(), Then::Nothing)
            } else if let Some(millis) = sql.strip_prefix("sour ") {
                let millis: u64 = millis.trim().parse().unwrap_or(0);
                ("0".to_string(), Then::Sour(millis))
            } else if let Some(millis) = sql.strip_prefix("down ") {
                let millis: i64 = millis.trim().parse().unwrap_or(-1);
                ("0".to_string(), Then::Down(millis))
            } else if let Some(millis) = sql.strip_prefix("stall ") {
                // Holds the lease: the answer comes `millis` later.
                let millis: u64 = millis.trim().parse().unwrap_or(0);
                std::thread::sleep(std::time::Duration::from_millis(millis));
                ("0".to_string(), Then::Nothing)
            } else {
                (sql.trim_start_matches("select ").trim().to_string(), Then::Nothing)
            };
            if stream.write_all(&number_reply(&n)).is_err() {
                return;
            }
            match then {
                Then::Nothing => {}
                Then::KillOthers => self.hang_up(|other| other != id),
                Then::Sour(millis) => {
                    *self.sour_until.lock().expect("the switch") =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(millis));
                    self.hang_up(|_| true);
                    return;
                }
                Then::NotifyOthers => {
                    let mut unasked = Vec::new();
                    // `NotificationResponse`: sender pid, channel, payload.
                    let mut note = 4242i32.to_be_bytes().to_vec();
                    note.extend_from_slice(&cstring("jobs"));
                    note.extend_from_slice(&cstring("ready"));
                    unasked.extend(framed(b'A', &note));
                    // `NoticeResponse`: severity and message fields, then 0.
                    let mut notice = vec![b'S'];
                    notice.extend_from_slice(&cstring("NOTICE"));
                    notice.push(b'M');
                    notice.extend_from_slice(&cstring("a notice nobody asked for"));
                    notice.push(0);
                    unasked.extend(framed(b'N', &notice));
                    let mut open = self.open.lock().expect("the connections");
                    for (other, stream) in open.iter_mut() {
                        if *other != id {
                            let _ = stream.write_all(&unasked);
                        }
                    }
                }
                Then::HalfErrorOthers => {
                    // The first 9 bytes of a `FATAL`: the kind, the length and
                    // a few bytes of the fields. The rest, and the close, come
                    // 1.5 s later, long after the check has looked.
                    let mut error = vec![b'S'];
                    error.extend_from_slice(&cstring("FATAL"));
                    error.push(b'M');
                    error.extend_from_slice(&cstring("terminating connection due to administrator command"));
                    error.push(0);
                    let whole = framed(b'E', &error);
                    let open: Vec<TcpStream> = self
                        .open
                        .lock()
                        .expect("the connections")
                        .iter()
                        .filter(|(other, _)| *other != id)
                        .filter_map(|(_, stream)| stream.try_clone().ok())
                        .collect();
                    for mut stream in open {
                        let _ = stream.write_all(&whole[..9]);
                        let rest = whole[9..].to_vec();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(1500));
                            let _ = stream.write_all(&rest);
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        });
                    }
                }
                Then::ErrorOthers => {
                    // The `FATAL` a server sends before it ends a backend, with
                    // the socket left open, so only the message says so.
                    let mut error = vec![b'S'];
                    error.extend_from_slice(&cstring("FATAL"));
                    error.push(b'M');
                    error.extend_from_slice(&cstring("terminating connection due to administrator command"));
                    error.push(0);
                    let unasked = framed(b'E', &error);
                    let mut open = self.open.lock().expect("the connections");
                    for (other, stream) in open.iter_mut() {
                        if *other != id {
                            let _ = stream.write_all(&unasked);
                        }
                    }
                }
                Then::ResetOthers => {
                    let mut open = self.open.lock().expect("the connections");
                    for (other, stream) in open.iter() {
                        if *other != id {
                            reset(stream);
                        }
                    }
                    open.retain(|(other, _)| *other == id);
                }
                Then::Down(millis) => {
                    let until = if millis < 0 {
                        std::time::Instant::now() + std::time::Duration::from_secs(86_400)
                    } else {
                        std::time::Instant::now() + std::time::Duration::from_millis(millis as u64)
                    };
                    *self.refusing_until.lock().expect("the switch") = Some(until);
                    self.hang_up(|_| true);
                    return;
                }
            }
        }
    }

    fn hang_up(&self, which: impl Fn(usize) -> bool) {
        let mut open = self.open.lock().expect("the connections");
        for (id, stream) in open.iter() {
            if which(*id) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        open.retain(|(id, _)| !which(*id));
    }
}

/// What the scripted server does after answering a statement.
enum Then {
    Nothing,
    KillOthers,
    /// Every other connection is reset: `RST`, not `FIN`.
    ResetOthers,
    /// Every other connection is sent a notification and a notice, unasked.
    NotifyOthers,
    /// Every other connection is sent a `FATAL` `ErrorResponse`, unasked, and
    /// left open.
    ErrorOthers,
    /// Every other connection is sent the first 9 bytes of a `FATAL`, and
    /// the rest with a hang-up 1.5 s later.
    HalfErrorOthers,
    /// For `N` ms, every new connection fails its check; then this one hangs up.
    Sour(u64),
    Down(i64),
}

/// Closes `stream` with an `RST`: zero linger, then the last handle goes.
///
/// The standard library's `set_linger` is unstable, so this sets it the way a
/// server that aborts connections does.
#[cfg(unix)]
fn reset(stream: &std::net::TcpStream) {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct Linger {
        onoff: std::ffi::c_int,
        linger: std::ffi::c_int,
    }
    unsafe extern "C" {
        fn setsockopt(
            fd: std::ffi::c_int,
            level: std::ffi::c_int,
            name: std::ffi::c_int,
            value: *const std::ffi::c_void,
            length: u32,
        ) -> std::ffi::c_int;
    }
    #[cfg(target_os = "linux")]
    const SOL_SOCKET: std::ffi::c_int = 1;
    #[cfg(target_os = "linux")]
    const SO_LINGER: std::ffi::c_int = 13;
    #[cfg(not(target_os = "linux"))]
    const SOL_SOCKET: std::ffi::c_int = 0xffff;
    #[cfg(not(target_os = "linux"))]
    const SO_LINGER: std::ffi::c_int = 0x0080;
    let linger = Linger { onoff: 1, linger: 0 };
    // SAFETY: `stream` owns an open socket for the length of this call, and
    // `linger` is a live `struct linger` whose size is the length passed.
    unsafe {
        setsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_LINGER,
            (&raw const linger).cast(),
            std::mem::size_of::<Linger>() as u32,
        );
    }
    // Shutting the read side sends nothing; it wakes the serving thread's
    // read, which returns, and its close then sends the `RST` the zero linger
    // asks for, since the clone here is dropped too.
    let _ = stream.shutdown(std::net::Shutdown::Read);
}

/// Not built: the tests that use it are Unix-only.
#[cfg(not(unix))]
fn reset(stream: &std::net::TcpStream) {
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// One `int4` row holding `n`, and the end of the exchange.
fn number_reply(n: &str) -> Vec<u8> {
    let mut description = 1i16.to_be_bytes().to_vec();
    description.extend_from_slice(&cstring("n"));
    description.extend_from_slice(&0i32.to_be_bytes());
    description.extend_from_slice(&0i16.to_be_bytes());
    description.extend_from_slice(&23i32.to_be_bytes());
    description.extend_from_slice(&4i16.to_be_bytes());
    description.extend_from_slice(&(-1i32).to_be_bytes());
    description.extend_from_slice(&0i16.to_be_bytes());
    let mut row = 1i16.to_be_bytes().to_vec();
    row.extend_from_slice(&(n.len() as i32).to_be_bytes());
    row.extend_from_slice(n.as_bytes());
    let mut reply = framed(b'T', &description);
    reply.extend(framed(b'D', &row));
    reply.extend(framed(b'C', &cstring("SELECT 1")));
    reply.extend(framed(b'Z', b"I"));
    reply
}

/// What every reconnect program starts with: `ask`, which answers a
/// statement's number or the error as text, and `until`, a clock-poll for a
/// pool state with a deadline.
const RECONNECT_PRELUDE: &str = "module demo::main;
import std::core::{Channel, Fiber, Fibers, List, Option, Result, print};
import std::db::{Cell, Db, DbError, Row};
import std::resilience::{Schedule};
import postgres::db::{Settings};
import postgres::pool::{Health, Pool, Reconnect, close, health, open, open_with, with_db, idle_count};

extern fn khora_sleep(millis: Int) -> ();
extern fn khora_monotonic_millis() -> Int;

fn now() -> Int { khora_monotonic_millis() }

fn shown(rows: List<Row>) -> String {
  match rows {
    List::Cons(row, _) => match row.cells {
      List::Cons(Cell::Number(n), _) => Int::to_string(n),
      _ => \"a row of another shape\",
    },
    List::Nil => \"no rows\",
  }
}

fn ask(pool: Pool, sql: String) -> String {
  match with_db(pool, fn () => db.query(sql, List::Nil)) {
    Result::Err(why) => \"no lease: \" + why.show(),
    Result::Ok(Result::Err(why)) => why.show(),
    Result::Ok(Result::Ok(rows)) => shown(rows),
  }
}

fn is(h: Health, live: Int, reconnecting: Int, down: Int) -> Bool {
  h.live == live && h.reconnecting == reconnecting && h.down == down
}

fn said(h: Health) -> String {
  Int::to_string(h.live) + \" live, \" + Int::to_string(h.reconnecting) + \" reconnecting, \"
    + Int::to_string(h.down) + \" down\"
}

/// Waits up to `within` ms for the pool to reach a state; answers what it
/// reached.
fn until(pool: Pool, live: Int, reconnecting: Int, down: Int, within: Int) -> String {
  let deadline = now() + within;
  while !is(health(pool), live, reconnecting, down) && now() < deadline {
    khora_sleep(5)
  };
  said(health(pool))
}

/// Waits `ms` by the clock. Used only after the server has been told to hang
/// up, as a margin for its `FIN` to cross loopback, so the next statement
/// finds the connection closed rather than racing the hang-up.
fn hold(ms: Int) -> () {
  let deadline = now() + ms;
  while now() < deadline { khora_sleep(5) }
}

/// Waits up to five seconds for `want` connections to be idle.
fn settled(pool: Pool, want: Int) -> Int {
  let deadline = now() + 5000;
  while idle_count(pool) != want && now() < deadline {
    khora_sleep(5)
  };
  idle_count(pool)
}
";

/// Builds `body` after [`RECONNECT_PRELUDE`], with `SETTINGS` filled in.
fn reconnect_program(settings: &str, body: &str) -> String {
    format!("{RECONNECT_PRELUDE}\n{}", body.replace("SETTINGS", settings))
}

/// Runs a reconnect program against its own [`Scripted`] server on both
/// backends, and answers each backend's run with the server it had.
fn run_scripted(name: &str, body: &str, patience: u64) -> Vec<(&'static str, Watched, Scripted)> {
    ["threads", "scheduler"]
        .into_iter()
        .map(|backend| {
            let server = Scripted::start();
            let exe = build(&format!("{name}_{backend}"), &reconnect_program(&server.settings(), body));
            let ran = run_watched(&exe, backend, std::time::Duration::from_secs(patience));
            (backend, ran, server)
        })
        .collect()
}

/// Asserts each run ended cleanly with `expected` on stdout.
fn assert_ran(runs: &[(&str, Watched, Scripted)], expected: &str, what: &str) {
    for (backend, ran, _) in runs {
        assert!(!ran.hung, "{backend} hung ({what}): stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(ran.stdout, expected, "{backend}: {what}");
    }
}

/// **A borrower stopped by `abort` never costs the pool its connection**,
/// whether the abort lands in the body, with a statement in flight, while
/// the borrower waits in `with_db` and is handed the connection, or while
/// fibers it spawned hold leases of their own or wait for one. A pool of
/// one, 200 trials at each point; after each, the pool must be whole within
/// three seconds (one live connection, idle), and in the child-fiber phases
/// the next caller must get its own number. The child-fiber phases stop the
/// parent with `abort` and with `cancel_within(5)`, a grace shorter than
/// the children's statements: the stop reaches the children through the
/// release of their handles, with one child's statement in flight and two
/// children waiting in `with_db`.
///
/// A borrower stopped with a statement in flight leaves its reply arriving on
/// the connection. That connection must not be lent to the next caller with
/// the old reply unread, and the lease must still come back: the give-back
/// sends it to its slot, which reads the rest of the reply and lends it
/// again, or replaces it.
///
/// A child cannot use its parent's lease -- a `Db` stays on the fiber it was
/// installed on, and `a_fiber_cannot_use_its_parents_lease` pins that
/// refusal -- so each child here takes its own.
#[test]
fn aborted_borrowers_never_lose_a_slot() {
    let body = r#"fn churn(pool: Pool) -> () {
  let mut going = true;
  while going {
    let _ = ask(pool, "select 1");
    ()
  }
}

fn holder(pool: Pool) -> () {
  let _ = ask(pool, "stall 20");
  ()
}

fn looper(n: Int) -> () with { db: Db } {
  let mut going = true;
  while going { let _ = db.query("stall " + Int::to_string(n), List::Nil); () }
}

/// One child: a lease of its own, and statements until it is stopped.
fn leased(pool: Pool, n: Int) -> () {
  let _ = with_db(pool, fn () => looper(n));
  ()
}

/// Three fibers, each running its statements on a lease of its own.
fn fanout(pool: Pool) -> Int {
  let a = Fiber::spawn(fn () => leased(pool, 20));
  let b = Fiber::spawn(fn () => leased(pool, 21));
  let c = Fiber::spawn(fn () => leased(pool, 22));
  Fiber::wait(a);
  Fiber::wait(b);
  Fiber::wait(c);
  1
}

fn parent(pool: Pool) -> () {
  let _ = fanout(pool);
  ()
}

/// 200 parents whose children hold or wait for leases, each stopped
/// 30-49 ms in; the trial the slot was lost at or the next caller went
/// unanswered, or -1.
fn children(pool: Pool, abort: Bool) -> Int {
  let mut trial = 0;
  let mut lost = 0 - 1;
  while trial < 200 && lost < 0 {
    let f = Fiber::spawn(fn () => parent(pool));
    khora_sleep(30 + trial % 20);
    if abort { Fiber::abort(f) } else { Fiber::cancel_within(f, 5) };
    Fiber::wait(f);
    if !whole(pool) { lost = trial } else {
      let want = Int::to_string(1000 + trial);
      if ask(pool, "select " + want) != want { lost = trial }
    };
    trial = trial + 1
  };
  lost
}

fn whole(pool: Pool) -> Bool {
  let deadline = now() + 3000;
  let mut ok = false;
  while !ok && now() < deadline {
    ok = is(health(pool), 1, 0, 0) && idle_count(pool) == 1;
    if !ok { khora_sleep(2) }
  };
  ok
}

/// 200 aborts at one point; the trial the slot was lost at, or -1.
fn storm(pool: Pool, handover: Bool) -> Int {
  let mut seed = 12345;
  let mut trial = 0;
  let mut lost = 0 - 1;
  while trial < 200 && lost < 0 {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    let r = seed / 65536 % 1000;
    if handover {
      let h = Fiber::spawn(fn () => holder(pool));
      khora_sleep(2);
      let f = Fiber::spawn(fn () => churn(pool));
      khora_sleep(14 + r % 12);
      Fiber::abort(f);
      Fiber::wait(f);
      Fiber::wait(h);
    } else {
      let f = Fiber::spawn(fn () => churn(pool));
      khora_sleep(1 + r % 5);
      Fiber::abort(f);
      Fiber::wait(f);
    };
    if !whole(pool) { lost = trial };
    trial = trial + 1
  };
  lost
}

fn report(point: String, lost: Int, pool: Pool) -> () {
  if lost < 0 { print(point + ": whole after 200 aborts") } else {
    print(point + ": lost at trial " + Int::to_string(lost) + ": " + said(health(pool)) + ", idle "
      + Int::to_string(idle_count(pool)))
  }
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = { fast: Schedule::UpTo(Schedule::backoff(10, 60), 2000), slow: Option::Some(100), handshake: 10000 };
  let pool = open_with(Fibers::open(), settings, 1, plan);
  print("start: " + until(pool, 1, 0, 0, 5000));
  let body = storm(pool, false);
  report("body", body, pool);
  if body < 0 {
    let handover = storm(pool, true);
    report("handover", handover, pool);
    if handover < 0 {
      let aborted = children(pool, true);
      report("children, abort", aborted, pool);
      if aborted < 0 {
        let graced = children(pool, false);
        report("children, cancel_within(5)", graced, pool);
        if graced < 0 {
          print("after: " + ask(pool, "select 42"));
          close(pool);
          print("closed")
        }
      }
    }
  };
  0
}
"#;
    let runs = run_scripted("pool_abort_storm", body, 360);
    assert_ran(
        &runs,
        "start: 1 live, 0 reconnecting, 0 down\nbody: whole after 200 aborts\n\
         handover: whole after 200 aborts\nchildren, abort: whole after 200 aborts\n\
         children, cancel_within(5): whole after 200 aborts\nafter: 42\nclosed\n",
        "an aborted borrower must not cost the pool its connection",
    );
}

/// **The time a connection sat idle is not charged to the fast phase.** A
/// pool of one on a 2 s fast phase is opened and left unleased for 2.5 s;
/// then a second pool asks the server to close every other connection, as
/// a restart or `pg_terminate_backend` does. The next caller must get its
/// own answer within a few backoff steps, and the pool must be whole.
///
/// A failed attempt is judged against when the slot's run of failures
/// began. Measured from when the slot last started connecting, a connection
/// that sat open, never lent, for longer than the fast phase found that
/// phase already over at its first failed check: the slot went straight to
/// `down`, and every caller was answered `Disconnected` until the slow
/// retry, although the server was back at once. Only the time since the
/// connection opened counts.
#[test]
fn an_idle_connection_closed_by_the_server_is_retried_on_the_fast_phase() {
    let body = r#"fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = {
    fast: Schedule::UpTo(Schedule::backoff(50, 5000), 2000), slow: Option::Some(30000), handshake: 10000,
  };
  let a = open_with(Fibers::open(), settings, 1, plan);
  let b = open_with(Fibers::open(), settings, 1, plan);
  print("a: " + until(a, 1, 0, 0, 5000));
  print("b: " + ask(b, "select 1"));
  // Longer than the fast phase, with a's connection never lent.
  hold(2500);
  print("kill: " + ask(b, "kill others"));
  hold(100);
  let t0 = now();
  let got = ask(a, "select 42");
  let took = now() - t0;
  print("never lent: " + got + (if took < 2000 { ", promptly" } else { ", after " + Int::to_string(took) + " ms" }));
  print("health: " + until(a, 1, 0, 0, 5000));
  close(a);
  close(b);
  print("closed");
  0
}
"#;
    let runs = run_scripted("pool_stale_idle", body, 60);
    assert_ran(
        &runs,
        "a: 1 live, 0 reconnecting, 0 down\nb: 1\nkill: 0\nnever lent: 42, promptly\n\
         health: 1 live, 0 reconnecting, 0 down\nclosed\n",
        "an idle connection's age must not be charged to the fast phase",
    );
}

/// **A connection that fails its check straight after it opens counts as a
/// failed attempt, so the slot backs off.** A pool of one on the default
/// plan; for 3 s every new connection finishes its handshake and is then
/// sent a `DataRow` nobody asked for, so the check before its first lease
/// fails. The server counts the connections it accepts.
///
/// The bound comes from the schedule. The default backoff starts at 50 ms
/// and doubles, each delay drawn at 50-100% of its value, so the delays are
/// at least 25, 50, 100, 200, 400, 800 and 1600 ms, and their running totals
/// pass 3 s after the seventh. With the lease that fails the check leading
/// each attempt, a slot makes at most eight connections in the window and
/// one more once the server has recovered: nine, with one to spare for
/// scheduling, ten. A caller asking throughout gets its own answer once the
/// server recovers.
///
/// A failed check marked the connection broken and the slot reconnected at
/// once: its connect succeeded, so the schedule never counted an attempt,
/// and one slot opened hundreds of connections a second -- on macOS enough
/// to exhaust the loopback until `connect` itself failed and the pool shrank
/// out.
#[test]
fn a_connection_that_fails_its_first_check_is_backed_off() {
    let body = r#"fn asker(pool: Pool, told: Channel<String>) -> () {
  Channel::send(told, ask(pool, "select 42"));
  ()
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let pool = open(Fibers::open(), settings, 1);
  print("start: " + until(pool, 1, 0, 0, 5000));
  let before = ask(pool, "conns");
  print("sour: " + ask(pool, "sour 3000"));
  // `sour` hangs up the connection it was asked on; let the `FIN` arrive
  // first, or the next lease can be lent it before the check can see it.
  hold(100);
  let told: Channel<String> = Channel::bounded(1);
  let f = Fiber::spawn(fn () => asker(pool, told));
  Fiber::wait(f);
  print("asked: " + (match Channel::receive(told) { Option::Some(s) => s, Option::None => "nothing" }));
  let after = ask(pool, "conns");
  let made = (match Int::of_string(after) { Option::Some(n) => n, Option::None => 0 - 1000 })
    - (match Int::of_string(before) { Option::Some(n) => n, Option::None => 1000 });
  print("connections while sour: " + Int::to_string(made));
  print("health: " + until(pool, 1, 0, 0, 5000));
  close(pool);
  print("closed");
  0
}
"#;
    let runs = run_scripted("pool_sour_backoff", body, 90);
    for (backend, ran, _) in &runs {
        assert!(!ran.hung, "{backend} hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        let made: i64 = ran
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("connections while sour: "))
            .and_then(|n| n.parse().ok())
            .unwrap_or(i64::MAX);
        eprintln!("{backend}: {made} connections while sour");
        assert!(
            made <= 10,
            "{backend}: {made} connections in a 3 s window; the schedule allows at most 10: {:?}",
            ran.stdout
        );
        let rest: Vec<&str> =
            ran.stdout.lines().filter(|l| !l.starts_with("connections while sour: ")).collect();
        assert_eq!(
            rest,
            ["start: 1 live, 0 reconnecting, 0 down", "sour: 0", "asked: 42",
             "health: 1 live, 0 reconnecting, 0 down", "closed"],
            "{backend}: the caller must be answered and the pool whole"
        );
    }
}

/// **A connection whose `ErrorResponse` has only begun to arrive is not
/// lent.** A pool of two; the server sends the idle connection the first 9
/// bytes of a `FATAL` -- its kind byte, its length and a few bytes of its
/// fields -- and the rest, with a hang-up, 1.5 s later. Both connections are
/// then leased at once, and each must answer its own statement.
///
/// The check judged whole messages only, and left a message still arriving
/// to the next statement: the half-arrived `FATAL` was lent, the statement
/// on it read the `FATAL` as its reply, and the caller was answered
/// `Disconnected` although the other connection was healthy. Its first byte
/// already says what it is.
#[test]
fn a_half_arrived_error_fails_the_check() {
    let body = r#"fn pair(pool: Pool) -> String {
  let told: Channel<String> = Channel::bounded(2);
  let a = Fiber::spawn(fn () => { Channel::send(told, ask(pool, "select 7")); () });
  let b = Fiber::spawn(fn () => { Channel::send(told, ask(pool, "select 8")); () });
  Fiber::wait(a);
  Fiber::wait(b);
  let one = match Channel::receive(told) { Option::Some(x) => x, Option::None => "none" };
  let two = match Channel::receive(told) { Option::Some(x) => x, Option::None => "none" };
  if one == "8" && two == "7" { "7 8" } else { one + " " + two }
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let pool = open(Fibers::open(), settings, 2);
  print("start: " + until(pool, 2, 0, 0, 5000));
  print("half: " + ask(pool, "half-error others"));
  hold(100);
  print("answers: " + pair(pool));
  print("health: " + until(pool, 2, 0, 0, 5000));
  close(pool);
  print("closed");
  0
}
"#;
    let runs = run_scripted("pool_half_error", body, 60);
    assert_ran(
        &runs,
        "start: 2 live, 0 reconnecting, 0 down\nhalf: 0\nanswers: 7 8\n\
         health: 2 live, 0 reconnecting, 0 down\nclosed\n",
        "a half-arrived error must fail the check",
    );
}

/// **A caller whose every offered connection fails its check waits, and is
/// answered once one passes.** A pool of one; for eight seconds each new
/// connection finishes its handshake and is then sent a `DataRow` nobody
/// asked for, so the check before each lease fails and the slot
/// reconnects. A caller in a fiber of its own asks throughout, and must get
/// its own answer, once, after the server recovers.
///
/// With failed checks counted toward the backoff, the slot makes a handful
/// of connections in those eight seconds, not thousands, and the caller is
/// answered by the first one after the window: at most the window plus one
/// capped delay (5 s), well inside the 30 s fast phase and this test's 90 s.
/// What it still shows is that the caller's wait is a loop over offers --
/// every failed check hands it another -- that never grows its stack.
///
/// `with_db` took the next offer by calling itself, one frame per failed
/// check, and a slot fails one a few milliseconds after it connects: the
/// caller's stack ran out within seconds, and the process ended with a
/// segmentation fault.
#[test]
fn a_caller_outlasts_any_number_of_failed_checks() {
    let body = r#"fn asker(pool: Pool, told: Channel<String>) -> () {
  Channel::send(told, ask(pool, "select 42"));
  ()
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let pool = open(Fibers::open(), settings, 1);
  print("start: " + until(pool, 1, 0, 0, 5000));
  print("sour: " + ask(pool, "sour 8000"));
  // `sour` hangs up the connection it was asked on; let the `FIN` arrive
  // first, or the next lease can be lent it before the check can see it.
  hold(100);
  let told: Channel<String> = Channel::bounded(1);
  let f = Fiber::spawn(fn () => asker(pool, told));
  Fiber::wait(f);
  print("asked: " + (match Channel::receive(told) { Option::Some(s) => s, Option::None => "nothing" }));
  print("health: " + until(pool, 1, 0, 0, 5000));
  close(pool);
  print("closed");
  0
}
"#;
    let runs = run_scripted("pool_sour_checks", body, 90);
    assert_ran(
        &runs,
        "start: 1 live, 0 reconnecting, 0 down\nsour: 0\nasked: 42\nhealth: 1 live, 0 reconnecting, 0 down\nclosed\n",
        "a caller must outlast failed checks",
    );
}

/// **A caller stopped while it holds the down token hands it on**, so the
/// callers after it are still answered at once. A pool of one that has shrunk
/// for good (no slow phase): every `with_db` takes the down token and is
/// answered `Disconnected`. 200 trials of a fiber doing that in a loop,
/// stopped 1-3 ms in, first by `cancel` and then by `abort`; after each, a
/// fresh caller must be answered within 500 ms.
///
/// The token went back only from the body of `with_db`, after several
/// cancellation points, so a caller stopped in between unwound holding it,
/// and every later `with_db` waited for ever.
#[test]
fn a_stopped_caller_never_keeps_the_down_token() {
    let body = r#"fn churn(pool: Pool) -> () {
  let mut going = true;
  while going {
    let _ = ask(pool, "select 1");
    ()
  }
}

fn answer_to(pool: Pool, told: Channel<Int>) -> () {
  let _ = ask(pool, "select 2");
  Channel::send(told, 1);
  ()
}

/// Asks in a fiber of its own and waits at most `within` ms for the answer.
fn answered_within(pool: Pool, within: Int) -> Bool {
  let told: Channel<Int> = Channel::bounded(1);
  let f = Fiber::spawn(fn () => answer_to(pool, told));
  let deadline = now() + within;
  while Channel::depth(told) == 0 && now() < deadline { khora_sleep(2) };
  let ok = Channel::depth(told) == 1;
  if ok { Fiber::wait(f) } else { Fiber::detach(f) };
  ok
}

/// 200 stopped callers; the trial after which a fresh caller hung, or -1.
fn storm(pool: Pool, abort: Bool) -> Int {
  let mut trial = 0;
  let mut hung = 0 - 1;
  while trial < 200 && hung < 0 {
    let f = Fiber::spawn(fn () => churn(pool));
    khora_sleep(1 + trial % 3);
    if abort { Fiber::abort(f) } else { Fiber::cancel(f) };
    Fiber::wait(f);
    if !answered_within(pool, 500) { hung = trial };
    trial = trial + 1
  };
  hung
}

fn report(how: String, hung: Int) -> () {
  if hung < 0 { print(how + ": every caller answered") } else {
    print(how + ": a caller hung after trial " + Int::to_string(hung))
  }
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = { fast: Schedule::UpTo(Schedule::backoff(10, 20), 60), slow: Option::None, handshake: 10000 };
  let pool = open_with(Fibers::open(), settings, 1, plan);
  print("start: " + until(pool, 1, 0, 0, 5000));
  print("down: " + ask(pool, "down -1"));
  hold(100);
  let _ = ask(pool, "select 1");
  print("shrunk: " + until(pool, 0, 0, 1, 3000));
  let canceled = storm(pool, false);
  report("cancel", canceled);
  if canceled < 0 { report("abort", storm(pool, true)) };
  0
}
"#;
    let runs = run_scripted("pool_token_storm", body, 240);
    assert_ran(
        &runs,
        "start: 1 live, 0 reconnecting, 0 down\ndown: 0\nshrunk: 0 live, 0 reconnecting, 1 down\n\
         cancel: every caller answered\nabort: every caller answered\n",
        "a stopped caller must not keep the down token",
    );
}

/// **A healthy idle connection that the server sent a notification or a
/// notice is lent, not reconnected.** A pool of two; the scripted server
/// writes a `NotificationResponse` and a `NoticeResponse` to the idle
/// connection, unasked, the way `LISTEN`/`NOTIFY` and `RAISE NOTICE` do; then
/// both connections are leased at once, and each must answer its own
/// statement, and the server must have accepted no connection beyond the
/// first two.
///
/// Then the server sends the idle connection a `FATAL` `ErrorResponse` and
/// leaves the socket open, as a server ending a backend does just before it
/// hangs up: that one must be reconnected (a third connection), and no
/// caller may be answered with an error.
///
/// The check before each lease counted any unread bytes as a broken
/// connection, so the notified one was closed and reconnected: a third
/// connection, and whatever session state the first one held was gone.
#[test]
fn an_idle_connection_that_was_sent_a_notification_is_lent() {
    let body = r#"fn pair(pool: Pool) -> String {
  let told: Channel<String> = Channel::bounded(2);
  let a = Fiber::spawn(fn () => { let _ = with_db(pool, fn () => {
      let _ = db.query("stall 200", List::Nil);
      Channel::send(told, "a");
      ()
    }); () });
  let b = Fiber::spawn(fn () => { let _ = with_db(pool, fn () => {
      let _ = db.query("stall 200", List::Nil);
      Channel::send(told, "b");
      ()
    }); () });
  Fiber::wait(a);
  Fiber::wait(b);
  Int::to_string(Channel::depth(told))
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let pool = open(Fibers::open(), settings, 2);
  print("start: " + until(pool, 2, 0, 0, 5000));
  print("notify: " + ask(pool, "notify others"));
  hold(100);
  print("leased at once: " + pair(pool));
  print("own answers: " + ask(pool, "select 7") + " " + ask(pool, "select 8"));
  print("connections: " + ask(pool, "conns"));
  print("error: " + ask(pool, "error others"));
  hold(100);
  print("leased at once: " + pair(pool));
  print("own answers: " + ask(pool, "select 7") + " " + ask(pool, "select 8"));
  print("connections: " + ask(pool, "conns"));
  print("health: " + until(pool, 2, 0, 0, 5000));
  close(pool);
  print("closed");
  0
}
"#;
    let runs = run_scripted("pool_notified_idle", body, 60);
    assert_ran(
        &runs,
        "start: 2 live, 0 reconnecting, 0 down\nnotify: 0\nleased at once: 2\nown answers: 7 8\n\
         connections: 2\nerror: 0\nleased at once: 2\nown answers: 7 8\nconnections: 3\n\
         health: 2 live, 0 reconnecting, 0 down\nclosed\n",
        "a notified idle connection must be lent, not reconnected",
    );
}

/// **A connection the server reset while it sat idle is not lent**, so the
/// statement after it neither fails nor ends the process. A pool of two; the
/// server resets the idle one (`RST`), then 20 statements one after another.
/// Each must be answered with its own number, and the process must exit
/// normally.
///
/// The check before each lease read the reset as "nothing has arrived", the
/// healthy answer, because a read that failed and a read that would have
/// blocked were one answer. The next statement was written to the reset
/// socket, and `SIGPIPE` killed the process before anything was printed.
#[cfg(unix)]
#[test]
fn a_connection_reset_while_idle_is_not_lent() {
    let body = r#"fn main() -> Int {
  let settings: Settings = SETTINGS;
  let pool = open(Fibers::open(), settings, 2);
  print("start: " + until(pool, 2, 0, 0, 5000));
  print("reset: " + ask(pool, "rst others"));
  hold(100);
  let mut n = 1;
  let mut wrong = 0;
  while n <= 20 {
    let got = ask(pool, "select " + Int::to_string(n));
    if got != Int::to_string(n) { wrong = wrong + 1 };
    n = n + 1
  };
  print("wrong " + Int::to_string(wrong) + " of 20");
  print("back: " + until(pool, 2, 0, 0, 5000));
  close(pool);
  print("closed");
  0
}
"#;
    let runs = run_scripted("pool_reset_idle", body, 60);
    assert_ran(
        &runs,
        "start: 2 live, 0 reconnecting, 0 down\nreset: 0\nwrong 0 of 20\n\
         back: 2 live, 0 reconnecting, 0 down\nclosed\n",
        "a reset idle connection must not be lent",
    );
}

/// **A server that accepts connections and never answers them is a server
/// that is down, not a pool that hangs.** The listener here takes each
/// connection and says nothing, the way a server stuck in its own startup
/// does. With a `handshake` bound of 300 ms and a fast phase of about a
/// second, callers must be answered `Disconnected` with the reason once the
/// fast phase has run out, and `close` must return within the bound plus a
/// margin even though an attempt is in the middle of its handshake.
///
/// With no bound on the handshake, the first attempt waited for the server's
/// first message for ever: the caller was never answered, and `close`,
/// which waits for the slot, never returned.
#[test]
fn a_server_that_never_answers_the_handshake_is_given_up_on() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("an address").port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            held.push(stream);
        }
    });
    let body = r#"fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = {
    fast: Schedule::UpTo(Schedule::backoff(20, 40), 1000), slow: Option::Some(60000), handshake: 300,
  };
  let pool = open_with(Fibers::open(), settings, 1, plan);
  let began = now();
  let got = ask(pool, "select 1");
  let waited = now() - began;
  print("caller: " + got);
  print("answered in time: " + (if waited < 5000 { "yes" } else { Int::to_string(waited) }));
  print("health: " + said(health(pool)));
  // `pool` is down now and waits 60 s between tries, so a second pool is
  // what has an attempt in its handshake: 100 ms in, of a 300 ms bound.
  let again = open_with(Fibers::open(), settings, 1, plan);
  hold(100);
  let closing = now();
  close(again);
  let took = now() - closing;
  print("close mid-handshake: " + (if took < 300 + 1000 { "prompt" } else { Int::to_string(took) + " ms" }));
  close(pool);
  print("closed");
  0
}
"#;
    let settings = format!(
        "{{ host: \"127.0.0.1\", port: {port}, user: \"khora\", database: \"khora\", secret: \"\" }}"
    );
    for backend in ["threads", "scheduler"] {
        let exe = build(&format!("pool_mute_{backend}"), &reconnect_program(&settings, body));
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(60));
        assert!(!ran.hung, "{backend} hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout,
            "caller: no lease: disconnected: the server did not finish the handshake within 300 ms\n\
             answered in time: yes\nhealth: 0 live, 0 reconnecting, 1 down\n\
             close mid-handshake: prompt\nclosed\n",
            "{backend}: a mute server must be given up on"
        );
    }
}

/// **A pool of two that loses one connection answers nobody `Disconnected`
/// while the other is live.** The server hangs up the idle connection; the
/// pool finds out when it next checks that connection before lending it,
/// lends the other one instead, and reconnects the lost one. Twenty
/// statements must each get their own number, and the pool must be back to
/// two live connections.
///
/// With the check before each lease disabled, the lost connection is lent
/// and its borrowers are answered `Disconnected`.
#[test]
fn a_pool_of_two_that_loses_one_answers_every_caller() {
    let body = "fn main() -> Int {
  let settings: Settings = SETTINGS;
  let pool = open(Fibers::open(), settings, 2);
  print(\"start: \" + until(pool, 2, 0, 0, 5000));
  print(\"kill: \" + ask(pool, \"kill others\"));
  hold(100);
  let mut wrong = 0;
  let mut n = 1;
  while n <= 20 {
    let got = ask(pool, \"select \" + Int::to_string(n));
    if got != Int::to_string(n) {
      print(\"asked \" + Int::to_string(n) + \": \" + got);
      wrong = wrong + 1
    };
    n = n + 1
  };
  print(\"wrong \" + Int::to_string(wrong) + \" of 20\");
  print(\"after: \" + until(pool, 2, 0, 0, 5000));
  close(pool);
  print(\"closed\");
  0
}
";
    let runs = run_scripted("pool_one_killed", body, 60);
    assert_ran(
        &runs,
        "start: 2 live, 0 reconnecting, 0 down\nkill: 0\nwrong 0 of 20\nafter: 2 live, 0 reconnecting, 0 down\nclosed\n",
        "no caller may be answered Disconnected while a live connection exists",
    );
}

/// **A pool that cannot reconnect fails its waiting callers with the reason,
/// and does not hang.** One connection; the server goes down for good; three
/// callers wait. When the reconnect schedule (at most 500 ms) gives up, all
/// three must be answered `Disconnected` with the reason the last attempt
/// failed, within the schedule plus a margin; a new caller must be answered
/// at once; and `close` must return.
///
/// With the pool not waking its waiters when the last connection goes, the
/// three wait for ever.
#[test]
fn a_pool_that_cannot_reconnect_fails_its_waiters_with_the_reason() {
    let body = "fn waiter(pool: Pool, done: Channel<String>) -> () {
  let began = now();
  let got = ask(pool, \"select 1\");
  let when = if now() - began < 2500 { \"in time\" } else { \"late\" };
  Channel::send(done, got + \" (\" + when + \")\");
  ()
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = { fast: Schedule::UpTo(Schedule::backoff(20, 100), 500), slow: Option::None, handshake: 10000 };
  let pool = open_with(Fibers::open(), settings, 1, plan);
  print(\"start: \" + until(pool, 1, 0, 0, 5000));
  print(\"down: \" + ask(pool, \"down -1\"));
  hold(100);
  let done: Channel<String> = Channel::bounded(3);
  // Adopted rather than bound to `_`: a handle let go waits for its fiber,
  // which would run the waiters one after another instead of together.
  let crew = Fibers::open();
  let mut spawned = 0;
  while spawned < 3 {
    Fibers::adopt(crew, Fiber::spawn(fn () => waiter(pool, done)));
    spawned = spawned + 1
  };
  let mut heard = 0;
  while heard < 3 {
    match Channel::receive(done) {
      Option::Some(got) => print(\"waiter: \" + got),
      Option::None => (),
    };
    heard = heard + 1
  };
  let began = now();
  let got = ask(pool, \"select 2\");
  print(\"new caller: \" + got + (if now() - began < 200 { \" (at once)\" } else { \" (late)\" }));
  print(\"health: \" + said(health(pool)));
  close(pool);
  print(\"closed\");
  0
}
";
    let reason = "no lease: disconnected: the server closed the connection while a reply was expected";
    let expected = format!(
        "start: 1 live, 0 reconnecting, 0 down\ndown: 0\n\
         waiter: {reason} (in time)\nwaiter: {reason} (in time)\nwaiter: {reason} (in time)\n\
         new caller: {reason} (at once)\nhealth: 0 live, 0 reconnecting, 1 down\nclosed\n"
    );
    let runs = run_scripted("pool_cannot_reconnect", body, 60);
    assert_ran(&runs, &expected, "waiters must get the reason, not hang");
}

/// **Reconnect attempts back off: the gaps grow, and stay under the cap plus
/// jitter.** One connection, a schedule of `backoff(40, 320)`, and a server
/// that turns connections away for 2.5 s. The server records when each
/// attempt arrived. The first gap is the first delay (20-40 ms) plus a
/// connect; the later ones are capped delays (160-320 ms) plus a connect.
///
/// The pool must reconnect once the server is back.
///
/// With a fixed 40 ms delay in place of the schedule, the late gaps stay
/// short.
#[test]
fn reconnect_attempts_back_off_up_to_the_cap() {
    let body = "fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = { fast: Schedule::UpTo(Schedule::backoff(40, 320), 20000), slow: Option::None, handshake: 10000 };
  let pool = open_with(Fibers::open(), settings, 1, plan);
  print(\"start: \" + until(pool, 1, 0, 0, 5000));
  print(\"down: \" + ask(pool, \"down 2500\"));
  hold(100);
  print(\"through the outage: \" + ask(pool, \"select 7\"));
  print(\"after: \" + until(pool, 1, 0, 0, 5000));
  close(pool);
  0
}
";
    let runs = run_scripted("pool_backoff", body, 60);
    assert_ran(
        &runs,
        "start: 1 live, 0 reconnecting, 0 down\ndown: 0\nthrough the outage: 7\nafter: 1 live, 0 reconnecting, 0 down\n",
        "a caller waits through the outage and is served",
    );
    for (backend, _, server) in &runs {
        let gaps: Vec<u128> = server.refused_gaps().iter().map(|g| g.as_millis()).collect();
        assert!(gaps.len() >= 5, "{backend}: too few attempts to judge a backoff: {gaps:?}");
        assert!(gaps[0] < 200, "{backend}: the first retry came late: {gaps:?}");
        // **The margin is one slice's overrun, and that is all a wait can
        // overrun by.** `pause` sleeps in 25 ms slices against an absolute
        // deadline on the monotonic clock: each slice is `min(left, 25)`, so
        // a slice that overran only shortens what is left, and the wait ends
        // at the first wake past the deadline. Thirteen slices of a 316 ms
        // wait do not add their overruns up; only the last one shows. 250 ms
        // is ten times a slice, which is the overrun recorded for loaded
        // runners here, and the test runs alone (`.config/nextest.toml`) so
        // it is not the load.
        assert!(
            gaps.iter().all(|g| *g <= 320 + 250),
            "{backend}: a gap passed the cap plus jitter and a margin: {gaps:?}"
        );
        assert!(
            gaps[gaps.len() - 2..].iter().all(|g| *g >= 150),
            "{backend}: the late gaps did not grow to the capped delay: {gaps:?}"
        );
    }
}

/// **`close` returns promptly while a connection is waiting to retry.** The
/// schedule waits 1.5-3 s between attempts; `close` is called well inside
/// such a wait and must return within a second, with the waiting caller
/// answered and every fiber ended.
///
/// With the wait not looking at `close`, `close` waits out the delay.
#[test]
fn close_during_a_reconnect_wait_returns_promptly() {
    let body = "fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = { fast: Schedule::UpTo(Schedule::backoff(3000, 3000), 60000), slow: Option::Some(30000), handshake: 10000 };
  let pool = open_with(Fibers::open(), settings, 1, plan);
  print(\"start: \" + until(pool, 1, 0, 0, 5000));
  print(\"down: \" + ask(pool, \"down -1\"));
  hold(100);
  let told: Channel<String> = Channel::bounded(1);
  let f = Fiber::spawn(fn () => { Channel::send(told, ask(pool, \"select 1\")); () });
  print(\"reconnecting: \" + until(pool, 0, 1, 0, 5000));
  let settle = now() + 300;
  while now() < settle { khora_sleep(5) };
  let began = now();
  close(pool);
  print(if now() - began < 1000 { \"closed in time\" } else { \"closed late\" });
  Fiber::wait(f);
  match Channel::receive(told) { Option::Some(got) => print(\"waiter: \" + got), Option::None => print(\"waiter: nothing\") };
  print(\"waiter ended\");
  0
}
";
    let runs = run_scripted("pool_close_in_backoff", body, 60);
    assert_ran(
        &runs,
        "start: 1 live, 0 reconnecting, 0 down\ndown: 0\nreconnecting: 0 live, 1 reconnecting, 0 down\n\
         closed in time\nwaiter: no lease: disconnected: the pool is closed\nwaiter ended\n",
        "close must end a reconnect's wait",
    );
}

/// **Canceled borrowers never cost the pool a connection, while
/// connections are being lost and reconnected.** A pool of two; 200 trials of
/// a fiber looping `select 1`, canceled 1-4 ms in; every tenth trial the
/// server hangs up the other connection first, so cancels land while a slot
/// is being checked, handed back and reconnected. Afterwards both
/// connections must be live and idle, and twenty statements must all be
/// answered.
///
/// With a cancellation point between taking a slot and registering its
/// give-back (the check run first), the pool loses slots.
#[test]
fn canceled_borrowers_never_lose_a_slot_while_reconnecting() {
    let body = "fn churn(pool: Pool) -> () {
  let mut going = true;
  while going {
    let _ = ask(pool, \"select 1\");
    ()
  }
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = { fast: Schedule::UpTo(Schedule::backoff(5, 50), 10000), slow: Option::Some(200), handshake: 10000 };
  let pool = open_with(Fibers::open(), settings, 2, plan);
  print(\"start: \" + until(pool, 2, 0, 0, 5000));
  let mut trial = 0;
  while trial < 200 {
    if trial % 10 == 0 { let _ = ask(pool, \"kill others\"); hold(20) };
    let f = Fiber::spawn(fn () => churn(pool));
    khora_sleep(1 + trial % 4);
    Fiber::cancel(f);
    Fiber::wait(f);
    trial = trial + 1
  };
  print(\"after: \" + until(pool, 2, 0, 0, 10000));
  print(\"idle: \" + Int::to_string(settled(pool, 2)));
  let mut right = 0;
  let mut n = 1;
  while n <= 20 {
    if ask(pool, \"select \" + Int::to_string(n)) == Int::to_string(n) { right = right + 1 };
    n = n + 1
  };
  print(\"answered \" + Int::to_string(right) + \" of 20\");
  close(pool);
  print(\"closed\");
  0
}
";
    let runs = run_scripted("pool_cancel_storm", body, 120);
    assert_ran(
        &runs,
        "start: 2 live, 0 reconnecting, 0 down\nafter: 2 live, 0 reconnecting, 0 down\nidle: 2\n\
         answered 20 of 20\nclosed\n",
        "no slot may be lost to a cancel",
    );
}

/// **A pool that shrank to nothing grows back when the server returns.**
/// Two connections; a fast schedule of at most 300 ms and a slow retry every
/// 500 ms (±20%). The server is down for 2 s: both connections shrink out,
/// and a caller is answered `Disconnected` at once. Once the server is back,
/// both must be live again within the outage plus one slow interval plus a
/// margin, and serve.
///
/// With the slow retry disabled, the pool stays at nothing.
#[test]
fn a_pool_shrunk_to_nothing_grows_back_when_the_server_returns() {
    let body = "fn main() -> Int {
  let settings: Settings = SETTINGS;
  let plan: Reconnect = { fast: Schedule::UpTo(Schedule::backoff(20, 100), 300), slow: Option::Some(500), handshake: 10000 };
  let pool = open_with(Fibers::open(), settings, 2, plan);
  print(\"start: \" + until(pool, 2, 0, 0, 5000));
  let began = now();
  print(\"down: \" + ask(pool, \"down 2000\"));
  hold(100);
  print(\"while down: \" + ask(pool, \"select 1\"));
  print(\"shrunk: \" + until(pool, 0, 0, 2, 3000));
  let asked = now();
  let got = ask(pool, \"select 2\");
  print(\"at nothing: \" + got + (if now() - asked < 200 { \" (at once)\" } else { \" (late)\" }));
  print(\"grown: \" + until(pool, 2, 0, 0, began + 2000 + 600 + 1500 - now()));
  print(\"after: \" + ask(pool, \"select 3\"));
  close(pool);
  0
}
";
    let reason = "no lease: disconnected: the server closed the connection while a reply was expected";
    let expected = format!(
        "start: 2 live, 0 reconnecting, 0 down\ndown: 0\nwhile down: {reason}\n\
         shrunk: 0 live, 0 reconnecting, 2 down\nat nothing: {reason} (at once)\n\
         grown: 2 live, 0 reconnecting, 0 down\nafter: 3\n"
    );
    let runs = run_scripted("pool_grows_back", body, 60);
    assert_ran(&runs, &expected, "a shrunk pool must grow back within one slow interval");
}

/// **An `abort` during a transaction's `ROLLBACK` does not cost the pool its
/// connection.** A pool of one; a transaction whose body spins is canceled
/// with `cancel_within(100)`, and the server stalls the `ROLLBACK` for a
/// second, so the abort lands while the rollback waits. The next transaction
/// must get the connection and commit, and the server must hear the
/// rollback before it.
///
/// With the give-back written inline in `with_db`, the abort stopped it at
/// its entry and the next transaction waited for ever.
#[test]
fn an_abort_during_a_rollback_gives_the_connection_back() {
    let main = "module demo::main;
import std::core::{Fiber, Fibers, Result, print};
import std::db::{Db, DbError, transaction};
import postgres::db::{Settings};
import postgres::pool::{Pool, close, open, with_db};

extern fn khora_sleep(millis: Int) -> ();

fn spinning() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let mut i = 0;
    while true { i = i + 1 };
    Result::Ok(i)
  })
}

fn empty() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => Result::Ok(1))
}

fn main() -> Int {
  let settings: Settings = { host: \"127.0.0.1\", port: PORT, user: \"khora\", database: \"khora\", secret: \"\" };
  let pool = open(Fibers::open(), settings, 1);
  let f = Fiber::spawn(fn () => { let _ = with_db(pool, spinning); () });
  khora_sleep(200);
  Fiber::cancel_within(f, 100);
  Fiber::wait(f);
  match with_db(pool, empty) {
    Result::Ok(Result::Ok(_)) => print(\"the next transaction committed\"),
    _ => print(\"the next transaction failed\"),
  };
  close(pool);
  0
}
";
    for backend in ["threads", "scheduler"] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let server = std::thread::spawn(move || {
            record_queries_stalling(listener, std::time::Duration::from_millis(1000), "ROLLBACK")
        });
        let exe = build(&format!("pg_abort_in_rollback_{backend}_{port}"), &main.replace("PORT", &port.to_string()));
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(20));
        if ran.hung {
            // The server waits 20 s for a message; do not wait for it too.
            panic!("{backend}: hung, which is what a lost lease does: {:?}", ran.stdout);
        }
        let recorded = server.join().expect("the server thread does not panic once connected");
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(ran.stdout, "the next transaction committed\n", "{backend}: heard {:?}", recorded.heard);
        assert_eq!(
            (recorded.heard.as_slice(), recorded.ended.as_str()),
            (["BEGIN", "ROLLBACK", "BEGIN", "COMMIT"].map(String::from).as_slice(), "Terminate"),
            "{backend}: the aborted transaction's rollback, then the next transaction on the same connection"
        );
    }
}

/// **A pool rides out a real server restart.** Four connections under load
/// from four fibers for six seconds, while `KHORA_POSTGRES_RESTART` restarts
/// the server a second and a half in. Every answer that arrives must be the
/// caller's own number (errors are counted, not assumed away); afterwards the
/// pool must be back to four live connections and fifty statements must all
/// succeed.
///
/// Needs `KHORA_POSTGRES` and `KHORA_POSTGRES_RESTART` (a command that
/// restarts the server on 5433, such as `pg_ctl restart -m fast`); skipped
/// without them.
#[test]
fn a_pool_rides_out_a_real_server_restart() {
    let (Some(_), Some(restart)) =
        (std::env::var_os("KHORA_POSTGRES"), std::env::var("KHORA_POSTGRES_RESTART").ok())
    else {
        eprintln!("skipping: set KHORA_POSTGRES=1 and KHORA_POSTGRES_RESTART to a restart command");
        return;
    };
    let body = "fn load(pool: Pool, me: Int, until_ms: Int, done: Channel<String>) -> () {
  let mut ok = 0;
  let mut failed = 0;
  let mut wrong = 0;
  let mut n = me * 1000000;
  while now() < until_ms {
    let got = ask(pool, \"select \" + Int::to_string(n));
    if got == Int::to_string(n) { ok = ok + 1 } else {
      match Int::of_string(got) { Option::None => failed = failed + 1, Option::Some(_) => wrong = wrong + 1 }
    };
    n = n + 1
  };
  Channel::send(done, Int::to_string(ok) + \" \" + Int::to_string(failed) + \" \" + Int::to_string(wrong));
  ()
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let pool = open(Fibers::open(), settings, 4);
  print(\"start: \" + until(pool, 4, 0, 0, 10000));
  let done: Channel<String> = Channel::bounded(4);
  let stop = now() + 6000;
  let crew = Fibers::open();
  let mut me = 1;
  while me <= 4 {
    let mine = me;
    Fibers::adopt(crew, Fiber::spawn(fn () => load(pool, mine, stop, done)));
    me = me + 1
  };
  let mut heard = 0;
  while heard < 4 {
    match Channel::receive(done) { Option::Some(line) => print(\"LOAD \" + line), Option::None => () };
    heard = heard + 1
  };
  let mut right = 0;
  let mut n = 1;
  while n <= 50 {
    if ask(pool, \"select \" + Int::to_string(n)) == Int::to_string(n) { right = right + 1 };
    n = n + 1
  };
  print(\"after: \" + until(pool, 4, 0, 0, 10000));
  print(\"answered \" + Int::to_string(right) + \" of 50\");
  close(pool);
  0
}
";
    for backend in ["threads", "scheduler"] {
        let exe = build(&format!("pg_restart_{backend}"), &reconnect_program(REAL, body));
        let restarter = {
            let restart = restart.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                std::process::Command::new("sh").arg("-c").arg(&restart).status()
            })
        };
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(90));
        let restarted = restarter.join().expect("the restart thread");
        assert!(restarted.is_ok_and(|s| s.success()), "{backend}: the restart command failed");
        assert!(!ran.hung, "{backend} hung: {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        eprintln!("{backend}: {}", ran.stdout);
        let loads: Vec<Vec<u64>> = ran
            .stdout
            .lines()
            .filter_map(|l| l.strip_prefix("LOAD "))
            .map(|l| l.split(' ').map(|n| n.parse().expect("a count")).collect())
            .collect();
        assert_eq!(loads.len(), 4, "{backend}: {}", ran.stdout);
        assert!(loads.iter().all(|l| l[2] == 0), "{backend}: a caller got another's answer: {loads:?}");
        let rest: Vec<&str> = ran.stdout.lines().filter(|l| !l.starts_with("LOAD ")).collect();
        assert_eq!(
            rest,
            ["start: 4 live, 0 reconnecting, 0 down", "after: 4 live, 0 reconnecting, 0 down", "answered 50 of 50"],
            "{backend}: the pool must be whole again and serve after the restart"
        );
    }
}

// --- a transaction inside a transaction, against the real server ----------

/// **Nested `transaction`s are savepoints, and the caller's answer matches
/// what is committed.** What this prevents: an inner `BEGIN` that PostgreSQL
/// only warns about, so the inner `COMMIT` committed the outer body's writes
/// and the inner `ROLLBACK` undid them. The caller was told `Err` with every
/// row committed, and `Ok` with only the last.
///
/// Every case prints what each level was told, then the rows really there
/// (read through a fresh lease) and whether the connection came back outside
/// a transaction. The first case has a second connection look at the table
/// between the inner `Ok` and the outer commit: an inner transaction commits
/// nothing. Two cases are F1, a `COMMIT` PostgreSQL answers with a
/// `ROLLBACK` tag and no error because a statement in the transaction failed,
/// and the same inside an inner body, whose `RELEASE` the server refuses:
/// the inner body is told `RolledBack` and the outer carries on. The stray
/// case sends the `rollback_to` a cancel delivers for a savepoint that never
/// opened: PostgreSQL would abort the enclosing transaction over it, so the
/// driver must not send it. The two connections case has one fiber's inner
/// transaction fail while the other's,
/// on the other connection, succeeds: each nests on its own.
#[test]
fn nested_transactions_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!("skipping: set KHORA_POSTGRES=1 and bring up packages/postgres/docker-compose.yml to run this");
        return;
    }
    let exe = build("postgres_nested", NESTED_PROGRAM);
    for backend in ["threads", "scheduler"] {
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(120));
        assert!(!ran.hung, "{backend}: the program hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(ran.stdout, "inner Ok, outer Ok\n  inner Ok(1)\n  another connection sees []\n  outer Ok(1), rows [1,2,3], connection clean\ninner Err, outer Ok\n  inner Err(rolled back: rejected: inner body failed)\n  outer Ok(1), rows [1,3], connection clean\ninner Ok, outer Err\n  inner Ok(1)\n  outer Err(rolled back: rejected: outer body failed), rows [], connection clean\nthree levels, the middle fails\n  innermost Ok(1)\n  middle Err(rolled back: rejected: middle body failed)\n  outer Ok(1), rows [1,5], connection clean\na failure ignored in the body\n  outer Err(rolled back: the server ended the transaction with ROLLBACK instead of committing it, because a statement in it failed), rows [], connection clean\na failure ignored in the inner body\n  inner Err(rolled back: current transaction is aborted, commands ignored until end of transaction block [25P02])\n  outer Ok(1), rows [1,3], connection clean\na stray rollback_to\n  depth 1, stray rollback_to Ok\n  outer Ok(1), rows [1,2], connection clean\ntwo connections\n  rows [100,102,200,201,202]\n", "{backend}");
    }
}

const NESTED_PROGRAM: &str = r##"module demo::main;

import std::core::{Channel, Fiber, Fibers, List, Option, Result, Shared, Show, print};
import std::db::{Cell, Db, DbError, Row, transaction};
import postgres::db::{Settings};
import postgres::pool::{Pool, close as close_pool, open as open_pool, with_db};

fn settings() -> Settings {
  { host: "127.0.0.1", port: 5433, user: "khora", database: "khora", secret: "khora" }
}

fn put(n: Int) -> Result<Int, DbError> with { db: Db } {
  db.execute("insert into khora_nested (n) values ($1)", List::Cons(Cell::Number(n), List::Nil))
}

fn fresh() -> () with { db: Db } {
  let _ = db.execute("drop table if exists khora_nested", List::Nil);
  let _ = db.execute("create table khora_nested (n int4)", List::Nil);
}

fn rows() -> String with { db: Db } {
  match db.query("select coalesce(string_agg(n::text, ',' order by n), '') from khora_nested", List::Nil) {
    Result::Ok(List::Cons(row, _)) => match row.cells {
      List::Cons(Cell::Text(t), _) => "[" + t + "]",
      _ => "[?]",
    },
    Result::Ok(_) => "[none]",
    Result::Err(problem) => "[error " + problem.show() + "]",
  }
}

/// Whether the connection is outside a transaction: `SAVEPOINT` is accepted
/// only inside a transaction block, so an accepted one means the connection
/// came back inside a transaction (which is then rolled back here).
fn idle() -> String with { db: Db } {
  match db.execute("savepoint khora_probe_idle", List::Nil) {
    Result::Ok(_) => { let _ = db.rollback(); "left inside a transaction" },
    Result::Err(_) => "clean",
  }
}

fn told(r: Result<Int, DbError>) -> String {
  match r { Result::Ok(n) => "Ok(" + Int::to_string(n) + ")", Result::Err(p) => "Err(" + p.show() + ")" }
}

/// `pool` has a second connection, which looks at the table between the
/// inner commit and the outer one: nothing is committed until the outer is.
fn inner_ok(pool: Pool) -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let _ = put(1);
    print("  inner " + told(transaction(fn () => put(2))));
    let seen = match with_db(pool, rows) { Result::Ok(s) => s, Result::Err(p) => "lease " + p.show() };
    print("  another connection sees " + seen);
    put(3)
  })
}

fn inner_err() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let _ = put(1);
    let inner: Result<Int, DbError> = transaction(fn () => { let _ = put(2); Result::Err(DbError::Rejected("inner body failed")) });
    print("  inner " + told(inner));
    put(3)
  })
}

fn outer_err() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let _ = put(1);
    print("  inner " + told(transaction(fn () => put(2))));
    let _ = put(3);
    Result::Err(DbError::Rejected("outer body failed"))
  })
}

fn three_levels() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let _ = put(1);
    let middle: Result<Int, DbError> = transaction(fn () => {
      let _ = put(2);
      print("  innermost " + told(transaction(fn () => put(3))));
      let _ = put(4);
      Result::Err(DbError::Rejected("middle body failed"))
    });
    print("  middle " + told(middle));
    put(5)
  })
}

fn ignored_failure() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let _ = put(1);
    let _ = db.execute("select 1/0", List::Nil);
    Result::Ok(7)
  })
}

fn ignored_failure_inside() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let _ = put(1);
    let inner = transaction(fn () => {
      let _ = put(2);
      let _ = db.execute("select 1/0", List::Nil);
      Result::Ok(7)
    });
    print("  inner " + told(inner));
    put(3)
  })
}

/// A `rollback_to` for a level whose savepoint was never opened, as a cancel
/// landing before `SAVEPOINT` went out delivers it: it must leave the
/// enclosing transaction alone, so the outer row commits.
fn stray_rollback_to() -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let _ = put(1);
    let level = db.depth();
    let stray = match db.rollback_to(level) { Result::Ok(_) => "Ok", Result::Err(p) => "Err(" + p.show() + ")" };
    print("  depth " + Int::to_string(level) + ", stray rollback_to " + stray);
    put(2)
  })
}

fn run(pool: Pool, name: String, case: () -> Result<Int, DbError> with { db: Db }) -> () {
  let _ = with_db(pool, fresh);
  print(name);
  let r = match with_db(pool, case) { Result::Ok(x) => told(x), Result::Err(p) => "lease " + p.show() };
  let there = match with_db(pool, rows) { Result::Ok(s) => s, Result::Err(p) => "lease " + p.show() };
  let after = match with_db(pool, idle) { Result::Ok(s) => s, Result::Err(p) => "lease " + p.show() };
  print("  outer " + r + ", rows " + there + ", connection " + after);
}

/// One side of the two-connection case: `first` opens its transaction and
/// says so before `second` opens its own, so each is inside a transaction on
/// its own connection while the other nests.
fn side(pool: Pool, base: Int, fails: Bool, ready: Channel<Int>, go: Channel<Int>) -> () {
  let _ = with_db(pool, fn () => transaction(fn () => {
    let _ = put(base);
    let _ = Channel::send(ready, base);
    let _ = Channel::receive(go);
    let inner: Result<Int, DbError> = transaction(fn () => {
      let _ = put(base + 1);
      if fails { Result::Err(DbError::Rejected("no")) } else { Result::Ok(1) }
    });
    let _ = Channel::send(ready, match inner { Result::Ok(_) => base + 1, Result::Err(_) => 0 - base });
    put(base + 2)
  }));
}

fn two_connections(pool: Pool) -> () {
  let _ = with_db(pool, fresh);
  print("two connections");
  let ready: Channel<Int> = Channel::bounded(4);
  let go_a: Channel<Int> = Channel::bounded(1);
  let go_b: Channel<Int> = Channel::bounded(1);
  let a = Fiber::spawn(fn () => side(pool, 100, true, ready, go_a));
  let _ = Channel::receive(ready);
  let b = Fiber::spawn(fn () => side(pool, 200, false, ready, go_b));
  let _ = Channel::receive(ready);
  let _ = Channel::send(go_a, 1);
  let _ = Channel::receive(ready);
  let _ = Channel::send(go_b, 1);
  let _ = Channel::receive(ready);
  Fiber::wait(a);
  Fiber::wait(b);
  let there = match with_db(pool, rows) { Result::Ok(s) => s, Result::Err(p) => "lease " + p.show() };
  print("  rows " + there);
}

fn main() -> () {
  let crew = Fibers::open();
  let pool = open_pool(crew, settings(), 2);
  run(pool, "inner Ok, outer Ok", fn () => inner_ok(pool));
  run(pool, "inner Err, outer Ok", inner_err);
  run(pool, "inner Ok, outer Err", outer_err);
  run(pool, "three levels, the middle fails", three_levels);
  run(pool, "a failure ignored in the body", ignored_failure);
  run(pool, "a failure ignored in the inner body", ignored_failure_inside);
  run(pool, "a stray rollback_to", stray_rollback_to);
  close_pool(pool);
  let crew2 = Fibers::open();
  let two = open_pool(crew2, settings(), 2);
  two_connections(two);
  close_pool(two);
}
"##;

/// **A cancel inside a nested transaction leaves nothing behind**, over 200
/// trials. Odd trials are canceled inside the inner body, even ones after
/// it answered `Ok`: its writes are then part of an outer transaction that
/// never commits. A quarter of them cancel without waiting for either, so
/// the cancel can land anywhere from reading the depth onward. What this
/// prevents: an inner transaction that committed the outer one, so a row
/// survived a cancel (100 of 200 did), an undo that rolled back the wrong
/// level, or a connection handed back inside a transaction. A `ROLLBACK TO`
/// for a savepoint that was never opened would abort the outer transaction. A
/// handler told `broken` about a healthy connection has it closed and
/// reconnected, so the backend's pid is watched across every trial.
#[test]
fn a_cancel_storm_inside_a_nested_transaction_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!("skipping: set KHORA_POSTGRES=1 and bring up packages/postgres/docker-compose.yml to run this");
        return;
    }
    let exe = build("postgres_nested_storm", NESTED_STORM_PROGRAM);
    for backend in ["threads", "scheduler"] {
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(300));
        assert!(!ran.hung, "{backend}: the program hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(
            ran.stdout,
            "trials 200, left inside a transaction 0, errors 0, connection replaced 0, rows surviving 0, other connections idle in a transaction 0\n",
            "{backend}"
        );
    }
}

const NESTED_STORM_PROGRAM: &str = r##"module demo::main;

//! A 200-trial cancel storm inside an inner transaction, real server 5433.
//!
//! Each trial: a fiber opens a transaction, writes a row, opens an inner
//! transaction, writes a row, says it is in, and spins until canceled. The
//! parent waits for that evidence, then a varying delay, then cancels. Before
//! the evidence, the cancel can land anywhere from `depth` to the inner write.
//! After each cancel the same (only) connection is asked whether it is inside
//! a transaction; at the end another connection counts rows and open
//! transactions.

import std::core::{Fiber, Fibers, List, Result, Shared, Show, print};
import std::db::{Cell, Db, DbError, Row, transaction};
import postgres::db::{Settings};
import postgres::pool::{Pool, close, open, with_db};

extern fn khora_monotonic_millis() -> Int;
extern fn khora_sleep(millis: Int) -> ();

fn settings() -> Settings {
  { host: "127.0.0.1", port: 5433, user: "khora", database: "khora", secret: "khora" }
}

fn put(n: Int) -> Result<Int, DbError> with { db: Db } {
  db.execute("insert into khora_nstorm (n) values ($1)", List::Cons(Cell::Number(n), List::Nil))
}

fn spin(turns: Int) -> Int {
  let mut i = 0;
  let mut acc = 0;
  while i < turns { acc = acc + i % 7; i = i + 1 };
  acc
}

/// Odd trials spin inside the inner body; even ones after the inner body
/// answered `Ok`, where a cancel must still undo the inner writes, because
/// they belong to the outer transaction that never committed.
fn nested(trial: Int, inside: Shared<Int>) -> Result<Int, DbError> with { db: Db } {
  transaction(fn () => {
    let _ = put(trial * 2);
    let _ = transaction(fn () => {
      let _ = put(trial * 2 + 1);
      if trial % 2 == 1 {
        Shared::set(inside, 1);
        let mut going = true;
        while going { let _ = spin(1000); () };
      };
      Result::Ok(1)
    });
    Shared::set(inside, 1);
    let mut going = true;
    while going { let _ = spin(1000); () };
    Result::Ok(1)
  })
}

fn trial_run(pool: Pool, trial: Int) -> () {
  let inside = Shared::of(0);
  let f = Fiber::spawn(fn () => { let _ = with_db(pool, fn () => nested(trial, inside)); () });
  if trial % 4 != 0 {
    let started = khora_monotonic_millis();
    while Shared::get(inside) < 1 && khora_monotonic_millis() - started < 5000 { khora_sleep(0) };
  } else {
    let _ = spin((trial * 7919) % 200000);
    ()
  };
  let _ = spin((trial * 104729) % 20000);
  Fiber::cancel(f);
  Fiber::wait(f);
}

fn left_open() -> Int with { db: Db } {
  match db.execute("savepoint khora_nstorm_probe", List::Nil) {
    Result::Ok(_) => { let _ = db.rollback(); 1 },
    Result::Err(DbError::Rejected(_)) => 0,
    Result::Err(problem) => { print("left_open error: " + problem.show()); 0 - 1 },
  }
}

fn fresh() -> () with { db: Db } {
  let _ = db.execute("drop table if exists khora_nstorm", List::Nil);
  let _ = db.execute("create table khora_nstorm (n int4)", List::Nil);
}

fn one_number(sql: String) -> Int with { db: Db } {
  match db.query(sql, List::Nil) {
    Result::Ok(List::Cons(row, _)) => match row.cells {
      List::Cons(Cell::Number(n), _) => n,
      _ => 0 - 1,
    },
    _ => 0 - 1,
  }
}

fn main() -> () {
  let crew = Fibers::open();
  let pool = open(crew, settings(), 1);
  let _ = with_db(pool, fresh);
  let n = 200;
  let mut trial = 0;
  let mut open_after = 0;
  let mut errors = 0;
  // A connection marked broken is closed and reconnected, which a harmless
  // undo never causes: count how often the server-side backend changed.
  let pid_sql = "select pg_backend_pid()::int4";
  let mut pid = match with_db(pool, fn () => one_number(pid_sql)) { Result::Ok(p) => p, Result::Err(_) => 0 - 2 };
  let mut replaced = 0;
  while trial < n {
    trial_run(pool, trial);
    match with_db(pool, left_open) {
      Result::Ok(0) => (),
      Result::Ok(1) => open_after = open_after + 1,
      Result::Ok(_) => errors = errors + 1,
      Result::Err(problem) => { errors = errors + 1; print("lease error: " + problem.show()) },
    };
    let now = match with_db(pool, fn () => one_number(pid_sql)) { Result::Ok(p) => p, Result::Err(_) => 0 - 2 };
    if now != pid { replaced = replaced + 1; pid = now };
    trial = trial + 1
  };
  close(pool);

  let crew2 = Fibers::open();
  let again = open(crew2, settings(), 1);
  let rows = match with_db(again, fn () => one_number("select count(*)::int4 from khora_nstorm")) { Result::Ok(c) => c, Result::Err(_) => 0 - 2 };
  let idle_in = match with_db(again, fn () => one_number(
    "select count(*)::int4 from pg_stat_activity where datname = current_database() and pid <> pg_backend_pid() and state like 'idle in transaction%' and query like '%khora_nstorm%'"
  )) { Result::Ok(c) => c, Result::Err(_) => 0 - 2 };
  close(again);
  print("trials " + Int::to_string(n) + ", left inside a transaction " + Int::to_string(open_after)
    + ", errors " + Int::to_string(errors) + ", connection replaced " + Int::to_string(replaced)
    + ", rows surviving " + Int::to_string(rows)
    + ", other connections idle in a transaction " + Int::to_string(idle_in));
}
"##;


// --- a lease's `db` stays on its fiber ----------------------------------------

/// The refusals the checker gives `main` compiled with `std` and the
/// postgres package, as each message and the text its span covers.
fn refusals_of(main: &str) -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("postgres_refusals");
    let db = KhoraDatabase::new();
    let files = sources(&db, &dir, main);
    let mine = *files.last().expect("the program is the last source");
    SourceRoot::new(&db, files);
    khora_types::diagnostics(&db, mine)
        .iter()
        .map(|e| {
            let (at, end) = (usize::from(e.range.start()), usize::from(e.range.end()));
            (e.message.clone(), main.get(at..end).unwrap_or("").to_string())
        })
        .collect()
}

/// The one-based line of the first occurrence of `what` in `text`.
fn line_of(text: &str, what: &str) -> usize {
    let at = text.find(what).expect("the text should be there");
    text[..at].matches('\n').count() + 1
}

/// Asserts that `found` is exactly one refusal at each of `uses`, in order:
/// the text a spawned fiber's use of its parent's `db` covers, and the text
/// that starts the line of the spawn that took it there. Each must name the
/// rewrite.
fn assert_refused_at(main: &str, found: &[&(String, String)], uses: &[(&str, &str)]) {
    assert_eq!(
        found.len(),
        uses.len(),
        "expected one refusal per use of the parent's `db`, got {found:#?}"
    );
    for ((message, covered), (used, spawn)) in found.iter().copied().zip(uses) {
        assert_eq!(covered, used, "the caret should be on the use: {message}");
        assert!(
            message.starts_with("`db` cannot be handed to another fiber"),
            "the refusal should name `db`: {message}"
        );
        assert!(
            message.contains(&format!("the fiber spawned at line {}, ", line_of(main, spawn))),
            "the refusal should name the spawn on the line of `{spawn}`: {message}"
        );
        assert!(
            message.contains("a fiber spawned inside a `with_db` body cannot use that body's `db`")
                && message.contains("take a lease in the spawned fiber instead")
                && message.contains("`Fiber::spawn(fn () => with_db(pool, work))`"),
            "the refusal should name the rewrite: {message}"
        );
    }
}

/// **Three fibers looping on their parent's lease are refused**, once per
/// fiber, at the call that needs `db`. This was the child-fiber phase of
/// `aborted_borrowers_never_lose_a_slot`, which takes a lease per child
/// instead. What it prevents: three fibers writing one lent connection's
/// `mut` fields, which the debug owner check traps on ("object made on
/// fiber 4 was counted on fiber 5") and a release build does not see.
#[test]
fn a_fiber_cannot_use_its_parents_lease() {
    let main = reconnect_program(
        "{ host: \"127.0.0.1\", port: 1, user: \"khora\", database: \"khora\", secret: \"\" }",
        r#"fn looper(n: Int) -> () with { db: Db } {
  let mut going = true;
  while going { let _ = db.query("stall " + Int::to_string(n), List::Nil); () }
}

fn fanout() -> Int with { db: Db } {
  let a = Fiber::spawn(fn () => looper(20));
  let b = Fiber::spawn(fn () => looper(21));
  let c = Fiber::spawn(fn () => looper(22));
  Fiber::wait(a);
  Fiber::wait(b);
  Fiber::wait(c);
  1
}

fn main() -> Int {
  let settings: Settings = SETTINGS;
  let pool = open(Fibers::open(), settings, 1);
  let _ = with_db(pool, fanout);
  close(pool);
  0
}
"#,
    );
    let found = refusals_of(&main);
    assert_refused_at(
        &main,
        &found.iter().collect::<Vec<_>>(),
        &[
            ("looper(20)", "let a = Fiber::spawn"),
            ("looper(21)", "let b = Fiber::spawn"),
            ("looper(22)", "let c = Fiber::spawn"),
        ],
    );
}

/// **Two fibers on one lease, each running a `transaction`, are refused.**
/// `both` in [`TWO_FIBERS_PROGRAM`]: the two fibers' levels were one
/// connection's, so each could release or roll back the other's savepoint,
/// and the package's depth guards turned that into an `Err` for both. A
/// fiber that takes its own lease has its own connection and its own depth.
/// Two fibers can still share one connection's depth through `over`, which
/// is where the guards are exercised now:
/// [`two_fibers_over_one_connection_get_answers_that_agree_with_the_rows`].
#[test]
fn two_fibers_nesting_on_one_lease_are_refused() {
    let found = refusals_of(TWO_FIBERS_PROGRAM);
    assert_eq!(found.len(), 4, "only `both` and `both_at_zero` should be refused: {found:#?}");
    let in_both: Vec<_> = found.iter().filter(|(_, covered)| covered.starts_with("fiber_")).collect();
    assert_refused_at(
        TWO_FIBERS_PROGRAM,
        &in_both,
        &[
            ("fiber_b(b_ok, b_open, c_open, b_done)", "let b = Fiber::spawn(fn () => fiber_b"),
            ("fiber_c(c_ok, b_open, c_open, b_done)", "let c = Fiber::spawn(fn () => fiber_c"),
        ],
    );
}

/// **Two views of one lease handed to two fibers are refused too.**
/// `both_at_zero` in [`TWO_FIBERS_PROGRAM`] wraps the lease's `db` in a
/// handler of its own in each fiber (`paused(lease(), ..)`), so both fibers
/// read depth 0 and both begin. The wrapper is built inside the spawned
/// fiber from the parent's `db`, and that use is what is refused, whatever
/// the wrapper does.
#[test]
fn two_views_of_one_lease_are_refused() {
    let found = refusals_of(TWO_FIBERS_PROGRAM);
    let at_lease: Vec<_> = found.iter().filter(|(_, covered)| covered == "lease()").collect();
    assert_refused_at(
        TWO_FIBERS_PROGRAM,
        &at_lease,
        &[
            ("lease()", "let b = Fiber::spawn(fn () => side"),
            ("lease()", "let c = Fiber::spawn(fn () => side"),
        ],
    );
}

/// Two fibers on one lease, each running a `transaction`: a program that
/// compiled and ran until a `Db` stayed on its fiber, and is kept whole as
/// what the refusal has to catch. `both` and `both_at_zero` are the two
/// shapes; everything else here compiles.
const TWO_FIBERS_PROGRAM: &str = r##"module demo::main;

//! Two fibers on one lease, each running a `transaction`, interleaved
//! deterministically with channels (the reviewer's `probes/twofib`).
//!
//! Case A (siblings inside the lease's outer transaction): B opens its
//! savepoint, C opens its own on top, B answers `Ok` and releases, then C
//! fails. Case B (no outer transaction): B begins, C nests on B's
//! transaction, B commits, then C fails.
//!
//! The oracle: every row is present exactly when every `transaction` that
//! wrote it was told `Ok` (and, in case A, the outer one too). Each case
//! prints `agree` or the rows that disagree, then whether the next lease
//! gets a connection outside a transaction.

import std::core::{Channel, Fiber, List, Option, Result, Shared, Show, print};
import std::core::{Fibers};
import std::db::{Cell, Db, DbError, Row, transaction};
import postgres::db::{Settings};
import postgres::pool::{Pool, close as close_pool, open as open_pool, with_db};

fn settings() -> Settings {
  { host: "127.0.0.1", port: 5433, user: "khora", database: "khora", secret: "khora" }
}

fn put(n: Int) -> Result<Int, DbError> with { db: Db } {
  db.execute("insert into khora_twofib (n) values ($1)", List::Cons(Cell::Number(n), List::Nil))
}

fn fresh() -> () with { db: Db } {
  let _ = db.execute("drop table if exists khora_twofib", List::Nil);
  let _ = db.execute("create table khora_twofib (n int4)", List::Nil);
}

fn present(n: Int) -> Bool with { db: Db } {
  match db.query("select count(*)::int4 from khora_twofib where n = $1", List::Cons(Cell::Number(n), List::Nil)) {
    Result::Ok(List::Cons(row, _)) => match row.cells {
      List::Cons(Cell::Number(k), _) => k > 0,
      _ => false,
    },
    _ => false,
  }
}

fn idle() -> String with { db: Db } {
  match db.execute("savepoint khora_probe_idle", List::Nil) {
    Result::Ok(_) => { let _ = db.rollback(); "left inside a transaction" },
    Result::Err(_) => "clean",
  }
}

fn ok(r: Result<Int, DbError>) -> Bool {
  match r { Result::Ok(_) => true, Result::Err(_) => false }
}

fn fiber_b(b_ok: Shared<Bool>, b_open: Channel<Int>, c_open: Channel<Int>, b_done: Channel<Int>) -> () with { db: Db } {
  let r = transaction(fn () => {
    let _ = put(10);
    let _ = Channel::send(b_open, 1);
    let _ = Channel::receive(c_open);
    Result::Ok(10)
  });
  Shared::set(b_ok, ok(r));
  let _ = Channel::send(b_done, 1);
}

fn fiber_c(c_ok: Shared<Bool>, b_open: Channel<Int>, c_open: Channel<Int>, b_done: Channel<Int>) -> () with { db: Db } {
  let _ = Channel::receive(b_open);
  let r: Result<Int, DbError> = transaction(fn () => {
    let _ = put(20);
    let _ = Channel::send(c_open, 1);
    let _ = Channel::receive(b_done);
    Result::Err(DbError::Rejected("C fails"))
  });
  Shared::set(c_ok, ok(r));
}

fn both(b_ok: Shared<Bool>, c_ok: Shared<Bool>) -> () with { db: Db } {
  let b_open: Channel<Int> = Channel::bounded(1);
  let c_open: Channel<Int> = Channel::bounded(1);
  let b_done: Channel<Int> = Channel::bounded(1);
  let b = Fiber::spawn(fn () => fiber_b(b_ok, b_open, c_open, b_done));
  let c = Fiber::spawn(fn () => fiber_c(c_ok, b_open, c_open, b_done));
  Fiber::wait(b);
  Fiber::wait(c);
}

/// What was told, as a line, and whether each row agrees with it.
fn verdict(pool: Pool, outer_ok: Bool, b_ok: Bool, c_ok: Bool, outer_rows: List<Int>) -> () {
  let mut wrong = "";
  let mut rest = outer_rows;
  let mut going = true;
  while going {
    match rest {
      List::Nil => going = false,
      List::Cons(n, more) => {
        let there = match with_db(pool, fn () => present(n)) { Result::Ok(p) => p, Result::Err(_) => false };
        if there != outer_ok { wrong = wrong + " row " + Int::to_string(n) };
        rest = more
      },
    }
  };
  let b_there = match with_db(pool, fn () => present(10)) { Result::Ok(p) => p, Result::Err(_) => false };
  let c_there = match with_db(pool, fn () => present(20)) { Result::Ok(p) => p, Result::Err(_) => false };
  if b_there != (b_ok && outer_ok) { wrong = wrong + " row 10 (B)" };
  if c_there != (c_ok && outer_ok) { wrong = wrong + " row 20 (C)" };
  let after = match with_db(pool, idle) { Result::Ok(s) => s, Result::Err(p) => "lease " + p.show() };
  print("  told: B " + (if b_ok { "Ok" } else { "Err" }) + ", C " + (if c_ok { "Ok" } else { "Err" })
    + (if outer_rows == List::Nil { "" } else if outer_ok { ", outer Ok" } else { ", outer Err" }));
  print("  " + (if wrong == "" { "agree" } else { "DISAGREE:" + wrong }) + "; next lease " + after);
}

fn case_a(pool: Pool) -> () {
  let _ = with_db(pool, fresh);
  print("A: two fibers nest inside one lease's transaction");
  let b_ok = Shared::of(false);
  let c_ok = Shared::of(false);
  let outer = with_db(pool, fn () => transaction(fn () => {
    let _ = put(1);
    both(b_ok, c_ok);
    put(3)
  }));
  let outer_ok = match outer { Result::Ok(Result::Ok(_)) => true, _ => false };
  verdict(pool, outer_ok, Shared::get(b_ok), Shared::get(c_ok), List::Cons(1, List::Cons(3, List::Nil)));
}

fn case_b(pool: Pool) -> () {
  let _ = with_db(pool, fresh);
  print("B: two fibers on one lease, no outer transaction");
  let b_ok = Shared::of(false);
  let c_ok = Shared::of(false);
  let _ = with_db(pool, fn () => both(b_ok, c_ok));
  verdict(pool, true, Shared::get(b_ok), Shared::get(c_ok), List::Nil);
}

/// The lease's handler, stopping after each `depth` answer until it is let
/// go: this is what lets case C have both fibers read the depth before
/// either begins.
fn paused(base: Db, read: Channel<Int>, go: Channel<Int>) -> Db {
  handler for Db {
    query: fn (sql, binds) => base.query(sql, binds),
    query_each: fn (sql, sets) => base.query_each(sql, sets),
    execute: fn (sql, binds) => base.execute(sql, binds),
    depth: fn () => {
      let d = base.depth();
      let _ = Channel::send(read, d);
      let _ = Channel::receive(go);
      d
    },
    begin: fn () => base.begin(),
    commit: fn () => base.commit(),
    rollback: fn () => base.rollback(),
    savepoint: fn level => base.savepoint(level),
    release: fn level => base.release(level),
    rollback_to: fn level => base.rollback_to(level),
    broken: fn () => base.broken(),
  }
}

fn lease() -> Db with { db: Db } {
  db
}

/// One side of case C: a transaction on `on` that writes `n`, then waits
/// for `hold` before answering (`Ok` if `succeeds`). It sends on `wrote`
/// once its body has written, or once `transaction` answers without running
/// the body, so either way the caller hears from it.
fn side(on: Db, n: Int, succeeds: Bool, wrote: Channel<Int>, hold: Channel<Int>, told: Shared<Bool>) -> () {
  with { db: on } {
    let ran = Shared::of(false);
    let r: Result<Int, DbError> = transaction(fn () => {
      Shared::set(ran, true);
      let _ = put(n);
      let _ = Channel::send(wrote, n);
      let _ = Channel::receive(hold);
      if succeeds { Result::Ok(n) } else { Result::Err(DbError::Rejected("C fails")) }
    });
    if !Shared::get(ran) { let _ = Channel::send(wrote, 0 - n); () };
    Shared::set(told, ok(r));
  }
}

fn both_at_zero(b_ok: Shared<Bool>, c_ok: Shared<Bool>) -> () with { db: Db } {
  let read: Channel<Int> = Channel::bounded(2);
  let go_b: Channel<Int> = Channel::bounded(1);
  let go_c: Channel<Int> = Channel::bounded(1);
  let wrote: Channel<Int> = Channel::bounded(2);
  let hold_b: Channel<Int> = Channel::bounded(1);
  let hold_c: Channel<Int> = Channel::bounded(1);
  // Two views of the one lease, so each fiber can be let go on its own.
  let b = Fiber::spawn(fn () => side(paused(lease(), read, go_b), 10, true, wrote, hold_b, b_ok));
  let c = Fiber::spawn(fn () => side(paused(lease(), read, go_c), 20, false, wrote, hold_c, c_ok));
  // Both have read the depth, and neither has begun.
  let _ = Channel::receive(read);
  let _ = Channel::receive(read);
  // B begins and writes; then C, which read the same depth 0, begins, and
  // either writes inside B's transaction or is refused.
  let _ = Channel::send(go_b, 1);
  let _ = Channel::receive(wrote);
  let _ = Channel::send(go_c, 1);
  let _ = Channel::receive(wrote);
  // B answers `Ok`, then C fails if its body ran at all.
  let _ = Channel::send(hold_b, 1);
  Fiber::wait(b);
  let _ = Channel::send(hold_c, 1);
  Fiber::wait(c);
}

fn case_c(pool: Pool) -> () {
  let _ = with_db(pool, fresh);
  print("C: two fibers on one lease both read depth 0, then both begin");
  let b_ok = Shared::of(false);
  let c_ok = Shared::of(false);
  let _ = with_db(pool, fn () => both_at_zero(b_ok, c_ok));
  verdict(pool, true, Shared::get(b_ok), Shared::get(c_ok), List::Nil);
}

fn main() -> () {
  let crew = Fibers::open();
  let pool = open_pool(crew, settings(), 1);
  case_a(pool);
  case_b(pool);
  case_c(pool);
  close_pool(pool);
}
"##;

// --- two fibers sharing one connection through `over` ------------------------

/// A server that keeps PostgreSQL's transaction and savepoint semantics for
/// one table of integers, on every connection it is given.
///
/// **Just the part that decides the answer to two fibers on one lease**:
/// `RELEASE` of a savepoint also releases every one opened after it, `ROLLBACK
/// TO` undoes them, `COMMIT` keeps every savepoint still open, and `SAVEPOINT`
/// outside a transaction is an error. That is what lets a test CI runs show
/// a row committed for a `transaction` that answered `Err`, without a real
/// server. Committed rows are shared between connections, as a table is.
fn tx_server(committed: std::sync::Arc<std::sync::Mutex<Vec<i64>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("an address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let committed = committed.clone();
            std::thread::spawn(move || tx_connection(stream, &committed));
        }
    });
    format!("{{ host: \"127.0.0.1\", port: {port}, user: \"khora\", database: \"khora\", secret: \"khora\" }}")
}

/// One savepoint (or the transaction itself, unnamed) and the rows written
/// since it was opened.
struct Frame {
    name: Option<String>,
    rows: Vec<i64>,
}

fn error_frame(code: &str, message: &str) -> Vec<u8> {
    let mut fields = Vec::new();
    fields.push(b'S');
    fields.extend_from_slice(&cstring("ERROR"));
    fields.push(b'C');
    fields.extend_from_slice(&cstring(code));
    fields.push(b'M');
    fields.extend_from_slice(&cstring(message));
    fields.push(0);
    framed(b'E', &fields)
}

/// Runs one statement against `frames` (empty outside a transaction),
/// answering its `CommandComplete` tag and any rows, or an error.
fn tx_statement(
    sql: &str,
    param: Option<i64>,
    frames: &mut Vec<Frame>,
    committed: &std::sync::Mutex<Vec<i64>>,
) -> Result<(String, Option<i64>), Vec<u8>> {
    let sql = sql.trim();
    let lower = sql.to_ascii_lowercase();
    let word = |i: usize| lower.split_whitespace().nth(i).unwrap_or("").to_string();
    if lower == "begin" {
        if frames.is_empty() {
            frames.push(Frame { name: None, rows: Vec::new() });
        }
        Ok(("BEGIN".into(), None))
    } else if lower == "commit" {
        let all: Vec<i64> = frames.drain(..).flat_map(|f| f.rows).collect();
        committed.lock().expect("the table").extend(all);
        Ok(("COMMIT".into(), None))
    } else if lower == "rollback" {
        frames.clear();
        Ok(("ROLLBACK".into(), None))
    } else if lower.starts_with("savepoint ") {
        if frames.is_empty() {
            return Err(error_frame("25P01", "SAVEPOINT can only be used in transaction blocks"));
        }
        frames.push(Frame { name: Some(word(1)), rows: Vec::new() });
        Ok(("SAVEPOINT".into(), None))
    } else if lower.starts_with("release savepoint ") || lower.starts_with("rollback to savepoint ") {
        let releasing = lower.starts_with("release");
        let name = word(if releasing { 2 } else { 3 });
        let Some(at) = frames.iter().rposition(|f| f.name.as_deref() == Some(name.as_str())) else {
            return Err(error_frame("3B001", &format!("savepoint \"{name}\" does not exist")));
        };
        if releasing {
            // The savepoint and every one opened after it go; their rows
            // become the enclosing level's.
            let rows: Vec<i64> = frames.drain(at..).flat_map(|f| f.rows).collect();
            frames.last_mut().expect("a transaction").rows.extend(rows);
            Ok(("RELEASE".into(), None))
        } else {
            frames.truncate(at + 1);
            frames[at].rows.clear();
            Ok(("ROLLBACK".into(), None))
        }
    } else if lower.starts_with("insert ") {
        let n = param.expect("a bound value");
        match frames.last_mut() {
            Some(top) => top.rows.push(n),
            None => committed.lock().expect("the table").push(n),
        }
        Ok(("INSERT 0 1".into(), None))
    } else if lower.starts_with("select count(*)") {
        let n = param.expect("a bound value");
        let count = committed.lock().expect("the table").iter().filter(|r| **r == n).count();
        Ok(("SELECT 1".into(), Some(count as i64)))
    } else if lower.starts_with("drop ") || lower.starts_with("create ") {
        committed.lock().expect("the table").clear();
        Ok(("CREATE TABLE".into(), None))
    } else {
        Err(error_frame("42601", &format!("the scripted server does not know `{sql}`")))
    }
}

fn tx_connection(mut stream: TcpStream, committed: &std::sync::Mutex<Vec<i64>>) {
    let _ = stream.set_nodelay(true);
    let mut length = [0u8; 4];
    if stream.read_exact(&mut length).is_err() {
        return;
    }
    let mut startup = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
    if stream.read_exact(&mut startup).is_err() {
        return;
    }
    let mut hello = framed(b'R', &0i32.to_be_bytes());
    hello.extend(framed(b'Z', b"I"));
    if stream.write_all(&hello).is_err() {
        return;
    }
    let mut frames: Vec<Frame> = Vec::new();
    let mut prepared = Statements::default();
    loop {
        // One request: a simple `Query`, or the extended protocol's frames up
        // to `Sync`, whose one parameter (if any) is an integer in text. The
        // SQL is in `Parse`, or in the `Parse` of an earlier request for the
        // statement `Bind` names.
        let mut simple = None;
        let mut sql = String::new();
        let mut param = None;
        loop {
            let Some((kind, payload)) = next_frame(&mut stream) else { return };
            match kind {
                b'X' => return,
                b'Q' => {
                    let end = payload.iter().position(|b| *b == 0).unwrap_or(payload.len());
                    simple = Some(String::from_utf8_lossy(&payload[..end]).into_owned());
                    break;
                }
                b'P' => sql = prepared.parse(&payload),
                b'B' => {
                    if let Some(known) = prepared.bound(&payload) {
                        sql = known;
                    }
                    param = bound_integer(&payload)
                }
                b'S' => break,
                _ => {}
            }
        }
        let mut reply = Vec::new();
        let statements: Vec<String> = match &simple {
            Some(text) => text.split(';').map(str::to_string).filter(|s| !s.trim().is_empty()).collect(),
            None => {
                reply.extend(framed(b'1', &[]));
                reply.extend(framed(b'2', &[]));
                vec![sql.clone()]
            }
        };
        for statement in statements {
            match tx_statement(&statement, param, &mut frames, committed) {
                Ok((tag, row)) => {
                    if let Some(value) = row {
                        let mut description = 1i16.to_be_bytes().to_vec();
                        description.extend_from_slice(&cstring("count"));
                        description.extend_from_slice(&0i32.to_be_bytes());
                        description.extend_from_slice(&0i16.to_be_bytes());
                        description.extend_from_slice(&23i32.to_be_bytes());
                        description.extend_from_slice(&4i16.to_be_bytes());
                        description.extend_from_slice(&(-1i32).to_be_bytes());
                        description.extend_from_slice(&0i16.to_be_bytes());
                        let text = value.to_string();
                        let mut data = 1i16.to_be_bytes().to_vec();
                        data.extend_from_slice(&(text.len() as i32).to_be_bytes());
                        data.extend_from_slice(text.as_bytes());
                        reply.extend(framed(b'T', &description));
                        reply.extend(framed(b'D', &data));
                    }
                    reply.extend(framed(b'C', &cstring(&tag)));
                }
                Err(error) => {
                    reply.extend(error);
                    break;
                }
            }
        }
        reply.extend(framed(b'Z', if frames.is_empty() { b"I" } else { b"T" }));
        if stream.write_all(&reply).is_err() {
            return;
        }
    }
}

/// The first parameter of a `Bind`, read as an integer in text.
fn bound_integer(payload: &[u8]) -> Option<i64> {
    let mut at = 0;
    for _ in 0..2 {
        at += payload[at..].iter().position(|b| *b == 0)? + 1;
    }
    let formats = i16::from_be_bytes(payload.get(at..at + 2)?.try_into().ok()?) as usize;
    at += 2 + 2 * formats;
    let count = i16::from_be_bytes(payload.get(at..at + 2)?.try_into().ok()?);
    at += 2;
    if count < 1 {
        return None;
    }
    let len = i32::from_be_bytes(payload.get(at..at + 4)?.try_into().ok()?);
    at += 4;
    if len < 0 {
        return None;
    }
    std::str::from_utf8(payload.get(at..at + len as usize)?).ok()?.parse().ok()
}

/// What each case of [`TWO_FIBERS_OVER_ONE_CONNECTION`] must print: every
/// answer agrees with the rows, and the connection is left outside a
/// transaction.
const TWO_FIBERS_OVER_EXPECTED: &str = "A: two fibers nest inside one connection's transaction\n  \
     told: B Err, C Err, outer Ok\n  agree; after it clean\n\
     B: two fibers on one connection, no outer transaction\n  \
     told: B Err, C Err\n  agree; after it clean\n\
     C: two fibers on one connection both read depth 0, then both begin\n  \
     told: B Ok, C Err\n  agree; after it clean\n";

/// **Two fibers sharing one connection through `serve` and `over` never get
/// an answer the rows disagree with.** This is what the package's depth
/// guards are for: a `Db` stays on its fiber, so a pool's lease cannot be
/// shared, but `over` hands each fiber a `Db` of its own over one serving
/// fiber's connection, and those fibers' transactions share its depth.
///
/// What this prevents: a row committed for a `transaction` that answered
/// `Err`. With loose depth guards, fiber B's `RELEASE` of level 1 also
/// released fiber C's level 2 on top of it (case A), and B's `COMMIT` kept
/// C's open savepoint (case B), so C's failure undid nothing. In case C
/// both fibers read depth 0 before either begins, and C's `BEGIN`, a warning
/// to PostgreSQL inside B's transaction, let C's `ROLLBACK` end B's
/// transaction and C's write be kept. The guards refuse an operation for a
/// level the connection is not exactly at, and turn a `COMMIT` with another
/// fiber's savepoint open into a `ROLLBACK`: in A and B both fibers are told
/// `Err` and neither row is kept, and in C, C's `BEGIN` is refused, so its
/// body never runs and B commits alone. Against a scripted server that keeps
/// PostgreSQL's savepoint rules, so CI runs it.
#[test]
fn two_fibers_over_one_connection_get_answers_that_agree_with_the_rows() {
    let committed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let settings = tx_server(committed);
    let main = TWO_FIBERS_OVER_ONE_CONNECTION.replace(
        "{ host: \"127.0.0.1\", port: 5433, user: \"khora\", database: \"khora\", secret: \"khora\" }",
        &settings,
    );
    assert_ne!(main, TWO_FIBERS_OVER_ONE_CONNECTION, "the settings should have been replaced");
    let exe = build("postgres_two_fibers_over_scripted", &main);
    for backend in ["threads", "scheduler"] {
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(60));
        assert!(!ran.hung, "{backend}: the program hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(ran.stdout, TWO_FIBERS_OVER_EXPECTED, "{backend}");
    }
}

/// The same program against the real server, whose savepoint rules are the
/// ones the scripted server copies.
#[test]
fn two_fibers_over_one_connection_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!("skipping: set KHORA_POSTGRES=1 and bring up packages/postgres/docker-compose.yml to run this");
        return;
    }
    let exe = build("postgres_two_fibers_over", TWO_FIBERS_OVER_ONE_CONNECTION);
    for backend in ["threads", "scheduler"] {
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(60));
        assert!(!ran.hung, "{backend}: the program hung: stdout {:?}", ran.stdout);
        assert_eq!(ran.code, Some(0), "{backend}: stderr {}", ran.stderr);
        assert_eq!(ran.stdout, TWO_FIBERS_OVER_EXPECTED, "{backend}");
    }
}

/// [`TWO_FIBERS_PROGRAM`]'s three cases, with every fiber given its own `Db`
/// over one serving fiber's connection (`over(requests)`) instead of its
/// parent's lease, which the checker refuses. Each fiber builds its `Db`
/// itself, so no `Db` crosses a fiber.
const TWO_FIBERS_OVER_ONE_CONNECTION: &str = r##"module demo::main;

import std::core::{Channel, Fiber, Iterator, List, Option, Result, Shared, Step, print};
import std::db::{Cell, Db, DbError, Row, transaction};
import postgres::db::{Request, Settings, over, serve};

fn settings() -> Settings {
  { host: "127.0.0.1", port: 5433, user: "khora", database: "khora", secret: "khora" }
}

fn put(n: Int) -> Result<Int, DbError> with { db: Db } {
  db.execute("insert into khora_twofib (n) values ($1)", List::Cons(Cell::Number(n), List::Nil))
}

fn fresh() -> () with { db: Db } {
  let _ = db.execute("drop table if exists khora_twofib", List::Nil);
  let _ = db.execute("create table khora_twofib (n int4)", List::Nil);
}

fn present(n: Int) -> Bool with { db: Db } {
  match db.query("select count(*)::int4 from khora_twofib where n = $1", List::Cons(Cell::Number(n), List::Nil)) {
    Result::Ok(List::Cons(row, _)) => match row.cells {
      List::Cons(Cell::Number(k), _) => k > 0,
      _ => false,
    },
    _ => false,
  }
}

fn idle() -> String with { db: Db } {
  match db.execute("savepoint khora_probe_idle", List::Nil) {
    Result::Ok(_) => { let _ = db.rollback(); "left inside a transaction" },
    Result::Err(_) => "clean",
  }
}

fn ok(r: Result<Int, DbError>) -> Bool {
  match r { Result::Ok(_) => true, Result::Err(_) => false }
}

fn fiber_b(b_ok: Shared<Bool>, b_open: Channel<Int>, c_open: Channel<Int>, b_done: Channel<Int>) -> () with { db: Db } {
  let r = transaction(fn () => {
    let _ = put(10);
    let _ = Channel::send(b_open, 1);
    let _ = Channel::receive(c_open);
    Result::Ok(10)
  });
  Shared::set(b_ok, ok(r));
  let _ = Channel::send(b_done, 1);
}

fn fiber_c(c_ok: Shared<Bool>, b_open: Channel<Int>, c_open: Channel<Int>, b_done: Channel<Int>) -> () with { db: Db } {
  let _ = Channel::receive(b_open);
  let r: Result<Int, DbError> = transaction(fn () => {
    let _ = put(20);
    let _ = Channel::send(c_open, 1);
    let _ = Channel::receive(b_done);
    Result::Err(DbError::Rejected("C fails"))
  });
  Shared::set(c_ok, ok(r));
}

fn both(requests: Channel<Request>, b_ok: Shared<Bool>, c_ok: Shared<Bool>) -> () {
  let b_open: Channel<Int> = Channel::bounded(1);
  let c_open: Channel<Int> = Channel::bounded(1);
  let b_done: Channel<Int> = Channel::bounded(1);
  let b = Fiber::spawn(fn () => with { db: over(requests) } { fiber_b(b_ok, b_open, c_open, b_done) });
  let c = Fiber::spawn(fn () => with { db: over(requests) } { fiber_c(c_ok, b_open, c_open, b_done) });
  Fiber::wait(b);
  Fiber::wait(c);
}

/// What was told, as a line, and whether each row agrees with it.
fn verdict(requests: Channel<Request>, outer_ok: Bool, b_ok: Bool, c_ok: Bool, outer_rows: List<Int>) -> () {
  with { db: over(requests) } {
    let mut wrong = "";
    for n in outer_rows {
      if present(n) != outer_ok { wrong = wrong + " row " + Int::to_string(n) }
    };
    if present(10) != (b_ok && outer_ok) { wrong = wrong + " row 10 (B)" };
    if present(20) != (c_ok && outer_ok) { wrong = wrong + " row 20 (C)" };
    let after = idle();
    print("  told: B " + (if b_ok { "Ok" } else { "Err" }) + ", C " + (if c_ok { "Ok" } else { "Err" })
      + (if outer_rows == List::Nil { "" } else if outer_ok { ", outer Ok" } else { ", outer Err" }));
    print("  " + (if wrong == "" { "agree" } else { "DISAGREE:" + wrong }) + "; after it " + after);
  }
}

fn case_a(requests: Channel<Request>) -> () {
  with { db: over(requests) } { fresh() };
  print("A: two fibers nest inside one connection's transaction");
  let b_ok = Shared::of(false);
  let c_ok = Shared::of(false);
  let outer = with { db: over(requests) } {
    transaction(fn () => {
      let _ = put(1);
      both(requests, b_ok, c_ok);
      put(3)
    })
  };
  let outer_ok = match outer { Result::Ok(_) => true, Result::Err(_) => false };
  verdict(requests, outer_ok, Shared::get(b_ok), Shared::get(c_ok), List::Cons(1, List::Cons(3, List::Nil)));
}

fn case_b(requests: Channel<Request>) -> () {
  with { db: over(requests) } { fresh() };
  print("B: two fibers on one connection, no outer transaction");
  let b_ok = Shared::of(false);
  let c_ok = Shared::of(false);
  both(requests, b_ok, c_ok);
  verdict(requests, true, Shared::get(b_ok), Shared::get(c_ok), List::Nil);
}

/// A `Db` over `base`, stopping after each `depth` answer until it is let
/// go: this is what lets case C have both fibers read the depth before
/// either begins.
fn paused(base: Db, read: Channel<Int>, go: Channel<Int>) -> Db {
  handler for Db {
    query: fn (sql, binds) => base.query(sql, binds),
    query_each: fn (sql, sets) => base.query_each(sql, sets),
    execute: fn (sql, binds) => base.execute(sql, binds),
    depth: fn () => {
      let d = base.depth();
      let _ = Channel::send(read, d);
      let _ = Channel::receive(go);
      d
    },
    begin: fn () => base.begin(),
    commit: fn () => base.commit(),
    rollback: fn () => base.rollback(),
    savepoint: fn level => base.savepoint(level),
    release: fn level => base.release(level),
    rollback_to: fn level => base.rollback_to(level),
    broken: fn () => base.broken(),
  }
}

/// One side of case C: a transaction on `on` that writes `n`, then waits
/// for `hold` before answering (`Ok` if `succeeds`). It sends on `wrote`
/// once its body has written, or once `transaction` answers without running
/// the body, so either way the caller hears from it.
fn side(on: Db, n: Int, succeeds: Bool, wrote: Channel<Int>, hold: Channel<Int>, told: Shared<Bool>) -> () {
  with { db: on } {
    let ran = Shared::of(false);
    let r: Result<Int, DbError> = transaction(fn () => {
      Shared::set(ran, true);
      let _ = put(n);
      let _ = Channel::send(wrote, n);
      let _ = Channel::receive(hold);
      if succeeds { Result::Ok(n) } else { Result::Err(DbError::Rejected("C fails")) }
    });
    if !Shared::get(ran) { let _ = Channel::send(wrote, 0 - n); () };
    Shared::set(told, ok(r));
  }
}

fn both_at_zero(requests: Channel<Request>, b_ok: Shared<Bool>, c_ok: Shared<Bool>) -> () {
  let read: Channel<Int> = Channel::bounded(2);
  let go_b: Channel<Int> = Channel::bounded(1);
  let go_c: Channel<Int> = Channel::bounded(1);
  let wrote: Channel<Int> = Channel::bounded(2);
  let hold_b: Channel<Int> = Channel::bounded(1);
  let hold_c: Channel<Int> = Channel::bounded(1);
  let b = Fiber::spawn(fn () => side(paused(over(requests), read, go_b), 10, true, wrote, hold_b, b_ok));
  let c = Fiber::spawn(fn () => side(paused(over(requests), read, go_c), 20, false, wrote, hold_c, c_ok));
  // Both have read the depth, and neither has begun.
  let _ = Channel::receive(read);
  let _ = Channel::receive(read);
  // B begins and writes; then C, which read the same depth 0, begins, and
  // either writes inside B's transaction or is refused.
  let _ = Channel::send(go_b, 1);
  let _ = Channel::receive(wrote);
  let _ = Channel::send(go_c, 1);
  let _ = Channel::receive(wrote);
  // B answers `Ok`, then C fails if its body ran at all.
  let _ = Channel::send(hold_b, 1);
  Fiber::wait(b);
  let _ = Channel::send(hold_c, 1);
  Fiber::wait(c);
}

fn case_c(requests: Channel<Request>) -> () {
  with { db: over(requests) } { fresh() };
  print("C: two fibers on one connection both read depth 0, then both begin");
  let b_ok = Shared::of(false);
  let c_ok = Shared::of(false);
  both_at_zero(requests, b_ok, c_ok);
  verdict(requests, true, Shared::get(b_ok), Shared::get(c_ok), List::Nil);
}

fn main() -> () {
  let requests: Channel<Request> = Channel::bounded(8);
  let server = Fiber::spawn(fn () => serve(settings(), requests));
  case_a(requests);
  case_b(requests);
  case_c(requests);
  Channel::close(requests);
  Fiber::wait(server);
}
"##;

// --- a pipelined batch whose borrower is canceled ------------------------------

/// Answers `select $1::int4 as n` with one `int4` row holding `$1`, pipelined
/// the way PostgreSQL answers it: each `Sync` ends a reply, and a request is
/// read until its `Sync` whatever came before.
///
/// **The first batch's replies stop half-way until the program says it has
/// canceled**: once every request of the batch is in, the first three
/// replies go out, the server says `B` on `control`, and the rest wait for a
/// byte back. The borrower is then canceled while its batch has replies
/// still to come. Everything asked after is answered at once.
///
/// Notes every `n` it answered in `heard`, in order.
fn answer_a_held_batch(listener: TcpListener, control: TcpListener, batch: usize) -> Vec<String> {
    let (mut stream, _) = listener.accept().expect("a connection");
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(20)));
    let mut heard = Vec::new();
    let mut length = [0u8; 4];
    if stream.read_exact(&mut length).is_err() {
        return heard;
    }
    let mut startup = vec![0u8; (i32::from_be_bytes(length) as usize).saturating_sub(4)];
    if stream.read_exact(&mut startup).is_err() {
        return heard;
    }
    let mut hello = framed(b'R', &0i32.to_be_bytes());
    hello.extend(framed(b'Z', b"I"));
    if stream.write_all(&hello).is_err() {
        return heard;
    }
    let mut control = Some(control);
    // Replies waiting to go out, and whether the one in progress parsed.
    let mut pending: Vec<Vec<u8>> = Vec::new();
    let mut value = String::new();
    let mut parsed = false;
    loop {
        let Some((kind, payload)) = next_frame(&mut stream) else { return heard };
        match kind {
            b'X' => return heard,
            b'P' => parsed = true,
            b'B' => {
                // Portal, statement, 0 format codes, 1 parameter, its length
                // and its text.
                let mut parts = payload.splitn(3, |b| *b == 0);
                let _portal = parts.next();
                let _statement = parts.next();
                let rest = parts.next().unwrap_or(&[]);
                let len = i32::from_be_bytes(rest[4..8].try_into().expect("a length")) as usize;
                value = String::from_utf8_lossy(&rest[8..8 + len]).into_owned();
            }
            b'S' => {
                let mut reply = Vec::new();
                if parsed {
                    reply.extend(framed(b'1', &[]));
                }
                reply.extend(framed(b'2', &[]));
                if parsed {
                    let mut description = 1i16.to_be_bytes().to_vec();
                    description.extend_from_slice(&cstring("n"));
                    description.extend_from_slice(&0i32.to_be_bytes());
                    description.extend_from_slice(&0i16.to_be_bytes());
                    description.extend_from_slice(&23i32.to_be_bytes());
                    description.extend_from_slice(&4i16.to_be_bytes());
                    description.extend_from_slice(&(-1i32).to_be_bytes());
                    description.extend_from_slice(&0i16.to_be_bytes());
                    reply.extend(framed(b'T', &description));
                }
                let mut row = 1i16.to_be_bytes().to_vec();
                row.extend_from_slice(&(value.len() as i32).to_be_bytes());
                row.extend_from_slice(value.as_bytes());
                reply.extend(framed(b'D', &row));
                reply.extend(framed(b'C', &cstring("SELECT 1")));
                reply.extend(framed(b'Z', b"I"));
                heard.push(value.clone());
                parsed = false;
                pending.push(reply);
                // The first statement prepares alone; the batch after it is
                // `batch - 1` requests in one write, held half-way.
                let holding = control.is_some() && heard.len() > 1;
                if !holding || pending.len() == batch - 1 {
                    let split = if holding { 3 } else { pending.len() };
                    let first: Vec<u8> = pending.drain(..split).flatten().collect();
                    if stream.write_all(&first).is_err() {
                        return heard;
                    }
                    if holding {
                        let (mut told, _) = control.take().expect("the control listener").accept().expect("the program");
                        let _ = told.set_read_timeout(Some(std::time::Duration::from_secs(20)));
                        let _ = told.write_all(b"B");
                        let mut canceled = [0u8; 1];
                        let _ = told.read_exact(&mut canceled);
                        let rest: Vec<u8> = pending.drain(..).flatten().collect();
                        if stream.write_all(&rest).is_err() {
                            return heard;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The program for [`a_borrower_canceled_mid_batch_leaves_the_connection_usable`].
///
/// A pool of one. A fiber runs a batch of `BATCH` on its lease and is
/// canceled once the server has sent the first three replies and holds the
/// rest; the batch's replies are then let through. The next borrower asks
/// 101, 102 and 103 one at a time on the same connection, then a batch of
/// 201 to 203, and prints what it got.
fn canceled_batch_program(port: u16, told: u16, batch: usize) -> String {
    format!(
        "module demo::main;
import std::core::{{Array, Fiber, Fibers, List, Result, print}};
import std::db::{{Cell, Db, DbError, Row}};
import std::net::socket::{{start, connect_to, receive, transmit, shut}};
import postgres::db::{{Settings}};
import postgres::pool::{{close, open, with_db}};

fn sets(from: Int, count: Int) -> List<List<Cell>> {{
  let mut out: List<List<Cell>> = List::Nil;
  let mut n = from + count - 1;
  while n >= from {{
    out = List::Cons(List::Cons(Cell::Number(n), List::Nil), out);
    n = n - 1
  }};
  out
}}

fn shown(answer: Result<List<Row>, DbError>) -> String {{
  match answer {{
    Result::Ok(List::Cons(row, List::Nil)) => match row.cells {{
      List::Cons(Cell::Number(n), List::Nil) => Int::to_string(n),
      _ => \"a row of another shape\",
    }},
    Result::Ok(_) => \"not one row\",
    Result::Err(why) => why.show(),
  }}
}}

fn batch() -> Int with {{ db: Db }} {{
  let answers = db.query_each(\"select $1::int4 as n\", sets(1, {batch}));
  List::length(answers)
}}

fn after() -> String with {{ db: Db }} {{
  let one = shown(db.query(\"select $1::int4 as n\", List::Cons(Cell::Number(101), List::Nil)));
  let two = shown(db.query(\"select $1::int4 as n\", List::Cons(Cell::Number(102), List::Nil)));
  let three = shown(db.query(\"select $1::int4 as n\", List::Cons(Cell::Number(103), List::Nil)));
  let each = List::map(db.query_each(\"select $1::int4 as n\", sets(201, 3)), shown);
  one + \" \" + two + \" \" + three + \" | \" + String::join(each, \" \")
}}

fn main() -> Int {{
  let settings: Settings = {{ host: \"127.0.0.1\", port: {port}, user: \"khora\", database: \"khora\", secret: \"\" }};
  if start() {{}} else {{ print(\"no sockets\") }};
  let crew = Fibers::open();
  let pool = open(crew, settings, 1);
  let f = Fiber::spawn(fn () => {{
    match with_db(pool, batch) {{
      Result::Ok(n) => print(\"the canceled batch finished with \" + Int::to_string(n) + \" answers\"),
      Result::Err(_) => print(\"the canceled batch had no lease\"),
    }}
  }});
  let control = connect_to(\"127.0.0.1\", {told});
  let one: Array<U8> = Array::new(1, 0);
  let _ = receive(control, one);
  Fiber::cancel(f);
  let _ = transmit(control, \"c\");
  shut(control);
  Fiber::wait(f);
  match with_db(pool, after) {{
    Result::Ok(said) => print(\"next borrower: \" + said),
    Result::Err(why) => print(\"next borrower had no lease: \" + why.show()),
  }};
  close(pool);
  0
}}
"
    )
}

/// **A borrower canceled while its batch's replies are still arriving
/// leaves the connection usable, and the next borrower gets its own
/// answers.**
///
/// What this prevents: the rest of a canceled batch's replies read as the
/// next borrower's answers. The borrower reads its own batch on its own
/// fiber, so a cancel stops it part-way, with replies still arriving. The
/// connection counts the replies it is owed -- one per set -- and whoever
/// touches it next reads them all first: the give-back sends it to its slot,
/// which settles it and lends it again. Counted as a flag, the first set's
/// `ReadyForQuery` cleared it, the give-back put the connection straight back
/// in `idle`, the next borrower's lease check found the rest of the batch
/// waiting and discarded the connection, and this server, which accepts one
/// connection, never saw the next borrower: the test fails either way the
/// connection is left out of step.
///
/// The first statement of the batch prepares the statement; the other
/// `BATCH - 1` go out in one write, and the server sends three replies,
/// holds the rest until the cancel has been delivered, then sends them.
#[test]
fn a_borrower_canceled_mid_batch_leaves_the_connection_usable() {
    const BATCH: usize = 8;
    for backend in ["threads", "scheduler"] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let control = TcpListener::bind("127.0.0.1:0").expect("a control port");
        let told = control.local_addr().expect("an address").port();
        let server = std::thread::spawn(move || answer_a_held_batch(listener, control, BATCH));
        let exe = build(
            &format!("pg_canceled_batch_{backend}_{port}"),
            &canceled_batch_program(port, told, BATCH),
        );
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(30));
        let heard = server.join().expect("the scripted server");
        assert!(!ran.hung, "{backend}: the program hung: {:?}", ran.stdout);
        assert_eq!(
            ran.stdout, "next borrower: 101 102 103 | 201 202 203\n",
            "{backend}: stderr {}; the server answered {heard:?}",
            ran.stderr
        );
        let expected: Vec<String> =
            (1..=BATCH).map(|n| n.to_string()).chain(["101", "102", "103", "201", "202", "203"].map(String::from)).collect();
        assert_eq!(heard, expected, "{backend}: the whole batch, then the next borrower's statements, in order");
    }
}

/// The program for [`query_each_against_a_real_server`].
const QUERY_EACH_REAL: &str = r#"module demo::main;
import std::core::{Fibers, List, Result, String, print};
import std::db::{Cell, Db, DbError, Row, transaction};
import postgres::db::{Settings};
import postgres::pool::{close, open, with_db};

fn sets(values: List<Int>) -> List<List<Cell>> {
  List::map(values, fn n => List::Cons(Cell::Number(n), List::Nil))
}

fn shown(answer: Result<List<Row>, DbError>) -> String {
  match answer {
    Result::Ok(List::Cons(row, List::Nil)) => match row.cells {
      List::Cons(Cell::Number(n), List::Nil) => Int::to_string(n),
      _ => "a row of another shape",
    },
    Result::Ok(_) => "not one row",
    Result::Err(DbError::Rejected(_)) => "rejected",
    Result::Err(DbError::Disconnected(_)) => "disconnected",
    Result::Err(DbError::RolledBack(_)) => "rolled back",
  }
}

fn said(answers: List<Result<List<Row>, DbError>>) -> String {
  String::join(List::map(answers, shown), " ")
}

/// 100 / n: 0 is refused by the server, and only that set.
fn divided(values: List<Int>) -> String with { db: Db } {
  said(db.query_each("select (100 / $1::int4)::int4 as n", sets(values)))
}

fn batches() -> () with { db: Db } {
  // First use: prepared by the first set, the rest pipelined.
  print("first:   " + divided([1, 2, 4]));
  // Prepared: one write. The zero fails alone.
  print("alone:   " + divided([5, 0, 10, 20]));
  // The server drops every prepared statement; the batch is refused
  // throughout, and the sets after the first are asked again.
  match db.execute("discard all", List::Nil) {
    Result::Ok(_) => (),
    Result::Err(_) => print("discard refused"),
  };
  print("dropped: " + divided([25, 50, 100]));
  print("after:   " + divided([1, 2]));
  // Inside a transaction a failed set aborts it: the sets after it are
  // refused, as separate `query` calls would be, and the transaction rolls
  // back.
  let inside: Result<String, DbError> = transaction(fn () => Result::Ok(divided([1, 0, 2])));
  match inside {
    Result::Ok(s) => print("in tx:   " + s),
    Result::Err(_) => print("in tx:   failed"),
  };
  print("then:    " + divided([4]));
}

fn main() -> Int {
  let settings: Settings = { host: "127.0.0.1", port: 5433, user: "khora", database: "khora", secret: "khora" };
  let crew = Fibers::open();
  let pool = open(crew, settings, 1);
  match with_db(pool, batches) {
    Result::Ok(_) => (),
    Result::Err(_) => print("no lease"),
  };
  close(pool);
  0
}
"#;

/// **`query_each` against a real server answers what `query` once per set
/// would.** A set the server refuses fails alone outside a transaction; a
/// batch after `DISCARD ALL` recovers after the first set, as separate calls
/// do; inside a transaction the first failure aborts it and every set after
/// it is refused. Skipped without `KHORA_POSTGRES`, like its neighbors.
#[test]
fn query_each_against_a_real_server() {
    if std::env::var_os("KHORA_POSTGRES").is_none() {
        eprintln!(
            "skipping: set KHORA_POSTGRES=1 and bring up \
             packages/postgres/docker-compose.yml to run this"
        );
        return;
    }
    for backend in ["threads", "scheduler"] {
        let exe = build(&format!("pg_query_each_real_{backend}"), QUERY_EACH_REAL);
        let ran = run_watched(&exe, backend, std::time::Duration::from_secs(30));
        assert!(!ran.hung, "{backend}: the program hung: {:?}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "first:   100 50 25\n\
             alone:   20 rejected 10 5\n\
             dropped: rejected 2 1\n\
             after:   100 50\n\
             in tx:   failed\n\
             then:    25\n",
            "{backend}: stderr {}",
            ran.stderr
        );
    }
}
