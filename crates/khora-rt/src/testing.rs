//! The test runner.
//!
//! A `test` block lowers to an ordinary function, registers itself here, and is
//! run on a fiber of its own so that a test which hangs does not hang the rest.

use super::*;
use crate::current::{enter, Fiber};
use crate::fiber::Handed;
use crate::heap::khora_drop;
use std::io::Write;
use std::sync::Mutex;

/// One test, waiting to be run.
struct PendingTest {
    name: String,
    code: Handed,
    call: Trampoline0,
}

/// A test that has been started, or one that has already finished.
///
/// The two are the same to the reporting loop below and different to the run:
/// a parallel run holds a join handle per test and joins them all afterwards; a
/// serial one has joined each before starting the next, and carries the result.
enum Finished {
    Running(std::thread::JoinHandle<Tagged>),
    Already(std::thread::Result<Tagged>),
}

impl Finished {
    fn outcome(self) -> std::thread::Result<Tagged> {
        match self {
            Finished::Running(handle) => handle.join(),
            Finished::Already(result) => result,
        }
    }
}

/// The tests a program declared, in the order they were written.
static PENDING: Mutex<Vec<PendingTest>> = Mutex::new(Vec::new());

/// Registers a test. Called once per `test` block by the generated entry point.
///
/// # Safety
///
/// `name` must point at `len` bytes of UTF-8 that outlive the run — a string
/// literal does — and `code` must be a test's compiled body.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_test_register(
    name: *const u8,
    len: usize,
    code: *const u8,
    call: Trampoline0,
) {
    // SAFETY: the caller guarantees `len` bytes at `name`, live for the run.
    let bytes = if len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(name, len) } };
    let name = String::from_utf8_lossy(bytes).into_owned();
    if let Ok(mut pending) = PENDING.lock() {
        pending.push(PendingTest { name, code: Handed(code as *mut u8), call });
    }
}

/// Which names to run, from the command line.
///
/// **Read from `argv`, not from an environment variable**, so that the compiled
/// test executable behaves the same whether `khora test --filter x` started it
/// or somebody ran it directly. A test binary that only obeys its filter when a
/// build tool sets a variable is a test binary nobody can debug by hand.
///
/// `--filter x` and `--filter=x` both work, and a bare argument is taken as the
/// filter too, which is what `cargo test name` trained everyone to expect.
/// Substring rather than a pattern: a regular expression here is a dependency
/// and a syntax to document, and nobody has wanted one yet.
pub(crate) fn name_filter() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--filter=") {
            return Some(value.to_string());
        }
        if arg == "--filter" {
            return args.next();
        }
        if !arg.starts_with('-') {
            return Some(arg);
        }
    }
    None
}

/// What the harness is in the middle of, for a trap to name.
///
/// **A trap ends the process, and that cannot change here.** `contain.rs` gives
/// the argument: containing one needs an unwinder, because Perceus leaves live
/// reference counts between the trap and any landing point, and a fiber
/// abandoned without running them leaks everything it touched. An exported call
/// escapes that by construction; a `test` block does not.
///
/// What *can* change is what the reader is told on the way out. A trapping test
/// printed `Int division by zero on fiber 5` -- a fiber number, in a run where
/// the tests are named -- and then took the process down mid-loop, so the
/// remaining tests neither ran nor were reported, and no summary line was ever
/// printed. Somebody reading `test a ... ok` and a stray message can reasonably
/// conclude the rest passed.
///
/// So the harness records what it started and what it has finished, and the
/// trap path reads it. Nothing is contained; the death is explained.
pub(crate) mod progress {
    use std::sync::Mutex;

    /// The name of every test, and how far the report has got.
    pub(crate) struct Progress {
        /// Every test this run intends to execute, in order.
        pub(crate) names: Vec<String>,
        /// How many have been reported. A trap happens in the one after.
        pub(crate) reported: usize,
        /// Whether a trap should say anything at all. False under `khora run`,
        /// where there is no harness and the plain message is right.
        pub(crate) active: bool,
    }

    pub(crate) static PROGRESS: Mutex<Option<Progress>> = Mutex::new(None);

    /// Records the run this harness is about to perform.
    pub(crate) fn begin(names: Vec<String>) {
        if let Ok(mut held) = PROGRESS.lock() {
            *held = Some(Progress { names, reported: 0, active: true });
        }
    }

    /// Notes that one more test has been reported.
    pub(crate) fn reported() {
        if let Ok(mut held) = PROGRESS.lock() {
            if let Some(progress) = held.as_mut() {
                progress.reported += 1;
            }
        }
    }

    /// The harness is done, so a later trap is not a test's.
    pub(crate) fn done() {
        if let Ok(mut held) = PROGRESS.lock() {
            if let Some(progress) = held.as_mut() {
                progress.active = false;
            }
        }
    }
}

/// What a trap should add, when one happens inside `khora test`.
///
/// Empty under `khora run` and `khora bench`, where the plain message is the
/// whole story and a harness sentence would be a lie.
///
/// **Deliberately not naming one test as the culprit** when the run is
/// parallel. Several are in flight at once and the harness cannot know which
/// one divided by zero without asking each fiber, which is exactly the
/// bookkeeping a trap has no stack to do. It names the ones that were still
/// running, which is true, and says what did not run, which is the part a
/// reader is otherwise never told.
pub(crate) fn trap_context() -> String {
    let Ok(held) = progress::PROGRESS.lock() else {
        return String::new();
    };
    let Some(progress) = held.as_ref() else {
        return String::new();
    };
    if !progress.active {
        return String::new();
    }

    let remaining: Vec<&str> =
        progress.names[progress.reported.min(progress.names.len())..].iter().map(String::as_str).collect();
    if remaining.is_empty() {
        return String::new();
    }

    let mut out = String::from("\nkhora: this ended the test run. ");
    if remaining.len() == 1 {
        out.push_str(&format!("`{}` was running.\n", remaining[0]));
    } else {
        out.push_str(&format!(
            "{} test(s) had not been reported: {}.\n",
            remaining.len(),
            remaining.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
        ));
    }
    out.push_str(
        "khora: a trap ends the process, so the rest of the run did not happen. \
         `khora test --filter <name>` runs one at a time.\n",
    );
    out
}

/// Says which assertion in the current test failed.
///
/// **A failing test said only that it had failed.** `test a well formed line
/// becomes an entry ... FAILED`, with no line, no values, and no indication
/// which of six assertions it was -- so the way to find out was to delete
/// assertions one at a time until it passed. Somebody did.
///
/// **The ordinal *and* the line.** It was the ordinal alone, on the reasoning
/// that a line needs the debug information only a debug profile emits, and that
/// a message which differs between profiles is worse than one that is always
/// the same. The first half of that is not true: the compiler knows the line
/// while it is lowering the call and can pass it as an immediate, which costs
/// nothing and is identical in both profiles.
///
/// The ordinal stays because it is the thing that cannot be wrong. A line is
/// where the `assert` was written; in a test with a loop or a helper, the same
/// line fails on many different iterations, and the count says which time.
///
/// A `line` of zero means the lowering had no position — nothing generated
/// today is in that case, and printing "line 0" would be worse than saying
/// only what is known.
#[unsafe(no_mangle)]
pub extern "C" fn khora_assert_failed(ordinal: u32, line: u32) {
    let mut err = std::io::stderr().lock();
    if line == 0 {
        let _ = writeln!(err, "khora: assertion {ordinal} failed");
    } else {
        let _ = writeln!(err, "khora: assertion {ordinal} failed, at line {line}");
    }
}

/// The same, for an assertion that was given something to say.
///
/// **A `Bool` is all `assert` has, and a `Bool` cannot say what it saw.**
/// `assert(l.port == 8080)` fails with an ordinal and a line, and the reader's
/// next move is to add a `print` and build again -- which on a program linking
/// `std` is the better part of a minute, to recover a value the assertion was
/// holding and dropped.
///
/// `assert_that` takes the sentence with it:
///
/// ```text
/// khora: assertion 3 failed, at line 47
///   port was 8081
/// ```
///
/// **A message rather than the two values.** An `assert_eq` rendering each side
/// through `Show` was the other candidate, and it is the worse trade here: it
/// needs a `Show` bound, so it does not work for every type a test compares,
/// and it fixes the sentence at "left/right" when the useful thing to say is
/// often neither operand -- the key that was missing, the input that produced
/// this. String interpolation already exists and composes:
/// `assert_that(ok, "port ${l.port} from ${source}")`.
///
/// # Safety
///
/// `message` must point at `len` bytes of UTF-8 live for the duration of the
/// call. Generated code passes a Khora string, which is exactly that.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_assert_that_failed(
    ordinal: u32,
    line: u32,
    message: *const u8,
    len: usize,
) {
    let mut err = std::io::stderr().lock();
    if line == 0 {
        let _ = writeln!(err, "khora: assertion {ordinal} failed");
    } else {
        let _ = writeln!(err, "khora: assertion {ordinal} failed, at line {line}");
    }
    if len > 0 && !message.is_null() {
        // SAFETY: the caller guarantees `len` bytes at `message` for this call.
        let bytes = unsafe { std::slice::from_raw_parts(message, len) };
        let _ = writeln!(err, "  {}", String::from_utf8_lossy(bytes));
    }
}

/// Runs every registered test, one fiber each, and reports.
///
/// Returns the process's exit status: 0 when every test passed.
///
/// **One fiber each, all at once.** That is the point rather than a detail —
/// tests are the first thing anyone writes that is embarrassingly parallel, and
/// a test that only passes when it runs alone is a test that is lying. Isolated
/// by construction too: a fiber has its own cancellation flag, and nothing else
/// is shared but what the program itself shares.
#[unsafe(no_mangle)]
pub extern "C" fn khora_test_run() -> i32 {
    let registered: Vec<PendingTest> = match PENDING.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => return 1,
    };
    let declared = registered.len();

    let filter = name_filter();
    let tests: Vec<PendingTest> = registered
        .into_iter()
        .filter(|t| filter.as_ref().is_none_or(|want| t.name.contains(want.as_str())))
        .collect();

    if tests.is_empty() {
        let mut out = std::io::stdout().lock();
        // Saying how many were skipped, because "no tests" from a filter that
        // matched nothing looks exactly like "no tests" from a file with none,
        // and one of those is a typo.
        // **A filter that matched nothing is a failure; a file with no tests is
        // not.** They print nearly the same sentence and mean opposite things:
        // one is a typo in a command somebody ran deliberately, and in CI it is
        // a step that tested nothing and went green. An evaluator found
        // `khora test --filter zzz` exiting 0 and named it for that reason.
        let missed = match &filter {
            Some(want) if declared > 0 => {
                let _ = writeln!(out, "no tests matching `{want}` ({declared} declared)");
                true
            }
            _ => {
                let _ = out.write_all(b"no tests\n");
                false
            }
        };
        return i32::from(missed);
    }
    let filtered_out = declared - tests.len();

    // What a trap should say if one happens during the run below. Recorded
    // before anything starts, because a trap does not wait for a convenient
    // moment.
    progress::begin(tests.iter().map(|t| t.name.clone()).collect());

    // **A program that counts objects runs its tests one at a time.**
    //
    // `khora_live_count` reports the whole process's heap, and the blocks below
    // run at once -- so a test asking what is live sees every other test's
    // allocations too, and subtracting two of its own readings can answer a
    // *negative* number when another block freed something in between. That is
    // not a flaky test; it is a test measuring something that is not its own.
    //
    // Whether the program counts is already known: the compiler switches the
    // counters on for a module that declares `extern fn khora_live_count`, and
    // nothing else turns them on. So the harness needs no flag and no
    // annotation -- a file with a counting test gets a serial run, and a file
    // without keeps the parallelism, which is most files.
    //
    // The parallel run is still the point for everything else, and the reason
    // is in the doc comment above: a test that only passes when it runs alone
    // is a test that is lying. A test that reads a process-wide counter is the
    // one honest exception, because "alone" is part of what it is measuring.
    let alone = crate::counters::counting();

    let start = |test: PendingTest| {
        let code = test.code;
        let call = test.call;
        std::thread::spawn(move || {
            let code = code;
            let _entered = enter(Fiber::spawned());
            let mut payload: u64 = 0;
            let which = (call)(code.0, &raw mut payload);
            Tagged { which, payload }
        })
    };

    // Each test still gets a thread and a fiber of its own either way: what
    // changes is whether the next one starts before this one has finished.
    let running: Vec<_> = if alone {
        tests
            .into_iter()
            .map(|test| {
                let name = test.name.clone();
                let finished = start(test).join();
                (name, Finished::Already(finished))
            })
            .collect()
    } else {
        tests
            .into_iter()
            .map(|test| {
                let name = test.name.clone();
                (name, Finished::Running(start(test)))
            })
            .collect()
    };

    // **The lock is taken per line rather than held across the loop**, and
    // that is a deadlock rather than a style. A fiber that traps writes its
    // message to stderr and then flushes stdout on the way to `exit`; a
    // `StdoutLock` held here while this thread sat in `join` blocked that
    // flush, and the two threads waited on each other for ever. `khora test`
    // printed the trap and then hung -- in CI a stuck job rather than a red
    // build, which is the worse of the two. `khora run` was always fine,
    // because nothing there holds the lock.
    let mut failed = 0usize;
    let mut total = 0usize;
    for (name, finished) in running {
        total += 1;
        let verdict = match finished.outcome() {
            // A test that ends any way other than "returned" did not pass.
            // Which way it was matters to the reader and not to the count.
            Ok(outcome) if outcome.which == 0 => "ok",
            Ok(outcome) if outcome.which == FAILED_WHICH => "FAILED",
            Ok(outcome) if outcome.which == CANCELLED_WHICH => "cancelled",
            Ok(outcome) => {
                // The error is nobody's to interpret here, and freeing its
                // fields would need a drop routine the runtime cannot know.
                // SAFETY: a live Khora object, or null.
                unsafe { khora_drop(outcome.payload as *mut u8, None) };
                "raised"
            }
            Err(_) => "panicked",
        };
        if verdict != "ok" {
            failed += 1;
        }
        // **Counted before the line is written, not after.** A trap in another
        // thread reads this the instant it happens, and a test whose verdict
        // has been decided is not one the reader should be told "had not been
        // reported" -- which is what the other order produced, listing a test
        // whose `... ok` was already on the screen.
        progress::reported();
        // **Flushed per line, because another thread may end the process.** A
        // trap writes to stderr, which is unbuffered, while this is stdout,
        // which is not when it is a pipe. An unflushed `test a ... ok` sitting
        // in the buffer was emitted *after* the trap's message, or worse,
        // in the middle of it -- the reported output included the literal line
        // `khora: test a ... ok`, which attaches the word `khora:` to a passing
        // test. One flush per test is nothing next to running the test.
        {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "test {name} ... {verdict}");
            let _ = out.flush();
        }
    }

    progress::done();

    let passed = total - failed;
    let mut out = std::io::stdout().lock();
    let _ = match filtered_out {
        0 => writeln!(out, "\n{passed} passed, {failed} failed"),
        skipped => writeln!(out, "\n{passed} passed, {failed} failed, {skipped} filtered out"),
    };
    i32::from(failed != 0)
}
