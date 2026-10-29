//! `timeout`: a deadline on the calling fiber, armed for the length of a
//! call.
//!
//! **What this is instead of: a child fiber per `timeout`.** Running the body
//! on a fiber of its own and canceling that fiber at the deadline is the easy
//! way, and it costs the body its caller: the answer has to be `Share`, the
//! body cannot reach the caller's regions or capabilities, and a `Map` built
//! inside it cannot come back. Here the body runs where it was called, and
//! the deadline cancels *this* fiber, which unwinds the body the way any
//! cancellation does: every `!`, back-edge and blocking call is where it stops,
//! and every finalizer between there and the `timeout` runs.
//!
//! # The protocol
//!
//! [`khora_timeout_begin`] opens a scope on the running fiber
//! ([`crate::current::Fiber::begin_deadline`]) and puts its deadline on the
//! one deadline list `Fiber::cancel_within` already uses. If the deadline
//! comes first, [`expire`] marks the scope fired and cancels the fiber
//! *without* the bit that says somebody asked ([`crate::current`]'s
//! `REQUESTED`), stops the children of nurseries opened inside the scope, and
//! wakes the fiber. [`khora_timeout_end`] closes the scope when the body
//! comes back, however it came back, takes the deadline off the list, and
//! answers whether this scope's deadline is what stopped it.
//!
//! **Only its own cancellation is taken back.** A cancel somebody asked for,
//! a force, or an enclosing scope's deadline is left set, and the body's
//! cancellation tag goes on past this `timeout` to whoever it belongs to. So
//! an outer deadline that fires inside an inner `timeout` surfaces as the
//! outer one's `None`, and a shutdown stays a shutdown.
//!
//! # What it does not reach
//!
//! Nothing a cancel does not reach: a foreign (C) call or a file-system call
//! already in progress finishes first, and a loop with no cancellation point
//! in it runs to its end.
//!
//! # In cleanup
//!
//! **A `timeout` opened in cleanup bounds its own body.** A finalizer, a
//! `claim` abandon handler and a `Shared::update` change function are held
//! off cancels by the shield or the pin, which would make a deadline opened
//! there inert -- a finalizer that bounded "close the connection" with a
//! `timeout` waited for it for ever. So a scope records the shield and pin
//! depth it was opened at, and its own deadline stops the fiber at exactly
//! that depth (`Fiber::stops_for_own_deadline`): its body unwinds to the
//! `timeout`, which answers `None`, and the rest of the cleanup runs.
//! Cleanup *inside* that body runs a shield deeper and is not cut short, and
//! a deadline opened outside the cleanup does not reach into it, so a
//! deadline still never cuts cleanup it did not open. What a deadline cannot
//! do is stop the cleanup around it; `Fiber::cancel_within` (a force) is
//! what bounds that.

use crate::current::{current, current_held, Fiber};
use crate::fiber::Deadlines;

/// Opens a `timeout` scope on the running fiber whose deadline is `millis`
/// from now, and answers its id for [`khora_timeout_end`].
///
/// **If the deadline thread cannot be started, the deadline fires at once.**
/// `Fiber::cancel_within` makes the same call for the same reason: the
/// operating system refused a thread, and stopping the body now is the bound
/// the caller asked for, reached early, where the other choice -- a panic out
/// of an `extern "C"` function -- ends the process. A deadline too far off
/// for the clock to name is never reached and is not recorded.
#[unsafe(no_mangle)]
pub extern "C" fn khora_timeout_begin(millis: i64) -> u64 {
    let fiber = current_held();
    let id = Deadlines::next_id();
    let wait = std::time::Duration::from_millis(millis.max(0) as u64);
    let when = std::time::Instant::now().checked_add(wait);
    // Open before it is on the list, so a deadline that comes due at once
    // finds the scope it is for.
    fiber.begin_deadline(id, when);
    if let Some(when) = when {
        if !Deadlines::add_scope(when, id, fiber.clone()) {
            expire(&fiber, id);
        }
    }
    id
}

/// Closes the `timeout` scope `id`, and answers 1 when its own deadline is
/// what canceled the body -- the cancellation has then been taken back, and
/// the `timeout` answers `None` -- and 0 otherwise.
///
/// Called on every way the body comes back: an answer, an error, or a
/// cancellation. A 1 alongside an answer or an error is a deadline that came
/// due after the body's last cancellation point; the answer or the error
/// stands, and the cancellation the deadline left is gone.
#[unsafe(no_mangle)]
// SHARE: `id` is a number, not a Khora value; nothing is stored.
pub extern "C" fn khora_timeout_end(id: u64) -> u8 {
    let ended = current(|fiber| fiber.end_deadline(id));
    if let Some(when) = ended.when {
        Deadlines::remove(when, id);
    }
    u8::from(ended.cut)
}

/// Fires scope `id`'s deadline on `fiber`, if the scope is still open.
///
/// **The flag, then the children, then the wake**, in the order
/// [`crate::fiber::deliver_to_fiber`] gives: a fiber parked on a child is
/// woken by that child stopping and then asks whether it was stopped itself,
/// so the flag has to be there first. The children are those of nurseries the
/// fiber opened inside this scope ([`crate::nursery::cancel_crews_in_scope`]):
/// a wait on a nursery is not a cancellation point, and a deadline that did
/// not reach the children would not end it.
pub(crate) fn expire(fiber: &Fiber, id: u64) {
    if !fiber.fire_deadline(id) {
        return;
    }
    crate::nursery::cancel_crews_in_scope(fiber.id(), id);
    crate::fiber::rouse(fiber);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::current::enter;
    use std::time::{Duration, Instant};

    /// Waits for the flag a deadline sets, or gives up after `patience`.
    fn until_canceled(fiber: &Fiber, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        while Instant::now() < deadline {
            if fiber.is_canceled() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }

    /// A body that answers in time leaves nothing behind: no cancellation,
    /// and no entry on the deadline list.
    #[test]
    fn a_scope_that_ends_first_takes_its_deadline_back() {
        std::thread::spawn(|| {
            let fiber = Fiber::spawned();
            let _entered = enter(fiber.clone());
            let before = Deadlines::len();
            let id = khora_timeout_begin(60_000);
            assert_eq!(Deadlines::len(), before + 1, "the deadline is on the list");
            assert_eq!(khora_timeout_end(id), 0, "nothing cut the body");
            assert!(!fiber.is_canceled());
            // Other tests' scopes come and go on the same list, so this one
            // asks for its own entry rather than for a count.
            assert!(Deadlines::len() <= before, "the entry went with the scope");
        })
        .join()
        .expect("the test thread");
    }

    /// The deadline cancels the fiber, and ending the scope takes exactly
    /// that cancellation back.
    #[test]
    fn a_deadline_that_fires_is_the_scopes_own_and_is_taken_back() {
        std::thread::spawn(|| {
            let fiber = Fiber::spawned();
            let _entered = enter(fiber.clone());
            let id = khora_timeout_begin(5);
            assert!(
                until_canceled(&fiber, Duration::from_secs(5)),
                "the deadline never fired"
            );
            assert_eq!(
                crate::cancel::khora_canceled(),
                1,
                "a cancellation point stops the body"
            );
            assert_eq!(khora_timeout_end(id), 1, "this scope's deadline cut it");
            assert!(!fiber.is_canceled(), "and its cancellation is gone");
        })
        .join()
        .expect("the test thread");
    }

    /// **A cancel somebody asked for is not a deadline's**, even when the
    /// deadline also fired. A `timeout` that took it back would turn a
    /// shutdown into a `None`.
    #[test]
    fn a_requested_cancel_survives_the_scope_ending() {
        std::thread::spawn(|| {
            let fiber = Fiber::spawned();
            let _entered = enter(fiber.clone());
            let id = khora_timeout_begin(5);
            assert!(until_canceled(&fiber, Duration::from_secs(5)));
            fiber.cancel();
            assert_eq!(
                khora_timeout_end(id),
                0,
                "the cancel was asked for, so it is not the deadline's"
            );
            assert!(fiber.is_canceled(), "and it stays");
        })
        .join()
        .expect("the test thread");
    }

    /// **An outer deadline is the outer scope's.** It fires while an inner
    /// scope is open, the inner one ends without taking it, and the outer one
    /// does.
    #[test]
    fn an_outer_deadline_inside_an_inner_scope_is_the_outers() {
        std::thread::spawn(|| {
            let fiber = Fiber::spawned();
            let _entered = enter(fiber.clone());
            let outer = khora_timeout_begin(5);
            let inner = khora_timeout_begin(60_000);
            assert!(until_canceled(&fiber, Duration::from_secs(5)));
            assert_eq!(khora_timeout_end(inner), 0, "the inner scope did not fire");
            assert!(fiber.is_canceled(), "so the cancellation goes on");
            assert_eq!(khora_timeout_end(outer), 1, "to the scope that did");
            assert!(!fiber.is_canceled());
        })
        .join()
        .expect("the test thread");
    }

    /// **Both deadlines fired, and the inner scope does not take the outer's
    /// cancellation as its own.** One flag records both, so an inner scope
    /// that cleared it on the way out would let the inner `timeout` answer
    /// `None` and the code after it run, past an outer deadline that had
    /// already come due.
    #[test]
    fn an_inner_scope_leaves_a_cancellation_an_outer_deadline_shares() {
        std::thread::spawn(|| {
            let fiber = Fiber::spawned();
            let _entered = enter(fiber.clone());
            let outer = khora_timeout_begin(60_000);
            let inner = khora_timeout_begin(60_000);
            expire(&fiber, inner);
            expire(&fiber, outer);
            assert_eq!(
                khora_timeout_end(inner),
                0,
                "the outer deadline shares the flag"
            );
            assert!(fiber.is_canceled(), "so it stays for the outer scope");
            assert_eq!(khora_timeout_end(outer), 1);
            assert!(!fiber.is_canceled());
        })
        .join()
        .expect("the test thread");
    }

    /// **A scope opened in cleanup stops its own body and nothing else.**
    /// Opened one shield deep, its deadline stops the fiber at that depth;
    /// the cleanup its body runs in turn, one shield deeper, is not cut
    /// short; and a scope opened outside the cleanup does not reach into it.
    /// Ending the scope answers that it cut the body and leaves the cancel
    /// the cleanup is running for.
    #[test]
    fn a_scope_opened_in_cleanup_stops_only_its_own_body() {
        std::thread::spawn(|| {
            let fiber = Fiber::spawned();
            let _entered = enter(fiber.clone());
            let outer = khora_timeout_begin(60_000);
            expire(&fiber, outer);
            fiber.shield();
            assert!(
                !fiber.stops_here(),
                "an outer scope's deadline does not reach into cleanup"
            );
            fiber.cancel();
            let inner = khora_timeout_begin(60_000);
            assert!(
                !fiber.stops_here(),
                "nor does the cancel the cleanup runs for"
            );
            expire(&fiber, inner);
            assert!(fiber.stops_here(), "the scope's own body stops");
            fiber.shield();
            assert!(
                !fiber.stops_here(),
                "cleanup inside that body is not cut short"
            );
            fiber.unshield();
            assert_eq!(khora_timeout_end(inner), 1, "the inner scope cut its body");
            assert!(fiber.is_canceled(), "and the cleanup's own cancel stays");
            assert!(!fiber.stops_here(), "held off by the shield again");
            fiber.unshield();
            assert_eq!(
                khora_timeout_end(outer),
                0,
                "a requested cancel is nobody's deadline"
            );
            assert!(fiber.stops_here());
        })
        .join()
        .expect("the test thread");
    }

    /// **An outer deadline that fired outside the cleanup survives a scope
    /// in the cleanup ending.** Both fired, at different depths, and nobody
    /// asked for a cancel: the inner scope cut its own body, and the
    /// cancellation that is left is the outer deadline's, for after the
    /// cleanup. Clearing it there let the code after the finalizer run on
    /// past an outer deadline that had already come due.
    #[test]
    fn an_outer_deadline_outlives_a_scope_in_cleanup() {
        std::thread::spawn(|| {
            let fiber = Fiber::spawned();
            let _entered = enter(fiber.clone());
            let outer = khora_timeout_begin(60_000);
            expire(&fiber, outer);
            fiber.shield();
            let inner = khora_timeout_begin(60_000);
            expire(&fiber, inner);
            assert_eq!(khora_timeout_end(inner), 1, "the inner scope cut its own body");
            assert!(fiber.is_canceled(), "and left the outer deadline's cancellation");
            fiber.unshield();
            assert_eq!(khora_timeout_end(outer), 1, "which the outer scope takes back");
            assert!(!fiber.is_canceled());
        })
        .join()
        .expect("the test thread");
    }

    /// A deadline that comes due after its scope has ended does nothing.
    #[test]
    fn a_deadline_for_a_closed_scope_does_nothing() {
        let fiber = Fiber::spawned();
        fiber.begin_deadline(7, None);
        let _ = fiber.end_deadline(7);
        expire(&fiber, 7);
        assert!(!fiber.is_canceled());
    }
}
