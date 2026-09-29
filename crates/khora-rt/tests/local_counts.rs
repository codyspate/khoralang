//! The runtime's counts with local counts on (`KHORA_RC_LOCAL=1`).
//!
//! **What these guard.** A program built with the switch tells the runtime
//! (`khora_rc_local`), and from then on `khora_dup`, `khora_drop`,
//! `khora_drop_reuse` and the drop glue that calls them count an object with
//! neither flag bit with a plain load and store, as generated code does.
//! Three things have to stay true: a local object is still counted exactly
//! (its last release frees it, once); a *shared* object is still counted
//! with the locked operation, or two threads lose updates; and a debug
//! build's owner check still runs on every runtime count of a local object,
//! since it is what reports a mark some entry forgot.
//!
//! A separate binary because the switch is process-wide and never turned
//! off: every test here runs with it on.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use khora_rt::{
    khora_alloc, khora_drop, khora_drop_reuse, khora_dup, khora_live_count, khora_rc_check_owners,
    khora_rc_local, khora_refcount, khora_reset_counters, khora_share, KHORA_FIELD_OFFSET,
};

/// Serializes the tests: the live-object counter is process-wide.
static RUNTIME: Mutex<()> = Mutex::new(());

/// How many times [`drop_cons`] ran.
static VISITS: AtomicUsize = AtomicUsize::new(0);

/// Runs `body` with local counts on, the counters reset and the runtime to
/// itself.
fn isolated(body: impl FnOnce()) {
    let _guard = RUNTIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    khora_rc_local();
    khora_reset_counters();
    VISITS.store(0, Ordering::Relaxed);
    body();
}

/// A cons cell's tag. Field 0 is its tail, or null.
const CONS_TAG: u32 = 1;

/// The drop glue of a cons cell: releases the tail through the runtime, as
/// every generated glue does.
extern "C" fn drop_cons(cell: *mut u8) {
    VISITS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: called on a live cons cell, whose field 0 is null or its tail.
    unsafe {
        let tail = *cell.add(KHORA_FIELD_OFFSET).cast::<*mut u8>();
        khora_drop(tail, Some(drop_cons));
    }
}

/// A list of `n` cells, built head-last as generated code builds one.
fn list(n: usize) -> *mut u8 {
    let mut head: *mut u8 = std::ptr::null_mut();
    for _ in 0..n {
        let cell = khora_alloc(8, CONS_TAG);
        // SAFETY: a fresh cell with one word of fields.
        unsafe { cell.add(KHORA_FIELD_OFFSET).cast::<*mut u8>().write(head) };
        head = cell;
    }
    head
}

/// How many threads count one object at once, and how often each. Enough
/// that the threads overlap even on a loaded machine: a mutant that counts
/// shared objects plainly passed one of these at 200 000 while the box ran
/// other builds, and failed it every time run alone.
const THREADS: usize = 4;
const EACH: usize = 1_000_000;

/// Runs `per_thread` on [`THREADS`] threads at once, released together.
fn at_once(per_thread: impl Fn() + Sync) {
    let ready = std::sync::Barrier::new(THREADS);
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                ready.wait();
                per_thread();
            });
        }
    });
}

/// **A local object is counted exactly by the runtime's plain path**: dups
/// and drops through `khora_dup` and `khora_drop` balance, the glue's child
/// releases free the tail, and the last release frees it all, once.
#[test]
fn a_local_list_is_freed_by_its_last_runtime_release() {
    isolated(|| {
        let base = khora_live_count();
        let head = list(1000);
        // SAFETY: `head` is live and this test holds its one reference.
        unsafe {
            for _ in 0..3 {
                khora_dup(head);
            }
            assert_eq!(khora_refcount(head), 4, "three runtime dups of a local object");
            for _ in 0..3 {
                khora_drop(head, Some(drop_cons));
            }
            assert_eq!(khora_refcount(head), 1, "three runtime drops of a local object");
            khora_drop(head, Some(drop_cons));
        }
        assert_eq!(khora_live_count(), base, "the last release freed the whole list");
        assert_eq!(VISITS.load(Ordering::Relaxed), 1000, "each cell's glue ran once");
    });
}

/// **A leaf is freed by its last release**, straight away rather than
/// through the drain, and a leaf inside a drain is freed too.
#[test]
fn a_leaf_is_freed_by_its_last_release() {
    isolated(|| {
        let base = khora_live_count();
        let leaf = khora_alloc(8, 0);
        // SAFETY: a fresh object with no references in it.
        unsafe {
            khora_dup(leaf);
            khora_drop(leaf, None);
            assert_eq!(khora_live_count(), base + 1, "a leaf with a holder left is kept");
            khora_drop(leaf, None);
        }
        assert_eq!(khora_live_count(), base, "the leaf's last release freed it");
    });
}

/// **The runtime's count of a local object takes the plain path** once the
/// program has asked for local counts.
///
/// What this guards: a runtime that ignored `khora_rc_local`. Every other
/// test here passes whether the local path is plain or locked, so without
/// this one the runtime half of the switch could stop switching and nothing
/// would notice. The only thing that tells the two apart is a race, so this
/// makes one on purpose: four threads duplicating an object that was never
/// shared, which is exactly the missed mark the owner check exists for (and
/// it is off here). A plain count loses some of the adds; a locked one
/// loses none. The objects are leaked, since their counts are wrong by
/// design.
///
/// **It costs a race that has to happen**, which a loaded machine may not
/// schedule, so it takes up to [`ROUNDS`] and fails only if no round lost
/// anything. One round lost updates every time it was run here, at load
/// 18. A machine with one CPU cannot overlap the threads at all, so it
/// skips.
#[test]
fn a_local_object_is_counted_without_a_lock() {
    const ROUNDS: usize = 20;
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 2 {
        return;
    }
    isolated(|| {
        let lost_one = (0..ROUNDS).any(|_| {
            let object = khora_alloc(8, 0) as usize;
            // SAFETY: the object is never freed, so every count is on a live
            // one. The concurrent plain counts of it are the point.
            at_once(|| unsafe {
                for _ in 0..EACH {
                    khora_dup(object as *mut u8);
                }
            });
            // SAFETY: still live; leaked on purpose.
            let count = unsafe { khora_refcount(object as *const u8) };
            count < 1 + (THREADS * EACH) as u64
        });
        assert!(
            lost_one,
            "{ROUNDS} rounds of {} concurrent dups of a local object all landed: the runtime locked them",
            THREADS * EACH
        );
    });
}

/// **A shared object is still counted with the locked add**, so four threads
/// duplicating it at once lose nothing.
#[test]
fn a_shared_object_keeps_every_concurrent_runtime_dup() {
    isolated(|| {
        let object = khora_alloc(8, 0);
        // SAFETY: a fresh leaf, held here.
        unsafe { khora_share(object, None) };
        let at = object as usize;
        // SAFETY: the object is held for the whole test.
        at_once(|| unsafe {
            for _ in 0..EACH {
                khora_dup(at as *mut u8);
            }
        });
        // SAFETY: still live, held here.
        let count = unsafe { khora_refcount(object) };
        assert_eq!(count, 1 + (THREADS * EACH) as u64, "a concurrent dup of a shared object was lost");
        // SAFETY: every reference counted above is released here.
        unsafe {
            for _ in 0..count {
                khora_drop(object, None);
            }
        }
    });
}

/// **A shared object is still counted with the locked subtract by
/// `khora_drop`**, so four threads releasing it at once lose nothing.
#[test]
fn a_shared_object_keeps_every_concurrent_runtime_drop() {
    isolated(|| {
        let object = khora_alloc(8, 0);
        // SAFETY: a fresh leaf, held here; the dups are what the threads
        // release.
        unsafe {
            khora_share(object, None);
            for _ in 0..THREADS * EACH {
                khora_dup(object);
            }
        }
        let at = object as usize;
        // SAFETY: each thread releases references counted above.
        at_once(|| unsafe {
            for _ in 0..EACH {
                khora_drop(at as *mut u8, None);
            }
        });
        // SAFETY: the one reference this test kept.
        unsafe {
            assert_eq!(khora_refcount(object), 1, "a concurrent drop of a shared object was lost");
            khora_drop(object, None);
        }
    });
}

/// **The same for `khora_drop_reuse`**, which releases like `khora_drop` and
/// answers null while other holders remain.
#[test]
fn a_shared_object_keeps_every_concurrent_reuse_release() {
    isolated(|| {
        let object = khora_alloc(8, 0);
        // SAFETY: as above.
        unsafe {
            khora_share(object, None);
            for _ in 0..THREADS * EACH {
                khora_dup(object);
            }
        }
        let at = object as usize;
        // SAFETY: each thread releases references counted above, never the
        // last, so no token comes back.
        at_once(|| unsafe {
            for _ in 0..EACH {
                let token = khora_drop_reuse(at as *mut u8, None);
                assert!(token.is_null(), "a token for an object others still hold");
            }
        });
        // SAFETY: the one reference this test kept.
        unsafe {
            assert_eq!(khora_refcount(object), 1, "a concurrent reuse release was lost");
            khora_drop(object, None);
        }
    });
}

/// Set in the copy of this binary that [`dies_in_a_copy`] starts.
const FATAL_CHILD: &str = "KHORA_RT_LOCAL_COUNTS_FATAL_CHILD";

/// Runs the test `name` in a copy of this binary with [`FATAL_CHILD`] set
/// to `case`, and requires the owner check's trap.
fn dies_in_a_copy(name: &str, case: &str) {
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(FATAL_CHILD, case)
        .output()
        .expect("the copy of the test binary should start");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(134), "the copy did not trap: {stderr}");
    assert!(
        stderr.contains("was counted on fiber") && stderr.contains("without being shared"),
        "the copy trapped for another reason: {stderr}"
    );
}

/// In the copy: the owner check on, a local object made on this thread's
/// fiber, and `count` applied to it on another thread's.
fn count_from_another_fiber(count: unsafe fn(*mut u8)) {
    khora_rc_check_owners();
    khora_rc_local();
    let object = khora_alloc(8, 0) as usize;
    // SAFETY: the object is live; the other thread counts it without a mark,
    // which is the missed crossing the check exists to report.
    std::thread::spawn(move || unsafe { count(object as *mut u8) })
        .join()
        .expect("the other thread");
}

/// **A runtime dup of a local object from a fiber that did not make it
/// traps** in a debug build, with local counts on.
#[test]
fn a_foreign_runtime_dup_traps() {
    if std::env::var(FATAL_CHILD).as_deref() == Ok("dup") {
        // SAFETY: see `count_from_another_fiber`.
        count_from_another_fiber(|p| unsafe { khora_dup(p) });
        return;
    }
    dies_in_a_copy("a_foreign_runtime_dup_traps", "dup");
}

/// **The same for a runtime drop**, the path drop glue takes for a child.
#[test]
fn a_foreign_runtime_drop_traps() {
    if std::env::var(FATAL_CHILD).as_deref() == Ok("drop") {
        // SAFETY: see `count_from_another_fiber`.
        count_from_another_fiber(|p| unsafe { khora_dup(p); khora_drop(p, None) });
        return;
    }
    dies_in_a_copy("a_foreign_runtime_drop_traps", "drop");
}
