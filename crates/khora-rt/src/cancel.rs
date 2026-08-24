//! Cancellation, one flag per fiber.
//!
//! A cancellation is not an error, and it travels the same tagged return an
//! error does — see [`crate::CANCELLED_WHICH`]. What is here is the flag a
//! cancellation point reads and the stop that unwinds when it is set.

use super::*;
use crate::counters::COUNTER_ORDER;
use crate::region::khora_region_close_root;
use std::cell::RefCell;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

thread_local! {
    /// Whether *this* fiber has been asked to stop.
    ///
    /// One per fiber, which is what makes cancelling one not cancel the rest.
    /// The main thread is a fiber too — it gets the flag every thread gets —
    /// so nothing has to special-case the program's own computation.
    ///
    /// Shared rather than owned, because the handle a parent holds has to be
    /// able to set it from outside.
    pub(crate) static CANCELLED: RefCell<Arc<AtomicUsize>> =
        RefCell::new(Arc::new(AtomicUsize::new(0)));

    /// Whether this thread is a spawned fiber rather than the program itself.
    ///
    /// Only [`khora_cancel_stop`] asks, and only to tell a program that has
    /// nowhere left to unwind to from a *fiber* that has nowhere left to
    /// unwind to. The first is an outcome; the second is a hole.
    pub(crate) static ON_FIBER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The running fiber's cancellation flag.
fn cancel_flag() -> Arc<AtomicUsize> {
    CANCELLED.with(|c| c.borrow().clone())
}

/// Asks the running computation to stop.
///
/// It stops at the next *cancellation point*, which is a `!` in a function
/// that can raise — never between two statements that do not mention one. See
/// `docs/design/effect-runtime.md` §6 for why that is the promise worth making.
///
/// Idempotent: asking twice is asking once.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancel() {
    cancel_flag().store(1, COUNTER_ORDER);
}

/// Whether a cancellation is pending.
///
/// Read at every cancellation point, so it is on the hot path of any loop that
/// does fallible work. A relaxed load of a word, which is what it costs.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancelled() -> u8 {
    cancel_flag().load(COUNTER_ORDER) as u8
}

/// Stops a cancelled computation that has nowhere left to unwind to.
///
/// Reached when a cancellation arrives at a frame with no error channel — a
/// function that caught every error in its row, so its signature promises a
/// value it can no longer produce. There is no frame between there and the
/// root that could carry the cancellation.
///
/// On the program's own computation that is an *outcome*: the root region's
/// finalizers run and the process exits 130, which is what the entry point
/// would have done anyway.
///
/// On a spawned fiber it is a *hole*, and this says so rather than taking the
/// whole program down quietly. A fiber's root should absorb a cancellation and
/// stop that fiber — which needs the spawned thunk to return a tagged value,
/// so the runtime can see how it ended. `docs/design/fibers.md` calls this out
/// as the piece 5.3 has not built yet.
///
/// # Safety
///
/// Must be called with no Khora frame relying on returning: it does not.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_cancel_stop() -> ! {
    if ON_FIBER.with(|f| f.get()) {
        fatal(
            "a cancellation reached a fiber's root, which cannot absorb one yet; \
             see docs/design/fibers.md",
        );
    }
    // SAFETY: nothing returns past this, so no other frame observes the
    // released root.
    unsafe { khora_region_close_root() };
    let _ = std::io::stdout().flush();
    std::process::exit(130)
}

/// Clears a pending cancellation.
///
/// For tests, and for a supervisor that has finished unwinding one computation
/// and is about to start another.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancel_reset() {
    cancel_flag().store(0, COUNTER_ORDER);
}
