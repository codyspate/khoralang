//! What the runtime counts, for tests to read.
//!
//! Allocations, live objects, and a tick that goes up every time it is read.
//! None of it is load-bearing: `khora_live_count` returning to zero is how
//! every leak test in the repository states its claim, and
//! `docs/design/compatibility.md` says allocation behaviour is not part of the
//! language's promise — so these are the compiler's own instrument rather than
//! a contract with anybody.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Whether anything is counting.
///
/// **Off, and switched on by the compiler for a program that asks.** Two
/// atomic read-modify-writes on every allocation is not free: removing them
/// took `bench/iteration`'s `for` loop from 114ms to 85ms and
/// `std::net::http` from 53,907 requests a second to 59,058. A relaxed load
/// and a not-taken branch costs neither -- 83ms, the whole of the win --
/// which is why this is a switch rather than a build of its own.
///
/// The reuse path is why it mattered after the loop work landed: a `for` over
/// a list allocates nothing now, and was still paying `LIVE_COUNT` twice an
/// element through `khora_alloc_reuse` and `khora_drop_reuse`.
pub(crate) static COUNTING: AtomicUsize = AtomicUsize::new(0);

/// Whether the counters are on.
#[inline(always)]
pub(crate) fn counting() -> bool {
    COUNTING.load(COUNTER_ORDER) != 0
}

/// Starts counting. Emitted into `main` for a program that names a counter.
///
/// The compiler can see whether a program declared one, so nothing has to be
/// asked for on a command line and no test had to change: a file with
/// `extern fn khora_live_count() -> Int;` in it gets counters and one without
/// pays nothing.
#[unsafe(no_mangle)]
pub extern "C" fn khora_enable_counters() {
    COUNTING.store(1, COUNTER_ORDER);
}

/// Total objects allocated since the process started or the counters were reset.
pub(crate) static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Objects allocated and not yet freed.
pub(crate) static LIVE_COUNT: AtomicUsize = AtomicUsize::new(0);

// `Relaxed` throughout: the counters publish no other memory, so nothing is
// ordered against them. A test that counts across threads establishes its
// happens-before by joining them, which is a far stronger edge than any
// ordering on the counter itself would give.
pub(crate) const COUNTER_ORDER: Ordering = Ordering::Relaxed;

/// Number of objects [`khora_alloc`] has produced since the last
/// [`khora_reset_counters`].
#[unsafe(no_mangle)]
pub extern "C" fn khora_alloc_count() -> usize {
    if !counting() {
        return NOT_COUNTING;
    }
    ALLOC_COUNT.load(COUNTER_ORDER)
}

/// Number of objects allocated and not yet freed.
///
/// This is the leak check the roadmap's phase 2 exit criterion is written
/// against: run a compiled program to completion and this must be zero.
#[unsafe(no_mangle)]
pub extern "C" fn khora_live_count() -> usize {
    if !counting() {
        return NOT_COUNTING;
    }
    LIVE_COUNT.load(COUNTER_ORDER)
}

/// What a counter answers when nothing was counting: `-1` read as an `Int`.
///
/// **Not zero.** Zero is what a leak check wants to see, and a check that
/// passes because nothing was measured is worse than one that fails. Reaching
/// this means a program read a counter without the compiler having seen it
/// declare one, which should not happen -- so it is loud.
const NOT_COUNTING: usize = usize::MAX;

/// A counter that goes up by one every time it is read, starting at 1.
///
/// A testing aid, beside the allocation counters and there for the same
/// reason: some behaviour is only visible over repetition, and a Khora program
/// has no way to remember how many times it has done something. Mutable state
/// is D11's, and a test should not have to wait for it.
#[unsafe(no_mangle)]
pub extern "C" fn khora_tick() -> i64 {
    static TICKS: AtomicUsize = AtomicUsize::new(0);
    TICKS.fetch_add(1, COUNTER_ORDER) as i64 + 1
}

/// Resets both counters to zero, for test isolation.
///
/// Call it when nothing is live. Resetting while objects are still allocated
/// leaves the live count describing a different population from the one that
/// will later be freed, and it will wrap when those frees arrive.
#[unsafe(no_mangle)]
pub extern "C" fn khora_reset_counters() {
    // Resetting is asking to count, so it is also switching them on: a host
    // calling the C API directly never went through the compiler's `main`.
    COUNTING.store(1, COUNTER_ORDER);
    ALLOC_COUNT.store(0, COUNTER_ORDER);
    LIVE_COUNT.store(0, COUNTER_ORDER);
}
