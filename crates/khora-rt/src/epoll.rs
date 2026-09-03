//! The scalable readiness backend on Linux.
//!
//! [`crate::reactor`] is written against `poll`, which is the same call on
//! every platform and testable on the one this was written on. It is also
//! `O(n)` in registered descriptors *per call*: the kernel is handed the whole
//! set every time and walks all of it, so a server holding a hundred thousand
//! idle connections spends its time re-describing them rather than serving.
//!
//! `epoll` moves that cost to registration. The set lives in the kernel, a
//! descriptor is described once when a fiber starts waiting on it, and
//! `epoll_wait` returns only what is ready — `O(ready)` rather than
//! `O(watching)`.
//!
//! # What it does not change
//!
//! The interface above it, which is the whole point of
//! `docs/design/scheduler.md` §2: an operation tries the syscall, and calls
//! [`crate::reactor::Reactor::poll`] only if it would have blocked. Nothing
//! above the reactor learns which mechanism answered, and the `poll` backend
//! stays exactly where it was for Windows and macOS.
//!
//! # Level-triggered, and disarmed on the way out
//!
//! Not `EPOLLET` and not `EPOLLONESHOT`, both of which are the usual answers
//! and both of which are wrong here.
//!
//! Edge-triggered requires draining the socket until it says `EWOULDBLOCK`,
//! because a level that stays high produces no second edge. The operations
//! above this do not drain: `khora_net_recv` performs *one* `recv` and hands
//! the bytes to the fiber. An edge-triggered reactor under a one-shot reader
//! loses a wakeup whenever more arrived than was asked for, and the fiber then
//! waits for data that is already in the buffer.
//!
//! One-shot disarms the descriptor after an event, which is nearly right — but
//! two fibers may wait on one socket, one for readable and one for writable,
//! and one-shot would disarm both when either fires.
//!
//! So: level-triggered, and the descriptor is re-described the moment its
//! watchers change. A fiber is woken, its watch is removed, and the mask is
//! recomputed from what is left — which is `EPOLL_CTL_DEL` when nothing is.
//! Between the wake and the fiber's retry the descriptor is not armed, so a
//! level that stays high cannot spin.
//!
//! # One entry per descriptor, not per fiber
//!
//! `epoll` is keyed by descriptor and the reactor's list is keyed by wait, so
//! the two are not one-to-one: two fibers waiting on one socket are two watches
//! and one registration whose mask is the union. [`Epoll::sync`] is the only
//! function that writes to the kernel, and it always writes the union of what
//! the reactor still holds — so an interest cannot be leaked by an unbalanced
//! add and remove, because there is no add and no remove, only a recomputation.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::reactor::{Interest, Socket, Watch};

/// A kernel-side set of descriptors and what is wanted from each.
pub(crate) struct Epoll {
    /// The `epoll` instance.
    fd: i32,
    /// What each descriptor is currently registered for, so that a
    /// recomputation that changes nothing costs no syscall.
    armed: Mutex<HashMap<Socket, u32>>,
}

impl Epoll {
    /// Opens one, or `None` if the kernel would not.
    ///
    /// `None` is not a failure to report: the reactor falls back to `poll`,
    /// which is slower and correct. A runtime that refused to start because
    /// `epoll_create1` failed would be worse than one that was `O(n)`.
    pub(crate) fn open() -> Option<Epoll> {
        // SAFETY: no arguments but a flag; the call returns a descriptor or -1.
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        Some(Epoll { fd, armed: Mutex::new(HashMap::new()) })
    }

    /// Registers `socket` for exactly `events`, or removes it when there are
    /// none.
    ///
    /// **The only writer, and it always writes the whole truth.** Callers hand
    /// it the union of what is still wanted rather than a delta, so there is no
    /// add to pair with a remove and no way to leak an interest by losing one
    /// half of a pair.
    fn set(&self, socket: Socket, events: u32) {
        let mut armed = self.armed.lock().expect("the epoll set");
        let current = armed.get(&socket).copied();
        if current == Some(events) {
            return;
        }
        let (op, kept) = match (current, events) {
            (None, 0) => return,
            (None, _) => (libc::EPOLL_CTL_ADD, Some(events)),
            (Some(_), 0) => (libc::EPOLL_CTL_DEL, None),
            (Some(_), _) => (libc::EPOLL_CTL_MOD, Some(events)),
        };
        // The descriptor is carried in `u64` so that a ready event names the
        // socket without a second lookup. `Socket` is `i32` here, so this is
        // widening rather than a cast that could lose anything.
        let mut event = libc::epoll_event { events, u64: socket as u64 };
        // SAFETY: `event` is live for the call, and `EPOLL_CTL_DEL` ignores it
        // on every kernel since 2.6.9 while older ones require it non-null --
        // which is why it is passed rather than `null_mut`.
        let done = unsafe { libc::epoll_ctl(self.fd, op, socket, &raw mut event) };
        if done < 0 {
            // **A descriptor the kernel will not take is forgotten rather than
            // retried.** The common cause is a socket closed by another fiber
            // between the watch being recorded and this call, and the right
            // answer is the same as for any other closed socket: the waiting
            // fiber's next retry fails and it stops waiting. Remembering a
            // registration the kernel does not have would be worse -- the next
            // `set` would send `MOD` for something absent and fail for ever.
            armed.remove(&socket);
            return;
        }
        match kept {
            Some(events) => armed.insert(socket, events),
            None => armed.remove(&socket),
        };
    }

    /// Re-describes `socket` from whatever the reactor still holds for it.
    pub(crate) fn sync(&self, socket: Socket, watching: &[Watch]) {
        self.set(socket, wanted(socket, watching));
    }

    /// Registers the reactor's own wakeup descriptor, permanently.
    ///
    /// It is never removed and never re-described, so it does not go through
    /// `sync` -- which computes a mask from the watch list, and the waker is
    /// not in it.
    pub(crate) fn watch_waker(&self, socket: Socket) {
        self.set(socket, libc::EPOLLIN as u32);
    }

    /// Waits up to `timeout` and answers the descriptors that are ready, each
    /// with the events the kernel reported.
    pub(crate) fn wait(&self, timeout: std::time::Duration) -> Vec<(Socket, u32)> {
        // **Rounded up, never down to zero.** A sub-millisecond slice truncated
        // to zero makes `epoll_wait` return immediately, and the worker then
        // spins through the scheduler at whatever rate the loop allows. A
        // millisecond of over-waiting is a latency; a zero is a busy core.
        let millis = match timeout.as_millis() {
            0 => 0,
            other => other.min(i32::MAX as u128) as i32,
        };
        let millis = if timeout.is_zero() { 0 } else { millis.max(1) };

        // 256 is the batch, not a limit on what may be registered: whatever
        // does not fit is reported by the next call, which happens immediately
        // because the descriptors are still ready.
        let mut events: [libc::epoll_event; 256] =
            // SAFETY: `epoll_event` is a plain C struct of two integers, for
            // which an all-zero pattern is valid, and every entry the call
            // reports is overwritten before it is read.
            unsafe { std::mem::zeroed() };
        // SAFETY: `events` is a live array of exactly `len` entries for the
        // duration of the call.
        let count = unsafe {
            libc::epoll_wait(self.fd, events.as_mut_ptr(), events.len() as i32, millis)
        };
        if count <= 0 {
            return Vec::new();
        }
        events[..count as usize]
            .iter()
            .map(|event| (event.u64 as Socket, event.events))
            .collect()
    }
}

impl Drop for Epoll {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from `epoll_create1` and nothing else
        // holds it.
        unsafe {
            libc::close(self.fd);
        }
    }
}

/// The union of what every watch on `socket` wants.
///
/// `EPOLLERR` and `EPOLLHUP` are reported whether or not they are asked for, so
/// they are not in the mask; a fiber woken by one retries, gets the error from
/// the syscall, and reports it — which is the fiber's business rather than the
/// reactor's, exactly as the `poll` backend has it.
fn wanted(socket: Socket, watching: &[Watch]) -> u32 {
    let mut events = 0u32;
    for watch in watching.iter().filter(|w| w.socket == socket) {
        events |= match watch.interest {
            Interest::Readable => libc::EPOLLIN as u32,
            Interest::Writable => libc::EPOLLOUT as u32,
        };
    }
    events
}

/// Whether a watch should be woken by these reported events.
///
/// A hangup or an error wakes whoever was waiting, whatever they were waiting
/// for: there is nothing left to become ready and a reactor that ignored one
/// would leak a fiber per disconnect.
pub(crate) fn wakes(watch: &Watch, events: u32) -> bool {
    let failed = (libc::EPOLLERR | libc::EPOLLHUP) as u32;
    if events & failed != 0 {
        return true;
    }
    match watch.interest {
        Interest::Readable => events & libc::EPOLLIN as u32 != 0,
        Interest::Writable => events & libc::EPOLLOUT as u32 != 0,
    }
}
