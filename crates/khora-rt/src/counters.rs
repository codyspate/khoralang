//! What the runtime counts, for tests to read.
//!
//! Allocations, live objects, and a tick that goes up every time it is read.
//! None of it is load-bearing: `khora_live_count` returning to zero is how
//! every leak test in the repository states its claim, and
//! `docs/design/compatibility.md` says allocation behaviour is not part of the
//! language's promise — so these are the compiler's own instrument rather than
//! a contract with anybody.

use std::sync::atomic::{AtomicUsize, Ordering};

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
    ALLOC_COUNT.load(COUNTER_ORDER)
}

/// Number of objects allocated and not yet freed.
///
/// This is the leak check the roadmap's phase 2 exit criterion is written
/// against: run a compiled program to completion and this must be zero.
#[unsafe(no_mangle)]
pub extern "C" fn khora_live_count() -> usize {
    LIVE_COUNT.load(COUNTER_ORDER)
}

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
    ALLOC_COUNT.store(0, COUNTER_ORDER);
    LIVE_COUNT.store(0, COUNTER_ORDER);
}
