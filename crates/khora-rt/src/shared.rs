//! Shared cells: the one way a mutable value crosses into another fiber.
//!
//! A `Shared<A>` is a lock and a word. What makes it the *only* way is not
//! here but in the checker — `docs/design/sharing.md` — and what is here is the
//! part that has to be right whatever the checker allows: the value behind the
//! lock is released by a callback registered when the cell was opened, because
//! generated code cannot reach through the lock.
//!
//! # Why the lock is not a `std::sync::Mutex`
//!
//! **What it prevents: a valid program hanging on the scheduler because a
//! fiber waiting for a cell took its worker's thread with it.** A change
//! function runs under the lock and can be preempted there, or wait on a
//! channel there, and either way its fiber goes back onto some worker's queue
//! still holding the cell. A `Mutex` makes the next fiber that wants the cell
//! block the *thread* it is running on; once every worker's thread was blocked
//! that way -- one worker, or as many waiting fibers as workers -- nothing was
//! left to run the holder. A change function looping 2,000 times among six
//! fibers hung every run on one, two and four CPUs.
//!
//! So a fiber that finds the cell taken parks, and its worker runs something
//! else -- eventually the holder. A thread that is not a fiber on a worker
//! (the program's own `main`, a blocking-pool thread, a foreign thread, and
//! every fiber on the threads backend) has nothing to give back, and blocks
//! as it always did. See [`Lock`].

use super::*;
use crate::counters::COUNTER_ORDER;
use crate::heap::{khora_alloc, khora_drop, khora_dup};
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

/// What a `Shared<A>` holds.
///
/// The value as the one word every Khora value fits in, plus what is needed to
/// let go of it. The runtime cannot know `A`, so the drop routine and whether
/// the word is even a pointer are recorded once when the cell is opened rather
/// than passed to every operation.
struct Cell {
    value: u64,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
}

/// A cell, its lock, and the fiber currently changing it.
///
/// `holder` is outside the lock deliberately. It is not for excluding anyone —
/// the lock does that — but for saying *deadlock* out loud, and a check that
/// had to take the lock to read it would be the very thing it is trying to
/// report.
struct Held {
    holder: AtomicUsize,
    lock: Lock,
    /// Read and written only by whoever holds `lock`: see [`Taken`].
    cell: UnsafeCell<Cell>,
}

/// The tag every shared cell carries.
const SHARED_TAG: u32 = 0;

/// Nobody holds the cell.
const FREE: u8 = 0;
/// Somebody holds it, and nobody has enrolled to wait for it.
const TAKEN: u8 = 1;
/// Somebody holds it -- or it has been handed to a waiter that has not run
/// yet -- and the line may hold waiters, so giving it back has to look.
const CONTENDED: u8 = 2;

/// A cell's lock: a word, and a line of waiters used only when it is taken.
///
/// **Taken and given back with one compare-exchange each while nobody else
/// wants it**, which is what a `Mutex` cost, because `Shared::update` is on
/// request paths: a server's counter, a pool's tally.
///
/// **A fiber on a worker waits by parking**, so its worker goes on running
/// fibers, the holder among them. A thread that is not a fiber on a worker
/// waits on a condition variable of its own: it has no worker to give back.
///
/// **Giving it back frees it and wakes the first in line, and a fiber that is
/// running may take it before the woken one runs.** That is what a `Mutex`
/// does, and why: handing the cell to the woken waiter instead holds it for
/// somebody who is not running, so the fiber giving it back, coming round for
/// it again, has to park too, and every contended `update` becomes a park, a
/// wake and a switch in series. Eight fibers adding to one cell took 448 ms
/// on two workers that way, against 67 ms with a `Mutex`.
///
/// **But a waiter cannot lose for long.** One that is woken, finds the cell
/// taken, and has been waiting longer than [`STARVING`] goes back to the
/// front of the line marked starving, and the next giving back **hands** the
/// cell to it rather than freeing it: `handed` names it and the word stays
/// [`CONTENDED`], so nobody else can take it before it runs. Without that a
/// fiber woken onto a busy worker's queue could lose the cell to whoever was
/// running every time it came round -- the starvation the channel's `handed`
/// list stopped on a pool's idle connections. This is the rule Go's
/// `sync.Mutex` arrived at for the same two pressures.
///
/// What it costs: under contention, a `Mutex` around the line, and for a
/// thread a condition variable allocated per wait. A waiter waits behind
/// newcomers for at most about [`STARVING`], and is then served in its turn.
struct Lock {
    state: AtomicU8,
    line: std::sync::Mutex<Line>,
}

/// Who is waiting for a [`Lock`], and who it was handed to.
struct Line {
    /// Oldest first, except that a waiter which has lost the cell to a
    /// newcomer goes back to the front. An entry is taken out by the giving
    /// back that wakes it, or by its waiter when it leaves without the lock.
    waiting: std::collections::VecDeque<Waiting>,
    /// The waiter the lock was handed to and that has not yet come back to
    /// claim it, or 0. While this is set the lock is that waiter's.
    handed: usize,
}

/// One waiter in a [`Line`].
struct Waiting {
    /// The waiter's fiber id: the running fiber's for a fiber, the thread's
    /// root fiber's for a thread. Unique for the life of the process.
    id: usize,
    /// Hand it the lock rather than freeing it: it has lost the lock to
    /// newcomers for longer than [`STARVING`].
    starving: bool,
    how: Wakes,
}

/// How to wake a waiter.
enum Wakes {
    /// A fiber parked on the scheduler.
    #[cfg(not(target_family = "wasm"))]
    Fiber(crate::scheduler::Waker),
    /// A thread blocked on this condition variable, with the line's lock.
    #[cfg(not(target_family = "wasm"))]
    Thread(std::sync::Arc<std::sync::Condvar>),
}

#[cfg(not(target_family = "wasm"))]
impl Wakes {
    /// Wakes the waiter. Never with the line's lock held: a wake on the
    /// scheduler takes the pool's parking lock.
    fn wake(self) {
        match self {
            Wakes::Fiber(waker) => waker.wake(),
            Wakes::Thread(turn) => turn.notify_one(),
        }
    }
}

/// How many times a caller tries a taken lock again before it enrolls.
///
/// A change function is meant to be short, so a holder running on another
/// worker or thread usually lets go within a few hundred cycles, and a few
/// tries find it free without a park. What a `Mutex` does too. On one worker
/// the holder cannot be running, and this is about a microsecond wasted per
/// wait.
#[cfg(not(target_family = "wasm"))]
const SPINS: u32 = 100;

/// How long a waiter loses the lock to newcomers before it is handed it.
///
/// Go's `sync.Mutex` uses the same millisecond for its starvation mode.
/// Shorter, and a contended cell goes back to a park per take; longer, and a
/// waiter's worst case grows with it.
#[cfg(not(target_family = "wasm"))]
const STARVING: std::time::Duration = std::time::Duration::from_millis(1);

impl Lock {
    fn new() -> Lock {
        Lock {
            state: AtomicU8::new(FREE),
            line: std::sync::Mutex::new(Line { waiting: std::collections::VecDeque::new(), handed: 0 }),
        }
    }

    /// Takes the lock if nobody holds it.
    fn try_take(&self) -> bool {
        self.state.compare_exchange(FREE, TAKEN, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }

    /// Takes the lock, waiting as long as it takes, or until `give_up` says
    /// to stop asking. True when this caller holds it.
    ///
    /// `give_up` is asked only when there is a wait ahead, and never once the
    /// lock is this caller's: a cell free when asked for is taken whatever
    /// the caller's state, as an uncontended one always was.
    fn take(&self, cell: usize, give_up: &dyn Fn() -> bool) -> bool {
        self.try_take() || self.take_slowly(cell, give_up)
    }

    /// Gives the lock back, and wakes the first waiter if there is one.
    fn give_back(&self) {
        if self.state.compare_exchange(TAKEN, FREE, Ordering::Release, Ordering::Relaxed).is_ok() {
            return;
        }
        self.give_back_slowly();
    }

    #[cfg(not(target_family = "wasm"))]
    #[inline(never)]
    fn give_back_slowly(&self) {
        let woken = {
            let mut line = self.line.lock().unwrap_or_else(|e| e.into_inner());
            self.serve(&mut line)
        };
        if let Some(waiter) = woken {
            waiter.wake();
        }
    }

    /// Gives a lock the caller holds to the first in line if it is starving,
    /// and otherwise frees it and wakes the first in line, if anybody is
    /// there. Under the line's lock; the wake it answers is sent after.
    #[cfg(not(target_family = "wasm"))]
    fn serve(&self, line: &mut Line) -> Option<Wakes> {
        let Some(next) = line.waiting.pop_front() else {
            self.state.store(FREE, Ordering::Release);
            return None;
        };
        if next.starving {
            // Stays CONTENDED: it is that waiter's now. The line's lock
            // orders this caller's writes to the cell before the waiter's
            // reads, since it reads `handed` under the same lock.
            line.handed = next.id;
        } else {
            // Free, for the woken waiter or a newcomer to take. Whoever is
            // still in line is not forgotten: the woken waiter comes back to
            // the line's lock and either takes the cell -- marking it
            // CONTENDED if anybody is left -- or marks it CONTENDED and
            // enrolls again; and if it leaves instead, `pass_on` wakes the
            // next.
            self.state.store(FREE, Ordering::Release);
        }
        Some(next.how)
    }

    /// For a waiter that was woken and is leaving without the lock: makes
    /// sure somebody still serves the rest of the line, which until now was
    /// relying on this waiter coming back.
    #[cfg(not(target_family = "wasm"))]
    fn pass_on(&self, line: &mut Line) -> Option<Wakes> {
        while !line.waiting.is_empty() {
            match self.state.load(Ordering::Acquire) {
                // Whoever holds it will look at the line when they give it back.
                CONTENDED => return None,
                TAKEN => {
                    if self
                        .state
                        .compare_exchange(TAKEN, CONTENDED, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                    {
                        return None;
                    }
                }
                _ => {
                    // Free: take it on the next waiter's behalf and serve it,
                    // as a giving back would have.
                    if self
                        .state
                        .compare_exchange(FREE, CONTENDED, Ordering::Acquire, Ordering::Relaxed)
                        .is_ok()
                    {
                        return self.serve(line);
                    }
                }
            }
        }
        None
    }

    /// Everything [`Lock::take`] does once the first try failed.
    ///
    /// **Every look at the word happens under the line's lock**, and an
    /// enrolled waiter's entry is put there under the same lock that saw the
    /// cell taken and marked it [`CONTENDED`]. A giving back that frees it
    /// either ran before that look, and the look finds it free, or runs after
    /// and finds the word CONTENDED, so it takes the slow path, and the entry.
    /// That is `crate::wait`'s rule for a wake racing a wait, kept here by the
    /// same means.
    #[cfg(not(target_family = "wasm"))]
    #[inline(never)]
    fn take_slowly(&self, cell: usize, give_up: &dyn Fn() -> bool) -> bool {
        for _ in 0..SPINS {
            std::hint::spin_loop();
            if self.state.load(Ordering::Relaxed) == FREE && self.try_take() {
                return true;
            }
        }
        let fiber = crate::scheduler::waker_for_current();
        let me = running_fiber();
        let turn = match fiber {
            Some(_) => None,
            None => Some(std::sync::Arc::new(std::sync::Condvar::new())),
        };
        // When this caller first enrolled, once it has: an entry that has
        // gone from the line since was taken out by a giving back that freed
        // the cell and woke this caller.
        let mut since: Option<std::time::Instant> = None;
        let mut line = self.line.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if line.handed == me {
                line.handed = 0;
                if give_up() {
                    // Canceled while it was being handed over: the next in
                    // line gets it rather than a waiter that is leaving.
                    let woken = self.serve(&mut line);
                    drop(line);
                    if let Some(waiter) = woken {
                        waiter.wake();
                    }
                    return false;
                }
                return true;
            }
            let enrolled = line.waiting.iter().position(|w| w.id == me);
            let seen = self.state.load(Ordering::Acquire);
            // A caller that has waited and been told to stop leaves, free
            // cell or not: its change is for a caller that is stopping. One
            // that has not waited yet is checked only once there is a wait
            // ahead, below.
            let leaving = if since.is_some() { give_up() } else { seen != FREE && give_up() };
            if leaving {
                let woken = match enrolled {
                    Some(at) => {
                        line.waiting.remove(at);
                        None
                    }
                    // Woken and leaving: the rest of the line was counting on
                    // this caller to come back.
                    None if since.is_some() => self.pass_on(&mut line),
                    None => None,
                };
                drop(line);
                if let Some(waiter) = woken {
                    waiter.wake();
                }
                return false;
            }
            if seen == FREE {
                if let Some(at) = enrolled {
                    line.waiting.remove(at);
                }
                let now = if line.waiting.is_empty() { TAKEN } else { CONTENDED };
                // Under the line's lock nobody else serves it, but the fast
                // path still races this, so it is a compare-exchange.
                if self.state.compare_exchange(FREE, now, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    return true;
                }
                continue;
            }
            if seen == TAKEN
                && self
                    .state
                    .compare_exchange(TAKEN, CONTENDED, Ordering::Relaxed, Ordering::Relaxed)
                    .is_err()
            {
                // Given back on the fast path since the load. Look again.
                continue;
            }
            // Taken, and the holder will look at the line when it gives it
            // back.
            if enrolled.is_none() {
                let how = match (&fiber, &turn) {
                    (Some(waker), _) => Wakes::Fiber(waker.clone()),
                    (None, Some(turn)) => Wakes::Thread(turn.clone()),
                    (None, None) => unreachable!("a thread always has a turn to wait on"),
                };
                match since {
                    None => {
                        since = Some(std::time::Instant::now());
                        line.waiting.push_back(Waiting { id: me, starving: false, how });
                    }
                    // Woken with the cell free, and a newcomer took it first.
                    // It has waited longer than anybody behind it.
                    Some(at) => {
                        let starving = at.elapsed() >= STARVING;
                        line.waiting.push_front(Waiting { id: me, starving, how });
                    }
                }
            }
            match &turn {
                None => {
                    drop(line);
                    // Woken by a giving back, by a cancellation, or by
                    // nothing in particular: the loop looks again in every
                    // case, and an entry still in line keeps its place.
                    crate::scheduler::park_current_for(crate::scheduler::Why::Cell, cell);
                    line = self.line.lock().unwrap_or_else(|e| e.into_inner());
                }
                Some(turn) => {
                    // Registered so that a cancellation reaches the wait, and
                    // bounded for the one that lands between the look and
                    // the wait: `crate::channel::park_until_moved`'s reasons.
                    crate::current::current(|f| f.park_on(turn));
                    let (held, _) = turn
                        .wait_timeout(line, crate::channel::LOOK_AGAIN)
                        .unwrap_or_else(|e| e.into_inner());
                    line = held;
                    crate::current::current(|f| f.unpark_from());
                }
            }
        }
    }

    /// WebAssembly has one thread and no scheduler, so a taken lock can only
    /// be this caller's own -- a re-entry [`deny_reentry`] stops first.
    #[cfg(target_family = "wasm")]
    fn take_slowly(&self, _cell: usize, _give_up: &dyn Fn() -> bool) -> bool {
        fatal("a shared cell was taken on a target with one thread")
    }

    #[cfg(target_family = "wasm")]
    fn give_back_slowly(&self) {
        self.state.store(FREE, Ordering::Release);
    }
}

/// The lock on a cell, held: the one way to reach the cell's contents.
///
/// **Given back on every path out**, the cancellation tag's included, because
/// a lock left held is a cell every later caller waits on for ever.
struct Taken<'a>(&'a Held);

impl<'a> Taken<'a> {
    /// Waits for the cell, or answers `None` when `give_up` says to stop.
    fn of(held: &'a Held, give_up: &dyn Fn() -> bool) -> Option<Taken<'a>> {
        // `then`, not `then_some`: a guard built for a take that failed
        // would be dropped at once and give back a lock somebody else holds.
        held.lock.take(held as *const Held as usize, give_up).then(|| Taken(held))
    }

    fn cell(&mut self) -> &mut Cell {
        // SAFETY: this holds the lock, so nothing else reads or writes the
        // cell until it is dropped; the borrow is tied to `self`.
        unsafe { &mut *self.0.cell.get() }
    }
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        self.0.lock.give_back();
    }
}

/// Never gives up: `get` and `set` have no answer for having stopped
/// waiting, so they wait for the cell whatever the caller's state.
fn never() -> bool {
    false
}

/// Whether an `update` or `modify` waiting for the cell should stop waiting.
///
/// [`crate::current::Fiber::gives_up_joining`], and for its reason: like a
/// join, a wait for a cell has no "gave up" answer, so the caller leaves on
/// the cancellation tag, and inside a shielded finalizer that would cut
/// cleanup short on a plain cancel. So a plain cancel ends the wait in
/// ordinary code -- a change function's included -- and only `abort` ends it
/// in cleanup.
#[cfg(not(target_family = "wasm"))]
fn stops_waiting() -> bool {
    crate::current::current(|fiber| fiber.gives_up_joining())
}

#[cfg(target_family = "wasm")]
fn stops_waiting() -> bool {
    false
}

/// What `update` and `modify` answer when they gave up waiting.
#[cfg(not(target_family = "wasm"))]
const GAVE_UP: u32 = crate::fiber::CANCELED_WHICH;

#[cfg(target_family = "wasm")]
const GAVE_UP: u32 = u32::MAX;

/// Stops the program rather than letting a fiber wait for itself.
///
/// Every operation checks, not only `update`: the lock is held for the whole of
/// a change function, so a `get` or a `set` from inside one is the same
/// deadlock reached by a different door. Read without locking, which is why
/// `holder` lives outside the lock — a check that had to take the lock to
/// read it would be the very thing it is trying to report.
///
/// **Only a fiber waiting for itself.** Two fibers each inside a change
/// function and each waiting for the other's cell are not caught: they park
/// for ever, and the rest of the program goes on, since neither holds a
/// worker's thread.
fn deny_reentry(held: &Held, doing: &str) -> usize {
    // **The running fiber's id, not the running thread's.** These were the
    // same thing while a fiber was a thread, and stop being under M:N: a fiber
    // scheduled onto a worker whose previous occupant holds this lock would
    // read that occupant's id, match the recorded holder, and be killed for a
    // re-entry it never performed. `crate::current`.
    let me = running_fiber();
    if held.holder.load(COUNTER_ORDER) == me {
        fatal(&format!(
            "this fiber is inside `Shared::update` on this cell, so it cannot also {doing} it: \
             a change function runs under the lock, and reaching the same cell again would \
             wait for itself"
        ));
    }
    me
}

/// Releases a word, given what was recorded about it.
///
/// # Safety
///
/// `value` must be null or a live object matching `boxed` and `glue`.
unsafe fn release_word(value: u64, boxed: bool, glue: Option<extern "C" fn(*mut u8)>) {
    if boxed {
        // SAFETY: per the contract above.
        unsafe { khora_drop(value as *mut u8, glue) };
    }
}

/// The cell behind a handle, or `None` once it has been released.
///
/// # Safety
///
/// `cell` must be a live object from [`khora_shared_open`].
unsafe fn held_of<'a>(cell: *mut u8) -> Option<&'a Held> {
    if cell.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_shared_open` wrote there. Shared rather than exclusive: a cell is
    // `Share`, so another fiber may be inside it at this moment.
    unsafe { (*cell.add(KHORA_FIELD_OFFSET).cast::<*mut Held>()).as_ref() }
}

/// Marks a cell's word shared, when the word is a counted object.
///
/// # Safety
///
/// When `boxed`, `value` must be null or a live object the caller holds and
/// `glue` its release routine.
unsafe fn share_word(value: u64, boxed: bool, glue: Option<extern "C" fn(*mut u8)>) {
    if boxed {
        // SAFETY: per this function's contract.
        unsafe { crate::share::khora_share(value as *mut u8, glue) };
    }
}

/// Opens a cell holding `value`.
///
/// Takes ownership of the value: the cell releases it when the cell goes.
///
/// # Safety
///
/// `value` must be null or a live Khora object when `boxed`, and `glue` its
/// drop routine.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_open(
    value: u64,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Held>() as u64, SHARED_TAG);
    crate::share::born_shared(object);
    // A cell is `Share`, so what it holds is reachable from every fiber the
    // cell reaches.
    // SAFETY: the caller hands over a live value whose release is `glue`.
    unsafe { share_word(value, boxed, glue) };
    let held: Box<Held> = Box::new(Held {
        holder: AtomicUsize::new(0),
        lock: Lock::new(),
        cell: UnsafeCell::new(Cell { value, boxed, glue }),
    });
    // SAFETY: `khora_alloc` returned an object with one field's worth of space,
    // zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Held>().write(Box::into_raw(held));
    }
    object
}

/// The value in the cell, as a new reference.
///
/// Duplicated *under the lock*: between reading the word and claiming a
/// reference to it, another fiber's `set` could otherwise take the last one and
/// free it.
///
/// # Safety
///
/// `cell` must be a live object from [`khora_shared_open`].
#[unsafe(no_mangle)]
// SHARE: hands out a value the entry that stored it already marked; stores nothing.
pub unsafe extern "C" fn khora_shared_get(cell: *mut u8) -> u64 {
    // SAFETY: `cell` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(held) = (unsafe { held_of(cell) }) else {
        fatal("reading a shared cell that has already been released");
    };
    deny_reentry(held, "read");
    let Some(mut taken) = Taken::of(held, &never) else {
        unreachable!("`never` never gives up")
    };
    let cell = taken.cell();
    if cell.boxed {
        // SAFETY: the cell has held a reference to this since it was stored.
        unsafe { khora_dup(cell.value as *mut u8) };
    }
    cell.value
}

/// Replaces the value, releasing the one that was there.
///
/// The old value is let go of *after* the lock, because releasing it runs its
/// drop routine — which may reach a cell of its own, and a lock held across
/// that is a lock ordering nobody agreed to.
///
/// # Safety
///
/// `cell` must be live, and `value` a live object owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_set(cell: *mut u8, value: u64) {
    // SAFETY: `cell` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(held) = (unsafe { held_of(cell) }) else {
        fatal("writing a shared cell that has already been released");
    };
    deny_reentry(held, "write");
    let (old, boxed, glue) = {
        let Some(mut taken) = Taken::of(held, &never) else {
            unreachable!("`never` never gives up")
        };
        let cell = taken.cell();
        // Marked under the lock, before the store that publishes it.
        // SAFETY: the caller owns `value`, and the cell's glue releases one.
        unsafe { share_word(value, cell.boxed, cell.glue) };
        let old = std::mem::replace(&mut cell.value, value);
        (old, cell.boxed, cell.glue)
    };
    // SAFETY: the cell owned this reference and has just given it up.
    unsafe { release_word(old, boxed, glue) };
}

/// How generated code hands over a change function.
///
/// The closure's own parameter and result are `A`, which has no single machine
/// type, so the shim converting them to and from the one word every Khora value
/// fits in is emitted per instantiation on the other side of the boundary.
/// Only scalars and pointers cross here, as everywhere else.
///
/// **The shim returns the change function's cancellation tag** and writes the
/// new value through the pointer only when the tag is zero. A change function
/// that came back with a tag computed nothing, and the word it would have
/// written is a zero or a null nobody produced: see [`khora_shared_update`].
type Change = extern "C" fn(*const u8, *mut u8, u64, *mut u64) -> u32;

/// Reads, transforms and writes, all as one step.
///
/// `change` is called with the current value and its result becomes the new
/// one. It runs **once**, under the lock, which is what makes the read and the
/// write atomic against every other fiber — and what makes a change function
/// that updates the cell it is changing a deadlock, reported here rather than
/// waited out.
///
/// **`change` cannot fail, and nothing inside it stops.** It runs
/// [`crate::cancel::Pinned`]: no cancellation point in it acts, and a blocking
/// call in it gives up with its "gave up" answer once the fiber is canceled.
/// Work that can fail belongs outside: compute it, then `set` the answer.
///
/// **One thing can still come back without an answer: a `Fiber::join`, `wait`
/// or `outcome` inside `change`** whose child was stopped, or which gave up
/// because this fiber was. Those have no "gave up" value to carry on with, so
/// the change function leaves on its cancellation tag. Then the change did not
/// happen: the cell keeps the value it had, the lock is let go, and the tag is
/// returned for the caller to leave on. Without that the cell would hold the
/// zero -- for a `String`, a null the next read crashes on -- and the fiber
/// would carry on as if it had not been stopped. The argument the change
/// function was given is its own, and its unwind released it; the cell's
/// reference was never handed over, so there is nothing to put back.
///
/// **A wait for the cell gives up the same way.** A caller canceled while
/// another fiber holds the cell stops waiting ([`stops_waiting`]) and gets
/// the tag back with `change` never called.
///
/// Writes the value the cell ended up holding through `out`, as a new
/// reference, so a caller can see what it did without a second read that
/// another fiber could get between. Returns 0, or the tag; `out` is not written
/// when the tag is not 0.
///
/// # Safety
///
/// `cell` must be live, `change` a live Khora closure of type `(A) -> A`
/// borrowed for the call, `call` the shim matching it, and `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_update(
    cell: *mut u8,
    change: *mut u8,
    call: Change,
    out: *mut u64,
) -> u32 {
    // SAFETY: `cell` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(held) = (unsafe { held_of(cell) }) else {
        fatal("updating a shared cell that has already been released");
    };

    let me = deny_reentry(held, "update");
    let Some(mut taken) = Taken::of(held, &stops_waiting) else {
        return GAVE_UP;
    };
    held.holder.store(me, COUNTER_ORDER);
    let cell = taken.cell();
    let (boxed, glue) = (cell.boxed, cell.glue);

    // The change function takes its argument owned, like every other Khora
    // parameter, so it gets a reference of its own and consumes it.
    if boxed {
        // SAFETY: the cell has held a reference to this since it was stored.
        unsafe { khora_dup(cell.value as *mut u8) };
    }

    // SAFETY: the caller guarantees a live closure and a matching shim. A
    // closure's first field is its code pointer, which is the convention
    // generated code uses to call one.
    let mut produced: u64 = 0;
    let which = unsafe {
        let _pinned = crate::cancel::Pinned::new();
        let code = *change.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
        call(code, change, cell.value, &raw mut produced)
    };
    if which != 0 {
        // The change did not happen. The cell still owns what it held.
        held.holder.store(0, COUNTER_ORDER);
        return which;
    }

    // What `change` returned was made on this fiber and is about to be the
    // cell's. Marked before the store, under the lock.
    // SAFETY: the change function handed over a live value of the cell's type.
    unsafe { share_word(produced, boxed, glue) };
    let old = std::mem::replace(&mut cell.value, produced);
    if boxed {
        // SAFETY: still under the lock, so nothing can have taken this. The
        // caller gets a reference of its own to what is now in there.
        unsafe { khora_dup(produced as *mut u8) };
    }
    held.holder.store(0, COUNTER_ORDER);
    drop(taken);

    // SAFETY: the cell owned this and has just given it up. Outside the lock,
    // because a drop routine can reach a cell of its own.
    unsafe { release_word(old, boxed, glue) };
    // SAFETY: the caller guarantees `out` is writable.
    unsafe { *out = produced };
    0
}

/// How generated code hands over a change function that also answers.
///
/// Two words come back where [`Change`] has one, and only scalars cross here,
/// so the new state and the answer are both written through pointers and the
/// tag is returned — the same shape as [`Change`], for the same reason.
type Modify = extern "C" fn(*const u8, *mut u8, u64, *mut u64, *mut u64) -> u32;

/// Reads, transforms, writes, and gives back something that is not the state.
///
/// [`khora_shared_update`] can only answer with what it left in the cell, and
/// that is not always what the caller needs to know. A handler that inserts a
/// record under a generated key has to get *that* key back; searching the new
/// state for it afterwards is a guess, and a wrong one as soon as two fibers
/// insert records that look alike.
///
/// So the change function returns both, and both are installed and handed back
/// under the one lock.
///
/// A change function that comes back with a cancellation tag changes nothing,
/// exactly as in [`khora_shared_update`]: the cell keeps its value, the lock is
/// let go, `answer` is not written, and the tag is returned. So does a caller
/// that gave up waiting for the cell, without calling `change` at all.
///
/// # Safety
///
/// `cell` must be live, `change` a live Khora closure borrowed for the call,
/// `call` the shim matching it, and `answer` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_shared_modify(
    cell: *mut u8,
    change: *mut u8,
    call: Modify,
    answer: *mut u64,
) -> u32 {
    // SAFETY: `cell` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(held) = (unsafe { held_of(cell) }) else {
        fatal("updating a shared cell that has already been released");
    };

    let me = deny_reentry(held, "update");
    let Some(mut taken) = Taken::of(held, &stops_waiting) else {
        return GAVE_UP;
    };
    held.holder.store(me, COUNTER_ORDER);
    let cell = taken.cell();
    let (boxed, glue) = (cell.boxed, cell.glue);

    if boxed {
        // SAFETY: the cell has held a reference to this since it was stored.
        unsafe { khora_dup(cell.value as *mut u8) };
    }

    let mut produced_answer: u64 = 0;
    let mut produced: u64 = 0;
    // SAFETY: the caller guarantees a live closure and a matching shim.
    let which = unsafe {
        let _pinned = crate::cancel::Pinned::new();
        let code = *change.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
        call(code, change, cell.value, &raw mut produced, &raw mut produced_answer)
    };
    if which != 0 {
        held.holder.store(0, COUNTER_ORDER);
        return which;
    }

    // As in `khora_shared_update`. The answer is not stored, so it stays local.
    // SAFETY: the change function handed over a live value of the cell's type.
    unsafe { share_word(produced, boxed, glue) };
    let old = std::mem::replace(&mut cell.value, produced);
    held.holder.store(0, COUNTER_ORDER);
    drop(taken);

    // SAFETY: the cell owned this and has just given it up. Outside the lock,
    // because a drop routine can reach a cell of its own.
    unsafe { release_word(old, boxed, glue) };
    // SAFETY: the caller guarantees `answer` is writable.
    unsafe { *answer = produced_answer };
    0
}

/// Releases a cell and the value in it.
///
/// This is a `drop_fields` callback: [`khora_drop`] calls it when the last
/// reference to the handle goes.
///
/// # Safety
///
/// `cell` must be a live object from [`khora_shared_open`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
// SHARE: releases; publishes nothing.
pub unsafe extern "C" fn khora_shared_release(cell: *mut u8) {
    if cell.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live handle; the field holds what
    // `khora_shared_open` wrote, and nothing else reads it after this. Every
    // caller that waited for the lock held a reference to the handle while it
    // waited, so none is left in line.
    unsafe {
        let slot = cell.add(KHORA_FIELD_OFFSET).cast::<*mut Held>();
        let held = *slot;
        if held.is_null() {
            return;
        }
        slot.write(std::ptr::null_mut());

        let held = Box::from_raw(held);
        let cell = held.cell.into_inner();
        release_word(cell.value, cell.boxed, cell.glue);
    }
}

/// The running fiber's id, or zero where there are no fibers.
///
/// WebAssembly is single-threaded and has no scheduler, so every caller *is*
/// the same one. Re-entry is still caught — the holder is recorded and
/// compared — it is simply always the same identity doing the holding, which
/// is the truth on that target rather than a weakening of the check.
#[cfg(not(target_family = "wasm"))]
fn running_fiber() -> usize {
    crate::current::current(|fiber| fiber.id())
}

#[cfg(target_family = "wasm")]
fn running_fiber() -> usize {
    1
}

#[cfg(test)]
mod tests {
    //! The cell's lock, on the scheduler and off it.
    //!
    //! The scheduler tests have **one worker**, the shape that hung: a waiter
    //! that blocked its worker's thread left nobody to run the holder. A test
    //! that goes wrong fails after ten seconds and leaks its pool, rather than
    //! hanging in a join of a worker that is never coming back.

    use super::*;
    use crate::coro::Task;
    use crate::current::{Fiber, Stop};
    use crate::fiber::CANCELED_WHICH;
    use crate::scheduler::{park_current, waker_for_current, Scheduler, Waker};
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// What a change function does here: the value in, the value out or a tag.
    type Body = Box<dyn Fn(u64) -> Result<u64, u32> + Send + Sync>;

    /// Where a waiter's `update` leaves what it answered, once it has.
    type Slot = Mutex<Option<Result<u64, u32>>>;

    /// The shim generated code would emit, calling a [`Body`] whose address
    /// the closure object carries where generated code keeps a code pointer.
    extern "C" fn shim(code: *const u8, _closure: *mut u8, value: u64, out: *mut u64) -> u32 {
        // SAFETY: `change` stores a leaked `Body` there, and nothing frees it.
        let body = unsafe { &*code.cast::<Body>() };
        match body(value) {
            Ok(next) => {
                // SAFETY: the runtime passes a writable word.
                unsafe { out.write(next) };
                0
            }
            Err(tag) => tag,
        }
    }

    /// A change-function closure object running `body`. Leaked: tests only.
    fn change(body: impl Fn(u64) -> Result<u64, u32> + Send + Sync + 'static) -> usize {
        let code = Box::into_raw(Box::new(Box::new(body) as Body));
        let object = khora_alloc(std::mem::size_of::<*const u8>() as u64, 0);
        // SAFETY: one field's worth of fresh space that nothing else holds.
        unsafe { object.add(KHORA_FIELD_OFFSET).cast::<*const u8>().write(code.cast()) };
        object as usize
    }

    /// A cell holding an `Int`. Leaked: tests only.
    fn cell(value: u64) -> usize {
        // SAFETY: an unboxed word needs no glue.
        unsafe { khora_shared_open(value, false, None) as usize }
    }

    fn update(cell: usize, change: usize) -> Result<u64, u32> {
        let mut out = 0;
        // SAFETY: a live cell, and a closure from `change` matching `shim`.
        let tag = unsafe { khora_shared_update(cell as *mut u8, change as *mut u8, shim, &raw mut out) };
        if tag == 0 { Ok(out) } else { Err(tag) }
    }

    fn get(cell: usize) -> u64 {
        // SAFETY: a live cell.
        unsafe { khora_shared_get(cell as *mut u8) }
    }

    /// A pool that is joined when the test passes and leaked when it panics.
    struct Pool(std::mem::ManuallyDrop<Scheduler>);

    impl Pool {
        fn one_worker() -> Pool {
            Pool(std::mem::ManuallyDrop::new(Scheduler::started(1, true)))
        }

        /// Waits up to ten seconds for `done`, and fails with the pool's
        /// counts if it does not come.
        fn until(&self, what: &str, mut done: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < deadline, "{what}: {:?} {:?}", self.0.counts(), self.0.audit());
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        /// Spawns `body` and answers its fiber.
        fn spawn(&self, body: impl FnOnce() + Send + 'static) -> Arc<Fiber> {
            let task = Task::new(body);
            let fiber = task.fiber().clone();
            self.0.spawn(task);
            fiber
        }

        fn parked(&self) -> usize {
            self.0.audit().parked
        }

        /// Spawns a fiber that updates `cell` with `body`, waits until it is
        /// parked in line for the cell, and answers its fiber and where its
        /// update's result will be.
        fn waiter(
            &self,
            cell: usize,
            body: impl Fn(u64) -> Result<u64, u32> + Send + Sync + 'static,
        ) -> (Arc<Fiber>, Arc<Slot>) {
            let closure = change(body);
            let result = Arc::new(Mutex::new(None));
            let into = result.clone();
            let lined = self.parked() + 1;
            let fiber = self.spawn(move || {
                let got = update(cell, closure);
                *into.lock().unwrap() = Some(got);
            });
            self.until("a waiter never lined up for the cell", || self.parked() == lined);
            (fiber, result)
        }
    }

    impl std::ops::Deref for Pool {
        type Target = Scheduler;
        fn deref(&self) -> &Scheduler {
            &self.0
        }
    }

    impl Drop for Pool {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                // SAFETY: dropped once, here, and never touched again.
                unsafe { std::mem::ManuallyDrop::drop(&mut self.0) };
            }
        }
    }

    /// A fiber that takes `cell` and parks inside its change function until
    /// it is woken, then runs `then` on the value. Answers its fiber and its
    /// waker once it is parked there.
    fn holder(
        pool: &Pool,
        cell: usize,
        then: impl Fn(u64) -> Result<u64, u32> + Send + Sync + 'static,
        after: impl FnOnce() + Send + 'static,
    ) -> (Arc<Fiber>, Waker) {
        let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let mine = waker.clone();
        let inside = change(move |n| {
            *mine.lock().unwrap() = waker_for_current();
            park_current();
            then(n)
        });
        let lined = pool.parked() + 1;
        let fiber = pool.spawn(move || {
            let _ = update(cell, inside);
            after();
        });
        pool.until("the holder never took the cell", || {
            waker.lock().unwrap().is_some() && pool.parked() == lined
        });
        let waker = waker.lock().unwrap().take().expect("the holder's waker");
        (fiber, waker)
    }

    fn result(of: &Slot) -> Option<Result<u64, u32>> {
        *of.lock().unwrap()
    }

    /// **A fiber waiting for a cell gives its worker back.** One worker. H
    /// holds the cell and parks inside its change function; R reads the cell;
    /// then W wakes H. While a wait for the cell blocked the thread, R took
    /// the only worker with it, W never ran, nothing ever woke H, and the
    /// program hung: `BLOCKED_WAKER` on one CPU, every run.
    #[test]
    fn a_fiber_waiting_for_a_cell_gives_its_worker_back() {
        let pool = Pool::one_worker();
        let c = cell(41);
        let (_, h) = holder(&pool, c, |n| Ok(n + 1), || {});

        let read = Arc::new(Mutex::new(None));
        let into = read.clone();
        pool.spawn(move || *into.lock().unwrap() = Some(get(c)));
        pool.spawn(move || h.wake());

        pool.until("the reader never got the cell", || read.lock().unwrap().is_some());
        assert_eq!(*read.lock().unwrap(), Some(42), "the reader got in before the holder's change");
    }

    /// **Fibers waiting for a cell get it in the order they lined up.** H
    /// holds the cell; A, B and C line up for it in that order; H lets go and
    /// does not come back. Each giving back wakes the one that has waited
    /// longest.
    #[test]
    fn fibers_waiting_for_a_cell_get_it_in_the_order_they_lined_up() {
        let pool = Pool::one_worker();
        let c = cell(0);
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let log = order.clone();
        let (_, h) = holder(&pool, c, move |n| {
            log.lock().unwrap().push("H");
            Ok(n + 1)
        }, || {});
        let mut results = Vec::new();
        for name in ["A", "B", "C"] {
            let log = order.clone();
            results.push(pool.waiter(c, move |n| {
                log.lock().unwrap().push(name);
                Ok(n + 1)
            }));
        }

        h.wake();
        pool.until("the cell's line never emptied", || results.iter().all(|(_, r)| result(r).is_some()));
        assert_eq!(*order.lock().unwrap(), ["H", "A", "B", "C"]);
        assert_eq!(get(c), 4);
    }

    /// Spins the running fiber's worker for `long`: past [`STARVING`], so a
    /// waiter woken while it spins counts as having waited that long.
    fn hold_the_worker_for(long: Duration) {
        let until = Instant::now() + long;
        while Instant::now() < until {
            std::hint::spin_loop();
        }
    }

    /// **A fiber that keeps losing a cell to newcomers is handed it.** H
    /// holds the cell; A lines up. H lets go -- which wakes A with the cell
    /// free -- and, still running on the one worker, takes it again at once,
    /// round after round. Each round holds the worker past [`STARVING`] and
    /// then gives it up *inside* the change function, so A runs only while H
    /// holds the cell, and finds it taken. Freed every time, the cell went to
    /// H every time and A got in only after H's last round; once A has
    /// waited longer than [`STARVING`], the next giving back hands it to A.
    #[test]
    fn a_fiber_that_keeps_losing_a_cell_to_newcomers_is_handed_it() {
        const ROUNDS: u64 = 20;
        let pool = Pool::one_worker();
        let c = cell(0);
        let a_saw = Arc::new(Mutex::new(None));
        let rounds_done = Arc::new(AtomicU64::new(0));
        let done = rounds_done.clone();
        let round = change(move |n| {
            hold_the_worker_for(STARVING * 2);
            crate::coro::suspend();
            Ok(n + 1)
        });
        let (_, h) = holder(&pool, c, |n| Ok(n + 1), move || {
            for _ in 0..ROUNDS {
                update(c, round).expect("one of H's rounds");
                done.fetch_add(1, Ordering::SeqCst);
            }
        });
        let saw = a_saw.clone();
        let (_, a_got) = pool.waiter(c, move |n| {
            *saw.lock().unwrap() = Some(n);
            Ok(n + 1000)
        });

        h.wake();
        pool.until("A never got the cell", || result(&a_got).is_some());
        let saw = a_saw.lock().unwrap().expect("A's change ran");
        assert!(saw <= 4, "A waited for {saw} of H's {ROUNDS} rounds: it lost the cell every time");
        pool.until("H never finished", || rounds_done.load(Ordering::SeqCst) == ROUNDS);
    }

    /// **A fiber canceled after the cell was handed to it passes the cell on
    /// rather than keeping it.** H holds the cell; A and B wait. H lets go,
    /// which frees the cell and wakes A, and takes it straight back for a
    /// second change that holds the worker past [`STARVING`] and then gives
    /// the worker up, still holding the cell. A runs, finds the cell taken,
    /// and goes back to the front of the line starving; so when H lets go
    /// again the cell is **handed** to A -- and H cancels A before the one
    /// worker gets to it. A comes back to find the cell its own and leaves,
    /// and the cell goes to B. Kept, A would run a change function for a
    /// caller that has been told to stop, or hold the cell for nobody.
    #[test]
    fn a_fiber_canceled_after_the_cell_was_handed_to_it_passes_it_on() {
        let pool = Pool::one_worker();
        let c = cell(0);
        let a_slot: Arc<Mutex<Option<Arc<Fiber>>>> = Arc::new(Mutex::new(None));
        let to_cancel = a_slot.clone();
        let round = change(|n| {
            hold_the_worker_for(STARVING * 2);
            crate::coro::suspend();
            Ok(n + 1)
        });
        let (_, h) = holder(&pool, c, |n| Ok(n + 1), move || {
            update(c, round).expect("H's second update");
            to_cancel.lock().unwrap().as_ref().expect("A").cancel();
        });
        let a_ran = Arc::new(AtomicBool::new(false));
        let ran = a_ran.clone();
        let (a, a_got) = pool.waiter(c, move |n| {
            ran.store(true, Ordering::SeqCst);
            Ok(n + 100)
        });
        *a_slot.lock().unwrap() = Some(a);
        let (_, b_got) = pool.waiter(c, |n| Ok(n + 1));

        h.wake();
        pool.until("the cell never reached B", || result(&b_got).is_some());
        assert_eq!(result(&a_got), Some(Err(CANCELED_WHICH)), "A kept a cell it was told to leave");
        assert!(!a_ran.load(Ordering::SeqCst), "the canceled fiber's change function ran");
        assert_eq!(result(&b_got), Some(Ok(3)));
    }

    /// **A woken fiber that is canceled before it runs passes the cell on.**
    /// H holds the cell; A and B wait. H lets go, which frees the cell and
    /// wakes A, and cancels A before the worker gets to it. A leaves without
    /// running its change. B, who was counting on A to come back and pass
    /// the line on, must still get the cell: nobody else would wake it.
    #[test]
    fn a_woken_fiber_canceled_before_it_runs_passes_the_cell_on() {
        let pool = Pool::one_worker();
        let c = cell(41);
        let a_slot: Arc<Mutex<Option<Arc<Fiber>>>> = Arc::new(Mutex::new(None));
        let to_cancel = a_slot.clone();
        let (_, h) = holder(&pool, c, |n| Ok(n + 1), move || {
            to_cancel.lock().unwrap().as_ref().expect("A").cancel();
        });
        let a_ran = Arc::new(AtomicBool::new(false));
        let ran = a_ran.clone();
        let (a, a_got) = pool.waiter(c, move |n| {
            ran.store(true, Ordering::SeqCst);
            Ok(n + 100)
        });
        *a_slot.lock().unwrap() = Some(a);
        let (_, b_got) = pool.waiter(c, |n| Ok(n + 1));

        h.wake();
        pool.until("the cell never reached B", || result(&b_got).is_some());
        assert_eq!(result(&a_got), Some(Err(CANCELED_WHICH)), "A ran for a caller told to stop");
        assert!(!a_ran.load(Ordering::SeqCst), "the canceled fiber's change function ran");
        assert_eq!(result(&b_got), Some(Ok(43)));
    }

    /// **A fiber canceled while it waits for a cell leaves the line, and the
    /// cell goes to the next.** H holds the cell; A and B wait. A is
    /// canceled: its `update` comes back with the cancellation tag and its
    /// change function never runs. When H gives the cell back it goes to B.
    #[test]
    fn a_fiber_canceled_while_it_waits_for_a_cell_leaves_the_line() {
        let pool = Pool::one_worker();
        let c = cell(41);
        let (_, h) = holder(&pool, c, |n| Ok(n + 1), || {});
        let a_ran = Arc::new(AtomicBool::new(false));
        let ran = a_ran.clone();
        let (a, a_got) = pool.waiter(c, move |n| {
            ran.store(true, Ordering::SeqCst);
            Ok(n + 100)
        });
        let (_, b_got) = pool.waiter(c, |n| Ok(n + 1));

        pool.cancel_fiber(a.id());
        pool.until("the canceled waiter never left", || result(&a_got).is_some());
        assert_eq!(result(&a_got), Some(Err(CANCELED_WHICH)), "a canceled wait answers the tag");
        assert!(result(&b_got).is_none(), "B got the cell while H still held it: {:?}", result(&b_got));

        h.wake();
        pool.until("the cell never reached B", || result(&b_got).is_some());
        assert_eq!(result(&b_got), Some(Ok(43)));
        assert!(!a_ran.load(Ordering::SeqCst), "the canceled waiter's change function ran");
        assert_eq!(get(c), 43);
    }

    /// The release takes A out of the line before its wake is sent. Cancel
    /// wakes A first; A leaves the cell wait and parks in shielded cleanup.
    /// The delayed release wake must not count as the cleanup sleep's timer.
    /// `serve` is called directly under the line lock to pause precisely at
    /// the point where `give_back_slowly` would send its returned wake.
    #[test]
    fn a_late_cell_wake_does_not_shorten_shielded_cleanup_sleep() {
        let pool = Pool::one_worker();
        let c = cell(41);
        // SAFETY: this test owns the live cell for its entire run.
        let held = unsafe { held_of(c as *mut u8).expect("the cell") };
        assert!(held.lock.take(held as *const Held as usize, &never));
        let began = Arc::new(Mutex::new(None::<Instant>));
        let elapsed = Arc::new(Mutex::new(None::<Duration>));
        let began_on_worker = began.clone();
        let elapsed_on_worker = elapsed.clone();
        let closure = change(|n| Ok(n + 1));
        let waiter = pool.spawn(move || {
            assert_eq!(update(c, closure), Err(CANCELED_WHICH));
            let _shield = crate::cancel::Shielded::new();
            let start = Instant::now();
            *began_on_worker.lock().unwrap() = Some(start);
            crate::time::khora_sleep(200);
            *elapsed_on_worker.lock().unwrap() = Some(start.elapsed());
        });
        pool.until("A never lined up for the cell", || {
            pool.parked() == 1 && held.lock.line.lock().unwrap().waiting.len() == 1
        });
        let late = {
            let mut line = held.lock.line.lock().unwrap();
            held.lock.serve(&mut line).expect("the cell's first waiter")
        };
        pool.cancel_fiber(waiter.id());
        pool.until("A never parked in shielded cleanup", || {
            began.lock().unwrap().is_some() && pool.parked() == 1
        });
        late.wake();
        pool.until("the cleanup sleep did not finish", || elapsed.lock().unwrap().is_some());
        let slept = elapsed.lock().unwrap().expect("elapsed cleanup sleep");
        assert!(slept >= Duration::from_millis(180), "late cell wake shortened cleanup sleep to {slept:?}");
    }

    /// An ordinary (unshielded) sleep must still end promptly when canceled;
    /// the deadline recheck must not turn cancellation into a full wait.
    #[test]
    fn cancellation_still_ends_an_unshielded_sleep() {
        let pool = Pool::one_worker();
        let elapsed = Arc::new(Mutex::new(None::<Duration>));
        let result = elapsed.clone();
        let started = Instant::now();
        let fiber = pool.spawn(move || {
            crate::time::khora_sleep(500);
            *result.lock().unwrap() = Some(started.elapsed());
        });
        pool.until("the ordinary sleep never parked", || pool.parked() == 1);
        pool.cancel_fiber(fiber.id());
        pool.until("cancel did not end the ordinary sleep", || elapsed.lock().unwrap().is_some());
        let waited = elapsed.lock().unwrap().expect("elapsed ordinary sleep");
        assert!(waited < Duration::from_millis(300), "cancellation waited out the timer: {waited:?}");
    }

    /// **A holder stopped inside its change function lets the cell go.** H is
    /// aborted while it waits inside its change function, which leaves on the
    /// cancellation tag as a stopped `join` does there. The change does not
    /// happen, and the fiber waiting behind it gets the cell with the value H
    /// found. A lock left held on that path would be a cell every later
    /// caller waits on for ever.
    #[test]
    fn a_holder_aborted_inside_its_change_function_lets_the_cell_go() {
        let pool = Pool::one_worker();
        let c = cell(41);
        let (h, _) = holder(
            &pool,
            c,
            |n| {
                if crate::current::current(|f| f.gives_up_joining()) {
                    Err(CANCELED_WHICH)
                } else {
                    Ok(n + 100)
                }
            },
            || {},
        );
        let (_, a_got) = pool.waiter(c, |n| Ok(n + 1));

        pool.stop_fiber(h.id(), Stop::Force);
        pool.until("the cell never reached the waiter", || result(&a_got).is_some());
        assert_eq!(result(&a_got), Some(Ok(42)), "the aborted change was kept, or the cell never freed");
        assert_eq!(get(c), 42);
    }

    /// **A thread that is not a fiber waits for a cell on its own thread, and
    /// is woken when the cell is given back** -- the program's `main`, a
    /// blocking-pool thread, a foreign thread. It has no worker to give back.
    /// Woken promptly, not by its wait's [`crate::channel::LOOK_AGAIN`]
    /// backstop: the best of three rounds is well under it.
    #[test]
    fn a_thread_waiting_for_a_cell_is_woken_when_it_is_given_back() {
        let pool = Pool::one_worker();
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let c = cell(41);
            let (_, h) = holder(&pool, c, |n| Ok(n + 1), || {});
            let reader = std::thread::spawn(move || {
                let got = get(c);
                (got, Instant::now())
            });
            // Long enough for the reader to have spun and gone to sleep.
            std::thread::sleep(Duration::from_millis(20));
            let woke = Instant::now();
            h.wake();
            let (got, read_at) = reader.join().expect("the reader");
            assert_eq!(got, 42);
            best = best.min(read_at.duration_since(woke));
        }
        assert!(best < Duration::from_millis(150), "the thread was not woken, only timed out: {best:?}");
    }

    /// **A thread that loses a freed cell to a newcomer keeps its place at
    /// the front.** A thread first in line is woken with the cell free rather
    /// than handed it, so H -- still running on the one worker -- takes it
    /// straight back and parks inside a second change function. The thread
    /// wakes to find it taken, and goes back in line: at the front, ahead of
    /// A, who lined up after it. Put at the back, it read the cell only after
    /// A's change, having waited longest of everybody.
    #[test]
    fn a_thread_that_loses_a_free_cell_to_a_newcomer_keeps_its_place() {
        let pool = Pool::one_worker();
        let c = cell(41);
        let second: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let mine = second.clone();
        let again = change(move |n| {
            *mine.lock().unwrap() = waker_for_current();
            park_current();
            Ok(n + 1)
        });
        let (_, h) = holder(&pool, c, |n| Ok(n + 1), move || {
            update(c, again).expect("H's second update");
        });
        let reader = std::thread::spawn(move || get(c));
        // Long enough for the thread to have spun and lined up.
        std::thread::sleep(Duration::from_millis(20));
        let (_, a_got) = pool.waiter(c, |n| Ok(n + 100));

        h.wake();
        pool.until("H never took the cell again", || second.lock().unwrap().is_some());
        // Long enough for the thread to have woken, found it taken, and
        // lined up again.
        std::thread::sleep(Duration::from_millis(20));
        second.lock().unwrap().take().expect("H's second waker").wake();

        pool.until("A never got the cell", || result(&a_got).is_some());
        let read = reader.join().expect("the reader");
        assert!(read < 100, "the thread read {read}, after A's change, though it lined up first");
    }

    /// **Fibers and plain threads on one cell lose no update.** Four workers
    /// and twenty fibers, half of them giving their worker up inside the
    /// change function, plus two threads, all adding one; and readers. The
    /// total is exact, and every fiber finishes.
    #[test]
    fn fibers_and_threads_on_one_cell_lose_no_update() {
        const FIBERS: u64 = 20;
        const THREADS: u64 = 2;
        const EACH: u64 = 300;
        let pool = Pool(std::mem::ManuallyDrop::new(Scheduler::started(4, true)));
        let c = cell(0);
        let plain = change(|n| Ok(n + 1));
        let yielding = change(|n| {
            crate::coro::suspend();
            Ok(n + 1)
        });
        let finished = Arc::new(AtomicU64::new(0));
        for f in 0..FIBERS {
            let finished = finished.clone();
            let body = if f % 2 == 0 { plain } else { yielding };
            pool.spawn(move || {
                for _ in 0..EACH {
                    update(c, body).expect("an update");
                    let _ = get(c);
                }
                finished.fetch_add(1, Ordering::SeqCst);
            });
        }
        let thread_done = Arc::new(AtomicU64::new(0));
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let thread_done = thread_done.clone();
                std::thread::spawn(move || {
                    for _ in 0..EACH {
                        update(c, plain).expect("an update");
                    }
                    thread_done.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        // Not `join` first: a lock that strands the fibers strands the
        // threads behind them too, and a join would wait for ever.
        pool.until("a fiber or a thread never finished", || {
            finished.load(Ordering::SeqCst) == FIBERS && thread_done.load(Ordering::SeqCst) == THREADS
        });
        for t in threads {
            t.join().expect("a thread");
        }
        assert_eq!(get(c), (FIBERS + THREADS) * EACH);
    }
}
