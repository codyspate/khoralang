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

    let running: Vec<_> = tests
        .into_iter()
        .map(|test| {
            let name = test.name.clone();
            let code = test.code;
            let call = test.call;
            let handle = std::thread::spawn(move || {
                let code = code;
                let _entered = enter(Fiber::spawned());
                let mut payload: u64 = 0;
                let which = (call)(code.0, &raw mut payload);
                Tagged { which, payload }
            });
            (name, handle)
        })
        .collect();

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
    for (name, handle) in running {
        total += 1;
        let verdict = match handle.join() {
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
        let _ = writeln!(std::io::stdout().lock(), "test {name} ... {verdict}");
    }

    let passed = total - failed;
    let mut out = std::io::stdout().lock();
    let _ = match filtered_out {
        0 => writeln!(out, "\n{passed} passed, {failed} failed"),
        skipped => writeln!(out, "\n{passed} passed, {failed} failed, {skipped} filtered out"),
    };
    i32::from(failed != 0)
}
