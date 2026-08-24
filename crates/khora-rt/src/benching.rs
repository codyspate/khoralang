//! The benchmark runner.
//!
//! A `bench` block lowers to the same thing a `test` block does — a body with
//! no name, no parameters and no caller — and the difference is entirely in
//! what happens to it here. A test is run once and either passed or did not. A
//! bench is run many times and what comes out is a distribution.
//!
//! # Why percentiles rather than a mean
//!
//! A mean over a run that included one garbage-collection pause, one scheduler
//! preemption or one page fault reports a number that describes none of the
//! iterations. The distribution is what somebody deciding whether a change made
//! things worse actually needs, and the tail is usually the interesting half:
//! P99 is what a server's slowest requests look like, and it can move by an
//! order of magnitude while the mean does not move at all.
//!
//! P50, P95 and P99 with the count, and no mean at all — offering one invites
//! somebody to quote it.
//!
//! # What this does not do
//!
//! **It does not stop the optimizer discarding the work.** A bench whose body
//! computes a value nobody reads may be compiled to nothing, and would then
//! report a few nanoseconds very confidently. Khora has no `black_box` yet, and
//! adding one means a compiler intrinsic rather than a library function. Until
//! then a bench body should end in something observable — a `print`, or a
//! mutation of a `Shared` — and this is written down here because the failure
//! is silent and looks like a win.
//!
//! **It runs one at a time, not one fiber each.** The opposite of the test
//! runner, and for the same reason it is right there: overlapping tests find
//! tests that lie, and overlapping benches contend for the same cores and
//! measure that instead.

use super::*;
use crate::cancel::ON_FIBER;
use crate::fiber::Handed;
use crate::heap::khora_drop;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct PendingBench {
    name: String,
    code: Handed,
    call: Trampoline0,
}

static PENDING: Mutex<Vec<PendingBench>> = Mutex::new(Vec::new());

/// How long to keep going before reporting, unless the cap comes first.
const BUDGET: Duration = Duration::from_millis(500);
/// Enough samples for a P99 to mean something: below a hundred, the
/// ninety-ninth percentile *is* the maximum and says so about one iteration.
const MIN_SAMPLES: usize = 100;
/// A ceiling, so that a bench measuring something in nanoseconds does not
/// collect ten million samples to fill its budget.
const MAX_SAMPLES: usize = 100_000;
/// Discarded. The first iterations pay for cold caches and lazy paging, and
/// including them makes P99 a report about start-up.
const WARMUP: usize = 5;

/// Registers a bench. Called once per `bench` block by the generated entry.
///
/// # Safety
///
/// As [`crate::testing::khora_test_register`]: `name` must point at `len` bytes
/// of UTF-8 that outlive the run, and `code` must be a compiled body.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_bench_register(
    name: *const u8,
    len: usize,
    code: *const u8,
    call: Trampoline0,
) {
    // SAFETY: the caller guarantees `len` bytes at `name`, live for the run.
    let bytes = if len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(name, len) } };
    let name = String::from_utf8_lossy(bytes).into_owned();
    if let Ok(mut pending) = PENDING.lock() {
        pending.push(PendingBench { name, code: Handed(code as *mut u8), call });
    }
}

/// Runs every registered bench and reports its distribution.
///
/// Returns the process's exit status: non-zero only if a bench *failed*, which
/// a bench can do — its body is ordinary Khora and `assert` works in it. A slow
/// bench is not a failure; nothing here knows what slow is.
#[unsafe(no_mangle)]
pub extern "C" fn khora_bench_run() -> i32 {
    let benches: Vec<PendingBench> = match PENDING.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => return 1,
    };

    let filter = crate::testing::name_filter();
    let mut out = std::io::stdout().lock();

    let selected: Vec<PendingBench> = benches
        .into_iter()
        .filter(|b| filter.as_ref().is_none_or(|want| b.name.contains(want.as_str())))
        .collect();

    if selected.is_empty() {
        let _ = out.write_all(b"no benchmarks\n");
        return 0;
    }

    let mut failed = 0usize;
    for bench in selected {
        ON_FIBER.with(|f| f.set(true));

        let mut samples: Vec<u64> = Vec::new();
        let mut broke = None;
        let started = Instant::now();

        for iteration in 0.. {
            let at = Instant::now();
            let mut payload: u64 = 0;
            let which = (bench.call)(bench.code.0, &raw mut payload);
            let elapsed = at.elapsed();

            if which != 0 {
                if which != FAILED_WHICH && which != CANCELLED_WHICH {
                    // Not ours to interpret, and freeing its fields would need
                    // a drop routine the runtime cannot know.
                    // SAFETY: a live Khora object, or null.
                    unsafe { khora_drop(payload as *mut u8, None) };
                }
                broke = Some(match which {
                    w if w == FAILED_WHICH => "FAILED",
                    w if w == CANCELLED_WHICH => "cancelled",
                    _ => "raised",
                });
                break;
            }

            if iteration >= WARMUP {
                samples.push(elapsed.as_nanos().min(u64::MAX as u128) as u64);
            }
            let enough = samples.len() >= MIN_SAMPLES && started.elapsed() >= BUDGET;
            if enough || samples.len() >= MAX_SAMPLES {
                break;
            }
        }

        if let Some(why) = broke {
            failed += 1;
            let _ = writeln!(out, "bench {} ... {why}", bench.name);
            continue;
        }

        samples.sort_unstable();
        let _ = writeln!(
            out,
            "bench {} ... P50 {}  P95 {}  P99 {}  ({} samples)",
            bench.name,
            nanos(percentile(&samples, 50.0)),
            nanos(percentile(&samples, 95.0)),
            nanos(percentile(&samples, 99.0)),
            samples.len()
        );
    }

    i32::from(failed != 0)
}

/// The nearest-rank percentile of a sorted slice.
///
/// Nearest-rank rather than interpolated: every value it reports is a
/// measurement that actually happened, which is the right property for a
/// latency number somebody is going to quote. Interpolation invents a duration
/// no iteration took.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

/// A duration in the largest unit that keeps it readable.
fn nanos(value: u64) -> String {
    match value {
        n if n < 1_000 => format!("{n}ns"),
        n if n < 1_000_000 => format!("{:.1}us", n as f64 / 1_000.0),
        n if n < 1_000_000_000 => format!("{:.1}ms", n as f64 / 1_000_000.0),
        n => format!("{:.2}s", n as f64 / 1_000_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nearest-rank: P100 is the largest sample, P50 of ten is the fifth.
    #[test]
    fn percentiles_are_real_measurements() {
        let sorted: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&sorted, 50.0), 5);
        assert_eq!(percentile(&sorted, 95.0), 10);
        assert_eq!(percentile(&sorted, 99.0), 10);
        assert_eq!(percentile(&sorted, 100.0), 10);
    }

    /// The tail is the interesting half, so a single outlier must reach P99 and
    /// must not reach P50. That is the whole reason there is no mean here.
    #[test]
    fn one_outlier_moves_the_tail_and_not_the_middle() {
        let mut samples = vec![10u64; 99];
        samples.push(10_000);
        samples.sort_unstable();
        assert_eq!(percentile(&samples, 50.0), 10);
        assert_eq!(percentile(&samples, 99.0), 10);
        assert_eq!(percentile(&samples, 100.0), 10_000);
    }

    #[test]
    fn an_empty_sample_set_does_not_panic() {
        assert_eq!(percentile(&[], 99.0), 0);
    }

    #[test]
    fn durations_read_in_a_sensible_unit() {
        assert_eq!(nanos(999), "999ns");
        assert_eq!(nanos(1_500), "1.5us");
        assert_eq!(nanos(2_500_000), "2.5ms");
        assert_eq!(nanos(3_000_000_000), "3.00s");
    }
}
