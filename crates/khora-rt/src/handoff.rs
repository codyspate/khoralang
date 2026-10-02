//! A queue that gives values away: `std::core::Handoff<A>`.
//!
//! **What this prevents: two fibers holding one writable object.** A
//! `Channel` marks what it carries shared, so it can carry only `Share`
//! values: a record with a `mut` field, an `Array` or a `Map` sent over one
//! would be written by the sender and the receiver at once. A hand-off
//! carries them, because the sender keeps nothing. That is a claim about
//! every object the value reaches, and this module checks it at the send, by
//! reference count, and traps naming the type when it does not hold.
//!
//! # The test
//!
//! Every writable object the value reaches must be held only by the value:
//! its count must equal the number of references to it from inside the
//! value, the root's being the sender's own one, which the send hands to the
//! queue. If it does, no binding, stack slot or other object anywhere holds
//! it, on this fiber or on any other, since a local object is reachable only
//! from the fiber that made it (`crate::share`). The same fact is what
//! `khora_drop_reuse` rests on when it hands a cell back as local: \"true, not
//! merely allowed\".
//!
//! An uncounted borrow cannot defeat it. A borrow lives while the binding it
//! was borrowed from holds a counted reference, and that reference fails the
//! test.
//!
//! **A `Share` part may be held elsewhere.** It cannot be written, so a
//! second holder is a reader, and the part is marked shared exactly as a
//! channel send marks it. A `Share` part with a count of one is held only by
//! the value, and moves with it unmarked, so its counts stay local on the
//! receiver too.
//!
//! **A shared or immortal object ends the walk.** It is counted atomically
//! already, and nothing below a shared object is local (`crate::share`'s
//! invariant).
//!
//! # What the compiler supplies
//!
//! Which parts are `Share` is a fact about types, which the runtime does not
//! have. So the code generator emits, beside each type's drop glue, a
//! **hand-off glue**: it visits the type's fields with [`khora_handoff_visit`],
//! each with a [`HandoffType`] saying how that field's type is walked and what
//! it is called. The drop glue alone could only have said \"these are the
//! children\", and the prototype that used it had to guess, with a switch,
//! that an aliased descendant was `Share`.
//!
//! A closure is the exception: its captures are not in its type. They are
//! reached through its drop glue and tested as if each could be written,
//! which is sound and can refuse a closure that captured an aliased `Share`
//! value.
//!
//! # What it costs
//!
//! A walk of every local object the value reaches, at every send: a call into
//! the type's glue per writable object, a call back here per field that holds
//! a reference, and a header load per object. An object with a count of one
//! costs nothing more; one with a higher count is looked up in a small list
//! until the walk ends. Measured on a release build, a record shaped like a
//! PostgreSQL connection (two byte buffers, a `Map` holding one prepared
//! statement, a session, two short lists: about a dozen objects) costs about
//! 2,400 user instructions per send. The real connection has not been
//! measured on this walk. Receiving costs
//! nothing in a release build. A debug build walks the value again on each
//! side to move the owner check with it (below).
//!
//! # What it does not claim
//!
//! **Uniqueness as the counts see it at the send, not a static proof.** The
//! counts are only as exact as the compiler's last-use analysis. A value
//! still held by something the program no longer reads -- the value being
//! matched on, while the arm that bound the sent value runs -- fails the test
//! at run time, deterministically, on every send that has that shape. The
//! compiler cannot tell the program so in advance, and this cannot tell which
//! binding held the extra reference: the message says what the counts say.
//!
//! # The debug owner check travels with the value
//!
//! A debug build records which fiber made each local object and traps when
//! another fiber counts it (`crate::share`). A moved object has to change
//! owner, or the receiver's first count traps. The prototype cleared the
//! owner, and that turned the check off for every object that had ever
//! crossed -- so a walk that wrongly answered \"unique\" was invisible, which
//! is the one failure this check is there to see. Here a send stamps every
//! object it moves with an in-transit owner that no fiber has, and a receive
//! stamps it with the receiver. A sender that kept a reference the walk
//! missed traps on its next count of it, before or after the receive.

use super::*;
use crate::channel::{channel_of, dequeue, enqueue, open, WhenFull};
use crate::trap::on_which_fiber;
use std::cell::RefCell;
use std::sync::atomic::Ordering;

/// How a type's objects are walked. The compiler's word for it is the
/// discriminant, so this is `repr(u64)` and matched exhaustively.
#[repr(u64)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Walked {
    /// Can be written. Its count must equal its edges from inside the value,
    /// and `glue` is its hand-off glue, which visits its fields.
    Writable = 0,
    /// A `Share` type. Moved with the value when its count is one, marked
    /// when it is more. `glue` is its drop glue, through which its children
    /// are found, all `Share` in turn.
    Share = 1,
    /// A type whose parts the compiler cannot see: a closure. Tested as
    /// `Writable`, and walked through its drop glue, `glue`.
    Unseen = 2,
}

/// What the compiler tells the runtime about one type, once, in a constant.
///
/// # Layout
///
/// A contract with the code generator, which emits one of these per type
/// reached from a hand-off's `A`: five words, in this order.
#[repr(C)]
pub struct HandoffType {
    /// A [`Walked`] discriminant.
    pub walked: u64,
    /// Hand-off glue for a `Writable` type, `void glue(void *object, void
    /// *walk)`; drop glue for the other two, `void glue(void *object)`. Null
    /// for an object with nothing to visit.
    pub glue: *const u8,
    /// The type's drop glue: how it is marked, and how a queue releases it.
    pub drop: Option<extern "C" fn(*mut u8)>,
    /// The type's name as a program would write it, for the trap.
    pub name: *const u8,
    /// Its length in bytes.
    pub name_len: u64,
}

// SAFETY: every field is written once, by the code generator, into a constant
// in read-only memory; nothing ever writes one, so any thread may read it.
unsafe impl Sync for HandoffType {}

impl HandoffType {
    fn walked(&self) -> Walked {
        match self.walked {
            0 => Walked::Writable,
            1 => Walked::Share,
            2 => Walked::Unseen,
            // A number the compiler never writes: the safe reading is the
            // strictest, which can refuse a value but never passes a wrong one.
            _ => Walked::Unseen,
        }
    }

    fn name(&self) -> String {
        if self.name.is_null() || self.name_len == 0 {
            return String::from("a value");
        }
        // SAFETY: the compiler points `name` at `name_len` bytes of a string
        // constant, live for the whole run.
        let bytes = unsafe { std::slice::from_raw_parts(self.name, self.name_len as usize) };
        format!("`{}`", String::from_utf8_lossy(bytes))
    }
}

/// How one pending object is walked, and what to call it if it fails.
#[derive(Clone, Copy)]
enum Via {
    /// By the compiler's description of its type.
    Typed(&'static HandoffType),
    /// Reached through a `Share` object's drop glue: `Share` too.
    Share(Option<extern "C" fn(*mut u8)>),
    /// Reached through a closure's drop glue: tested as writable.
    Unseen(Option<extern "C" fn(*mut u8)>),
}

impl Via {
    fn walked(self) -> Walked {
        match self {
            Via::Typed(t) => t.walked(),
            Via::Share(_) => Walked::Share,
            Via::Unseen(_) => Walked::Unseen,
        }
    }

    fn drop_glue(self) -> Option<extern "C" fn(*mut u8)> {
        match self {
            Via::Typed(t) => t.drop,
            Via::Share(glue) | Via::Unseen(glue) => glue,
        }
    }

    fn name(self) -> String {
        match self {
            Via::Typed(t) => t.name(),
            Via::Share(_) => String::from("a value"),
            Via::Unseen(_) => String::from("a value a closure captured"),
        }
    }
}

/// An object reached more than once may be: its count, and the references
/// to it found inside the value so far.
struct Multi {
    object: usize,
    count: u64,
    edges: u64,
    via: Via,
}

/// One send's walk, reused across sends on a thread so a send allocates
/// nothing once the buffers have grown.
#[derive(Default)]
pub struct Walk {
    /// Objects still to visit.
    pending: Vec<(usize, Via)>,
    /// Writable objects with a count above one. See [`Multi`].
    multi: Vec<Multi>,
    /// `Share` objects held from outside the value, marked once the test
    /// has passed.
    marks: Vec<(usize, Option<extern "C" fn(*mut u8)>)>,
    /// Every local object that moves, when the owner check is on.
    moved: Vec<usize>,
    stamping: bool,
}

thread_local! {
    static WALK: RefCell<Option<Box<Walk>>> = const { RefCell::new(None) };
}

/// The owner a debug build writes on a value between its send and its
/// receive: the largest id the owner bits hold, which no fiber has until four
/// million have been made, and then only one in four million.
const IN_TRANSIT: u64 = KHORA_OWNER_MASK >> KHORA_OWNER_SHIFT;

impl Walk {
    fn clear(&mut self, stamping: bool) {
        self.pending.clear();
        self.multi.clear();
        self.marks.clear();
        self.moved.clear();
        self.stamping = stamping;
    }

    /// One reference to `object`, found inside the value.
    fn reach(&mut self, object: *mut u8, via: Via) {
        if object.is_null() {
            return;
        }
        // SAFETY: a reference inside a value the sender owns, so live.
        let word = unsafe { (*object.cast::<KhoraHeader>()).refcount.load(Ordering::Relaxed) };
        if word & (KHORA_SHARED | KHORA_IMMORTAL) != 0 {
            return;
        }
        let count = word & KHORA_COUNT_MASK;
        let at = object as usize;
        match via.walked() {
            Walked::Share => {
                // A count of one is this reference: nothing else reads it,
                // so it moves. More, and something outside may -- or the
                // value reaches it twice, which marking costs nothing to
                // handle either.
                if count == 1 {
                    self.moving(at, via);
                } else {
                    self.marks.push((at, via.drop_glue()));
                }
            }
            Walked::Writable | Walked::Unseen => {
                if count == 1 {
                    self.moving(at, via);
                } else if let Some(seen) = self.multi.iter_mut().find(|m| m.object == at) {
                    seen.edges += 1;
                } else {
                    self.multi.push(Multi { object: at, count, edges: 1, via });
                    self.moving(at, via);
                }
            }
        }
    }

    fn moving(&mut self, at: usize, via: Via) {
        self.pending.push((at, via));
        if self.stamping {
            self.moved.push(at);
        }
    }

    /// Visits everything reachable from what [`Self::reach`] queued.
    fn run(&mut self) {
        while let Some((at, via)) = self.pending.pop() {
            let object = at as *mut u8;
            match via {
                Via::Typed(t) => match t.walked() {
                    Walked::Writable => {
                        if t.glue.is_null() {
                            continue;
                        }
                        // SAFETY: the compiler wrote a `Writable` type's
                        // hand-off glue here, which reads `object`'s fields
                        // and calls back into `khora_handoff_visit` with
                        // `self`, and does nothing else.
                        let glue: extern "C" fn(*mut u8, *mut Walk) =
                            unsafe { std::mem::transmute(t.glue) };
                        glue(object, self);
                    }
                    Walked::Share => self.children(object, t.drop, Via::Share),
                    Walked::Unseen => self.children(object, t.drop, Via::Unseen),
                },
                Via::Share(glue) => self.children(object, glue, Via::Share),
                Via::Unseen(glue) => self.children(object, glue, Via::Unseen),
            }
        }
    }

    /// Queues what `object`'s drop glue releases, walked as `how` says.
    fn children(
        &mut self,
        object: *mut u8,
        glue: Option<extern "C" fn(*mut u8)>,
        how: fn(Option<extern "C" fn(*mut u8)>) -> Via,
    ) {
        let Some(glue) = glue else { return };
        for (child, child_glue) in crate::share::children(object, glue) {
            self.reach(child as *mut u8, how(child_glue));
        }
    }

    /// The first writable object held from outside the value, if any.
    fn outside(&self) -> Option<&Multi> {
        self.multi.iter().find(|m| m.edges != m.count)
    }
}

/// Visits one field of an object a hand-off glue is walking.
///
/// Called only from generated hand-off glue, with the `walk` it was given.
///
/// # Safety
///
/// `walk` must be the pointer the runtime passed the glue, `child` null or a
/// live object the walked object holds, and `described` a constant the
/// compiler emitted for `child`'s type.
#[unsafe(no_mangle)]
// SHARE: records a reference for the walk; publishes nothing.
pub unsafe extern "C" fn khora_handoff_visit(
    walk: *mut Walk,
    child: *mut u8,
    described: &'static HandoffType,
) {
    // SAFETY: the caller passes back the walk the runtime lent the glue,
    // which is live and not otherwise borrowed while the glue runs.
    let walk = unsafe { &mut *walk };
    walk.reach(child, Via::Typed(described));
}

/// Visits every element of an array a hand-off glue is walking.
///
/// `each` is generated for the element type: it reads the element at the
/// address it is given and visits what it holds. Pointer elements are handed
/// their slot, as inline ones are, so one shape of callback serves both.
///
/// # Safety
///
/// As [`khora_handoff_visit`], with `array` a live array whose elements are
/// the type `each` was generated for.
#[unsafe(no_mangle)]
// SHARE: records references for the walk; publishes nothing.
pub unsafe extern "C" fn khora_handoff_elements(
    walk: *mut Walk,
    array: *mut u8,
    each: extern "C" fn(*mut u8, *mut Walk),
) {
    if array.is_null() {
        return;
    }
    let (len, stride) = crate::array::shape(array);
    for index in 0..len {
        // SAFETY: every slot is inside the array, which the value holds.
        let slot = unsafe { crate::array::slot(array, index, stride) };
        each(slot, walk);
    }
}

/// Opens a hand-off that holds at most `capacity` values.
///
/// # Safety
///
/// `boxed` must say truthfully whether the values are pointers, `root`
/// describe their type, and `root.drop` release one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_handoff_open(
    capacity: i64,
    boxed: bool,
    root: &'static HandoffType,
) -> *mut u8 {
    open(capacity, WhenFull::Block, boxed, root.drop, Some(root))
}

/// Gives `value` away: tests that nothing outside it holds anything writable
/// it reaches, then queues it, waiting while the queue is full.
///
/// **Traps when the test fails, in every build.** Marking the value shared
/// instead, as a channel does, would leave two fibers writing one object with
/// atomic counts and nothing else between them -- a race the owner check
/// would not see, because the object would be shared. Answers false when the
/// hand-off is closed or this fiber is canceled while it waits, and the value
/// is then released here.
///
/// # Safety
///
/// `handle` must be a live hand-off, and `value` a live object the caller
/// owns and gives up.
#[unsafe(no_mangle)]
// SHARE: moves a graph nothing else holds, marking its aliased `Share` parts first.
pub unsafe extern "C" fn khora_handoff_send(handle: *mut u8, value: u64) -> bool {
    // SAFETY: `handle` is live, this function's documented precondition.
    let Some(channel) = (unsafe { channel_of(handle) }) else {
        fatal("sending on a hand-off that has already been released");
    };
    if let (true, Some(root)) = (channel.boxed, channel.handing) {
        // SAFETY: the caller gives up `value`, a live object of `root`'s type.
        unsafe { give_away(value as *mut u8, root) };
    }
    enqueue(channel, value)
}

/// Takes a value, waiting while the hand-off is empty. `false` once it is
/// closed and drained, or when this fiber is canceled while it waits.
///
/// # Safety
///
/// `handle` must be a live hand-off and `out` a writable word.
#[unsafe(no_mangle)]
// SHARE: hands out a value its send tested and marked; stores nothing.
pub unsafe extern "C" fn khora_handoff_receive(handle: *mut u8, out: *mut u64) -> bool {
    // SAFETY: `handle` is live, this function's documented precondition.
    let Some(channel) = (unsafe { channel_of(handle) }) else {
        fatal("receiving on a hand-off that has already been released");
    };
    // SAFETY: the caller guarantees a writable word.
    let got = unsafe { dequeue(channel, out) };
    if got && channel.boxed {
        if let Some(root) = channel.handing {
            // SAFETY: the word is the value the queue held, live and now the
            // caller's.
            unsafe { adopt(out.read() as *mut u8, root) };
        }
    }
    got
}

/// Tests `value` and prepares it to cross: traps if something outside it
/// holds a writable part, marks its aliased `Share` parts, and in a debug
/// build stamps what moves as in transit.
///
/// # Safety
///
/// `value` must be null or a live object of `root`'s type, owned by the
/// caller.
unsafe fn give_away(value: *mut u8, root: &'static HandoffType) {
    if value.is_null() {
        return;
    }
    let stamping = crate::share::checking();
    let mut walk = WALK.with(|w| w.borrow_mut().take()).unwrap_or_default();
    walk.clear(stamping);
    walk.reach(value, Via::Typed(root));
    walk.run();
    if let Some(held) = walk.outside() {
        if !trusting() {
            let (count, edges, name) = (held.count, held.edges, held.via.name());
            fatal_held(root, &name, count, edges);
        }
    }
    for &(at, glue) in &walk.marks {
        // SAFETY: reached from the live value, and `glue` is its drop glue.
        unsafe { crate::share::khora_share(at as *mut u8, glue) };
    }
    if stamping {
        for &at in &walk.moved {
            // SAFETY: reached from the live value; local, so no other fiber
            // writes this word.
            unsafe { stamp(at as *mut u8, IN_TRANSIT) };
        }
    }
    WALK.with(|w| *w.borrow_mut() = Some(walk));
}

/// Makes the calling fiber the owner of what a value moved, in a debug build.
/// Nothing in a release build, where no object records an owner.
///
/// Called on the receive, and wherever a queue releases a value it holds:
/// that release counts the objects on the releasing fiber.
///
/// # Safety
///
/// `value` must be null or a live object of `root`'s type that a hand-off
/// gave away and nothing has counted since.
pub(crate) unsafe fn adopt(value: *mut u8, root: &'static HandoffType) {
    if value.is_null() || !crate::share::checking() {
        return;
    }
    let mut walk = WALK.with(|w| w.borrow_mut().take()).unwrap_or_default();
    walk.clear(true);
    // Unshare nothing and test nothing: the send did both. This only finds
    // the same objects again.
    walk.reach(value, Via::Typed(root));
    walk.run();
    let owner = crate::share::owner_bits() >> KHORA_OWNER_SHIFT;
    for &at in &walk.moved {
        // SAFETY: as in `give_away`.
        unsafe { stamp(at as *mut u8, owner) };
    }
    WALK.with(|w| *w.borrow_mut() = Some(walk));
}

/// Writes `owner` into an object's owner bits.
///
/// # Safety
///
/// `object` must be live and local, so no other fiber writes its count.
unsafe fn stamp(object: *mut u8, owner: u64) {
    // SAFETY: the caller's contract.
    let count = unsafe { &(*object.cast::<KhoraHeader>()).refcount };
    let word = count.load(Ordering::Relaxed);
    count.store(
        (word & !KHORA_OWNER_MASK) | ((owner << KHORA_OWNER_SHIFT) & KHORA_OWNER_MASK),
        Ordering::Relaxed,
    );
}

/// The trap: a writable part of a value given away is held outside it.
///
/// **It cannot say which binding holds the extra reference**: a count is a
/// number, not a list of holders. So it names the type sent, the part that
/// failed, and the counts, and says what kind of thing holds it.
#[cold]
fn fatal_held(root: &HandoffType, part: &str, count: u64, edges: u64) -> ! {
    let whole = root.name();
    let which = if part == whole { String::from("it") } else { part.to_string() };
    let _ = std::io::stdout().flush();
    let _ = writeln!(
        std::io::stderr(),
        "khora: a `Handoff` send of {whole} found {which} still held outside the value: \
         {count} references, {edges} of them from inside it. Two fibers would hold one \
         writable object, so the program stops here. Everything writable in a value sent \
         must be held only by that value; something this fiber still uses after the send \
         holds it -- a binding read again later, or a value it was taken out of that is \
         read again later{}",
        on_which_fiber()
    );
    let _ = std::io::stderr().flush();
    std::process::exit(134)
}

/// Whether a debug build was asked to skip the trap: the mutation switch.
///
/// **What it is for: showing the owner check catches a walk that is wrong.**
/// With `KHORA_HANDOFF_MUTANT=unique` a send whose test fails queues the
/// value anyway, as a walk that wrongly answered \"unique\" would, and the
/// owner check must then trap on the sender's next count of what it kept.
/// `tests/handoff.rs` runs it. Honored only while the owner check is on, so a
/// release build never reads the variable and cannot be told to skip.
fn trusting() -> bool {
    use std::sync::atomic::AtomicU8;
    static MUTANT: AtomicU8 = AtomicU8::new(0);
    if !crate::share::checking() {
        return false;
    }
    match MUTANT.load(Ordering::Relaxed) {
        0 => {
            let yes = std::env::var("KHORA_HANDOFF_MUTANT").is_ok_and(|v| v == "unique");
            MUTANT.store(if yes { 2 } else { 1 }, Ordering::Relaxed);
            yes
        }
        known => known == 2,
    }
}
