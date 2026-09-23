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

/// Whether the fiber running `main` was cut short on its way out.
///
/// **A nursery shutdown used to exit 0, so a supervisor was told it
/// succeeded.** When the root's body is a nursery the children are cancelled
/// and their finalizers run, but the nursery absorbs the cancellation and
/// returns normally, so control reaches the entry point's success path and
/// `main` hands back its own value. Nothing between there and `exit` had asked
/// whether the run was cut short, and a `restart: on-failure` policy reading
/// that status could not tell a signalled shutdown from a clean finish.
///
/// **`is_cancelled` rather than `has_absorbed`, because absorbing is not the
/// only way a cancellation arrives here.** A nursery that cancels its children
/// and collects them never absorbs anything itself: the flag it sets is on the
/// root, and the children return normally. Reading the absorbed flag alone
/// reported 0 for exactly the shape this exists for -- measured.
///
/// Shielding is deliberately not consulted. A finalizer holds a cancellation
/// off so it can finish; that says nothing about what the program's exit status
/// should be once it has.
#[unsafe(no_mangle)]
pub extern "C" fn khora_root_absorbed() -> u8 {
    u8::from(current(|fiber| fiber.is_cancelled() || fiber.has_absorbed()))
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

/// Ends the program the way a cancellation reaching the entry point does.
///
/// The root region's finalizers run and the process exits 130, which is what
/// the entry point would have done anyway. Shared by [`khora_cancel_absorb`]
/// and [`khora_cancel_stop`], because "there is no fiber to stop, so this is
/// the program's own outcome" is one answer and not two.
fn end_the_program() -> ! {
    // **It used to end here without saying anything**, and
    // `reference/traps.md` conceded as much: "the program ends at 130 having
    // printed nothing". A status alone is a poor way to learn this, because
    // 130 is also what a shell reports for Ctrl-C -- so the one reading it is
    // usually a person who did not press anything, or a supervisor that
    // believes somebody did. The status stays what the table promises; what
    // was missing is the sentence naming the call that got here and the two
    // that do not.
    let _ = writeln!(
        std::io::stderr(),
        "khora: a cancellation reached the entry point, so the program ends here \
         with status 130.\n\
         \n\
         A cancelled fiber has no answer, so `Fiber::join` on one unwinds the \
         joiner -- and `main` has nowhere to unwind to. `join_all` joins, so it \
         ends the same way.\n\
         \n\
         `Fiber::wait` waits for a fiber without asking for its answer, which is \
         what you want when the point was \"not before that finishes\". \
         `Fiber::detach` stops waiting altogether and asks the fiber to stop. \
         Neither ends the program.\n\
         \n\
         To keep an answer from a fiber that may be cancelled, have it publish \
         to a `Shared` cell and read the cell after `Fiber::cancel` and \
         `Fiber::detach`: a cell holds whatever the fiber managed before it \
         stopped, including partial progress. The answer arrives beside the \
         fiber rather than on its return type, and nothing types the relation \
         between the two, which is the cost of the shape that works today."
    );
    // SAFETY: nothing returns past this, so no other frame observes the
    // released root.
    unsafe { khora_region_close_root() };
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(130)
}

/// Absorbs a cancellation at the frame that has nowhere to send it.
///
/// **Reached only from a function `can_stop` pruned** -- one that, by the
/// analysis, reaches no cancellation point and so returns without a tag. Every
/// other infallible function hands a cancellation on through its tag, and
/// never calls this. What follows describes the older rule, under which every
/// infallible frame could get here; it is kept until the absorbing path is
/// removed, because a pruned frame that the analysis got wrong still lands
/// here.
///
/// # The frame this is called from
///
/// A cancellation travels the tagged return an error travels, and a function
/// whose `raises` row is empty does not have one. So a *total* `catch` — one
/// naming every case in the row it catches, or a `_` arm — inside such a
/// function receives a cancellation it can neither handle nor pass on:
/// `catch` is forbidden from swallowing one, because no arm can name it
/// (`docs/design/effect-runtime.md` §6), and the frame has no channel to
/// propagate it out. `Router::serve_connection` is written exactly that way —
/// "what a fiber runs, and it does not fail", with a `_` arm — and so is every
/// `Fiber::spawn(fn () => .. catch { .. })`.
///
/// Generated code releases everything that frame owns first, calls this, and
/// then returns a zero of its own return type. **The code generator only emits
/// the call where a zero is a value rather than a hole**: `()` and the
/// scalars. `khora-codegen-llvm/src/lower/failure.rs`, `leave_with`, is the
/// other half of that rule, and [`khora_cancel_stop`] is what it emits instead
/// when the return type is a pointer.
///
/// # What it does
///
/// On the program's own computation, [`end_the_program`]. There is no fiber to
/// stop, the outcome is the entry point's, and the zero the caller was about
/// to hand back is never produced because nothing returns.
///
/// On a spawned fiber it **returns**, having recorded that this fiber gave up.
/// [`crate::fiber::khora_fiber_spawn`] reads that record when the thunk comes
/// back and stores a cancellation as the fiber's answer in place of the word
/// the thunk handed over. So the handle reports `cancelled` rather than a
/// value, a `join` on it unwinds its joiner the way a join on any cancelled
/// fiber does, a `wait` returns, and **the process keeps running** — which is
/// `docs/design/fibers.md` §2's promise that a cancellation reaching a fiber's
/// root stops that fiber and not the program.
///
/// # What is honest about it, and what is not
///
/// **The flag is not cleared.** It stays the state of record, so the next
/// cancellation point this fiber reaches — in a *fallible* frame, the only
/// kind that has one — observes it and unwinds again. §6 calls that "delayed
/// to the next mark that can carry it, never lost", and it is the same
/// sentence here.
///
/// **The tail of an infallible caller still runs.** This returns into a frame
/// whose own caller may be infallible too, and nothing between there and the
/// fiber's root has a cancellation point to stop at. The cost is real and it
/// is bounded: infallible code, with no `!` in it, ending at the fiber's root.
/// It is also strictly less than what used to happen, which was that the same
/// code ran and *then* the process aborted.
///
/// **A scalar zero is a fabricated answer.** Where the absorbing frame is the
/// fiber's root thunk — the common shape, and the only one `Fibers::adopt` can
/// hold, since it fixes a child's answer at `()` — nothing ever reads it,
/// because the record above replaces it. Where it is an inner frame, an
/// infallible caller may compute with a zero it did not earn. A pointer is
/// where that stops being a fabricated answer and becomes no answer at all,
/// which is the line [`khora_cancel_stop`] is on the other side of.
#[unsafe(no_mangle)]
pub extern "C" fn khora_cancel_absorb() {
    if !current(|fiber| fiber.is_spawned()) {
        end_the_program();
    }
    current(|fiber| fiber.absorb());
}

/// Stops a cancelled computation that cannot even hand back a zero.
///
/// Everything [`khora_cancel_absorb`] says holds up to its last paragraph:
/// this is the same frame, with a *pointer* return type. A Khora pointer is a
/// reference-counted object, `0` is not one, and an infallible caller is
/// entitled to read through whatever it is given. So there is no value to
/// produce here and no frame to produce it for.
///
/// On the program's own computation this is the ordinary outcome and the
/// status is 130. On a spawned fiber it is the one shape left that takes the
/// process down, and the message names *that* shape.
///
/// # What the comment here used to say, and why it was wrong
///
/// It claimed the remaining gap was "somewhere in the serving path", named a
/// listener inside `Router::listen` with connections in flight, and asserted
/// that "a fiber in a `loop`, a fiber running a nursery with two live
/// children, and a fiber that catches every case in its row are all detached
/// cleanly". Two of those three were false, and the third was true for a
/// reason that had nothing to do with servers:
///
///   - **A fiber that catches every case in its row aborted**, every run. It
///     is the shape above, and it needs no socket:
///     `a_fiber_that_catches_every_case_is_not_a_hole` in
///     `khora-codegen-llvm/tests/fibers.rs` is a dozen lines of it with no
///     `std::net` anywhere in the program. The reason it looked like a serving
///     bug is that `Router::serve_connection` is written that way -- a `_` arm
///     in a function with no `raises` row -- so a server is where it was met.
///     `a_handler_that_cancels_itself_stops_its_connection_and_not_the_server`
///     in `tests/net_cancel.rs` is that meeting, with a real listener and a
///     real connection.
///   - **A fiber running a nursery with live children hangs**, and always did.
///     Cancelling it does not reach the children, and the nursery's wait is
///     not a cancellation point, so the parent sits in `khora_fibers_wait` for
///     ever. That is a gap in cancellation *transitivity* rather than in fiber
///     roots — `docs/design/fibers.md` promises "cancelling a nursery cancels
///     its children, transitively" and the runtime does not do it — and it is
///     untouched by anything here.
///   - **A fiber in a `loop` does detach cleanly**, because its thunk can
///     raise and so its root already carried the cancellation. That was never
///     the shape in question.
///
/// A fiber that joins a child it cancelled reaches this frame too, with a
/// cancellation `Fiber::join` manufactured rather than one the fiber was
/// asked for. It absorbs now, like the rest.
///
/// # Safety
///
/// Must be called with no Khora frame relying on returning: it does not.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_cancel_stop() -> ! {
    if current(|fiber| fiber.is_spawned()) {
        fatal(
            "a cancellation reached a frame that cannot carry one and cannot \
             hand back a value either.\n\
             \n\
             The shape is a function that catches every case in the row of \
             something it calls -- `f()! catch { .. }` with an arm for each \
             case, or a `_` arm -- and whose own return type is a boxed value. \
             A cancellation is in no row, so no arm names it and there is no \
             channel left to send it on; and unlike a `()`, an `Int` or a \
             `Float`, a boxed answer has no zero that is a value rather than a \
             null.\n\
             \n\
             **The return type is what decides, not the shape of the arms.** \
             The same total `catch` in a function returning `Int` absorbs the \
             cancellation and returns zero; in one returning a `String` or a \
             record it arrives here.\n\
             \n\
             Give that function a `raises` row, so the cancellation has a way \
             out of it -- or move the total `catch` into a caller that has \
             one. Inside a `Fiber::spawn` thunk, the thunk itself carries a \
             row: `Fiber::spawn(fn () => risky()!)` and a `Fiber::join(h)!` at \
             the other end is the shape that works.",
        );
    }
    end_the_program()
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
    use crate::current::{enter, Fiber};
    use crate::fiber::{khora_fiber_join, khora_fiber_release, CANCELLED_WHICH};
    use crate::heap::khora_alloc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// How many times [`stopping_thunk`] reached its end.
    static RAN: AtomicUsize = AtomicUsize::new(0);

    /// What generated code does at a total `catch` in a frame with no channel:
    /// release the frame, absorb, and hand back a zero of its own return type.
    ///
    /// Written out here because this crate has no compiler, and the point of
    /// the test is the runtime's half of that bargain.
    extern "C" fn stopping_thunk(_code: *const u8, _body: *mut u8) -> u64 {
        khora_cancel_absorb();
        RAN.fetch_add(1, Ordering::SeqCst);
        0
    }

    /// The same, without absorbing anything: a fiber that simply answered.
    extern "C" fn plain_thunk(_code: *const u8, _body: *mut u8) -> u64 {
        7
    }

    /// A fiber that cancels itself and then finishes normally, which an
    /// infallible thunk does: it has nowhere to stop.
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

    /// Absorbing records that this fiber gave up, and leaves the flag alone.
    ///
    /// **The flag is the state of record**, before and after. Clearing it here
    /// would mean a fallible frame further out -- one that *does* have a
    /// channel -- reached its next `!` and carried on as though nothing had
    /// been asked of it.
    #[test]
    fn absorbing_records_the_fiber_gave_up_without_clearing_the_flag() {
        let fiber = Fiber::spawned();
        let _entered = enter(fiber.clone());
        assert!(!fiber.has_absorbed(), "nothing has given up yet");

        khora_cancel();
        assert_eq!(khora_cancelled(), 1);
        khora_cancel_absorb();

        assert!(fiber.has_absorbed(), "the fiber records that a frame gave up");
        assert_eq!(khora_cancelled(), 1, "and the flag is still the state of record");
    }

    /// It is idempotent, because a fiber with two total `catch`es on its way
    /// out reaches it twice and the second says nothing new.
    #[test]
    fn absorbing_twice_is_absorbing_once() {
        let fiber = Fiber::spawned();
        let _entered = enter(fiber.clone());
        khora_cancel_absorb();
        khora_cancel_absorb();
        assert!(fiber.has_absorbed());
    }

    /// The end of it: a fiber whose thunk absorbed a cancellation answers
    /// `cancelled` rather than the zero its infallible signature made it hand
    /// back, and the process is still here to ask.
    ///
    /// **This is what `khora_cancel_stop` used to abort on**, on any thunk
    /// with a total `catch` in it. The tag is what makes a joiner able to tell
    /// "it was stopped" from "it answered nought".
    #[test]
    fn a_thunk_that_absorbed_answers_cancelled_rather_than_a_value() {
        RAN.store(0, Ordering::SeqCst);
        // SAFETY: a live closure whose drop is the default, an infallible
        // trampoline matching `plain`, and an answer that is not a pointer --
        // which is what `boxed: false` says.
        let handle = unsafe {
            crate::fiber::khora_fiber_spawn(
                closure(),
                None,
                None,
                Some(stopping_thunk),
                false,
                None,
            )
        };

        let mut answer: u64 = 0;
        // SAFETY: a live handle from the spawn above, and a writable word.
        let which = unsafe { khora_fiber_join(handle, &raw mut answer) };
        assert_eq!(which, CANCELLED_WHICH, "the fiber was stopped, not answered");
        assert_eq!(answer, 0, "and a cancellation carries no payload");
        assert_eq!(RAN.load(Ordering::SeqCst), 1, "the thunk did return");

        // SAFETY: the last reference to the handle.
        unsafe { khora_fiber_release(handle) };
    }

    /// And a fiber that absorbed nothing still answers what it computed, which
    /// is the half that would break silently if the record were read wrong.
    #[test]
    fn a_thunk_that_absorbed_nothing_answers_what_it_computed() {
        // SAFETY: as above.
        let handle = unsafe {
            crate::fiber::khora_fiber_spawn(closure(), None, None, Some(plain_thunk), false, None)
        };
        let mut answer: u64 = 0;
        // SAFETY: as above.
        let which = unsafe { khora_fiber_join(handle, &raw mut answer) };
        assert_eq!(which, 0);
        assert_eq!(answer, 7);
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
            crate::fiber::khora_fiber_spawn(closure(), None, None, Some(cancelling_thunk), false, None)
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
