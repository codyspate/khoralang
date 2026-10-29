//! `Select::wait`: a receive from whichever of several channels has a value
//! first.
//!
//! # The shape: find, then take
//!
//! [`khora_select_wait`] waits until one of a [`Selection`]'s channels has a
//! value for this fiber and answers which, **without taking it**. The branch
//! for that channel then takes it with [`khora_channel_take_selected`], which
//! never waits. If somebody else took it in between, the branch finds
//! nothing and `Select::wait` waits again.
//!
//! **Why not take the value here.** The runtime cannot build a branch's
//! answer -- it does not know the value's type -- so a value taken here has
//! to be handed to Khora code that does, and that code runs a loop over the
//! branches to find the right one. A loop is a cancellation point, and a
//! cancel there would unwind with a value off its channel and in nobody's
//! hands. Found but not taken, a value a cancel interrupts is still in its
//! channel.
//!
//! **One promise is kept across the answer, and nothing can stop the fiber
//! while it is.** On the scheduler a selector enrolls on every channel's
//! waiting list, as a `receive` does, so a send hands it a value with the
//! same promise a receive gets (`Queue::handed`), and FIFO among waiters
//! holds for selectors too. Before answering, every promise but the one on
//! the channel it answers is handed to the next receiver in that channel's
//! line ([`give_back`]), so a second value is never stuck behind it. The one
//! it keeps is the branch's to take, and from the answer to the take the
//! fiber's entry polls are held off
//! ([`crate::current::Fiber::hold_entry_polls`]): the branch is a function
//! value, calling one polls on entry, and a cancel there would leave the
//! promise held by a fiber that has gone, keeping the value from everybody
//! for the life of the channel. `Select::wait` reaches its branch with no
//! loop and no `!` in between, so no other cancellation point can land there
//! either.
//!
//! **A thread cannot wait on two condition variables**, so a thread that
//! selects -- every fiber on the thread backend, and `main` -- waits on one
//! of its own ([`Watcher`]), and every channel it selects over tells it when
//! a value arrives or the channel closes.
//!
//! **Fair.** Each wait looks at the channels from a start drawn afresh, so a
//! channel that is always ready does not starve one listed after it.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// What a thread selecting over several channels waits on.
///
/// The count is how many times one of its channels has changed since it
/// was made, so a change that lands between the thread's last look and its
/// wait is seen rather than slept through.
pub(crate) struct Watcher {
    moved: Mutex<u64>,
    changed: Arc<Condvar>,
}

/// Tells each thread selecting on a channel that it changed.
///
/// Called with the channel's lock let go, for the reason
/// [`Channel::release`] gives for releasing outside it.
pub(crate) fn tell(watching: Vec<Arc<Watcher>>) {
    for watcher in watching {
        *watcher.moved.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        watcher.changed.notify_all();
    }
}

/// What [`khora_select_wait`] answers when every channel is closed and
/// drained.
pub const SELECT_ALL_CLOSED: i64 = -1;
/// What it answers when the deadline passed first.
pub const SELECT_TIMED_OUT: i64 = -2;
/// What it answers when this fiber was asked to stop while it waited. The
/// cancellation check after the call is where it stops.
pub const SELECT_GAVE_UP: i64 = -3;

/// Where each wait starts looking: a counter run through a mixer, so one
/// fiber's waits alternate and many fibers' waits spread out.
static TURN: AtomicU64 = AtomicU64::new(0);

/// A start in `0..count`, drawn afresh each call. SplitMix64's finalizer.
fn start(count: usize) -> usize {
    let mut x = TURN.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % count as u64) as usize
}

/// The channels one `Select::wait` waits on, in the order its branches were
/// added: what a [`khora_selection_open`] handle points at.
///
/// **Built for one wait and let go after it**, so two `Select` values that
/// share a prefix of branches never share a list whose indices would then
/// mean two different things.
pub(crate) struct Selection {
    channels: Vec<*mut u8>,
    /// When a wait on it gives up; `None` for never.
    ///
    /// **Fixed when the selection is opened, not per wait**, so a wait that
    /// comes round again after a branch found its value gone does not grant
    /// itself the whole budget a second time.
    deadline: Option<std::time::Instant>,
}

/// The tag every selection carries.
const SELECTION_TAG: u32 = 0;

/// The list behind a selection handle.
///
/// # Safety
///
/// `handle` must be a live object from [`khora_selection_open`].
unsafe fn selection_of<'a>(handle: *mut u8) -> &'a mut Selection {
    // SAFETY: the caller guarantees a live handle, whose field holds what
    // `khora_selection_open` wrote. A selection is not `Share`, so only the
    // fiber that opened it is in here.
    unsafe { &mut **handle.add(KHORA_FIELD_OFFSET).cast::<*mut Selection>() }
}

/// Opens an empty selection whose waits give up `millis` from now, or never
/// for a negative `millis`.
#[unsafe(no_mangle)]
pub extern "C" fn khora_selection_open(millis: i64) -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Selection>() as u64, SELECTION_TAG);
    let deadline = u64::try_from(millis).ok().and_then(|millis| {
        std::time::Instant::now().checked_add(std::time::Duration::from_millis(millis))
    });
    let list = Box::new(Selection {
        channels: Vec::new(),
        deadline,
    });
    // SAFETY: one field's worth of fresh, zeroed, aligned space, and nothing
    // else holds the pointer yet.
    unsafe {
        object
            .add(KHORA_FIELD_OFFSET)
            .cast::<*mut Selection>()
            .write(Box::into_raw(list))
    };
    object
}

/// Adds `channel` to a selection, which keeps a reference to it until the
/// selection is released.
///
/// # Safety
///
/// `selection` must be live from [`khora_selection_open`], and `channel` a
/// live channel handle.
#[unsafe(no_mangle)]
// SHARE: a channel handle is born shared; the selection stays on this fiber.
pub unsafe extern "C" fn khora_selection_add(selection: *mut u8, channel: *mut u8) {
    // SAFETY: the caller guarantees both are live.
    unsafe {
        crate::heap::khora_dup(channel);
        selection_of(selection).channels.push(channel);
    }
}

/// [`khora_channel_release`] as a `drop_fields` callback.
extern "C" fn channel_release_shim(channel: *mut u8) {
    // SAFETY: only reached through `khora_drop`, with the object whose last
    // reference it just released.
    unsafe { khora_channel_release(channel) };
}

/// Lets go of a selection and the references it kept. A `drop_fields`
/// callback.
///
/// # Safety
///
/// `selection` must be a live object from [`khora_selection_open`] whose
/// count has reached zero.
#[unsafe(no_mangle)]
// SHARE: releases; publishes nothing.
pub unsafe extern "C" fn khora_selection_release(selection: *mut u8) {
    if selection.is_null() {
        return;
    }
    // SAFETY: the caller guarantees the last use of a live selection, so the
    // box is taken back once and the slot cleared for a second release.
    let list = unsafe {
        let slot = selection.add(KHORA_FIELD_OFFSET).cast::<*mut Selection>();
        let raw = slot.read();
        if raw.is_null() {
            return;
        }
        slot.write(std::ptr::null_mut());
        Box::from_raw(raw)
    };
    for channel in list.channels {
        // SAFETY: each was a live channel the selection took a reference to.
        unsafe { crate::heap::khora_drop(channel, Some(channel_release_shim)) };
    }
}

/// Waits until one of a selection's channels has a value this fiber may
/// take, and answers its index; [`SELECT_ALL_CLOSED`] when every channel is
/// closed and drained, [`SELECT_TIMED_OUT`] when the selection's deadline
/// passed first, and [`SELECT_GAVE_UP`] when this fiber gave up on a stop.
///
/// **Takes nothing.** The value is still in its channel, for
/// [`khora_channel_take_selected`]; see the module for why. An index answered
/// holds entry polls off until [`khora_select_settled`] or the fiber's next
/// cancellation point of its own.
///
/// **A cancellation point**, and like a receive it finds a value before it
/// looks at the flag, so a value already there is answered whatever the flag
/// says.
///
/// # Safety
///
/// `selection` must be a live object from [`khora_selection_open`].
#[unsafe(no_mangle)]
// SHARE: reads the selection and its channels; stores nothing.
pub unsafe extern "C" fn khora_select_wait(selection: *mut u8) -> i64 {
    // SAFETY: the caller guarantees a live selection; each handle in it is a
    // live channel it holds a reference to.
    let selection = unsafe { selection_of(selection) };
    let channels: Vec<&Channel> = selection
        .channels
        .iter()
        .map(|handle| unsafe { channel_of(*handle) })
        .map(|c| {
            c.unwrap_or_else(|| fatal("selecting on a channel that has already been released"))
        })
        .collect();
    if channels.is_empty() {
        return SELECT_ALL_CLOSED;
    }
    let at = wait(&channels, selection.deadline);
    if at >= 0 {
        crate::current::current(|fiber| fiber.hold_entry_polls());
    }
    at
}

/// Ends the hold [`khora_select_wait`] put on entry polls, once the branch
/// has answered. Without it, a branch with no cancellation point of its own
/// left the hold standing until the fiber's next one, and a call cycle with
/// none could not be stopped in the meantime.
#[unsafe(no_mangle)]
pub extern "C" fn khora_select_settled() {
    crate::current::current(|fiber| fiber.release_entry_polls());
}

/// Whether `state` has a value the caller may take: one nobody is owed, or
/// one owed to `me`.
///
/// **A thread is owed nothing**, the rule a thread's receive follows, so for
/// it a value promised to a parked fiber is not there: answering it would
/// send the branch to a take that finds nothing, and the wait round again at
/// once, a spin for as long as the fiber takes to run.
fn ready(state: &Queue, me: usize, thread: bool) -> bool {
    state.items.len() > state.handed.len() || !thread && state.handed.contains(&me)
}

/// The wait itself, over channels already looked up.
fn wait(channels: &[&Channel], deadline: Option<std::time::Instant>) -> i64 {
    let me = crate::current::current(|fiber| fiber.id());
    let thread = !crate::coro::on_a_fiber();
    let mut enrolled = false;
    loop {
        let first = start(channels.len());
        let mut closed = 0;
        let mut found = None;
        for step in 0..channels.len() {
            let at = (first + step) % channels.len();
            let state = channels[at].state.lock().unwrap_or_else(|e| e.into_inner());
            if ready(&state, me, thread) {
                found = Some(at);
                break;
            }
            if state.closed {
                closed += 1;
            }
        }
        // Off every list, with every promise but the found channel's handed
        // on, before answering anything: see the module.
        if std::mem::take(&mut enrolled) {
            for (at, channel) in channels.iter().enumerate() {
                give_back(channel, me, found == Some(at));
            }
        }
        if let Some(at) = found {
            return at as i64;
        }
        if closed == channels.len() {
            return SELECT_ALL_CLOSED;
        }
        if deadline.is_some_and(|at| std::time::Instant::now() >= at) {
            return SELECT_TIMED_OUT;
        }
        if stopping() {
            return SELECT_GAVE_UP;
        }
        match waker_for_current() {
            Some(waker) => {
                #[cfg(test)]
                enroll_probe::pause_here(me);
                // Enrolled under the lock that saw each one empty, or a send
                // between the look and the enrollment leaves this fiber
                // parked on a value that is already there. One that has a
                // value by now ends the enrolling, and the look above finds it.
                //
                // **So does one that has closed**, as in [`park_thread`]. A
                // channel open at the look and closed by now has woken every
                // waiter it had, and this fiber was not one; if it was the
                // last open channel, nothing would ever wake the park below.
                // Looking again costs a pass and finds every channel closed.
                let mut arrived = false;
                for channel in channels {
                    let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
                    if ready(&state, me, false) || state.closed {
                        arrived = true;
                        break;
                    }
                    state.receivers.push_back(waker.clone());
                }
                enrolled = true;
                if !arrived {
                    crate::scheduler::park_current_until(Why::ChannelValue, 0, deadline);
                }
            }
            None => park_thread(channels, me, deadline),
        }
    }
}

/// Test-only: holds a chosen fiber between its look at the channels and its
/// enrollment on them, so a test can land a close or a send in that window
/// on every run rather than once in fifty. Nothing here is compiled outside
/// `cfg(test)`.
#[cfg(test)]
pub(crate) mod enroll_probe {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// The fiber to hold, or 0 for none.
    pub(crate) static TARGET: AtomicUsize = AtomicUsize::new(0);
    /// Set once the target is held.
    pub(crate) static HELD: AtomicBool = AtomicBool::new(false);
    /// Cleared by the test to let it go on.
    pub(crate) static HOLD: AtomicBool = AtomicBool::new(false);

    pub(super) fn pause_here(me: usize) {
        if TARGET.load(Ordering::SeqCst) != me {
            return;
        }
        HELD.store(true, Ordering::SeqCst);
        while HOLD.load(Ordering::SeqCst) {
            std::hint::spin_loop();
        }
    }
}

/// Takes `me` off `channel`'s waiting list, and unless `keep`, hands any
/// value a send promised it to the next receiver in line, waking that one.
///
/// **What keeps a second value from being lost.** Two sends to two of a
/// selector's channels can each pick it before it runs, and it takes at most
/// one. Without this, every receiver on the other channel would wait behind
/// a promise nobody keeps.
///
/// **And the threads waiting on the channel are told.** A thread is never
/// promised a value, so a thread that was woken by the send found the value
/// owed to this fiber and waited again; a value that is nobody's once more
/// is news to it. Without the tell it waited out [`LOOK_AGAIN`].
fn give_back(channel: &Channel, me: usize, keep: bool) {
    let (next, threads, watching) = {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        state.receivers.retain(|w| w.fiber() != me);
        if keep {
            return;
        }
        let Some(at) = state.handed.iter().position(|&f| f == me) else {
            return;
        };
        state.handed.remove(at);
        (
            hand_to_next(&mut state),
            state.threads_receiving,
            state.watching(),
        )
    };
    // Outside the lock, for the reason [`Channel::release`] gives.
    channel.a_value_arrived(threads);
    tell(watching);
    if let Some(waker) = next {
        waker.wake();
    }
}

/// Waits for any of `channels` to change, on a thread with no worker to
/// give back: until a send or a close to one of them, a stop, or `deadline`.
///
/// Registered with the fiber as what it is parked on, so a cancel wakes it,
/// and bounded by [`LOOK_AGAIN`] for the cancel that lands between the look
/// and the wait, as [`park_until_moved`] is.
fn park_thread(channels: &[&Channel], me: usize, deadline: Option<std::time::Instant>) {
    let watcher = Arc::new(Watcher {
        moved: Mutex::new(0),
        changed: Arc::new(Condvar::new()),
    });
    let mut arrived = false;
    for channel in channels {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        if ready(&state, me, true) || state.closed {
            arrived = true;
            break;
        }
        state.watchers.push(watcher.clone());
    }
    if !arrived {
        crate::current::current(|fiber| fiber.park_on(&watcher.changed));
        let moved = watcher.moved.lock().unwrap_or_else(|e| e.into_inner());
        if *moved == 0 && !stopping() {
            let mut slice = LOOK_AGAIN;
            if let Some(at) = deadline {
                slice = slice.min(at.saturating_duration_since(std::time::Instant::now()));
            }
            let _ = watcher
                .changed
                .wait_timeout(moved, slice)
                .unwrap_or_else(|e| e.into_inner());
        } else {
            drop(moved);
        }
        crate::current::current(|fiber| fiber.unpark_from());
    }
    for channel in channels {
        let mut state = channel.state.lock().unwrap_or_else(|e| e.into_inner());
        state.watchers.retain(|w| !Arc::ptr_eq(w, &watcher));
    }
}

/// Takes a value from a channel a select found ready, if one is still there,
/// and never waits: the value promised to this fiber if there is one, as a
/// `receive` would, and otherwise one nobody is owed.
///
/// **The hold on entry polls narrows to one more poll**
/// ([`crate::current::Fiber::hold_one_more_entry_poll`]). The function the
/// value is handed to is a function value, and calling one polls on entry: a
/// cancel landing there unwound with the value as its argument, off its
/// channel and seen by nobody. A `receive` written in place hands its value
/// on with no poll at all; this is the same promise for a value that travels
/// through a call. Past that entry the branch's polls count, so a cancel
/// reaches a branch made only of calls.
///
/// # Safety
///
/// `handle` must be live and `out` a writable word.
#[unsafe(no_mangle)]
// SHARE: hands out a value the entry that stored it already marked; stores nothing.
pub unsafe extern "C" fn khora_channel_take_selected(handle: *mut u8, out: *mut u64) -> bool {
    // SAFETY: the caller's guarantees are `khora_channel_poll`'s.
    let taken = unsafe { khora_channel_poll(handle, out) };
    if taken {
        crate::current::current(|fiber| fiber.hold_one_more_entry_poll());
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coro::Task;
    use crate::scheduler::Scheduler;
    use std::sync::atomic::AtomicUsize;

    fn open(capacity: i64) -> usize {
        // SAFETY: unboxed, so the word is a number and nothing is released.
        unsafe { khora_channel_open(capacity, 0, false, None) as usize }
    }

    fn send(handle: usize, value: u64) -> bool {
        // SAFETY: a live handle from `open`, released at the end of its test.
        unsafe { khora_channel_send(handle as *mut u8, value) }
    }

    fn close(handle: usize) {
        // SAFETY: as above.
        unsafe { khora_channel_close(handle as *mut u8) };
    }

    fn take(handle: usize) -> Option<u64> {
        let mut out = 0u64;
        // SAFETY: as above, and a writable word.
        let taken = unsafe { khora_channel_take_selected(handle as *mut u8, &mut out) };
        khora_select_settled();
        taken.then_some(out)
    }

    /// One wait, and the value from the channel it found, as `Select::wait`
    /// does it: wait, take, and wait again if the take came back empty.
    fn select(handles: &[usize], millis: i64) -> Result<(usize, u64), i64> {
        let selection = khora_selection_open(millis);
        for handle in handles {
            // SAFETY: a live selection and a live channel.
            unsafe { khora_selection_add(selection, *handle as *mut u8) };
        }
        let answer = loop {
            // SAFETY: a live selection.
            let at = unsafe { khora_select_wait(selection) };
            if at < 0 {
                break Err(at);
            }
            if let Some(value) = take(handles[at as usize]) {
                break Ok((at as usize, value));
            }
        };
        // SAFETY: the last reference to the selection.
        unsafe { crate::heap::khora_drop(selection, Some(khora_selection_release_shim)) };
        answer
    }

    extern "C" fn khora_selection_release_shim(selection: *mut u8) {
        // SAFETY: reached only through `khora_drop`.
        unsafe { khora_selection_release(selection) };
    }

    fn release(handle: usize) {
        // SAFETY: the last use of a handle from `open`.
        unsafe { khora_channel_release(handle as *mut u8) };
    }

    fn settled(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// The channel that has a value is the one picked.
    #[test]
    fn whichever_is_ready_is_picked() {
        let (a, b) = (open(4), open(4));
        assert!(send(b, 7));
        assert_eq!(select(&[a, b], -1), Ok((1, 7)));
        assert!(send(a, 8));
        assert_eq!(select(&[a, b], -1), Ok((0, 8)));
        release(a);
        release(b);
    }

    /// **Neither channel is starved when both are always ready.** Red with
    /// `start` answering 0.
    #[test]
    fn two_ready_channels_are_picked_fairly() {
        let (a, b) = (open(2), open(2));
        let mut picked = [0usize; 2];
        for n in 0..1000 {
            assert!(send(a, n));
            assert!(send(b, n));
            let (at, _) = select(&[a, b], -1).expect("a value");
            picked[at] += 1;
            let other = if at == 0 { b } else { a };
            assert!(
                take(other).is_some(),
                "the value not picked stayed in its channel"
            );
        }
        assert!(
            picked.iter().all(|&n| n >= 350),
            "picks were {picked:?} in 1000 rounds"
        );
        release(a);
        release(b);
    }

    /// One closed channel is waited past; all closed is `-1`; a deadline is
    /// `-2`.
    #[test]
    fn closed_channels_and_a_deadline() {
        let (a, b) = (open(1), open(1));
        close(a);
        let began = std::time::Instant::now();
        assert_eq!(
            select(&[a, b], 30),
            Err(SELECT_TIMED_OUT),
            "one closed is not all closed"
        );
        assert!(began.elapsed() >= std::time::Duration::from_millis(30));
        assert!(send(b, 3));
        assert_eq!(select(&[a, b], -1), Ok((1, 3)));
        close(b);
        assert_eq!(select(&[a, b], -1), Err(SELECT_ALL_CLOSED));
        release(a);
        release(b);
    }

    /// **Two values for one parked selector: one is taken, and the other goes
    /// to the receiver waiting behind it.** The worker is held while both
    /// sends pick the selector, so both promise it a value. Red without
    /// [`give_back`]'s hand-on: the receiver waits behind a promise nobody
    /// keeps.
    #[test]
    fn a_second_promise_goes_to_the_next_receiver() {
        let pool = Scheduler::started(1, true);
        let (a, b) = (open(4), open(4));
        let taken = Arc::new(AtomicUsize::new(0));
        let behind = Arc::new(AtomicUsize::new(0));
        let (t, bh) = (taken.clone(), behind.clone());
        pool.spawn(Task::new(move || {
            if select(&[a, b], -1).is_ok() {
                t.fetch_add(1, Ordering::SeqCst);
            }
        }));
        settled("the selector never parked", || pool.audit().parked == 1);
        // One receiver behind it on each channel: whichever the selector
        // takes, the other value has somebody in line for it.
        for channel in [a, b] {
            let bh = bh.clone();
            pool.spawn(Task::new(move || {
                let mut out = 0;
                // SAFETY: a live handle and a writable word.
                if unsafe { khora_channel_receive(channel as *mut u8, &mut out) } {
                    bh.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        settled("the receivers never parked", || pool.audit().parked == 3);
        let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (held, running) = (
            hold.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let started = running.clone();
        pool.spawn(Task::new(move || {
            started.store(true, Ordering::SeqCst);
            while held.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
        }));
        settled("the holder never ran", || running.load(Ordering::SeqCst));
        assert!(send(a, 1));
        assert!(send(b, 2));
        hold.store(false, Ordering::SeqCst);
        settled(
            "a value was stranded behind the selector's second promise",
            || taken.load(Ordering::SeqCst) == 1 && behind.load(Ordering::SeqCst) == 1,
        );
        // The receiver that got nothing is still parked; a close lets it go.
        close(a);
        close(b);
        settled("the last receiver never left", || pool.audit().parked == 0);
        drop(pool);
        release(a);
        release(b);
    }

    /// A selector canceled while it waits leaves empty-handed and on no
    /// list: the next value goes to the receiver behind it.
    #[test]
    fn a_canceled_selector_leaves_no_entry_behind() {
        let pool = Scheduler::started(1, true);
        let (a, b) = (open(4), open(4));
        let gave_up = Arc::new(AtomicUsize::new(0));
        let g = gave_up.clone();
        let task = Task::new(move || {
            if select(&[a, b], -1) == Err(SELECT_GAVE_UP) {
                g.fetch_add(1, Ordering::SeqCst);
            }
        });
        let id = task.fiber().id();
        pool.spawn(task);
        settled("the selector never parked", || pool.audit().parked == 1);
        let got = Arc::new(AtomicUsize::new(0));
        let gg = got.clone();
        pool.spawn(Task::new(move || {
            let mut out = 0;
            // SAFETY: a live handle and a writable word.
            if unsafe { khora_channel_receive(a as *mut u8, &mut out) } {
                gg.store(out as usize, Ordering::SeqCst);
            }
        }));
        settled("the receiver never parked", || pool.audit().parked == 2);
        pool.cancel_fiber(id);
        settled("the canceled selector never left", || {
            gave_up.load(Ordering::SeqCst) == 1
        });
        assert!(send(a, 5));
        settled("the value went to the selector that left", || {
            got.load(Ordering::SeqCst) == 5
        });
        drop(pool);
        release(a);
        release(b);
    }

    /// How many threads are watching `handle`: a thread selecting on it has
    /// enrolled and is about to wait, or is waiting.
    fn watchers(handle: usize) -> usize {
        // SAFETY: a live handle from `open`.
        let channel = unsafe { channel_of(handle as *mut u8) }.expect("a live channel");
        channel
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .watchers
            .len()
    }

    /// Rounds of `waits` that took the backstop's time or longer: a wake
    /// that was never sent, found by the [`LOOK_AGAIN`] re-look instead.
    fn backstopped(waits: &[std::time::Duration]) -> usize {
        waits.iter().filter(|w| **w >= LOOK_AGAIN * 4 / 5).count()
    }

    /// **A thread selecting is woken by the send itself, not by the
    /// backstop.** A thread waits on a condition variable of its own, and a
    /// channel it selects over has to signal it; a send that did not would
    /// still be found, by the [`LOOK_AGAIN`] re-look a quarter of a second
    /// later, on every round. Each round waits until the thread is enrolled
    /// on the channel before sending, and no round may take the backstop's
    /// time. Red with [`tell`] signaling nobody.
    #[test]
    fn a_thread_is_woken_by_the_send_rather_than_the_backstop() {
        let (a, b) = (open(1), open(1));
        let mut waits = Vec::new();
        for round in 0..20 {
            let waiter = std::thread::spawn(move || {
                let got = select(&[a, b], -1);
                (got, std::time::Instant::now())
            });
            settled("the waiter never enrolled", || watchers(b) == 1);
            let sent = std::time::Instant::now();
            assert!(send(b, round));
            let (got, woke) = waiter.join().expect("the waiter");
            assert_eq!(got, Ok((1, round)));
            waits.push(woke.saturating_duration_since(sent));
        }
        assert_eq!(backstopped(&waits), 0, "waits after the send: {waits:?}");
        release(a);
        release(b);
    }

    /// **A promise a fiber selector hands back wakes a thread selecting on
    /// the same channels.** Two sends each promise their value to a parked
    /// fiber selector while its worker is held; the fiber takes one and
    /// hands the other promise back. A thread selecting on the same channels
    /// is never promised a value, so it was told of each send, found both
    /// values owed to the fiber, and waited again: without a tell when the
    /// promise is released it waited out the backstop, every round. Red with
    /// `give_back` not telling watchers.
    #[test]
    fn a_promise_handed_back_wakes_a_thread_selector() {
        let pool = Scheduler::started(1, true);
        let mut waits = Vec::new();
        let finished = Arc::new(AtomicUsize::new(0));
        for round in 0..5 {
            let (a, b) = (open(4), open(4));
            let done = finished.clone();
            pool.spawn(Task::new(move || {
                let _ = select(&[a, b], -1);
                done.fetch_add(1, Ordering::SeqCst);
            }));
            settled("the fiber selector never parked", || {
                pool.audit().parked == 1
            });
            let thread = std::thread::spawn(move || {
                let got = select(&[a, b], -1);
                (got, std::time::Instant::now())
            });
            settled("the thread selector never enrolled", || {
                watchers(a) == 1 && watchers(b) == 1
            });
            let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let running = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (held, started) = (hold.clone(), running.clone());
            pool.spawn(Task::new(move || {
                started.store(true, Ordering::SeqCst);
                while held.load(Ordering::SeqCst) {
                    std::hint::spin_loop();
                }
            }));
            settled("the holder never ran", || running.load(Ordering::SeqCst));
            assert!(send(a, 1));
            assert!(send(b, 2));
            let freed = std::time::Instant::now();
            hold.store(false, Ordering::SeqCst);
            let (got, woke) = thread.join().expect("the thread selector");
            assert!(got.is_ok(), "the thread selector got {got:?}");
            waits.push(woke.saturating_duration_since(freed));
            // Finished, not merely unparked: a selector that was woken is
            // running, and may still be taking its value off a channel, so
            // releasing the channels on `parked == 0` freed them under it.
            settled("the fiber selector never finished", || {
                finished.load(Ordering::SeqCst) == round + 1
            });
            release(a);
            release(b);
        }
        drop(pool);
        assert_eq!(
            backstopped(&waits),
            0,
            "waits after the fiber ran: {waits:?}"
        );
    }

    /// **A fiber selector sees a close that lands between its look and its
    /// enrollment.** The selector is held at that point
    /// ([`enroll_probe`]) while both its channels are closed, then let go; it
    /// must answer all-closed. A channel found closed while enrolling was
    /// neither enrolled nor counted as arrived, so with both closed there the
    /// fiber parked with nobody left to wake it. A stuck selector is
    /// canceled so the test fails rather than hangs. Red with the enrollment
    /// loop ignoring a close.
    #[test]
    fn a_close_during_the_enrollment_ends_the_wait() {
        let pool = Scheduler::started(1, true);
        let (a, b) = (open(1), open(1));
        let answer = Arc::new(std::sync::atomic::AtomicI64::new(i64::MIN));
        let seen = answer.clone();
        let task = Task::new(move || {
            let got = match select(&[a, b], -1) {
                Ok(_) => 0,
                Err(status) => status,
            };
            seen.store(got, Ordering::SeqCst);
        });
        let id = task.fiber().id();
        enroll_probe::HOLD.store(true, Ordering::SeqCst);
        enroll_probe::TARGET.store(id, Ordering::SeqCst);
        pool.spawn(task);
        settled("the selector never reached its enrollment", || {
            enroll_probe::HELD.load(Ordering::SeqCst)
        });
        close(a);
        close(b);
        enroll_probe::HOLD.store(false, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while answer.load(Ordering::SeqCst) == i64::MIN {
            if std::time::Instant::now() >= deadline {
                pool.cancel_fiber(id);
                settled("the stuck selector never left", || {
                    answer.load(Ordering::SeqCst) != i64::MIN
                });
                panic!("the selector was still waiting 5 s after both channels closed");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(answer.load(Ordering::SeqCst), SELECT_ALL_CLOSED);
        drop(pool);
        release(a);
        release(b);
    }

    /// A thread selecting is woken by a send to any of its channels, and by
    /// the last close.
    #[test]
    fn a_thread_is_woken_by_any_of_its_channels() {
        let (a, b) = (open(1), open(1));
        let waiter = std::thread::spawn(move || select(&[a, b], -1));
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(send(b, 9));
        assert_eq!(waiter.join().expect("the waiter"), Ok((1, 9)));
        let waiter = std::thread::spawn(move || select(&[a, b], -1));
        std::thread::sleep(std::time::Duration::from_millis(20));
        close(a);
        close(b);
        assert_eq!(waiter.join().expect("the waiter"), Err(SELECT_ALL_CLOSED));
        release(a);
        release(b);
    }

    /// **An answer holds entry polls off until the value is taken, and then
    /// for one more**, so a cancel that arrives between the answer and the
    /// branch's entry leaves the value in its channel or in the branch's
    /// hands rather than unwinding with it. Red (`left: 1`) with the hold
    /// taken out of `khora_select_wait`.
    #[test]
    fn an_answer_holds_entry_polls_until_a_point() {
        std::thread::spawn(|| {
            let fiber = crate::current::Fiber::spawned();
            let _entered = crate::current::enter(fiber.clone());
            let a = open(1);
            assert!(send(a, 4));
            let selection = khora_selection_open(-1);
            // SAFETY: a live selection and channel.
            unsafe { khora_selection_add(selection, a as *mut u8) };
            // SAFETY: a live selection.
            assert_eq!(unsafe { khora_select_wait(selection) }, 0);
            fiber.cancel();
            assert_eq!(
                crate::cancel::khora_entry_canceled(),
                0,
                "the `take` closure's entry poll"
            );
            assert_eq!(
                crate::cancel::khora_entry_canceled(),
                0,
                "and any before the take"
            );
            let mut out = 0u64;
            // SAFETY: a live handle and a writable word.
            assert!(unsafe { khora_channel_take_selected(a as *mut u8, &mut out) });
            assert_eq!(out, 4);
            assert_eq!(
                crate::cancel::khora_entry_canceled(),
                0,
                "the branch's own entry poll"
            );
            assert_eq!(
                crate::cancel::khora_entry_canceled(),
                1,
                "a poll inside the branch stops it"
            );
            // SAFETY: the last reference to the selection.
            unsafe { crate::heap::khora_drop(selection, Some(khora_selection_release_shim)) };
            release(a);
        })
        .join()
        .expect("the test thread");
    }
}
