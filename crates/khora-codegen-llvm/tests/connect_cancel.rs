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
  let c = connect_to(\"127.0.0.1\", {port})! catch {{ _ => -1 }};
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
  let c = connect_to(\"127.0.0.1\", {port})! catch {{ _ => -1 }};
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

/// **Each way a connect can fail raises its own `SocketError`.** A deadline
/// that passes while the kernel waits on a host that never answers raises
/// `TimedOut`, from `connect_by`; a port where nothing listens raises
/// `Unreachable`, from both connects. No negative handle comes back from
/// either, and the deadline is kept: the timed-out connect gives up within a
/// few seconds of it, not at the kernel's two minutes.
#[test]
fn a_failed_connect_raises_the_case_for_how_it_failed() {
    let quiet = never_answering();
    let closed = {
        let gone = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        gone.local_addr().expect("an address").port()
    };
    let source = format!(
        "module main;
import std::core::{{print}};
import std::net::socket::{{SocketError, connect_by, connect_to, shut, start}};

extern fn khora_monotonic_millis() -> Int;

fn why(e: SocketError) -> String {{
  match e {{
    SocketError::Denied(host) => \"denied ${{host}}\",
    SocketError::Unreachable(host) => \"unreachable ${{host}}\",
    SocketError::TimedOut(host) => \"timed out ${{host}}\",
  }}
}}

fn connected(h: Int) -> String {{
  shut(h);
  if h >= 0 {{ \"connected\" }} else {{ \"a negative handle ${{h}}\" }}
}}

fn by(port: Int, within: Int) -> String raises SocketError {{
  connected(connect_by(\"127.0.0.1\", port, khora_monotonic_millis() + within)!)
}}

fn to(port: Int) -> String raises SocketError {{
  connected(connect_to(\"127.0.0.1\", port)!)
}}

pub fn main() -> Int {{
  if start() {{}} else {{ print(\"no sockets\") }};
  let began = khora_monotonic_millis();
  print(by({quiet}, 300)! catch {{ e => why(e) }});
  let took = khora_monotonic_millis() - began;
  print(\"gave up in time ${{took >= 250 && took < 5000}}\");
  if took < 250 || took >= 5000 {{ print(\"took ${{took}} ms\") }};
  print(by({closed}, 5000)! catch {{ e => why(e) }});
  print(to({closed})! catch {{ e => why(e) }});
  0
}}
",
        quiet = quiet.port
    );
    let exe = build("connect_raises", &source);
    for backend in BACKENDS {
        let ran = run(&exe, backend, PATIENCE);
        assert!(!ran.hung, "`{backend}`: still running after {PATIENCE:?}: {}{}", ran.stdout, ran.stderr);
        assert_eq!(
            ran.stdout,
            format!(
                "timed out 127.0.0.1:{quiet}\ngave up in time true\n\
                 unreachable 127.0.0.1:{closed}\nunreachable 127.0.0.1:{closed}\n",
                quiet = quiet.port
            ),
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
    }
}

/// **An HTTP call whose deadline passes in the connect is `TimedOut`, not
/// `Unreachable`.** The host is one that never answers a SYN, so the call
/// spends its whole 300 ms budget in the plain dialer's connect, and the
/// socket's `TimedOut` must reach the caller as `CallError::TimedOut(300)`:
/// a timeout is the failure a retry with a longer budget can fix, and
/// `Unreachable` would tell the caller not to bother.
#[test]
fn an_http_call_whose_connect_runs_out_of_time_is_timed_out() {
    let quiet = never_answering();
    let source = format!(
        "module main;
import std::core::{{Result, print}};
import std::net::http::{{Call, CallError, HttpClient}};

pub fn main() -> Int {{
  with {{ http: HttpClient::within(300) }} {{
    match http.send(Call::get(\"http://127.0.0.1:{port}/\")) {{
      Result::Ok(_) => print(\"answered\"),
      Result::Err(why) => print(why.show()),
    }}
  }};
  0
}}
",
        port = quiet.port
    );
    let exe = build("connect_http_timed_out", &source);
    for backend in BACKENDS {
        let ran = run(&exe, backend, PATIENCE);
        assert!(!ran.hung, "`{backend}`: still running after {PATIENCE:?}: {}{}", ran.stdout, ran.stderr);
        assert_eq!(
            ran.stdout,
            "CallError::TimedOut(300): no answer within 300 ms. That is the call's deadline, or, \
             for a client with no deadline (`HttpClient::within(0)`), the limit on silence between \
             reads; `HttpClient::within(ms)` or `HttpClient::bounded_within(limit, ms)` sets the deadline\n",
            "`{backend}`: {}",
            ran.stderr
        );
        assert_eq!(ran.code, Some(0), "`{backend}`: {}", ran.stderr);
    }
}

/// The threads named `khora-lookup` in process `pid` right now.
#[cfg(target_os = "linux")]
fn lookup_threads(pid: u32) -> usize {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else { return 0 };
    tasks
        .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .filter(|name| name.trim_end() == "khora-lookup")
        .count()
}

/// **The lookup pool's 32 threads are a bound, not a cost every program
/// pays.** A program that resolves no name starts no lookup thread, and one
/// that resolves one name starts one. A pool that started its threads up
/// front would hold 32 stacks in every program that links the runtime,
/// whether or not it ever looked anything up.
///
/// Counted from outside, in `/proc/<pid>/task`, while each program waits
/// after its work, on both backends. The program that connects by address
/// never reaches the resolver; the one that connects to `localhost` does,
/// and is the control that shows the count can see a lookup thread at all.
#[cfg(target_os = "linux")]
#[test]
fn a_program_starts_lookup_threads_only_for_its_lookups() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let program = |host: &str| {
        format!(
            r#"module main;
import std::core::{{print}};
import std::clock::{{Clock}};
import std::net::socket::{{SocketError, connect_to, shut, start}};

pub fn main() -> Int {{
  if start() {{}} else {{ print("no sockets") }};
  let handle = connect_to("{host}", {port})! catch {{ _ => -1 }};
  if handle >= 0 {{ shut(handle) }};
  print("ready");
  with {{ clock: Clock::real() }} {{ clock.sleep(5000) }};
  0
}}
"#
        )
    };
    let by_address = build("lookup_threads_none", &program("127.0.0.1"));
    let by_name = build("lookup_threads_one", &program("localhost"));
    for backend in BACKENDS {
        for (exe, want) in [(&by_address, 0), (&by_name, 1)] {
            let mut child = Command::new(exe)
                .env("KHORA_FIBERS", backend)
                .env_remove("KHORA_LOOKUP_THREADS")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("the program should run");
            let mut stdout = child.stdout.take().expect("piped");
            let mut said = [0u8; 6];
            let ready = stdout.read_exact(&mut said).is_ok() && &said == b"ready\n";
            let counted = lookup_threads(child.id());
            let _ = child.kill();
            let _ = child.wait();
            assert!(ready, "`{backend}` {}: never said it was ready", exe.display());
            assert_eq!(counted, want, "`{backend}` {}: lookup threads", exe.display());
        }
    }
}
