//! The one word every loop back-edge reads.
//!
//! **What this prevents: a loop paying a function call per trip to learn that
//! nothing has happened.** A back-edge asks two questions -- has this fiber
//! been cancelled, and should it give its worker back -- and both used to be
//! out-of-line calls: `khora_cancelled` through the `#[inline(never)]` read in
//! `crate::current`, and `khora_safepoint` through a thread-local. On a loop
//! whose body is a few instructions, the calls *were* the loop: 10x in a
//! function with a `raises` row, and as much again in any program that can
//! spawn, on the thread backend where the safepoint has nothing to do.
//!
//! Almost always both answers are "no", and almost always that is true of the
//! whole process: no fiber anywhere is cancelled, and no scheduler is running.
//! So generated code reads this word with one relaxed load and branches past
//! both questions when it is zero, and asks them properly only when it is not.
//!
//! # The two halves
//!
//! - **Low 32 bits: cancelled fibers still running.** A fiber is counted on its
//!   first cancellation and uncounted when it finishes or is uncancelled --
//!   `crate::current::Fiber` keeps the per-fiber half of that. **A count
//!   rather than a flag that stays set**: a flag set on the first cancel would
//!   send every back-edge in a long-running server down the slow path for the
//!   rest of its life after one request timed out.
//! - **High 32 bits: scheduler pools alive.** The safepoint budget is only ever
//!   granted by a worker around a resume, so with no pool there is no budget
//!   anywhere and `khora_safepoint` is exactly a no-op. On the thread backend,
//!   which is the default, that is always.
//!
//! # What it costs, and what it does not buy
//!
//! Every back-edge pays a load and a branch, and a `!` in a function with a
//! row pays the same, where they paid a call. While *any* fiber in the process
//! is cancelled and has not finished, every back-edge in every fiber takes the
//! slow path: the load, a branch taken, and one call to `khora_back_edge` (or
//! to whichever of the two questions the back-edge asks). Measured on a
//! short loop, that is about what the two unconditional calls cost before the
//! word existed -- 5% under it, in the one program and machine measured -- and
//! not less: the slow path buys nothing, it only has to not lose. Two calls
//! behind the branch lost, by half again, which is why there is one.
//!
//! **On the scheduler backend the safepoint is still a call on every
//! back-edge.** The pool half is non-zero for as long as a pool exists, so the
//! word says nothing a back-edge could skip on. Making the budget itself
//! inline needs a per-worker counter generated code can reach, which is a
//! thread-local, and LLVM treats a thread-local's address as fixed for the
//! length of a function: it hoists it out of the loop, and a fiber that
//! migrates at the safepoint goes on decrementing the budget of the worker it
//! left. That is the bug `crate::current::running` is `#[inline(never)]` to
//! prevent, and it is not repeated here.

use std::sync::atomic::{AtomicU64, Ordering};

/// Cancelled fibers still running (low half), and live scheduler pools (high
/// half). Zero means a back-edge has nothing to ask. See the module.
///
/// Exported under this name because generated code loads it directly.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static khora_poll: AtomicU64 = AtomicU64::new(0);

/// The half of [`khora_poll`] that counts cancelled fibers. Generated code
/// masks with the same constant at a `!`, where only a cancellation matters.
pub const POLL_CANCELLED: u64 = 0xffff_ffff;

/// One live scheduler pool, in [`khora_poll`]'s high half.
const POLL_POOL: u64 = 1 << 32;

/// A fiber has become cancelled and has not finished.
pub(crate) fn count_cancelled(word: &AtomicU64) {
    word.fetch_add(1, Ordering::Relaxed);
}

/// A counted fiber finished, or was uncancelled.
///
/// **Must follow the matching [`count_cancelled`] in the word's modification
/// order**, or the low half borrows from the high one and reads as a pool that
/// does not exist. `crate::current::Fiber` gets that from the release/acquire
/// pair on its own state word; this checks it where it is cheap to.
pub(crate) fn uncount_cancelled(word: &AtomicU64) {
    let before = word.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(
        before & POLL_CANCELLED != 0,
        "a cancelled fiber was uncounted more often than it was counted"
    );
}

/// A scheduler pool is about to start its workers.
///
/// Before them, so that no worker can grant a budget while back-edges are
/// still skipping the safepoint.
pub(crate) fn pool_started() {
    khora_poll.fetch_add(POLL_POOL, Ordering::Relaxed);
}

/// A scheduler pool has joined its workers, so none of them holds a budget.
pub(crate) fn pool_stopped() {
    khora_poll.fetch_sub(POLL_POOL, Ordering::Relaxed);
}

/// The slow path of a back-edge that asks both questions: gives the worker
/// back if the budget is spent, then answers whether a cancellation should be
/// acted on here, 1 or 0.
///
/// **One call where there were two, and that is the whole reason it exists.**
/// Generated code reaches this only when [`khora_poll`] is non-zero, which on
/// the scheduler backend is always. Two out-of-line calls on that path, plus
/// the branch around them, measured slower than the two unconditional calls
/// the back-edge made before the poll existed; one call brings it back under.
///
/// The order is the one the back-edge always had: the safepoint first, so a
/// cancellation that arrives while the worker is away is seen on the way back.
/// The fiber may come back on another worker, which is why the cancellation
/// is read through [`crate::current::current`] after the switch rather than
/// before it.
#[unsafe(no_mangle)]
pub extern "C" fn khora_back_edge() -> u8 {
    crate::scheduler::khora_safepoint();
    crate::cancel::khora_cancelled()
}
