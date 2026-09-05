#![cfg(feature = "llvm")]

//! `HttpClient`, against servers that answer in every shape it has to read.
//!
//! The server upstairs frames a body one way — `Content-Length` — because that
//! is the only way a *request* is framed in practice. An answer has three, and
//! a client meets all three in a morning: a length, a chunked body, or nothing
//! but the close. So the counterpart on the other side of these tests is
//! written in Rust and says exactly what each case needs it to say, which a
//! Khora server could not be made to do without teaching it to answer wrongly.
//!
//! **One `#[test]` per listener, on ports of their own.** `tests/http.rs`
//! explains why: cargo runs a binary's tests concurrently, and two tests each
//! binding a port is one failure and one reset.

use crate::harness;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;

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

/// Compiles `body` as the whole of `main` and runs it.
fn run(name: &str, body: &str) -> String {
    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, List, Map, Option, Result, Show, print}};
import std::net::http::{{Answer, Call, CallError, HttpClient, Method, Url, parse_url}};

fn said(why: CallError) -> String {{
  match why {{
    CallError::BadUrl(m) => "bad url: " + m,
    CallError::Unreachable(m) => "unreachable: " + m,
    CallError::Insecure(m) => "insecure: " + m,
    CallError::Closed(m) => "closed: " + m,
    CallError::Malformed(m) => "malformed: " + m,
    CallError::TooLarge(n) => "too large: " + Int::to_string(n),
    CallError::Denied(m) => "denied: " + m,
  }}
}}

/// Status, then body, then whichever headers a test asked about.
fn report(answer: Result<Answer, CallError>) -> () {{
  match answer {{
    Result::Err(why) => print(said(why)),
    Result::Ok(got) => {{
      print(Int::to_string(got.status));
      print(got.body);
    }},
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
    let root = SourceRoot::new(&db, sources(&db, &dir, &main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

// --- a server that answers however a test needs it to -----------------------

/// Serves one connection with `answer`, and hands back what was asked.
///
/// Raw bytes rather than a response builder: the point of every one of these
/// is a framing the builder would not produce.
fn serving(answer: &'static [u8]) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else { return };
        let request = read_request(&mut socket);
        let _ = socket.write_all(answer);
        let _ = socket.flush();
        // Every request the client makes carries `Connection: close`, so
        // closing is what ends the answer — and for the two framings that have
        // no length, it is the only thing that does.
        drop(socket);
        let _ = send.send(request);
    });
    (port, receive)
}

/// Reads one request: headers, then whatever `Content-Length` promised.
fn read_request(socket: &mut TcpStream) -> String {
    let mut got: Vec<u8> = Vec::new();
    let head = loop {
        if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let mut chunk = [0u8; 4096];
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&got).into_owned(),
            Ok(n) => got.extend_from_slice(&chunk[..n]),
        }
    };
    let length: usize = String::from_utf8_lossy(&got[..head])
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().to_string())
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while got.len() < head + length {
        let mut chunk = [0u8; 4096];
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8_lossy(&got).into_owned()
}

// --- reading a URL ----------------------------------------------------------

/// Everything a client has to get right before it can open a socket, and none
/// of it needs one.
#[test]
fn a_url_is_taken_apart_the_way_a_dialler_needs_it() {
    let out = run(
        "client_urls",
        r#"  let shown = fn (text: String) => match parse_url(text) {
    Option::None => print("no"),
    Option::Some(url) => print(
      (if url.secure { "https" } else { "http" })
        + " " + url.host + " " + Int::to_string(url.port) + " " + url.target
    ),
  };
  shown("http://example.com/things?a=1");
  shown("https://example.com/things");
  shown("http://example.com:8080/");
  shown("https://example.com");
  shown("HTTP://Example.com/X");
  shown("http://[::1]:9000/v1");
  shown("http://[::1]");
  shown("ftp://example.com/");
  shown("example.com/things");
  shown("http:///things");
  shown("http://example.com:/x");
  shown("http://example.com:notanumber/x");
  shown("http://example.com:99999/x");"#,
    );
    assert_eq!(
        out,
        // The default port comes from the scheme, a missing path is `/`, the
        // scheme folds case and the host does not, and everything that is not
        // a URL this can dial is refused rather than guessed at.
        "http example.com 80 /things?a=1\n\
         https example.com 443 /things\n\
         http example.com 8080 /\n\
         https example.com 443 /\n\
         http Example.com 80 /X\n\
         http ::1 9000 /v1\n\
         http ::1 80 /\n\
         no\nno\nno\nno\nno\nno\n"
    );
}

/// A URL the client cannot read never reaches a socket, and says which one.
#[test]
fn an_unreadable_url_is_refused_before_anything_is_opened() {
    let out = run(
        "client_bad_url",
        r#"  with { http: HttpClient::real() } {
    report(http.send(Call::get("not a url")));
  }"#,
    );
    assert_eq!(out, "bad url: not a url\n");
}

// --- the three ways an answer ends ------------------------------------------

/// `Content-Length`, which is what almost every answer uses.
#[test]
fn an_answer_framed_by_its_length_is_read() {
    let answer = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello";
    let (port, asked) = serving(answer);
    let out = run(
        "client_length",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/things?a=1")));
  }}"#
        ),
    );
    assert_eq!(out, "200\nhello\n");

    let request = asked.recv().expect("the server saw a request");
    assert!(request.starts_with("GET /things?a=1 HTTP/1.1\r\n"), "{request}");
    assert!(request.contains("Host: 127.0.0.1:"), "the port is kept when it is not the default: {request}");
    assert!(request.contains("Connection: close\r\n"), "{request}");
    assert!(request.contains("Content-Length: 0\r\n"), "a body-less request still says so: {request}");
}

/// **A chunked answer**, which the server upstairs cannot produce and every
/// real one does.
#[test]
fn a_chunked_answer_is_reassembled() {
    let answer = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   5\r\nhello\r\n1\r\n \r\n5\r\nworld\r\n0\r\n\r\n";
    let (port, _asked) = serving(answer);
    let out = run(
        "client_chunked",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/")));
  }}"#
        ),
    );
    assert_eq!(out, "200\nhello world\n", "three chunks, one body, no framing left in it");
}

/// A chunk size may carry extensions after a `;`, and they are ignored.
#[test]
fn a_chunk_extension_is_ignored() {
    let answer = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   3;name=value\r\nabc\r\n0\r\n\r\n";
    let (port, _asked) = serving(answer);
    let out = run(
        "client_chunk_ext",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/")));
  }}"#
        ),
    );
    assert_eq!(out, "200\nabc\n");
}

/// A trailer after the last chunk is read past rather than into the body.
#[test]
fn a_trailer_after_the_last_chunk_is_not_body() {
    let answer = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   2\r\nhi\r\n0\r\nExpires: never\r\n\r\n";
    let (port, _asked) = serving(answer);
    let out = run(
        "client_trailer",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/")));
  }}"#
        ),
    );
    assert_eq!(out, "200\nhi\n");
}

/// **Neither header**, which is how HTTP/1.0 and some proxies answer: the body
/// runs to the close.
#[test]
fn an_answer_framed_only_by_the_close_is_read() {
    let answer = b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nto the end";
    let (port, _asked) = serving(answer);
    let out = run(
        "client_until_close",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/")));
  }}"#
        ),
    );
    assert_eq!(out, "200\nto the end\n", "an older version is answered, not refused");
}

// --- what the caller is told ------------------------------------------------

/// A 404 arrived successfully. `ok` is about the status and a `CallError` is
/// about not having an answer at all — conflating them is how a client ends up
/// retrying a "not found".
#[test]
fn a_refusal_is_an_answer_and_not_an_error() {
    let answer = b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nno such x";
    let (port, _asked) = serving(answer);
    let out = run(
        "client_404",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    match http.send(Call::get("http://127.0.0.1:{port}/nope")) {{
      Result::Err(why) => print(said(why)),
      Result::Ok(got) => {{
        print(Int::to_string(got.status));
        print(if got.ok() {{ "ok" }} else {{ "not ok" }});
        print(got.body);
      }},
    }}
  }}"#
        ),
    );
    assert_eq!(out, "404\nnot ok\nno such x\n");
}

/// Headers come back keyed by the lowercased name, as the server's do.
#[test]
fn headers_are_read_without_regard_to_case() {
    let answer =
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Trace-Id: abc123\r\nContent-Length: 2\r\n\r\n{}";
    let (port, _asked) = serving(answer);
    let out = run(
        "client_headers",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    match http.send(Call::get("http://127.0.0.1:{port}/")) {{
      Result::Err(why) => print(said(why)),
      Result::Ok(got) => {{
        print(match got.header("Content-Type") {{ Option::Some(v) => v, Option::None => "-" }});
        print(match got.header("x-trace-id") {{ Option::Some(v) => v, Option::None => "-" }});
        print(match got.header("X-Missing") {{ Option::Some(v) => v, Option::None => "-" }});
      }},
    }}
  }}"#
        ),
    );
    assert_eq!(out, "application/json\nabc123\n-\n");
}

/// What a POST puts on the wire, which is the half the answer cannot show.
#[test]
fn a_post_sends_its_body_and_the_headers_it_was_given() {
    let answer = b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok";
    let (port, asked) = serving(answer);
    let out = run(
        "client_post",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    let call = Call::with_header(
      Call::json("http://127.0.0.1:{port}/entries", "{{\"memo\":\"rent\"}}"),
      "Authorization", "Bearer t0ken"
    );
    report(http.send(call));
  }}"#
        ),
    );
    assert_eq!(out, "201\nok\n");

    let request = asked.recv().expect("the server saw a request");
    assert!(request.starts_with("POST /entries HTTP/1.1\r\n"), "{request}");
    assert!(request.contains("Content-Type: application/json\r\n"), "{request}");
    assert!(request.contains("Authorization: Bearer t0ken\r\n"), "{request}");
    assert!(request.contains("Content-Length: 15\r\n"), "the JSON is fifteen bytes: {request}");
    assert!(request.ends_with("{\"memo\":\"rent\"}"), "the body is last and unaltered: {request}");
}

/// Nothing listening is `Unreachable`, and it says where it tried.
#[test]
fn a_closed_port_is_unreachable() {
    // Bound and dropped: the port is almost certainly free and nothing is on it.
    let port = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        listener.local_addr().expect("an address").port()
    };
    let out = run(
        "client_unreachable",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/")));
  }}"#
        ),
    );
    assert!(out.starts_with("unreachable: 127.0.0.1:"), "{out}");
}

/// An answer that promises more than it sends is short, not silently truncated.
#[test]
fn an_answer_that_stops_short_of_its_length_is_an_error() {
    let answer = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nonly this much";
    let (port, _asked) = serving(answer);
    let out = run(
        "client_short",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/")));
  }}"#
        ),
    );
    assert_eq!(out, "closed: the answer stopped short of its length\n");
}

/// Bytes that are not an answer are named as such rather than parsed into
/// something plausible.
#[test]
fn something_that_is_not_http_is_malformed() {
    let answer = b"NOT-HTTP hello\r\n\r\n";
    let (port, _asked) = serving(answer);
    let out = run(
        "client_malformed",
        &format!(
            r#"  with {{ http: HttpClient::real() }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/")));
  }}"#
        ),
    );
    assert!(out.starts_with("malformed: "), "{out}");
}

/// The limit is the caller's, and exceeding it is an error rather than a body
/// with the end quietly missing.
#[test]
fn an_answer_over_the_limit_is_refused() {
    let big = "x".repeat(4096);
    let answer: &'static [u8] =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{big}", big.len())
            .into_bytes()
            .leak();
    let (port, _asked) = serving(answer);
    let out = run(
        "client_too_large",
        &format!(
            r#"  with {{ http: HttpClient::bounded(512) }} {{
    report(http.send(Call::get("http://127.0.0.1:{port}/")));
  }}"#
        ),
    );
    assert_eq!(out, "too large: 512\n");
}

// --- the capability is the point --------------------------------------------

/// **A handler answers without a socket**, which is the whole reason this is an
/// effect. A test for code that calls an API should not need the API.
#[test]
fn a_double_needs_no_network() {
    let out = run(
        "client_double",
        r#"  let canned = handler for HttpClient {
    send: fn call => match call.method {
      Method::Get => Result::Ok({ status: 200, headers: Map::new(), body: "from the double" }),
      _ => Result::Err(CallError::Malformed("this double only answers GET")),
    },
  };
  with { http: canned } {
    report(http.send(Call::get("https://api.example.com/v1/thing")));
    report(http.send(Call::post("https://api.example.com/v1/thing", "{}")));
  }"#,
    );
    assert_eq!(out, "200\nfrom the double\nmalformed: this double only answers GET\n");
}

// --- both halves, in one process --------------------------------------------

/// **The client against Khora's own server**, which is the only test here that
/// can disagree with itself.
///
/// Everything above answers with bytes a Rust thread wrote, so it proves the
/// client reads HTTP. This proves the two halves of `std::net::http` agree
/// about it: the request the client renders is one the server's parser
/// accepts, and the response the server renders is one the client's parser
/// reads back. A framing mistake shared by both would pass every test above
/// and fail this one.
#[test]
fn the_client_and_the_server_understand_each_other() {
    // Bound and released, so the server below almost certainly gets it.
    let port = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        listener.local_addr().expect("an address").port()
    };

    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, Fiber, Fibers, List, Map, Option, Result, SharedFn, Show, attempt, print}};
import std::net::http::{{
  Answer, Call, CallError, HttpClient, HttpError, Params, Request, Response, Router
}};

fn greet(request: Request) -> Response {{
  Response::text(200, "hello " + match Params::get(request.params, "name") {{
    Option::Some(name) => name,
    Option::None => "nobody",
  }})
}}

fn echo(request: Request) -> Response {{
  Response::with_header(
    Response::text(201, "{{\"got\":\"" + request.body + "\"}}"),
    "X-Seen", "yes"
  )
}}

fn serve() -> () {{
  let router = Router::new()
    |> Router::get("/greet/:name", SharedFn::of(fn r => greet(r)))
    |> Router::post("/echo", SharedFn::of(fn r => echo(r)));
  Router::listen(router, {port})! catch {{
    _ => print("the server stopped"),
  }}
}}

fn ask(client: HttpClient, call: Call) -> () {{
  with {{ http: client }} {{
    match http.send(call) {{
      Result::Err(_) => print("no answer"),
      Result::Ok(got) => {{
        print(Int::to_string(got.status));
        print(got.body);
        print(match got.header("x-seen") {{ Option::Some(v) => v, Option::None => "-" }});
      }},
    }}
  }}
}}

fn main() -> () {{
  let crew = Fibers::open();
  Fibers::adopt(crew, Fiber::spawn(fn () => serve()));

  // The server prints "listening on ..." before it accepts, and a call that
  // arrives first would be refused — so the first call is retried rather than
  // the test being a race about scheduling order.
  let client = HttpClient::real();
  let mut tries = 0;
  let mut going = true;
  while going {{
    with {{ http: client }} {{
      match http.send(Call::get("http://127.0.0.1:{port}/greet/ada")) {{
        Result::Ok(got) => {{
          print(Int::to_string(got.status));
          print(got.body);
          going = false
        }},
        Result::Err(_) => {{
          tries = tries + 1;
          if tries > 200 {{ print("never came up"); going = false }}
        }},
      }}
    }}
  }};

  ask(client, Call::json("http://127.0.0.1:{port}/echo", "rent"));
  ask(client, Call::get("http://127.0.0.1:{port}/nowhere"));
}}
"#
    );

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("client_round_trip");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, &main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling the round trip failed:\n  {}\n\n{main}", messages.join("\n  "));
    }

    // The server never returns, so the program is killed once it has said
    // everything the test is waiting for.
    let mut child = std::process::Command::new(&exe)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the program should start");
    let mut out = child.stdout.take().expect("a pipe");

    let mut said = String::new();
    let mut buffer = [0u8; 1024];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while said.lines().count() < 9 && std::time::Instant::now() < deadline {
        match out.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => said.push_str(&String::from_utf8_lossy(&buffer[..n])),
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    let said = said.replace("\r\n", "\n");
    let lines: Vec<&str> = said.lines().collect();
    assert!(lines.first().is_some_and(|l| l.starts_with("listening on")), "{said}");
    assert_eq!(&lines[1..3], ["200", "hello ada"], "a path parameter came back: {said}");
    assert_eq!(
        &lines[3..6],
        ["201", "{\"got\":\"rent\"}", "yes"],
        "a POST body reached the handler and an extra header came back: {said}"
    );
    assert_eq!(
        &lines[6..9],
        ["404", "no route for /nowhere", "-"],
        "an unmounted route is a 404, and a header the answer does not carry is absent          rather than empty: {said}"
    );
}
