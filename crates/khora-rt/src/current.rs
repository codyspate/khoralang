//! Which fiber is running.
//!
//! Everything a fiber owns rather than borrows from whatever thread happens to
//! be carrying it: its identity, whether it has been asked to stop, and whether
//! it is a spawned fiber or the program's own computation.
//!
//! # Why this exists before there is a scheduler
//!
//! Today a fiber *is* an operating-system thread, so a thread-local is a
//! perfectly good place to keep per-fiber state, and that is where all of it
//! was. Phase 11 makes fiber 42 start on one worker and resume on another, at
//! which point every one of those thread-locals is answering a question about
//! the wrong thing.
//!
//! One of them was already a latent bug rather than a future one.
//! [`crate::shared`] keeps a per-fiber id so that `Shared::update` can refuse
//! re-entry: a change function runs under the cell's lock, so reaching the same
//! cell from inside one would wait for itself, and the runtime says so instead
//! of hanging. With the id in thread-local storage that check fails in both
//! directions under M:N, and the worse direction is the false one — a fiber
//! scheduled onto a worker whose previous occupant holds the lock reads the
//! same id, matches the recorded holder, and is killed for a re-entry it never
//! performed. A correct program aborts, depending on timing.
//!
//! So the fix lands now, on its own, while it is a refactor with tests either
//! side of it rather than one strand of a scheduler.
//!
//! # The shape
//!
//! A raw pointer in thread-local storage, updated whenever the running fiber
//! changes — which today is once, when a thread starts, and after 11A will be
//! at every context switch. The `Arc` keeping the fiber alive is held by
//! [`Entered`] and by whoever can cancel it.
//!
//! A pointer rather than a cloned `Arc`, because [`crate::cancel::khora_cancelled`]
//! runs at every cancellation point. It used to clone an `Arc` to read one
//! word; now it loads a pointer.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::counters::COUNTER_ORDER;

/// Fiber ids, handed out on first use and never reused.
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The span a fiber is inside, as `std::trace::Context` holds it.
///
/// All zeroes means the fiber is inside no span, because a span id of zero is
/// already how `Span::parent` says "this is a root". [`crate::span`] has the
/// argument.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct SpanContext {
    pub(crate) trace_high: i64,
    pub(crate) trace_low: i64,
    pub(crate) span: i64,
    pub(crate) sampled: bool,
}

/// What belongs to a fiber rather than to a thread.
pub(crate) struct Fiber {
    /// Only ever compared for equality. Never zero, so zero can mean "nobody".
    id: usize,
    /// Whether this fiber has been asked to stop, and whether it has finished:
    /// [`CANCELLED`] and [`RETIRED`], two bits of one word.
    ///
    /// Shared rather than owned, because a parent holding the handle sets it
    /// from outside.
    ///
    /// **One word, so that the flag and [`crate::poll`]'s count cannot
    /// disagree.** The fiber is counted exactly while it is cancelled and not
    /// retired, and every change to either bit is one compare-and-swap, so the
    /// thread whose CAS moves the fiber into or out of that state is the one
    /// that adjusts the count. With the flag and the count in two words,
    /// `uncancel` could clear the flag, a racing `cancel` set it again and see
    /// the fiber still counted, and `uncancel` then uncount it -- a fiber left
    /// cancelled and uncounted, whose loops with no call in them never look.
    ///
    /// Retired is kept separately from cancelled because a fiber cancelled
    /// *after* it finished -- a handle released or detached late, the ordinary
    /// case -- must still read as cancelled but must not be counted, or nothing
    /// would ever uncount it and every loop in the process would take the slow
    /// path from then on.
    state: AtomicU8,
    /// The word this fiber is counted in. [`crate::poll::khora_poll`] except
    /// in tests, which need a word no other test is changing under them.
    poll: &'static AtomicU64,
    /// How deep this fiber is inside a region's finalizers.
    ///
    /// A count rather than a flag, because a finalizer that releases a region
    /// of its own is an ordinary thing to do. Zero means a cancellation point
    /// answers honestly; anything else means one is *running cleanup* and must
    /// be allowed to finish. [`crate::cancel::Shielded`] is where the reason
    /// lives.
    ///
    /// On the fiber rather than on the thread, because a finalizer may park —
    /// and the worker that comes back to it may not be the one that left.
    shielded: AtomicUsize,
    /// How deep this fiber is inside a `Shared::update` or `modify` change
    /// function. [`crate::cancel::Pinned`] says why.
    pinned: AtomicUsize,
    /// Set while a worker is inside this fiber's `resume`. See
    /// `crate::coro::ResumedOnce`; debug builds, and a release build asked for
    /// `fiber-audit`.
    #[cfg(any(debug_assertions, feature = "fiber-audit"))]
    pub(crate) resuming: std::sync::atomic::AtomicBool,
    /// Whether this is a spawned fiber rather than the program's own
    /// computation.
    ///
    /// Asked by [`crate::cancel::khora_cancel_absorb`] and
    /// [`crate::cancel::khora_cancel_stop`], to tell a program that has
    /// nowhere left to unwind to from a *fiber* that has nowhere left to
    /// unwind to. The first is an outcome — the entry point ends at 130 — and
    /// the second stops one fiber and leaves the process running.
    spawned: bool,
    /// Whether a frame on this fiber gave up on a cancellation it could not
    /// carry.
    ///
    /// Set by [`crate::cancel::khora_cancel_absorb`] and read once, by the
    /// body in [`crate::fiber::khora_fiber_spawn`], to decide what the fiber
    /// *answered*. A fiber whose root absorbed a cancellation did not produce
    /// a value, whatever word its infallible signature made it hand back, so
    /// the handle reports a cancellation rather than that word.
    ///
    /// Separate from `cancelled`, and the difference is the whole point.
    /// `cancelled` is what somebody *asked* for; this is what a frame did
    /// about it. A fiber can be cancelled and still finish normally — that is
    /// the whole of "a cancellation point is a `!`" — and a fiber can absorb a
    /// cancellation `Fiber::join` handed it without ever having been cancelled
    /// itself.
    absorbed: AtomicUsize,
    /// Where this fiber is in the sleep/wake protocol. [`crate::wait`].
    wait: crate::wait::Wait,
    /// What this fiber is parked on, while it is parked off the scheduler.
    ///
    /// On a worker, `cancel_fiber` wakes through the pool. Off one -- which is
    /// every fiber today, since a fiber is an operating-system thread -- a
    /// parked fiber is a thread inside `Condvar::wait`, and storing a word
    /// does not wake a thread. So a blocking primitive leaves the variable it
    /// is about to wait on here, and [`Fiber::cancel`] notifies it.
    ///
    /// An `Arc<Condvar>` rather than a borrow of the primitive: a `Channel` is
    /// a raw `Box` with no handle a fiber could hold, so the variable has to
    /// outlive it independently.
    parked_on: Mutex<Option<Arc<Condvar>>>,
    /// The span this fiber is inside, for `std::trace`. [`crate::span`].
    ///
    /// Here rather than in thread-local storage for the reason at the top of
    /// this module: two fibers serving two requests are inside two different
    /// spans at the same instant, and a slot on the thread answers about
    /// whichever request the worker last touched.
    ///
    /// A `Mutex` because a `Fiber` is shared through an `Arc` and this is not
    /// `Copy` into an atomic. It is never contended in practice -- only the
    /// running fiber touches its own slot, and a canceller does not read it --
    /// and a span begins far less often than a cancellation point is checked.
    span: Mutex<SpanContext>,
}

/// The bit of [`Fiber`]'s state that says it has been asked to stop.
const CANCELLED: u8 = 1;
/// The bit that says it has finished. Never cleared.
const RETIRED: u8 = 2;
/// The bit that says a cancellation may interrupt this fiber's cleanup too.
/// Never cleared. [`Fiber::force`] has the reason for both.
const FORCED: u8 = 4;

/// Which of the two requests to stop is being delivered.
///
/// **An enum passed down rather than a second copy of every delivery path.**
/// A stop reaches a fiber by several routes -- its handle, the open-nursery
/// walk, a nursery's wait and release, the release of a handle it holds, and
/// the scheduler's wake -- and a force must take every one, or a forced parent
/// leaves a child in cleanup that nobody forced. (The signal watcher only ever
/// cancels.) Matched exhaustively where it is turned into bits or a call, so a
/// third kind of stop cannot be added without those saying what it does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Stop {
    /// Stop at the next cancellation point, letting cleanup finish.
    Cancel,
    /// The same, and cleanup itself stops at its next cancellation point.
    Force,
}

/// Whether a fiber in `state` is one [`crate::poll`] counts: cancelled, and
/// still running.
fn is_counted_state(state: u8) -> bool {
    state & (CANCELLED | RETIRED) == CANCELLED
}

impl Fiber {
    /// The fiber a thread carries when nothing has been installed: the
    /// program's own computation.
    fn root() -> Fiber {
        Fiber {
            id: next_id(),
            state: AtomicU8::new(0),
            poll: &crate::poll::khora_poll,
            shielded: AtomicUsize::new(0),
            pinned: AtomicUsize::new(0),
            #[cfg(any(debug_assertions, feature = "fiber-audit"))]
            resuming: std::sync::atomic::AtomicBool::new(false),
            spawned: false,
            absorbed: AtomicUsize::new(0),
            wait: crate::wait::Wait::default(),
            parked_on: Mutex::new(None),
            span: Mutex::new(SpanContext::default()),
        }
    }

    /// A fiber somebody spawned.
    ///
    /// **The current span is inherited here, and this is the only place it
    /// could be.** This runs on the spawning side, before the child exists, so
    /// the value copied is the spawner's own and no synchronisation is needed.
    /// It is what makes a request that fans out into three fibers one trace
    /// rather than four: `std::trace` has no other way to know what a child
    /// was started from, because a closure crossing a fiber boundary carries
    /// its captures and not its caller.
    ///
    /// A copy rather than a share. What the child opens afterwards is the
    /// child's business, and a slot both could write would leave the parent
    /// holding a span it never entered.
    pub(crate) fn spawned() -> Arc<Fiber> {
        Fiber::spawned_counting_in(&crate::poll::khora_poll)
    }

    /// [`Fiber::spawned`], counted in `poll` rather than the process's word.
    fn spawned_counting_in(poll: &'static AtomicU64) -> Arc<Fiber> {
        let inherited = current(|spawner| spawner.span());
        Arc::new(Fiber {
            id: next_id(),
            state: AtomicU8::new(0),
            poll,
            shielded: AtomicUsize::new(0),
            pinned: AtomicUsize::new(0),
            #[cfg(any(debug_assertions, feature = "fiber-audit"))]
            resuming: std::sync::atomic::AtomicBool::new(false),
            spawned: true,
            absorbed: AtomicUsize::new(0),
            wait: crate::wait::Wait::default(),
            parked_on: Mutex::new(None),
            span: Mutex::new(inherited),
        })
    }

    /// Where this fiber is in the sleep/wake protocol.
    pub(crate) fn wait(&self) -> &crate::wait::Wait {
        &self.wait
    }

    /// The span this fiber is inside. All zeroes when it is inside none.
    pub(crate) fn span(&self) -> SpanContext {
        // A poisoned lock would mean a panic while holding four words of
        // plain data, which cannot leave them inconsistent. Reporting no
        // current span is better than a second panic on the way out of the
        // first one.
        self.span.lock().map(|held| *held).unwrap_or_default()
    }

    /// Makes `context` the span this fiber is inside.
    pub(crate) fn set_span(&self, context: SpanContext) {
        if let Ok(mut held) = self.span.lock() {
            *held = context;
        }
    }

    pub(crate) fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn is_spawned(&self) -> bool {
        self.spawned
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) & CANCELLED != 0
    }

    /// Asks this fiber to stop. Idempotent: asking twice is asking once.
    ///
    /// **Setting the flag is not enough on its own.** A fiber parked in
    /// `Channel::receive` observes the flag at its next cancellation point and
    /// will not reach one while it is parked, so cancelling it used to hang
    /// for ever -- and `reference/concurrency.md` says the opposite, that a
    /// blocked operation is made runnable so the fiber can unwind. Waking
    /// whatever it registered is what makes that true.
    pub(crate) fn cancel(&self) {
        self.stop(Stop::Cancel);
    }

    /// Cancels this fiber if it is not cancelled already, and lets the
    /// cancellation interrupt its cleanup as well.
    ///
    /// **What this prevents: a shutdown that waits for ever on a finalizer
    /// that never finishes.** Cleanup runs [`crate::cancel::Shielded`], so a
    /// finalizer blocked on a `receive` nobody will answer holds its fiber --
    /// and a nursery holding that fiber, and whoever waits on the nursery --
    /// indefinitely. Nothing inside the program could end that, because asking
    /// again is deliberately not escalation: see [`Fiber::cancel`] and the
    /// test `a_shielded_fiber_cancelled_twice_still_finishes_its_cleanup`.
    ///
    /// **A separate request rather than a second `cancel`**, because the
    /// runtime itself cancels one fiber more than once in ordinary operation --
    /// a nursery cancels a child when its parent is cancelled and again when a
    /// sibling fails -- and "the second cancel forces" would cut cleanup short
    /// by accident in exactly the programs that did nothing wrong.
    ///
    /// **[`FORCED`] is never cleared**, by [`Fiber::uncancel`] included, which
    /// declines to clear [`CANCELLED`] on a forced fiber. A force is the one
    /// escalation there is, and what escalates to it is a deadline that has
    /// run out; if code in the forced fiber's own cleanup could reset it, the
    /// deadline would have fired and the fiber would run on, which is the hang
    /// this exists to end.
    ///
    /// What it costs: cleanup that is forced is cut off at its next
    /// cancellation point, so a `ROLLBACK` in flight may not be sent. That is
    /// what was asked for. It cannot reach a single foreign call or file-system
    /// syscall already in progress, which returns first.
    ///
    /// Idempotent, and counted exactly as a cancellation is: once, while
    /// running.
    pub(crate) fn force(&self) {
        self.stop(Stop::Force);
    }

    /// Delivers `stop`: sets the bits, then wakes whatever this fiber is
    /// parked on.
    ///
    /// **Setting the flag is not enough on its own**, for either kind. A fiber
    /// parked in `Channel::receive` would not reach a cancellation point to see
    /// it, and a forced fiber parked in a shielded finalizer is the case force
    /// exists for.
    pub(crate) fn stop(&self, stop: Stop) {
        let bits = match stop {
            Stop::Cancel => CANCELLED,
            Stop::Force => CANCELLED | FORCED,
        };
        self.set_bits(bits);
        let parked = self.parked_on.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(moved) = parked.as_ref() {
            moved.notify_all();
        }
    }

    /// Whether [`Fiber::force`] has been called on this fiber.
    pub(crate) fn is_forced(&self) -> bool {
        self.state.load(Ordering::Acquire) & FORCED != 0
    }

    /// The stop a fiber holding a child should pass on to it, if any: what
    /// this fiber was asked for, force outranking cancel.
    pub(crate) fn pending_stop(&self) -> Option<Stop> {
        let state = self.state.load(Ordering::Acquire);
        if state & FORCED != 0 {
            Some(Stop::Force)
        } else if state & CANCELLED != 0 {
            Some(Stop::Cancel)
        } else {
            None
        }
    }

    /// Sets `bits` -- [`CANCELLED`], with or without [`FORCED`] -- and counts
    /// this fiber in [`crate::poll`] when that makes it one of the cancelled
    /// fibers still running.
    ///
    /// **Counted on the transition, not on the request.** Whether this CAS
    /// counts is decided by comparing the state it read with the state it
    /// writes, so forcing a fiber that is already cancelled adds nothing, and
    /// forcing one that is not counts it exactly as a cancel would. A force
    /// that counted on its own would leave the word one too high for the life
    /// of the process.
    ///
    /// **The word is incremented before the CAS that makes the fiber counted,
    /// and taken back if the CAS loses.** Whoever later uncounts it does so
    /// only after a CAS that read the state this CAS wrote, so the subtraction
    /// is ordered after the addition and the low half never goes below zero --
    /// which in the shared word would borrow from the pool half and read as a
    /// scheduler that does not exist. The cost of this order is a count one
    /// too high for the width of a lost CAS, which sends a back-edge down the
    /// slow path once and is otherwise harmless.
    fn set_bits(&self, bits: u8) {
        let mut seen = self.state.load(Ordering::Acquire);
        loop {
            if seen & bits == bits {
                return;
            }
            let next = seen | bits;
            let counts = !is_counted_state(seen) && is_counted_state(next);
            if counts {
                crate::poll::count_cancelled(self.poll);
            }
            match self
                .state
                .compare_exchange(seen, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(now) => {
                    if counts {
                        crate::poll::uncount_cancelled(self.poll);
                    }
                    seen = now;
                }
            }
        }
    }

    /// Registers what this fiber is about to wait on, so `cancel` can reach it.
    pub(crate) fn park_on(&self, moved: &Arc<Condvar>) {
        let mut parked = self.parked_on.lock().unwrap_or_else(|e| e.into_inner());
        *parked = Some(Arc::clone(moved));
    }

    /// Forgets it again. Called on every path out of the wait, woken or not.
    pub(crate) fn unpark_from(&self) {
        let mut parked = self.parked_on.lock().unwrap_or_else(|e| e.into_inner());
        *parked = None;
    }

    /// Clears the [`CANCELLED`] bit, and takes this fiber off the count if it
    /// was on it -- unless the fiber has been forced, when it does nothing.
    ///
    /// **A forced fiber cannot be uncancelled.** Force is what a caller's
    /// deadline escalates to once cleanup has overrun, and `khora_cancel_reset`
    /// is reachable from code running *in* that cleanup; letting it clear the
    /// flag would let the thing being stopped decide not to be. Clearing
    /// [`CANCELLED`] while leaving [`FORCED`] set would be worse still: a
    /// state that says "forced" and stops nowhere.
    ///
    /// **One CAS on the one word both the flag and the count follow.** With
    /// them in two words, a `cancel` landing between this clearing the flag
    /// and uncounting would set the flag again, see the fiber still counted,
    /// and leave; the uncount then left a cancelled fiber off the count, and
    /// its loops never looked. Here the CAS that clears the flag is the one
    /// that decides the uncount, so a racing `cancel` either loses to it and
    /// counts afresh, or wins and makes it retry. A racing force is the same:
    /// if it lands first, the retry sees [`FORCED`] and leaves.
    pub(crate) fn uncancel(&self) {
        let mut seen = self.state.load(Ordering::Acquire);
        loop {
            if seen & CANCELLED == 0 || seen & FORCED != 0 {
                return;
            }
            match self.state.compare_exchange(
                seen,
                seen & !CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if is_counted_state(seen) {
                        crate::poll::uncount_cancelled(self.poll);
                    }
                    return;
                }
                Err(now) => seen = now,
            }
        }
    }

    /// This fiber has finished, so no back-edge anywhere has a reason to look
    /// for it again -- including after a cancellation that arrives late.
    ///
    /// Idempotent. Called by [`crate::fiber::khora_fiber_spawn`] the moment
    /// the thunk returns, which is earlier than the `Drop` below would run:
    /// the handle keeps the fiber alive for as long as anybody holds it, and
    /// a server holds its listener's for the life of the process.
    pub(crate) fn retire(&self) {
        let before = self.state.fetch_or(RETIRED, Ordering::AcqRel);
        if is_counted_state(before) {
            crate::poll::uncount_cancelled(self.poll);
        }
    }

    /// Whether [`crate::poll`] is counting this fiber as cancelled and running.
    #[cfg(test)]
    pub(crate) fn is_counted(&self) -> bool {
        is_counted_state(self.state.load(Ordering::SeqCst))
    }

    /// Records that a frame gave up on a cancellation it could not carry.
    ///
    /// Idempotent, and deliberately not cleared: the fiber is on its way out
    /// and the only reader is the one that decides what it answered.
    pub(crate) fn absorb(&self) {
        self.absorbed.store(1, COUNTER_ORDER);
    }

    /// Whether [`Fiber::absorb`] has been called on this fiber.
    pub(crate) fn has_absorbed(&self) -> bool {
        self.absorbed.load(COUNTER_ORDER) != 0
    }

    /// Whether a cancellation is pending *and may be acted on here*.
    ///
    /// The one predicate behind [`crate::cancel::khora_cancelled`] and behind
    /// the check `crate::channel` makes before it gives up on a parked
    /// receive. Two readers of one rule rather than two copies of it: they
    /// disagreed once already, and a blocking primitive that stops on a
    /// cancellation a cancellation point would ignore hands back "the channel
    /// is closed" for a channel that is open.
    ///
    /// **Forced cuts through the shield, and nothing else does.** Both bits
    /// come from one load, so this never pairs a cancellation from before a
    /// force with a force from after it.
    pub(crate) fn stops_here(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        state & CANCELLED != 0
            && !self.is_pinned()
            && (state & FORCED != 0 || !self.is_shielded())
    }

    /// Whether a blocking wait should give up and hand back its "gave up"
    /// answer: a closed channel's `None`, a `false` from a send, a wait that
    /// returns early.
    ///
    /// [`Fiber::stops_here`], and one case more: **a cancelled fiber inside a
    /// change function gives up waiting, shield or no shield**, because it
    /// holds the cell's lock and a wait that never ends would hold it for
    /// ever. It does not *stop* there -- [`crate::cancel::Pinned`] says why --
    /// so the change function goes on with the "gave up" answer, returns, and
    /// the fiber stops at its first cancellation point after the lock is let
    /// go. A wait on another fiber has no such answer and asks
    /// [`Fiber::gives_up_joining`] instead.
    pub(crate) fn gives_up_waiting(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        state & CANCELLED != 0
            && (self.is_pinned() || state & FORCED != 0 || !self.is_shielded())
    }

    /// Whether a `join`, `wait` or `outcome` should stop waiting for another
    /// fiber: [`Fiber::gives_up_waiting`] without the change-function case.
    ///
    /// **What this prevents: a plain cancel cutting cleanup short.** A join
    /// has no "gave up" answer to carry on with. It comes back cancelled, and
    /// the change function and then the finalizer around it leave on that tag,
    /// so giving up here inside a shielded finalizer skipped the rest of the
    /// cleanup -- a rollback that never ran -- on a cancel that only `abort`
    /// is meant to be able to turn into that. So a join gives up exactly when
    /// the fiber would stop once the change function returned: cancelled and
    /// unshielded, or forced.
    ///
    /// What it costs: a finalizer whose change function joins holds the
    /// cell's lock until the child finishes, which is the wait
    /// [`crate::cancel::Pinned`] otherwise bounds. The bound here is the child,
    /// and `abort` ends it.
    pub(crate) fn gives_up_joining(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        state & CANCELLED != 0 && (state & FORCED != 0 || !self.is_shielded())
    }

    /// Whether this fiber is inside a change function.
    fn is_pinned(&self) -> bool {
        self.pinned.load(COUNTER_ORDER) != 0
    }

    /// Enters a change function. Nests, though a change function that updates
    /// a cell is refused before it gets here.
    pub(crate) fn pin(&self) {
        self.pinned.fetch_add(1, COUNTER_ORDER);
    }

    /// Leaves one. Saturating, for the reason [`Fiber::unshield`] gives.
    pub(crate) fn unpin(&self) {
        let depth = self.pinned.load(COUNTER_ORDER);
        self.pinned.store(depth.saturating_sub(1), COUNTER_ORDER);
    }

    /// Whether this fiber is running cleanup that must not be interrupted.
    pub(crate) fn is_shielded(&self) -> bool {
        self.shielded.load(COUNTER_ORDER) != 0
    }

    /// Enters a shielded stretch. Nests.
    pub(crate) fn shield(&self) {
        self.shielded.fetch_add(1, COUNTER_ORDER);
    }

    /// Leaves one. Saturating rather than wrapping, because a count that has
    /// gone wrong should stop shielding rather than shield for ever.
    pub(crate) fn unshield(&self) {
        let depth = self.shielded.load(COUNTER_ORDER);
        self.shielded.store(depth.saturating_sub(1), COUNTER_ORDER);
    }
}

fn next_id() -> usize {
    NEXT.fetch_add(1, COUNTER_ORDER) + 1
}

/// A fiber nobody retired is uncounted when it goes: one the test or bench
/// runner entered rather than spawned, or a thread's root at thread exit.
impl Drop for Fiber {
    fn drop(&mut self) {
        self.retire();
    }
}

thread_local! {
    /// The fiber this thread carries when nothing else has been installed.
    ///
    /// The program's own computation is a fiber too, so nothing has to
    /// special-case it.
    static ROOT: Arc<Fiber> = Arc::new(Fiber::root());

    /// The running fiber, or null before [`ROOT`] has been asked for.
    static CURRENT: Cell<*const Fiber> = const { Cell::new(std::ptr::null()) };
}

/// Reads [`CURRENT`] on *this* thread, right now.
///
/// **`#[inline(never)]` is load-bearing, and this is the second bug of the
/// kind.** A thread-local is reached through a base address the compiler holds
/// in a register, and it may compute that address once and reuse it for a
/// whole function — including across a loop. That is sound everywhere except
/// here, where a fiber can change worker in the middle of one, so the reused
/// address belongs to the thread that used to be running it.
///
/// `coro::installed` says the same thing about the yielder, where the symptom
/// was a `SIGSEGV` on an unrelated thread. Here it is quieter and worse to
/// diagnose: a fiber asks who it is, and is told about whichever fiber the
/// *previous* worker is running now. It failed
/// `a_fiber_keeps_its_identity_across_workers` with `left: 30, right: 28` as
/// soon as 11D made migration common — and a wrong answer from here is a
/// cancellation flag read off the wrong fiber.
///
/// Not inlining moves the address computation into the callee, where it runs
/// on the thread actually executing. The switch's inline assembly then does
/// the rest: it clobbers memory, so the *value* cannot be carried across a
/// suspension either.
#[inline(never)]
fn running() -> *const Fiber {
    CURRENT.with(|c| c.get())
}

/// Installs `fiber` as the running one on this thread. See [`running`].
#[inline(never)]
fn set_running(fiber: *const Fiber) {
    CURRENT.with(|c| c.set(fiber));
}

/// Installs `fiber` and returns what was there. See [`running`].
#[inline(never)]
fn swap_running(fiber: *const Fiber) -> *const Fiber {
    CURRENT.with(|c| c.replace(fiber))
}

/// This thread's own root fiber. See [`running`].
#[inline(never)]
fn root_fiber() -> Arc<Fiber> {
    ROOT.with(Arc::clone)
}

/// This thread's root fiber, as something that can be held.
///
/// **Not [`current`], which is a borrow.** The signal watcher needs a handle on
/// the program's own computation that outlives the call, and a closure-scoped
/// reference cannot give it one. Installing it as the running fiber matches
/// what `current` would have done, so the two never disagree about which fiber
/// this thread is carrying.
///
/// `cfg(unix)` because the signal watcher is the only caller and does not
/// exist on Windows, where an unconditional definition is dead code and the
/// workspace denies warnings.
#[cfg(unix)]
pub(crate) fn this_root() -> Arc<Fiber> {
    let root = root_fiber();
    if running().is_null() {
        set_running(Arc::as_ptr(&root));
    }
    root
}

/// The running fiber.
///
/// Never fails: a thread that has not entered one is carrying its own root.
pub(crate) fn current<T>(body: impl FnOnce(&Fiber) -> T) -> T {
    let pointer = running();
    if !pointer.is_null() {
        // SAFETY: the pointer was installed by `enter`, whose guard restores
        // the previous value before the `Arc` it holds can be dropped, and by
        // the `ROOT` branch below, whose `Arc` lives as long as the thread.
        return body(unsafe { &*pointer });
    }
    // The `Arc` stays alive in `ROOT` for as long as the thread does, so the
    // pointer left in `CURRENT` outlives this call.
    let root = root_fiber();
    set_running(Arc::as_ptr(&root));
    body(&root)
}

/// Makes `fiber` the running one until the guard is dropped.
///
/// Today this is called once per spawned thread. After 11A it is what a
/// context switch does, which is the reason it is a guard rather than a pair of
/// calls: restoring the previous fiber on every path out, including a panic, is
/// what stops a switch that unwinds from leaving the wrong fiber installed.
pub(crate) fn enter(fiber: Arc<Fiber>) -> Entered {
    let previous = swap_running(Arc::as_ptr(&fiber));
    Entered { _fiber: fiber, previous }
}

/// Restores whichever fiber was running before.
pub(crate) struct Entered {
    /// Held so the pointer in [`CURRENT`] stays valid for the guard's life.
    _fiber: Arc<Fiber>,
    previous: *const Fiber,
}

impl Drop for Entered {
    fn drop(&mut self) {
        set_running(self.previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_that_entered_nothing_is_its_own_root() {
        current(|f| {
            assert!(!f.is_spawned(), "the program's own computation is not spawned");
            assert!(!f.is_cancelled());
        });
    }

    #[test]
    fn the_root_is_the_same_fiber_every_time() {
        let first = current(|f| f.id());
        let second = current(|f| f.id());
        assert_eq!(first, second);
    }

    #[test]
    fn ids_are_distinct_and_never_zero() {
        let a = Fiber::spawned();
        let b = Fiber::spawned();
        assert_ne!(a.id(), b.id());
        assert_ne!(a.id(), 0, "zero has to mean nobody");
    }

    #[test]
    fn entering_swaps_the_running_fiber_and_leaving_puts_it_back() {
        let outer = current(|f| f.id());
        let fiber = Fiber::spawned();
        let inner = fiber.id();
        {
            let _entered = enter(fiber);
            assert_eq!(current(|f| f.id()), inner);
            assert!(current(|f| f.is_spawned()));
        }
        assert_eq!(current(|f| f.id()), outer, "the previous fiber should be back");
    }

    /// A switch that unwinds must not leave the wrong fiber installed, which is
    /// why `enter` returns a guard rather than being a pair of calls.
    #[test]
    fn a_panic_while_entered_still_restores() {
        let outer = current(|f| f.id());
        let caught = std::panic::catch_unwind(|| {
            let _entered = enter(Fiber::spawned());
            panic!("as if a switch unwound");
        });
        assert!(caught.is_err());
        assert_eq!(current(|f| f.id()), outer);
    }

    #[test]
    fn nesting_restores_in_order() {
        let outer = current(|f| f.id());
        let one = Fiber::spawned();
        let two = Fiber::spawned();
        let (a, b) = (one.id(), two.id());

        let first = enter(one);
        assert_eq!(current(|f| f.id()), a);
        {
            let _second = enter(two);
            assert_eq!(current(|f| f.id()), b);
        }
        assert_eq!(current(|f| f.id()), a);
        drop(first);
        assert_eq!(current(|f| f.id()), outer);
    }

    /// Cancellation belongs to the fiber, so entering another one is not
    /// cancelled by it and leaving does not clear it.
    #[test]
    fn cancellation_follows_the_fiber_rather_than_the_thread() {
        let fiber = Fiber::spawned();
        {
            let _entered = enter(fiber.clone());
            current(|f| f.cancel());
            assert!(current(|f| f.is_cancelled()));
        }
        assert!(!current(|f| f.is_cancelled()), "the root was never cancelled");
        assert!(fiber.is_cancelled(), "and the fiber still is");
    }

    /// The `Arc` is what a parent holds to cancel a child from outside.
    #[test]
    fn a_fiber_can_be_cancelled_from_another_thread() {
        let fiber = Fiber::spawned();
        let handle = fiber.clone();
        std::thread::spawn(move || handle.cancel()).join().expect("the thread");
        assert!(fiber.is_cancelled());
    }

    /// **The reason this module exists.** `Shared::update` refuses re-entry by
    /// recording the running fiber's id, and with that id in thread-local
    /// storage two fibers taking turns on one worker read the same value.
    ///
    /// The consequence under M:N is a fiber killed for a re-entry it never
    /// performed. This asserts the ids differ across a switch *on one thread*,
    /// which is the shape a scheduler produces and a thread-local cannot.
    #[test]
    fn two_fibers_on_one_thread_have_different_ids() {
        let one = Fiber::spawned();
        let two = Fiber::spawned();

        let first = {
            let _entered = enter(one);
            current(|f| f.id())
        };
        let second = {
            let _entered = enter(two);
            current(|f| f.id())
        };

        assert_ne!(
            first, second,
            "one worker carried both, so a thread-local id would have matched"
        );
    }

    // --- what `crate::poll` is told ------------------------------------------

    /// A fresh word per test: tests run on threads of one process, and a count
    /// another test is changing is not one this test can assert on.
    fn word() -> &'static AtomicU64 {
        Box::leak(Box::new(AtomicU64::new(0)))
    }

    fn counted(word: &AtomicU64) -> u64 {
        word.load(Ordering::SeqCst)
    }

    /// Counted once however often it is asked, and uncounted when it finishes.
    #[test]
    fn a_cancelled_fiber_is_counted_once_until_it_finishes() {
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        assert_eq!(counted(poll), 0);
        fiber.cancel();
        fiber.cancel();
        assert_eq!(counted(poll), 1, "asking twice is asking once");
        fiber.retire();
        assert_eq!(counted(poll), 0, "a finished fiber gives no back-edge a reason to look");
        fiber.retire();
        assert_eq!(counted(poll), 0, "and finishing is idempotent");
    }

    /// **The case a flag that stays set gets wrong.** A handle released or
    /// detached after its fiber finished cancels a fiber that is not running;
    /// counting it would leave the count up for the life of the process.
    ///
    /// Guards the [`RETIRED`] bit: finished stays finished. Red (`left: 1`)
    /// when `retire` clears the cancelled bit instead of setting retired, the
    /// reviewer's mutant M3.
    #[test]
    fn a_fiber_cancelled_after_it_finished_is_never_counted() {
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        fiber.retire();
        fiber.cancel();
        assert!(fiber.is_cancelled(), "the flag is still what was asked");
        assert_eq!(counted(poll), 0);
    }

    /// `khora_cancel_reset` clears the flag, and the count with it.
    #[test]
    fn uncancelling_uncounts_and_a_second_cancel_counts_again() {
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        fiber.uncancel();
        assert_eq!(counted(poll), 0, "uncancelling what was never cancelled takes nothing off");
        fiber.cancel();
        fiber.uncancel();
        assert_eq!(counted(poll), 0);
        fiber.cancel();
        assert_eq!(counted(poll), 1);
        fiber.retire();
        assert_eq!(counted(poll), 0);
    }

    /// A fiber that is dropped without having been retired -- one entered by
    /// the test or bench runner rather than spawned -- is uncounted then.
    #[test]
    fn dropping_a_counted_fiber_uncounts_it() {
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        fiber.cancel();
        assert_eq!(counted(poll), 1);
        drop(fiber);
        assert_eq!(counted(poll), 0);
    }

    /// The program's own computation is what SIGTERM cancels, and it never
    /// finishes, so only the count says a back-edge in `main` must look.
    #[test]
    fn the_root_is_counted_in_the_process_word() {
        // Its own thread, so this thread's root is not left cancelled for the
        // next test that runs on it.
        std::thread::spawn(|| {
            let before = crate::poll::khora_poll.load(Ordering::SeqCst);
            current(|root| {
                assert!(!root.is_spawned());
                root.cancel();
            });
            let after = crate::poll::khora_poll.load(Ordering::SeqCst);
            // Other tests on other threads may move the word too, but only
            // by fibers of their own, and never below what they added.
            assert!(
                after & crate::poll::POLL_CANCELLED > 0 && after != before,
                "the root's cancellation is counted (before {before:#x}, after {after:#x})"
            );
            current(|root| root.uncancel());
        })
        .join()
        .expect("the thread");
    }

    /// **A cancel racing the fiber's finish.** Whichever wins, the count ends
    /// at zero -- never stuck above it, and never below, which in the shared
    /// word would borrow from the pool half and read as a scheduler that does
    /// not exist.
    ///
    /// Red (`left: 2000`) when [`RETIRED`] is dropped from `retire` (mutant M3).
    /// Against the earlier two-word protocol it also caught a `retire` that
    /// checked the state and then acted on it in two steps, but only with a
    /// `yield_now` widening the gap between them (mutant M2); unwidened, that
    /// mutant passed five runs in five. `retire` is now one `fetch_or`, which
    /// has no gap to widen.
    #[test]
    fn a_cancel_racing_the_finish_leaves_the_count_at_zero() {
        const FIBERS: usize = 2000;
        let poll = word();
        let fibers: Vec<Arc<Fiber>> =
            (0..FIBERS).map(|_| Fiber::spawned_counting_in(poll)).collect();
        let cancelling = fibers.clone();
        let canceller = std::thread::spawn(move || {
            for fiber in &cancelling {
                fiber.cancel();
                fiber.uncancel();
                fiber.cancel();
            }
        });
        for fiber in &fibers {
            fiber.retire();
            assert!(counted(poll) <= FIBERS as u64, "the count went below zero");
        }
        canceller.join().expect("the canceller");
        assert_eq!(counted(poll), 0, "every fiber finished, so nothing is counted");
        drop(fibers);
        assert_eq!(counted(poll), 0, "and dropping them takes nothing further off");
    }

    /// **Cancel racing uncancel on one fiber, in lockstep.** With the flag and
    /// the count in two words, `uncancel` could clear the flag, `cancel` set it
    /// again and see the fiber still counted, and then `uncancel` uncount it:
    /// a fiber cancelled and not counted, whose call-free loops never look.
    /// About 2% of rounds did that. A batch of many fibers races too loosely
    /// to hit it, which is why this is one fiber, many times.
    ///
    /// Whatever order the two land in, the flag and the count must agree.
    #[test]
    fn cancel_racing_uncancel_leaves_flag_and_count_agreeing() {
        use std::sync::atomic::AtomicUsize as Gen;
        const ROUNDS: usize = 300_000;
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        let go = Arc::new(Gen::new(0));
        let done = Arc::new(Gen::new(0));
        let (other, go2, done2) = (fiber.clone(), go.clone(), done.clone());
        let canceller = std::thread::spawn(move || {
            for round in 1..=ROUNDS {
                while go2.load(Ordering::Acquire) < round {
                    std::hint::spin_loop();
                }
                other.cancel();
                done2.fetch_add(1, Ordering::AcqRel);
            }
        });
        let (mut missed, mut phantom) = (0usize, 0usize);
        for round in 1..=ROUNDS {
            fiber.cancel();
            go.store(round, Ordering::Release);
            fiber.uncancel();
            while done.load(Ordering::Acquire) < round {
                std::hint::spin_loop();
            }
            match (fiber.is_cancelled(), fiber.is_counted()) {
                (true, false) => missed += 1,
                (false, true) => phantom += 1,
                (true, true) | (false, false) => {}
            }
            assert_eq!(
                counted(poll),
                u64::from(fiber.is_counted()),
                "the word disagrees with the fiber in round {round}"
            );
        }
        canceller.join().expect("the canceller");
        assert_eq!((missed, phantom), (0, 0), "(cancelled and uncounted, counted and not cancelled)");
    }

    /// **Two cancellers of one fiber at once.** Both may see it uncounted and
    /// both add; only one may keep what it added, or the fiber is counted
    /// twice and uncounted once, and the count never comes back down.
    #[test]
    fn two_cancellers_at_once_count_the_fiber_once() {
        const FIBERS: usize = 20_000;
        const CANCELLERS: usize = 4;
        let poll = word();
        let fibers: Arc<Vec<Arc<Fiber>>> =
            Arc::new((0..FIBERS).map(|_| Fiber::spawned_counting_in(poll)).collect());
        let start = Arc::new(std::sync::Barrier::new(CANCELLERS));
        let cancellers: Vec<_> = (0..CANCELLERS)
            .map(|_| {
                let fibers = fibers.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for fiber in fibers.iter() {
                        fiber.cancel();
                    }
                })
            })
            .collect();
        for canceller in cancellers {
            canceller.join().expect("a canceller");
        }
        assert_eq!(counted(poll), FIBERS as u64, "each cancelled fiber counted exactly once");
        for fiber in fibers.iter() {
            fiber.retire();
        }
        assert_eq!(counted(poll), 0);
    }

    // --- force: the one stop that reaches into cleanup ----------------------

    /// **The regression the separate force operation exists to prevent.** The
    /// runtime cancels one fiber more than once in ordinary operation -- a
    /// nursery cancels a child when its parent is cancelled and again when a
    /// sibling fails -- so if a second cancel escalated, cleanup in programs
    /// that did nothing wrong would be cut short.
    ///
    /// Through the real cancellation point, [`crate::cancel::khora_cancelled`],
    /// and the real shield, rather than the predicate alone.
    #[test]
    fn a_shielded_fiber_cancelled_twice_still_finishes_its_cleanup() {
        let fiber = Fiber::spawned_counting_in(word());
        let _entered = enter(fiber.clone());
        let _cleanup = crate::cancel::Shielded::new();
        fiber.cancel();
        fiber.cancel();
        assert_eq!(crate::cancel::khora_cancelled(), 0, "a second cancel cut cleanup short");
        assert!(!fiber.is_forced());
    }

    /// A forced fiber stops at its next cancellation point, shield or no
    /// shield: that is the whole of what force adds.
    #[test]
    fn a_shielded_fiber_that_is_forced_stops_at_its_next_cancellation_point() {
        let fiber = Fiber::spawned_counting_in(word());
        let _entered = enter(fiber.clone());
        let _cleanup = crate::cancel::Shielded::new();
        fiber.cancel();
        assert_eq!(crate::cancel::khora_cancelled(), 0, "cancelled and shielded: cleanup runs");
        fiber.force();
        assert_eq!(crate::cancel::khora_cancelled(), 1, "forced: cleanup stops too");
        // Nested cleanup is still cleanup.
        let _inner = crate::cancel::Shielded::new();
        assert_eq!(crate::cancel::khora_cancelled(), 1);
    }

    /// Force on a fiber nobody cancelled is a cancellation as well, and is
    /// counted as one -- once, however it is repeated or mixed with cancel.
    #[test]
    fn forcing_an_uncancelled_fiber_cancels_it_and_counts_it_once() {
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        fiber.force();
        assert!(fiber.is_cancelled(), "force implies cancel");
        assert!(fiber.is_forced());
        assert_eq!(counted(poll), 1);
        fiber.force();
        fiber.cancel();
        assert_eq!(counted(poll), 1, "forcing again, or cancelling after, adds nothing");
        fiber.retire();
        assert_eq!(counted(poll), 0);
    }

    /// Forcing a fiber that is already cancelled escalates it without counting
    /// it a second time. Red (`left: 2`) if the count followed the request
    /// rather than the transition into the counted state.
    #[test]
    fn forcing_a_cancelled_fiber_does_not_count_it_again() {
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        fiber.cancel();
        fiber.force();
        assert!(fiber.is_forced());
        assert_eq!(counted(poll), 1);
        fiber.retire();
        assert_eq!(counted(poll), 0);
    }

    /// A finished fiber forced late is flagged and never counted, the same
    /// rule [`a_fiber_cancelled_after_it_finished_is_never_counted`] pins for
    /// cancel.
    #[test]
    fn a_fiber_forced_after_it_finished_is_never_counted() {
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        fiber.retire();
        fiber.force();
        assert!(fiber.is_forced() && fiber.is_cancelled());
        assert_eq!(counted(poll), 0);
    }

    /// **FORCED never clears, and `uncancel` does not undo a force.** What
    /// escalates to a force is a deadline that ran out; cleanup running in
    /// the forced fiber that could reset it would make the deadline fire and
    /// the fiber run on. So `uncancel` on a forced fiber leaves the
    /// cancellation, the force and the count exactly as they were.
    #[test]
    fn uncancel_does_not_undo_a_force() {
        let poll = word();
        let fiber = Fiber::spawned_counting_in(poll);
        let _entered = enter(fiber.clone());
        let _cleanup = crate::cancel::Shielded::new();
        fiber.force();
        fiber.uncancel();
        crate::cancel::khora_cancel_reset();
        assert!(fiber.is_cancelled(), "uncancel cleared a forced fiber's cancellation");
        assert!(fiber.is_forced(), "uncancel cleared the force");
        assert_eq!(counted(poll), 1, "and the count agrees with the flag");
        assert_eq!(crate::cancel::khora_cancelled(), 1, "and it still stops in cleanup");
    }

    /// Cancellers and forcers of one set of fibers at once: each fiber is
    /// counted exactly once and ends up forced. The shape of a nursery whose
    /// sibling failed while its parent was being forced.
    ///
    /// What this guards is a lost CAS: the count taken back when a
    /// `compare_exchange` loses to a racing cancel or force. No deterministic
    /// test can reach that window; this one and
    /// `two_cancellers_at_once_count_the_fiber_once` are its only guards, and
    /// together they are probabilistic: with the take-back removed, at least
    /// one of the two was red in 4 of 5 runs here (5 of 5 for the reviewer).
    /// It also catches counting on the request rather than the
    /// transition -- red on every run for the reviewer, 3 of 10 batch runs for
    /// me, so the load decides -- but
    /// [`forcing_a_cancelled_fiber_does_not_count_it_again`] is the
    /// deterministic guard for that.
    #[test]
    fn cancelling_and_forcing_at_once_count_each_fiber_once() {
        const FIBERS: usize = 20_000;
        let poll = word();
        let fibers: Arc<Vec<Arc<Fiber>>> =
            Arc::new((0..FIBERS).map(|_| Fiber::spawned_counting_in(poll)).collect());
        let start = Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = [Stop::Cancel, Stop::Force, Stop::Cancel, Stop::Force]
            .into_iter()
            .map(|stop| {
                let fibers = fibers.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for fiber in fibers.iter() {
                        fiber.stop(stop);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("a worker");
        }
        assert_eq!(counted(poll), FIBERS as u64, "each fiber counted exactly once");
        assert!(fibers.iter().all(|f| f.is_forced()), "every force landed");
        for fiber in fibers.iter() {
            fiber.retire();
        }
        assert_eq!(counted(poll), 0);
    }
}
