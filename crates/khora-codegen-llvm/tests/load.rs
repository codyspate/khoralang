#![cfg(feature = "llvm")]

//! What happens when there is more work than there is capacity to do it.
//!
//! Roadmap 13.2. `khora-rt`'s soak (11F) asks whether the scheduler's
//! *arithmetic* survives adversarial execution, and it does. This asks a
//! different question, one level up and in Khora rather than in Rust: when a
//! program is offered more work than it can serve, does it decline at the edge
//! or does it queue until it dies?
//!
//! **Every claim here is already written down as prose.** `bounded_nursery`
//! says overload "becomes latency instead of collapse"; `Channel` says a full
//! one "stops the sender until a taker catches up", and that this is "how a
//! service under more load than it can serve declines work at the edge instead
//! of queueing it until memory runs out". Neither sentence was tested. A
//! design document is not evidence, and the failure it describes is the kind
//! that only ever appears in production.
//!
//! The measurement is a `Shared` gauge: each fiber raises a counter on the way
//! in and lowers it on the way out, and the same update records the highest it
//! ever reached. One cell rather than two, because a peak read from a second
//! cell is a peak read from a different moment.

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

/// Compiles `items` and `body` and runs the program.
fn run(name: &str, items: &str, body: &str) -> String {
    let main = format!(
        r#"module demo::main;
import std::core::{{
  Changed, Channel, Eq, Fibers, List, Nursery, Option, Result, Share, Shared, Show, Task,
  bounded_nursery, print
}};

extern fn khora_live_count() -> Int;

/// How many fibers are inside at once, and the most there have ever been.
///
/// One cell, because a peak kept in a second one is a peak from a different
/// moment. `Shared::update` runs under the lock, so the raise and the record
/// are one step.
pub type Gauge = {{ live: Int, peak: Int }};

fn entered(gauge: Shared<Gauge>) -> () {{
  Shared::update(gauge, fn now => {{
    live: now.live + 1,
    peak: if now.live + 1 > now.peak {{ now.live + 1 }} else {{ now.peak }},
  }});
}}

fn left(gauge: Shared<Gauge>) -> () {{
  Shared::update(gauge, fn now => {{ live: now.live - 1, peak: now.peak }});
}}

/// Enough arithmetic to keep a fiber on a worker for a moment.
fn spin(rounds: Int) -> Int {{
  let mut i = 0;
  let mut total = 0;
  while i < rounds {{
    total = total + i;
    i = i + 1
  }};
  total
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
        SourceFile::new(&db, dir.join("main.kh"), main.clone()),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "`{name}` did not exit cleanly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

// --- a bound that actually bounds -------------------------------------------

/// **The claim `bounded_nursery` makes about itself.** Two hundred pieces of
/// work into a nursery of eight: every one runs, and never more than eight at
/// a time.
///
/// The limit is what turns a ceiling met by exhausting memory into one met by
/// starting work more slowly. If the peak came back at two hundred the
/// sentence in `std/core.kh` would be decoration.
#[test]
fn a_bounded_nursery_never_exceeds_its_limit() {
    let out = run(
        "load_nursery_bound",
        r#"fn worker(gauge: Shared<Gauge>, done: Shared<Int>) -> () {
  entered(gauge);
  let _ = spin(2000);
  Shared::update(done, fn n => n + 1);
  left(gauge);
}

fn crowd<'e>(gauge: Shared<Gauge>, done: Shared<Int>) -> ()
  with { 'e | nursery: Nursery }
{
  let mut i = 0;
  while i < 200 {
    nursery.adopt(Task::spawn(fn () => worker(gauge, done)));
    i = i + 1
  };
}

fn workload() -> () {
  let gauge = Shared::of({ live: 0, peak: 0 });
  let done = Shared::of(0);
  bounded_nursery(8, fn () => crowd(gauge, done));
  let final = Shared::get(gauge);
  print("done " + Int::to_string(Shared::get(done)));
  print("live " + Int::to_string(final.live));
  print("within " + (if final.peak <= 8 { "yes" } else { "no, " + Int::to_string(final.peak) }));
}
"#,
        r#"  workload();
  // Read here rather than inside: the cells above are still held while the
  // function holding them is running, and a count taken then is a count of
  // things that have not been released *yet* rather than of things that will
  // not be.
  print("live objects " + Int::to_string(khora_live_count()));"#,
    );
    assert_eq!(
        out,
        "done 200\nlive 0\nwithin yes\nlive objects 0\n",
        "every piece of work ran, none was still running at the end, the bound held, \
         and nothing leaked"
    );
}

/// An unbounded nursery over work you are already holding is the other half of
/// that advice, and it should not be slower or wrong — only unbounded.
#[test]
fn an_unbounded_nursery_still_finishes_everything() {
    let out = run(
        "load_nursery_unbounded",
        r#"fn worker(done: Shared<Int>) -> () {
  let _ = spin(500);
  Shared::update(done, fn n => n + 1);
}

fn crowd<'e>(done: Shared<Int>) -> ()
  with { 'e | nursery: Nursery }
{
  let mut i = 0;
  while i < 64 {
    nursery.adopt(Task::spawn(fn () => worker(done)));
    i = i + 1
  };
}
"#,
        r#"  let done = Shared::of(0);
  bounded_nursery(0, fn () => crowd(done));
  print(Int::to_string(Shared::get(done)));"#,
    );
    assert_eq!(out, "64\n");
}

// --- backpressure -----------------------------------------------------------

/// **A full channel stops the sender.** One producer with two hundred values,
/// a channel of four, and a consumer that is deliberately slower.
///
/// The assertion is on the *depth*, sampled by the consumer: if a full channel
/// buffered instead of waiting, the depth would climb past four and memory
/// would be holding the difference. Every value still arrives, and in order —
/// backpressure that dropped or reordered work would be a different bug wearing
/// the same clothes.
#[test]
fn a_full_channel_stops_its_sender() {
    let out = run(
        "load_backpressure",
        r#"fn produce(work: Channel<Int>) -> () {
  let mut i = 0;
  while i < 200 {
    Channel::send(work, i);
    i = i + 1
  };
  Channel::close(work);
}

/// Takes everything, slowly, watching how much was waiting each time.
fn consume(work: Channel<Int>, deepest: Shared<Int>) -> Int {
  let mut seen = 0;
  let mut ordered = true;
  let mut going = true;
  while going {
    match Channel::receive(work) {
      Option::None => going = false,
      Option::Some(value) => {
        if value != seen { ordered = false };
        let depth = Channel::depth(work);
        Shared::update(deepest, fn most => if depth > most { depth } else { most });
        let _ = spin(400);
        seen = seen + 1
      },
    }
  };
  if ordered { seen } else { 0 - 1 }
}
"#,
        r#"  let work: Channel<Int> = Channel::bounded(4);
  let deepest = Shared::of(0);
  let crew = Fibers::open();
  Fibers::adopt(crew, Task::spawn(fn () => produce(work)));
  let seen = consume(work, deepest);
  Fibers::wait(crew);
  print("received " + Int::to_string(seen));
  print("deepest " + (if Shared::get(deepest) <= 4 { "within" } else { "over" }));"#,
    );
    assert_eq!(
        out,
        "received 200\ndeepest within\n",
        "two hundred values, in order, and never more than four of them waiting"
    );
}

/// Closing a channel releases what is queued *before* it answers `None`.
///
/// Values already sent are still worth having, and a shutdown that dropped
/// them would lose work that was accepted. The other half of the same claim:
/// closing twice is allowed, because the fiber that owns a channel and the one
/// that finishes with it are often different code.
#[test]
fn a_closed_channel_drains_before_it_ends() {
    let out = run(
        "load_drain",
        r#"fn drain(work: Channel<Int>) -> Int {
  let mut total = 0;
  let mut going = true;
  while going {
    match Channel::receive(work) {
      Option::None => going = false,
      Option::Some(value) => total = total + value,
    }
  };
  total
}
"#,
        r#"  let work: Channel<Int> = Channel::bounded(8);
  Channel::send(work, 1);
  Channel::send(work, 2);
  Channel::send(work, 3);
  Channel::close(work);
  Channel::close(work);
  print(Int::to_string(drain(work)));
  print(if Channel::send(work, 4) { "accepted after close" } else { "refused after close" });"#,
    );
    assert_eq!(out, "6\nrefused after close\n");
}

// --- overload, and what comes after it --------------------------------------

/// **The shape of a service under load**, without a socket in it: a bounded
/// queue, a bounded pool of workers, and more work offered than either can
/// hold at once.
///
/// Four claims in one program, because they are only interesting together:
/// nothing is lost, nothing is done twice, the worker pool never exceeds its
/// size, and the queue never exceeds its depth. A system that dropped work
/// under load would satisfy the last two and fail the first, which is exactly
/// the trade this is here to refuse.
#[test]
fn overload_becomes_latency_rather_than_loss() {
    let out = run(
        "load_overload",
        r#"fn worker(work: Channel<Int>, gauge: Shared<Gauge>, total: Shared<Int>) -> () {
  let mut going = true;
  while going {
    match Channel::receive(work) {
      Option::None => going = false,
      Option::Some(value) => {
        entered(gauge);
        let _ = spin(300);
        Shared::update(total, fn n => n + value);
        left(gauge)
      },
    }
  };
}

fn offer(work: Channel<Int>, deepest: Shared<Int>) -> () {
  let mut i = 1;
  while i <= 300 {
    Channel::send(work, i);
    let depth = Channel::depth(work);
    Shared::update(deepest, fn most => if depth > most { depth } else { most });
    i = i + 1
  };
  Channel::close(work);
}

fn hire<'e>(work: Channel<Int>, gauge: Shared<Gauge>, total: Shared<Int>) -> ()
  with { 'e | nursery: Nursery }
{
  let mut i = 0;
  while i < 4 {
    nursery.adopt(Task::spawn(fn () => worker(work, gauge, total)));
    i = i + 1
  };
}
"#,
        r#"  let work: Channel<Int> = Channel::bounded(8);
  let gauge = Shared::of({ live: 0, peak: 0 });
  let total = Shared::of(0);
  let deepest = Shared::of(0);

  let crew = Fibers::open();
  Fibers::adopt(crew, Task::spawn(fn () => offer(work, deepest)));
  bounded_nursery(4, fn () => hire(work, gauge, total));
  Fibers::wait(crew);

  // 1 + 2 + ... + 300. Every unit of offered work was done, and done once.
  print("total " + Int::to_string(Shared::get(total)));
  print("queue " + (if Shared::get(deepest) <= 8 { "bounded" } else { "unbounded" }));
  print("workers " + (if Shared::get(gauge).peak <= 4 { "bounded" } else { "unbounded" }));
  print("idle " + Int::to_string(Shared::get(gauge).live));"#,
    );
    assert_eq!(
        out,
        "total 45150\nqueue bounded\nworkers bounded\nidle 0\n",
        "all three hundred units done exactly once, with both bounds held"
    );
}

/// **And afterwards it is a working service again.** A burst, then quiet, then
/// ordinary work — the counters back at rest and nothing left over.
///
/// Recovery is the half of overload that nobody tests, and a system that
/// survives the burst by leaking a fiber per request looks identical until the
/// second burst.
#[test]
fn a_service_recovers_after_the_burst() {
    let out = run(
        "load_recovery",
        r#"fn burst(work: Channel<Int>, total: Shared<Int>) -> () {
  let mut i = 0;
  while i < 150 {
    Channel::send(work, 1);
    i = i + 1
  };
}

fn worker(work: Channel<Int>, total: Shared<Int>) -> () {
  let mut going = true;
  while going {
    match Channel::receive(work) {
      Option::None => going = false,
      Option::Some(value) => { Shared::update(total, fn n => n + value); },
    }
  };
}

fn serve<'e>(work: Channel<Int>, total: Shared<Int>) -> ()
  with { 'e | nursery: Nursery }
{
  let mut i = 0;
  while i < 3 {
    nursery.adopt(Task::spawn(fn () => worker(work, total)));
    i = i + 1
  };
}

fn rounds() -> () {
  let total = Shared::of(0);
  print("burst left " + Int::to_string(round(total, 150)));
  print("after burst " + Int::to_string(Shared::get(total)));
  print("quiet left " + Int::to_string(round(total, 3)));
  print("after quiet " + Int::to_string(Shared::get(total)));
}

fn round(total: Shared<Int>, count: Int) -> Int {
  let work: Channel<Int> = Channel::bounded(4);
  let crew = Fibers::open();
  Fibers::adopt(crew, Task::spawn(fn () => {
    let mut i = 0;
    while i < count {
      Channel::send(work, 1);
      i = i + 1
    };
    Channel::close(work)
  }));
  bounded_nursery(3, fn () => serve(work, total));
  Fibers::wait(crew);
  Channel::depth(work)
}
"#,
        r#"  rounds();
  print("live objects " + Int::to_string(khora_live_count()));"#,
    );
    assert_eq!(
        out,
        "burst left 0\nafter burst 150\nquiet left 0\nafter quiet 153\nlive objects 0\n",
        "the queue is empty after each round, the second round is ordinary, and \
         nothing was left on the heap"
    );
}

// --- shutting down ----------------------------------------------------------

/// **Shutdown finishes what was accepted.** Work already in the queue is
/// completed rather than abandoned, and `Fibers::wait` does not return until
/// every worker has stopped.
///
/// This is the difference between "stop" and "stop *now*", and a service that
/// confuses them loses whatever it had already told a client it would do.
#[test]
fn shutdown_completes_what_was_already_accepted() {
    let out = run(
        "load_shutdown",
        r#"fn worker(work: Channel<Int>, done: Shared<Int>) -> () {
  let mut going = true;
  while going {
    match Channel::receive(work) {
      Option::None => going = false,
      Option::Some(_) => {
        let _ = spin(200);
        Shared::update(done, fn n => n + 1);
      },
    }
  };
}

fn hire<'e>(work: Channel<Int>, done: Shared<Int>) -> ()
  with { 'e | nursery: Nursery }
{
  let mut i = 0;
  while i < 2 {
    nursery.adopt(Task::spawn(fn () => worker(work, done)));
    i = i + 1
  };
}
"#,
        r#"  let work: Channel<Int> = Channel::bounded(32);
  let done = Shared::of(0);
  // Twenty accepted before anybody starts, then the door is shut. Everything
  // queued is owed to somebody.
  let mut i = 0;
  while i < 20 {
    Channel::send(work, i);
    i = i + 1
  };
  Channel::close(work);
  bounded_nursery(2, fn () => hire(work, done));
  print(Int::to_string(Shared::get(done)));
  print(Int::to_string(Channel::depth(work)));"#,
    );
    assert_eq!(out, "20\n0\n", "everything accepted was done, and the queue is empty");
}

// --- a server with more clients than it can serve at once --------------------

/// Every `.kh` file of `std`, plus the server below.
fn std_sources(db: &KhoraDatabase, dir: &std::path::Path, main: &str) -> Vec<SourceFile> {
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

/// **The claim, with a socket in it.** Twenty-four clients at once against a
/// server that will serve four.
///
/// `Router::listen` bounds itself to 256, which is more than a test can
/// usefully exceed — so the server here is `serve_forever` inside a nursery of
/// four, which is the same code path with a number small enough to cross.
///
/// Each answer carries the highest number of handlers that have ever been
/// running at once, so the assertion is not "it did not fall over" but the
/// specific thing the design claims: **the bound held while the load was
/// above it, and every client was still answered.** Overload became latency.
#[test]
fn a_server_under_more_load_than_it_can_serve_answers_everybody() {
    let port = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        listener.local_addr().expect("an address").port()
    };

    let main = format!(
        r#"module demo::main;
import std::core::{{
  Nursery, Option, Result, Share, Shared, SharedFn, Show, Task, attempt, bounded_nursery, print
}};
import std::net::http::{{HttpError, Request, Response, Router}};
import std::net::socket::{{invalid_handle, listen_on, start}};

pub type Gauge = {{ live: Int, peak: Int }};

/// Enough arithmetic that a handler is still running when the next arrives.
fn spin(rounds: Int) -> Int {{
  let mut i = 0;
  let mut total = 0;
  while i < rounds {{
    total = total + i;
    i = i + 1
  }};
  total
}}

/// Answers with the most handlers that have ever been running at once.
///
/// The peak is read from the same update that raised it, so it is the peak as
/// of this request rather than as of some later moment.
fn work(gauge: Shared<Gauge>, request: Request) -> Response {{
  let entered = Shared::update(gauge, fn now => {{
    live: now.live + 1,
    peak: if now.live + 1 > now.peak {{ now.live + 1 }} else {{ now.peak }},
  }});
  let _ = spin(20000);
  let after = Shared::update(gauge, fn now => {{ live: now.live - 1, peak: now.peak }});
  Response::text(200, Int::to_string(after.peak))
}}

fn serve<'e>(gauge: Shared<Gauge>, server: Int) -> ()
  with {{ 'e | nursery: Nursery }}
  raises HttpError
{{
  let router = Router::new()
    |> Router::get("/work", SharedFn::of(fn r => work(gauge, r)));
  Router::serve_forever(router, server)!
}}

fn main() -> () {{
  let gauge = Shared::of({{ live: 0, peak: 0 }});
  if start() {{ }} else {{ print("no sockets") }};
  let server = listen_on({port});
  if server == invalid_handle() {{
    print("could not bind")
  }} else {{
    print("listening on {port}");
    match attempt(fn () => bounded_nursery(4, fn () => serve(gauge, server)!)!) {{
      Result::Ok(_) => (),
      Result::Err(_) => print("the server stopped"),
    }}
  }}
}}
"#
    );

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("load_server");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, std_sources(&db, &dir, &main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling the load server failed:\n  {}\n\n{main}", messages.join("\n  "));
    }

    let mut child = std::process::Command::new(&exe)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the server should start");

    // Wait for the port rather than for a duration.
    let up = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < up {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let answers = ask_together(port, 24);
    let after = ask_together(port, 1);
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(answers.len(), 24, "every client got an answer: {answers:?}");
    let peaks: Vec<i32> = answers
        .iter()
        .map(|a| {
            let (head, body) = a.split_once("\r\n\r\n").unwrap_or(("", ""));
            assert!(head.starts_with("HTTP/1.1 200 "), "an answer that is not 200: {a:?}");
            body.trim().parse().unwrap_or_else(|_| panic!("a peak, not {body:?}"))
        })
        .collect();
    let highest = peaks.iter().copied().max().unwrap_or(0);
    assert!(
        highest <= 4,
        "the nursery of four served {highest} at once, so the bound is decoration: {peaks:?}"
    );
    assert!(
        highest > 1,
        "the load never actually overlapped ({peaks:?}), so this proved nothing — \
         raise the client count or the work per request"
    );
    assert_eq!(after.len(), 1, "and the server still answers afterwards");
}

/// Opens `count` connections and reads an answer on each.
///
/// **No barrier, and two attempts at one are why.** The first sized it to the
/// clients it asked for, so one refused connect left a thread that never
/// arrived and the rest waiting for it. The second opened every socket before
/// releasing them, which needs the listening backlog to hold all of them at
/// once — and that backlog was 16, so twenty-four clients hung the test rather
/// than the server. The second attempt is how the backlog was found.
///
/// Both were the test synchronising harder than the thing under test needs. A
/// thread per client, each connecting and asking straight away, overlaps
/// perfectly well — and the peak each answer carries is the evidence that it
/// did, which is better than a barrier's promise that it should have.
fn ask_together(port: u16, count: usize) -> Vec<String> {
    use std::io::{Read, Write};
    let asking: Vec<_> = (0..count)
        .map(|_| {
            std::thread::spawn(move || {
                let mut socket = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
                socket.set_read_timeout(Some(std::time::Duration::from_secs(30))).ok()?;
                socket
                    .write_all(b"GET /work HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                    .ok()?;
                socket.flush().ok()?;
                let mut said = String::new();
                socket.read_to_string(&mut said).ok()?;
                if said.is_empty() { None } else { Some(said) }
            })
        })
        .collect();
    asking.into_iter().filter_map(|t| t.join().ok().flatten()).collect()
}
