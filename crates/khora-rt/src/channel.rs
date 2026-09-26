//! A bounded channel: the one way a value moves from one fiber to another.
//!
//! `Shared<A>` is a cell two fibers may both change. This is the other half of
//! the problem, and `docs/design/sharing.md` does not list it — it took writing
//! a database driver to find.
//!
//! # What was missing
//!
//! An effect handler must be safe to hand to another fiber, so it may not
//! capture anything writable. A PostgreSQL connection *is* writable — it
//! buffers bytes that arrived and were not yet a whole message — and is also
//! **strictly serial**, since two fibers writing one socket interleave their
//! frames. So a `Db` capability over a connection cannot be written at all:
//!
//! - the handler cannot capture the connection, because it is not `Share`;
//! - `Shared<Connection>` cannot hold it, because `Shared<A>` needs `A: Share`;
//! - and running the query inside `Shared::update` is refused by design — a
//!   change function has no error row, so it cannot fail and cannot do I/O.
//!
//! The missing piece was never a lock but a way for **one fiber to own the
//! resource** and the others to ask it.
//!
//! # Why a channel and not a mutex
//!
//! A mutex would have worked and been smaller. But a lock held across a network
//! round trip is a lock held across code its author did not write — the hazard
//! `shared.rs` calls out about its own critical section — and a bounded channel
//! is needed elsewhere anyway: backpressure is a bounded queue, and a pool of
//! workers is a channel of idle ones. One primitive, three uses.
//!
//! # The shape
//!
//! A queue, a capacity, and two lists of fibers to wake — one waiting to send
//! because the queue is full, one waiting to receive because it is empty. The
//! parking follows `fiber::Done` exactly: enrol the waker **under the same lock
//! that reads the state**, or a value that arrives between the two leaves a
//! fiber parked for ever on an event that already happened.
//!
//! A thread that is not a fiber blocks on a condition variable instead, having
//! no worker to give back. Both happen, since `main` is not a fiber.
//!
//! # What crosses
//!
//! One word, as everywhere else in this runtime, plus — recorded once when the
//! channel is opened — whether it is a pointer and how to release it. A value
//! in the queue is **owned by the queue**: `send` takes the caller's reference
//! and `receive` gives it back, so nothing is duplicated and a value abandoned
//! in a closed channel is released by the close.

use super::*;
use crate::heap::khora_alloc;
use crate::scheduler::{park_current, waker_for_current, Waker};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

/// The tag every channel carries.
const CHANNEL_TAG: u32 = 0;

/// What a send does when the queue is full.
///
/// **A property of the channel, not of the send.** Which one is right is
/// decided by what the queue is *for* -- a request path that must not stall, a
/// metrics feed that would rather lose a sample -- and that is one answer per
/// channel. Deciding it per call would let two senders disagree about whether
/// the queue is lossy, which is not a thing a queue can be halfway.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WhenFull {
    /// Wait for room. The default, and the only one with backpressure.
    Block,
    /// Refuse the value and say so.
    Drop,
    /// Evict the oldest and take the new one.
    Slide,
}

impl WhenFull {
    /// The word the compiler passes, which is the discriminant and nothing
    /// cleverer -- an unknown value is `Block`, because the safe reading of a
    /// number nobody recognises is the one that loses no data.
    fn of(word: i64) -> WhenFull {
        match word {
            1 => WhenFull::Drop,
            2 => WhenFull::Slide,
            _ => WhenFull::Block,
        }
    }
}

/// What a `Channel<A>` holds.
struct Queue {
    items: VecDeque<u64>,
    /// Fibers waiting for room. Woken by a receive.
    senders: Vec<Waker>,
    /// Fibers waiting for a value. Woken by a send.
    receivers: Vec<Waker>,
    /// Threads blocked in [`park_until_moved`] for room, and for a value.
    ///
    /// **What lets a send or receive wake one thread, or none, instead of
    /// all of them.** On the thread backend every fiber is a thread, so a
    /// pool's idle channel has one blocked thread per request waiting for a
    /// connection. Waking all of them for each connection given back made
    /// every one take the lock, find nothing, and block again: a futex storm
    /// that was 42% of a database request's CPU. Counted under the lock the
    /// waiters block with, so a count of zero means nobody is blocked and
    /// nobody can start blocking without first seeing the new state.
    threads_sending: usize,
    threads_receiving: usize,
    /// How many times a blocked thread was woken by a notification rather
    /// than its timeout. Tests only.
    #[cfg(test)]
    woken: usize,
    /// No more values will ever be sent.
    closed: bool,
}

struct Channel {
    state: Mutex<Queue>,
    /// For threads waiting for room. One variable per side, so that a
    /// receive, which can only ever help a sender, never wakes a receiver.
    room: Arc<Condvar>,
    /// For threads waiting for a value.
    arrived: Arc<Condvar>,
    capacity: usize,
    full: WhenFull,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
}

impl Channel {
    /// Wakes one thread blocked for a value, if any is.
    ///
    /// **One, because one value can satisfy one receiver.** A second woken
    /// thread would find the queue empty again and block again, having cost
    /// two context switches. `waiting` is the count read under the lock that
    /// changed the queue, so a thread that blocks later sees the value first
    /// and never waits for this wake.
    fn a_value_arrived(&self, waiting: usize) {
        if waiting > 0 {
            self.arrived.notify_one();
        }
    }

    /// Wakes one thread blocked for room, if any is. [`Self::a_value_arrived`]'s
    /// argument, the other way round.
    fn room_appeared(&self, waiting: usize) {
        if waiting > 0 {
            self.room.notify_one();
        }
    }
}

impl Channel {
    /// Releases a value the queue owned.
    ///
    /// Outside the lock at every call site, because a drop routine may reach a
    /// channel or a cell of its own and a lock held across that is a lock
    /// ordering nobody agreed to. `shared.rs` gives the same reasoning for
    /// releasing after the lock rather than under it.
    fn release(&self, value: u64) {
        if !self.boxed || value == 0 {
            return;
        }
        // SAFETY: `boxed` says the word is a live Khora object, the queue has
        // held a reference to it since it was sent, and `glue` is the routine
        // recorded for exactly this type when the channel was opened.
        unsafe { crate::heap::khora_drop(value as *mut u8, self.glue) };
    }
}

/// The channel behind a handle.
///
/// # Safety
///
/// `handle` must be a live object from [`khora_channel_open`].
unsafe fn channel_of<'a>(handle: *mut u8) -> Option<&'a Channel> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_channel_open` wrote there. Shared rather than exclusive: a
    // channel is `Share`, so another fiber may be inside it right now.
    unsafe { (*handle.add(KHORA_FIELD_OFFSET).cast::<*mut Channel>()).as_ref() }
}

/// Opens a channel that will hold at most `capacity` values.
///
/// **Capacity is at least one.** A zero-capacity channel is a rendezvous, where
/// a send does not complete until a receive begins; that is a different and
/// useful thing, and building it out of this one's parts would mean a sender
/// waiting for a receiver that is itself waiting for a sender. Asking for zero
/// gets one rather than a deadlock, and `std::core` says so.
///
/// # Safety
///
/// `glue` must be the drop routine for the values that will be sent, and
/// `boxed` must say truthfully whether those values are pointers. `strategy`
/// is a [`WhenFull`] discriminant.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_open(
    capacity: i64,
    strategy: i64,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Channel>() as u64, CHANNEL_TAG);
    let channel: Box<Channel> = Box::new(Channel {
        state: Mutex::new(Queue {
            items: VecDeque::new(),
            senders: Vec::new(),
            receivers: Vec::new(),
            threads_sending: 0,
            threads_receiving: 0,
            #[cfg(test)]
            woken: 0,
            closed: false,
        }),
        room: Arc::new(Condvar::new()),
        arrived: Arc::new(Condvar::new()),
        capacity: if capacity < 1 { 1 } else { capacity as usize },
        full: WhenFull::of(strategy),
        boxed,
        glue,
    });
    // SAFETY: `khora_alloc` returned an object with one field's worth of
    // space, zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Channel>().write(Box::into_raw(channel));
    }
    object
}


/// How long a parked fiber waits before looking at its cancellation flag.
///
/// Only a backstop; see [`park_until_moved`]. Long, because the window it
/// covers is a few instructions wide and a shorter one would cost every parked
/// fiber wakeups to catch a case that almost never happens.
///
/// Shared with [`crate::fiber`], whose completion latch closes the same race
/// against the same flag. One number rather than two that drift.
pub(crate) const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(250);

/// Blocks a caller that has no scheduler worker to give back (every fiber on
/// the thread backend; `main` and other plain threads on either) until the
/// side it waits for moves: `moved` is the channel's `arrived` variable for a
/// receiver and its `room` variable for a sender.
///
/// **Two mechanisms, and both are load-bearing.** Registering that condition
/// variable with the fiber is what makes cancellation immediate: `cancel`
/// notifies all of whatever the fiber left there, so a cancelled thread is
/// woken even though a send or receive would wake only one waiter on it, and
/// an idle parked fiber costs nothing until somebody actually cancels it.
///
/// The timeout is what makes it *correct*. A cancellation landing between the
/// caller's flag check and this `wait` would notify a thread that is not
/// waiting yet, and that wake is lost. Closing that window exactly needs the
/// canceller to hold this channel's own lock while it notifies -- and it
/// cannot, because a `Channel` is a raw `Box` with no handle a fiber could
/// keep a reference to. So the registration is the fast path and the timeout
/// is the bound: an ordinary cancellation is observed at once, and the one
/// that loses the race is observed within `LOOK_AGAIN`.
///
/// `waiting` picks the count this thread is in while it blocks, which is what
/// tells the other side whether there is anybody to notify at all.
fn park_until_moved(
    moved: &Arc<Condvar>,
    mut state: std::sync::MutexGuard<'_, Queue>,
    waiting: fn(&mut Queue) -> &mut usize,
) {
    *waiting(&mut state) += 1;
    crate::current::current(|fiber| fiber.park_on(moved));
    let (mut state, _timeout) =
        moved.wait_timeout(state, LOOK_AGAIN).unwrap_or_else(|e| e.into_inner());
    *waiting(&mut state) -= 1;
    #[cfg(test)]
    if !_timeout.timed_out() {
        state.woken += 1;
    }
    drop(state);
    crate::current::current(|fiber| fiber.unpark_from());
}

/// Whether this fiber should give up a wait: [`crate::current::Fiber::gives_up_waiting`].
///
/// The predicate `khora_cancelled` answers with, plus a change function's
/// case, and deliberately one call rather than the same expression written
/// twice: a blocking primitive that gives up on a cancellation a cancellation
/// point would ignore hands back "the channel is closed" for a channel that is
/// open -- which inside a change function is the point, and outside one is a
/// bug.
fn stopping() -> bool {
    crate::current::current(|fiber| fiber.gives_up_waiting())
}

/// Sends a value, waiting while the channel is full.
///
/// Takes ownership: the queue releases it if nobody ever receives it. Answers
/// false when the channel is closed, in which case the value is released here
/// rather than silently kept — a send to a closed channel has nowhere to put
/// it, and leaking it would be the quietest possible failure.
///
/// # Safety
///
/// `handle` must be live, and `value` a live object owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_send(handle: *mut u8, value: u64) -> bool {
    // SAFETY: `handle` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(channel) = (unsafe { channel_of(handle) }) else {
        fatal("sending on a channel that has already been released");
    };

    loop {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            drop(state);
            channel.release(value);
            return false;
        }
        if state.items.len() < channel.capacity {
            state.items.push_back(value);
            let waiting = std::mem::take(&mut state.receivers);
            let threads = state.threads_receiving;
            drop(state);
            channel.a_value_arrived(threads);
            for waker in waiting {
                waker.wake();
            }
            return true;
        }

        // Full, and what that means was decided when the channel was opened.
        //
        // **`Drop` answers false and `Slide` answers true**, and the asymmetry
        // is the point rather than an oversight: a dropping channel makes the
        // loss visible at the send, so a caller can count it, and a sliding one
        // hides it because the whole reason to slide is that the newest value
        // is the one worth having and nobody is going to act on the loss. Pick
        // per queue, knowing which of the two you are getting.
        match channel.full {
            WhenFull::Drop => {
                drop(state);
                channel.release(value);
                return false;
            }
            WhenFull::Slide => {
                let evicted = state.items.pop_front();
                state.items.push_back(value);
                let waiting = std::mem::take(&mut state.receivers);
                let threads = state.threads_receiving;
                drop(state);
                // After the lock, for the reason `release` gives: a drop
                // routine may reach a channel of its own.
                if let Some(old) = evicted {
                    channel.release(old);
                }
                channel.a_value_arrived(threads);
                for waker in waiting {
                    waker.wake();
                }
                return true;
            }
            WhenFull::Block => {}
        }

        // About to wait for room, and a fiber that has been asked to stop
        // should not. The value goes back the same way a closed channel sends
        // it back, for the same reason: a send with nowhere to put its value
        // must not be the quietest possible leak.
        if stopping() {
            drop(state);
            channel.release(value);
            return false;
        }

        // Enrol under the same lock that saw it full, or a receive between the
        // two leaves this fiber parked on room that already exists.
        match waker_for_current() {
            Some(waker) => {
                state.senders.push(waker);
                drop(state);
                park_current();
            }
            // Not a fiber, so there is no worker to give back.
            None => park_until_moved(&channel.room, state, |q| &mut q.threads_sending),
        }
    }
}

/// Takes a value, waiting while the channel is empty.
///
/// Answers false when the channel is closed **and drained**, which is the only
/// honest ordering: values already sent are still worth having, and a reader
/// that stopped at the close would lose them.
///
/// # Safety
///
/// `handle` must be live and `out` a writable word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_receive(handle: *mut u8, out: *mut u64) -> bool {
    // SAFETY: `handle` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(channel) = (unsafe { channel_of(handle) }) else {
        fatal("receiving on a channel that has already been released");
    };

    loop {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = state.items.pop_front() {
            let waiting = std::mem::take(&mut state.senders);
            let threads = state.threads_sending;
            drop(state);
            channel.room_appeared(threads);
            for waker in waiting {
                waker.wake();
            }
            // SAFETY: the caller guarantees a writable word.
            unsafe { out.write(value) };
            return true;
        }
        if state.closed {
            return false;
        }
        // Nothing to take, so this is about to wait -- and a fiber that has
        // been asked to stop should not. **Here rather than after the wait**,
        // so that a send racing the cancellation still wins: the loop retries
        // the queue first and only reaches this once there is genuinely
        // nothing. That is the invariant the caller's cancellation check
        // depends on, because it unwinds -- a cancelled receive must never be
        // holding a value nobody will ever see again.
        if stopping() {
            return false;
        }

        match waker_for_current() {
            Some(waker) => {
                state.receivers.push(waker);
                drop(state);
                park_current();
            }
            None => park_until_moved(&channel.arrived, state, |q| &mut q.threads_receiving),
        }
    }
}

/// Says nothing more will be sent, and releases everyone waiting.
///
/// **Idempotent**, because the owner of a channel and the fiber that finishes
/// with it are often two different pieces of code and neither should have to
/// know whether the other went first.
///
/// Values already in the queue stay there for a reader to drain. What is left
/// when the handle is finally released is freed by [`khora_channel_release`].
///
/// # Safety
///
/// `handle` must be a live object from [`khora_channel_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_close(handle: *mut u8) {
    // SAFETY: `handle` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(channel) = (unsafe { channel_of(handle) }) else { return };
    let (senders, receivers) = {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        (std::mem::take(&mut state.senders), std::mem::take(&mut state.receivers))
    };
    // Everyone, on both sides: a closed channel answers every one of them.
    channel.room.notify_all();
    channel.arrived.notify_all();
    for waker in senders.into_iter().chain(receivers) {
        waker.wake();
    }
}

/// How many values are waiting to be taken.
///
/// For a pool reporting its depth and for tests. Not a synchronisation
/// primitive: the answer is stale the moment it is given, which is true of
/// every such count and is why nothing here branches on one.
///
/// Takes a value if one is already there, and never waits.
///
/// **The answer is "nothing right now", which is not "nothing ever".** A false
/// here means the queue was empty at the instant it was read, and says nothing
/// about whether the channel is closed -- a caller polling a live channel and
/// a caller polling a drained closed one get the same answer, and the way to
/// tell them apart is `receive`, which waits and then says. This exists for the
/// loop that has something else to do rather than for the loop that spins.
///
/// # Safety
///
/// `handle` must be live and `out` a writable word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_poll(handle: *mut u8, out: *mut u64) -> bool {
    // SAFETY: `handle` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(channel) = (unsafe { channel_of(handle) }) else {
        fatal("polling a channel that has already been released");
    };

    let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(value) = state.items.pop_front() else {
        return false;
    };
    // Room appeared, so anybody waiting for it is woken -- exactly as a
    // receive does, because to a blocked sender this *is* a receive.
    let waiting = std::mem::take(&mut state.senders);
    let threads = state.threads_sending;
    drop(state);
    channel.room_appeared(threads);
    for waker in waiting {
        waker.wake();
    }
    // SAFETY: the caller promised a writable word.
    unsafe { out.write(value) };
    true
}

/// # Safety
///
/// `handle` must be a live object from [`khora_channel_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_depth(handle: *mut u8) -> i64 {
    // SAFETY: `handle` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(channel) = (unsafe { channel_of(handle) }) else { return 0 };
    channel.state.lock().unwrap_or_else(|e| e.into_inner()).items.len() as i64
}

/// Frees the channel and everything abandoned in it.
///
/// Called by the drop glue for a channel handle, once nothing refers to it.
///
/// # Safety
///
/// `handle` must be a live object from [`khora_channel_open`], and nothing may
/// use it afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_release(handle: *mut u8) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live handle and that this is the last
    // use of it, so taking the box back is sound.
    let pointer = unsafe { handle.add(KHORA_FIELD_OFFSET).cast::<*mut Channel>() };
    // SAFETY: as above.
    let raw = unsafe { pointer.read() };
    if raw.is_null() {
        return;
    }
    // SAFETY: as above. Cleared first, so a double release finds null.
    unsafe { pointer.write(std::ptr::null_mut()) };
    // SAFETY: the box was leaked by `khora_channel_open` and this is its only
    // owner now.
    let channel = unsafe { Box::from_raw(raw) };

    let abandoned: Vec<u64> = {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        state.items.drain(..).collect()
    };
    for value in abandoned {
        channel.release(value);
    }
}

#[cfg(test)]
mod tests {
    // SAFETY, for every call below: each handle comes from `open` in the same
    // test, is used only while that test holds it, and is released exactly
    // once at the end. The channels are opened unboxed, so no value in them is
    // a pointer and no glue is called.
    use super::*;

    fn open(capacity: i64) -> *mut u8 {
        // Unboxed, so the word is a number and nothing is released.
        unsafe { khora_channel_open(capacity, 0, false, None) }
    }

    fn take(handle: *mut u8) -> Option<u64> {
        let mut out = 0u64;
        if unsafe { khora_channel_receive(handle, &mut out) } {
            Some(out)
        } else {
            None
        }
    }

    #[test]
    fn a_value_sent_is_the_value_received() {
        let channel = open(4);
        assert!(unsafe { khora_channel_send(channel, 7) });
        assert_eq!(take(channel), Some(7));
        unsafe { khora_channel_release(channel) };
    }

    #[test]
    fn values_come_back_in_the_order_they_went_in() {
        let channel = open(8);
        for value in 1..=5 {
            assert!(unsafe { khora_channel_send(channel, value) });
        }
        let got: Vec<u64> = (0..5).filter_map(|_| take(channel)).collect();
        assert_eq!(got, [1, 2, 3, 4, 5]);
        unsafe { khora_channel_release(channel) };
    }

    /// Values already sent are still worth having. A reader that stopped at the
    /// close would lose them.
    #[test]
    fn a_closed_channel_is_drained_before_it_ends() {
        let channel = open(4);
        unsafe { khora_channel_send(channel, 1) };
        unsafe { khora_channel_send(channel, 2) };
        unsafe { khora_channel_close(channel) };
        assert_eq!(take(channel), Some(1));
        assert_eq!(take(channel), Some(2));
        assert_eq!(take(channel), None, "and then it is over");
        unsafe { khora_channel_release(channel) };
    }

    #[test]
    fn sending_to_a_closed_channel_is_refused() {
        let channel = open(4);
        unsafe { khora_channel_close(channel) };
        assert!(!unsafe { khora_channel_send(channel, 1) });
        unsafe { khora_channel_release(channel) };
    }

    /// The owner of a channel and the fiber that finishes with it are often two
    /// different pieces of code, and neither should have to know which went
    /// first.
    #[test]
    fn closing_twice_is_allowed() {
        let channel = open(1);
        unsafe { khora_channel_close(channel) };
        unsafe { khora_channel_close(channel) };
        unsafe { khora_channel_release(channel) };
    }

    /// The backpressure that makes this worth having: a full channel stops the
    /// sender until a reader takes something.
    #[test]
    fn a_full_channel_blocks_the_sender_until_there_is_room() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let channel = open(1) as usize;
        let sent_both = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&sent_both);

        let writer = std::thread::spawn(move || {
            let handle = channel as *mut u8;
            unsafe { khora_channel_send(handle, 1) };
            unsafe { khora_channel_send(handle, 2) };
            flag.store(true, Ordering::Release);
        });

        // The second send cannot have completed: capacity is one and nothing
        // has been taken. Not a sleep-and-hope — the read below is what
        // releases it, and the join proves it was released.
        while unsafe { khora_channel_depth(channel as *mut u8) } == 0 {
            std::hint::spin_loop();
        }
        assert!(!sent_both.load(Ordering::Acquire), "the second send should still be waiting");

        assert_eq!(take(channel as *mut u8), Some(1));
        assert_eq!(take(channel as *mut u8), Some(2));
        writer.join().expect("the writer finishes once there is room");
        assert!(sent_both.load(Ordering::Acquire));
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// A reader waiting on an empty channel is released by a close, or it waits
    /// for a value that is never coming.
    #[test]
    fn closing_releases_a_waiting_reader() {
        let channel = open(1) as usize;
        let reader = std::thread::spawn(move || take(channel as *mut u8));
        // Give the reader time to be waiting rather than not yet started; the
        // close is correct either way, which is what makes this safe to race.
        std::thread::yield_now();
        unsafe { khora_channel_close(channel as *mut u8) };
        assert_eq!(reader.join().expect("the reader is released"), None);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// The count of blocked threads, and of wakes that were notifications
    /// rather than timeouts.
    fn blocked_and_woken(handle: *mut u8) -> (usize, usize) {
        let channel = unsafe { channel_of(handle) }.expect("a live channel");
        let state = channel.state.lock().unwrap();
        (state.threads_receiving + state.threads_sending, state.woken)
    }

    fn until(mut done: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "gave up waiting");
            std::thread::yield_now();
        }
    }

    /// **One value wakes one blocked thread.** On the thread backend a pool's
    /// idle channel has a blocked thread per request waiting for a
    /// connection; waking every one of them for each connection given back
    /// had them all take the lock, find nothing and block again, which was
    /// 42% of a database request's CPU.
    #[test]
    fn one_value_wakes_one_blocked_receiver() {
        const READERS: usize = 8;
        let channel = open(1) as usize;
        let readers: Vec<_> =
            (0..READERS).map(|_| std::thread::spawn(move || take(channel as *mut u8))).collect();
        until(|| blocked_and_woken(channel as *mut u8).0 == READERS);

        assert!(unsafe { khora_channel_send(channel as *mut u8, 1) });
        until(|| blocked_and_woken(channel as *mut u8).0 == READERS - 1);
        // Long enough for every thread a broadcast would have woken to have
        // run: they are runnable the moment the send returns.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let (_, woken) = blocked_and_woken(channel as *mut u8);

        unsafe { khora_channel_close(channel as *mut u8) };
        let got: Vec<_> = readers.into_iter().filter_map(|r| r.join().unwrap()).collect();
        assert_eq!(got, [1]);
        assert_eq!(woken, 1, "one value should wake one thread, not every thread waiting");
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// The other half of waking one: **nobody is left waiting for the
    /// timeout.** Every value must reach a blocked thread by notification; a
    /// wake that went to nobody would show here as a receiver released by its
    /// `LOOK_AGAIN` timeout instead.
    #[test]
    fn every_value_reaches_a_blocked_receiver_by_notification() {
        const READERS: usize = 8;
        let channel = open(1) as usize;
        let readers: Vec<_> =
            (0..READERS).map(|_| std::thread::spawn(move || take(channel as *mut u8))).collect();
        until(|| blocked_and_woken(channel as *mut u8).0 == READERS);

        for value in 0..READERS as u64 {
            assert!(unsafe { khora_channel_send(channel as *mut u8, value) });
        }
        let mut got: Vec<u64> = readers.into_iter().filter_map(|r| r.join().unwrap()).collect();
        got.sort_unstable();
        assert_eq!(got, (0..READERS as u64).collect::<Vec<_>>());
        let (_, woken) = blocked_and_woken(channel as *mut u8);
        assert!(woken >= READERS, "{woken} of {READERS} receivers were woken by a send");
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// The same for room: one receive wakes one blocked sender.
    #[test]
    fn one_receive_wakes_one_blocked_sender() {
        const WRITERS: usize = 6;
        let channel = open(1) as usize;
        assert!(unsafe { khora_channel_send(channel as *mut u8, 100) });
        let writers: Vec<_> = (0..WRITERS as u64)
            .map(|w| std::thread::spawn(move || unsafe { khora_channel_send(channel as *mut u8, w) }))
            .collect();
        until(|| blocked_and_woken(channel as *mut u8).0 == WRITERS);

        assert_eq!(take(channel as *mut u8), Some(100));
        until(|| blocked_and_woken(channel as *mut u8).0 == WRITERS - 1);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let (_, woken) = blocked_and_woken(channel as *mut u8);
        assert_eq!(woken, 1, "one slot of room should wake one sender");

        for _ in 0..WRITERS {
            assert!(take(channel as *mut u8).is_some());
        }
        for writer in writers {
            assert!(writer.join().unwrap());
        }
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A cancel racing a send does not strand a value.** Waking one thread
    /// per value rests on the woken thread taking it. A cancelled receiver
    /// may be the one notified, or may leave while another is; either way a
    /// value must never sit queued while a live receiver stays blocked, which
    /// only the `LOOK_AGAIN` timeout would rescue, 250 ms later.
    ///
    /// Each round blocks 8 cancellable receivers, sends 4 values and cancels
    /// 0 to 2 of them, sometimes before the sends, sometimes during them, and
    /// sometimes while the receivers are still parking. The oracle is read
    /// 120 ms after the last send: under `LOOK_AGAIN`, so only a lost wake
    /// can fail it, and it costs that long only when it fails.
    #[test]
    fn a_cancel_racing_a_send_strands_no_value() {
        use crate::current::{enter, Fiber};
        use std::sync::atomic::{AtomicUsize, Ordering};
        const READERS: usize = 8;
        const SENDS: u64 = 4;
        const STALL: std::time::Duration = std::time::Duration::from_millis(120);
        let mut stalls = Vec::new();
        for round in 0..24usize {
            let cancels = round % 3;
            let channel = open(64) as usize;
            let fibers: Vec<Arc<Fiber>> = (0..READERS).map(|_| Fiber::spawned()).collect();
            let returned = Arc::new(AtomicUsize::new(0));
            let readers: Vec<_> = fibers
                .iter()
                .map(|fiber| {
                    let fiber = Arc::clone(fiber);
                    let returned = Arc::clone(&returned);
                    std::thread::spawn(move || {
                        let _in = enter(fiber);
                        let got = take(channel as *mut u8);
                        returned.fetch_add(1, Ordering::SeqCst);
                        got
                    })
                })
                .collect();
            // Three rounds in four wait until every receiver is blocked; the
            // fourth races the park itself.
            if round % 4 != 3 {
                until(|| blocked_and_woken(channel as *mut u8).0 == READERS);
            }
            let cancel_first = round % 2 == 0;
            if cancel_first {
                (0..cancels).for_each(|victim| fibers[victim].cancel());
            }
            let sender = std::thread::spawn(move || {
                for value in 0..SENDS {
                    assert!(unsafe { khora_channel_send(channel as *mut u8, value) });
                    std::thread::yield_now();
                }
            });
            if !cancel_first {
                (0..cancels).for_each(|victim| fibers[READERS - 1 - victim].cancel());
            }
            sender.join().unwrap();

            let deadline = std::time::Instant::now() + STALL;
            let depth = || unsafe { khora_channel_depth(channel as *mut u8) };
            while depth() > 0
                && returned.load(Ordering::SeqCst) < READERS
                && std::time::Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            if depth() > 0 && returned.load(Ordering::SeqCst) < READERS {
                stalls.push((round, depth(), returned.load(Ordering::SeqCst)));
            }

            unsafe { khora_channel_close(channel as *mut u8) };
            let mut got: Vec<u64> = readers.into_iter().filter_map(|r| r.join().unwrap()).collect();
            while let Some(left) = take(channel as *mut u8) {
                got.push(left);
            }
            got.sort_unstable();
            assert_eq!(got, (0..SENDS).collect::<Vec<_>>(), "round {round}: a value lost or doubled");
            unsafe { khora_channel_release(channel as *mut u8) };
        }
        assert!(
            stalls.is_empty(),
            "a value stayed queued while a receiver was blocked (round, queued, returned): {stalls:?}"
        );
    }

    #[test]
    fn several_writers_and_readers_lose_nothing() {
        const WRITERS: u64 = 4;
        const EACH: u64 = 250;

        let channel = open(8) as usize;
        let writers: Vec<_> = (0..WRITERS)
            .map(|w| {
                std::thread::spawn(move || {
                    for i in 0..EACH {
                        unsafe { khora_channel_send(channel as *mut u8, w * EACH + i) };
                    }
                })
            })
            .collect();

        let mut seen = Vec::new();
        while (seen.len() as u64) < WRITERS * EACH {
            if let Some(value) = take(channel as *mut u8) {
                seen.push(value);
            }
        }
        for writer in writers {
            writer.join().expect("a writer");
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..WRITERS * EACH).collect::<Vec<u64>>());
        unsafe { khora_channel_release(channel as *mut u8) };
    }
}
