//! Waiting on a socket without blocking a worker.
//!
//! [`crate::wait`] is how a fiber waits for *something*, and timers are one
//! thing to wait for. This is the other: a descriptor becoming ready.
//!
//! # The shape, which is the part that matters
//!
//! ```text
//! Khora:      let request = connection.read()!
//!
//! runtime:    try the syscall
//!               ├─ it worked      → return the bytes
//!               └─ it would block → register, park the fiber, run another
//!                                   ... the reactor says ready ...
//!                                 → wake, resume, retry
//! ```
//!
//! The first line is the whole point and does not change: no `async`, no
//! `await`, no `Future`, no coloured functions. `std::net::socket` keeps its
//! blocking shape, and a program that already reads a socket benefits without
//! being edited.
//!
//! # `poll` first, and why that is not a reversal
//!
//! `docs/design/scheduler.md` §2 argues for an **operation-oriented** interface
//! — submit an operation, suspend until it completes — rather than a readiness
//! one, because IOCP is completion-based and making it fake readiness costs a
//! buffer and a copy. That argument is about the *interface*, and the interface
//! here is exactly that: [`wait_until_ready`] is called by an operation that
//! already tried and would block, and returns when it is worth trying again.
//! Nothing above the reactor learns which mechanism answered.
//!
//! Underneath it is `poll` — `WSAPoll` on Windows, the same call by the same
//! name on Linux and macOS, the same struct in a different width. One code
//! path, three platforms, testable on the one this is written on.
//!
//! # `epoll` underneath it on Linux
//!
//! `poll` is O(n) in registered descriptors *per call*: the kernel is handed
//! the whole set every time and walks all of it, so a server holding a hundred
//! thousand idle connections spends its time re-describing them. `epoll` moves
//! that cost to registration and returns only what is ready.
//!
//! It is a backend rather than a rewrite. Everything in this file — the watch
//! list, the deadline that rides on a watch, the loopback waker, the contract
//! that a fiber is woken once per registration — is unchanged, and
//! [`crate::epoll`] is consulted where `poll_sockets` would have been. A kernel
//! that will not open one falls back to `poll`, which is slower and correct.
//!
//! Windows and macOS still use `poll`. `WSAPoll` is not what a scalable Windows
//! server uses — IOCP is — but IOCP is *completion*-based, and the operations
//! above this one perform their own syscall and ask the reactor only when it
//! would have blocked. Making IOCP answer that shape means owning the buffer
//! and the operation, which is a different interface rather than a different
//! backend; `docs/design/scheduler.md` §2 has the argument and
//! `docs/release-readiness.md` has it as open. `kqueue` is the same shape as
//! `epoll` and is the smaller of the two remaining jobs, and it is not written
//! because nothing here can run it.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// A socket, as this platform names one.
///
/// Windows' `SOCKET` is a pointer-sized handle rather than a small integer, so
/// this is not `i32` everywhere and code that assumed it was would be wrong on
/// exactly one platform.
#[cfg(windows)]
pub(crate) type Socket = usize;
#[cfg(not(windows))]
pub(crate) type Socket = i32;

/// What a fiber is waiting for a socket to become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interest {
    Readable,
    Writable,
}

/// One fiber waiting on one socket.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Watch {
    pub(crate) socket: Socket,
    pub(crate) interest: Interest,
    pub(crate) fiber: usize,
    /// When to give up, if the caller set a receive deadline.
    ///
    /// **Here rather than in the timer heap, and that is a measurement.** A
    /// socket read that would block used to push a deadline onto
    /// `crate::wait::Timers` — a global mutex and an `O(log n)` insertion per
    /// read. `bench/service` did 1,262,225 of them in five seconds against
    /// 1,262,221 socket waits, which is one apiece, and the heap grew to a
    /// million entries of ten-second deadlines the process never lived long
    /// enough to see come due.
    ///
    /// A deadline belongs to the wait it bounds, and the reactor is already
    /// holding every wait. So it rides along: `poll` shortens its own timeout
    /// to the soonest one and reports whatever has passed, and the timer heap
    /// goes back to being for `sleep`.
    pub(crate) deadline: Option<std::time::Instant>,
}

/// Every registered wait, indexed by socket and by fiber.
///
/// **Indexed because a flat list made every wait cost the whole server.**
/// `register`, a readiness, `forget` and the kernel mask each scanned or
/// rebuilt the entire list under the one lock every worker and the reactor
/// thread take: at 256 connections on `bench/service` that lock was
/// contended on 15% of acquisitions and the scheduler left 30% of its CPUs
/// idle. With the index, each of those touches only the watches on one
/// socket, usually one.
///
/// What it costs: two hash-map updates per registration and per removal, and
/// a `Vec` per socket and per fiber that is almost always of length one.
#[derive(Default)]
struct Watches {
    by_socket: std::collections::HashMap<Socket, Vec<Watch>>,
    /// The sockets each fiber is waiting on, so `forget` does not search.
    ///
    /// A socket appears once per watch, so a fiber that registered one socket
    /// twice has it twice here.
    by_fiber: std::collections::HashMap<usize, Vec<Socket>>,
    len: usize,
    /// No registered deadline is earlier than this.
    ///
    /// **A lower bound, not the minimum, and that is what keeps it cheap.**
    /// Removing a watch never raises it, so a stale bound only costs one scan
    /// that finds nothing and recomputes it — while a bound that could be
    /// *later* than a real deadline would let `poll` sleep past it.
    earliest: Option<std::time::Instant>,
}

impl Watches {
    fn add(&mut self, watch: Watch) {
        self.by_socket.entry(watch.socket).or_default().push(watch);
        self.by_fiber.entry(watch.fiber).or_default().push(watch.socket);
        self.len += 1;
        if let Some(at) = watch.deadline {
            self.earliest = Some(self.earliest.map_or(at, |e| e.min(at)));
        }
    }

    /// The watches on one socket, which is all `Epoll::sync` needs to see.
    fn on(&self, socket: Socket) -> &[Watch] {
        self.by_socket.get(&socket).map_or(&[], Vec::as_slice)
    }

    /// Takes one watch's socket out of its fiber's entry.
    fn unlink(&mut self, fiber: usize, socket: Socket) {
        if let Some(sockets) = self.by_fiber.get_mut(&fiber) {
            if let Some(at) = sockets.iter().position(|s| *s == socket) {
                sockets.swap_remove(at);
            }
            if sockets.is_empty() {
                self.by_fiber.remove(&fiber);
            }
        }
    }

    /// Removes every watch on `socket` that `take` selects, and answers them.
    fn take_on(&mut self, socket: Socket, mut take: impl FnMut(&Watch) -> bool) -> Vec<Watch> {
        let Some(list) = self.by_socket.get_mut(&socket) else { return Vec::new() };
        let mut taken = Vec::new();
        list.retain(|w| {
            if take(w) {
                taken.push(*w);
                false
            } else {
                true
            }
        });
        if list.is_empty() {
            self.by_socket.remove(&socket);
        }
        for watch in &taken {
            self.unlink(watch.fiber, socket);
        }
        self.len -= taken.len();
        taken
    }

    /// Removes every watch `fiber` holds, and answers the sockets touched.
    fn forget(&mut self, fiber: usize) -> Vec<Socket> {
        let Some(mut sockets) = self.by_fiber.remove(&fiber) else { return Vec::new() };
        sockets.sort_unstable();
        sockets.dedup();
        for socket in &sockets {
            if let Some(list) = self.by_socket.get_mut(socket) {
                let before = list.len();
                list.retain(|w| w.fiber != fiber);
                self.len -= before - list.len();
                if list.is_empty() {
                    self.by_socket.remove(socket);
                }
            }
        }
        sockets
    }

    /// Removes every watch whose deadline has passed, if any can have.
    ///
    /// Answers the watches removed. A full scan, but only when `earliest`
    /// says a deadline may have passed; afterwards `earliest` is exact again.
    fn expire(&mut self, now: std::time::Instant) -> Vec<Watch> {
        match self.earliest {
            Some(at) if at <= now => {}
            Some(_) | None => return Vec::new(),
        }
        let mut expired = Vec::new();
        let mut earliest: Option<std::time::Instant> = None;
        self.by_socket.retain(|_, list| {
            list.retain(|w| match w.deadline {
                Some(at) if at <= now => {
                    expired.push(*w);
                    false
                }
                Some(at) => {
                    earliest = Some(earliest.map_or(at, |e| e.min(at)));
                    true
                }
                None => true,
            });
            !list.is_empty()
        });
        for watch in &expired {
            self.unlink(watch.fiber, watch.socket);
        }
        self.len -= expired.len();
        self.earliest = earliest;
        expired
    }

    /// Every watch, for the `poll` backend, which hands the kernel the whole
    /// set on every call.
    fn all(&self) -> Vec<Watch> {
        self.by_socket.values().flatten().copied().collect()
    }
}

/// The descriptors fibers are waiting on.
#[derive(Default)]
pub(crate) struct Reactor {
    watching: Mutex<Watches>,
    /// The kernel-side set, where there is one.
    ///
    /// Opened on the first registration rather than in a constructor, for the
    /// same reason the waker is: a program that never waits on a socket should
    /// never open one.
    #[cfg(target_os = "linux")]
    scalable: std::sync::OnceLock<Option<crate::epoll::Epoll>>,
    /// Tests only: never open `epoll`, so Linux runs the `poll` fallback that
    /// macOS and Windows always run. Without it every test here exercised
    /// only `epoll` on the one platform they could be run on.
    #[cfg(all(test, target_os = "linux"))]
    without_epoll: bool,
    /// Set while a `poll` is in flight, so a caller can tell whether the
    /// reactor has looked since it registered.
    polling: AtomicBool,
    /// A socket the reactor also watches, so that registering can interrupt a
    /// `poll` already in flight. See [`Reactor::nudge`].
    waker: std::sync::OnceLock<Option<Nudge>>,
}

/// The two ends of a loopback pair used only to make `poll` return.
///
/// **Without this the reactor could not be told anything.** `poll` waits on the
/// set of sockets it was given, and a socket registered a microsecond later is
/// not in that set — so a fiber that had just parked waited for the timeout
/// rather than for its data. At a millisecond that is not a latency, it is a
/// throughput ceiling: `bench/service` answered 57,467 requests a second with
/// it and 613,571 with the reactor spinning instead, which is the same
/// scheduler doing the same work with the waiting taken out.
///
/// A loopback pair rather than an `eventfd` or a pipe because the reactor
/// already speaks sockets on all three platforms, and one mechanism that works
/// everywhere beats three that are each better.
struct Nudge {
    /// Watched by every `poll`. Drained and ignored.
    listen: std::net::TcpStream,
    /// Written to by `register`, to end a `poll` that is already waiting.
    poke: std::net::TcpStream,
}

impl Reactor {
    /// Records that `fiber` wants `socket` to become ready.
    pub(crate) fn register(&self, watch: Watch) {
        {
            let mut watching = self.watching.lock().expect("the reactor");
            watching.add(watch);
            // **Inside the lock, so the kernel and the list cannot disagree.**
            // `sync` writes the union of what the list holds; computing that
            // outside would race another registration on the same socket and
            // could describe an interest that had already been withdrawn.
            self.rearm(watch.socket, &watching);
        }
        // After the entry is visible, so a `poll` woken by this cannot look
        // before there is something to see.
        //
        // **Not on `epoll`, unless the deadline is close.** An `epoll_ctl`
        // reaches an `epoll_wait` already in progress, so the kernel reports
        // this socket to the poll in flight without being told; the nudge was
        // two syscalls and a spurious wake of the thread in that poll, paid on
        // half of all requests at 256 connections. What the kernel cannot do
        // is shorten a timeout already computed, so a deadline that could fall
        // inside the poll in flight still nudges. Every `poll` is capped at
        // `LONGEST_SLICE`, so a deadline beyond it cannot.
        //
        // **Limit: `polling` is one bool for two pollers.** When the reactor
        // thread and a worker are both in `poll`, one leaving clears the flag
        // while the other is still asleep; a nudge sent in that window is
        // dropped, and the deadline is reported at the end of that poller's
        // slice instead -- up to about `LONGEST_SLICE` (50 ms) late. Already
        // true on `main` before this change; not fixed here.
        let near = watch.deadline.is_some_and(|at| {
            at.saturating_duration_since(std::time::Instant::now()) < LONGEST_SLICE
        });
        // Tests only: `REACTOR_MUTANT=drop-fallback` skips the nudge the `poll`
        // fallback needs, and `drop-near` the one a near deadline needs, so
        // the two `..._during_a_poll_...` tests can be watched going red on
        // Linux. Compiled out of every build but `cargo test`.
        #[cfg(test)]
        let mutant = std::env::var("REACTOR_MUTANT").unwrap_or_default();
        #[cfg(test)]
        let (near, kernel) = (
            near && mutant != "drop-near",
            self.kernel_sees_registrations() || mutant == "drop-fallback",
        );
        #[cfg(not(test))]
        let kernel = self.kernel_sees_registrations();
        if near || !kernel {
            self.nudge();
        }
    }

    /// Whether a registration reaches a wait already in progress with no help.
    ///
    /// True for `epoll`, whose set lives in the kernel. False for `poll`,
    /// which was handed a copy of the list when it started.
    #[cfg(target_os = "linux")]
    fn kernel_sees_registrations(&self) -> bool {
        self.scalable().is_some()
    }

    #[cfg(not(target_os = "linux"))]
    fn kernel_sees_registrations(&self) -> bool {
        false
    }

    /// Forgets everything a fiber was waiting on.
    ///
    /// Called when it is woken by something else — a cancellation, or another
    /// registration completing first — so a stale entry does not wake it again
    /// later against a socket it has stopped caring about.
    pub(crate) fn forget(&self, fiber: usize) {
        let mut watching = self.watching.lock().expect("the reactor");
        for socket in watching.forget(fiber) {
            self.rearm(socket, &watching);
        }
    }

    /// Re-describes `socket` to the kernel from what the list still holds.
    ///
    /// Nothing on a platform with no scalable backend, where the whole set is
    /// handed to `poll` afresh every call and there is nothing to keep in step.
    #[cfg(target_os = "linux")]
    fn rearm(&self, socket: Socket, watching: &Watches) {
        if let Some(epoll) = self.scalable() {
            epoll.sync(socket, watching.on(socket));
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn rearm(&self, _socket: Socket, _watching: &Watches) {}

    /// The kernel-side set, opened once.
    #[cfg(target_os = "linux")]
    fn scalable(&self) -> Option<&crate::epoll::Epoll> {
        self.scalable
            .get_or_init(|| {
                #[cfg(test)]
                if self.without_epoll {
                    return None;
                }
                let epoll = crate::epoll::Epoll::open()?;
                // The waker is in the set from the moment there is a set, and
                // never leaves it.
                if let Some(nudge) = self.waker() {
                    epoll.watch_waker(socket_of(&nudge.listen));
                }
                Some(epoll)
            })
            .as_ref()
    }

    pub(crate) fn len(&self) -> usize {
        self.watching.lock().expect("the reactor").len
    }

    /// Waits up to `timeout` for any registered socket, and returns the fibers
    /// whose sockets are ready.
    ///
    /// Those entries are removed: a fiber is woken once per registration, and
    /// re-registers if its retry would block again.
    pub(crate) fn poll(&self, timeout: std::time::Duration) -> Vec<usize> {
        // **The waker goes into the set, and it is the reason the timeout can
        // be generous.** A registration arriving mid-`poll` writes a byte to
        // it, `poll` returns at once, and the next round includes the new
        // socket. Without it the only thing that ended a wait was the timeout,
        // and every fiber that parked a moment too late paid all of it.
        // **Claimed before the list is read, and the order is the whole
        // argument.** A registration that sees this false knows the reactor is
        // not waiting, so the entry it just pushed will be picked up when the
        // next `poll` reads the list — no nudge needed. A registration that
        // sees it true may or may not be in the list this round, so it nudges
        // and the reactor looks again. Reading the list first would leave a
        // window where a registration is neither in the list nor able to say
        // so, and that fiber waits for the timeout.
        self.polling.store(true, Ordering::Release);
        #[cfg(target_os = "linux")]
        let scalable = self.scalable().is_some();
        #[cfg(not(target_os = "linux"))]
        let scalable = false;
        // **Only the `poll` backend needs the whole set copied out.** `epoll`
        // holds it in the kernel, and copying it anyway was a scan of every
        // wait in the server, under the lock every registration takes, on
        // every pass.
        let (mut watching, empty, earliest) = {
            let held = self.watching.lock().expect("the reactor");
            let copy = if scalable { Vec::new() } else { held.all() };
            (copy, held.len == 0, held.earliest)
        };
        let waking = self.waker().map(|nudge| Watch {
            socket: socket_of(&nudge.listen),
            interest: Interest::Readable,
            fiber: WAKER_FIBER,
            deadline: None,
        });
        if empty && waking.is_none() {
            // Nothing to wait on and no way to be told. Sleeping rather than
            // spinning, because the alternative is a thread at a hundred per
            // cent doing nothing.
            self.polling.store(false, Ordering::Release);
            std::thread::sleep(timeout.min(std::time::Duration::from_millis(1)));
            return Vec::new();
        }
        if let Some(waking) = waking {
            watching.push(waking);
        }

        // **Never wait past the soonest deadline.** `earliest` may be earlier
        // than any deadline still registered, which costs an early return,
        // never a late one.
        let now = std::time::Instant::now();
        let timeout = timeout.min(LONGEST_SLICE);
        let slice = match earliest {
            Some(at) => timeout.min(at.saturating_duration_since(now)),
            None => timeout,
        };

        let mut woken = Vec::new();

        if scalable {
            #[cfg(target_os = "linux")]
            {
                let epoll = self.scalable().expect("just checked");
                let ready = epoll.wait(slice);
                self.polling.store(false, Ordering::Release);
                let mut watching = self.watching.lock().expect("the reactor");
                let waker = self.waker().map(|nudge| socket_of(&nudge.listen));
                for (socket, events) in ready {
                    if Some(socket) == waker {
                        self.drain();
                        continue;
                    }
                    // **Every watch on the socket that the events answer**,
                    // because `epoll` reports a descriptor and the list is
                    // keyed by wait: two fibers on one socket are one event.
                    for watch in watching.take_on(socket, |w| crate::epoll::wakes(w, events)) {
                        woken.push(watch.fiber);
                    }
                    // Disarmed here if nothing is left waiting on it, which is
                    // what keeps a level that stays high from spinning.
                    self.rearm(socket, &watching);
                }
            }
        } else {
            let ready = poll_sockets(&watching, slice);
            self.polling.store(false, Ordering::Release);

            let mut watching = self.watching.lock().expect("the reactor");
            for index in ready {
                if index.fiber == WAKER_FIBER {
                    self.drain();
                    continue;
                }
                // At most one: the entry `poll` reported, if nobody took it
                // off in the meantime.
                let mut first = true;
                let taken = watching.take_on(index.socket, |w| {
                    let hit = first && w.fiber == index.fiber;
                    first &= !hit;
                    hit
                });
                woken.extend(taken.iter().map(|w| w.fiber));
            }
        }
        let mut watching = self.watching.lock().expect("the reactor");
        // Whatever ran out of time leaves by the same door: the fiber retries,
        // finds nothing, and `wait_until_ready_by` sees its deadline has
        // passed. A timeout and a readiness are the same event to everything
        // above here.
        let expired = watching.expire(std::time::Instant::now());
        for watch in &expired {
            woken.push(watch.fiber);
            self.rearm(watch.socket, &watching);
        }
        woken
    }

    /// The waker pair, made the first time anything registers.
    ///
    /// Late rather than in a constructor because `Reactor` is `Default` and a
    /// loopback connection is not something to open in one — a program that
    /// never waits on a socket should never open it.
    fn waker(&self) -> Option<&Nudge> {
        self.waker
            .get_or_init(|| match connected_pair() {
                Ok((listen, poke)) => {
                    // Neither end may ever block. A full buffer means a wakeup
                    // is already pending, which is exactly as good as another.
                    let _ = listen.set_nonblocking(true);
                    let _ = poke.set_nonblocking(true);
                    Some(Nudge { listen, poke })
                }
                // No pair, so no waker: the timeout is all there is, which is
                // slow rather than wrong.
                Err(_) => None,
            })
            .as_ref()
    }

    /// Ends a `poll` that is already waiting.
    pub(crate) fn nudge(&self) {
        // Only when somebody is actually waiting. A nudge is two syscalls —
        // the write here and the read that drains it — and paying them when
        // the reactor is between polls buys nothing, because the next poll
        // reads the list afresh.
        if !self.polling.load(Ordering::Acquire) {
            return;
        }
        if let Some(nudge) = self.waker() {
            use std::io::Write;
            // A full buffer is a wakeup already on its way.
            let _ = (&nudge.poke).write(&[1]);
        }
    }

    /// Throws away whatever [`Reactor::nudge`] wrote.
    fn drain(&self) {
        if let Some(nudge) = self.waker() {
            use std::io::Read;
            let mut bin = [0u8; 256];
            while let Ok(read) = (&nudge.listen).read(&mut bin) {
                if read < bin.len() {
                    break;
                }
            }
        }
    }
}

/// The fiber id the waker's own entry carries, which belongs to no fiber.
///
/// `usize::MAX` rather than zero: zero is what `block_until_ready` uses for a
/// wait that has no fiber behind it, and two meanings on one number is how the
/// wrong one gets woken.
const WAKER_FIBER: usize = usize::MAX;

/// The longest any [`Reactor::poll`] waits, whatever it is asked for.
///
/// **A registration relies on this to skip the nudge.** One whose deadline is
/// further away than this cannot be missed by a poll already in flight, since
/// that poll returns first and the next one sees the deadline.
const LONGEST_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

/// Every watch whose socket is ready, or an empty list if the wait timed out.
///
/// One function per platform rather than one with two halves inside it. They
/// do the same thing and say it the same way; what differs is the width of a
/// descriptor and the spelling of the ready flags, and neither is worth a
/// `cfg` in the middle of a body.
///
/// A closed or broken socket counts as **ready**. The retry will see the error
/// and report it, which is the fiber's business rather than the reactor's — and
/// a reactor that ignored a hangup would leak a fiber per disconnect.
#[cfg(windows)]
fn poll_sockets(watching: &[Watch], timeout: std::time::Duration) -> Vec<Watch> {
    use windows_sys::Win32::Networking::WinSock::{
        WSAPoll, POLLERR, POLLHUP, POLLNVAL, POLLRDNORM, POLLWRNORM, WSAPOLLFD,
    };

    let mut fds: Vec<WSAPOLLFD> = watching
        .iter()
        .map(|w| WSAPOLLFD {
            fd: w.socket,
            events: match w.interest {
                Interest::Readable => POLLRDNORM,
                Interest::Writable => POLLWRNORM,
            },
            revents: 0,
        })
        .collect();

    // SAFETY: `fds` is a valid array of `fds.len()` entries for the duration of
    // the call, which is what `WSAPoll` documents as its contract.
    let count = unsafe { WSAPoll(fds.as_mut_ptr(), fds.len() as u32, millis(timeout)) };
    if count <= 0 {
        return Vec::new();
    }
    let interesting = POLLRDNORM | POLLWRNORM | POLLERR | POLLHUP | POLLNVAL;
    ready(&fds, watching, |fd| fd.revents & interesting != 0)
}

/// The same, for Linux and macOS. See the Windows one above.
#[cfg(not(windows))]
fn poll_sockets(watching: &[Watch], timeout: std::time::Duration) -> Vec<Watch> {
    let mut fds: Vec<libc::pollfd> = watching
        .iter()
        .map(|w| libc::pollfd {
            fd: w.socket,
            events: match w.interest {
                Interest::Readable => libc::POLLIN,
                Interest::Writable => libc::POLLOUT,
            },
            revents: 0,
        })
        .collect();

    // SAFETY: as above. `poll` reads `fds.len()` entries and writes `revents`
    // into each of them.
    let count =
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis(timeout)) };
    if count <= 0 {
        return Vec::new();
    }
    let interesting =
        libc::POLLIN | libc::POLLOUT | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
    ready(&fds, watching, |fd| fd.revents & interesting != 0)
}

/// A timeout as the milliseconds both calls want.
fn millis(timeout: std::time::Duration) -> i32 {
    timeout.as_millis().min(i32::MAX as u128) as i32
}

/// The watches whose descriptor `is_ready` says came back.
///
/// The two `poll` calls fill in parallel with `watching`, so pairing them is
/// the same on both and only the predicate differs.
fn ready<T>(fds: &[T], watching: &[Watch], is_ready: impl Fn(&T) -> bool) -> Vec<Watch> {
    fds.iter().zip(watching).filter(|(fd, _)| is_ready(fd)).map(|(_, w)| *w).collect()
}

/// Blocks this thread until `socket` is ready.
///
/// For a program with no scheduler to park a fiber on. A socket in
/// non-blocking mode would otherwise spin, and spinning is worse than the
/// blocking read this replaced.
///
/// Answers false when the wait ended without readiness — the deadline passed,
/// or this fiber was cancelled.
pub(crate) fn block_until_ready(
    socket: Socket,
    interest: Interest,
    deadline: Option<std::time::Instant>,
) -> bool {
    // The deadline is honoured by the caller's own loop here rather than by a
    // reactor that is not running: this is the no-scheduler path.
    let watch = [Watch { socket, interest, fiber: 0, deadline: None }];
    loop {
        // **A cancellation ends this wait, and only a check here can see it.**
        // The fiber running this loop executes nothing else: `accept` on an
        // idle listener has no `!` and no back-edge above it, so there is no
        // cancellation point for the flag to be observed at. Without this,
        // cancelling a listener hung the process for ever with no message on
        // any stream — the flag was set on the right fiber and readable from
        // inside this loop, and nothing looked at it.
        //
        // The same call `crate::channel` makes for a parked receive, for the
        // same reason and deliberately the same predicate.
        if crate::current::current(|fiber| fiber.gives_up_waiting()) {
            return false;
        }
        // A long wait rather than an indefinite one, so a socket closed from
        // another thread does not leave this here for ever. `poll` reports a
        // hangup, but only if it is looking.
        let mut slice = std::time::Duration::from_millis(50);
        if let Some(at) = deadline {
            let left = at.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            slice = slice.min(left);
        }
        if !poll_sockets(&watch, slice).is_empty() {
            return true;
        }
    }
}

/// A pair of connected sockets over loopback, for tests.
///
/// There is no `socketpair` on Windows, so this is a listener, a connect and an
/// accept — which works everywhere and is what a test actually wants anyway,
/// since a real socket is the thing under test.
fn connected_pair() -> std::io::Result<(std::net::TcpStream, std::net::TcpStream)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let client = std::net::TcpStream::connect(address)?;
    let (server, _) = listener.accept()?;
    Ok((client, server))
}

/// The same, for tests, where a machine that cannot open a loopback socket is
/// a machine the test cannot run on anyway.
#[cfg(test)]
pub(crate) fn a_connected_pair() -> (std::net::TcpStream, std::net::TcpStream) {
    connected_pair().expect("a loopback pair")
}

/// The platform's handle for a `TcpStream`.
pub(crate) fn socket_of(stream: &std::net::TcpStream) -> Socket {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        stream.as_raw_socket() as Socket
    }
    #[cfg(not(windows))]
    {
        use std::os::fd::AsRawFd;
        stream.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Duration;

    /// Every backend this platform can run: `epoll` and the `poll` fallback on
    /// Linux, the fallback alone elsewhere.
    ///
    /// **The fallback is what macOS and Windows run**, and before this the
    /// `..._during_a_poll_...` tests only ever saw `epoll` on the one platform
    /// they could be run on, so a nudge missing from the fallback path could
    /// not fail anywhere but CI.
    fn every_backend() -> Vec<(&'static str, Reactor)> {
        #[cfg(target_os = "linux")]
        {
            vec![
                ("epoll", Reactor::default()),
                ("poll", Reactor { without_epoll: true, ..Reactor::default() }),
            ]
        }
        #[cfg(not(target_os = "linux"))]
        {
            vec![("poll", Reactor::default())]
        }
    }
    /// **The backend is the scalable one, and a fallback would be silent.**
    /// Every other test here passes either way, because a reactor that quietly
    /// went back to `poll` gives the same answers a little slower. So this one
    /// asks.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_waits_on_epoll_rather_than_poll() {
        let (client, _server) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 1,
            deadline: None,
        });
        assert!(
            reactor.scalable().is_some(),
            "the reactor fell back to `poll` on a kernel that has `epoll`"
        );
    }

    /// **Two fibers, one socket, one descriptor.** `epoll` is keyed by
    /// descriptor and the watch list by wait, so this is the case where the two
    /// are not one-to-one: one event has to wake both, and the mask registered
    /// has to be the union rather than whichever arrived last.
    #[test]
    fn two_fibers_on_one_socket_are_both_woken() {
        let (client, mut server) = a_connected_pair();
        let reactor = Reactor::default();
        for fiber in [11, 12] {
            reactor.register(Watch {
                socket: socket_of(&client),
                interest: Interest::Readable,
                fiber,
                deadline: None,
            });
        }
        server.write_all(b"x").expect("the write should land");

        let mut woken = Vec::new();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while woken.len() < 2 && std::time::Instant::now() < until {
            woken.extend(reactor.poll(Duration::from_millis(50)));
        }
        woken.sort_unstable();
        assert_eq!(woken, vec![11, 12], "both waiters should have been told");
        assert_eq!(reactor.len(), 0, "and neither should still be registered");
    }

    /// **A socket that stays readable must not spin.** Level-triggered
    /// readiness reports for as long as the data is there, so a reactor that
    /// left the descriptor armed after waking its fiber would report it again
    /// on every pass with nobody to wake. The fiber is woken once; the second
    /// look finds nothing and waits out its timeout.
    #[test]
    fn a_socket_left_readable_is_reported_once() {
        let (client, mut server) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 21,
            deadline: None,
        });
        // Never read by anybody, so the socket stays readable throughout.
        server.write_all(b"still here").expect("the write should land");

        let mut woken = Vec::new();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while woken.is_empty() && std::time::Instant::now() < until {
            woken.extend(reactor.poll(Duration::from_millis(50)));
        }
        assert_eq!(woken, vec![21]);

        let began = std::time::Instant::now();
        let again = reactor.poll(Duration::from_millis(80));
        assert!(again.is_empty(), "the descriptor was still armed with nobody waiting");
        assert!(
            began.elapsed() >= Duration::from_millis(50),
            "it returned at once, which is the spin this is about: {:?}",
            began.elapsed()
        );
    }

    /// A fiber that stops waiting takes its descriptor out of the kernel's set
    /// with it, so a later readiness wakes nobody rather than an id that has
    /// moved on.
    #[test]
    fn forgetting_a_fiber_stops_its_socket_waking_anything() {
        let (client, mut server) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 31,
            deadline: None,
        });
        reactor.forget(31);
        server.write_all(b"x").expect("the write should land");

        assert!(reactor.poll(Duration::from_millis(60)).is_empty());
        assert_eq!(reactor.len(), 0);
    }

    #[test]
    fn a_socket_with_nothing_on_it_is_not_ready() {
        let (client, _server) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 1,
            deadline: None,
        });

        assert!(reactor.poll(Duration::from_millis(20)).is_empty());
        assert_eq!(reactor.len(), 1, "an unready watch stays registered");
    }

    #[test]
    fn a_socket_with_something_on_it_wakes_its_fiber() {
        let (client, mut server) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 7,
            deadline: None,
        });

        server.write_all(b"hello").expect("writing");
        assert_eq!(reactor.poll(Duration::from_secs(5)), [7]);
        assert_eq!(reactor.len(), 0, "a woken watch is taken off");
    }

    /// Registers a watch while `reactor` is inside a `poll(LONGEST_SLICE)`,
    /// and answers what that poll and any after it reported, and how long
    /// from just before the poll began until something was reported.
    ///
    /// **Retries a round in which the test itself was late, and only that.**
    /// A lost wake shows as the poll in flight running its whole
    /// `LONGEST_SLICE`, so the bound the callers assert (`LONGEST_SLICE` less
    /// 15 ms) only tells a lost wake from a delivered one if the registration
    /// lands well inside that slice. The two macOS CI failures, 43.6 ms and
    /// 57.5 ms, are what a *delivered* 5 ms deadline gives when the
    /// registration lands at about 38 ms and 52 ms -- and 43.6 ms is shorter
    /// than any lost wake can be, since the poll in flight runs 50 ms. So the
    /// test thread was late, not the reactor. A registration later than
    /// `REGISTER_BY` says nothing about the reactor either way, so that round
    /// is repeated rather than judged; every round that *is* judged has the
    /// original bound. A runner that never registers in time fails, saying
    /// so, and never passes.
    fn register_during_a_poll(
        name: &str,
        reactor: Reactor,
        watch: impl Fn() -> Watch,
    ) -> (Vec<usize>, Duration) {
        const REGISTER_BY: Duration = Duration::from_millis(10);
        let reactor = std::sync::Arc::new(reactor);
        // Opens the backend and the waker, so the poll below is a real wait
        // and not the first-use setup.
        let (idle, _idle_peer) = a_connected_pair();
        reactor.register(Watch {
            socket: socket_of(&idle),
            interest: Interest::Readable,
            fiber: 1,
            deadline: None,
        });
        let mut late = Vec::new();
        for _ in 0..50 {
            let polling = reactor.clone();
            let began = std::time::Instant::now();
            let waiter = std::thread::spawn(move || polling.poll(LONGEST_SLICE));
            while !reactor.polling.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(5));
            let watch = watch();
            let registered = began.elapsed();
            reactor.register(watch);
            let mut woken = waiter.join().expect("the poll");
            // A poll the nudge ended may return before a deadline has passed,
            // and callers go round again; none of them may sleep past it.
            while woken.is_empty() && began.elapsed() < LONGEST_SLICE * 2 {
                woken = reactor.poll(LONGEST_SLICE);
            }
            let took = began.elapsed();
            if registered <= REGISTER_BY {
                return (woken, took);
            }
            // Leave the reactor as the round found it.
            reactor.forget(watch.fiber);
            late.push(registered);
        }
        panic!("{name}: the test never registered within {REGISTER_BY:?} of the poll, so it could not judge the reactor: {late:?}");
    }

    /// **A registration made while a `poll` is already waiting is seen by that
    /// poll.** `register` skips the nudge on `epoll` because `epoll_ctl`
    /// reaches an `epoll_wait` in progress, and must *not* skip it on the
    /// `poll` fallback -- what macOS and Windows run -- whose set was copied
    /// when the poll began. Either way round, a mistake here reports the
    /// socket when the poll times out rather than when its data arrived, and
    /// every request that registered mid-poll pays the slice.
    #[test]
    fn a_registration_during_a_poll_is_seen_by_that_poll() {
        for (name, reactor) in every_backend() {
            let (client, mut server) = a_connected_pair();
            server.write_all(b"x").expect("the write should land");
            let (woken, took) = register_during_a_poll(name, reactor, || Watch {
                socket: socket_of(&client),
                interest: Interest::Readable,
                fiber: 2,
                deadline: None,
            });
            assert_eq!(woken, [2], "{name}: the poll in flight did not report the new socket");
            assert!(
                took < LONGEST_SLICE - Duration::from_millis(15),
                "{name}: it waited out its slice: {took:?}"
            );
        }
    }

    /// **A deadline registered during a poll ends that poll.** The kernel
    /// cannot shorten a timeout already computed, so this is the one
    /// registration on `epoll` that still has to nudge.
    #[test]
    fn a_deadline_registered_during_a_poll_is_honoured_by_that_poll() {
        for (name, reactor) in every_backend() {
            let (quiet, _quiet_peer) = a_connected_pair();
            let (woken, took) = register_during_a_poll(name, reactor, || Watch {
                socket: socket_of(&quiet),
                interest: Interest::Readable,
                fiber: 3,
                deadline: Some(std::time::Instant::now() + Duration::from_millis(5)),
            });
            assert_eq!(woken, [3], "{name}: the deadline was not reported");
            assert!(
                took < LONGEST_SLICE - Duration::from_millis(15),
                "{name}: it slept past the deadline: {took:?}"
            );
        }
    }

    /// Only the socket that became ready. The whole point is that one busy
    /// connection does not wake the other ninety-nine thousand.
    #[test]
    fn only_the_ready_socket_wakes() {
        let (quiet, _quiet_peer) = a_connected_pair();
        let (busy, mut busy_peer) = a_connected_pair();

        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&quiet),
            interest: Interest::Readable,
            fiber: 1,
            deadline: None,
        });
        reactor.register(Watch { socket: socket_of(&busy), interest: Interest::Readable, fiber: 2, deadline: None });

        busy_peer.write_all(b"x").expect("writing");
        assert_eq!(reactor.poll(Duration::from_secs(5)), [2]);
        assert_eq!(reactor.len(), 1, "the quiet one is still waiting");
    }

    /// A connected socket is writable straight away, which is what a `send`
    /// that would have blocked comes back to.
    #[test]
    fn a_writable_socket_is_ready_immediately() {
        let (client, _server) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Writable,
            fiber: 3,
            deadline: None,
        });
        assert_eq!(reactor.poll(Duration::from_secs(5)), [3]);
    }

    /// A peer that hangs up is *ready*, not silent. A fiber waiting on a closed
    /// socket must be woken so its retry can see the end of the stream —
    /// otherwise every disconnect leaks a fiber.
    #[test]
    fn a_closed_peer_wakes_the_reader() {
        let (client, server) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 9,
            deadline: None,
        });

        drop(server);
        assert_eq!(reactor.poll(Duration::from_secs(5)), [9]);
    }

    #[test]
    fn forgetting_a_fiber_takes_all_of_its_watches_off() {
        let (a, _pa) = a_connected_pair();
        let (b, _pb) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch { socket: socket_of(&a), interest: Interest::Readable, fiber: 4, deadline: None });
        reactor.register(Watch { socket: socket_of(&b), interest: Interest::Readable, fiber: 4, deadline: None });
        reactor.register(Watch { socket: socket_of(&b), interest: Interest::Readable, fiber: 5, deadline: None });

        reactor.forget(4);
        assert_eq!(reactor.len(), 1);
    }

    #[test]
    fn polling_nothing_is_not_a_spin() {
        let reactor = Reactor::default();
        let at = std::time::Instant::now();
        assert!(reactor.poll(Duration::from_millis(5)).is_empty());
        assert!(at.elapsed() >= Duration::from_micros(500), "it should have waited");
    }

    /// Several sockets ready at once come back together, so one pass of the
    /// reactor wakes all of them rather than one per pass.
    #[test]
    fn everything_ready_comes_back_in_one_pass() {
        let mut peers = Vec::new();
        let reactor = Reactor::default();
        for fiber in 0..8usize {
            let (client, mut peer) = a_connected_pair();
            peer.write_all(b"x").expect("writing");
            reactor.register(Watch {
                socket: socket_of(&client),
                interest: Interest::Readable,
                fiber,
                deadline: None,
            });
            // Held so the sockets stay open.
            peers.push((client, peer));
        }

        let mut woken = reactor.poll(Duration::from_secs(5));
        woken.sort();
        assert_eq!(woken, (0..8).collect::<Vec<_>>());
        assert_eq!(reactor.len(), 0);
    }

    /// The data is really there when the fiber is woken, which is the property
    /// the retry depends on.
    #[test]
    fn the_bytes_are_there_when_the_wake_arrives() {
        let (mut client, mut server) = a_connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 1,
            deadline: None,
        });

        server.write_all(b"payload").expect("writing");
        assert_eq!(reactor.poll(Duration::from_secs(5)), [1]);

        let mut buffer = [0u8; 7];
        client.read_exact(&mut buffer).expect("reading");
        assert_eq!(&buffer, b"payload");
    }

    /// **A descriptor number reused after its deadline expired must be
    /// watched again.** Base left the expired socket armed in `Epoll::armed`
    /// after the deadline pass removed its watch without rearming. When the
    /// number was reused by a fresh connection with the same interest mask,
    /// `Epoll::set` saw the mask unchanged and made no `epoll_ctl` call, so
    /// the kernel -- which had already dropped the closed descriptor's item
    /// -- never told the reactor the new connection was ready. The fiber
    /// waiting on it hung until its own deadline, or forever if it set none.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_descriptor_reused_after_its_deadline_expired_is_watched_again() {
        use std::os::fd::{AsRawFd, IntoRawFd};
        let reactor = Reactor::default();
        let (first, _first_peer) = a_connected_pair();
        let number = socket_of(&first);
        reactor.register(Watch {
            socket: number,
            interest: Interest::Readable,
            fiber: 1,
            deadline: Some(std::time::Instant::now() + Duration::from_millis(5)),
        });
        let began = std::time::Instant::now();
        let mut woken = Vec::new();
        while woken.is_empty() && began.elapsed() < Duration::from_secs(1) {
            woken = reactor.poll(Duration::from_millis(50));
        }
        assert_eq!(woken, [1], "the deadline should end the first wait");
        reactor.forget(1); // what wait_until_ready_by does after park

        // A new connection lands on the same descriptor number, which is what
        // `accept` does with a number the server just closed. Done as one
        // `dup2` onto the live number rather than a close followed by a
        // `dup2`: tests run on threads of one process, and a number left free
        // for even a moment can be taken by another test's socket and closed
        // again under this one, which reads as the very lost wakeup this test
        // is looking for. `dup2` closes the old file and installs the new one
        // atomically, so the kernel still drops the old file's epoll item.
        let first = first.into_raw_fd(); // `number`, closed by the `dup2` below
        let (second, mut second_peer) = a_connected_pair();
        // SAFETY: test-only; `first` is `number` and owned here (taken out of
        // its stream above), and `dup2` closes it and gives the number to
        // `second`'s open file.
        let got = unsafe { libc::dup2(second.as_raw_fd(), first) };
        assert_eq!(got, number);
        drop(second); // only `number` refers to the new file now
        second_peer.write_all(b"x").unwrap();
        reactor.register(Watch { socket: number, interest: Interest::Readable, fiber: 2, deadline: None });
        let began = std::time::Instant::now();
        let mut woken = Vec::new();
        while woken.is_empty() && began.elapsed() < Duration::from_millis(500) {
            woken = reactor.poll(Duration::from_millis(50));
        }
        // SAFETY: we own `number` now (the `dup2` above made it ours; the
        // stream that used to own it was dropped without closing it).
        unsafe { libc::close(number) };
        assert_eq!(
            woken,
            [2],
            "LOST WAKEUP: data is waiting on a reused descriptor and the reactor never reported it"
        );
    }
}
