//! The shared bit: what marks it, what clears it, and what reads past it.
//!
//! **What these guard.** A value another fiber can reach carries bit 63 of
//! its count word, set by `khora_share` on everything the value reaches. The
//! walk has to reach all of it without running out of stack, stop at what is
//! already marked, and leave the count itself alone. Every reader of the
//! count has to read past the bit, or a shared object's last reference looks
//! like one of many and the object leaks. And a reused cell has to come back
//! local, since its one holder is the fiber that reused it.
//!
//! The Khora-level tests that each crossing marks what it publishes are in
//! `khora-codegen-llvm`'s `sharing` tests. These are the runtime's half.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use khora_rt::{
    khora_alloc, khora_alloc_reuse, khora_drop, khora_drop_reuse, khora_dup, khora_is_shared,
    khora_live_count, khora_refcount, khora_reset_counters, khora_share, KHORA_FIELD_OFFSET,
};

/// Serializes the tests: the live-object counter is process-wide.
static RUNTIME: Mutex<()> = Mutex::new(());

/// How many times [`drop_cons`] ran.
static VISITS: AtomicUsize = AtomicUsize::new(0);

/// Runs `body` with the counters reset and the runtime to itself.
fn isolated(body: impl FnOnce()) {
    let _guard = RUNTIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    khora_reset_counters();
    VISITS.store(0, Ordering::Relaxed);
    body();
}

/// A cons cell's tag. Field 0 is its tail, or null.
const CONS_TAG: u32 = 1;

/// The drop glue of a cons cell: releases the tail. The shape the code
/// generator emits for a list, and what `khora_share` walks through.
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

/// Every cell of a list, head first.
fn cells(head: *mut u8) -> Vec<*mut u8> {
    let mut out = Vec::new();
    let mut at = head;
    while !at.is_null() {
        out.push(at);
        // SAFETY: a live cell of a list this test built.
        at = unsafe { *at.add(KHORA_FIELD_OFFSET).cast::<*mut u8>() };
    }
    out
}

/// **A million-cell list is marked without running out of stack**, and every
/// cell ends up shared with its count untouched.
///
/// A recursive walk is a frame per cell, which is past any thread's stack at
/// this depth. The time is printed, not asserted: it is the cost of sending a
/// fresh structure that size, once.
#[test]
fn a_million_cell_list_is_marked_without_recursion() {
    isolated(|| {
        const CELLS: usize = 1_000_000;
        let head = list(CELLS);
        let began = Instant::now();
        // SAFETY: `head` is live and this test holds its only reference.
        unsafe { khora_share(head, Some(drop_cons)) };
        let took = began.elapsed();
        eprintln!("marking {CELLS} cells took {took:?}");

        let all = cells(head);
        assert_eq!(all.len(), CELLS);
        for cell in &all {
            // SAFETY: every cell is live; the list holds each one.
            unsafe {
                assert!(khora_is_shared(*cell), "a cell the walk should have reached is local");
                assert_eq!(khora_refcount(*cell), 1, "marking changed a count");
            }
        }
        assert_eq!(khora_live_count(), CELLS, "marking freed something");
        assert_eq!(VISITS.load(Ordering::Relaxed), CELLS, "each cell is visited once");

        // Shared, so the last reference must still free all of it: this is
        // what an unmasked `previous > 1` would get wrong.
        // SAFETY: the only reference.
        unsafe { khora_drop(head, Some(drop_cons)) };
        assert_eq!(khora_live_count(), 0, "a shared list's last release freed nothing");
    });
}

/// **Marking what is already marked stops at the root**: one load, no visit.
///
/// Sending a structure a second time, or sending a new head on a shared tail,
/// is the common case for a server, and it must not cost the size of what was
/// sent before.
#[test]
fn marking_a_shared_list_again_stops_at_the_root() {
    isolated(|| {
        const CELLS: usize = 1_000_000;
        let head = list(CELLS);
        // SAFETY: live, and held by this test.
        unsafe { khora_share(head, Some(drop_cons)) };
        VISITS.store(0, Ordering::Relaxed);

        let began = Instant::now();
        // SAFETY: as above.
        unsafe { khora_share(head, Some(drop_cons)) };
        let took = began.elapsed();
        eprintln!("marking {CELLS} shared cells again took {took:?}");
        assert_eq!(VISITS.load(Ordering::Relaxed), 0, "the walk went past a shared root");

        // A new local head on the shared list: the walk marks the head and
        // stops at the first shared cell.
        let fresh = khora_alloc(8, CONS_TAG);
        // SAFETY: a fresh cell; it takes over this test's reference to `head`.
        unsafe { fresh.add(KHORA_FIELD_OFFSET).cast::<*mut u8>().write(head) };
        // SAFETY: live and held.
        unsafe { khora_share(fresh, Some(drop_cons)) };
        assert_eq!(VISITS.load(Ordering::Relaxed), 1, "only the new head is visited");
        // SAFETY: live, and this test's reference.
        unsafe {
            assert!(khora_is_shared(fresh));
            khora_drop(fresh, Some(drop_cons));
        }
        assert_eq!(khora_live_count(), 0);
    });
}

/// **Marking a shared object's children needs its count, not its word.**
/// A shared object released by two holders is freed by the second, exactly
/// once. The count word then has bit 63 set, and a decrement that compared
/// the whole word with 1 would call the last release "not the last".
#[test]
fn a_shared_object_is_freed_by_its_last_release() {
    isolated(|| {
        let head = list(3);
        // SAFETY: live and held; the dup gives a second reference.
        unsafe {
            khora_share(head, Some(drop_cons));
            khora_dup(head);
            assert_eq!(khora_refcount(head), 2, "the count read past the shared bit");
            khora_drop(head, Some(drop_cons));
            assert_eq!(khora_live_count(), 3, "the first of two releases freed it");
            khora_drop(head, Some(drop_cons));
        }
        assert_eq!(khora_live_count(), 0, "the last release of a shared object did not free it");
    });
}

/// **A reused cell is local again.** Reuse takes a cell only from its one
/// holder, so after `khora_alloc_reuse` nobody else can reach it, and keeping
/// the bit would send a local object down the shared path for good.
#[test]
fn a_reused_cell_is_local_again() {
    isolated(|| {
        let cell = khora_alloc(8, CONS_TAG);
        // SAFETY: a fresh cell with a null tail, held only here.
        unsafe {
            cell.add(KHORA_FIELD_OFFSET).cast::<*mut u8>().write(std::ptr::null_mut());
            khora_share(cell, Some(drop_cons));
            assert!(khora_is_shared(cell));

            let token = khora_drop_reuse(cell, Some(drop_cons));
            assert_eq!(token, cell, "a unique shared cell is handed out for reuse");
            let again = khora_alloc_reuse(token, 8, CONS_TAG);
            assert_eq!(again, cell, "the same memory");
            assert!(!khora_is_shared(again), "a reused cell kept the shared bit");
            assert_eq!(khora_refcount(again), 1);
            again.add(KHORA_FIELD_OFFSET).cast::<*mut u8>().write(std::ptr::null_mut());
            khora_drop(again, Some(drop_cons));
        }
        assert_eq!(khora_live_count(), 0);
    });
}

/// **Reuse is refused while a second holder exists**, shared bit or not. The
/// masked count is what `khora_drop_reuse` tests.
#[test]
fn a_shared_cell_with_two_holders_is_not_reused() {
    isolated(|| {
        let cell = list(1);
        // SAFETY: live and held; the dup is the second holder's reference.
        unsafe {
            khora_share(cell, Some(drop_cons));
            khora_dup(cell);
            let token = khora_drop_reuse(cell, Some(drop_cons));
            assert!(token.is_null(), "a cell somebody else holds was handed out for reuse");
            khora_drop(cell, Some(drop_cons));
        }
        assert_eq!(khora_live_count(), 0);
    });
}
