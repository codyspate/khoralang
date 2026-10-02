//! `Handoff`'s send, at the runtime's level: what moves, what is marked, and
//! what is counted.
//!
//! **What these guard.** A hand-off's send walks the value it gives away and
//! leaves the counts alone. A graph held only by itself must cross with no
//! object marked shared, so the receiver counts it as its own, plainly. A
//! `Share` part held from outside as well must be marked, as a channel marks
//! what it sends, or the two fibers holding it would count it plainly at
//! once. Nothing is freed or leaked by either.
//!
//! The type descriptions and hand-off glue here are written by hand, in the
//! shapes the code generator emits (`khora-codegen-llvm`'s
//! `backend/handoff.rs`). The trap, the owner check and both fiber backends
//! are in that crate's `handoff` tests, where a program can be built to trap.

use std::sync::Mutex;

use khora_rt::{
    khora_alloc, khora_channel_release, khora_drop, khora_dup, khora_handoff_open,
    khora_handoff_receive, khora_handoff_send, khora_handoff_visit, khora_is_shared,
    khora_live_count, khora_refcount, khora_reset_counters, HandoffType, Walk, Walked,
    KHORA_FIELD_OFFSET,
};

/// Serializes the tests: the live-object counter is process-wide.
static RUNTIME: Mutex<()> = Mutex::new(());

fn isolated(body: impl FnOnce()) {
    let _guard = RUNTIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    khora_reset_counters();
    body();
}

/// Field `index` of an object, as a pointer.
fn field(object: *mut u8, index: usize) -> *mut u8 {
    // SAFETY: every object here has the fields its tests read.
    unsafe { *object.add(KHORA_FIELD_OFFSET + 8 * index).cast::<*mut u8>() }
}

fn set(object: *mut u8, index: usize, value: *mut u8) {
    // SAFETY: as `field`; the object is fresh and has the room.
    unsafe { object.add(KHORA_FIELD_OFFSET + 8 * index).cast::<*mut u8>().write(value) };
}

// --- a `Share` list: `Cons(tail)` -------------------------------------------

extern "C" fn drop_cons(cell: *mut u8) {
    // SAFETY: a live cons cell, whose field 0 is null or its tail.
    unsafe { khora_drop(field(cell, 0), Some(drop_cons)) };
}

static LIST: HandoffType = HandoffType {
    walked: Walked::Share as u64,
    glue: std::ptr::null(),
    drop: Some(drop_cons),
    name: b"List".as_ptr(),
    name_len: 4,
};

fn list(n: usize) -> *mut u8 {
    let mut head = std::ptr::null_mut();
    for _ in 0..n {
        let cell = khora_alloc(8, 1);
        set(cell, 0, head);
        head = cell;
    }
    head
}

fn cells(mut at: *mut u8) -> Vec<*mut u8> {
    let mut out = Vec::new();
    while !at.is_null() {
        out.push(at);
        at = field(at, 0);
    }
    out
}

// --- a writable record: `{ mut next: Conn?, mut side: Conn?, tags: List }` --

extern "C" fn drop_conn(object: *mut u8) {
    // SAFETY: a live `Conn`, whose three fields are null or live.
    unsafe {
        khora_drop(field(object, 0), Some(drop_conn));
        khora_drop(field(object, 1), Some(drop_conn));
        khora_drop(field(object, 2), Some(drop_cons));
    }
}

/// The hand-off glue the compiler would emit for `Conn`: each field that
/// holds a reference, with its own type's description.
extern "C" fn visit_conn(object: *mut u8, walk: *mut Walk) {
    // SAFETY: called by the runtime's walk on a live `Conn`, with the walk it
    // lent; the descriptions are statics.
    unsafe {
        khora_handoff_visit(walk, field(object, 0), &CONN);
        khora_handoff_visit(walk, field(object, 1), &CONN);
        khora_handoff_visit(walk, field(object, 2), &LIST);
    }
}

static CONN: HandoffType = HandoffType {
    walked: Walked::Writable as u64,
    glue: visit_conn as *const u8,
    drop: Some(drop_conn),
    name: b"Conn".as_ptr(),
    name_len: 4,
};

fn conn(next: *mut u8, side: *mut u8, tags: *mut u8) -> *mut u8 {
    let object = khora_alloc(24, 0);
    set(object, 0, next);
    set(object, 1, side);
    set(object, 2, tags);
    object
}

/// A chain of `links` records, each with a list of `tags` cells, and every
/// object in it.
fn chain(links: usize, tags: usize) -> (*mut u8, Vec<*mut u8>) {
    let mut every = Vec::new();
    let mut head = std::ptr::null_mut();
    for _ in 0..links {
        let tags = list(tags);
        every.extend(cells(tags));
        head = conn(head, std::ptr::null_mut(), tags);
        every.push(head);
    }
    (head, every)
}

/// A hand-off handle's drop glue, as generated code passes it.
extern "C" fn drop_handoff(handle: *mut u8) {
    // SAFETY: called once, by `khora_drop`, on the last reference to a handle
    // `khora_handoff_open` made.
    unsafe { khora_channel_release(handle) };
}

/// Sends `value` through a fresh hand-off and receives it on this thread.
fn hand_over(value: *mut u8) -> *mut u8 {
    // SAFETY: `CONN` describes `value` and releases it; the handle is used
    // only here and released once; `value` is live and given up.
    unsafe {
        let handoff = khora_handoff_open(1, true, &CONN);
        assert!(khora_handoff_send(handoff, value as u64), "the send was refused");
        let mut out = 0u64;
        assert!(khora_handoff_receive(handoff, &mut out), "nothing arrived");
        khora_drop(handoff, Some(drop_handoff));
        out as *mut u8
    }
}

/// **A graph held only by itself crosses with nothing marked.** Twelve
/// objects, about the size of a connection: three records and nine list
/// cells. Every count is what it was and no object carries the shared bit,
/// so the receiver counts them all as its own, with a plain add.
#[test]
fn a_graph_held_only_by_itself_crosses_unmarked() {
    isolated(|| {
        let (root, every) = chain(3, 3);
        assert_eq!(every.len(), 12);
        let got = hand_over(root);
        assert_eq!(got, root, "the value that arrived is the value sent");
        for object in &every {
            // SAFETY: each is live; the graph holds it.
            unsafe {
                assert!(!khora_is_shared(*object), "a moved object was marked shared");
                assert_eq!(khora_refcount(*object), 1, "a send changed a count");
            }
        }
        assert_eq!(khora_live_count(), every.len(), "a send freed or made something");
        // SAFETY: the receiver's one reference.
        unsafe { khora_drop(got, Some(drop_conn)) };
        assert_eq!(khora_live_count(), 0, "the moved graph leaked");
    });
}

/// **A `Share` part held outside the value too is marked, and the value
/// still crosses.** The sender keeps a reference to one record's list; that
/// list is all marked shared, as a channel send marks, and nothing writable
/// is.
#[test]
fn an_aliased_share_part_is_marked_and_the_rest_moves() {
    isolated(|| {
        let (root, every) = chain(2, 4);
        let kept = field(root, 2);
        // SAFETY: a live list; this is the sender's extra reference to it.
        unsafe { khora_dup(kept) };
        let got = hand_over(root);
        let marked = cells(kept);
        for object in &every {
            // SAFETY: live, held by the graph.
            let shared = unsafe { khora_is_shared(*object) };
            assert_eq!(
                shared,
                marked.contains(object),
                "only the aliased list should be shared, and all of it"
            );
        }
        // SAFETY: the sender's reference, then the receiver's.
        unsafe {
            khora_drop(kept, Some(drop_cons));
            khora_drop(got, Some(drop_conn));
        }
        assert_eq!(khora_live_count(), 0, "a marked part leaked");
    });
}

/// **A writable object reached twice from inside, and from nowhere else,
/// moves.** `root.next` and `root.side` are the same record: its count is
/// two, both references are the value's own, so the test passes and nothing
/// is marked. A walk that treated any count above one as held outside
/// would refuse every diamond.
#[test]
fn a_writable_part_reached_twice_from_inside_moves() {
    isolated(|| {
        let both = conn(std::ptr::null_mut(), std::ptr::null_mut(), list(2));
        // SAFETY: live; the root takes a second reference.
        unsafe { khora_dup(both) };
        let root = conn(both, both, std::ptr::null_mut());
        let got = hand_over(root);
        // SAFETY: live, held twice by the root.
        unsafe {
            assert!(!khora_is_shared(both), "a diamond was marked");
            assert_eq!(khora_refcount(both), 2);
            khora_drop(got, Some(drop_conn));
        }
        assert_eq!(khora_live_count(), 0);
    });
}
