//! Cancellation, one flag per fiber.
//!
//! A cancellation is not an error, and it travels the same tagged return an
//! error does — see [`crate::CANCELLED_WHICH`]. Every function that can reach
//! a cancellation point returns a tag, whatever its `raises` row, so there is
//! no frame a cancellation can arrive at and have nowhere to go. What is here
//! is the flag a cancellation point reads, and the two masks on it.

use crate::current::current;

/// Asks the running computation to stop.
///
/// It stops at the next *cancellation point*: a loop back-edge, a call to a
/// function that can reach one, or a blocking operation -- never between two
/// statements that are none of those. See `docs/design/effect-runtime.md` §6
/// for the list, and for why that is the promise worth making.
///
/// Idempotent: asking twice is asking once.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancel() {
    current(|fiber| fiber.cancel());
}

/// Whether a cancellation is pending *and may be acted on here*.
///
/// Asked at every cancellation point, but only after generated code has loaded
/// [`crate::poll::khora_poll`] and found some fiber in the process cancelled:
/// a call here per loop trip was what made a loop in a function with a row
/// ten times slower than the same loop without one. When it is asked, it is
/// two relaxed loads of a word.
///
/// The second word is [`Shielded`]: a cancellation that arrives while a
/// finalizer is running is remembered rather than observed, so the finalizer
/// finishes and the unwind carries on afterwards.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancelled() -> u8 {
    u8::from(current(|fiber| fiber.stops_here()))
}

/// Whether the fiber running `main` was cancelled, asked once `main` has
/// returned normally.
///
/// **What this prevents: a signalled shutdown exiting 0, so a supervisor is
/// told the program succeeded.** A cancellation that arrives after `main`'s
/// last cancellation point -- during a nursery's final collection of its
/// children, say -- lets `main` return its own value, and nothing between
/// there and `exit` would otherwise ask whether the run was cut short. A
/// `restart: on-failure` policy reading that status could not tell a
/// signalled shutdown from a clean finish.
///
/// The name is older than the design: nothing absorbs a cancellation any more.
/// It is the root's cancel flag and nothing else.
///
/// Shielding is deliberately not consulted. A finalizer holds a cancellation
/// off so it can finish; that says nothing about what the program's exit status
/// should be once it has.
#[unsafe(no_mangle)]
pub extern "C" fn khora_root_absorbed() -> u8 {
    u8::from(current(|fiber| fiber.is_cancelled()))
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
/// The price, and the bound on it: a finalizer that hangs is not interrupted by
/// a cancel, however many are sent. `Fiber::abort` interrupts it -- a forced
/// fiber is not held by this -- and `Fiber::cancel_within` asks for that after
/// a deadline the caller chooses. What aborting costs is cleanup cut off
/// part-way, which is why it is never the default.
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

/// Holds this fiber inside a `Shared::update` or `modify` change function,
/// for as long as it is alive.
///
/// **What this prevents: a cell whose lock is never let go of.** A change
/// function runs under the cell's lock, in a Rust frame, so nothing inside it
/// stops at a cancellation point: no cancellation point acts, not even for a
/// forced fiber.
///
/// **And nothing inside one waits for ever on a cancelled fiber.** A blocking
/// call there -- a `receive`, a sleep, a socket -- gives up and hands back its
/// "gave up" answer as soon as the fiber is cancelled, shielded or not
/// ([`crate::current::Fiber::gives_up_waiting`]). The change function carries
/// on with that answer and returns, the lock is let go, and the fiber stops at
/// its next cancellation point.
///
/// **A wait with no "gave up" answer still leaves.** `Fiber::join`, `wait` and
/// `outcome` inside a change function come back cancelled when they give up,
/// or when the child was stopped by somebody else, and the change function
/// leaves on that tag. The shim hands the tag back instead of an answer, and
/// [`crate::shared::khora_shared_update`] leaves the cell holding what it held:
/// the change did not happen, and no zero nobody computed is stored. The lock
/// is let go and the caller leaves on the tag like after any cancelled call.
///
/// **Those three do not give up on a plain cancel in cleanup.** Leaving on the
/// tag would skip the rest of a shielded finalizer, and only `abort` may do
/// that: [`crate::current::Fiber::gives_up_joining`]. So in a finalizer they
/// wait for the child with the lock held, and `abort` is what ends the wait.
///
/// What it costs is stated rather than hidden: a change function that loops
/// without blocking runs to its end after a cancel, and after a force. One
/// that loops for ever can only be ended by the process ending. The lock is
/// the reason, and a change function is meant to be short.
pub(crate) struct Pinned;

impl Pinned {
    pub(crate) fn new() -> Pinned {
        current(|fiber| fiber.pin());
        Pinned
    }
}

impl Drop for Pinned {
    fn drop(&mut self) {
        current(|fiber| fiber.unpin());
    }
}

/// Clears a pending cancellation.
///
/// For tests, and for a supervisor that has finished unwinding one computation
/// and is about to start another.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancel_reset() {
    current(|fiber| fiber.uncancel());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KHORA_FIELD_OFFSET;
    use crate::fiber::{
        khora_fiber_cancelled, khora_fiber_join, khora_fiber_outcome, khora_fiber_release,
        CANCELLED_WHICH, STOPPED_WHICH,
    };
    use crate::heap::khora_alloc;

    /// What generated code hands back from a thunk that was stopped
    /// part-way, whatever its `raises` row: the cancellation tag and no
    /// payload. Written out here because this crate has no compiler.
    extern "C" fn stopped_thunk(_code: *const u8, _body: *mut u8, out: *mut u64) -> u32 {
        // SAFETY: the runtime passes a writable word.
        unsafe { out.write(0) };
        CANCELLED_WHICH
    }

    /// A fiber that simply answered.
    extern "C" fn plain_thunk(_code: *const u8, _body: *mut u8) -> u64 {
        7
    }

    /// A fiber that cancels itself and then finishes normally: it reaches no
    /// cancellation point after the cancel, so nothing stops it.
    extern "C" fn cancelling_thunk(_code: *const u8, _body: *mut u8) -> u64 {
        khora_cancel();
        0
    }

    /// A closure object of type `() -> A`: one field, the code pointer. The
    /// trampolines above ignore it, but `khora_fiber_spawn` reads it before
    /// calling and would fault on a null.
    fn closure() -> *mut u8 {
        let object = khora_alloc(std::mem::size_of::<*const u8>() as u64, 0);
        // SAFETY: one field's worth of freshly allocated space, and nothing
        // else holds the pointer yet.
        unsafe {
            object.add(KHORA_FIELD_OFFSET).cast::<*const u8>().write(std::ptr::null());
        }
        object
    }

    /// A thunk that hands back the cancellation tag is reported stopped by
    /// every question asked of its handle -- `join`, `cancelled` and
    /// `outcome` -- and the three agree.
    ///
    /// **What this pins: the stored tag is the only record of a stop.** The
    /// runtime used to keep a second one on the fiber, for frames that had no
    /// tag to hand a cancellation back on; `cancelled` and `outcome` read
    /// that one first. Every thunk has the tag now, and a `cancelled` that
    /// stopped reading the stored answer would say a stopped fiber finished.
    #[test]
    fn a_thunk_that_hands_back_the_cancellation_tag_is_reported_stopped() {
        // SAFETY: a live closure whose drop is the default, and a tagged
        // trampoline matching `call`, with an answer that is not a pointer.
        let handle = unsafe {
            crate::fiber::khora_fiber_spawn(closure(), None, Some(stopped_thunk), None, false, None, None)
        };

        let mut answer: u64 = 0;
        // SAFETY: a live handle from the spawn above, and a writable word.
        let which = unsafe { khora_fiber_join(handle, &raw mut answer) };
        assert_eq!(which, CANCELLED_WHICH, "the fiber was stopped, not answered");
        assert_eq!(answer, 0, "and a cancellation carries no payload");
        // SAFETY: the same live handle, joined.
        assert!(unsafe { khora_fiber_cancelled(handle) }, "`cancelled` says it was stopped");
        // SAFETY: the same live handle, and a writable word.
        let outcome = unsafe { khora_fiber_outcome(handle, &raw mut answer) };
        assert_eq!(outcome, STOPPED_WHICH, "`outcome` says it was stopped");

        // SAFETY: the last reference to the handle.
        unsafe { khora_fiber_release(handle) };
    }

    /// And a fiber that finished answers what it computed, **even when it
    /// was cancelled on the way**, if it reached no cancellation point after
    /// the cancel. A stop is what the thunk handed back, not what was asked.
    #[test]
    fn a_thunk_that_finished_answers_what_it_computed() {
        // SAFETY: as above, with a plain trampoline matching `plain`.
        let handle = unsafe {
            crate::fiber::khora_fiber_spawn(closure(), None, None, Some(plain_thunk), false, None, None)
        };
        let mut answer: u64 = 0;
        // SAFETY: as above.
        let which = unsafe { khora_fiber_join(handle, &raw mut answer) };
        assert_eq!(which, 0);
        assert_eq!(answer, 7);
        // SAFETY: the last reference to the handle.
        unsafe { khora_fiber_release(handle) };

        // SAFETY: as above.
        let handle = unsafe {
            crate::fiber::khora_fiber_spawn(closure(), None, None, Some(cancelling_thunk), false, None, None)
        };
        // SAFETY: as above.
        let which = unsafe { khora_fiber_join(handle, &raw mut answer) };
        assert_eq!(which, 0, "it finished");
        // SAFETY: the same live handle, joined.
        assert!(!unsafe { khora_fiber_cancelled(handle) }, "and was not stopped");
        // SAFETY: the last reference to the handle.
        unsafe { khora_fiber_release(handle) };
    }

    /// **A fiber that was cancelled and has finished is no longer counted**,
    /// while its handle is still held.
    ///
    /// The handle keeps the fiber alive, so leaving the uncounting to its
    /// `Drop` would mean one long-held handle to a cancelled fiber -- a
    /// server's listener, stopped and kept -- sends every back-edge in the
    /// process down the slow path until the process ends.
    #[test]
    fn a_cancelled_fiber_that_finished_is_uncounted_while_its_handle_is_held() {
        // SAFETY: as above.
        let handle = unsafe {
            crate::fiber::khora_fiber_spawn(closure(), None, None, Some(cancelling_thunk), false, None, None)
        };
        let mut answer: u64 = 0;
        // SAFETY: as above.
        unsafe { khora_fiber_join(handle, &raw mut answer) };
        // SAFETY: a live handle, joined.
        let state = unsafe { crate::fiber::fiber_state(handle) }.expect("a live handle");
        assert!(state.fiber.is_cancelled(), "the thunk did cancel itself");
        assert!(!state.fiber.is_counted(), "a finished fiber is still counted");
        // SAFETY: the last reference to the handle.
        unsafe { khora_fiber_release(handle) };
    }
}
