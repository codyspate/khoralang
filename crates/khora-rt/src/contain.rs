//! Containing a trap at an export boundary.
//!
//! `docs/design/c-export.md` §8, and the correction to `docs/design/traps.md`
//! §4 that made it possible.
//!
//! A trap ends the process, and for a program that is right. For a **library**
//! it means taking down a host that never agreed to run a supervisor — a
//! Python interpreter, a Node runtime, somebody's editor — which is the one
//! support `traps.md` leaned on that does not apply here.
//!
//! # Why this can work here and not for a fiber
//!
//! `traps.md` §3 says containment needs an unwinder because Perceus leaves
//! live reference counts between the trap and the boundary, and dropping a
//! fiber without running them leaks everything it touched.
//!
//! An **export** escapes that argument by construction. Its signature is
//! scalars and `Ptr` in, scalars out; it cannot `raise`; and it cannot be
//! handed a capability, so it can reach no effect. There is no module-level
//! mutable binding to store a value in and nothing heap-allocated crosses the
//! signature either way, so **every allocation an exported call makes is
//! reachable only from its own stack**. Discarding all of them is therefore
//! sound, and needs no knowledge of what the stack held.
//!
//! # The one hole, and the guard over it
//!
//! A spawned fiber breaks the escape argument: it outlives the call and may
//! hold a reference to something the registry would free. So a spawn while
//! containment is armed **disarms it**, and that call traps the way it always
//! did. Refusing to contain is the safe direction; freeing an object a live
//! fiber is holding is not.
//!
//! # Off by default, and why
//!
//! A host that opted into nothing gets exactly today's behaviour. Containment
//! is a promise about what happens after a bug, and a promise nobody asked for
//! is one nobody has designed around — `khora_trapped()` returning non-zero is
//! only useful to a caller that checks it, and a caller that does not check it
//! would silently take a zero for an answer. So it is opt-in, per process,
//! through `khora_set_trap_policy`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

unsafe extern "C" {
    /// `csrc/guard.c`. The frame that owns the `jmp_buf` calls the body.
    pub(crate) safe fn khora_guard_armed() -> i32;
    /// Jumps to the landing point. Undefined unless armed.
    pub(crate) safe fn khora_guard_jump() -> !;
}

/// Whether the host has asked for containment. Process-wide, set once.
static POLICY: AtomicUsize = AtomicUsize::new(POLICY_ABORT);

/// A trap ends the process, which is the default and what `traps.md` decided.
const POLICY_ABORT: usize = 0;
/// A trap inside an exported call discards the call and returns to the host.
const POLICY_CONTAIN: usize = 1;

thread_local! {
    /// Every object allocated while a guarded call is on this thread's stack.
    ///
    /// `None` when no guarded call is running.
    static REGISTRY: RefCell<Option<Vec<*mut u8>>> = const { RefCell::new(None) };

    /// Whether [`REGISTRY`] holds a list, as a plain flag.
    ///
    /// Cheaper than `RefCell::try_borrow_mut`, which costs a load, a compare
    /// and two stores. Kept in step by `begin`, `end`, `discard` and `disarm`,
    /// which are the only four things that change the registry's shape.
    static ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Asks that a trap inside an exported call be contained rather than ending
/// the process.
///
/// `0` restores the default. Any other value asks for containment.
#[unsafe(no_mangle)]
pub extern "C" fn khora_set_trap_policy(contain: i32) {
    POLICY.store(
        if contain == 0 { POLICY_ABORT } else { POLICY_CONTAIN },
        Ordering::Relaxed,
    );
}

/// Whether the last exported call on this thread was discarded by a trap.
///
/// Stays set until [`khora_clear_trap`], the way `errno` does, because the
/// value an exported call returns after a trap is meaningless and a host has
/// to have somewhere to learn that from.
#[unsafe(no_mangle)]
pub extern "C" fn khora_trapped() -> i32 {
    TRAPPED.with(|t| i32::from(t.get()))
}

/// Clears what [`khora_trapped`] reports.
#[unsafe(no_mangle)]
pub extern "C" fn khora_clear_trap() {
    TRAPPED.with(|t| t.set(false));
}

thread_local! {
    static TRAPPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Records an allocation, if a guarded call is collecting them.
///
/// **The global check comes first, and that is the whole of the cost story.**
/// Both hooks sit on the path of every allocation in every program, including
/// the overwhelming majority that never export anything — so what matters is
/// what the uninvolved case pays.
///
/// A thread-local read is not it. Measured on a benchmark that does nothing
/// but allocate and free — two million iterations, two strings each — against
/// the same build with the hooks deleted: a thread-local guard cost **12%**,
/// and swapping the `RefCell` for a plain `Cell` changed nothing, because the
/// expense was the thread-local access and not the borrow check. Reading
/// `POLICY` first — a static that was already there — is a load from a fixed
/// address and a branch that is never taken, and brings it to **2.6%** on that
/// benchmark and proportionally less on any program that also does work.
///
/// The alternative considered and rejected was a second allocator symbol
/// emitted only by `--lib` builds. It would have missed every allocation the
/// runtime makes on the program's behalf, and so leaked exactly the objects
/// that are hardest to account for.
#[inline]
pub(crate) fn record(ptr: *mut u8) {
    if POLICY.load(Ordering::Relaxed) != POLICY_CONTAIN {
        return;
    }
    if !ACTIVE.with(|a| a.get()) {
        return;
    }
    REGISTRY.with(|r| {
        if let Ok(mut slot) = r.try_borrow_mut() {
            if let Some(list) = slot.as_mut() {
                list.push(ptr);
            }
        }
    });
}

/// Forgets an object that has been freed normally, so the registry does not
/// free it a second time.
///
/// A swap-remove from the tail: an object freed during a call is usually one
/// allocated recently, so the scan is short in the case that happens.
#[inline]
pub(crate) fn forget(ptr: *mut u8) {
    // The same order as `record`, for the same reason.
    if POLICY.load(Ordering::Relaxed) != POLICY_CONTAIN {
        return;
    }
    if !ACTIVE.with(|a| a.get()) {
        return;
    }
    REGISTRY.with(|r| {
        if let Ok(mut slot) = r.try_borrow_mut() {
            if let Some(list) = slot.as_mut() {
                if let Some(at) = list.iter().rposition(|p| *p == ptr) {
                    list.swap_remove(at);
                }
            }
        }
    });
}

/// Begins collecting allocations for a guarded call.
///
/// Returns whether collection actually started: it does not when the policy is
/// the default, and it does not for a nested call, whose allocations belong to
/// the outer registry and must be freed with it.
pub(crate) fn begin() -> bool {
    if POLICY.load(Ordering::Relaxed) != POLICY_CONTAIN {
        return false;
    }
    REGISTRY.with(|r| {
        let mut slot = r.borrow_mut();
        if slot.is_some() {
            return false;
        }
        *slot = Some(Vec::new());
        ACTIVE.with(|a| a.set(true));
        true
    })
}

/// Stops collecting, discarding the record. Called when a call *succeeded*:
/// its allocations are the caller's business now.
pub(crate) fn end() {
    ACTIVE.with(|a| a.set(false));
    REGISTRY.with(|r| {
        if let Ok(mut slot) = r.try_borrow_mut() {
            *slot = None;
        }
    });
}

/// Whether a trap on this thread can be contained.
pub(crate) fn can_contain() -> bool {
    khora_guard_armed() != 0 && REGISTRY.with(|r| r.borrow().is_some())
}

/// Frees everything the guarded call allocated, and stops collecting.
///
/// **Raw frees, no reference counting, no drop glue.** Every object in the
/// list was allocated during this call, so everything any of them points at is
/// also in the list and will be freed by its own entry. Running drop glue
/// instead would cascade into children that are then visited again — a double
/// free — and decrementing instead of freeing would leave a tree whose root is
/// gone. Freeing each exactly once is the operation that matches the invariant.
///
/// Returns how many objects were released, for the message.
pub(crate) fn discard() -> usize {
    ACTIVE.with(|a| a.set(false));
    let taken = REGISTRY.with(|r| r.borrow_mut().take());
    let Some(list) = taken else { return 0 };
    let freed = list.len();
    for ptr in list {
        // SAFETY: every pointer came from `khora_alloc` on this thread during
        // this call, is removed from the list by `forget` if it was freed
        // normally, and appears once. Nothing outside the call can hold one:
        // an export takes and returns only scalars, holds no capability, and
        // a spawn disarms containment before this can run.
        unsafe { crate::heap::release_raw(ptr) };
    }
    TRAPPED.with(|t| t.set(true));
    freed
}

/// Gives up on containing anything on this thread.
///
/// Called when a fiber is spawned: a fiber outlives the call that made it and
/// may hold a reference to something the registry would free, so the escape
/// argument no longer holds and the only safe answer is the old one.
pub(crate) fn disarm() {
    ACTIVE.with(|a| a.set(false));
    REGISTRY.with(|r| {
        if let Ok(mut slot) = r.try_borrow_mut() {
            *slot = None;
        }
    });
}

/// Runs one exported call with a landing point, if the host asked for one.
///
/// `body` is a per-export thunk the backend generates; `ctx` points at a
/// struct holding its arguments and its result. The return value is the
/// thunk's, or zero if the call was discarded — which the caller must not pass
/// on, and `khora_trapped()` is how it finds out.
///
/// **Not guarded when the policy is the default**, in which case this is a
/// direct call and costs a predictable branch. That matters because every
/// exported call goes through here whether or not anybody opted in.
///
/// # Safety
///
/// `body` must be a valid function pointer and `ctx` whatever it expects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_export_call(
    body: extern "C" fn(*mut u8) -> u64,
    ctx: *mut u8,
) -> u64 {
    if !begin() {
        return body(ctx);
    }
    let mut trapped: i32 = 0;
    // SAFETY: `body` and `ctx` are the caller's to vouch for, and `trapped` is
    // a live local for the duration of the call.
    let result = unsafe { khora_guarded_call(body, ctx.cast(), &raw mut trapped) };
    if trapped == 0 {
        end();
    }
    result
}

unsafe extern "C" {
    /// `csrc/guard.c`. Owns the `jmp_buf` in its own frame, which is the whole
    /// reason it is C.
    fn khora_guarded_call(
        body: extern "C" fn(*mut u8) -> u64,
        ctx: *mut std::ffi::c_void,
        trapped: *mut i32,
    ) -> u64;
}


/// Whether a host has asked for containment.
///
/// Read by the wrapper an export goes through, so that a program nobody opted
/// into pays a load and a predictable branch rather than a struct on the stack
/// and an indirect call. The feature is rare and the check is on the hot path
/// of every exported call, which is the wrong way round unless it is cheap.
#[unsafe(no_mangle)]
pub extern "C" fn khora_contain_enabled() -> i32 {
    i32::from(POLICY.load(Ordering::Relaxed) == POLICY_CONTAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guarded call that does not trap returns what the body returned, and
    /// leaves nothing behind.
    #[test]
    fn a_call_that_returns_normally_is_not_contained() {
        khora_set_trap_policy(1);
        khora_clear_trap();
        extern "C" fn body(_: *mut u8) -> u64 {
            7
        }
        // SAFETY: `body` takes no context and ignores its argument.
        let got = unsafe { khora_export_call(body, std::ptr::null_mut()) };
        assert_eq!(got, 7);
        assert_eq!(khora_trapped(), 0, "nothing trapped");
        khora_set_trap_policy(0);
    }

    /// The default is the old behaviour, and `begin` says so by declining.
    #[test]
    fn containment_is_off_unless_asked_for() {
        khora_set_trap_policy(0);
        assert!(!begin(), "the default policy collects nothing");
        assert!(!can_contain(), "and cannot contain");
    }

    /// A spawn gives up on containing the call it happened in.
    #[test]
    fn a_spawn_disarms_the_registry() {
        khora_set_trap_policy(1);
        assert!(begin(), "collecting");
        disarm();
        assert!(!can_contain(), "a spawn took containment off the table");
        end();
        khora_set_trap_policy(0);
    }

    /// **The flag and the registry must agree**, because the flag is what the
    /// allocation path reads and the registry is what holds the answer. Every
    /// one of the four functions that changes the registry's shape is exercised
    /// here, because a drift between them is either a leak or a lost object and
    /// neither would show up as a failure anywhere near the cause.
    #[test]
    fn the_fast_path_flag_tracks_the_registry() {
        let agrees = || {
            let flag = ACTIVE.with(|a| a.get());
            let has = REGISTRY.with(|r| r.borrow().is_some());
            assert_eq!(flag, has, "the flag and the registry disagree");
        };
        khora_set_trap_policy(1);
        agrees();
        assert!(begin());
        agrees();
        end();
        agrees();

        assert!(begin());
        disarm();
        agrees();

        assert!(begin());
        discard();
        agrees();
        khora_set_trap_policy(0);
        khora_clear_trap();
    }

    /// What the registry records and forgets, without allocating a real object.
    #[test]
    fn a_freed_object_is_not_freed_twice() {
        khora_set_trap_policy(1);
        assert!(begin());
        // Never dereferenced: what is under test is the bookkeeping, and a
        // real allocation would drag the whole heap into a unit test.
        let a = std::ptr::dangling_mut::<u8>();
        let b = std::ptr::without_provenance_mut::<u8>(2);
        record(a);
        record(b);
        forget(a);
        let left = REGISTRY.with(|r| r.borrow().as_ref().map(|l| l.len()));
        assert_eq!(left, Some(1), "the one that was freed normally is gone");
        end();
        khora_set_trap_policy(0);
    }
}
