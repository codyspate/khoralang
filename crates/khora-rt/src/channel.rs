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
use std::sync::{Condvar, Mutex};

/// The tag every channel carries.
const CHANNEL_TAG: u32 = 0;

/// What a `Channel<A>` holds.
struct Queue {
    items: VecDeque<u64>,
    /// Fibers waiting for room. Woken by a receive.
    senders: Vec<Waker>,
    /// Fibers waiting for a value. Woken by a send.
    receivers: Vec<Waker>,
    /// No more values will ever be sent.
    closed: bool,
}

struct Channel {
    state: Mutex<Queue>,
    /// For waiters that are threads rather than fibers.
    moved: Condvar,
    capacity: usize,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
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
/// `boxed` must say truthfully whether those values are pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_open(
    capacity: i64,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Channel>() as u64, CHANNEL_TAG);
    let channel: Box<Channel> = Box::new(Channel {
        state: Mutex::new(Queue {
            items: VecDeque::new(),
            senders: Vec::new(),
            receivers: Vec::new(),
            closed: false,
        }),
        moved: Condvar::new(),
        capacity: if capacity < 1 { 1 } else { capacity as usize },
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
            drop(state);
            channel.moved.notify_all();
            for waker in waiting {
                waker.wake();
            }
            return true;
        }

        // Full. Enrol under the same lock that saw it full, or a receive
        // between the two leaves this fiber parked on room that already exists.
        match waker_for_current() {
            Some(waker) => {
                state.senders.push(waker);
                drop(state);
                park_current();
            }
            None => {
                // Not a fiber, so there is no worker to give back.
                let _unused = channel
                    .moved
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
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
    let Some(channel) = (unsafe { channel_of(handle) }) else {
        fatal("receiving on a channel that has already been released");
    };

    loop {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = state.items.pop_front() {
            let waiting = std::mem::take(&mut state.senders);
            drop(state);
            channel.moved.notify_all();
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

        match waker_for_current() {
            Some(waker) => {
                state.receivers.push(waker);
                drop(state);
                park_current();
            }
            None => {
                let _unused = channel
                    .moved
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
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
    let Some(channel) = (unsafe { channel_of(handle) }) else { return };
    let (senders, receivers) = {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        (std::mem::take(&mut state.senders), std::mem::take(&mut state.receivers))
    };
    channel.moved.notify_all();
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
/// # Safety
///
/// `handle` must be a live object from [`khora_channel_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_channel_depth(handle: *mut u8) -> i64 {
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
        unsafe { khora_channel_open(capacity, false, None) }
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
