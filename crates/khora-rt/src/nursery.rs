//! Nurseries: a value whose release stops what is still running.
//!
//! This is where structured concurrency comes from, and it needed nothing of
//! its own. Releasing a nursery cancels its children and waits for them, and a
//! nursery is released by the block that opened it on every path out — so a
//! fiber cannot outlive the block that spawned it and nobody writes the cancel.

use super::*;
use crate::fiber::{fiber_state, khora_fiber_cancel, khora_fiber_join, khora_fiber_release, Handed};
use crate::heap::{khora_alloc, khora_drop};
use std::sync::Mutex;

/// [`khora_fiber_release`] as a `drop_fields` callback. See [`release_shim`].
extern "C" fn fiber_release_shim(fiber: *mut u8) {
    // SAFETY: only ever reached through `khora_drop`, which calls it with the
    // object whose last reference it just released.
    unsafe { khora_fiber_release(fiber) };
}

/// The fibers a nursery is responsible for.
///
/// Held Rust-side for the same reason a region's finalizers are: adopting one
/// *grows* the list, and nothing in Khora grows a value in place.
struct Children {
    /// The most children this nursery will hold at once, or zero for as many
    /// as are adopted.
    limit: usize,
    /// How long the list may get before it is worth sweeping.
    ///
    /// **Sweeping on every adoption was a third of a server's throughput.**
    /// Asking a child whether it has finished takes its lock, so a full pass
    /// over a bounded nursery of 256 was 256 lock-unlock pairs per connection —
    /// 2,134 requests a second against 6,406 for the same architecture written
    /// straight in Rust. Set to twice the survivors after each sweep, so the
    /// work is amortised to about one check per adoption however many children
    /// there are.
    sweep_at: usize,
    held: Vec<Handed>,
}

/// The shortest list worth walking. Below this a sweep costs more in
/// bookkeeping than the handles it reclaims.
const SWEEP_FLOOR: usize = 64;

type Crew = Mutex<Children>;

/// The tag every nursery object carries.
const FIBERS_TAG: u32 = 0;

/// The list behind a nursery handle, or null once it has been released.
///
/// # Safety
///
/// `fibers` must be a live object from [`khora_fibers_open`].
unsafe fn crew<'a>(fibers: *mut u8) -> Option<&'a Crew> {
    if fibers.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_fibers_open` wrote there.
    unsafe { (*fibers.add(KHORA_FIELD_OFFSET).cast::<*mut Crew>()).as_ref() }
}

/// Opens a nursery: a set of fibers that ends when the binding holding it does.
///
/// Releasing it *cancels then waits*, which is the answer for the path where
/// the block did not finish — a raise, or a cancellation, passing through. On
/// the ordinary path [`khora_fibers_wait`] has already emptied it, so the
/// release finds nothing to stop. That is what lets one object mean both "wait
/// for the children" and "the answer is no longer wanted" without ever being
/// told which happened.
#[unsafe(no_mangle)]
pub extern "C" fn khora_fibers_open() -> *mut u8 {
    khora_fibers_open_bounded(0)
}

/// Opens a nursery that will hold at most `limit` running children.
///
/// **The bound is what turns a capacity ceiling into a queue.** A fiber is an
/// operating-system thread today, so a server adopting one per connection
/// spends about 33 KB apiece — measured — and an unbounded nursery meets its
/// ceiling by exhausting memory, which is the worst way to meet one. With a
/// bound, adopting the child past the limit *waits* for an older one to finish,
/// the accept loop stops accepting, and the connections pile up in the
/// listening socket's backlog where the operating system already knows how to
/// hold them. Overload becomes latency instead of collapse.
///
/// Zero means unbounded, which is right for a nursery over a known handful of
/// concurrent tasks — the shape `nursery(..)` is usually used for — and wrong
/// for one fed by the outside world.
#[unsafe(no_mangle)]
pub extern "C" fn khora_fibers_open_bounded(limit: i64) -> *mut u8 {
    let limit = if limit > 0 { limit as usize } else { 0 };
    let object = khora_alloc(std::mem::size_of::<*mut Crew>() as u64, FIBERS_TAG);
    let list: Box<Crew> = Box::new(Mutex::new(Children {
        limit,
        sweep_at: SWEEP_FLOOR,
        held: Vec::new(),
    }));
    // SAFETY: `khora_alloc` returned an object with one field's worth of
    // space, zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Crew>().write(Box::into_raw(list));
    }
    object
}

/// Whether a fiber has already run to its end.
///
/// Asked without joining, so a nursery can let go of a child that has finished
/// without waiting on one that has not.
///
/// # Safety
///
/// `fiber` must be a live object from [`khora_fiber_spawn`].
unsafe fn fiber_finished(fiber: *mut u8) -> bool {
    // SAFETY: the caller guarantees a live handle.
    let Some(state) = (unsafe { fiber_state(fiber) }) else { return true };
    state.completion.finished()
}

/// Makes `fiber` this nursery's responsibility, taking its reference.
///
/// **Children that have finished are let go of first**, and that sweep is not
/// housekeeping — without it a nursery only ever grows. A server adopts one
/// fiber per connection into a nursery it drains when it stops accepting,
/// which is never, so every answered request left its handle in the list: three
/// thousand requests, three thousand operating-system handles, none of them
/// pointing at a running thread. Measured on the link shortener, which is what
/// it took to see it.
///
/// **Not on every adoption**, which is the other measured thing. Asking a child
/// whether it has finished takes its lock, so sweeping each time cost a
/// bounded nursery 256 lock-unlock pairs per connection and two thirds of the
/// server's throughput. `sweep_at` holds it to about one check per adoption
/// amortised, by only walking the list once it has grown to twice what the
/// last sweep left behind.
///
/// # Safety
///
/// `fibers` must be live from [`khora_fibers_open`] and `fiber` live from
/// [`khora_fiber_spawn`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fibers_adopt(fibers: *mut u8, fiber: *mut u8) {
    // SAFETY: the caller guarantees a live nursery.
    let Some(list) = (unsafe { crew(fibers) }) else {
        fatal("adopting a fiber into a nursery that has already ended");
    };

    // Until there is room. Each turn sweeps what has finished and, if that was
    // not enough, takes the oldest child out to be waited for — outside the
    // lock, because a join is not instant once the child is still running and a
    // lock held across one is a nursery nobody else can adopt into.
    loop {
        let (done, waiting) = {
            // Locked: a nursery exists to be adopted into from more than one
            // fiber, so this is the one place contention is expected rather
            // than incidental.
            let mut crew = list.lock().unwrap_or_else(|e| e.into_inner());

            // Only when the list has grown past its mark, or when there is no
            // room and a sweep is the cheapest way to find some.
            let crowded = crew.limit > 0 && crew.held.len() >= crew.limit;
            let done: Vec<Handed> = if crowded || crew.held.len() >= crew.sweep_at {
                // SAFETY: every handle in the list was live when adopted and
                // this list has held the only reference since.
                let (done, keep): (Vec<Handed>, Vec<Handed>) = std::mem::take(&mut crew.held)
                    .into_iter()
                    .partition(|Handed(f)| unsafe { fiber_finished(*f) });
                crew.held = keep;
                crew.sweep_at = SWEEP_FLOOR.max(crew.held.len().saturating_mul(2));
                done
            } else {
                Vec::new()
            };

            if crew.limit == 0 || crew.held.len() < crew.limit {
                crew.held.push(Handed(fiber));
                (done, None)
            } else {
                // Oldest first, which is the order `khora_fibers_wait` uses and
                // the only one that cannot starve a child.
                (done, Some(crew.held.remove(0)))
            }
        };

        // Joining a thread that has already ended returns at once, but a drop
        // routine can reach another nursery, and a lock held across one of
        // those is a lock ordering nobody agreed to.
        for Handed(spent) in done {
            // SAFETY: as above; this is the last reference to each.
            unsafe {
                khora_fiber_join(spent);
                khora_drop(spent, Some(fiber_release_shim));
            }
        }

        match waiting {
            None => return,
            Some(Handed(oldest)) => {
                // SAFETY: as above.
                unsafe {
                    khora_fiber_join(oldest);
                    khora_drop(oldest, Some(fiber_release_shim));
                }
            }
        }
    }
}

/// Waits for every fiber in the nursery, oldest first, and empties it.
///
/// Oldest first because there is no reason to prefer otherwise and an order
/// that is stated is easier to reason about than one that is not. Every child
/// is waited for regardless: a nursery that returned after the first one
/// finished would not be structured at all.
///
/// # Safety
///
/// `fibers` must be a live object from [`khora_fibers_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fibers_wait(fibers: *mut u8) {
    // SAFETY: the caller guarantees a live nursery.
    let Some(list) = (unsafe { crew(fibers) }) else { return };
    // **Drained in rounds, until a round finds nothing.** A child may adopt a
    // fiber of its own while this one is waiting — that is what a shareable
    // nursery is for — and a single pass would return with that grandchild
    // still running, which is precisely the promise a nursery makes.
    //
    // Taken under the lock and joined outside it, because holding the lock
    // across a join would deadlock against exactly that adoption.
    loop {
        let waiting = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        if waiting.is_empty() {
            return;
        }
        for Handed(fiber) in waiting {
            // SAFETY: each handle was live when adopted and this list has held
            // the only reference since.
            unsafe {
                khora_fiber_join(fiber);
                khora_drop(fiber, Some(fiber_release_shim));
            }
        }
    }
}

/// Cancels every fiber in the nursery, then waits for all of them.
///
/// This is a `drop_fields` callback, and it is the whole of structured
/// concurrency's failure case: the block is leaving without finishing, so the
/// answers its children were computing are no longer wanted. Cancelled *first*
/// and in one pass, so the children stop concurrently rather than one waiting
/// out the next.
///
/// # Safety
///
/// `fibers` must be a live object from [`khora_fibers_open`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fibers_release(fibers: *mut u8) {
    if fibers.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live nursery; the field holds what
    // `khora_fibers_open` wrote, and nothing else reads it after this.
    unsafe {
        let slot = fibers.add(KHORA_FIELD_OFFSET).cast::<*mut Crew>();
        let list = *slot;
        if list.is_null() {
            return;
        }
        slot.write(std::ptr::null_mut());

        // Same rounds as `khora_fibers_wait`, for the same reason: a child
        // being cancelled runs its finalizers on the way out, and one of those
        // may still be adopting. The list is this function's alone now — the
        // slot was nulled above — so each round takes what the last one did not
        // know about.
        let list = Box::from_raw(list);
        let mut round = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        while !round.is_empty() {
            for Handed(fiber) in round.iter() {
                khora_fiber_cancel(*fiber);
            }
            for Handed(fiber) in round {
                khora_fiber_join(fiber);
                khora_drop(fiber, Some(fiber_release_shim));
            }
            round = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        }
    }
}
