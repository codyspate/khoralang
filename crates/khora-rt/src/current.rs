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
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use crate::counters::COUNTER_ORDER;

/// Fiber ids, handed out on first use and never reused.
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// What belongs to a fiber rather than to a thread.
pub(crate) struct Fiber {
    /// Only ever compared for equality. Never zero, so zero can mean "nobody".
    id: usize,
    /// Whether this fiber has been asked to stop.
    ///
    /// Shared rather than owned, because a parent holding the handle sets it
    /// from outside.
    cancelled: AtomicUsize,
    /// Set while a worker is inside this fiber's `resume`. See
    /// `crate::coro::ResumedOnce`; debug builds only.
    #[cfg(debug_assertions)]
    pub(crate) resuming: std::sync::atomic::AtomicBool,
    /// Whether this is a spawned fiber rather than the program's own
    /// computation.
    ///
    /// Only [`crate::cancel::khora_cancel_stop`] asks, and only to tell a
    /// program that has nowhere left to unwind to from a *fiber* that has
    /// nowhere left to unwind to. The first is an outcome; the second is a
    /// hole.
    spawned: bool,
    /// Where this fiber is in the sleep/wake protocol. [`crate::wait`].
    wait: crate::wait::Wait,
}

impl Fiber {
    /// The fiber a thread carries when nothing has been installed: the
    /// program's own computation.
    fn root() -> Fiber {
        Fiber {
            id: next_id(),
            cancelled: AtomicUsize::new(0),
            #[cfg(debug_assertions)]
            resuming: std::sync::atomic::AtomicBool::new(false),
            spawned: false,
            wait: crate::wait::Wait::default(),
        }
    }

    /// A fiber somebody spawned.
    pub(crate) fn spawned() -> Arc<Fiber> {
        Arc::new(Fiber {
            id: next_id(),
            cancelled: AtomicUsize::new(0),
            #[cfg(debug_assertions)]
            resuming: std::sync::atomic::AtomicBool::new(false),
            spawned: true,
            wait: crate::wait::Wait::default(),
        })
    }

    /// Where this fiber is in the sleep/wake protocol.
    pub(crate) fn wait(&self) -> &crate::wait::Wait {
        &self.wait
    }

    pub(crate) fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn is_spawned(&self) -> bool {
        self.spawned
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(COUNTER_ORDER) != 0
    }

    /// Asks this fiber to stop. Idempotent: asking twice is asking once.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(1, COUNTER_ORDER);
    }

    pub(crate) fn uncancel(&self) {
        self.cancelled.store(0, COUNTER_ORDER);
    }
}

fn next_id() -> usize {
    NEXT.fetch_add(1, COUNTER_ORDER) + 1
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
}
