//! Waiting on a socket without blocking a worker.
//!
//! Phase 11C.2. [`crate::wait`] made a fiber able to wait for *something*, and
//! timers gave it one thing to wait for. This gives it the other: a descriptor
//! becoming ready.
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
//! The first line is the whole point and does not change. No `async`, no
//! `await`, no `Future`, no coloured functions — `std::net::socket` keeps its
//! blocking shape and every Khora program that already reads a socket benefits
//! without being edited.
//!
//! # `poll` first, and why that is not a reversal
//!
//! `docs/design/scheduler.md` §2 argues for an **operation-oriented** interface
//! — submit an operation, suspend until it completes — rather than a readiness
//! one, because IOCP is completion-based and forcing it to fake readiness costs
//! a buffer and a copy. That argument is about the *interface*, and the
//! interface here is exactly that: [`wait_until_ready`] is called by an
//! operation that has already tried and would block, and returns when it is
//! worth trying again. Nothing above the reactor learns which mechanism
//! answered.
//!
//! The mechanism underneath starts as `poll` — `WSAPoll` on Windows, and the
//! same call by the same name on Linux and macOS, with the same struct in a
//! different width. One code path, three platforms, correct on all of them,
//! and testable on the one this is written on.
//!
//! **It does not scale, and that is a deliberate staging rather than an
//! oversight.** `poll` is O(n) in registered descriptors per call, so a
//! hundred thousand waiting sockets would spend all its time in the kernel
//! walking a list. epoll, kqueue and IOCP are what that becomes, and they are
//! reached for the same way `io_uring` is: once the architecture above them is
//! proven and there is something to measure. The exit criterion's socket row
//! is *not* claimed by this module.

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
}

/// The descriptors fibers are waiting on.
#[derive(Default)]
pub(crate) struct Reactor {
    watching: Mutex<Vec<Watch>>,
    /// Set while a `poll` is in flight, so a caller can tell whether the
    /// reactor has looked since it registered.
    polling: AtomicBool,
}

impl Reactor {
    /// Records that `fiber` wants `socket` to become ready.
    pub(crate) fn register(&self, watch: Watch) {
        self.watching.lock().expect("the reactor").push(watch);
    }

    /// Forgets everything a fiber was waiting on.
    ///
    /// Called when it is woken by something else — a cancellation, or another
    /// registration completing first — so a stale entry does not wake it again
    /// later against a socket it has stopped caring about.
    pub(crate) fn forget(&self, fiber: usize) {
        self.watching.lock().expect("the reactor").retain(|w| w.fiber != fiber);
    }

    pub(crate) fn len(&self) -> usize {
        self.watching.lock().expect("the reactor").len()
    }

    /// Waits up to `timeout` for any registered socket, and returns the fibers
    /// whose sockets are ready.
    ///
    /// Those entries are removed: a fiber is woken once per registration, and
    /// re-registers if its retry would block again.
    pub(crate) fn poll(&self, timeout: std::time::Duration) -> Vec<usize> {
        let watching = self.watching.lock().expect("the reactor").clone();
        if watching.is_empty() {
            // Nothing to wait on. Sleeping here rather than spinning, because
            // the alternative is a thread at a hundred per cent doing nothing.
            std::thread::sleep(timeout.min(std::time::Duration::from_millis(1)));
            return Vec::new();
        }

        self.polling.store(true, Ordering::Release);
        let ready = poll_sockets(&watching, timeout);
        self.polling.store(false, Ordering::Release);

        if ready.is_empty() {
            return Vec::new();
        }
        let mut woken = Vec::new();
        let mut watching = self.watching.lock().expect("the reactor");
        for index in ready {
            let Some(entry) = watching.iter().position(|w| {
                w.socket == index.socket && w.fiber == index.fiber
            }) else {
                continue;
            };
            woken.push(watching.remove(entry).fiber);
        }
        woken
    }
}

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
pub(crate) fn block_until_ready(
    socket: Socket,
    interest: Interest,
    deadline: Option<std::time::Instant>,
) -> bool {
    let watch = [Watch { socket, interest, fiber: 0 }];
    loop {
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
#[cfg(test)]
pub(crate) fn connected_pair() -> (std::net::TcpStream, std::net::TcpStream) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener");
    let address = listener.local_addr().expect("its address");
    let client = std::net::TcpStream::connect(address).expect("connecting");
    let (server, _) = listener.accept().expect("accepting");
    (client, server)
}

/// The platform's handle for a `TcpStream`.
#[cfg(test)]
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

    #[test]
    fn a_socket_with_nothing_on_it_is_not_ready() {
        let (client, _server) = connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 1,
        });

        assert!(reactor.poll(Duration::from_millis(20)).is_empty());
        assert_eq!(reactor.len(), 1, "an unready watch stays registered");
    }

    #[test]
    fn a_socket_with_something_on_it_wakes_its_fiber() {
        let (client, mut server) = connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 7,
        });

        server.write_all(b"hello").expect("writing");
        assert_eq!(reactor.poll(Duration::from_secs(5)), [7]);
        assert_eq!(reactor.len(), 0, "a woken watch is taken off");
    }

    /// Only the socket that became ready. The whole point is that one busy
    /// connection does not wake the other ninety-nine thousand.
    #[test]
    fn only_the_ready_socket_wakes() {
        let (quiet, _quiet_peer) = connected_pair();
        let (busy, mut busy_peer) = connected_pair();

        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&quiet),
            interest: Interest::Readable,
            fiber: 1,
        });
        reactor.register(Watch { socket: socket_of(&busy), interest: Interest::Readable, fiber: 2 });

        busy_peer.write_all(b"x").expect("writing");
        assert_eq!(reactor.poll(Duration::from_secs(5)), [2]);
        assert_eq!(reactor.len(), 1, "the quiet one is still waiting");
    }

    /// A connected socket is writable straight away, which is what a `send`
    /// that would have blocked comes back to.
    #[test]
    fn a_writable_socket_is_ready_immediately() {
        let (client, _server) = connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Writable,
            fiber: 3,
        });
        assert_eq!(reactor.poll(Duration::from_secs(5)), [3]);
    }

    /// A peer that hangs up is *ready*, not silent. A fiber waiting on a closed
    /// socket must be woken so its retry can see the end of the stream —
    /// otherwise every disconnect leaks a fiber.
    #[test]
    fn a_closed_peer_wakes_the_reader() {
        let (client, server) = connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 9,
        });

        drop(server);
        assert_eq!(reactor.poll(Duration::from_secs(5)), [9]);
    }

    #[test]
    fn forgetting_a_fiber_takes_all_of_its_watches_off() {
        let (a, _pa) = connected_pair();
        let (b, _pb) = connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch { socket: socket_of(&a), interest: Interest::Readable, fiber: 4 });
        reactor.register(Watch { socket: socket_of(&b), interest: Interest::Readable, fiber: 4 });
        reactor.register(Watch { socket: socket_of(&b), interest: Interest::Readable, fiber: 5 });

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
            let (client, mut peer) = connected_pair();
            peer.write_all(b"x").expect("writing");
            reactor.register(Watch {
                socket: socket_of(&client),
                interest: Interest::Readable,
                fiber,
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
        let (mut client, mut server) = connected_pair();
        let reactor = Reactor::default();
        reactor.register(Watch {
            socket: socket_of(&client),
            interest: Interest::Readable,
            fiber: 1,
        });

        server.write_all(b"payload").expect("writing");
        assert_eq!(reactor.poll(Duration::from_secs(5)), [1]);

        let mut buffer = [0u8; 7];
        client.read_exact(&mut buffer).expect("reading");
        assert_eq!(&buffer, b"payload");
    }
}
