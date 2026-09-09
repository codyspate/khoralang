//! Cancellation, one flag per fiber.
//!
//! A cancellation is not an error, and it travels the same tagged return an
//! error does — see [`crate::CANCELLED_WHICH`]. What is here is the flag a
//! cancellation point reads and the stop that unwinds when it is set.

use super::*;
use crate::current::current;
use crate::region::khora_region_close_root;

/// Asks the running computation to stop.
///
/// It stops at the next *cancellation point*, which is a `!` in a function
/// that can raise — never between two statements that do not mention one. See
/// `docs/design/effect-runtime.md` §6 for why that is the promise worth making.
///
/// Idempotent: asking twice is asking once.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancel() {
    current(|fiber| fiber.cancel());
}

/// Whether a cancellation is pending *and may be acted on here*.
///
/// Read at every cancellation point, so it is on the hot path of any loop that
/// does fallible work. Two relaxed loads of a word, which is what it costs.
///
/// The second word is [`Shielded`]: a cancellation that arrives while a
/// finalizer is running is remembered rather than observed, so the finalizer
/// finishes and the unwind carries on afterwards.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancelled() -> u8 {
    u8::from(current(|fiber| fiber.is_cancelled() && !fiber.is_shielded()))
}

/// Holds a pending cancellation off for as long as it is alive.
///
/// **Cleanup cannot itself be cancelled.** A transaction rolled back on the
/// way out of a cancelled fiber has to send a `ROLLBACK` and read the reply,
/// and every `!` on that path is a cancellation point that would find the flag
/// still set — so without this, the rollback that cancellation is supposed to
/// cause would be interrupted by the same cancellation, one statement in. The
/// connection would go back to the pool inside an open transaction holding its
/// locks, which is the exact failure `std::db` exists to prevent.
///
/// So [`crate::region::khora_region_release`] wraps its finalizers in one.
/// This is the same answer Trio reached with `CancelScope(shield=True)` and Go
/// with `context.WithoutCancel`, arrived at from the same direction: the
/// alternative is cleanup that only runs when nothing went wrong, which is not
/// cleanup.
///
/// **The flag is not cleared**, only masked. When the last shield goes the
/// cancellation is observed at the next cancellation point and the unwind
/// continues from where it was — the finalizer got its turn, and nothing else
/// changed.
///
/// The price is honest and worth stating: a finalizer that hangs cannot be
/// interrupted. Everything with cancellation pays it, and the usual answer is
/// a deadline on the cleanup itself, which Khora does not have yet.
pub(crate) struct Shielded;

impl Shielded {
    pub(crate) fn new() -> Shielded {
        current(|fiber| fiber.shield());
        Shielded
    }
}

impl Drop for Shielded {
    fn drop(&mut self) {
        current(|fiber| fiber.unshield());
    }
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
/// stop that fiber.
///
/// **The comment here used to say that piece was unbuilt, and it is.** A
/// spawned thunk that can fail does return the tagged pair, and `design/fibers.md`
/// §2 is right that a cancellation stops that fiber: a fiber in a `loop`, a
/// fiber running a nursery with two live children, and a fiber that catches
/// every case in its row are all detached cleanly while running. What still
/// reaches here is a listener inside `Router::listen` with connections in
/// flight -- five rounds out of five -- so the remaining gap is somewhere in
/// the serving path rather than in fiber roots at large. Roadmap 16.8 has the
/// measurement and the three experiments that bound it.
///
/// # Safety
///
/// Must be called with no Khora frame relying on returning: it does not.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_cancel_stop() -> ! {
    if current(|fiber| fiber.is_spawned()) {
        fatal(
            "a cancellation reached a fiber's root, which cannot absorb one yet.\n\
             \n\
             The shape this is usually reached from is a server: cancelling or \
             detaching the fiber inside `Router::listen` while it is still \
             serving connections. Drain first and detach last -- stop accepting \
             work, wait for what is in flight, and let go of the listener after \
             that. An idle listener detaches cleanly.\n\
             \n\
             This is a gap rather than a rule: roadmap 16.8.",
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
    current(|fiber| fiber.uncancel());
}
