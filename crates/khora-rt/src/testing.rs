//! The test runner.
//!
//! A `test` block lowers to an ordinary function, registers itself here, and is
//! run on a fiber of its own so that a test which hangs does not hang the rest.

use super::*;
use crate::cancel::ON_FIBER;
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
    let tests: Vec<PendingTest> = match PENDING.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => return 1,
    };
    if tests.is_empty() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"no tests\n");
        return 0;
    }

    let running: Vec<_> = tests
        .into_iter()
        .map(|test| {
            let name = test.name.clone();
            let code = test.code;
            let call = test.call;
            let handle = std::thread::spawn(move || {
                let code = code;
                ON_FIBER.with(|f| f.set(true));
                let mut payload: u64 = 0;
                let which = (call)(code.0, &raw mut payload);
                Tagged { which, payload }
            });
            (name, handle)
        })
        .collect();

    let mut failed = 0usize;
    let mut total = 0usize;
    let mut out = std::io::stdout().lock();
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
        let _ = writeln!(out, "test {name} ... {verdict}");
    }

    let passed = total - failed;
    let _ = writeln!(out, "\n{passed} passed, {failed} failed");
    i32::from(failed != 0)
}
