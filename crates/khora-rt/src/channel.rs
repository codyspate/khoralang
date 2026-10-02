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
//! parking follows `fiber::Done` exactly: enroll the waker **under the same lock
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
pub(crate) enum WhenFull {
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
    /// number nobody recognizes is the one that loses no data.
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
    /// Fibers waiting for room, oldest first. A receive wakes one.
    senders: VecDeque<Waker>,
    /// Fibers waiting for a value, oldest first. A send wakes one.
    ///
    /// **One, and every entry is a fiber that is still waiting.** Woken all
    /// at once, every receiver but one found the queue empty and parked
    /// again -- on a pool's idle channel, a hundred wakes for each
    /// connection handed back. Waking one is only safe if the one woken is
    /// still there to take the value, so a fiber takes its own entry out
    /// every time it comes back to the lock ([`withdraw`]), whether a send,
    /// a cancel or anything else woke it. An entry is therefore a fiber that
    /// has not looked at the queue since it enrolled, and it will look.
    receivers: VecDeque<Waker>,
    /// Receivers a send picked and woke, each owed one of the values in
    /// `items`.
    ///
    /// **What stops a receive that never waited from taking a woken
    /// receiver's value.** A send wakes the receiver that has waited longest,
    /// but that fiber runs only when its worker reaches it, and any fiber that
    /// runs first and receives found the value sitting there. The woken one
    /// then found the queue empty and enrolled again at the back, behind the
    /// fiber that took it: on a pool's idle channel under load the same
    /// request lost that race several times in a row, and those requests
    /// were the server's slowest 1% -- p99 twice the p90 the others saw.
    /// A value is only free to take while `items` holds more than this list
    /// has entries; a fiber listed here takes one whatever the count.
    ///
    /// Never longer than `items`: an entry is added with a value, and a send
    /// that adds no value (a full sliding channel) adds no entry. What it
    /// costs: a scan of this list, usually one or two long, on every return
    /// to the lock after a park.
    handed: Vec<usize>,
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
    /// How many waits in [`park_until_moved`] ended before their timeout.
    ///
    /// **Only a floor on the notifications delivered**, which is all
    /// `every_value_reaches_a_blocked_receiver_by_notification` asks of it: a
    /// condition variable may also wake a thread nobody notified, and that is
    /// counted here too. So this can say no wake was lost, and cannot say how
    /// many wakes were sent -- [`Notified`] says that. Tests only.
    #[cfg(test)]
    woken: usize,
    /// No more values will ever be sent.
    closed: bool,
}

pub(crate) struct Channel {
    state: Mutex<Queue>,
    /// For threads waiting for room. One variable per side, so that a
    /// receive, which can only ever help a sender, never wakes a receiver.
    room: Notified,
    /// For threads waiting for a value.
    arrived: Notified,
    capacity: usize,
    full: WhenFull,
    pub(crate) boxed: bool,
    pub(crate) glue: Option<extern "C" fn(*mut u8)>,
    /// What a `Handoff` knows about the type it carries, or `None` for a
    /// `Channel`. See `crate::handoff`.
    ///
    /// **The one field the two queues differ by.** A hand-off is this queue
    /// with a different send and receive; its waits, wakes, close and release
    /// are the channel's, so the parking rules `withdraw` and
    /// `park_until_moved` document hold for it without a second copy.
    pub(crate) handing: Option<&'static crate::handoff::HandoffType>,
}

/// How many of the threads blocked on a condition variable to wake.
///
/// Shared with [`crate::current::Fiber::stop`], so that a cancellation's wake
/// is a value its test can read, like a channel's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wake {
    /// One value, or one slot of room, can help one thread.
    One,
    /// A close is news to every thread blocked on either side. A cancel is
    /// news to one thread that cannot be told apart from the others blocked
    /// with it, so waking one might wake the wrong one.
    All,
}

impl Wake {
    /// The one place a [`Wake`] becomes a call.
    pub(crate) fn notify(self, moved: &Condvar) {
        match self {
            Wake::One => moved.notify_one(),
            Wake::All => moved.notify_all(),
        }
    }
}

/// One side's condition variable, which under test remembers every decision
/// to notify it or not.
///
/// **What the one-wake tests measure, because what a waiter sees cannot be.**
/// A condition variable may wake a thread nobody notified, and on Windows it
/// does: counted at the waiter, 6 blocked senders once showed 50 wakes for one
/// receive. Counted here, a spurious wakeup is not in the record at all. What
/// it cannot show is that the operating system delivered the wake it was
/// asked for, or a notification sent to [`Notified::condvar`] directly --
/// which [`crate::current::Fiber::stop`] does, and records itself.
struct Notified {
    condvar: Arc<Condvar>,
    #[cfg(test)]
    notices: Mutex<Vec<Notice>>,
}

/// One decision: how many threads the channel counted as blocked on that
/// side, under its lock, and what it woke -- `None` being nobody.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Notice {
    wake: Option<Wake>,
    waiting: usize,
}

impl Notified {
    fn new() -> Notified {
        Notified {
            condvar: Arc::new(Condvar::new()),
            #[cfg(test)]
            notices: Mutex::new(Vec::new()),
        }
    }

    /// Wakes `wake` of the threads blocked here, or nobody. `waiting` is
    /// only recorded.
    fn notify(
        &self,
        wake: Option<Wake>,
        #[cfg_attr(not(test), allow(unused_variables))] waiting: usize,
    ) {
        #[cfg(test)]
        self.notices.lock().unwrap_or_else(|e| e.into_inner()).push(Notice { wake, waiting });
        if let Some(wake) = wake {
            wake.notify(&self.condvar);
        }
    }

    /// The variable itself, to wait on and to register with a fiber.
    fn condvar(&self) -> &Arc<Condvar> {
        &self.condvar
    }
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
        self.arrived.notify((waiting > 0).then_some(Wake::One), waiting);
    }

    /// Wakes one thread blocked for room, if any is. [`Self::a_value_arrived`]'s
    /// argument, the other way round.
    fn room_appeared(&self, waiting: usize) {
        self.room.notify((waiting > 0).then_some(Wake::One), waiting);
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
        // A value a hand-off gave away carries the in-transit owner in a debug
        // build, and whoever releases it here -- a closed send, a canceled
        // send, the last holder of the handle -- is not that owner.
        if let Some(carried) = self.handing {
            // SAFETY: as below; the value is live and `carried` describes it.
            unsafe { crate::handoff::adopt(value as *mut u8, carried) };
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
pub(crate) unsafe fn channel_of<'a>(handle: *mut u8) -> Option<&'a Channel> {
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
// SHARE: takes a drop routine, not a value; the handle it makes is born shared, in `open`.
pub unsafe extern "C" fn khora_channel_open(
    capacity: i64,
    strategy: i64,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    open(capacity, WhenFull::of(strategy), boxed, glue, None)
}

/// Opens a queue: a channel when `handing` is `None`, a hand-off otherwise.
pub(crate) fn open(
    capacity: i64,
    full: WhenFull,
    boxed: bool,
    glue: Option<extern "C" fn(*mut u8)>,
    handing: Option<&'static crate::handoff::HandoffType>,
) -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Channel>() as u64, CHANNEL_TAG);
    crate::share::born_shared(object);
    let channel: Box<Channel> = Box::new(Channel {
        state: Mutex::new(Queue {
            items: VecDeque::new(),
            senders: VecDeque::new(),
            receivers: VecDeque::new(),
            handed: Vec::new(),
            threads_sending: 0,
            threads_receiving: 0,
            #[cfg(test)]
            woken: 0,
            closed: false,
        }),
        room: Notified::new(),
        arrived: Notified::new(),
        capacity: if capacity < 1 { 1 } else { capacity as usize },
        full,
        boxed,
        glue,
        handing,
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
/// notifies all of whatever the fiber left there, so a canceled thread is
/// woken even though a send or receive would wake only one waiter on it, and
/// an idle parked fiber costs nothing until somebody actually cancels it.
///
/// The timeout is what makes it *correct*. A cancellation landing between the
/// caller's flag check and this `wait` would notify a thread that is not
/// waiting yet, and that wake is lost. Closing that window exactly needs the
/// canceler to hold this channel's own lock while it notifies -- and it
/// cannot, because a `Channel` is a raw `Box` with no handle a fiber could
/// keep a reference to. So the registration is the fast path and the timeout
/// is the bound: an ordinary cancellation is observed at once, and the one
/// that loses the race is observed within `LOOK_AGAIN`.
///
/// `waiting` picks the count this thread is in while it blocks, which is what
/// tells the other side whether there is anybody to notify at all.
fn park_until_moved(
    moved: &Notified,
    mut state: std::sync::MutexGuard<'_, Queue>,
    waiting: fn(&mut Queue) -> &mut usize,
) {
    *waiting(&mut state) += 1;
    crate::current::current(|fiber| fiber.park_on(moved.condvar()));
    let (mut state, _timeout) =
        moved.condvar().wait_timeout(state, LOOK_AGAIN).unwrap_or_else(|e| e.into_inner());
    *waiting(&mut state) -= 1;
    #[cfg(test)]
    if !_timeout.timed_out() {
        state.woken += 1;
    }
    drop(state);
    crate::current::current(|fiber| fiber.unpark_from());
}

/// Takes this fiber's own entry out of `waiting`, if it left one.
///
/// **What makes waking one waiter safe.** A fiber enrolls, parks, and is
/// woken -- by the send or receive that picked it, by a cancel, or by
/// anything else. If it then left the channel while its entry stayed, the
/// next send could pick that entry and wake a fiber that has gone, and the
/// value would sit queued behind receivers that are still waiting. Called
/// under the channel's lock on every return to it after a park, before the
/// queue is looked at, so an entry never outlives its fiber's next look.
///
/// What it costs: a scan of the waiting list, once per wake of a fiber that
/// had enrolled. A fiber the send picked is at the front.
fn withdraw(waiting: &mut VecDeque<Waker>, enrolled: &mut Option<usize>) {
    if let Some(fiber) = enrolled.take() {
        if let Some(at) = waiting.iter().position(|w| w.fiber() == fiber) {
            waiting.remove(at);
        }
    }
}

/// Picks the receiver that has waited longest to be woken for the value just
/// queued, and records that the value is its.
///
/// Picks nobody when every queued value is already owed to a woken receiver,
/// which only a sliding channel reaches: its send replaced a value rather than
/// adding one, so there is nothing new to give.
fn hand_to_next(state: &mut Queue) -> Option<Waker> {
    if state.handed.len() >= state.items.len() {
        return None;
    }
    let next = state.receivers.pop_front()?;
    state.handed.push(next.fiber());
    Some(next)
}

/// Who is asking [`take_for`] for a value.
#[derive(Clone, Copy)]
enum Taker {
    /// A fiber that enrolled as this id and parked, and is back.
    Woken(usize),
    /// A fiber that has not parked in this call.
    Fresh,
    /// Not a fiber: a thread, which a send never picks.
    Thread,
}

impl Taker {
    fn of(parked_as: Option<usize>) -> Taker {
        match parked_as {
            Some(fiber) => Taker::Woken(fiber),
            None if crate::coro::on_a_fiber() => Taker::Fresh,
            None => Taker::Thread,
        }
    }
}

/// The oldest value, if this taker may have it.
///
/// A fiber a send handed a value to always gets one, and another fiber gets
/// one only while more are queued than are owed ([`Queue::handed`]).
///
/// **A thread takes any value, and the newest promise goes.** A thread is
/// never on the receivers list, so nothing is ever handed to it, and a
/// promise to a fiber that never runs again -- its pool stopped with it
/// parked -- would otherwise keep that value from a thread for ever. The
/// fiber whose promise went finds nothing and waits again, which is what
/// every woken receiver did before promises existed.
///
/// **A woken receiver is owed *a* value, not the one its send queued.**
/// Values come out oldest first whoever takes them, so the order the channel
/// promises is kept whichever woken receiver runs first.
fn take_for(state: &mut Queue, taker: Taker) -> Option<u64> {
    match taker {
        Taker::Woken(fiber) => match state.handed.iter().position(|&f| f == fiber) {
            Some(at) => {
                state.handed.remove(at);
            }
            None if state.items.len() <= state.handed.len() => return None,
            None => {}
        },
        Taker::Fresh => {
            if state.items.len() <= state.handed.len() {
                return None;
            }
        }
        Taker::Thread => {
            if !state.items.is_empty() && state.items.len() <= state.handed.len() {
                state.handed.pop();
            }
        }
    }
    state.items.pop_front()
}

/// Whether this fiber should give up a wait: [`crate::current::Fiber::gives_up_waiting`].
///
/// The predicate `khora_canceled` answers with, plus a change function's
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
    // Whoever receives it is another fiber, so it is marked before the queue's
    // lock publishes it. Even a send that finds the channel closed marks it:
    // the value is then released here, which a shared value survives.
    if channel.boxed {
        // SAFETY: the caller owns `value`, a live object, and `glue` is how
        // the channel was told to release one.
        unsafe { crate::share::khora_share(value as *mut u8, channel.glue) };
    }
    enqueue(channel, value)
}

/// Puts a value the caller has made ready to cross into the queue, waiting
/// while it is full. The half of a send a channel and a hand-off share: they
/// differ in what they do to the value first, and in nothing after.
pub(crate) fn enqueue(channel: &Channel, value: u64) -> bool {
    let mut enrolled = None;
    loop {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        withdraw(&mut state.senders, &mut enrolled);
        if state.closed {
            drop(state);
            channel.release(value);
            return false;
        }
        if state.items.len() < channel.capacity {
            state.items.push_back(value);
            let next = hand_to_next(&mut state);
            let threads = state.threads_receiving;
            drop(state);
            channel.a_value_arrived(threads);
            if let Some(waker) = next {
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
                let next = hand_to_next(&mut state);
                let threads = state.threads_receiving;
                drop(state);
                // After the lock, for the reason `release` gives: a drop
                // routine may reach a channel of its own.
                if let Some(old) = evicted {
                    channel.release(old);
                }
                channel.a_value_arrived(threads);
                if let Some(waker) = next {
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

        // Enroll under the same lock that saw it full, or a receive between the
        // two leaves this fiber parked on room that already exists.
        match waker_for_current() {
            Some(waker) => {
                enrolled = Some(waker.fiber());
                state.senders.push_back(waker);
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
// SHARE: hands out a value the entry that stored it already marked; stores nothing.
pub unsafe extern "C" fn khora_channel_receive(handle: *mut u8, out: *mut u64) -> bool {
    // SAFETY: `handle` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(channel) = (unsafe { channel_of(handle) }) else {
        fatal("receiving on a channel that has already been released");
    };
    // SAFETY: the caller guarantees a writable word.
    unsafe { dequeue(channel, out) }
}

/// Takes a value out of the queue, waiting while it is empty: the half of a
/// receive a channel and a hand-off share.
///
/// # Safety
///
/// `out` must be a writable word.
pub(crate) unsafe fn dequeue(channel: &Channel, out: *mut u64) -> bool {
    let mut enrolled = None;
    loop {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        let parked_as = enrolled;
        withdraw(&mut state.receivers, &mut enrolled);
        if let Some(value) = take_for(&mut state, Taker::of(parked_as)) {
            let next = state.senders.pop_front();
            let threads = state.threads_sending;
            drop(state);
            channel.room_appeared(threads);
            if let Some(waker) = next {
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
        // depends on, because it unwinds -- a canceled receive must never be
        // holding a value nobody will ever see again.
        if stopping() {
            return false;
        }

        match waker_for_current() {
            Some(waker) => {
                enrolled = Some(waker.fiber());
                state.receivers.push_back(waker);
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
// SHARE: acts on a handle, which is born shared; stores nothing another fiber can reach.
pub unsafe extern "C" fn khora_channel_close(handle: *mut u8) {
    // SAFETY: `handle` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(channel) = (unsafe { channel_of(handle) }) else { return };
    let (senders, receivers, sending, receiving) = {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        (
            std::mem::take(&mut state.senders),
            std::mem::take(&mut state.receivers),
            state.threads_sending,
            state.threads_receiving,
        )
    };
    // Everyone, on both sides, whatever the counts say: a closed channel
    // answers every one of them.
    channel.room.notify(Some(Wake::All), sending);
    channel.arrived.notify(Some(Wake::All), receiving);
    for waker in senders.into_iter().chain(receivers) {
        waker.wake();
    }
}

/// How many values are waiting to be taken.
///
/// For a pool reporting its depth and for tests. Not a synchronization
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
// SHARE: hands out a value the entry that stored it already marked; stores nothing.
pub unsafe extern "C" fn khora_channel_poll(handle: *mut u8, out: *mut u64) -> bool {
    // SAFETY: `handle` is live, which is this function's own documented
    // precondition and the one thing a C caller can get wrong.
    let Some(channel) = (unsafe { channel_of(handle) }) else {
        fatal("polling a channel that has already been released");
    };

    let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
    // A fiber's poll never takes a value a send handed to a parked receiver:
    // it has not waited for anything.
    let Some(value) = take_for(&mut state, Taker::of(None)) else {
        return false;
    };
    // Room appeared, so one sender waiting for it is woken -- exactly as a
    // receive does, because to a blocked sender this *is* a receive.
    let next = state.senders.pop_front();
    let threads = state.threads_sending;
    drop(state);
    channel.room_appeared(threads);
    if let Some(waker) = next {
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
// SHARE: acts on a handle, which is born shared; stores nothing another fiber can reach.
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
// SHARE: releases; publishes nothing.
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

    /// How many threads are blocked in the channel, on either side.
    fn blocked(handle: *mut u8) -> usize {
        let channel = unsafe { channel_of(handle) }.expect("a live channel");
        let state = channel.state.lock().unwrap();
        state.threads_receiving + state.threads_sending
    }

    /// How many waits ended before their timeout; see [`Queue::woken`].
    fn woken(handle: *mut u8) -> usize {
        let channel = unsafe { channel_of(handle) }.expect("a live channel");
        let woken = channel.state.lock().unwrap().woken;
        woken
    }

    /// Every notification sent so far to one side: `room` or `arrived`.
    fn notices(handle: *mut u8, side: fn(&Channel) -> &Notified) -> Vec<Notice> {
        let channel = unsafe { channel_of(handle) }.expect("a live channel");
        let notices = side(channel).notices.lock().unwrap().clone();
        notices
    }

    fn until(mut done: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "gave up waiting");
            std::thread::yield_now();
        }
    }

    /// Whether a send or receive decided right: one `notify_one` when it
    /// found threads blocked, and nothing when it found none.
    ///
    /// **Read at the notifier, so a spurious wakeup cannot fail it.** This
    /// was counted at the waiters, and on Windows 6 blocked senders were once
    /// counted as 50 wakes for one receive: a thread the operating system
    /// woke for nothing found no room, blocked again and was counted again. A
    /// broadcast fails it on the first notice.
    ///
    /// `waiting` is not pinned to the number of threads started. It is read
    /// under the lock the waiters block with, and a thread woken spuriously
    /// just then has left the count until it blocks again; if all of them had,
    /// waking nobody would be right.
    fn woke_one_if_any(notice: &Notice) -> bool {
        match *notice {
            Notice { wake: Some(Wake::One), waiting } => waiting > 0,
            Notice { wake: None, waiting } => waiting == 0,
            Notice { wake: Some(Wake::All), .. } => false,
        }
    }

    /// The one notice a single send or receive left on `side`, checked.
    fn assert_woke_one(notices: &[Notice], what: &str) {
        match notices {
            [notice] if woke_one_if_any(notice) => {}
            other => panic!("{what} should notify one blocked thread, once: {other:?}"),
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
        until(|| blocked(channel as *mut u8) == READERS);

        assert!(unsafe { khora_channel_send(channel as *mut u8, 1) });
        assert_woke_one(&notices(channel as *mut u8, |c| &c.arrived), "one value");

        unsafe { khora_channel_close(channel as *mut u8) };
        let got: Vec<_> = readers.into_iter().filter_map(|r| r.join().unwrap()).collect();
        assert_eq!(got, [1]);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// The other half of waking one: **nobody is left waiting for the
    /// timeout.** A send that notified nobody, or a thread nobody, would
    /// leave a receiver to its `LOOK_AGAIN` timeout, and it would be missing
    /// from `woken`.
    ///
    /// Counted at the waiters, because only they can say a wake arrived. A
    /// spurious wakeup inflates the count, which can hide a lost wake but
    /// cannot fail this. The notices say the other half: whenever a send
    /// found a receiver blocked, it woke one and not all of them.
    #[test]
    fn every_value_reaches_a_blocked_receiver_by_notification() {
        const READERS: usize = 8;
        let channel = open(1) as usize;
        let readers: Vec<_> =
            (0..READERS).map(|_| std::thread::spawn(move || take(channel as *mut u8))).collect();
        until(|| blocked(channel as *mut u8) == READERS);

        for value in 0..READERS as u64 {
            assert!(unsafe { khora_channel_send(channel as *mut u8, value) });
        }
        let mut got: Vec<u64> = readers.into_iter().filter_map(|r| r.join().unwrap()).collect();
        got.sort_unstable();
        assert_eq!(got, (0..READERS as u64).collect::<Vec<_>>());
        let woken = woken(channel as *mut u8);
        assert!(woken >= READERS, "{woken} of {READERS} receivers were woken by a send");
        let sent = notices(channel as *mut u8, |c| &c.arrived);
        assert!(
            sent.len() == READERS && sent.iter().all(woke_one_if_any),
            "each send should wake one receiver if any was blocked: {sent:?}"
        );
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
        until(|| blocked(channel as *mut u8) == WRITERS);

        assert_eq!(take(channel as *mut u8), Some(100));
        assert_woke_one(&notices(channel as *mut u8, |c| &c.room), "one slot of room");

        for _ in 0..WRITERS {
            assert!(take(channel as *mut u8).is_some());
        }
        for writer in writers {
            assert!(writer.join().unwrap());
        }
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A close wakes everyone, on both sides.** It is news to every thread
    /// blocked on the channel, and a close that woke one would leave the rest
    /// to their `LOOK_AGAIN` timeout.
    #[test]
    fn closing_wakes_every_blocked_thread_on_both_sides() {
        const EACH: usize = 3;
        let full = open(1) as usize;
        let empty = open(1) as usize;
        assert!(unsafe { khora_channel_send(full as *mut u8, 100) });
        let writers: Vec<_> = (0..EACH as u64)
            .map(|w| std::thread::spawn(move || unsafe { khora_channel_send(full as *mut u8, w) }))
            .collect();
        let readers: Vec<_> =
            (0..EACH).map(|_| std::thread::spawn(move || take(empty as *mut u8))).collect();
        until(|| blocked(full as *mut u8) == EACH && blocked(empty as *mut u8) == EACH);

        for channel in [full, empty] {
            unsafe { khora_channel_close(channel as *mut u8) };
            let sides: [fn(&Channel) -> &Notified; 2] = [|c| &c.room, |c| &c.arrived];
            for side in sides {
                let sent: Vec<Option<Wake>> =
                    notices(channel as *mut u8, side).iter().map(|n| n.wake).collect();
                assert_eq!(
                    sent.last(),
                    Some(&Some(Wake::All)),
                    "a close should wake every thread on a side"
                );
            }
        }
        assert!(writers.into_iter().all(|w| !w.join().unwrap()), "a closed channel refuses");
        assert!(readers.into_iter().all(|r| r.join().unwrap().is_none()));
        for channel in [full, empty] {
            unsafe { khora_channel_release(channel as *mut u8) };
        }
    }

    /// **A cancel wakes every thread on the variable its fiber is parked on**,
    /// not one: the canceled thread cannot be singled out from the others
    /// blocked there, and `notify_one` could wake one of them and leave it to
    /// its `LOOK_AGAIN` timeout.
    #[test]
    fn canceling_a_blocked_receiver_wakes_every_thread_on_its_side() {
        use crate::current::{enter, Fiber};
        let channel = open(1) as usize;
        let fiber = Fiber::spawned();
        let reader = {
            let fiber = Arc::clone(&fiber);
            std::thread::spawn(move || {
                let _in = enter(fiber);
                take(channel as *mut u8)
            })
        };
        // Blocked means `park_on` has run: it is under the lock the count is.
        until(|| blocked(channel as *mut u8) == 1);

        fiber.cancel();
        assert_eq!(*fiber.stop_wakes.lock().unwrap(), [Wake::All]);
        assert_eq!(reader.join().unwrap(), None, "a canceled receive gives up");
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A cancel racing a send does not strand a value.** Waking one thread
    /// per value rests on the woken thread taking it. A canceled receiver
    /// may be the one notified, or may leave while another is; either way a
    /// value must never sit queued while a live receiver stays blocked, which
    /// only the `LOOK_AGAIN` timeout would rescue, 250 ms later.
    ///
    /// Each round blocks 8 cancelable receivers, sends 4 values and cancels
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
                until(|| blocked(channel as *mut u8) == READERS);
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

    // --- fibers on the scheduler: one wake per value ---------------------------

    use crate::coro::Task;
    use crate::scheduler::Scheduler;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Spawns `count` fibers that each receive once from `channel`, and waits
    /// until every one of them is parked in the receive. Answers how many
    /// have received, and how many got nothing (a close, or a cancel).
    fn parked_receivers(
        pool: &Scheduler,
        channel: usize,
        count: usize,
    ) -> (Arc<AtomicUsize>, Arc<AtomicUsize>, Vec<usize>) {
        let got = Arc::new(AtomicUsize::new(0));
        let none = Arc::new(AtomicUsize::new(0));
        let mut ids = Vec::new();
        for _ in 0..count {
            let (got, none) = (got.clone(), none.clone());
            let task = Task::new(move || match take(channel as *mut u8) {
                Some(_) => {
                    got.fetch_add(1, Ordering::SeqCst);
                }
                None => {
                    none.fetch_add(1, Ordering::SeqCst);
                }
            });
            ids.push(task.fiber().id());
            pool.spawn(task);
        }
        until(|| pool.audit().parked == count);
        (got, none, ids)
    }

    /// Waits, with a deadline that turns a hang into a message, for `done`.
    fn settled_on(pool: &Scheduler, what: &str, mut done: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "{what}: {:?} {:?}",
                pool.counts(),
                pool.audit()
            );
            std::thread::yield_now();
        }
    }

    /// **One value wakes one parked fiber**, as it wakes one blocked thread.
    ///
    /// Woken all at once, every receiver but one found the queue empty and
    /// parked again: on a pool's idle channel under 256 connections, about a
    /// hundred wakes for each connection handed back, each a resume, a lock
    /// and a park.
    #[test]
    fn one_value_wakes_one_parked_fiber() {
        const READERS: usize = 16;
        let pool = Scheduler::started(1, true);
        let channel = open(1) as usize;
        let (got, _, _) = parked_receivers(&pool, channel, READERS);

        let before = pool.counts().wakes;
        assert!(unsafe { khora_channel_send(channel as *mut u8, 1) });
        settled_on(&pool, "the value was never taken", || got.load(Ordering::SeqCst) == 1);
        settled_on(&pool, "the rest never parked again", || pool.audit().parked == READERS - 1);
        let wakes = pool.counts().wakes - before;
        assert_eq!(wakes, 1, "one send woke {wakes} of {READERS} parked receivers");

        unsafe { khora_channel_close(channel as *mut u8) };
        settled_on(&pool, "the close left a receiver parked", || pool.audit().parked == 0);
        drop(pool);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A close wakes every parked fiber**, on both sides, and each gets the
    /// closed answer.
    #[test]
    fn closing_wakes_every_parked_fiber() {
        const EACH: usize = 6;
        let pool = Scheduler::started(2, true);
        let empty = open(1) as usize;
        let (_, none, _) = parked_receivers(&pool, empty, EACH);

        let full = open(1) as usize;
        assert!(unsafe { khora_channel_send(full as *mut u8, 100) });
        let refused = Arc::new(AtomicUsize::new(0));
        for n in 0..EACH as u64 {
            let refused = refused.clone();
            pool.spawn(Task::new(move || {
                if !unsafe { khora_channel_send(full as *mut u8, n) } {
                    refused.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        settled_on(&pool, "the senders never parked", || pool.audit().parked == 2 * EACH);

        unsafe { khora_channel_close(empty as *mut u8) };
        unsafe { khora_channel_close(full as *mut u8) };
        settled_on(&pool, "a close left somebody parked", || {
            none.load(Ordering::SeqCst) == EACH && refused.load(Ordering::SeqCst) == EACH
        });
        drop(pool);
        unsafe { khora_channel_release(empty as *mut u8) };
        unsafe { khora_channel_release(full as *mut u8) };
    }

    /// **The same for room: one receive wakes one parked sender**, and every
    /// value still arrives, in the order the channel took them.
    #[test]
    fn one_receive_wakes_one_parked_sender() {
        const WRITERS: usize = 12;
        let pool = Scheduler::started(1, true);
        let channel = open(1) as usize;
        assert!(unsafe { khora_channel_send(channel as *mut u8, 1000) });
        let sent = Arc::new(AtomicUsize::new(0));
        for n in 0..WRITERS as u64 {
            let sent = sent.clone();
            pool.spawn(Task::new(move || {
                assert!(unsafe { khora_channel_send(channel as *mut u8, n) });
                sent.fetch_add(1, Ordering::SeqCst);
            }));
        }
        settled_on(&pool, "the senders never parked", || pool.audit().parked == WRITERS);

        let before = pool.counts().wakes;
        assert_eq!(take(channel as *mut u8), Some(1000));
        settled_on(&pool, "no sender filled the room", || sent.load(Ordering::SeqCst) == 1);
        settled_on(&pool, "the rest never parked again", || pool.audit().parked == WRITERS - 1);
        let wakes = pool.counts().wakes - before;
        assert_eq!(wakes, 1, "one slot of room woke {wakes} of {WRITERS} parked senders");

        let mut got = Vec::new();
        while got.len() < WRITERS {
            got.push(take(channel as *mut u8).expect("a value"));
        }
        got.sort_unstable();
        assert_eq!(got, (0..WRITERS as u64).collect::<Vec<_>>());
        drop(pool);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A receiver that leaves empty-handed takes no later value's wake
    /// with it.** Waking one receiver per value rests on the woken one being
    /// a receiver that is still there. R1 is canceled while it waits: the
    /// cancel wakes it, it finds nothing and leaves. The next value must go
    /// to R2 or R3, which are still parked -- a send that woke R1's leftover
    /// enrollment instead would wake a fiber that has gone, and the value
    /// would sit queued behind two waiting receivers.
    #[test]
    fn a_receiver_that_left_takes_no_later_wake_with_it() {
        for wake_local in [true, false] {
            let pool = Scheduler::started(1, wake_local);
            let channel = open(4) as usize;
            let (got, none, ids) = parked_receivers(&pool, channel, 3);

            pool.cancel_fiber(ids[0]);
            settled_on(&pool, "the canceled receiver never left", || none.load(Ordering::SeqCst) == 1);
            assert!(unsafe { khora_channel_send(channel as *mut u8, 7) });
            settled_on(&pool, "the value stayed queued while two receivers waited", || {
                got.load(Ordering::SeqCst) == 1
            });

            unsafe { khora_channel_close(channel as *mut u8) };
            settled_on(&pool, "the close left a receiver parked", || none.load(Ordering::SeqCst) == 2);
            drop(pool);
            unsafe { khora_channel_release(channel as *mut u8) };
        }
    }

    /// **The same on the sending side**: a sender canceled while it waits
    /// for room leaves, and the room a receive makes goes to a sender that is
    /// still waiting.
    #[test]
    fn a_sender_that_left_takes_no_later_room_with_it() {
        let pool = Scheduler::started(1, true);
        let channel = open(1) as usize;
        assert!(unsafe { khora_channel_send(channel as *mut u8, 1000) });
        let (sent, refused) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut ids = Vec::new();
        for n in 0..3u64 {
            let (sent, refused) = (sent.clone(), refused.clone());
            let task = Task::new(move || {
                if unsafe { khora_channel_send(channel as *mut u8, n) } {
                    sent.fetch_add(1, Ordering::SeqCst);
                } else {
                    refused.fetch_add(1, Ordering::SeqCst);
                }
            });
            ids.push(task.fiber().id());
            pool.spawn(task);
        }
        settled_on(&pool, "the senders never parked", || pool.audit().parked == 3);

        pool.cancel_fiber(ids[0]);
        settled_on(&pool, "the canceled sender never left", || refused.load(Ordering::SeqCst) == 1);
        assert_eq!(take(channel as *mut u8), Some(1000));
        settled_on(&pool, "the room stayed empty while two senders waited", || {
            sent.load(Ordering::SeqCst) == 1
        });

        unsafe { khora_channel_close(channel as *mut u8) };
        settled_on(&pool, "the close left a sender parked", || refused.load(Ordering::SeqCst) == 2);
        drop(pool);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A burst of values reaches as many parked receivers**, whatever order
    /// the wakes and the receivers run in, and with no more wakes than
    /// values.
    #[test]
    fn a_burst_of_sends_reaches_every_parked_receiver_once() {
        const READERS: usize = 32;
        for workers in [1usize, 4] {
            let pool = Scheduler::started(workers, true);
            let channel = open(READERS as i64) as usize;
            let (got, _, _) = parked_receivers(&pool, channel, READERS);
            let before = pool.counts().wakes;
            let sender = Task::new(move || {
                for n in 0..READERS as u64 {
                    assert!(unsafe { khora_channel_send(channel as *mut u8, n) });
                }
            });
            pool.spawn(sender);
            settled_on(&pool, "a value was stranded", || got.load(Ordering::SeqCst) == READERS);
            let wakes = pool.counts().wakes - before;
            assert!(wakes <= READERS as u64, "{workers} workers: {wakes} wakes for {READERS} values");
            drop(pool);
            unsafe { khora_channel_release(channel as *mut u8) };
        }
    }

    /// **The review's herd, measured.** Two hundred fibers receive in a loop
    /// from one channel while one fiber sends two hundred values, yielding
    /// after each the way a request handler returns to its loop. Wakes per
    /// value is the number: a send that woke every parked receiver made it
    /// about a hundred, with every woken receiver running at once on the
    /// sender's worker and parking again before the next send.
    #[test]
    fn the_send_herd_is_one_wake_per_value() {
        const RECEIVERS: usize = 200;
        const VALUES: usize = 200;
        let pool = Scheduler::started(4, true);
        let channel = open(1) as usize;
        let (got, _, _) = parked_receivers(&pool, channel, RECEIVERS);
        let before = pool.counts().wakes;
        pool.spawn(Task::new(move || {
            for n in 0..VALUES as u64 {
                assert!(unsafe { khora_channel_send(channel as *mut u8, n) });
                crate::coro::suspend();
            }
        }));
        settled_on(&pool, "a value was stranded", || got.load(Ordering::SeqCst) == VALUES);
        let wakes = pool.counts().wakes - before;
        // The senders' own parks on a full slot are wakes too, one per
        // receive at most; so twice the values bounds a one-wake-per-value
        // channel, where a herd is a hundred times.
        assert!(
            wakes <= 2 * VALUES as u64,
            "{wakes} wakes for {VALUES} values ({:.1} per value)",
            wakes as f64 / VALUES as f64
        );
        drop(pool);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// Occupies a one-worker pool's only worker with a fiber that spins
    /// without a safepoint, so the wakes made meanwhile queue up in the
    /// order they were made. Answers the switch that lets it go.
    fn hold_the_worker(pool: &Scheduler) -> Arc<std::sync::atomic::AtomicBool> {
        use std::sync::atomic::AtomicBool;
        let hold = Arc::new(AtomicBool::new(true));
        let started = Arc::new(AtomicBool::new(false));
        let (held, running) = (hold.clone(), started.clone());
        pool.spawn(Task::new(move || {
            running.store(true, Ordering::SeqCst);
            while held.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
        }));
        settled_on(pool, "the holder never ran", || started.load(Ordering::SeqCst));
        hold
    }

    /// **A canceled receiver that runs before the one a send woke leaves
    /// empty-handed, and leaves no entry behind.** R1 is canceled while it
    /// waits, and before it runs, a send picks R0. R1 runs first, and the
    /// value is R0's, so R1 gives up on the cancel rather than taking it. R1's
    /// entry has to be gone too: left in the list, it would take the next
    /// send's wake to a fiber that has finished, and that value would sit
    /// queued while R2 waits.
    #[test]
    fn a_canceled_receiver_takes_no_value_another_was_woken_for() {
        let pool = Scheduler::started(1, true);
        let channel = open(4) as usize;
        let (got, none, ids) = parked_receivers(&pool, channel, 3);

        let hold = hold_the_worker(&pool);
        pool.cancel_fiber(ids[1]);
        assert!(unsafe { khora_channel_send(channel as *mut u8, 1) });
        hold.store(false, Ordering::SeqCst);
        // Settled either way: R2 and one of R0 or R1 still parked, or R1 gone.
        settled_on(&pool, "the first value was never taken", || {
            got.load(Ordering::SeqCst) == 1 && pool.audit().parked + none.load(Ordering::SeqCst) == 2
        });
        assert_eq!(none.load(Ordering::SeqCst), 1, "the canceled receiver took the woken one's value");

        assert!(unsafe { khora_channel_send(channel as *mut u8, 2) });
        settled_on(&pool, "the second value stayed queued while a receiver waited", || {
            got.load(Ordering::SeqCst) == 2
        });
        drop(pool);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **The same for a sender that fills room another was woken for.** S1
    /// is canceled while it waits for room, a receive picks S0, and S1 runs
    /// first and sends, since a send looks for room before the flag. S0 finds
    /// the channel full again and waits. The next receive's room must reach
    /// S0, not S1's leftover entry.
    #[test]
    fn a_sender_that_fills_room_it_was_not_woken_for_leaves_no_entry() {
        let pool = Scheduler::started(1, true);
        let channel = open(1) as usize;
        assert!(unsafe { khora_channel_send(channel as *mut u8, 1000) });
        let sent = Arc::new(AtomicUsize::new(0));
        let mut ids = Vec::new();
        for n in 0..2u64 {
            let sent = sent.clone();
            let task = Task::new(move || {
                assert!(unsafe { khora_channel_send(channel as *mut u8, n) });
                sent.fetch_add(1, Ordering::SeqCst);
            });
            ids.push(task.fiber().id());
            pool.spawn(task);
            settled_on(&pool, "a sender never parked", || pool.audit().parked == ids.len());
        }

        let hold = hold_the_worker(&pool);
        pool.cancel_fiber(ids[1]);
        assert_eq!(take(channel as *mut u8), Some(1000));
        hold.store(false, Ordering::SeqCst);
        settled_on(&pool, "the room was never filled", || {
            sent.load(Ordering::SeqCst) == 1 && pool.audit().parked == 1
        });

        assert_eq!(take(channel as *mut u8), Some(1), "the canceled sender's value came first");
        settled_on(&pool, "the room stayed empty while a sender waited", || {
            sent.load(Ordering::SeqCst) == 2
        });
        assert_eq!(take(channel as *mut u8), Some(0));
        drop(pool);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A value sent to a parked receiver is that receiver's, and a receive
    /// that never waited cannot take it first.** R0 has waited longest. The
    /// send picks it and queues it behind B, a fiber that was already
    /// runnable and receives next. If B could take the value, R0 would find
    /// the queue empty and enroll again at the back, behind B's own entry and
    /// everyone else's: a pool's idle channel under load did that to the same
    /// waiter again and again, and the waits that lost several times in a row
    /// were a server's slowest 1%.
    #[test]
    fn a_value_sent_to_a_parked_receiver_is_not_taken_by_a_later_one() {
        let pool = Scheduler::started(1, true);
        let channel = open(4) as usize;
        let first = Arc::new(AtomicUsize::new(0));
        let taker = first.clone();
        pool.spawn(Task::new(move || {
            if let Some(value) = take(channel as *mut u8) {
                taker.store(value as usize, Ordering::SeqCst);
            }
        }));
        settled_on(&pool, "the first receiver never parked", || pool.audit().parked == 1);

        let hold = hold_the_worker(&pool);
        let later = Arc::new(AtomicUsize::new(0));
        let latecomer = later.clone();
        // Queued before the send's wake, so it runs first.
        pool.spawn(Task::new(move || {
            if let Some(value) = take(channel as *mut u8) {
                latecomer.store(value as usize, Ordering::SeqCst);
            }
        }));
        assert!(unsafe { khora_channel_send(channel as *mut u8, 1) });
        hold.store(false, Ordering::SeqCst);
        settled_on(&pool, "the value was never taken", || {
            first.load(Ordering::SeqCst) + later.load(Ordering::SeqCst) == 1 && pool.audit().parked == 1
        });
        assert_eq!(
            later.load(Ordering::SeqCst),
            0,
            "a receive that had not waited took the value sent to the one that had"
        );

        assert!(unsafe { khora_channel_send(channel as *mut u8, 2) });
        settled_on(&pool, "the second value never reached the receiver still waiting", || {
            later.load(Ordering::SeqCst) == 2
        });
        drop(pool);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A thread takes a value promised to a fiber, and the fiber waits
    /// again.** A promise holds a value for a fiber that is going to run;
    /// a thread cannot wait on that, because the fiber may never run (its pool
    /// stopped with it parked), and nothing ever promises a value to a thread.
    /// Here the promised fiber is held off its worker while the thread polls.
    #[test]
    fn a_thread_takes_a_value_promised_to_a_fiber_that_has_not_run() {
        let pool = Scheduler::started(1, true);
        let channel = open(4) as usize;
        let (got, _, _) = parked_receivers(&pool, channel, 1);

        let hold = hold_the_worker(&pool);
        assert!(unsafe { khora_channel_send(channel as *mut u8, 1) });
        let mut out = 0u64;
        let taken = unsafe { khora_channel_poll(channel as *mut u8, &mut out) };
        // Before the assertion, or a failure leaves the worker spinning and
        // dropping the pool waits for it for ever.
        hold.store(false, Ordering::SeqCst);
        assert!(taken, "a thread's poll was refused a value promised to a fiber that had not run");
        assert_eq!(out, 1);
        settled_on(&pool, "the fiber whose value went never waited again", || pool.audit().parked == 1);

        assert!(unsafe { khora_channel_send(channel as *mut u8, 2) });
        settled_on(&pool, "the next value never reached the fiber", || got.load(Ordering::SeqCst) == 1);
        drop(pool);
        unsafe { khora_channel_release(channel as *mut u8) };
    }

    /// **A poll that takes a value makes room for a parked sender**, exactly
    /// as a receive does.
    #[test]
    fn a_poll_wakes_one_parked_sender() {
        let pool = Scheduler::started(1, true);
        let channel = open(1) as usize;
        assert!(unsafe { khora_channel_send(channel as *mut u8, 1000) });
        let sent = Arc::new(AtomicUsize::new(0));
        let counter = sent.clone();
        pool.spawn(Task::new(move || {
            assert!(unsafe { khora_channel_send(channel as *mut u8, 1) });
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        settled_on(&pool, "the sender never parked", || pool.audit().parked == 1);

        let mut out = 0u64;
        assert!(unsafe { khora_channel_poll(channel as *mut u8, &mut out) });
        assert_eq!(out, 1000);
        settled_on(&pool, "the room a poll made never reached the parked sender", || {
            sent.load(Ordering::SeqCst) == 1
        });
        assert_eq!(take(channel as *mut u8), Some(1));
        drop(pool);
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
