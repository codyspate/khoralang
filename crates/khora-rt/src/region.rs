//! Regions, and the finalizers they run.
//!
//! A region is an ordinary counted object whose release runs a list of
//! closures. That the list can grow is why its release is written here rather
//! than generated: nothing in Khora grows a value in place, so the `Vec` has to
//! live on this side. Everything else about a region is ordinary, which is what
//! makes its finalizers run on exactly the paths that already release a local —
//! including a raise passing through. `docs/design/memory.md`.

use super::*;
use crate::heap::{khora_alloc, khora_drop, khora_dup};
use std::sync::Mutex;

/// One deferred finalizer: the closure to call, and the glue to release it
/// with.
///
/// The glue travels with the closure because the runtime cannot work it out.
/// A closure's drop routine is *generated* — one shared routine switching on
/// the site tag — so the only thing that knows the pointer is the code that
/// built the closure, which is exactly the code that defers it.
#[repr(C)]
struct Finalizer {
    closure: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
    /// How to call it. See [`khora_region_defer`].
    call: Option<Trampoline1>,
}

/// A region's finalizers, in the order they were deferred, and the one fiber
/// that may defer them.
///
/// Held Rust-side rather than as a Khora list because deferring *grows* it, and
/// nothing in Khora can grow a value in place. The Khora object is a handle:
/// one field holding a pointer to this.
///
/// **`owner` is the backstop for a finalizer run on the wrong fiber.** A
/// finalizer's captures need not be `Share`, so it may hold a `mut` record
/// the deferring fiber goes on writing; run on another fiber, it reads a field
/// that fiber can replace and free under it. The checker refuses every route
/// a `Region` or `Scope` has to another fiber (`khora_types::REGION_TYPE`),
/// so this never fires on a program that compiled -- unless a route was
/// missed, and then it traps at the defer rather than racing at the release.
/// It costs a fiber-id read per defer.
///
/// **The `Mutex` guards against a runtime bug, not a language one.** Every
/// defer comes from `owner`, so it is never contended. It stays because a
/// region is not a hot path -- it is touched when a resource is acquired, not
/// when one is used -- and a lock taken uncontended costs next to nothing.
struct Finalizers {
    owner: usize,
    list: Mutex<Vec<Finalizer>>,
}

/// The running fiber's id, as [`Finalizers::owner`] records it.
fn this_fiber() -> usize {
    crate::current::current(|fiber| fiber.id())
}

/// The tag every region object carries. Regions are not an ADT, so no variant
/// index competes for it.
const REGION_TAG: u32 = 0;

/// The region that ends when the program does.
///
/// One per program, created on first use and released by the generated entry
/// point after `main` returns — on the failing path as well as the ordinary
/// one, because a finalizer that only runs when nothing went wrong is not a
/// finalizer.
static mut ROOT: *mut u8 = std::ptr::null_mut();

/// A reference to the root region.
///
/// **Only the program's own fiber may reach it**, and a spawned fiber that
/// asks is a fatal error. `Region::root()` is reachable by name, so no type
/// rule keeps it on one fiber, and a finalizer a child deferred into it would
/// run at exit on the main fiber while the child might still be writing what
/// it captured. A child that wants something released opens a `scoped` of its
/// own, and the message says so.
///
/// A `test` block is the exception the harness makes: each runs on a spawned
/// fiber, and gets a root region of its own, released when the test ends.
/// [`crate::testing::test_root`].
///
/// # Safety
///
/// Single-threaded, like everything else here: fibers running across cores
/// (A5) will need this behind the same lock the refcounts eventually go
/// behind. `docs/roadmap.md` D10. The spawned-fiber trap is what keeps the
/// program's fibers off it; a foreign thread calling an exported function is
/// not a spawned fiber, and is not kept off it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_root() -> *mut u8 {
    if crate::current::current(|fiber| fiber.is_spawned()) {
        if let Some(root) = crate::testing::test_root() {
            return root;
        }
        fatal(
            "`Region::root()` or `Scope::root()` reached from a spawned fiber: the root \
             region belongs to the program's own fiber, so give this one a scope of its own \
             with `scoped(work)`",
        );
    }
    // SAFETY: single-threaded per the note above.
    unsafe {
        if ROOT.is_null() {
            ROOT = khora_region_open();
        }
        khora_dup(ROOT);
        ROOT
    }
}

/// Releases the root region, running whatever was deferred to it.
///
/// Called once by the generated entry point. A second call is a no-op, so a
/// program that never touched the root region costs nothing.
///
/// # Safety
///
/// Must be called after every other Khora frame has returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_close_root() {
    // SAFETY: single-threaded, and the caller guarantees nothing else is
    // still running.
    unsafe {
        let root = ROOT;
        ROOT = std::ptr::null_mut();
        if !root.is_null() {
            khora_drop(root, Some(release_shim));
        }
    }
}

/// [`khora_region_release`] as a `drop_fields` callback.
///
/// The callback type is a *safe* `extern "C" fn`, because that is what
/// generated code passes and generated code has no notion of Rust's `unsafe`.
/// The release itself keeps its contract, so the shim is where the claim that
/// the contract holds is made — once, here, rather than at every drop site.
// SHARE: releases a region; see `khora_region_release`.
pub(crate) extern "C" fn release_shim(region: *mut u8) {
    // SAFETY: only ever reached through `khora_drop`, which calls it with the
    // object whose last reference it just released.
    unsafe { khora_region_release(region) };
}

/// Opens a region, returning a Khora object that owns it.
///
/// The object is ordinary in every way that matters — reference counted,
/// dropped by [`khora_drop`] — which is the whole design. Its release runs the
/// finalizers, so a region ends exactly when the binding holding it does: at
/// the end of a block, at an early `return`, or on a raise passing through.
/// Every one of those paths already releases a boxed local, so none of them
/// needed a new rule.
#[unsafe(no_mangle)]
pub extern "C" fn khora_region_open() -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Finalizers>() as u64, REGION_TAG);
    // Every region handle is born shared. Its references all live on its
    // owner, so it could be born local; it is not, because a shared handle
    // costs an atomic count and a missed route would cost a torn one.
    crate::share::born_shared(object);
    let list = Box::new(Finalizers { owner: this_fiber(), list: Mutex::default() });
    // SAFETY: `khora_alloc` returned an object with one field's worth of
    // space, zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>().write(Box::into_raw(list));
    }
    object
}

/// Registers a finalizer to run when `region` ends.
///
/// Takes ownership of `closure`: the region releases it after calling it, so
/// the caller hands over a reference of its own rather than lending one.
///
/// **`call` is how to call it, and generated code always passes one.** A Khora
/// `() -> ()` closure hands back a cancellation tag and a word, a 16-byte
/// aggregate; calling it through a `void` function pointer is right on some
/// targets and reads a hidden return pointer nobody passed on x86-64 Windows,
/// which is errata 35. The trampoline takes the pair apart on the generated
/// side. `None` is a closure written in Rust, for this crate's own tests,
/// which returns nothing.
///
/// # Safety
///
/// `region` must be a live object from [`khora_region_open`], `closure` a
/// live Khora closure of type `() -> ()` whose drop routine is `glue`, and
/// `call`, when given, the trampoline matching it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_defer(
    region: *mut u8,
    closure: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
    call: Option<Trampoline1>,
) {
    if region.is_null() {
        fatal("deferring a finalizer to a null region");
    }
    // SAFETY: the caller guarantees a live region, whose field holds the
    // pointer `khora_region_open` wrote there.
    let list = unsafe { *region.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>() };
    if list.is_null() {
        fatal("deferring a finalizer to a region that has already been released");
    }
    // SAFETY: non-null, so the box `khora_region_open` made is still alive,
    // and `owner` is never written after it was.
    if unsafe { (*list).owner } != this_fiber() {
        fatal(
            "deferring a finalizer to a region another fiber opened: a finalizer runs on \
             the fiber that deferred it, so give this fiber a scope of its own with \
             `scoped(work)`",
        );
    }
    // SHARE: stores the closure; not a crossing. The check above makes this
    // fiber the region's owner, the only one that can release it and so run
    // the closure, which is why it is never marked.
    //
    // SAFETY: as above; the box is alive until the region is released, and the
    // field is the only handle to it.
    unsafe {
        (*list)
            .list
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Finalizer { closure, glue, call })
    };
}

/// Runs a region's finalizers and frees its list.
///
/// This is a `drop_fields` callback: [`khora_drop`] calls it when the last
/// reference to the region goes, and frees the object itself afterwards.
///
/// **Reverse order.** A finalizer deferred later may depend on one deferred
/// earlier — a transaction rolled back before the connection it ran on is
/// closed — so the last acquired is the first released, the same rule a stack
/// of scopes follows.
///
/// A finalizer that itself defers is deferring to a region that is already
/// releasing, which [`khora_region_defer`] rejects rather than silently
/// dropping.
///
/// # Safety
///
/// `region` must be a live object from [`khora_region_open`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_release(region: *mut u8) {
    // SHARE: releases, and runs finalizers on the fiber that deferred them: a
    // region's references never leave its owner, and the check below traps
    // a release anywhere else, so nothing here crosses.
    if region.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live region, so it has a field's worth
    // of space past the header. Computing the address reads nothing.
    let slot = unsafe { region.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>() };
    // SAFETY: the caller guarantees a live region; the field holds what
    // `khora_region_open` wrote, and nothing else reads it after this.
    let list = unsafe { *slot };
    if list.is_null() {
        return;
    }
    // **The release is checked as the defer is.** A route the checker missed
    // may hand another fiber only the last reference, with no defer for the
    // check there to see; this fiber would then run the owner's finalizers
    // over captures the owner may still be writing. Trapped here instead, in
    // every build. A fiber-id read per region release.
    //
    // SAFETY: non-null, so the box `khora_region_open` made is still alive,
    // and `owner` is never written after it was.
    if unsafe { (*list).owner } != this_fiber() {
        fatal(
            "releasing a region another fiber opened: a region's finalizers run on the \
             fiber that deferred them, so give this fiber a scope of its own with \
             `scoped(work)`",
        );
    }
    // Cleared before running anything, so a finalizer that reaches this region
    // again finds it released rather than re-entering the list being drained.
    unsafe { slot.write(std::ptr::null_mut()) };

    // SAFETY: the pointer came from `Box::into_raw` in `khora_region_open` and
    // has not been freed — the null check above is what guarantees that.
    let list = unsafe { Box::from_raw(list) };
    let list = list.list.into_inner().unwrap_or_else(|e| e.into_inner());

    // **Finalizers are not cancelable.** This release may itself be part of a
    // cancellation unwinding, in which case the flag is still set and the
    // first `!` inside a finalizer would stop it half-done — a rollback that
    // never reaches the server, a connection returned to the pool inside an
    // open transaction. [`crate::cancel::Shielded`] has the argument.
    let _shield = crate::cancel::Shielded::new();

    for finalizer in list.into_iter().rev() {
        // **Not marked: this is not a crossing.** A region's references all
        // live on the fiber that opened it, so the fiber releasing it last is
        // the one that deferred this closure, and its captures stay local,
        // counted by the fiber that made them. A missed route shows up as the
        // owner check's trap in a debug build, not as a torn count.
        //
        // SAFETY: a closure's first field is its code pointer, and a `() -> ()`
        // closure is called with its own object as the only argument. Through
        // the trampoline `khora_region_defer` was handed when there is one,
        // which is every closure generated code built; directly for a Rust
        // one, which returns nothing. What it answered is not read: a
        // finalizer runs shielded, so it cannot have been stopped unless it
        // was forced, and then there is nothing left here to do but go on to
        // the next one, which the force stops at its first cancellation point.
        unsafe {
            let code = *finalizer.closure.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
            // **The finalizer runs outside the drain this release may be part
            // of**, so an object whose scope ends inside it -- a region, a
            // fiber handle, a nursery -- is released there, at the end of its
            // scope, not after the finalizer returns (S3;
            // `crate::heap::Isolated` has the argument). Around the call only:
            // the closure's own captures are released below, back in the
            // outer drain, so a chain of regions each captured by the last
            // one's finalizer is still released flat.
            let isolated = crate::heap::Isolated::new();
            match finalizer.call {
                Some(call) => {
                    let mut answer: u64 = 0;
                    let _which = call(code, finalizer.closure, &raw mut answer);
                }
                None => {
                    let call: extern "C" fn(*mut u8) = std::mem::transmute(code);
                    call(finalizer.closure);
                }
            }
            drop(isolated);
            khora_drop(finalizer.closure, finalizer.glue);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::{khora_cancel, khora_cancel_reset, khora_canceled};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A closure object of type `() -> ()`: one field, holding the code
    /// pointer that `khora_region_release` calls it through. Fabricated here
    /// because generated code is what usually builds one, and this crate has
    /// no compiler.
    fn closure(code: extern "C" fn(*mut u8)) -> *mut u8 {
        let object = khora_alloc(std::mem::size_of::<*const u8>() as u64, 0);
        // SAFETY: one field's worth of space, freshly allocated, and nothing
        // else holds the pointer yet.
        unsafe {
            object.add(KHORA_FIELD_OFFSET).cast::<extern "C" fn(*mut u8)>().write(code);
        }
        object
    }

    static RAN: AtomicUsize = AtomicUsize::new(0);
    static SAW: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn watching(_closure: *mut u8) {
        RAN.fetch_add(1, Ordering::SeqCst);
        SAW.store(usize::from(khora_canceled()), Ordering::SeqCst);
    }

    /// **The 13.3 property.** A finalizer running as part of a cancellation
    /// must not itself be canceled, or a rollback stops at its first `!` and
    /// the connection goes back to the pool holding locks.
    #[test]
    fn a_finalizer_does_not_see_the_cancellation_that_is_running_it() {
        let region = khora_region_open();
        // SAFETY: a live region and a live closure whose drop is the default.
        unsafe { khora_region_defer(region, closure(watching), None, None) };

        khora_cancel();
        assert_eq!(khora_canceled(), 1, "the flag is set before the region ends");

        // SAFETY: the only reference, as `khora_drop` would have found it.
        unsafe { khora_region_release(region) };

        assert_eq!(RAN.load(Ordering::SeqCst), 1, "the finalizer ran");
        assert_eq!(SAW.load(Ordering::SeqCst), 0, "and ran with the cancellation held off");
        assert_eq!(khora_canceled(), 1, "which masks the flag rather than clearing it");

        khora_cancel_reset();
        // SAFETY: released above, so the fields are already gone.
        unsafe { khora_drop(region, None) };
    }

    static INNER: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn opens_another(_closure: *mut u8) {
        extern "C" fn innermost(_closure: *mut u8) {
            INNER.store(usize::from(khora_canceled()), Ordering::SeqCst);
        }
        let region = khora_region_open();
        // SAFETY: as above.
        unsafe {
            khora_region_defer(region, closure(innermost), None, None);
            khora_region_release(region);
            khora_drop(region, None);
        }
    }

    /// The shield nests, because a finalizer that releases a region of its own
    /// is ordinary — a lease returning a connection that rolls back first.
    #[test]
    fn the_shield_survives_a_finalizer_that_ends_a_region() {
        let region = khora_region_open();
        // SAFETY: as above.
        unsafe { khora_region_defer(region, closure(opens_another), None, None) };

        khora_cancel();
        // SAFETY: as above.
        unsafe { khora_region_release(region) };

        assert_eq!(INNER.load(Ordering::SeqCst), 0, "the inner finalizer is shielded too");
        assert_eq!(khora_canceled(), 1, "and the outer one leaves the flag alone");

        khora_cancel_reset();
        // SAFETY: released above.
        unsafe { khora_drop(region, None) };
    }

    /// Nothing is masked once the region has ended, so an ordinary program
    /// pays no attention to any of this.
    #[test]
    fn an_uncanceled_region_is_unaffected() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn counting(_closure: *mut u8) {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }

        let region = khora_region_open();
        // SAFETY: as above.
        unsafe {
            khora_region_defer(region, closure(counting), None, None);
            khora_region_defer(region, closure(counting), None, None);
            khora_region_release(region);
            khora_drop(region, None);
        }
        assert_eq!(COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(khora_canceled(), 0);
    }

    // --- A region stays on the fiber that opened it -------------------------
    //
    // The checker refuses every route a `Region` or `Scope` has to another
    // fiber. These are the runtime's backstop for the routes it cannot see,
    // and each one ends the process, so each runs in a copy of this test
    // binary and the parent reads how the copy died.

    /// Set in the copy of the test binary that [`dies_in_a_copy`] starts,
    /// naming which of the fatal cases to run.
    const FATAL_CHILD: &str = "KHORA_RT_REGION_FATAL_CHILD";

    /// Runs the test `name` in a copy of this binary with [`FATAL_CHILD`] set
    /// to `case`, and requires that it exit 134 saying `expected`.
    fn dies_in_a_copy(name: &str, case: &str, expected: &str) {
        let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(FATAL_CHILD, case)
            .output()
            .expect("the copy of the test binary should start");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(134), "the copy did not trap: {stderr}");
        assert!(stderr.contains(expected), "the copy trapped for another reason: {stderr}");
    }

    /// Runs `body` on a fresh thread carrying a spawned fiber, as
    /// `khora_fiber_spawn` runs a child on the thread backend.
    fn on_a_spawned_fiber(body: impl FnOnce() + Send + 'static) {
        std::thread::spawn(move || {
            let _entered = crate::current::enter(crate::current::Fiber::spawned());
            body();
        })
        .join()
        .expect("the spawned fiber's thread");
    }

    extern "C" fn nothing(_closure: *mut u8) {}

    /// **A defer from a fiber that did not open the region is fatal.** This
    /// is what a route the checker missed looks like: the finalizer would run
    /// on the region's owner while the deferring fiber still wrote its
    /// captures.
    #[test]
    fn a_defer_from_another_fiber_is_fatal() {
        if std::env::var(FATAL_CHILD).as_deref() == Ok("defer") {
            let region = khora_region_open() as usize;
            on_a_spawned_fiber(move || {
                // SAFETY: a live region, opened above and not yet released,
                // and a live closure whose drop is the default.
                unsafe { khora_region_defer(region as *mut u8, closure(nothing), None, None) };
            });
            return;
        }
        dies_in_a_copy(
            "region::tests::a_defer_from_another_fiber_is_fatal",
            "defer",
            "deferring a finalizer to a region another fiber opened",
        );
    }

    /// **The root region from a spawned fiber is fatal.** It is reachable by
    /// name, so no type keeps a child off it.
    #[test]
    fn the_root_region_from_a_spawned_fiber_is_fatal() {
        if std::env::var(FATAL_CHILD).as_deref() == Ok("root") {
            on_a_spawned_fiber(|| {
                // SAFETY: nothing else in this copy touches the root region.
                let _ = unsafe { khora_region_root() };
            });
            return;
        }
        dies_in_a_copy(
            "region::tests::the_root_region_from_a_spawned_fiber_is_fatal",
            "root",
            "`Region::root()` or `Scope::root()` reached from a spawned fiber",
        );
    }

    /// **A release on a fiber that did not open the region is fatal.** This
    /// is a route the checker missed where the other fiber never defers: it
    /// only drops the last reference, and so would run the owner's
    /// finalizers over captures the owner may still be writing. The
    /// review's `var_capture.kh` is this shape.
    #[test]
    fn a_release_on_another_fiber_is_fatal() {
        if std::env::var(FATAL_CHILD).as_deref() == Ok("release") {
            let region = khora_region_open() as usize;
            // SAFETY: a live region and a live closure whose drop is the default.
            unsafe { khora_region_defer(region as *mut u8, closure(nothing), None, None) };
            on_a_spawned_fiber(move || {
                // SAFETY: the only reference, handed to this fiber, released
                // the way generated code releases it.
                unsafe { khora_drop(region as *mut u8, Some(release_shim)) };
            });
            return;
        }
        dies_in_a_copy(
            "region::tests::a_release_on_another_fiber_is_fatal",
            "release",
            "releasing a region another fiber opened",
        );
    }

    /// A defer from the fiber that opened the region is the ordinary case, on
    /// a spawned fiber as on the program's own: the owner is the fiber, not
    /// "the main one".
    #[test]
    fn a_spawned_fiber_defers_into_its_own_region() {
        static RAN: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn counting(_closure: *mut u8) {
            RAN.fetch_add(1, Ordering::SeqCst);
        }
        on_a_spawned_fiber(|| {
            let region = khora_region_open();
            // SAFETY: a live region this fiber opened, and its only reference.
            unsafe {
                khora_region_defer(region, closure(counting), None, None);
                khora_drop(region, Some(release_shim));
            }
        });
        assert_eq!(RAN.load(Ordering::SeqCst), 1);
    }

    // --- S3: a drain left open across a finalizer ---------------------------
    //
    // `crate::heap` releases a graph through a per-thread queue: the first
    // last-drop claims the drain, and every nested last-drop is queued behind
    // it. A region's release runs user finalizers from inside that drain, and a
    // finalizer may block. These tests pin what a finalizer that blocks, or
    // that ends a scope of its own, must not do to anybody else's frees.

    /// Waits up to five seconds for `done`, so a red run fails rather than
    /// hangs.
    fn eventually(done: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        true
    }

    /// **S3.** A fiber whose finalizer is parked must not hold the frees of
    /// the next fiber its worker runs.
    ///
    /// One worker, so the second fiber is certain to run on the thread the
    /// first one parked on. Before the fix the second fiber's region went into
    /// the parked fiber's drain queue and its finalizer never ran, while the
    /// fiber itself finished: `finished true`, finalizer 0, which is what
    /// `p/starve` showed from Khora.
    #[test]
    fn a_parked_finalizer_does_not_swallow_the_next_fibers_finalizer() {
        use crate::coro::Task;
        use crate::scheduler::{park_current, waker_for_current, Scheduler, Waker};

        static PARKED: Mutex<Option<Waker>> = Mutex::new(None);
        static FIRST_DONE: AtomicUsize = AtomicUsize::new(0);
        static SECOND_RAN: AtomicUsize = AtomicUsize::new(0);
        static SECOND_SAW: AtomicUsize = AtomicUsize::new(usize::MAX);

        extern "C" fn parks(_closure: *mut u8) {
            *PARKED.lock().unwrap() = waker_for_current();
            park_current();
        }
        extern "C" fn counts(_closure: *mut u8) {
            SECOND_RAN.fetch_add(1, Ordering::SeqCst);
        }

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(|| {
            let region = khora_region_open();
            // SAFETY: a live region and a live closure whose drop is the default;
            // the only reference, released the way generated code releases it.
            unsafe {
                khora_region_defer(region, closure(parks), None, None);
                khora_drop(region, Some(release_shim));
            }
            FIRST_DONE.store(1, Ordering::SeqCst);
        }));
        assert!(eventually(|| PARKED.lock().unwrap().is_some()), "the first fiber never parked");

        pool.spawn(Task::new(|| {
            let region = khora_region_open();
            // SAFETY: as above.
            unsafe {
                khora_region_defer(region, closure(counts), None, None);
                khora_drop(region, Some(release_shim));
            }
            SECOND_SAW.store(SECOND_RAN.load(Ordering::SeqCst), Ordering::SeqCst);
        }));
        assert!(
            eventually(|| SECOND_SAW.load(Ordering::SeqCst) != usize::MAX),
            "the second fiber never finished"
        );
        assert_eq!(
            SECOND_SAW.load(Ordering::SeqCst),
            1,
            "the second fiber's region ended and its finalizer had not run: its release was \
             queued behind the first fiber's parked finalizer"
        );

        // Let the first one go, so the pool can wind down.
        if let Some(waker) = PARKED.lock().unwrap().take() {
            waker.wake();
        }
        pool.drain();
        assert_eq!(FIRST_DONE.load(Ordering::SeqCst), 1);
        assert_eq!(SECOND_RAN.load(Ordering::SeqCst), 1);
    }

    /// The same, for any `drop_fields` callback that suspends, not only a
    /// region's. A fiber handle's release waits for the child, and a nursery's
    /// release waits for every child, so both are this shape. The suspension is
    /// what must not leave a drain behind on the worker.
    #[test]
    fn a_release_that_suspends_leaves_no_drain_on_its_worker() {
        use crate::coro::Task;
        use crate::scheduler::{park_current, waker_for_current, Scheduler, Waker};

        static PARKED: Mutex<Option<Waker>> = Mutex::new(None);
        static SECOND_RAN: AtomicUsize = AtomicUsize::new(0);
        static SECOND_SAW: AtomicUsize = AtomicUsize::new(usize::MAX);

        extern "C" fn parks_while_releasing(_object: *mut u8) {
            *PARKED.lock().unwrap() = waker_for_current();
            park_current();
        }
        extern "C" fn counts(_closure: *mut u8) {
            SECOND_RAN.fetch_add(1, Ordering::SeqCst);
        }

        let pool = Scheduler::new(1);
        pool.spawn(Task::new(|| {
            let object = khora_alloc(0, 0);
            // SAFETY: the only reference, and a callback that touches no field.
            unsafe { khora_drop(object, Some(parks_while_releasing)) };
        }));
        assert!(eventually(|| PARKED.lock().unwrap().is_some()), "the first fiber never parked");

        pool.spawn(Task::new(|| {
            let region = khora_region_open();
            // SAFETY: as in the test above.
            unsafe {
                khora_region_defer(region, closure(counts), None, None);
                khora_drop(region, Some(release_shim));
            }
            SECOND_SAW.store(SECOND_RAN.load(Ordering::SeqCst), Ordering::SeqCst);
        }));
        assert!(
            eventually(|| SECOND_SAW.load(Ordering::SeqCst) != usize::MAX),
            "the second fiber never finished"
        );
        assert_eq!(SECOND_SAW.load(Ordering::SeqCst), 1, "the region's release was queued behind a parked release");

        if let Some(waker) = PARKED.lock().unwrap().take() {
            waker.wake();
        }
        pool.drain();
    }

    /// **Both backends.** A region that ends inside a finalizer runs its own
    /// finalizers there, at the end of its scope, not after the finalizer
    /// that contains it has returned.
    ///
    /// No scheduler: this is the thread backend's form of the defect. Before
    /// the fix the inner release was queued behind the outer finalizer, so
    /// the outer one saw it not run, and a finalizer that then blocked (or a
    /// fiber handle released there, whose release is a join) never reached it.
    #[test]
    fn a_region_ended_inside_a_finalizer_runs_its_finalizers_at_once() {
        static INNER_RAN: AtomicUsize = AtomicUsize::new(0);
        static OUTER_SAW: AtomicUsize = AtomicUsize::new(usize::MAX);

        extern "C" fn inner(_closure: *mut u8) {
            INNER_RAN.fetch_add(1, Ordering::SeqCst);
        }
        extern "C" fn outer(_closure: *mut u8) {
            let region = khora_region_open();
            // SAFETY: as above.
            unsafe {
                khora_region_defer(region, closure(inner), None, None);
                khora_drop(region, Some(release_shim));
            }
            OUTER_SAW.store(INNER_RAN.load(Ordering::SeqCst), Ordering::SeqCst);
        }

        let region = khora_region_open();
        // SAFETY: as above.
        unsafe {
            khora_region_defer(region, closure(outer), None, None);
            khora_drop(region, Some(release_shim));
        }
        assert_eq!(INNER_RAN.load(Ordering::SeqCst), 1);
        assert_eq!(
            OUTER_SAW.load(Ordering::SeqCst),
            1,
            "the inner region's scope ended and its finalizer had not run yet"
        );
    }
}
