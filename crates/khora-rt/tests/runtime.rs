//! Exercises the runtime the way generated code will: raw pointers, explicit
//! offsets, and `dup`/`drop` written out by hand where the compiler will write
//! them out for real.
//!
//! Everything lives in one test binary on purpose. The allocation counters are
//! process-global, so a test asserting "nothing is live" would otherwise be
//! reading another test's objects — cargo runs the tests in a binary on several
//! threads at once. One binary means one owner for that state, and [`isolated`]
//! serializes access to it. New tests belong inside `isolated` even when they
//! do not look at the counters, because allocating at all perturbs them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use khora_rt::{
    khora_alloc, khora_alloc_count, khora_drop, khora_dup, khora_live_count, khora_print_bool,
    khora_print_int, khora_print_str, khora_refcount, khora_reset_counters, KHORA_FIELD_OFFSET,
    KHORA_HEADER_ALIGN, KHORA_HEADER_SIZE,
};

/// Serializes tests, since the runtime's counters are shared by all of them.
static RUNTIME: Mutex<()> = Mutex::new(());

/// Counts `drop_fields` calls, so a test can see the exact point at which an
/// object is torn down. Freeing is otherwise unobservable — reading the memory
/// afterwards to check would itself be undefined behavior.
static FIELD_DROPS: AtomicUsize = AtomicUsize::new(0);

/// Runs `body` with the runtime's global state reset and exclusively held.
fn isolated(body: impl FnOnce()) {
    // A panicking test poisons the lock; the state is reset on entry anyway, so
    // recovering keeps one failure from cascading into every later test.
    let _guard = RUNTIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    khora_reset_counters();
    FIELD_DROPS.store(0, Ordering::Relaxed);
    body();
}

/// Tag of a test object holding plain words and owning nothing.
const LEAF_TAG: u32 = 7;

/// Tag of a test object whose field 0 is an owned reference to another object.
const BOX_TAG: u32 = 9;

/// Bytes of fields in an object holding two machine words.
const TWO_WORDS: usize = 16;

/// `drop_fields` for an object that owns nothing: it only records that the
/// runtime reached the teardown step.
extern "C" fn record_teardown(_object: *mut u8) {
    FIELD_DROPS.fetch_add(1, Ordering::Relaxed);
}

/// `drop_fields` for a `BOX_TAG` object: releases the child in field 0.
///
/// This is the shape the code generator emits per ADT variant — offset past the
/// header, load the owned references, drop each one.
extern "C" fn drop_boxed_child(object: *mut u8) {
    FIELD_DROPS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the runtime calls this only for an object that is still allocated
    // and whose refcount has reached zero, so we have exclusive access to its
    // fields; `BOX_TAG` objects are built below with a child pointer in field 0.
    unsafe {
        let child = read_ptr_field(object, 0);
        khora_drop(child, None);
    }
}

/// Stores a word into field `index`, the way generated code addresses fields.
///
/// # Safety
///
/// `object` must be a live object with room for `index + 1` words of fields.
unsafe fn write_word_field(object: *mut u8, index: usize, value: u64) {
    // SAFETY: the caller guarantees the field exists, and fields begin at
    // `KHORA_FIELD_OFFSET` and are 8-aligned, so this stays inside the
    // allocation and is correctly aligned for a `u64`.
    unsafe { object.add(KHORA_FIELD_OFFSET).cast::<u64>().add(index).write(value) }
}

/// Reads back a word field.
///
/// # Safety
///
/// `object` must be a live object whose field `index` holds an initialized word.
unsafe fn read_word_field(object: *const u8, index: usize) -> u64 {
    // SAFETY: as `write_word_field`.
    unsafe { object.add(KHORA_FIELD_OFFSET).cast::<u64>().add(index).read() }
}

/// Stores an owned reference into field `index`.
///
/// # Safety
///
/// `object` must be a live object with room for `index + 1` words of fields.
unsafe fn write_ptr_field(object: *mut u8, index: usize, value: *mut u8) {
    // SAFETY: as `write_word_field`; a pointer is one machine word.
    unsafe { object.add(KHORA_FIELD_OFFSET).cast::<*mut u8>().add(index).write(value) }
}

/// Reads an owned reference out of field `index`.
///
/// # Safety
///
/// `object` must be a live object whose field `index` holds a reference stored
/// by [`write_ptr_field`].
unsafe fn read_ptr_field(object: *const u8, index: usize) -> *mut u8 {
    // SAFETY: as `write_ptr_field`.
    unsafe { object.add(KHORA_FIELD_OFFSET).cast::<*mut u8>().add(index).read() }
}

/// Allocates a `BOX_TAG` object owning `child`. The caller's reference to
/// `child` is transferred into the field.
fn box_of(child: *mut u8) -> *mut u8 {
    let parent = khora_alloc(8, BOX_TAG);
    // SAFETY: `parent` was just allocated with one word of fields.
    unsafe { write_ptr_field(parent, 0, child) };
    parent
}

#[test]
fn a_fresh_object_starts_with_one_reference() {
    isolated(|| {
        let object = khora_alloc(TWO_WORDS, LEAF_TAG);
        assert!(!object.is_null(), "khora_alloc must not hand back null");

        // SAFETY: `object` is live and we own the reference `khora_alloc` gave us.
        let refcount = unsafe { khora_refcount(object) };
        assert_eq!(refcount, 1, "a new object is owned by exactly its allocator");

        // SAFETY: as above; the object owns no references, so no drop_fields.
        unsafe { khora_drop(object, None) };
    });
}

#[cfg(target_pointer_width = "64")]
#[test]
fn the_header_matches_the_offsets_the_code_generator_reproduces() {
    isolated(|| {
        let object = khora_alloc(TWO_WORDS, LEAF_TAG);

        // Read the header by raw offset rather than through the runtime's own
        // accessors: the documented numbers are the contract with codegen, and
        // this test fails if they move.
        //
        // SAFETY: `object` is a live allocation of header plus 16 bytes, so
        // offsets 0, 8 and 12 are all inside it and correctly aligned for the
        // types read there.
        let (refcount, tag, field_bytes) = unsafe {
            (
                object.cast::<usize>().read(),
                object.add(8).cast::<u32>().read(),
                object.add(12).cast::<u32>().read(),
            )
        };

        assert_eq!(refcount, 1, "refcount lives at offset 0");
        assert_eq!(tag, LEAF_TAG, "tag lives at offset 8");
        assert_eq!(field_bytes, TWO_WORDS as u32, "field_bytes lives at offset 12");
        assert_eq!(KHORA_HEADER_SIZE, 16, "the header is 16 bytes on a 64-bit target");
        assert_eq!(KHORA_FIELD_OFFSET, 16, "fields start immediately after the header");
        assert_eq!(
            object as usize % KHORA_HEADER_ALIGN,
            0,
            "the allocation must be aligned for the fields that follow it"
        );

        // SAFETY: `object` is live and owned here.
        unsafe { khora_drop(object, None) };
    });
}

#[test]
fn fields_are_reached_by_offsetting_past_the_header() {
    isolated(|| {
        let object = khora_alloc(TWO_WORDS, LEAF_TAG);

        // SAFETY: `object` has two words of fields, so both indices are in
        // bounds, and we hold the only reference to it.
        let (first, second) = unsafe {
            write_word_field(object, 0, 0xdead_beef);
            write_word_field(object, 1, u64::MAX);
            (read_word_field(object, 0), read_word_field(object, 1))
        };

        assert_eq!(first, 0xdead_beef, "field 0 sits at KHORA_FIELD_OFFSET");
        assert_eq!(second, u64::MAX, "field 1 sits one word after field 0");

        // Writing the fields must not have disturbed the header.
        // SAFETY: `object` is live and owned here.
        assert_eq!(unsafe { khora_refcount(object) }, 1, "field stores must not touch the header");

        // SAFETY: as above.
        unsafe { khora_drop(object, None) };
    });
}

#[test]
fn fields_start_zeroed_so_a_half_built_object_is_still_safe_to_drop() {
    isolated(|| {
        // The object the code generator is midway through building: allocated,
        // fields not yet stored.
        let object = khora_alloc(TWO_WORDS, BOX_TAG);

        // SAFETY: `object` is live with two words of fields.
        let (first, second) = unsafe { (read_word_field(object, 0), read_word_field(object, 1)) };
        assert_eq!(first, 0, "field 0 must be zeroed by khora_alloc");
        assert_eq!(second, 0, "field 1 must be zeroed by khora_alloc");

        // Dropping it runs drop_fields over those zeros, which reach khora_drop
        // as nulls and are ignored. This is the pairing that makes an aborted
        // construction survivable rather than a wild free.
        //
        // SAFETY: `object` is live and owned; `drop_boxed_child` matches the
        // BOX_TAG layout, and field 0 holds a null child.
        unsafe { khora_drop(object, Some(drop_boxed_child)) };

        assert_eq!(khora_live_count(), 0, "the half-built object must still be freed");
    });
}

#[test]
fn dup_and_drop_move_the_refcount_by_exactly_one() {
    isolated(|| {
        let object = khora_alloc(8, LEAF_TAG);

        // SAFETY: every call below is made while we hold at least one reference
        // to a live `object`, which is what dup, drop and refcount require.
        unsafe {
            assert_eq!(khora_refcount(object), 1, "allocation yields one reference");

            khora_dup(object);
            assert_eq!(khora_refcount(object), 2, "dup increments");

            khora_dup(object);
            assert_eq!(khora_refcount(object), 3, "dup increments again");

            khora_drop(object, None);
            assert_eq!(khora_refcount(object), 2, "drop decrements");

            khora_drop(object, None);
            assert_eq!(khora_refcount(object), 1, "drop decrements again");

            khora_drop(object, None);
        }

        assert_eq!(khora_live_count(), 0, "the balanced sequence must free the object");
    });
}

#[test]
fn an_object_is_torn_down_exactly_when_its_last_reference_goes() {
    isolated(|| {
        let object = khora_alloc(8, LEAF_TAG);

        // SAFETY: `object` is live throughout, and we own both references by
        // the time the first drop runs.
        unsafe {
            khora_dup(object);

            khora_drop(object, Some(record_teardown));
            assert_eq!(
                FIELD_DROPS.load(Ordering::Relaxed),
                0,
                "drop_fields must not run while a reference remains"
            );
            assert_eq!(khora_live_count(), 1, "the object is still live at refcount 1");

            khora_drop(object, Some(record_teardown));
        }

        assert_eq!(
            FIELD_DROPS.load(Ordering::Relaxed),
            1,
            "drop_fields must run once, on the drop that reaches zero"
        );
        assert_eq!(khora_live_count(), 0, "reaching zero must free the object");
    });
}

#[test]
fn dropping_a_parent_drops_the_child_it_owns() {
    isolated(|| {
        let child = khora_alloc(8, LEAF_TAG);
        let parent = box_of(child);

        assert_eq!(khora_live_count(), 2, "parent and child are two allocations");

        // SAFETY: `parent` is live and solely owned here, and `drop_boxed_child`
        // is the correct field-dropping routine for a BOX_TAG object.
        unsafe { khora_drop(parent, Some(drop_boxed_child)) };

        assert_eq!(FIELD_DROPS.load(Ordering::Relaxed), 1, "the parent's drop_fields must run");
        assert_eq!(khora_live_count(), 0, "dropping the parent must free the child too");
    });
}

#[test]
fn a_child_two_parents_share_outlives_the_first_parent_dropped() {
    isolated(|| {
        // The reason `dup` exists: the second parent takes its own reference to
        // a child the first already owns.
        let child = khora_alloc(8, LEAF_TAG);
        // SAFETY: `child` is live and we own the reference being duplicated.
        unsafe { khora_dup(child) };

        let first = box_of(child);
        let second = box_of(child);
        assert_eq!(khora_live_count(), 3, "one child, two parents");

        // SAFETY: `first` is live and solely owned; its drop_fields matches its
        // layout and releases one of the child's two references.
        unsafe { khora_drop(first, Some(drop_boxed_child)) };

        // SAFETY: `child` is still live, held by `second`.
        assert_eq!(unsafe { khora_refcount(child) }, 1, "the surviving parent still owns the child");
        assert_eq!(khora_live_count(), 2, "the child must not be freed while shared");

        // SAFETY: `second` is live and solely owned.
        unsafe { khora_drop(second, Some(drop_boxed_child)) };
        assert_eq!(khora_live_count(), 0, "the last parent takes the child with it");
    });
}

#[test]
fn an_object_with_no_fields_round_trips() {
    isolated(|| {
        // A nullary ADT constructor: header only, distinguished by its tag.
        let object = khora_alloc(0, LEAF_TAG);
        assert!(!object.is_null(), "a zero-field object is still an allocation");

        // SAFETY: `object` is live and solely owned.
        unsafe {
            assert_eq!(khora_refcount(object), 1, "a zero-field object is refcounted like any other");
            khora_drop(object, None);
        }

        assert_eq!(khora_live_count(), 0, "a zero-field object must be freed");
    });
}

#[test]
fn dup_drop_and_refcount_tolerate_null() {
    isolated(|| {
        // SAFETY: null is explicitly permitted by all three functions.
        unsafe {
            khora_dup(std::ptr::null_mut());
            khora_drop(std::ptr::null_mut(), None);
            khora_drop(std::ptr::null_mut(), Some(drop_boxed_child));
            assert_eq!(khora_refcount(std::ptr::null()), 0, "null owns nothing");
        }

        assert_eq!(
            FIELD_DROPS.load(Ordering::Relaxed),
            0,
            "a null drop must not call drop_fields on nothing"
        );
        assert_eq!(khora_alloc_count(), 0, "null operations must not allocate");
        assert_eq!(khora_live_count(), 0, "null operations must not change the live count");
    });
}

#[test]
fn the_live_count_returns_to_zero_when_every_object_is_dropped() {
    isolated(|| {
        let objects: Vec<*mut u8> = (0..16).map(|tag| khora_alloc(TWO_WORDS, tag)).collect();
        assert_eq!(khora_alloc_count(), 16, "every allocation is counted");
        assert_eq!(khora_live_count(), 16, "none have been freed yet");

        for object in objects {
            // SAFETY: each pointer is a live object we own exactly one
            // reference to, and none of them owns anything.
            unsafe { khora_drop(object, None) };
        }

        assert_eq!(khora_live_count(), 0, "this is the phase 2 exit criterion in miniature");
        assert_eq!(khora_alloc_count(), 16, "freeing must not rewind the total");
    });
}

#[test]
fn the_live_count_catches_a_deliberate_leak() {
    isolated(|| {
        // Proves the counter can fail, rather than assuming a zero it would
        // report whatever we did. This is the shape of the bug the phase 2 exit
        // criterion is meant to catch: a missing drop for `leaked`.
        let kept = khora_alloc(8, LEAF_TAG);
        let leaked = khora_alloc(8, LEAF_TAG);

        // SAFETY: `kept` is live and solely owned; `leaked` is deliberately not
        // dropped here.
        unsafe { khora_drop(kept, None) };

        assert_eq!(khora_alloc_count(), 2, "both objects were allocated");
        assert_eq!(khora_live_count(), 1, "the leak must be visible in the live count");

        // Tidy up now the assertion has been made, so the leak does not follow
        // the rest of the suite around.
        // SAFETY: `leaked` is still live and we still hold its only reference.
        unsafe { khora_drop(leaked, None) };
        assert_eq!(khora_live_count(), 0, "and the count clears once the leak is fixed");
    });
}

#[test]
fn resetting_the_counters_clears_both_of_them() {
    isolated(|| {
        for _ in 0..3 {
            let object = khora_alloc(8, LEAF_TAG);
            // SAFETY: `object` is live and solely owned.
            unsafe { khora_drop(object, None) };
        }
        assert_eq!(khora_alloc_count(), 3, "three allocations happened");

        khora_reset_counters();

        assert_eq!(khora_alloc_count(), 0, "reset clears the total");
        assert_eq!(khora_live_count(), 0, "reset clears the live count");
    });
}

#[test]
fn the_counters_survive_allocation_from_several_threads() {
    isolated(|| {
        const THREADS: usize = 8;
        const PER_THREAD: usize = 250;

        // Each thread only ever touches its own objects, which is what makes
        // non-atomic refcounts sound; the counters are the one piece of shared
        // state, and this is what their atomics are for.
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..PER_THREAD {
                        let object = khora_alloc(TWO_WORDS, LEAF_TAG);
                        // SAFETY: this thread allocated `object` and holds its
                        // only reference; no other thread can observe it.
                        unsafe {
                            khora_dup(object);
                            khora_drop(object, None);
                            khora_drop(object, None);
                        }
                    }
                });
            }
        });

        assert_eq!(
            khora_alloc_count(),
            THREADS * PER_THREAD,
            "no increment may be lost to a race"
        );
        assert_eq!(khora_live_count(), 0, "every thread's objects must be freed");
    });
}

#[test]
fn printing_accepts_the_edges_of_every_type_it_supports() {
    isolated(|| {
        // Output content is checked end to end by the code generator's tests;
        // what matters here is that no input crosses this boundary badly.
        khora_print_int(0);
        khora_print_int(i64::MIN);
        khora_print_int(i64::MAX);
        khora_print_bool(true);
        khora_print_bool(false);

        let text = "khora — unicode is written through unvalidated";
        // SAFETY: the slice's bytes are initialized and live for the call, and
        // a zero length makes the null pointer unread.
        unsafe {
            khora_print_str(text.as_ptr(), text.len());
            khora_print_str(std::ptr::null(), 0);
        }

        assert_eq!(khora_live_count(), 0, "printing must not allocate a Khora object");
    });
}
