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

/// One nursery's children, behind their lock.
///
/// **A newtype so the thread-safety claim can be made about the crew itself.**
/// `Children` holds `*mut u8` fiber handles, which makes it neither `Send` nor
/// `Sync` by default, and an `Arc<Mutex<Children>>` is therefore an `Arc` of
/// something that cannot cross a thread -- which is both a clippy error and a
/// fair description of the problem. The claim belongs here, where the handles
/// are, rather than on a wrapper around the `Arc`.
struct Crew(Mutex<Children>);

// SAFETY: the handles are only ever read under this mutex, and the one thing
// done to one from another thread -- `crate::fiber::deliver`, with either stop,
// which sets atomic bits and wakes -- is safe from any thread. Crossing threads
// is the point: the fiber stopping a nursery is by definition not the fiber
// that opened it.
unsafe impl Send for Crew {}
unsafe impl Sync for Crew {}

impl std::ops::Deref for Crew {
    type Target = Mutex<Children>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

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
static OPEN: Mutex<Vec<(usize, Arc<Crew>)>> = Mutex::new(Vec::new());

/// Notes that the running fiber has opened `crew`.
fn opened(crew: &Arc<Crew>) {
    let id = crate::current::current(|fiber| fiber.id());
    OPEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, crew.clone()));
}

/// Forgets one nursery, identified by the crew itself.
///
/// By identity rather than by fiber: the binding holding a nursery can be moved,
/// so the fiber releasing it need not be the one that opened it.
fn closed(crew: &Arc<Crew>) {
    let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = open
        .iter()
        .position(|(_, held)| Arc::ptr_eq(held, crew))
    {
        open.swap_remove(at);
    }
}

/// Delivers `stop` to every child of every nursery `fiber` has open.
///
/// Called by [`crate::fiber::deliver`] as a cancellation or a force is
/// delivered. Transitive without recursing here: stopping a child that is
/// itself inside a nursery comes back through this function for that child,
/// with the same `stop`. **A force has to take this path as well as a
/// cancel**: a forced parent is blocked joining its children, so a child left
/// in shielded cleanup because only its parent was forced keeps the parent
/// exactly where the force was meant to get it out of.
///
/// **Nothing is locked while a child is cancelled.** The handles are copied out
/// from under both locks first. Cancelling reaches the scheduler, the timers and
/// the reactor, and a child's own exit path takes the crew's lock to deregister
/// itself — so cancelling while holding it is a deadlock that looks exactly like
/// the hang this exists to fix.
pub(crate) fn cancel_open_crews(fiber: usize, stop: crate::current::Stop) {
    let crews: Vec<Arc<Crew>> = {
        let open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        open.iter()
            .filter(|(owner, _)| *owner == fiber)
            .map(|(_, crew)| crew.clone())
            .collect()
    };

    // **Identities, cloned under the crew's lock, not handles.** A handle in
    // `held` or `joining` is live while this lock is held -- a round takes it
    // out of `joining` under the lock before it releases it -- and not a
    // moment longer. Delivering is what ends that: the child stops, its
    // parent's wait returns, and the parent frees the handle and the state
    // behind it while this is still delivering. A raw handle copied out here
    // and dereferenced below was a use-after-free on the `khora-deadlines`
    // thread (`crate::fiber::deliver_to_fiber` has the sequence). An
    // `Arc<Fiber>` keeps the one thing delivery needs alive for as long as it
    // needs it.
    let mut children: Vec<Arc<crate::current::Fiber>> = Vec::new();
    for crew in &crews {
        let held = crew.lock().unwrap_or_else(|e| e.into_inner());
        for handle in held.held.iter().map(|Handed(f)| *f).chain(held.joining.iter().copied()) {
            // SAFETY: live while the lock is held, as above.
            if let Some(state) = unsafe { crate::fiber::fiber_state(handle) } {
                children.push(state.fiber.clone());
            }
        }
    }

    for child in &children {
        crate::fiber::deliver_to_fiber(child, stop);
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
    let list: Arc<Crew> = Arc::new(Crew(Mutex::new(Children {
        limit,
        sweep_at: SWEEP_FLOOR,
        held: Vec::new(),
        joining: Vec::new(),
        failed: 0,
    })));
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
                // **The child waited on for room is still a child, and a stop
                // has to reach it.** Out of `held`, it was on no list
                // `cancel_open_crews` walks, and `wait_for` does not give up:
                // an adopter cancelled here waited the child out -- 16 s,
                // measured -- and then carried on. So it goes on `joining`
                // for the length of the wait, where a cancel that arrives
                // mid-wait is delivered to it, and a stop already pending is
                // passed on here. Registered before the question is asked, so
                // a cancel landing between the two is seen by one of them.
                //
                // Still *waited* for, as `khora_fibers_wait` waits for the
                // children it cancels: a stopped child is prompt, and one
                // stuck in cleanup is what `abort` exists for. The adopter
                // stops at the check its caller makes after this returns.
                list.lock().unwrap_or_else(|e| e.into_inner()).joining.push(oldest);
                if crate::current::current(|me| me.stops_here()) {
                    let stop = if crate::current::current(|me| me.is_forced()) {
                        crate::current::Stop::Force
                    } else {
                        crate::current::Stop::Cancel
                    };
                    // SAFETY: the handle came out of `held`, which holds the
                    // only reference, and it is released only below.
                    unsafe { crate::fiber::deliver(oldest, stop) };
                }
                // SAFETY: as above.
                unsafe { wait_for(oldest) };
                {
                    let mut crew = list.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(at) = crew.joining.iter().position(|f| *f == oldest) {
                        crew.joining.swap_remove(at);
                    }
                }
                // SAFETY: as above, and out of `joining` before it goes.
                unsafe {
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
        // the way out -- and cancelling twice says nothing new: a second cancel
        // is not escalation, which
        // `a_shielded_fiber_cancelled_twice_still_finishes_its_cleanup` pins.
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
            // Added to, not replaced: an `adopt` waiting for room keeps the
            // child it waits on here too.
            crew.joining.extend(round.iter().map(|Handed(f)| *f));
            round
        };
        if waiting.is_empty() {
            return list.lock().unwrap_or_else(|e| e.into_inner()).failed;
        }
        // **And a round nobody is waiting on because *this* fiber was told to
        // stop.** Cancelling a nursery cancels its children, transitively.
        //
        // **This check covers only a stop that arrives between rounds.** One
        // that arrives while this fiber is parked in `wait_for` below is
        // delivered, not noticed: `cancel_open_crews` walks this crew's
        // `joining` list at the moment of the cancel, and a force the waiter
        // receives mid-wait is passed on by `wait_for` itself
        // (`FiberState::wait_passing_on_a_force`).
        //
        // The children are still *waited* for after being cancelled. A nursery
        // that returned while one was winding up would not be structured, and
        // that is as true of a cancellation as it is of a failure.
        let stopping = crate::current::current(|fiber| fiber.stops_here());
        // A forced waiter forces: the children are what it is waiting on.
        let stop = if crate::current::current(|fiber| fiber.is_forced()) {
            crate::current::Stop::Force
        } else {
            crate::current::Stop::Cancel
        };
        if stopping || list.lock().unwrap_or_else(|e| e.into_inner()).failed > 0 {
            for Handed(fiber) in waiting.iter() {
                // SAFETY: as below.
                unsafe { crate::fiber::deliver(*fiber, stop) };
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
        // Cancelled whatever happened; forced when the fiber releasing it was,
        // because this runs in that fiber's cleanup and the children are what
        // it is waiting for.
        let stop = if crate::current::current(|fiber| fiber.is_forced()) {
            crate::current::Stop::Force
        } else {
            crate::current::Stop::Cancel
        };
        while !round.is_empty() {
            for Handed(fiber) in round.iter() {
                crate::fiber::deliver(*fiber, stop);
            }
            for Handed(fiber) in round {
                wait_for(fiber);
                khora_drop(fiber, Some(fiber_release_shim));
            }
            round = std::mem::take(&mut list.lock().unwrap_or_else(|e| e.into_inner()).held);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::{khora_cancelled, Shielded};
    use crate::fiber::{khora_fiber_cancel, khora_fiber_force, khora_fiber_join, khora_fiber_spawn};
    use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// How long a child's cleanup waits to be stopped before it gives up and
    /// reports that nothing reached it. A red run costs this much.
    const PATIENCE: Duration = Duration::from_secs(5);

    /// What a child in cleanup reports.
    const RUNNING: usize = 0;
    const STOPPED: usize = 1;
    const RAN_OUT: usize = 2;

    /// A closure object of type `() -> A`: one field, the code pointer. The
    /// trampolines here ignore it, but `khora_fiber_spawn` reads it.
    fn closure() -> *mut u8 {
        let object = khora_alloc(std::mem::size_of::<*const u8>() as u64, 0);
        // SAFETY: one field's worth of freshly allocated space, and nothing
        // else holds the pointer yet.
        unsafe {
            object.add(KHORA_FIELD_OFFSET).cast::<*const u8>().write(std::ptr::null());
        }
        object
    }

    /// Spawns `thunk` as an infallible fiber with a non-pointer answer.
    fn spawn(thunk: crate::PlainTrampoline1) -> *mut u8 {
        // SAFETY: a live closure whose drop is the default, an infallible
        // trampoline matching `plain`, and an answer that is not a pointer.
        unsafe { khora_fiber_spawn(closure(), None, None, Some(thunk), false, None) }
    }

    /// Cleanup that stops only when a cancellation point says so: shielded,
    /// as a region's finalizers are, and polling the real cancellation point.
    fn shielded_cleanup(inside: &AtomicUsize, outcome: &AtomicUsize) {
        let _cleanup = Shielded::new();
        inside.store(1, Ordering::SeqCst);
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if khora_cancelled() == 1 {
                outcome.store(STOPPED, Ordering::SeqCst);
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        outcome.store(RAN_OUT, Ordering::SeqCst);
    }

    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !done() {
            assert!(Instant::now() < deadline, "{what} never happened");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    extern "C" fn release_nursery(fibers: *mut u8) {
        // SAFETY: only reached through `khora_drop`, with the last reference.
        unsafe { khora_fibers_release(fibers) };
    }

    static NURSED_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static NURSED_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);

    extern "C" fn nursed_child(_code: *const u8, _body: *mut u8) -> u64 {
        shielded_cleanup(&NURSED_INSIDE, &NURSED_OUTCOME);
        0
    }

    extern "C" fn nursery_parent(_code: *const u8, _body: *mut u8) -> u64 {
        let nursery = khora_fibers_open();
        // SAFETY: a live nursery and a live handle, whose reference the
        // nursery takes; then the nursery's last reference.
        unsafe {
            khora_fibers_adopt(nursery, spawn(nursed_child));
            khora_fibers_wait(nursery);
            khora_drop(nursery, Some(release_nursery));
        }
        0
    }

    /// **Force reaches a nursery's children.** A parent blocked waiting on a
    /// nursery is forced, and the child it is waiting on -- in shielded
    /// cleanup, where a cancel alone must not reach -- stops.
    ///
    /// Cancelled first, **twice**, and given time, so the test also says a
    /// cancel was delivered and did not stop the cleanup. Twice because that
    /// is the runtime's own shape: each cancel of the parent reaches the child
    /// through `cancel_open_crews`, so a second cancel that escalated would
    /// force a child here that nobody forced. Without this half, a child that
    /// stopped on a cancel would pass for the wrong reason.
    #[test]
    fn a_forced_parent_stops_its_nursery_child_in_shielded_cleanup() {
        let parent = spawn(nursery_parent);
        until("the child reaching its cleanup", || NURSED_INSIDE.load(Ordering::SeqCst) == 1);

        // SAFETY: a live handle, held by this test until it is joined.
        unsafe {
            khora_fiber_cancel(parent);
            khora_fiber_cancel(parent);
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(NURSED_OUTCOME.load(Ordering::SeqCst), RUNNING, "a cancel cut cleanup short");

        let forced = Instant::now();
        // SAFETY: as above.
        unsafe { khora_fiber_force(parent) };
        let mut answer = 0u64;
        // SAFETY: as above, and a writable word; then the last reference.
        unsafe {
            khora_fiber_join(parent, &raw mut answer);
            crate::fiber::khora_fiber_release(parent);
        }
        assert_eq!(
            NURSED_OUTCOME.load(Ordering::SeqCst),
            STOPPED,
            "the force never reached the child (it waited {:?})",
            forced.elapsed()
        );
    }

    static HELD_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static HELD_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);
    static HELD_PARENT_READY: AtomicUsize = AtomicUsize::new(0);
    static HELD_CHILD: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn held_child(_code: *const u8, _body: *mut u8) -> u64 {
        shielded_cleanup(&HELD_INSIDE, &HELD_OUTCOME);
        0
    }

    extern "C" fn holding_parent(_code: *const u8, _body: *mut u8) -> u64 {
        let child = spawn(held_child);
        HELD_CHILD.store(child, Ordering::SeqCst);
        HELD_PARENT_READY.store(1, Ordering::SeqCst);
        // Until this fiber is forced, which is what the test does next.
        let deadline = Instant::now() + PATIENCE;
        while !crate::current::current(|me| me.is_forced()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        // Releasing the handle is what a frame's cleanup does with a fiber it
        // holds, and it waits for the child.
        // SAFETY: the only reference to a live handle.
        unsafe { crate::fiber::khora_fiber_release(child) };
        0
    }

    /// **Force reaches a child held by handle, through the handle's release.**
    /// That release is the other road a stop takes from a fiber to its child,
    /// and it runs in the holder's cleanup: a forced holder that passed on only
    /// a cancel would wait for ever on a child whose own cleanup is shielded.
    #[test]
    fn a_forced_holder_forces_the_child_whose_handle_it_releases() {
        let parent = spawn(holding_parent);
        until("the parent holding its child", || HELD_PARENT_READY.load(Ordering::SeqCst) == 1);
        until("the child reaching its cleanup", || HELD_INSIDE.load(Ordering::SeqCst) == 1);
        // The child was cancelled once already, as a detached parent's child
        // would be; that must not end its cleanup.
        // SAFETY: the parent holds this handle until it is forced, below.
        unsafe { khora_fiber_cancel(HELD_CHILD.load(Ordering::SeqCst)) };
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(HELD_OUTCOME.load(Ordering::SeqCst), RUNNING, "a cancel cut cleanup short");

        // SAFETY: a live handle, held by this test until it is joined.
        unsafe { khora_fiber_force(parent) };
        let mut answer = 0u64;
        // SAFETY: as above, and a writable word; then the last reference.
        unsafe {
            khora_fiber_join(parent, &raw mut answer);
            crate::fiber::khora_fiber_release(parent);
        }
        assert_eq!(HELD_OUTCOME.load(Ordering::SeqCst), STOPPED, "the force never reached the child");
    }

    static LATE_WAIT_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static LATE_WAIT_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);
    static LATE_RELEASE_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static LATE_RELEASE_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);

    extern "C" fn late_wait_child(_code: *const u8, _body: *mut u8) -> u64 {
        shielded_cleanup(&LATE_WAIT_INSIDE, &LATE_WAIT_OUTCOME);
        0
    }

    extern "C" fn late_release_child(_code: *const u8, _body: *mut u8) -> u64 {
        shielded_cleanup(&LATE_RELEASE_INSIDE, &LATE_RELEASE_OUTCOME);
        0
    }

    /// A fiber forces itself, then -- as cleanup would -- opens a nursery and
    /// adopts a child that goes into shielded cleanup of its own. The force
    /// was delivered before the nursery existed, so `cancel_open_crews` never
    /// saw it; only the nursery's own wait or release can pass it on.
    fn forced_then_nursery(child: crate::PlainTrampoline1, inside: &AtomicUsize, wait: bool) {
        crate::current::current(|me| me.force());
        let _cleanup = Shielded::new();
        let nursery = khora_fibers_open();
        // SAFETY: a live nursery and a live handle, whose reference the
        // nursery takes; then the nursery's last reference.
        unsafe {
            khora_fibers_adopt(nursery, spawn(child));
            until("the child reaching its cleanup", || inside.load(Ordering::SeqCst) == 1);
            if wait {
                khora_fibers_wait(nursery);
            }
            khora_drop(nursery, Some(release_nursery));
        }
    }

    extern "C" fn late_wait_parent(_code: *const u8, _body: *mut u8) -> u64 {
        forced_then_nursery(late_wait_child, &LATE_WAIT_INSIDE, true);
        0
    }

    extern "C" fn late_release_parent(_code: *const u8, _body: *mut u8) -> u64 {
        forced_then_nursery(late_release_child, &LATE_RELEASE_INSIDE, false);
        0
    }

    fn run_to_end(parent: *mut u8) {
        let mut answer = 0u64;
        // SAFETY: a live handle from `spawn`, a writable word; then its last
        // reference.
        unsafe {
            khora_fiber_join(parent, &raw mut answer);
            crate::fiber::khora_fiber_release(parent);
        }
    }

    /// **A forced fiber waiting on a nursery forces its children**, including
    /// children adopted after the force arrived.
    #[test]
    fn a_forced_fiber_waiting_on_a_nursery_forces_what_it_waits_for() {
        run_to_end(spawn(late_wait_parent));
        assert_eq!(LATE_WAIT_OUTCOME.load(Ordering::SeqCst), STOPPED, "the wait passed on only a cancel");
    }

    /// **A forced fiber releasing a nursery forces its children**: the release
    /// is cleanup, and it waits for them.
    #[test]
    fn a_forced_fiber_releasing_a_nursery_forces_what_it_releases() {
        run_to_end(spawn(late_release_parent));
        assert_eq!(
            LATE_RELEASE_OUTCOME.load(Ordering::SeqCst),
            STOPPED,
            "the release passed on only a cancel"
        );
    }

    // --- a force that lands while the waiter is already in its cleanup -----
    //
    // The shape a deadline produces: cancel now, force once the cleanup has
    // overrun. By then the cancelled fiber is inside a wait that does not give
    // up, and a force that only walks `OPEN` finds nothing to pass on. Each
    // test cancels, confirms the child is still running with the waiter
    // inside that wait, and only then forces, so a child stopped by the cancel
    // fails the test before the force is tried.

    /// Until the waiter is cancelled, as a frame running on is until it
    /// reaches its next cancellation point.
    fn until_cancelled() {
        let deadline = Instant::now() + PATIENCE;
        while !crate::current::current(|me| me.is_cancelled()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Cancel `parent`, wait until `inside` says it is in its cleanup's wait,
    /// check `outcome` is still running, then force and wait for the end.
    fn cancel_then_force_inside(
        parent: *mut u8,
        child_inside: &AtomicUsize,
        waiting: &AtomicUsize,
        outcome: &AtomicUsize,
    ) {
        until("the child reaching its cleanup", || child_inside.load(Ordering::SeqCst) == 1);
        // SAFETY: a live handle from `spawn`, held by this test until the end.
        unsafe { khora_fiber_cancel(parent) };
        until("the parent reaching its wait", || waiting.load(Ordering::SeqCst) == 1);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(outcome.load(Ordering::SeqCst), RUNNING, "a cancel cut cleanup short");
        // SAFETY: as above.
        unsafe { khora_fiber_force(parent) };
        run_to_end(parent);
    }

    static HANDLE_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static HANDLE_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);
    static HANDLE_RELEASING: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn handle_child(_code: *const u8, _body: *mut u8) -> u64 {
        shielded_cleanup(&HANDLE_INSIDE, &HANDLE_OUTCOME);
        0
    }

    extern "C" fn handle_holder(_code: *const u8, _body: *mut u8) -> u64 {
        let child = spawn(handle_child);
        until_cancelled();
        let _cleanup = Shielded::new();
        HANDLE_RELEASING.store(1, Ordering::SeqCst);
        // SAFETY: the only reference to a live handle.
        unsafe { crate::fiber::khora_fiber_release(child) };
        0
    }

    /// Forced while already releasing a child's handle, which passed on only
    /// the cancel it had when the release began.
    #[test]
    fn a_force_arriving_while_releasing_a_handle_reaches_the_child() {
        cancel_then_force_inside(
            spawn(handle_holder),
            &HANDLE_INSIDE,
            &HANDLE_RELEASING,
            &HANDLE_OUTCOME,
        );
        assert_eq!(HANDLE_OUTCOME.load(Ordering::SeqCst), STOPPED, "the force never reached the child");
    }

    static CREW_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static CREW_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);
    static CREW_RELEASING: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn crew_child(_code: *const u8, _body: *mut u8) -> u64 {
        shielded_cleanup(&CREW_INSIDE, &CREW_OUTCOME);
        0
    }

    extern "C" fn crew_parent(_code: *const u8, _body: *mut u8) -> u64 {
        let nursery = khora_fibers_open();
        // SAFETY: a live nursery and a live handle, whose reference it takes.
        unsafe { khora_fibers_adopt(nursery, spawn(crew_child)) };
        until_cancelled();
        let _cleanup = Shielded::new();
        CREW_RELEASING.store(1, Ordering::SeqCst);
        // SAFETY: the nursery's last reference.
        unsafe { khora_drop(nursery, Some(release_nursery)) };
        0
    }

    /// Forced while already releasing a nursery, whose crew the release took
    /// out of `OPEN` before it began to wait.
    #[test]
    fn a_force_arriving_while_releasing_a_nursery_reaches_its_children() {
        cancel_then_force_inside(spawn(crew_parent), &CREW_INSIDE, &CREW_RELEASING, &CREW_OUTCOME);
        assert_eq!(CREW_OUTCOME.load(Ordering::SeqCst), STOPPED, "the force never reached the child");
    }

    static FULL_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static FULL_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);
    static FULL_ADOPTING: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn full_child(_code: *const u8, _body: *mut u8) -> u64 {
        shielded_cleanup(&FULL_INSIDE, &FULL_OUTCOME);
        0
    }

    extern "C" fn full_quick(_code: *const u8, _body: *mut u8) -> u64 {
        0
    }

    extern "C" fn full_parent(_code: *const u8, _body: *mut u8) -> u64 {
        let nursery = khora_fibers_open_bounded(1);
        // SAFETY: a live nursery and live handles, whose references it takes;
        // then the nursery's last reference.
        unsafe {
            khora_fibers_adopt(nursery, spawn(full_child));
            until("the child reaching its cleanup", || FULL_INSIDE.load(Ordering::SeqCst) == 1);
            FULL_ADOPTING.store(1, Ordering::SeqCst);
            khora_fibers_adopt(nursery, spawn(full_quick));
            khora_drop(nursery, Some(release_nursery));
        }
        0
    }

    /// Forced while blocked adopting into a full bounded nursery, waiting on
    /// the oldest child, which is in neither `held` nor `joining`.
    #[test]
    fn a_force_arriving_while_adopting_into_a_full_nursery_reaches_the_oldest_child() {
        cancel_then_force_inside(spawn(full_parent), &FULL_INSIDE, &FULL_ADOPTING, &FULL_OUTCOME);
        assert_eq!(FULL_OUTCOME.load(Ordering::SeqCst), STOPPED, "the force never reached the child");
    }

    static DEEP_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static DEEP_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);
    static DEEP_RELEASING: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn deep_grandchild(_code: *const u8, _body: *mut u8) -> u64 {
        shielded_cleanup(&DEEP_INSIDE, &DEEP_OUTCOME);
        0
    }

    /// The middle fiber: holds the grandchild, and once cancelled releases it
    /// in cleanup -- where the force must find it and pass it on again.
    extern "C" fn deep_middle(_code: *const u8, _body: *mut u8) -> u64 {
        let grandchild = spawn(deep_grandchild);
        until_cancelled();
        let _cleanup = Shielded::new();
        // SAFETY: the only reference to a live handle.
        unsafe { crate::fiber::khora_fiber_release(grandchild) };
        0
    }

    extern "C" fn deep_top(_code: *const u8, _body: *mut u8) -> u64 {
        let middle = spawn(deep_middle);
        until_cancelled();
        let _cleanup = Shielded::new();
        DEEP_RELEASING.store(1, Ordering::SeqCst);
        // SAFETY: the only reference to a live handle. Passes the cancel on to
        // the middle fiber, which then begins releasing the grandchild.
        unsafe { crate::fiber::khora_fiber_release(middle) };
        0
    }

    /// **Transitive**: the fiber the force is forwarded to is itself inside a
    /// release, and must forward it again, to the grandchild.
    #[test]
    fn a_force_arriving_in_cleanup_is_passed_on_to_grandchildren() {
        cancel_then_force_inside(spawn(deep_top), &DEEP_INSIDE, &DEEP_RELEASING, &DEEP_OUTCOME);
        assert_eq!(DEEP_OUTCOME.load(Ordering::SeqCst), STOPPED, "the force stopped a generation short");
    }

    // --- a stop delivered by somebody who does not own the handle ---------

    static FREED_CHILD_ID: AtomicUsize = AtomicUsize::new(0);
    static FREED_PARENT_ID: AtomicUsize = AtomicUsize::new(0);
    static FREED_INSIDE: AtomicUsize = AtomicUsize::new(0);
    static FREED_WAITING: AtomicUsize = AtomicUsize::new(0);
    static FREED_OUTCOME: AtomicUsize = AtomicUsize::new(RUNNING);

    extern "C" fn freed_child(_code: *const u8, _body: *mut u8) -> u64 {
        FREED_CHILD_ID.store(crate::current::current(|me| me.id()), Ordering::SeqCst);
        shielded_cleanup(&FREED_INSIDE, &FREED_OUTCOME);
        0
    }

    extern "C" fn freed_parent(_code: *const u8, _body: *mut u8) -> u64 {
        FREED_PARENT_ID.store(crate::current::current(|me| me.id()), Ordering::SeqCst);
        let nursery = khora_fibers_open();
        // SAFETY: a live nursery and a live handle, whose reference the
        // nursery takes; then the nursery's last reference.
        unsafe {
            khora_fibers_adopt(nursery, spawn(freed_child));
            until("the child reaching its cleanup", || FREED_INSIDE.load(Ordering::SeqCst) == 1);
            FREED_WAITING.store(1, Ordering::SeqCst);
            // Joins the child, then frees its handle and state -- while the
            // deliverer below may still be inside `cancel_open_crews`.
            khora_fibers_wait(nursery);
            khora_drop(nursery, Some(release_nursery));
        }
        0
    }

    /// **A stop passed on through a nursery does not read the child's state
    /// after the child's parent has freed it.**
    ///
    /// The `SIGSEGV` behind `cancel_everywhere::a_deadline_ends_a_nursery_
    /// child_stuck_in_cleanup`, made deterministic. `cancel_open_crews` runs
    /// on a thread that owns nothing -- `khora-deadlines`, or whoever cancels
    /// the parent -- and copied the children's raw handles out of the crew.
    /// Forcing the child stops it; its parent's wait returns and frees the
    /// handle and the `FiberState`; and the deliverer then read `state.fiber`
    /// out of the freed block to pass the force on to the child's own
    /// nurseries. Crashing needs the block to have been reused, which is one
    /// run in four in that program and never on demand, so this holds the
    /// deliverer at exactly that point until the parent has freed the state
    /// (`crate::fiber::delivery_probe`) and asks whether it was freed from
    /// under a delivery. On both backends: `KHORA_FIBERS=scheduler` runs it
    /// on the pool.
    #[test]
    fn a_stop_passed_on_through_a_nursery_outlives_the_childs_release() {
        let parent = spawn(freed_parent);
        until("the parent waiting on its child", || FREED_WAITING.load(Ordering::SeqCst) == 1);
        let parent_id = FREED_PARENT_ID.load(Ordering::SeqCst);
        let child_id = FREED_CHILD_ID.load(Ordering::SeqCst);
        // Into its wait, not just about to start it.
        std::thread::sleep(Duration::from_millis(50));

        let deliverer = std::thread::spawn(move || {
            *crate::fiber::delivery_probe::PAUSE.lock().unwrap() = Some((std::thread::current().id(), child_id));
            // What `Deadline::expire` does on the `khora-deadlines` thread.
            cancel_open_crews(parent_id, crate::current::Stop::Force);
        });
        deliverer.join().expect("the deliverer");
        run_to_end(parent);

        assert_eq!(FREED_OUTCOME.load(Ordering::SeqCst), STOPPED, "the force never reached the child");
        assert!(
            crate::fiber::delivery_probe::FREED.lock().unwrap().contains(&child_id),
            "the child's state was never freed, so the question was never asked"
        );
        assert!(
            !crate::fiber::delivery_probe::FREED_WHILE_DELIVERING.lock().unwrap().contains(&child_id),
            "the child's state was freed while a stop was still being delivered through it"
        );
    }
}
