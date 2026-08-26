//! Socket operations that suspend a fiber instead of a worker.
//!
//! [`crate::reactor`] can say when a socket is ready; this is what
//! turns that into a `recv` a Khora program can call and not notice.
//!
//! # What `std::net::socket` sees
//!
//! The same shape it has now. `recv` takes a handle, a buffer and a length, and
//! returns how many bytes arrived — it simply calls one of these instead of the
//! C symbol. No `async`, no `await`, no second colour of function, and no
//! change to a single line of Khora above it.
//!
//! # Why the loop is here and not in Khora
//!
//! A non-blocking `recv` that would have blocked reports it through `errno` on
//! one platform and `WSAGetLastError` on another, and both are integers whose
//! meaning is a table. Putting the retry in Khora would mean teaching `std` to
//! read them, in three files, and getting `EAGAIN`, `EWOULDBLOCK` and
//! `WSAEWOULDBLOCK` right in each. Putting it here means `std` sees one
//! function that either worked or did not.
//!
//! # Off a scheduler
//!
//! A program that never spawns a fiber has no worker to give back, so there is
//! nothing to suspend and nothing to be fair to. These block the calling thread
//! instead, by polling that one socket — which is what the socket would have
//! done on its own before it was made non-blocking, so such a program behaves
//! exactly as it did.

#![allow(dead_code)]

use crate::reactor::{Interest, Socket};

/// Puts a socket into non-blocking mode.
///
/// Called once per socket by `std::net::socket`, after `socket` and after
/// `accept`. Once per socket rather than once per operation, because it is a
/// syscall and a read is not.
///
/// **A socket nobody prepared still works**: it blocks, exactly as it always
/// did, and the retry loops below simply never see a would-block. That is the
/// right failure mode for a socket that arrived from somewhere this runtime
/// does not know about.
///
/// Returns 0, or -1 with the platform's error left where the caller can read
/// it.
#[unsafe(no_mangle)]
pub extern "C" fn khora_net_prepare(socket: Socket) -> i32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{ioctlsocket, FIONBIO};
        let mut on: u32 = 1;
        // SAFETY: `socket` is a handle the caller owns and `on` is a live u32.
        unsafe { ioctlsocket(socket, FIONBIO, &raw mut on) }
    }
    #[cfg(not(windows))]
    {
        // SAFETY: an ordinary `fcntl` on a descriptor the caller owns.
        let flags = unsafe { libc::fcntl(socket, libc::F_GETFL, 0) };
        if flags < 0 {
            return -1;
        }
        // SAFETY: as above.
        unsafe { libc::fcntl(socket, libc::F_SETFL, flags | libc::O_NONBLOCK) }
    }
}

/// Whether the last socket call failed only because it would have blocked.
fn would_block() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{WSAGetLastError, WSAEWOULDBLOCK};
        // SAFETY: no arguments, and it reads this thread's last winsock error.
        unsafe { WSAGetLastError() == WSAEWOULDBLOCK }
    }
    #[cfg(not(windows))]
    {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        errno == libc::EAGAIN || errno == libc::EWOULDBLOCK
    }
}

/// Waits for a socket, by suspending the fiber or blocking the thread.
///
/// The choice is not the caller's and is not visible to it: on a worker the
/// fiber is parked and the worker goes on to something else, and off one the
/// thread waits. Both come back when it is worth trying again.
fn wait(socket: Socket, interest: Interest, deadline: Option<std::time::Instant>) -> bool {
    match crate::scheduler::wait_until_ready_by(socket, interest, deadline) {
        crate::scheduler::Waited::Ready => true,
        crate::scheduler::Waited::TimedOut => false,
        // No worker to give back, so this thread does the waiting — and has to
        // honour the same deadline, because a program with no scheduler is
        // still a program that asked for one.
        crate::scheduler::Waited::Unscheduled => {
            crate::reactor::block_until_ready(socket, interest, deadline)
        }
    }
}

/// Receive deadlines, by socket, in milliseconds.
///
/// **This is `SO_RCVTIMEO` moved somewhere it can still work.** The kernel's
/// receive timeout applies only to a call that would have blocked, and a socket
/// the reactor drives never has one — so the option goes silently inert the
/// moment `khora_net_prepare` touches the socket, and a server using it to shed
/// a slow client parks a fiber on that client for ever instead.
///
/// Keyed by the raw handle, which is sound only because the entry is removed
/// when the socket closes: handles are reused, and a stale deadline would
/// otherwise be inherited by whatever opened next. `khora_net_forget` is that
/// removal, and `std::net` calls it from `shut`.
static TIMEOUTS: std::sync::Mutex<Option<std::collections::HashMap<usize, u64>>> =
    std::sync::Mutex::new(None);

/// A socket as a table key.
///
/// `Socket` is a `usize` on Windows and an `i32` everywhere else, so exactly
/// one of the two platforms sees this cast as redundant and the other needs
/// it. One place to say so beats an allow at every use.
#[allow(clippy::unnecessary_cast)]
fn key(socket: Socket) -> usize {
    socket as usize
}

fn deadline_for(socket: Socket) -> Option<std::time::Instant> {
    let guard = TIMEOUTS.lock().expect("the receive deadlines");
    let millis = guard.as_ref()?.get(&key(socket)).copied()?;
    Some(std::time::Instant::now() + std::time::Duration::from_millis(millis))
}

/// Opens an outbound connection, resolving `host` first.
///
/// **The one place a Khora program dials out.** Written in Khora over
/// `connect(2)` it would mean `getaddrinfo`, `sockaddr` for two address
/// families, and one copy per platform of struct arithmetic that has no
/// business being in a standard library. Here it is once, in Rust:
/// `TcpStream::connect` brings DNS, IPv6 and the platform's resolver, and what
/// comes back is a handle the reactor takes over like an accepted one.
///
/// **It blocks the worker while it connects.** A DNS lookup is not something
/// the reactor can wait on, and `crate::blocking` is not reachable from a plain
/// `extern fn` yet. A connect happens once per connection where a query happens
/// many times, so this is the right thing to get wrong first — `docs/roadmap.md`
/// Phase 13.
///
/// Returns the handle, or -1.
///
/// # Safety
///
/// `host` must point at `host_len` bytes of UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_net_connect(host: *const u8, host_len: u64, port: i64) -> isize {
    // `isize` with -1 for failure, matching `raw_accept`. A `Socket` is a
    // `usize` on Windows and cannot carry the -1 that `invalid_handle` is on
    // both sides of the boundary.
    if host.is_null() || !(0..=65535).contains(&port) {
        return -1;
    }
    // SAFETY: the caller guarantees `host_len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(host, host_len as usize) };
    let Ok(name) = std::str::from_utf8(bytes) else { return -1 };

    let Ok(stream) = std::net::TcpStream::connect((name, port as u16)) else {
        return -1;
    };
    // Nagle off: a wire protocol writes a small message and waits for the
    // answer, which is the exact shape Nagle delays for no gain.
    let _ = stream.set_nodelay(true);

    // Handed to the reactor, so the descriptor must outlive this `TcpStream`.
    #[cfg(windows)]
    let handle = {
        use std::os::windows::io::IntoRawSocket;
        stream.into_raw_socket() as Socket
    };
    #[cfg(not(windows))]
    let handle = {
        use std::os::fd::IntoRawFd;
        stream.into_raw_fd() as Socket
    };

    if khora_net_prepare(handle) != 0 {
        return -1;
    }
    handle as isize
}

#[unsafe(no_mangle)]
/// How long a receive on `socket` may wait before it reports a timeout.
///
/// Replaces `setsockopt(SO_RCVTIMEO)`, which cannot fire on a socket the
/// reactor drives. Zero clears it.
pub extern "C" fn khora_net_set_timeout(socket: Socket, millis: i64) -> i32 {
    let mut guard = TIMEOUTS.lock().expect("the receive deadlines");
    let table = guard.get_or_insert_with(std::collections::HashMap::new);
    if millis <= 0 {
        table.remove(&key(socket));
    } else {
        table.insert(key(socket), millis as u64);
    }
    0
}

/// Forgets everything the runtime remembers about `socket`.
///
/// Called when a socket is closed. Not optional: handles are reused, and the
/// next connection to be handed this number would inherit the deadline.
#[unsafe(no_mangle)]
pub extern "C" fn khora_net_forget(socket: Socket) {
    if let Some(table) = TIMEOUTS.lock().expect("the receive deadlines").as_mut() {
        table.remove(&key(socket));
    }
}

/// `recv`, retried until it says something other than "not yet".
///
/// Returns what the platform's `recv` returns: the byte count, `0` for a peer
/// that has closed, or `-1` for a real failure with the error left in place.
///
/// # Safety
///
/// `into` must point at `length` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_net_recv(socket: Socket, into: *mut u8, length: isize) -> isize {
    // Absolute, and taken once: a read that goes round this loop several times
    // because of a spurious wake must not be granted the whole timeout again.
    let deadline = deadline_for(socket);
    loop {
        // SAFETY: the caller guarantees `length` writable bytes at `into`.
        let read = unsafe { raw_recv(socket, into, length) };
        if read >= 0 || !would_block() {
            return read;
        }
        if !wait(socket, Interest::Readable, deadline) {
            // **A negative return, and nothing else** — no `EAGAIN` to
            // imitate `SO_RCVTIMEO` down to the error number.
            //
            // Unnecessary: no Khora reads it. `std::net` looks at the sign, and
            // `std::fs` says outright that C's error numbers are a table it
            // declines to know.
            //
            // And unsound, because `errno` is thread-local and a fiber is not.
            // It would be set on whichever worker is running, and a fiber that
            // suspends before its caller looks — at any safepoint, which is
            // every loop back-edge — reads it off a thread that never made the
            // call. Any shim tempted to report through `errno` has the same
            // problem.
            return -1;
        }
    }
}

/// `send`, retried until it says something other than "not yet".
///
/// One attempt, not a loop over a partial write: a short write is the caller's
/// to notice, and `std::net::socket` already loops over one because a blocking
/// `send` could always return early.
///
/// # Safety
///
/// `from` must point at `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_net_send(socket: Socket, from: *const u8, length: isize) -> isize {
    loop {
        // SAFETY: the caller guarantees `length` readable bytes at `from`.
        let written = unsafe { raw_send(socket, from, length) };
        if written >= 0 || !would_block() {
            return written;
        }
        // A write that cannot proceed is back-pressure from the peer, and the
        // deadline `std::net` sets is a *receive* timeout. Left alone until
        // something asks for a send deadline by name.
        wait(socket, Interest::Writable, None);
    }
}

/// `accept`, retried until a connection arrives.
///
/// The accepted socket is **not** prepared here. `std::net::socket` calls
/// [`khora_net_prepare`] on it, in the same place it would have set any other
/// option — keeping every decision about a new connection in one visible spot
/// rather than half here and half there.
///
/// # Safety
///
/// `address` and `length` must be null, or a valid `sockaddr` buffer and the
/// live `socklen_t` describing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_net_accept(
    socket: Socket,
    address: *mut u8,
    length: *mut u8,
) -> isize {
    loop {
        // SAFETY: the caller guarantees the pair are null or a valid buffer
        // and its length.
        let accepted = unsafe { raw_accept(socket, address, length) };
        if accepted >= 0 || !would_block() {
            return accepted;
        }
        // No deadline on `accept`: a listener waiting for the next connection
        // is not a slow client, and a server that timed out its own accept
        // loop would be a server that stops serving.
        wait(socket, Interest::Readable, None);
    }
}

// --- the platform's own calls ------------------------------------------------
//
// Thin, and separate from the loops above so that the retry logic is written
// once and reads the same on every platform.

#[cfg(windows)]
unsafe fn raw_recv(socket: Socket, into: *mut u8, length: isize) -> isize {
    use windows_sys::Win32::Networking::WinSock::recv;
    // SAFETY: the caller's guarantee, narrowed to the `i32` winsock takes.
    unsafe { recv(socket, into, length.min(i32::MAX as isize) as i32, 0) as isize }
}

#[cfg(not(windows))]
unsafe fn raw_recv(socket: Socket, into: *mut u8, length: isize) -> isize {
    // SAFETY: the caller's guarantee.
    unsafe { libc::recv(socket, into.cast(), length as usize, 0) }
}

#[cfg(windows)]
unsafe fn raw_send(socket: Socket, from: *const u8, length: isize) -> isize {
    use windows_sys::Win32::Networking::WinSock::send;
    // SAFETY: the caller's guarantee, narrowed to the `i32` winsock takes.
    unsafe { send(socket, from, length.min(i32::MAX as isize) as i32, 0) as isize }
}

#[cfg(not(windows))]
unsafe fn raw_send(socket: Socket, from: *const u8, length: isize) -> isize {
    // SAFETY: the caller's guarantee.
    unsafe { libc::send(socket, from.cast(), length as usize, 0) }
}

#[cfg(windows)]
unsafe fn raw_accept(socket: Socket, address: *mut u8, length: *mut u8) -> isize {
    use windows_sys::Win32::Networking::WinSock::{accept, INVALID_SOCKET};
    // SAFETY: the caller's guarantee about the address pair.
    let accepted = unsafe { accept(socket, address.cast(), length.cast()) };
    if accepted == INVALID_SOCKET {
        -1
    } else {
        accepted as isize
    }
}

#[cfg(not(windows))]
unsafe fn raw_accept(socket: Socket, address: *mut u8, length: *mut u8) -> isize {
    // SAFETY: the caller's guarantee about the address pair.
    unsafe { libc::accept(socket, address.cast(), length.cast()) as isize }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactor::{a_connected_pair, socket_of};
    use std::io::Write;

    /// Off a scheduler this blocks the thread, so the bytes are there when it
    /// returns — which is what a program that never spawns a fiber expects,
    /// and what it got before any of this existed.
    #[test]
    fn a_read_off_a_scheduler_blocks_until_the_bytes_arrive() {
        let (client, mut server) = a_connected_pair();
        let socket = socket_of(&client);
        assert_eq!(khora_net_prepare(socket), 0, "the socket should go non-blocking");

        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            server.write_all(b"late").expect("writing");
        });

        let mut buffer = [0u8; 4];
        // SAFETY: four writable bytes at `buffer`.
        let read = unsafe { khora_net_recv(socket, buffer.as_mut_ptr(), 4) };
        assert_eq!(read, 4);
        assert_eq!(&buffer, b"late");
        writer.join().expect("the writer");
    }

    /// A peer that closes reports zero rather than blocking for ever, which is
    /// how every reader above this learns the stream ended.
    #[test]
    fn a_closed_peer_reads_zero() {
        let (client, server) = a_connected_pair();
        let socket = socket_of(&client);
        assert_eq!(khora_net_prepare(socket), 0);
        drop(server);

        let mut buffer = [0u8; 4];
        // SAFETY: four writable bytes at `buffer`.
        let read = unsafe { khora_net_recv(socket, buffer.as_mut_ptr(), 4) };
        assert_eq!(read, 0, "end of stream");
    }

    #[test]
    fn a_write_off_a_scheduler_reaches_the_peer() {
        use std::io::Read;
        let (client, mut server) = a_connected_pair();
        let socket = socket_of(&client);
        assert_eq!(khora_net_prepare(socket), 0);

        // SAFETY: five readable bytes.
        let written = unsafe { khora_net_send(socket, b"hello".as_ptr(), 5) };
        assert_eq!(written, 5);

        let mut buffer = [0u8; 5];
        server.read_exact(&mut buffer).expect("reading");
        assert_eq!(&buffer, b"hello");
    }

    /// **The point.** The same `recv`, on a worker, suspends the fiber instead
    /// of the thread — so one worker serves two connections that are both
    /// waiting.
    #[test]
    fn a_read_on_a_worker_suspends_the_fiber_and_not_the_worker() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        let done = Arc::new(AtomicUsize::new(0));
        let mut peers = Vec::new();
        let pool = Scheduler::new(1);

        for n in 0..2usize {
            let (client, peer) = a_connected_pair();
            let socket = socket_of(&client);
            assert_eq!(khora_net_prepare(socket), 0);
            let counter = done.clone();
            pool.spawn(Task::new(move || {
                let _client = client;
                let mut buffer = [0u8; 1];
                // SAFETY: one writable byte.
                let read = unsafe { khora_net_recv(socket, buffer.as_mut_ptr(), 1) };
                assert_eq!(read, 1);
                assert_eq!(buffer[0], n as u8);
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
            peers.push(peer);
        }

        // Both fibers are waiting on one worker, which on threads would mean
        // the second could not have started.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pool.watching() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "only {} of 2 fibers are waiting",
                pool.watching()
            );
            std::thread::yield_now();
        }

        for (n, peer) in peers.iter_mut().enumerate() {
            peer.write_all(&[n as u8]).expect("writing");
        }
        pool.drain();
        assert_eq!(done.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// A socket nobody prepared still works — it blocks, as it always did.
    #[test]
    fn an_unprepared_socket_still_reads() {
        let (client, mut server) = a_connected_pair();
        server.write_all(b"z").expect("writing");

        let mut buffer = [0u8; 1];
        // SAFETY: one writable byte.
        let read = unsafe { khora_net_recv(socket_of(&client), buffer.as_mut_ptr(), 1) };
        assert_eq!(read, 1);
        assert_eq!(buffer[0], b'z');
    }

    /// **A receive deadline still fires once the socket is non-blocking.**
    ///
    /// The regression `SO_RCVTIMEO` would have become. A connected, silent peer
    /// is exactly the slow client `std::net::http` sets ten seconds against;
    /// with the socket prepared the kernel's option can never fire, so the
    /// scheduler'''s timer has to — and it has to look from Khora exactly like
    /// the failed read it replaces, which is a negative return.
    #[test]
    fn a_receive_deadline_reports_a_timeout_the_way_the_kernel_did() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;

        let outcome = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen = outcome.clone();

        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            let (mine, _peer) = a_connected_pair();
            let socket = crate::reactor::socket_of(&mine);
            khora_net_prepare(socket);
            khora_net_set_timeout(socket, 60);

            let mut buffer = [0u8; 16];
            let began = std::time::Instant::now();
            // SAFETY: sixteen writable bytes.
            let read = unsafe { khora_net_recv(socket, buffer.as_mut_ptr(), 16) };
            khora_net_forget(socket);
            *seen.lock().expect("the outcome") = Some((read, began.elapsed()));
        }));
        pool.drain();

        let (read, took) = outcome.lock().expect("the outcome").expect("it ran");
        assert_eq!(read, -1, "a timed-out receive must look like a failed one");
        assert!(took >= std::time::Duration::from_millis(55), "returned early: {took:?}");
    }

    /// **A long deadline must not fire early**, which is the half of the timer
    /// anomaly that could reach a user.
    ///
    /// `std::net::http` sets ten seconds to shed a client that has stopped
    /// talking. A deadline that came due sooner would drop connections that
    /// were merely slow, and the counters in a `bench/service` run say
    /// something about deadlines is wrong — 763,737 registered and 692,795
    /// fired, inside a process that did not live ten seconds. This test says
    /// whether that reaches the read. It passes, so it does not.
    #[test]
    fn a_long_deadline_does_not_fire_early() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;

        let outcome = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen = outcome.clone();

        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            let (mine, mut peer) = a_connected_pair();
            let socket = socket_of(&mine);
            khora_net_prepare(socket);
            khora_net_set_timeout(socket, 5_000);

            std::thread::spawn(move || {
                use std::io::Write;
                std::thread::sleep(std::time::Duration::from_millis(300));
                let _ = peer.write_all(b"late but inside the deadline");
                std::thread::sleep(std::time::Duration::from_millis(400));
            });

            let mut buffer = [0u8; 64];
            let began = std::time::Instant::now();
            // SAFETY: sixty-four writable bytes.
            let read = unsafe { khora_net_recv(socket, buffer.as_mut_ptr(), 64) };
            khora_net_forget(socket);
            *seen.lock().expect("the outcome") = Some((read, began.elapsed()));
        }));
        pool.drain();

        let (read, took) = outcome.lock().expect("the outcome").expect("it ran");
        assert!(read > 0, "a five-second deadline cut off a read at {took:?}");
        assert!(took >= std::time::Duration::from_millis(250), "{took:?}");
        assert!(took < std::time::Duration::from_secs(4), "it waited far too long: {took:?}");
    }

    /// A socket with no deadline set waits as long as it takes.
    #[test]
    fn without_a_deadline_a_receive_waits() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;
        use std::io::Write;

        let got = std::sync::Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let seen = got.clone();

        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            let (mine, mut peer) = a_connected_pair();
            let socket = crate::reactor::socket_of(&mine);
            khora_net_prepare(socket);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(80));
                let _ = peer.write_all(b"late");
                std::thread::sleep(std::time::Duration::from_millis(200));
            });
            let mut buffer = [0u8; 16];
            // SAFETY: sixteen writable bytes.
            seen.store(unsafe { khora_net_recv(socket, buffer.as_mut_ptr(), 16) }, std::sync::atomic::Ordering::SeqCst);
        }));
        pool.drain();

        assert_eq!(got.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    /// Closing forgets the deadline, so the next socket to be handed that
    /// number does not inherit it.
    #[test]
    fn forgetting_a_socket_clears_its_deadline() {
        let (mine, _peer) = a_connected_pair();
        let socket = socket_of(&mine);
        khora_net_set_timeout(socket, 5_000);
        assert!(deadline_for(socket).is_some());
        khora_net_forget(socket);
        assert!(deadline_for(socket).is_none(), "a reused handle would inherit it");
    }
}
