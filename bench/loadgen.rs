// Requests a second against a server that is already running, and what the
// latency and the server's memory were while they were being answered.
//
//     rustc -O -o bench/loadgen.exe bench/loadgen.rs
//     ./bench/loadgen.exe --port 18952 --label service --connections 64 --seconds 5
//
// **Why this exists.** `bench/load.py` spends one operating-system *process*
// per connection, because one Python process cannot drive more than a fraction
// of what these servers answer. `bench/compare.py` then established that the
// rig itself was what the numbers described: pointed at one unchanged server
// it reported 747k requests a second at 48 processes, 1.50M at 96 and 2.43M at
// 160. A rate that climbs with the client's concurrency is the client's rate.
//
// **What was actually slow, measured rather than assumed.** A connection doing
// blocking reads on this platform answers about 7,900 requests a second, and
// the same connection with the socket in non-blocking mode, spinning on the
// read, answers 42,091 -- five times as many, against the same server in the
// same sitting. The cost was never the client's arithmetic. It was the
// thread going to sleep on every response and being woken again, about 120
// microseconds each time, on a round trip that takes 29.
//
// So: no blocking, and no thread per connection either. A handful of threads
// each drive many non-blocking connections in a round-robin loop, which is
// what lets the generator use a few cores hard instead of asking the scheduler
// to wake sixty-four threads that are each idle almost all of the time. The
// server needs the rest of the machine, and a generator that takes every core
// is measuring itself.
//
// **No pipelining, deliberately.** `control_keepalive.rs` and `floor` answer
// one response per `read` without parsing what arrived, which is honest for
// what they are for and means several requests in one write would be answered
// once. One request in flight per connection is the only shape that measures
// all of these servers the same way, and it is what `load.py` did, so the two
// remain comparable.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const REQUEST: &[u8] = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n";

fn main() {
    let options = Options::parse();

    let per_thread = split(options.connections, options.threads);
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicU64::new(0));
    let mut threads = Vec::new();

    for (index, count) in per_thread.iter().copied().enumerate() {
        if count == 0 {
            continue;
        }
        let stop = Arc::clone(&stop);
        let ready = Arc::clone(&ready);
        let port = options.port;
        // One thread carries the probe, so exactly one connection in the whole
        // run is timed and it is competing with all the others.
        let probe = index == 0;
        threads.push(std::thread::spawn(move || drive(port, count, probe, &stop, &ready)));
    }

    // **The clock starts when every connection is open.** A connection still
    // being established is not offering load, and counting the time it took to
    // open against the rate makes a server look slower the more connections
    // are pointed at it.
    let deadline = Instant::now() + Duration::from_secs(30);
    while ready.load(Ordering::Relaxed) < options.connections as u64 {
        if Instant::now() > deadline {
            eprintln!(
                "only {} of {} connections opened within 30s",
                ready.load(Ordering::Relaxed),
                options.connections
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut watcher = options.watch.map(Rss::watching);
    let began = Instant::now();
    std::thread::sleep(Duration::from_secs(options.seconds));
    stop.store(true, Ordering::Relaxed);
    let elapsed = began.elapsed().as_secs_f64();

    let mut answered = 0u64;
    let mut failed = 0u64;
    let mut latencies: Vec<u32> = Vec::new();
    for thread in threads {
        let worked = thread.join().expect("a load thread");
        answered += worked.answered;
        failed += worked.failed;
        latencies.extend(worked.latencies);
    }
    let peak = watcher.as_mut().and_then(Rss::stop);
    latencies.sort_unstable();

    let rate = answered as f64 / elapsed;
    // **The provenance goes with the number, not beside it in a README.**
    // The fourth of the conditions on /docs/performance/ is that the machine,
    // the profile and the date are printed with the figure; a number that has
    // to be paired up with its circumstances by hand eventually is not.
    println!("machine     {} {} {}-core", std::env::consts::OS, std::env::consts::ARCH, cores());
    println!("at          {} (unix seconds)", now());
    println!("label       {}", options.label);
    println!("connections {}", options.connections);
    println!("threads     {}", options.threads);
    println!("seconds     {elapsed:.2}");
    println!("answered    {answered}");
    println!("req/s       {rate:.0}");
    if failed > 0 {
        println!("failed      {failed}");
    }
    if let Some(p50) = percentile(&latencies, 50.0) {
        println!("latency_p50_us  {p50}");
        println!("latency_p95_us  {}", percentile(&latencies, 95.0).unwrap_or(p50));
        println!("latency_p99_us  {}", percentile(&latencies, 99.0).unwrap_or(p50));
        println!("latency_max_us  {}", latencies.last().copied().unwrap_or(p50));
        println!("latency_samples {}", latencies.len());
    }
    if let Some(peak) = peak {
        println!("server_peak_rss_kb {peak}");
    }
    println!("json {}", as_json(&options, elapsed, answered, failed, rate, &latencies, peak));
}

/// `connections` shared out over `threads`, the remainder going to the first
/// few rather than to one.
fn split(connections: usize, threads: usize) -> Vec<usize> {
    let threads = threads.max(1).min(connections.max(1));
    let each = connections / threads;
    let extra = connections % threads;
    (0..threads).map(|i| each + usize::from(i < extra)).collect()
}

/// What one thread's connections did.
struct Worked {
    answered: u64,
    failed: u64,
    latencies: Vec<u32>,
}

/// One connection's state inside the round-robin loop.
struct Wire {
    socket: TcpStream,
    /// Bytes of the current answer already seen, kept only so a terminator
    /// split across two reads is still found.
    carry: [u8; 3],
    carried: usize,
    /// When the request now in flight was written. `None` on connections that
    /// are not the probe.
    sent: Option<Instant>,
    live: bool,
}

/// `count` connections on this thread, one request in flight on each.
fn drive(port: u16, count: usize, probe: bool, stop: &AtomicBool, ready: &AtomicU64) -> Worked {
    let mut worked = Worked { answered: 0, failed: 0, latencies: Vec::new() };
    if probe {
        // Reserved once, so that a reallocation is never timed as latency.
        worked.latencies.reserve(8_000_000);
    }

    let mut wires: Vec<Wire> = Vec::with_capacity(count);
    for _ in 0..count {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(socket) => {
                socket.set_nodelay(true).ok();
                socket.set_nonblocking(true).ok();
                wires.push(Wire {
                    socket,
                    carry: [0; 3],
                    carried: 0,
                    sent: None,
                    live: true,
                });
            }
            Err(_) => worked.failed += 1,
        }
        ready.fetch_add(1, Ordering::Relaxed);
    }

    // Only the first connection of the probe thread is timed.
    let timed = if probe { 0usize } else { usize::MAX };
    let mut buffer = [0u8; 8192];

    // Every connection starts with a request in flight, so the first pass of
    // the loop below has something to read on all of them.
    for (index, wire) in wires.iter_mut().enumerate() {
        if send(wire, index == timed) {
            worked.failed += 1;
        }
    }

    while !stop.load(Ordering::Relaxed) {
        let mut progressed = false;
        for index in 0..wires.len() {
            if !wires[index].live {
                continue;
            }
            match poll(&mut wires[index], &mut buffer) {
                Answer::Waiting => {}
                Answer::Gone => {
                    wires[index].live = false;
                    worked.failed += 1;
                }
                Answer::Complete => {
                    progressed = true;
                    worked.answered += 1;
                    if let Some(at) = wires[index].sent.take() {
                        let took = at.elapsed().as_micros();
                        worked.latencies.push(took.min(u32::MAX as u128) as u32);
                    }
                    if send(&mut wires[index], index == timed) {
                        wires[index].live = false;
                        worked.failed += 1;
                    }
                }
            }
        }
        if !progressed {
            // Nothing was ready anywhere. Spinning is the point -- it is what
            // keeps the round trip off the thread scheduler -- but a hint to
            // the processor that this is a spin costs nothing and behaves
            // better on a machine whose cores are shared with the server.
            std::hint::spin_loop();
        }
    }
    worked
}

/// Writes a request. Answers `true` when the connection is finished.
fn send(wire: &mut Wire, timed: bool) -> bool {
    wire.carried = 0;
    wire.sent = if timed { Some(Instant::now()) } else { None };
    loop {
        match wire.socket.write_all(REQUEST) {
            Ok(()) => return false,
            // A send buffer with no room is not a failure; it is the server
            // being slower than this loop, which is the case worth measuring.
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => std::hint::spin_loop(),
            Err(_) => return true,
        }
    }
}

enum Answer {
    Waiting,
    Complete,
    Gone,
}

/// One non-blocking read, and whether it finished the answer.
fn poll(wire: &mut Wire, buffer: &mut [u8; 8192]) -> Answer {
    match wire.socket.read(&mut buffer[wire.carried..]) {
        Ok(0) => Answer::Gone,
        Ok(read) => {
            buffer[..wire.carried].copy_from_slice(&wire.carry[..wire.carried]);
            let filled = wire.carried + read;
            if buffer[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                Answer::Complete
            } else {
                // The terminator can straddle two reads. Every server here
                // answers in one write so this does not happen in practice,
                // and the failure it prevents -- a response counted late or
                // never -- is the kind of wrong number that gets believed.
                wire.carried = filled.min(3);
                let from = filled - wire.carried;
                wire.carry[..wire.carried].copy_from_slice(&buffer[from..filled]);
                Answer::Waiting
            }
        }
        Err(ref e) if e.kind() == ErrorKind::WouldBlock => Answer::Waiting,
        Err(ref e) if e.kind() == ErrorKind::Interrupted => Answer::Waiting,
        Err(_) => Answer::Gone,
    }
}

/// How many hardware threads the machine has, or 0 when it will not say.
fn cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
}

/// Seconds since the epoch. Raw rather than formatted, because formatting a
/// date without a dependency is more code than it is worth and a driver that
/// wants a calendar can convert one number.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn percentile(sorted: &[u32], nth: f64) -> Option<u32> {
    if sorted.is_empty() {
        return None;
    }
    let at = ((nth / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted.get(at).copied()
}

fn as_json(
    options: &Options,
    elapsed: f64,
    answered: u64,
    failed: u64,
    rate: f64,
    latencies: &[u32],
    peak: Option<u64>,
) -> String {
    let mut out = format!(
        "{{\"label\":\"{}\",\"os\":\"{}\",\"arch\":\"{}\",\"cores\":{},\"at\":{},\"connections\":{},\"threads\":{},\"seconds\":{:.3},\"answered\":{},\"failed\":{},\"rate\":{:.1}",
        options.label,
        std::env::consts::OS,
        std::env::consts::ARCH,
        cores(),
        now(),
        options.connections,
        options.threads,
        elapsed,
        answered,
        failed,
        rate
    );
    if let Some(p50) = percentile(latencies, 50.0) {
        out.push_str(&format!(
            ",\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"max_us\":{}",
            p50,
            percentile(latencies, 95.0).unwrap_or(p50),
            percentile(latencies, 99.0).unwrap_or(p50),
            latencies.last().copied().unwrap_or(p50)
        ));
    }
    if let Some(peak) = peak {
        out.push_str(&format!(",\"peak_rss_kb\":{peak}"));
    }
    out.push('}');
    out
}

// --- the server's resident memory ------------------------------------------

/// Samples a process's resident set while a run is under way.
///
/// **Shelling out rather than binding an API.** The bench tree has no
/// dependencies and is built with a bare `rustc -O`, which is what keeps it
/// runnable by anybody with a toolchain and no lockfile. `tasklist` and
/// `/proc` are the versions of this question every machine can already answer.
struct Rss {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Rss {
    fn watching(pid: u32) -> Rss {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let thread = {
            let stop = Arc::clone(&stop);
            let peak = Arc::clone(&peak);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Some(kb) = resident_kb(pid) {
                        peak.fetch_max(kb, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        };
        Rss { stop, peak, thread: Some(thread) }
    }

    fn stop(&mut self) -> Option<u64> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        match self.peak.load(Ordering::Relaxed) {
            0 => None,
            peak => Some(peak),
        }
    }
}

#[cfg(windows)]
fn resident_kb(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // `"name","pid","session","#","4,468 K"` -- and **the memory field has a
    // thousands comma inside it**, so splitting on the last comma returns
    // `468 K"` and reports four and a half megabytes as 468 KB. The field
    // separator is quote-comma-quote, which the number's own comma is not.
    let last = text.rsplit(",\"").next()?.trim().trim_matches('"');
    let digits: String = last.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(not(windows))]
fn resident_kb(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
            return digits.parse().ok();
        }
    }
    None
}

// --- the command line -------------------------------------------------------

struct Options {
    port: u16,
    label: String,
    connections: usize,
    threads: usize,
    seconds: u64,
    watch: Option<u32>,
}

impl Options {
    fn parse() -> Options {
        let mut options = Options {
            port: 0,
            label: String::from("server"),
            connections: 64,
            // Eight, because that is where it stopped mattering. Against the
            // keep-alive control on a sixteen-core machine the rate climbed
            // 68k, 122k, 181k, 208k at one, two, four and eight threads, and
            // then sat at 210k, 210k, 215k at twelve, sixteen and twenty-four.
            // A generator whose rate stops changing when it is given more of
            // the machine is no longer the thing being measured, which is the
            // first of the four conditions on /docs/performance/.
            threads: 8,
            seconds: 5,
            watch: None,
        };
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < args.len() {
            let flag = args[i].clone();
            let value = |i: &mut usize| {
                *i += 1;
                args.get(*i).cloned().unwrap_or_else(|| {
                    eprintln!("{flag} needs a value");
                    std::process::exit(2)
                })
            };
            match args[i].as_str() {
                "--port" => options.port = value(&mut i).parse().expect("a port"),
                "--label" => options.label = value(&mut i),
                "--connections" => options.connections = value(&mut i).parse().expect("a count"),
                "--threads" => options.threads = value(&mut i).parse().expect("a count"),
                "--seconds" => options.seconds = value(&mut i).parse().expect("a duration"),
                "--watch-pid" => options.watch = Some(value(&mut i).parse().expect("a pid")),
                "--help" | "-h" => {
                    println!("loadgen --port N [--label NAME] [--connections N] [--threads N] [--seconds N] [--watch-pid N]");
                    std::process::exit(0)
                }
                other => {
                    eprintln!("unknown option {other}");
                    std::process::exit(2)
                }
            }
            i += 1;
        }
        if options.port == 0 {
            eprintln!("--port is required");
            std::process::exit(2);
        }
        if options.connections == 0 {
            eprintln!("--connections must be at least 1");
            std::process::exit(2);
        }
        options
    }
}
