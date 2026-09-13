//! Nurseries: a value whose release stops what is still running.
//!
//! This is where structured concurrency comes from, and it needed nothing of
//! its own. Releasing a nursery cancels its children and waits for them, and a
//! nursery is released by the block that opened it on every path out — so a
//! fiber cannot outlive the block that spawned it and nobody writes the cancel.

use super::*;
use crate::fiber::{
    failed_and_reported, fiber_state, khora_fiber_cancel, khora_fiber_release, wait_for, Handed,
};
use crate::heap::{khora_alloc, khora_drop};
use std::sync::{Arc, Mutex};

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
    /// The children a `khora_fibers_wait` round is currently joining.
    ///
    /// **A child being waited on is still a child.** A round moves its fibers
    /// out of `held` so the next round sees only new adoptions; for a while it
    /// moved them nowhere else, which left the fibers a cancellation most needs
    /// to reach findable by nobody. Raw pointers rather than `Handed` because
    /// this is a view, not a second owner: the round still releases each handle
    /// exactly once, and removes it from here first.
    joining: Vec<*mut u8>,
    /// How many children have ended with an error.
    ///
    /// **Kept on the nursery rather than answered at the wait**, because the
    /// wait is not the only place a child's outcome is seen. Adopting sweeps
    /// the children that have finished, and a failure swept there would
    /// otherwise be let go of before anybody counted it -- so a program that
    /// adopts a thousand fibers and waits once would report only whatever
    /// happened to be left in the list at the end.
    failed: i64,
}

/// The shortest list worth walking. Below this a sweep costs more in
/// bookkeeping than the handles it reclaims.
const SWEEP_FLOOR: usize = 64;

type Crew = Mutex<Children>;

/// One registered nursery.
///
/// **The `Send` claim covers the crew, not the handles.** `Children` holds
/// `*mut u8` fiber handles, which is what makes it non-`Send` by default. Those
/// are only ever read under the crew's own mutex, and the one thing done to a
/// handle from here — `khora_fiber_cancel`, which sets an atomic flag and wakes
/// — is already safe from any thread. What genuinely crosses threads is the
/// `Arc`: the fiber cancelling a nursery is by definition not the fiber that
/// opened it.
struct Registered(Arc<Crew>);

// SAFETY: as above.
unsafe impl Send for Registered {}

/// Every nursery currently open, and the fiber that opened it.
///
/// **A cancellation has to be delivered, not waited for.** Cancelling a fiber
/// whose body is a nursery flags that fiber and nothing else; the fiber is
/// inside `khora_fibers_wait`, blocked in `JoinHandle::join` on a child that
/// nobody has told to stop. It will not look at its own flag again until that
/// join returns, and if the child is in a `loop` it never does. Measured: the
/// same child cancelled directly stops in 107 ms, and under a nursery never.
///
/// So the parent-to-child edge has to exist at the moment the cancellation
/// arrives, and this is it.
static OPEN: Mutex<Vec<(usize, Registered)>> = Mutex::new(Vec::new());

/// Notes that the running fiber has opened `crew`.
fn opened(crew: &Arc<Crew>) {
    let id = crate::current::current(|fiber| fiber.id());
    OPEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, Registered(crew.clone())));
}

/// Forgets one nursery, identified by the crew itself.
///
/// By identity rather than by fiber: the binding holding a nursery can be moved,
/// so the fiber releasing it need not be the one that opened it.
fn closed(crew: &Arc<Crew>) {
    let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = open
        .iter()
        .position(|(_, Registered(held))| Arc::ptr_eq(held, crew))
    {
        open.swap_remove(at);
    }
}

/// Cancels every child of every nursery `fiber` has open.
///
/// Called by [`crate::fiber::khora_fiber_cancel`] as the cancellation is
/// delivered. Transitive without recursing here: cancelling a child that is
/// itself inside a nursery comes back through this function for that child.
///
/// **Nothing is locked while a child is cancelled.** The handles are copied out
/// from under both locks first. Cancelling reaches the scheduler, the timers and
/// the reactor, and a child's own exit path takes the crew's lock to deregister
/// itself — so cancelling while holding it is a deadlock that looks exactly like
/// the hang this exists to fix.
pub(crate) fn cancel_open_crews(fiber: usize) {
    let crews: Vec<Arc<Crew>> = {
        let open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        open.iter()
            .filter(|(owner, _)| *owner == fiber)
            .map(|(_, Registered(crew))| crew.clone())
            .collect()
    };

    let mut children: Vec<*mut u8> = Vec::new();
    for crew in &crews {
        let held = crew.lock().unwrap_or_else(|e| e.into_inner());
        children.extend(held.held.iter().map(|Handed(f)| *f));
        children.extend(held.joining.iter().copied());
    }

    for child in children {
        // SAFETY: a handle in `held` or `joining` is one the crew holds a
        // reference to; `joining` entries are removed before their round
        // releases them, so neither list can name a freed fiber.
        unsafe { crate::fiber::khora_fiber_cancel(child) };
    }
}

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
    // **`Arc`, so `OPEN` can hold one too.** A cancellation reaches this crew
    // through the registry while the fiber that opened it is blocked in a join,
    // so the two references have to be able to outlive each other either way.
    let list: Arc<Crew> = Arc::new(Mutex::new(Children {
        limit,
        sweep_at: SWEEP_FLOOR,
        held: Vec::new(),
        joining: Vec::new(),
        failed: 0,
    }));
    opened(&list);
    // SAFETY: `khora_alloc` returned an object with one field's worth of
    // space, zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object
            .add(KHORA_FIELD_OFFSET)
            .cast::<*mut Crew>()
            .write(Arc::into_raw(list) as *mut Crew);
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
        //
        // **The sweep counts what it buries.** These children have finished
        // and one of them may have finished badly; letting them go without
        // asking is how a failure disappeared between two adoptions.
        let mut swept: i64 = 0;
        for Handed(spent) in done {
            // SAFETY: as above; this is the last reference to each.
            unsafe {
                wait_for(spent);
                if failed_and_reported(spent) {
                    swept += 1;
                }
                khora_drop(spent, Some(fiber_release_shim));
            }
        }
        if swept > 0 {
            record_failures(list, swept);
        }

        match waiting {
            None => return,
            Some(Handed(oldest)) => {
                // SAFETY: as above.
                unsafe {
                    wait_for(oldest);
                    if failed_and_reported(oldest) {
                        record_failures(list, 1);
                    }
                    khora_drop(oldest, Some(fiber_release_shim));
                }
            }
        }
    }
}

/// Records `count` failures, and stops whatever is still running.
///
/// **The first failure ends the nursery's other work.** That is what makes a
/// nursery a unit rather than a bag: the block asked for these fibers together,
/// so one of them failing means the answer the group was computing is not
/// coming, and the siblings are working on a question nobody will ask. Trio and
/// every structured-concurrency design since says the same, and the alternative
/// is what this replaced -- siblings running on for as long as they liked while
/// the failure waited to be noticed.
///
/// Cancelled rather than killed: a child stops at its next `!` and runs its
/// finalizers on the way out, which is the only kind of stopping this runtime
/// has and the only kind worth having.
fn record_failures(list: &Crew, count: i64) {
    let stopping = {
        let mut crew = list.lock().unwrap_or_else(|e| e.into_inner());
        crew.failed += count;
        // Only the first failure cancels. Later ones are arriving *because* of
        // it -- a sibling that stopped where it was told to and then failed on
        // the way out -- and cancelling twice says nothing new.
        if crew.failed > count {
            Vec::new()
        } else {
            crew.held.iter().map(|Handed(fiber)| *fiber).collect::<Vec<_>>()
        }
    };
    for fiber in stopping {
        // SAFETY: every handle in the list was live when adopted and the list
        // holds the only reference; this only sets a flag on it.
        unsafe { khora_fiber_cancel(fiber) };
    }
}

/// Waits for every fiber in the nursery, oldest first, and empties it.
///
/// Answers how many children ended with an error, which is the whole of what a
/// nursery can say about them: every child's error has a type of its own and a
/// nursery holds them as bare handles, so the count is what survives. `std`
/// turns a non-zero answer into a `ChildFailed` raise.
///
/// **A child that failed stops the others.** The first failure cancels every
/// sibling still running, here and at every later observation -- see
/// [`record_failures`]. Every child is still *waited* for, because a nursery
/// that returned while one was winding up would not be structured at all.
///
/// Oldest first because there is no reason to prefer otherwise and an order
/// that is stated is easier to reason about than one that is not.
///
/// # Safety
///
/// `fibers` must be a live object from [`khora_fibers_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fibers_wait(fibers: *mut u8) -> i64 {
    // SAFETY: the caller guarantees a live nursery.
    let Some(list) = (unsafe { crew(fibers) }) else { return 0 };
    // **Drained in rounds, until a round finds nothing.** A child may adopt a
    // fiber of its own while this one is waiting — that is what a shareable
    // nursery is for — and a single pass would return with that grandchild
    // still running, which is precisely the promise a nursery makes.
    //
    // Taken under the lock and joined outside it, because holding the lock
    // across a join would deadlock against exactly that adoption.
    loop {
        let waiting = {
            let mut crew = list.lock().unwrap_or_else(|e| e.into_inner());
            let round = std::mem::take(&mut crew.held);
            // Visible to `cancel_open_crews` for as long as it is being joined.
            crew.joining = round.iter().map(|Handed(f)| *f).collect();
            round
        };
        if waiting.is_empty() {
            return list.lock().unwrap_or_else(|e| e.into_inner()).failed;
        }
        // **And a round nobody is waiting on because *this* fiber was told to
        // stop.** `docs/design/fibers.md` promises that cancelling a nursery
        // cancels its children, transitively, and nothing else looks.
        //
        // **This covers only the cancellation that arrives between rounds, and
        // that is not the common one.** A parent blocked in `wait_for` below is
        // inside `JoinHandle::join` on the thread backend, which cannot be
        // given a deadline, so a cancellation arriving then is not seen until
        // the round it is waiting on completes -- and if the children are in
        // `loop`s, it never does. The hang is still reachable: cancel a fiber
        // that is already inside this call and it waits for ever.
        //
        // Closing it properly means cancelling at the point the cancellation is
        // *delivered* rather than where it is noticed -- a fiber would have to
        // know its open nurseries so `khora_fiber_cancel` could walk them --
        // and that is a change to what a `Fiber` owns, with a lock order to get
        // right between the fiber and the crew. It is not a repair.
        //
        // The children are still *waited* for after being cancelled. A nursery
        // that returned while one was winding up would not be structured, and
        // that is as true of a cancellation as it is of a failure.
        let stopping = crate::current::current(|fiber| fiber.stops_here());
        if stopping || list.lock().unwrap_or_else(|e| e.into_inner()).failed > 0 {
            for Handed(fiber) in waiting.iter() {
                // SAFETY: as below.
                unsafe { khora_fiber_cancel(*fiber) };
            }
        }
        for index in 0..waiting.len() {
            let fiber = waiting[index].0;
            // SAFETY: each handle was live when adopted and this list has held
            // the only reference since.
            unsafe {
                wait_for(fiber);
                if failed_and_reported(fiber) {
                    // The siblings still in this round are not in the list any
                    // more -- this round took them -- so `record_failures`
                    // cannot reach them and they are stopped here.
                    for Handed(other) in waiting.iter().skip(index + 1) {
                        khora_fiber_cancel(*other);
                    }
                    record_failures(list, 1);
                }
            }
        }
        for Handed(fiber) in waiting {
            // Out of `joining` before the handle goes, so a cancellation
            // arriving now cannot name a fiber this round has finished with.
            {
                let mut crew = list.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(at) = crew.joining.iter().position(|f| *f == fiber) {
                    crew.joining.swap_remove(at);
                }
            }
            // SAFETY: as above; this is the last reference to each.
            unsafe { khora_drop(fiber, Some(fiber_release_shim)) };
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
        let list = Arc::from_raw(list as *const Crew);
        // Out of the registry first: a child cancelled below may come back
        // through `cancel_open_crews`, and must not find a crew going away.
        closed(&list);
        let mut round = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        while !round.is_empty() {
            for Handed(fiber) in round.iter() {
                khora_fiber_cancel(*fiber);
            }
            for Handed(fiber) in round {
                wait_for(fiber);
                khora_drop(fiber, Some(fiber_release_shim));
            }
            round = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        }
    }
}
