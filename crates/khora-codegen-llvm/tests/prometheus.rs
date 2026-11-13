#![cfg(feature = "llvm")]

//! `packages/prometheus`, compiled and run: many fibers counting at once, a
//! scrape over a real socket, and the rendered text read by Prometheus' own
//! parser.
//!
//! The package's `khora test` blocks check the text byte for byte. What they
//! cannot check is the three things here: that no increment is lost when a
//! hundred fibers make them on both fiber backends, that a router serves the
//! text with the content type a scraper reads, and that a parser written by
//! somebody else accepts what this one writes.

use crate::harness;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// Its own, and [`SHOP_PORT`] beside it; `traps_in_a_server.rs` lists the
/// ports in use.
const PORT: u16 = 18743;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// `std` and the package, without the package's `test`-block files, which a
/// `main` build has no entry point for, plus `main`.
fn sources(db: &KhoraDatabase, dir: &Path, main: &str) -> Vec<SourceFile> {
    let mut out = Vec::new();
    let mut stack = vec![
        repository().join("std"),
        repository().join("packages").join("prometheus").join("src"),
    ];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
                && !path.to_string_lossy().ends_with("_test.kh")
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
        panic!("`{name}` did not build:\n  {}", messages.join("\n  "));
    }
    exe
}

/// Runs `exe` on `backend` and answers its stdout, failing with its status
/// and stderr if it did not exit cleanly.
fn run(exe: &Path, backend: &str) -> String {
    let output = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .output()
        .expect("the program should start");
    assert!(
        output.status.success(),
        "`{}` on `{backend}` ended with {:?}; it said: {}",
        exe.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A hundred fibers, ten thousand changes each, on one counter series and
/// one histogram series at once.
///
/// The fibers are spawned before any of them is waited for, and the
/// histogram's fibers run alongside the counter's, so each cell is contended
/// by a hundred writers and the registry by both metrics.
const CONTENDED: &str = r#"module demo::main;

import std::core::{Fibers, List, print};
import prometheus::metrics::{CounterChild, HistogramChild, MetricError, Registry};

fn bump(hits: CounterChild, times: Int) -> () {
  let mut i = 0;
  while i < times {
    hits.inc();
    i = i + 1;
  };
}

fn time(seconds: HistogramChild, times: Int) -> () {
  let mut i = 0;
  while i < times {
    seconds.observe(0.25);
    i = i + 1;
  };
}

pub fn main() -> () raises MetricError {
  let registry = Registry::new();
  let hits = registry.counter("hits_total", "", ["route"])!.labels(["/"])!;
  let seconds = registry.histogram("seconds", "", List::Nil, [0.1, 1.0])!.labels(List::Nil)!;
  let crew = Fibers::open();
  let mut f = 0;
  while f < 100 {
    Fibers::spawn(crew, fn () => bump(hits, 10000));
    Fibers::spawn(crew, fn () => time(seconds, 10000));
    f = f + 1;
  };
  let _failed = Fibers::wait(crew);
  print(registry.render());
}
"#;

/// **What this prevents: a metric that undercounts when the service is
/// busiest.** A counter changed with a read and a separate write loses the
/// increments of every fiber that read between them; at a hundred fibers
/// that is most of them. `0.25` is a power of two, so the histogram's sum is
/// exact and can be compared as text.
#[test]
fn a_hundred_fibers_lose_no_increment() {
    let exe = build("prometheus_contended", CONTENDED);
    let expected = "# TYPE hits_total counter
hits_total{route=\"/\"} 1000000
# TYPE seconds histogram
seconds_bucket{le=\"0.1\"} 0
seconds_bucket{le=\"1\"} 1000000
seconds_bucket{le=\"+Inf\"} 1000000
seconds_sum 250000
seconds_count 1000000

";
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), expected, "on `{backend}`");
    }
}

/// Scrapes taken while eight fibers observe into one histogram series.
///
/// Each scrape is checked here rather than printed: its `+Inf` bucket and
/// `_count` are equal, its buckets never go down, and every observation is
/// `0.5`, so `_sum` is exactly half of `_count`. The scraping loop also
/// counts how many different `_count`s it saw, because a test whose scrapes
/// all landed before or after the observations proves nothing.
const SCRAPED_WHILE_OBSERVED: &str = r#"module demo::main;

import std::core::{Fibers, Iterator, List, Option, Step, String, print};
import prometheus::metrics::{HistogramChild, MetricError, Registry};

fn observe(seconds: HistogramChild, times: Int) -> () {
  let mut i = 0;
  while i < times {
    seconds.observe(0.5);
    i = i + 1;
  };
}

/// The number after the last space of the line that starts with `prefix`.
fn value(scrape: String, prefix: String) -> String {
  let mut found = "";
  for line in String::split(scrape, "\n") {
    if String::starts_with(line, prefix) {
      found = Option::unwrap_or(List::last(String::split(line, " ")), "");
    };
  };
  found
}

/// Whether the `_bucket` values in `scrape` never go down.
fn cumulative(scrape: String) -> Bool {
  let mut previous = -1;
  let mut ok = true;
  for line in String::split(scrape, "\n") {
    if String::starts_with(line, "lat_bucket") {
      let n = Option::unwrap_or(Int::of_string(Option::unwrap_or(List::last(String::split(line, " ")), "")), -2);
      if n < previous { ok = false };
      previous = n;
    };
  };
  ok
}

pub fn main() -> () raises MetricError {
  let registry = Registry::new();
  let seconds = registry.histogram("lat", "", List::Nil, [0.1, 1.0])!.labels(List::Nil)!;
  let crew = Fibers::open();
  let mut f = 0;
  while f < 8 {
    Fibers::spawn(crew, fn () => observe(seconds, 50000));
    f = f + 1;
  };
  let mut torn = 0;
  let mut scrapes = 0;
  let mut distinct = 0;
  let mut last = "";
  while seconds.count() < 400000 {
    let scrape = registry.render();
    let count = value(scrape, "lat_count ");
    let everything = value(scrape, "lat_bucket{le=\"+Inf\"} ");
    let below = value(scrape, "lat_bucket{le=\"1\"} ");
    let n = Option::unwrap_or(Int::of_string(count), -1);
    let half = Float::to_string(Int::to_float(n) * 0.5);
    if count != everything || below != count || value(scrape, "lat_sum ") != half || !cumulative(scrape) {
      torn = torn + 1;
    };
    if count != last { distinct = distinct + 1 };
    last = count;
    scrapes = scrapes + 1;
  };
  let _failed = Fibers::wait(crew);
  let enough = distinct >= 5;
  print("torn ${Int::to_string(torn)}, interleaved ${if enough { "yes" } else { "no" }}");
  if !enough { print("only ${Int::to_string(distinct)} distinct counts in ${Int::to_string(scrapes)} scrapes") };
}
"#;

/// **What this prevents: a scrape that disagrees with itself.** A histogram
/// series is one cell, so a render reads its buckets, sum and count from one
/// snapshot; a renderer that read the cell twice for one series would put a
/// `_count` beside a `+Inf` bucket from a different moment, which the format
/// says are the same number.
#[test]
fn a_scrape_during_observations_is_never_torn() {
    let exe = build("prometheus_scraped_while_observed", SCRAPED_WHILE_OBSERVED);
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), "torn 0, interleaved yes\n", "on `{backend}`");
    }
}

/// Fibers that all ask for the same new series at once.
///
/// **What this prevents: a series made twice, and the increments on the
/// copy that lost the race never rendered.** Every fiber walks the same
/// 2,000 label values in the same order, asking `labels` for each, so each
/// series is created while up to a hundred fibers want it: two keys created
/// once each raced too rarely to show a lost series in any run.
const RACING_LABELS: &str = r#"module demo::main;

import std::core::{Fibers, List, print};
import prometheus::metrics::{Counter, MetricError, Registry};

fn walk(requests: Counter, keys: Int) -> () raises MetricError {
  let mut i = 0;
  while i < keys {
    requests.labels([Int::to_string(i)])!.inc();
    i = i + 1;
  };
}

pub fn main() -> () raises MetricError {
  let registry = Registry::new();
  let requests = registry.counter("requests_total", "", ["key"])!;
  let crew = Fibers::open();
  let mut f = 0;
  while f < 100 {
    Fibers::spawn(crew, fn () => walk(requests, 2000)!);
    f = f + 1;
  };
  let failed = Fibers::wait(crew);
  let mut total = 0.0;
  let mut short = 0;
  let mut i = 0;
  while i < 2000 {
    let seen = requests.labels([Int::to_string(i)])!.get();
    total = total + seen;
    if seen < 100.0 { short = short + 1 };
    i = i + 1;
  };
  print("failed ${Int::to_string(failed)}, total ${Float::to_string(total)}, short ${Int::to_string(short)}");
}
"#;

#[test]
fn fibers_racing_to_make_a_series_share_one() {
    let exe = build("prometheus_racing_labels", RACING_LABELS);
    for backend in BACKENDS {
        assert_eq!(run(&exe, backend), "failed 0, total 200000, short 0\n", "on `{backend}`");
    }
}

/// Every shape the format has, and every value that is easy to write wrong:
/// all three escapes in a label value and in help text, the infinities,
/// `NaN`, numbers too long for `Float::to_string` at both ends, histogram
/// bounds only an exponent tells apart, and an empty histogram.
const EVERY_SHAPE: &str = r#"module demo::main;

import std::core::{List, print};
import prometheus::metrics::{MetricError, Registry, default_buckets};

pub fn main() -> () raises MetricError {
  let registry = Registry::new();
  let requests = registry.counter("http_requests_total", "Requests, by \\ route\nand status.", ["route", "status"])!;
  requests.labels(["/a\\b \"c\"\nd", "200"])!.inc();
  requests.labels(["/orders", "500"])!.add(2.5)!;
  let level = registry.gauge("water_level", "", ["tank"])!;
  let zero = 0.0;
  level.labels(["up"])!.set(1.0 / zero);
  level.labels(["down"])!.set(zero - 1.0 / zero);
  level.labels(["odd"])!.set(zero / zero);
  level.labels(["huge"])!.set(1.0e300);
  level.labels(["neg"])!.set(-0.125);
  level.labels(["least"])!.set(5.0e-324);
  level.labels(["tiny"])!.set(1.0e-300);
  level.labels(["digits"])!.set(1.2345678901234567e-20);
  let small = registry.histogram("small_seconds", "", List::Nil, [1.0e-31, 2.0e-31])!;
  small.observe(1.5e-31)!;
  let seconds = registry.histogram("request_seconds", "Time to answer.", ["route"], default_buckets())!;
  let orders = seconds.labels(["/orders"])!;
  orders.observe(0.003);
  orders.observe(0.2);
  orders.observe(42.0);
  let _ = registry.histogram("empty_seconds", "", List::Nil, List::Nil)!;
  print(registry.render());
}
"#;

/// What the reference parser made of [`EVERY_SHAPE`], as
/// `tests/fixtures/prometheus_parse.py` prints it.
///
/// `http_requests` loses its `_total`: the parser names a counter's family
/// without the suffix and keeps it on the sample, as Prometheus does.
const PARSED: &str = r#"family empty_seconds histogram ''
  empty_seconds_bucket {le='+Inf'} 0.0
  empty_seconds_sum {} 0.0
  empty_seconds_count {} 0.0
family http_requests counter 'Requests, by \\ route\nand status.'
  http_requests_total {route='/a\\b "c"\nd',status='200'} 1.0
  http_requests_total {route='/orders',status='500'} 2.5
family request_seconds histogram 'Time to answer.'
  request_seconds_bucket {le='0.005',route='/orders'} 1.0
  request_seconds_bucket {le='0.01',route='/orders'} 1.0
  request_seconds_bucket {le='0.025',route='/orders'} 1.0
  request_seconds_bucket {le='0.05',route='/orders'} 1.0
  request_seconds_bucket {le='0.1',route='/orders'} 1.0
  request_seconds_bucket {le='0.25',route='/orders'} 2.0
  request_seconds_bucket {le='0.5',route='/orders'} 2.0
  request_seconds_bucket {le='1',route='/orders'} 2.0
  request_seconds_bucket {le='2.5',route='/orders'} 2.0
  request_seconds_bucket {le='5',route='/orders'} 2.0
  request_seconds_bucket {le='10',route='/orders'} 2.0
  request_seconds_bucket {le='+Inf',route='/orders'} 3.0
  request_seconds_sum {route='/orders'} 42.203
  request_seconds_count {route='/orders'} 3.0
family small_seconds histogram ''
  small_seconds_bucket {le='1e-31'} 0.0
  small_seconds_bucket {le='2e-31'} 1.0
  small_seconds_bucket {le='+Inf'} 1.0
  small_seconds_sum {} 1.5e-31
  small_seconds_count {} 1.0
family water_level gauge ''
  water_level {tank='digits'} 1.2345678901234567e-20
  water_level {tank='down'} -Inf
  water_level {tank='huge'} 1e+300
  water_level {tank='least'} 5e-324
  water_level {tank='neg'} -0.125
  water_level {tank='odd'} NaN
  water_level {tank='tiny'} 1e-300
  water_level {tank='up'} +Inf
"#;

/// **What this prevents: text that matches what this package's author
/// believed the format to be, and not what Prometheus reads.** The byte
/// comparisons in the package's own tests were written by the same hand as
/// the renderer; this hands the output to `prometheus_client`'s parser.
///
/// Gated on `KHORA_PROMETHEUS_PYTHON`, an interpreter that can import
/// `prometheus_client`, the way the real-server tests gate on
/// `KHORA_POSTGRES`: CI has no such interpreter, and installing one from a
/// test is a network call. Set and broken is a failure, not a skip.
#[test]
fn the_reference_parser_reads_every_shape() {
    let Some(python) = std::env::var_os("KHORA_PROMETHEUS_PYTHON") else {
        eprintln!(
            "skipping: set KHORA_PROMETHEUS_PYTHON to a Python that can import \
             prometheus_client (`uv venv v && uv pip install --python v/bin/python prometheus_client==0.26.0`)"
        );
        return;
    };
    for (backend, file, scrape) in every_shape("python") {
        let parsed = Command::new(&python)
            .arg(repository().join("crates/khora-codegen-llvm/tests/fixtures/prometheus_parse.py"))
            .arg(&file)
            .output()
            .expect("the parser should start");
        assert!(
            parsed.status.success(),
            "on `{backend}`, the parser refused the scrape: {}\n{scrape}",
            String::from_utf8_lossy(&parsed.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&parsed.stdout), PARSED, "on `{backend}`");
    }
}

/// [`EVERY_SHAPE`] built and run on each backend, each scrape written to a
/// file of its own: the backend, the file, and the scrape.
///
/// `which` names the build, so the two parser tests never build into or
/// read each other's directory when nextest runs them at once.
fn every_shape(which: &str) -> Vec<(&'static str, PathBuf, String)> {
    let name = format!("prometheus_every_shape_{which}");
    let exe = build(&name, EVERY_SHAPE);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(&name);
    BACKENDS
        .iter()
        .map(|backend| {
            let scrape = run(&exe, backend);
            // `print` ends its line, and `render` already ended the last one.
            let scrape = scrape.strip_suffix('\n').expect("the program printed a line").to_string();
            let file = dir.join(format!("scrape-{backend}.txt"));
            std::fs::write(&file, &scrape).expect("a scrape file");
            (*backend, file, scrape)
        })
        .collect()
}

/// What Prometheus' Go parsers made of [`EVERY_SHAPE`], as
/// `tests/fixtures/prometheus_go` prints it: every series, sorted, with its
/// value as Go's `strconv` writes it.
const PARSED_BY_GO: &str = r#"{__name__="empty_seconds_bucket", le="+Inf"} 0
{__name__="empty_seconds_count"} 0
{__name__="empty_seconds_sum"} 0
{__name__="http_requests_total", route="/a\\b \"c\"\nd", status="200"} 1
{__name__="http_requests_total", route="/orders", status="500"} 2.5
{__name__="request_seconds_bucket", le="+Inf", route="/orders"} 3
{__name__="request_seconds_bucket", le="0.005", route="/orders"} 1
{__name__="request_seconds_bucket", le="0.01", route="/orders"} 1
{__name__="request_seconds_bucket", le="0.025", route="/orders"} 1
{__name__="request_seconds_bucket", le="0.05", route="/orders"} 1
{__name__="request_seconds_bucket", le="0.1", route="/orders"} 1
{__name__="request_seconds_bucket", le="0.25", route="/orders"} 2
{__name__="request_seconds_bucket", le="0.5", route="/orders"} 2
{__name__="request_seconds_bucket", le="1.0", route="/orders"} 2
{__name__="request_seconds_bucket", le="10.0", route="/orders"} 2
{__name__="request_seconds_bucket", le="2.5", route="/orders"} 2
{__name__="request_seconds_bucket", le="5.0", route="/orders"} 2
{__name__="request_seconds_count", route="/orders"} 3
{__name__="request_seconds_sum", route="/orders"} 42.203
{__name__="small_seconds_bucket", le="+Inf"} 1
{__name__="small_seconds_bucket", le="1e-31"} 0
{__name__="small_seconds_bucket", le="2e-31"} 1
{__name__="small_seconds_count"} 1
{__name__="small_seconds_sum"} 1.5e-31
{__name__="water_level", tank="digits"} 1.2345678901234567e-20
{__name__="water_level", tank="down"} -Inf
{__name__="water_level", tank="huge"} 1e+300
{__name__="water_level", tank="least"} 5e-324
{__name__="water_level", tank="neg"} -0.125
{__name__="water_level", tank="odd"} NaN
{__name__="water_level", tank="tiny"} 1e-300
{__name__="water_level", tank="up"} +Inf
"#;

/// **What this prevents: a scrape the Python parser accepts and Prometheus
/// does not.** `prometheus_client`'s parser is lenient: it took a scrape
/// with two `# TYPE` lines for one family and one with the same series
/// twice, and Prometheus refuses the first and drops a sample of the second
/// on every scrape. This reads [`EVERY_SHAPE`] with Prometheus' own Go
/// parsers, `expfmt` (what `promtool check metrics` uses) and `textparse`
/// (the server's scrape loop), and fails on a duplicate series as well.
///
/// Gated on `KHORA_PROMETHEUS_GO`, a `go` binary. The checker is built from
/// `tests/fixtures/prometheus_go`, whose `go.mod` and `go.sum` pin the two
/// libraries, into this test's own directory; the first build fetches the
/// modules. Set and broken is a failure, not a skip.
#[test]
fn prometheus_own_parsers_read_every_shape() {
    let Some(go) = std::env::var_os("KHORA_PROMETHEUS_GO") else {
        eprintln!("skipping: set KHORA_PROMETHEUS_GO to a `go` binary (1.26 or later)");
        return;
    };
    let checker = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("prometheus_go").join("checker");
    let built = Command::new(&go)
        .args(["build", "-o"])
        .arg(&checker)
        .arg(".")
        .current_dir(repository().join("crates/khora-codegen-llvm/tests/fixtures/prometheus_go"))
        .env("GOFLAGS", "-mod=readonly")
        .output()
        .expect("go should start");
    assert!(built.status.success(), "the Go checker did not build: {}", String::from_utf8_lossy(&built.stderr));
    for (backend, file, scrape) in every_shape("go") {
        let parsed = Command::new(&checker).arg(&file).output().expect("the checker should start");
        assert!(
            parsed.status.success(),
            "on `{backend}`, Prometheus' parsers refused the scrape: {}\n{scrape}",
            String::from_utf8_lossy(&parsed.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&parsed.stdout), PARSED_BY_GO, "on `{backend}`");
    }
}

/// A router with the metrics route and one route of its own that counts.
const SERVED: &str = r#"module demo::main;

import std::core::{ChildFailed, List, SharedFn};
import std::net::http::{HttpError, Request, Response, Router};
import prometheus::metrics::{MetricError, Registry};
import prometheus::serve::{MountError, mount};

pub fn main() -> () raises HttpError + ChildFailed + MetricError + MountError {
  let registry = Registry::new();
  let hits = registry.counter("hello_total", "Greetings.", List::Nil)!.labels(List::Nil)!;
  Router::new()
    |> Router::get("/hello", SharedFn::of(fn (_request: Request) => {
         hits.inc();
         Response::text(200, "hi")
       }))
    |> mount(registry)!
    |> Router::listen(@PORT@)!
}
"#;

struct Server {
    child: std::process::Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(exe: &Path, backend: &str) -> Server {
    let mut child = Command::new(exe)
        .env("KHORA_FIBERS", backend)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the server should start");
    let mut stdout = child.stdout.take().expect("piped");
    let announced = format!("listening on {PORT}");
    let mut said = Vec::new();
    let mut byte = [0u8; 1];
    while !String::from_utf8_lossy(&said).contains(&announced) {
        match stdout.read(&mut byte) {
            Ok(0) | Err(_) => {
                let _ = child.kill();
                panic!("on `{backend}`, the server stopped before it was listening: {}", String::from_utf8_lossy(&said));
            }
            Ok(_) => said.push(byte[0]),
        }
    }
    Server { child }
}

/// One request on its own connection, and everything the server sent back.
fn ask(path: &str) -> String {
    let deadline = std::time::Duration::from_secs(10);
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", PORT)).expect("the server should accept");
    socket.set_read_timeout(Some(deadline)).expect("a read deadline");
    socket
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes())
        .expect("the request should send");
    let mut said = Vec::new();
    let _ = socket.read_to_end(&mut said);
    String::from_utf8_lossy(&said).into_owned()
}

/// **What this prevents: a scrape endpoint a scraper does not recognize.**
/// Without `version=0.0.4` in the content type, a current Prometheus guesses
/// at the format and may read the body as OpenMetrics.
#[test]
fn a_router_serves_the_metrics_with_their_content_type() {
    let exe = build("prometheus_served", &SERVED.replace("@PORT@", &PORT.to_string()));
    for backend in BACKENDS {
        let _server = start(&exe, backend);
        assert!(ask("/hello").ends_with("\r\n\r\nhi"), "on `{backend}`, /hello did not answer");
        assert!(ask("/hello").ends_with("\r\n\r\nhi"), "on `{backend}`, /hello did not answer");
        let answer = ask("/metrics");
        let (head, body) = answer.split_once("\r\n\r\n").expect("a head and a body");
        assert!(head.starts_with("HTTP/1.1 200 "), "on `{backend}`: {head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("\r\ncontent-type: text/plain; version=0.0.4; charset=utf-8\r\n"),
            "on `{backend}`: {head}"
        );
        assert_eq!(body, "# HELP hello_total Greetings.\n# TYPE hello_total counter\nhello_total 2\n", "on `{backend}`");
    }
}

/// Its own port, for the documented service: see [`PORT`].
const SHOP_PORT: u16 = 18744;

/// The page's one `khora` block and one `text` block, in that order.
///
/// Line endings are normalized first: a Windows checkout has the page with
/// CRLF (`.gitattributes` pins only `*.sh`), and the blocks are compared
/// with what a program prints, which is LF everywhere.
fn documented() -> (String, String) {
    let page = std::fs::read_to_string(repository().join("website/content/docs/packages/prometheus.md"))
        .expect("the package's page")
        .replace("\r\n", "\n");
    let block = |fence: &str| -> String {
        let start = page.find(fence).unwrap_or_else(|| panic!("no {fence} block on the page")) + fence.len();
        let end = start + page[start..].find("\n```").expect("a closing fence");
        page[start..end].to_string()
    };
    (block("```khora\n"), format!("{}\n", block("```text\n")))
}

/// One request with `method` on its own connection, and the body.
fn send(port: u16, method: &str, path: &str) -> String {
    let deadline = std::time::Duration::from_secs(10);
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", port)).expect("the server should accept");
    socket.set_read_timeout(Some(deadline)).expect("a read deadline");
    socket
        .write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())
        .expect("the request should send");
    let mut said = Vec::new();
    let _ = socket.read_to_end(&mut said);
    let said = String::from_utf8_lossy(&said).into_owned();
    said.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or(said)
}

/// A scrape with every bucket line the page leaves out removed, and each
/// `_sum`'s value replaced by `0`.
///
/// The page shows the `0.005` and `+Inf` buckets only, and says so. The sums
/// are elapsed milliseconds, and a debug build on a busy machine can take one.
fn as_the_page_shows(scrape: &str) -> String {
    let mut out = String::new();
    for line in scrape.lines() {
        if line.contains("_bucket{") && !line.contains("le=\"0.005\"") && !line.contains("le=\"+Inf\"") {
            continue;
        }
        if let Some((series, _)) = line.split_once("_sum{").and_then(|(n, rest)| rest.split_once("} ").map(|(l, v)| ((n, l), v))) {
            out.push_str(&format!("{}_sum{{{}}} 0\n", series.0, series.1));
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// **What this prevents: a worked example that does not compile, or that
/// prints something other than what the page says it prints.** The page's
/// checker cannot load a package, so this is what holds the example to it:
/// the block is taken from the page, built against the package, served, and
/// sent the requests the page describes.
#[test]
fn the_documented_service_answers_as_documented() {
    let (program, shown) = documented();
    assert!(program.contains("Router::listen(8080)"), "the example listens on 8080");
    let exe = build("prometheus_documented", &program.replace("Router::listen(8080)", &format!("Router::listen({SHOP_PORT})")));
    let mut child = Command::new(&exe).stdout(std::process::Stdio::piped()).spawn().expect("the service should start");
    let mut stdout = child.stdout.take().expect("piped");
    let mut said = Vec::new();
    let mut byte = [0u8; 1];
    while !String::from_utf8_lossy(&said).contains("listening on") {
        match stdout.read(&mut byte) {
            Ok(0) | Err(_) => {
                let _ = child.kill();
                panic!("the service stopped before it was listening");
            }
            Ok(_) => said.push(byte[0]),
        }
    }
    let _server = Server { child };
    assert_eq!(send(SHOP_PORT, "GET", "/orders"), "[]");
    assert_eq!(send(SHOP_PORT, "GET", "/orders"), "[]");
    assert_eq!(send(SHOP_PORT, "POST", "/refunds"), "refunds are closed");
    let scrape = send(SHOP_PORT, "GET", "/metrics");
    assert_eq!(as_the_page_shows(&scrape), shown, "the whole scrape was:\n{scrape}");
}
