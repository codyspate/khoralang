//! What survives a fiber changing worker.
//!
//! **The claim the release gate makes** is that no language-visible behaviour
//! depends on a fiber staying on one operating-system thread, unless the
//! program has explicitly entered a documented thread-affine FFI boundary. It
//! had never been tested, and it is the sort of claim that stays true until
//! somebody adds one `thread_local!` in a hurry.
//!
//! Fibers are OS threads by default, where the claim is trivially true because
//! nothing migrates. Under the scheduler backend a fiber starts on one worker
//! and resumes on another, and these run there.
//!
//! # Making a fiber move, rather than hoping it does
//!
//! The first version of this module used `suspend()` and asserted afterwards
//! that some fiber had been seen on more than one worker. `suspend` puts a
//! fiber where its *own* worker will reach it soonest, so moving depends on
//! another worker stealing it — a race, not a promise. On a quiet machine it
//! passed; on the second run it reported *no fiber ever changed worker*; and
//! under `check-linux.sh`, which runs the whole suite fifteen times over, it
//! failed eleven times in fifteen. A test that is only sometimes able to
//! observe the thing it tests is a test that is sometimes lying.
//!
//! So these park instead. A parked fiber is woken by a thread that is not a
//! worker, and `scheduler::wake` hands the task to `inject`, which puts it on
//! the **shared** queue — where any worker may take it. Migration stops being
//! something to wait for and becomes what normally happens, which is also the
//! realistic shape: a fiber suspending on a socket and being woken by the
//! reactor does exactly this.
//!
//! The retry is still here as a backstop, and one combined test rather than
//! three means one migration has to be observed rather than three.
//!
//! # What is checked
//!
//! Everything [`crate::current`] holds, because that module exists precisely
//! to be the answer to "what belongs to the fiber rather than to the thread":
//! the fiber's identity, its cancellation flag, and the current span. Each was
//! a thread-local at some point in this runtime's life, and the identity one
//! was a live bug — `Shared::update`'s re-entry check read the wrong fiber's
//! id and killed correct programs.
//!
//! # And the other side of the rule
//!
//! `/docs/reference/ffi/` tells foreign code what it may not retain across a
//! Khora suspension: a thread-local address, native thread identity,
//! errno-like thread state, a thread-affine handle. A rule stated and never
//! demonstrated is a rule nobody can check a library against, so the second
//! half of this module stands up a *pretend* thread-affine library — one that
//! records the thread it was initialised on and notices when it is used from
//! another — and shows both halves: obeying the rule works, and breaking it is
//! caught by the library rather than by luck.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::coro::Task;
    use crate::current::{SpanContext, current};
    use crate::scheduler::{Scheduler, Waker, park_current, waker_for_current};

    const WORKERS: usize = 4;
    const FIBERS: usize = 16;
    const TURNS: usize = 16;

    /// How many whole runs to spend looking for a migration.
    ///
    /// A backstop rather than the mechanism: park-and-wake makes migration the
    /// normal case, so the first attempt is expected to succeed. This is what
    /// stops a saturated machine turning a real check into a red build.
    const ATTEMPTS: usize = 25;

    /// The worker a fiber is on right now.
    fn worker() -> String {
        std::thread::current().name().unwrap_or_default().to_string()
    }

    /// Runs `once` until it reports a migration, or [`ATTEMPTS`] have gone by.
    fn until_a_fiber_moves(what: &str, once: impl Fn() -> bool) {
        for _ in 0..ATTEMPTS {
            if once() {
                return;
            }
        }
        panic!("no fiber changed worker in {ATTEMPTS} runs, so this proves nothing about {what}");
    }

    /// Wakes whatever is handed to it until told to stop.
    ///
    /// Not a worker, deliberately: a wake from off the pool is what sends a
    /// task to the shared queue instead of back to the worker it came from.
    struct Shouter {
        stop: Arc<AtomicUsize>,
        hands: Option<std::thread::JoinHandle<()>>,
    }

    impl Shouter {
        fn watching(box_office: Arc<Mutex<Vec<Waker>>>) -> Shouter {
            let stop = Arc::new(AtomicUsize::new(0));
            let finished = stop.clone();
            let hands = std::thread::spawn(move || {
                while finished.load(Ordering::SeqCst) == 0 {
                    let wakers: Vec<Waker> = box_office.lock().unwrap().drain(..).collect();
                    for waker in &wakers {
                        waker.wake();
                    }
                    std::thread::yield_now();
                }
            });
            Shouter { stop, hands: Some(hands) }
        }
    }

    impl Drop for Shouter {
        fn drop(&mut self) {
            self.stop.store(1, Ordering::SeqCst);
            if let Some(hands) = self.hands.take() {
                let _ = hands.join();
            }
        }
    }

    /// **Nothing a fiber owns follows its worker.**
    ///
    /// Identity, cancellation and the current span in one run, because they
    /// are one claim and because one run means one migration to observe rather
    /// than three.
    ///
    /// Each is seeded differently per fiber, so a slot that belonged to the
    /// *worker* shows up as one fiber reading another's value rather than as
    /// nothing at all — which is the failure that looks right.
    #[test]
    fn nothing_a_fiber_owns_follows_its_worker() {
        let wrong = Arc::new(Mutex::new(Vec::<String>::new()));
        let finished = Arc::new(AtomicUsize::new(0));
        let runs = Arc::new(AtomicUsize::new(0));

        let complaints = wrong.clone();
        let completions = finished.clone();
        let attempts = runs.clone();
        until_a_fiber_moves("what a fiber owns", move || {
            attempts.fetch_add(1, Ordering::SeqCst);
            let moved = Arc::new(AtomicUsize::new(0));
            let box_office = Arc::new(Mutex::new(Vec::<Waker>::new()));
            let shouter = Shouter::watching(box_office.clone());

            let pool = Scheduler::new(WORKERS);
            for each in 0..FIBERS {
                let migrations = moved.clone();
                let said = complaints.clone();
                let done = completions.clone();
                let desk = box_office.clone();
                pool.spawn(Task::new(move || {
                    let cancelling = each % 2 == 0;
                    if cancelling {
                        current(|fiber| fiber.cancel());
                    }
                    let span = SpanContext {
                        trace_high: 1000 + each as i64,
                        trace_low: 2000 + each as i64,
                        span: 3000 + each as i64,
                        sampled: each % 3 == 0,
                    };
                    current(|fiber| fiber.set_span(span));
                    let mine = current(|fiber| fiber.id());

                    let mut first = worker();
                    let mut seen_elsewhere = false;
                    for _ in 0..TURNS {
                        let now = worker();
                        if now != first {
                            seen_elsewhere = true;
                            first = now;
                        }

                        let id = current(|fiber| fiber.id());
                        if id != mine {
                            said.lock().unwrap().push(format!("id {mine} became {id}"));
                        }
                        if current(|fiber| fiber.is_cancelled()) != cancelling {
                            said.lock()
                                .unwrap()
                                .push(format!("fiber {mine} lost its cancellation flag"));
                        }
                        let now_span = current(|fiber| fiber.span());
                        if now_span != span {
                            said.lock().unwrap().push(format!("{span:?} became {now_span:?}"));
                        }

                        // Park, and let the shouter put it on the shared queue.
                        let Some(waker) = waker_for_current() else { break };
                        desk.lock().unwrap().push(waker);
                        if !park_current() {
                            break;
                        }
                    }

                    if seen_elsewhere {
                        migrations.fetch_add(1, Ordering::SeqCst);
                    }
                    done.fetch_add(1, Ordering::SeqCst);
                }));
            }
            pool.drain();
            drop(shouter);
            moved.load(Ordering::SeqCst) > 0
        });

        let complaints = wrong.lock().unwrap();
        assert!(complaints.is_empty(), "something a fiber owns followed its worker: {complaints:?}");
        assert_eq!(
            finished.load(Ordering::SeqCst),
            runs.load(Ordering::SeqCst) * FIBERS,
            "a fiber did not finish"
        );
    }

    /// Kept under its old name so that `docs/design/soundness.md` and
    /// `scheduler.md` still cite something that exists.
    ///
    /// It named the identity half specifically, and the identity half was a
    /// real bug: the id used to live in thread-local storage, so a fiber
    /// scheduled onto a worker whose previous occupant held a `Shared` lock
    /// read that occupant's id, matched the recorded holder, and was killed
    /// for a re-entry it never performed.
    #[test]
    fn a_fiber_keeps_its_identity_across_workers() {
        nothing_a_fiber_owns_follows_its_worker();
    }

    // -----------------------------------------------------------------------
    // Thread-affine foreign libraries
    // -----------------------------------------------------------------------

    /// A pretend foreign library that may only be used from the thread that
    /// initialised it.
    ///
    /// Real ones are everywhere: an OpenGL context, a COM apartment, a GUI
    /// toolkit's main loop, SQLite built without `SQLITE_THREADSAFE`. All of
    /// them behave like this and most are less polite about it.
    struct Affine {
        owner: std::thread::ThreadId,
    }

    impl Affine {
        fn initialise() -> Affine {
            Affine { owner: std::thread::current().id() }
        }

        /// Whether this call is on the thread that initialised it.
        fn used_here(&self) -> bool {
            std::thread::current().id() == self.owner
        }
    }

    /// **Obeying the rule works: no suspension, no migration.**
    ///
    /// A run of foreign calls with nothing between them stays on one thread,
    /// because an ordinary foreign call cannot secretly suspend Khora — which
    /// is what the FFI reference promises, and the reason a thread-affine
    /// library is usable from Khora at all.
    #[test]
    fn a_thread_affine_library_is_safe_across_calls_that_do_not_suspend() {
        let broke = Arc::new(AtomicUsize::new(0));

        let pool = Scheduler::new(WORKERS);
        for _ in 0..FIBERS {
            let complaints = broke.clone();
            pool.spawn(Task::new(move || {
                let library = Affine::initialise();
                for _ in 0..256 {
                    if !library.used_here() {
                        complaints.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }));
        }
        pool.drain();

        assert_eq!(
            broke.load(Ordering::SeqCst),
            0,
            "a run of foreign calls with no suspension between them changed thread, which would \
             mean an ordinary foreign call can suspend Khora"
        );
    }

    /// **Breaking the rule is caught, and this is why the rule exists.**
    ///
    /// The same library used either side of a suspension. The fiber comes back
    /// on a different worker, and the handle is then in use from a thread that
    /// did not initialise it. A real library answers that with undefined
    /// behaviour; this one counts it.
    ///
    /// The assertion is that it **does** happen: the point is to demonstrate
    /// the hazard the reference warns about, not to hope it is absent.
    #[test]
    fn a_thread_affine_handle_held_across_a_suspension_is_used_from_the_wrong_thread() {
        let caught = Arc::new(AtomicUsize::new(0));

        let complaints = caught.clone();
        until_a_fiber_moves("a handle used from the wrong thread", move || {
            let before = complaints.load(Ordering::SeqCst);
            let box_office = Arc::new(Mutex::new(Vec::<Waker>::new()));
            let shouter = Shouter::watching(box_office.clone());

            let pool = Scheduler::new(WORKERS);
            for _ in 0..FIBERS {
                let said = complaints.clone();
                let desk = box_office.clone();
                pool.spawn(Task::new(move || {
                    let library = Affine::initialise();
                    for _ in 0..TURNS {
                        // Exactly what the reference forbids: the handle is
                        // held across the suspension and used again after it.
                        let Some(waker) = waker_for_current() else { break };
                        desk.lock().unwrap().push(waker);
                        if !park_current() {
                            break;
                        }
                        if !library.used_here() {
                            said.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }));
            }
            pool.drain();
            drop(shouter);
            complaints.load(Ordering::SeqCst) > before
        });

        assert!(
            caught.load(Ordering::SeqCst) > 0,
            "a handle held across a suspension was never used from another thread"
        );
    }

    /// **A `blocking` call runs on one thread from start to finish.**
    ///
    /// The documented boundary for foreign work that goes away for a while.
    /// The fiber suspends and may resume anywhere, but the *call* does not
    /// move — it runs on one pool thread — so a thread-affine library used
    /// entirely inside one `blocking` call is used correctly.
    #[test]
    fn a_blocking_call_does_not_move_while_it_runs() {
        let moved = Arc::new(AtomicUsize::new(0));

        let pool = Scheduler::new(WORKERS);
        for _ in 0..FIBERS {
            let complaints = moved.clone();
            pool.spawn(Task::new(move || {
                let split = crate::blocking::blocking(|| {
                    let library = Affine::initialise();
                    let mut wrong = 0;
                    for _ in 0..1000 {
                        if !library.used_here() {
                            wrong += 1;
                        }
                    }
                    wrong
                });
                if split > 0 {
                    complaints.fetch_add(split, Ordering::SeqCst);
                }
            }));
        }
        pool.drain();

        assert_eq!(
            moved.load(Ordering::SeqCst),
            0,
            "work inside one `blocking` call ran on more than one thread, which would make the \
             documented boundary for thread-affine foreign work useless"
        );
    }
}
