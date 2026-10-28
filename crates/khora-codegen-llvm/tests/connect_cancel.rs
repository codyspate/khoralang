// Not on Windows: see "A host that never answers" below.
#![cfg(all(feature = "llvm", not(windows)))]

//! A fiber canceled inside `connect_to` stops at once, and lets go of
//! everything it opened.
//!
//! **What this guards: a cancel that waits for the kernel.** A connect to a
//! host that never answers is the kernel retransmitting SYNs, which on Linux
//! gives up after about two minutes. A fiber parked in such a connect is a
//! nursery that cannot stop its siblings, a router that cannot shut down and a
//! deadline around an outbound call that does nothing, all for those two
//! minutes. The watchdog below is 20 seconds, far under the kernel's floor, so
//! a connect that is not a cancellation point fails by being killed.
//!
//! # A host that never answers, made locally
//!
//! A listener nobody accepts from, whose backlog [`never_answering`] has
//! filled. Once the kernel's accept queue is full it drops a new SYN rather
//! than answering it, so the next connect hangs exactly as one to an
//! unroutable host does, with no network and no address that only some
//! machines drop. The helper proves the premise before anything relies on
//! it: it keeps connecting until one attempt times out, and fails by name if
//! none ever does.
//!
//! **Not on Windows.** Winsock answers a SYN to a full backlog with a reset,
//! so the connect is refused rather than left waiting and there is nothing
//! here to cancel. An address that drops packets (`10.255.255.1`) depends on
//! the runner's network, which this test will not; the Windows path is the
//! same reactor wait and is covered by `cargo check` and review alone.

use crate::harness;

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

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

fn build(name: &str, source: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("program");
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, source));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }
    exe
}

/// How a run ended.
struct Ran {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    /// Whether the watchdog killed it.
    hung: bool,
}

/// Runs `exe` on `backend`, killing it after `patience`.
fn run(exe: &PathBuf, backend: &str, patience: Duration) -> Ran {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = Instant::now();
    let mut hung = false;
    while child.try_wait().expect("waiting").is_none() {
        if started.elapsed() > patience {
            let _ = child.kill();
            hung = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().expect("reaping");
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_string(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    Ran {
        stdout: stdout.replace("\r\n", "\n"),
        stderr: stderr.replace("\r\n", "\n"),
        code: status.code(),
        hung,
    }
}

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// Far under the two minutes a kernel spends on a connect nobody answers, and
/// far over what any of these programs needs when connect can be canceled.
const PATIENCE: Duration = Duration::from_secs(20);

/// A listening port whose connects hang, and what keeps it that way.
///
/// The listener and the connections that filled its queue are both returned:
/// dropping either would let the next connect through, or refuse it.
pub(crate) struct NeverAnswering {
    pub(crate) port: u16,
    _listener: TcpListener,
    _filled: Vec<TcpStream>,
}

/// Fills a fresh listener's accept queue until a connect times out.
///
/// Panics if none does within a thousand attempts, or if one is refused:
/// either means this platform answers a full queue, and every test built on
/// this would then pass or fail for a reason that has nothing to do with
/// canceling a connect.
pub(crate) fn never_answering() -> NeverAnswering {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    let address: SocketAddr = listener.local_addr().expect("an address");
    let mut filled = Vec::new();
    for _ in 0..1000 {
        match TcpStream::connect_timeout(&address, Duration::from_millis(300)) {
            Ok(stream) => filled.push(stream),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                return NeverAnswering { port: address.port(), _listener: listener, _filled: filled };
            }
            Err(e) => panic!(
                "a connect to a full backlog failed with {e} after {} succeeded, rather than \
                 waiting: this platform answers a full queue, so nothing here can hang",
                filled.len()
            ),
        }
    }
    panic!("a thousand connects all went through, so the backlog never filled");
}

/// The program both connect tests share: a dialer parked in `connect_to`, a
/// control parked in `clock.sleep`, and the live count around each.
///
/// The control is what makes a leak delta mean something. On some backends a
/// canceled fiber has cost objects that have nothing to do with sockets; a
/// connect that leaks shows up as a delta the sleep does not have.
fn cancel_program(port: u16) -> String {
    format!(
        "module main;
import std::core::{{Fiber, Shared, Result, Scope, attempt, print, scoped}};
import std::clock::{{Clock}};
import std::net::socket::{{start, connect_to}};
import std::net::tls::{{TlsError, client as tls_client, connect as tls_connect}};

extern fn khora_live_count() -> Int;
extern fn khora_monotonic_millis() -> Int;

fn dial(dialing: Shared<Bool>) -> () {{
  Shared::set(dialing, true);
  let c = connect_to(\"127.0.0.1\", {port});
  print(\"TAIL connect_to returned ${{c}}\");
}}

fn secure_dial(dialing: Shared<Bool>) -> () with {{ scope: Scope }} raises TlsError {{
  let trust = tls_client()!;
  Shared::set(dialing, true);
  let _session = tls_connect(trust, \"localhost\", {port})!;
  print(\"TAIL tls connect returned\");
}}

fn tls_dial(dialing: Shared<Bool>) -> () {{
  match attempt(fn () => scoped(fn () => secure_dial(dialing)!)!) {{
    Result::Err(_) => print(\"TAIL tls connect failed\"),
    Result::Ok(_) => (),
  }}
}}

fn nap(dialing: Shared<Bool>) -> () {{
  Shared::set(dialing, true);
  with {{ clock: Clock::real() }} {{ clock.sleep(60000) }};
  print(\"TAIL sleep returned\");
}}

/// Spawns one of the three, waits until it says it is in, cancels it, and
/// answers how long the cancel took to land.
fn cut(which: Int, dialing: Shared<Bool>) -> Int with {{ clock: Clock }} {{
  let f = if which == 0 {{
    Fiber::spawn(fn () => dial(dialing))
  }} else if which == 1 {{
    Fiber::spawn(fn () => tls_dial(dialing))
  }} else {{
    Fiber::spawn(fn () => nap(dialing))
  }};
  while !Shared::get(dialing) {{ clock.sleep(5) }};
  // Not a timing assertion: only more likely to be inside the call itself
  // rather than on the line before it. A cancel that lands earlier finds a
  // canceled fiber at the call, which must give up just the same.
  clock.sleep(100);
  let began = khora_monotonic_millis();
  Fiber::cancel(f);
  Fiber::wait(f);
  let took = khora_monotonic_millis() - began;
  if Fiber::canceled(f) {{ took }} else {{ -1 }}
}}

fn round(name: String, which: Int) -> () with {{ clock: Clock }} {{
  let dialing = Shared::of(false);
  let before = khora_live_count();
  let took = cut(which, dialing);
  let delta = khora_live_count() - before;
  // Read after the count, so the cell's own release is not inside it.
  let entered = Shared::get(dialing);
  let fast = took >= 0 && took < 1000;
  print(\"${{name}} stopped ${{took >= 0}} within a second ${{fast}} live ${{delta}}\");
  if !fast || !entered {{ print(\"${{name}} took ${{took}} ms, entered ${{entered}}\") }};
}}

pub fn main() -> Int {{
  if start() {{}} else {{ print(\"no sockets\") }};
  with {{ clock: Clock::real() }} {{
    round(\"sleep\", 2);
    round(\"connect_to\", 0);
    round(\"connect_to again\", 0);
    round(\"tls connect\", 1);
  }};
  0
}}
"
    )
}

/// **Canceling a fiber inside `connect_to`, or inside a TLS `connect`, stops
/// it within a second and leaks nothing.**
///
/// Each is canceled once it has said it is about to dial a host that will
/// never answer. Neither may run its tail. The live count around each must
/// not move, and the sleep is the control that shows a canceled fiber costs
/// nothing on this backend when it was parked somewhere already known to be
/// a cancellation point. `connect_to` runs twice, so a socket or registration
/// left behind by the first would show in the second.
#[test]
fn a_fiber_canceled_inside_connect_stops_at_once_and_leaks_nothing() {
    let quiet = never_answering();
    let exe = build("connect_cancel", &cancel_program(quiet.port));
    for backend in BACKENDS {
        let ran = run(&exe, backend, PATIENCE);
        assert!(
            !ran.hung,
            "`{backend}`: still running after {PATIENCE:?}, so a cancel did not reach a fiber \
             inside connect: {}{}",
            ran.stdout,
            ran.stderr
        );
        assert!(!ran.stdout.contains("TAIL"), "`{backend}`: a canceled dialer ran on: {}", ran.stdout);
        assert_eq!(
            ran.stdout,
            "sleep stopped true within a second true live 0\n\
             connect_to stopped true within a second true live 0\n\
             connect_to again stopped true within a second true live 0\n\
             tls connect stopped true within a second true live 0\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
    }
}

/// **A nursery whose child fails while a sibling is inside `connect_to`
/// returns promptly.**
///
/// The failure cancels the sibling, and the nursery waits for it. If the
/// connect is not a cancellation point, that wait is the kernel's two
/// minutes. The failing child notes the time it raised; `main` measures from
/// there to the nursery handing the failure back.
///
/// **Adoption order is load-bearing**, as in `tls_cancel.rs`: a nursery looks
/// at its children in the order it adopted them, so the one that fails goes
/// first.
#[test]
fn a_nursery_returns_promptly_when_a_child_fails_beside_one_inside_connect() {
    let quiet = never_answering();
    let port = quiet.port;
    let source = format!(
        "module main;
import std::core::{{Nursery, Shared, Result, attempt, nursery, print}};
import std::clock::{{Clock}};
import std::net::socket::{{start, connect_to}};

extern fn khora_monotonic_millis() -> Int;

pub type Oops = | Bad;

fn dial(dialing: Shared<Bool>) -> () {{
  Shared::set(dialing, true);
  let c = connect_to(\"127.0.0.1\", {port});
  print(\"TAIL connect_to returned ${{c}}\");
}}

fn doomed(dialing: Shared<Bool>, raised: Shared<Int>) -> () raises Oops {{
  with {{ clock: Clock::real() }} {{
    while !Shared::get(dialing) {{ clock.sleep(5) }};
    clock.sleep(100);
  }};
  Shared::set(raised, khora_monotonic_millis());
  raise Oops::Bad
}}

fn both(dialing: Shared<Bool>, raised: Shared<Int>) -> () with {{ nursery: Nursery }} {{
  nursery.spawn(fn () => doomed(dialing, raised)!);
  nursery.spawn(fn () => dial(dialing));
}}

pub fn main() -> Int {{
  if start() {{}} else {{ print(\"no sockets\") }};
  let dialing = Shared::of(false);
  let raised = Shared::of(0);
  let outcome = attempt(fn () => nursery(fn () => both(dialing, raised))!);
  let took = khora_monotonic_millis() - Shared::get(raised);
  match outcome {{
    Result::Err(_) => print(\"the nursery failed\"),
    Result::Ok(_) => print(\"the nursery succeeded, which is wrong\"),
  }};
  print(\"within a second ${{took < 1000}}\");
  if took >= 1000 {{ print(\"took ${{took}} ms\") }};
  0
}}
"
    );
    let exe = build("connect_nursery", &source);
    for backend in BACKENDS {
        let ran = run(&exe, backend, PATIENCE);
        assert!(
            !ran.hung,
            "`{backend}`: the nursery was still waiting after {PATIENCE:?} for a child inside \
             connect: {}{}",
            ran.stdout,
            ran.stderr
        );
        assert_eq!(
            ran.stdout,
            "the nursery failed\nwithin a second true\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
    }
}
