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
///
/// **`#[inline(never)]` is load-bearing: it keeps `errno` read on the thread
/// that made the call.** `errno` is a thread-local, and the compiler treats
/// its address as fixed for the life of a function. Inlined into the retry
/// loops below, the development-profile build (which `khora build` from a
/// source checkout and every test suite link) took the address once, before
/// the loop, and reused it on every turn. The release profile inlined it too
/// and happened not to. A turn that suspends in [`wait`] can resume on another
/// worker, and the next failed `recv` was then judged by the errno of the
/// thread it left -- whatever that worker's last failed syscall had been. A
/// would-block read came back as a real failure, `khora_net_recv` returned -1
/// in the middle of a stream nobody had closed, and the Postgres driver took
/// that for a lost connection. `crate::current::running` is the same bug
/// about the running fiber.
///
/// **What makes it safe is that the function reading the thread-local has no
/// suspension point inside it.** Out of line, this computes errno's address
/// and reads it with nothing in between that can move the fiber; the only
/// suspension is in its callers, which no longer hold the address at all.
/// Whether the compiler hoists is an optimisation choice that differs by
/// profile, so it must not be what correctness rests on. What it costs: one
/// call per would-block, next to a syscall.
#[inline(never)]
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
        // **A wake is not readiness when it came from a cancellation.**
        // `cancel_fiber` flags and then wakes, which is how a parked fiber gets
        // a chance to notice — but a retry loop has no cancellation point to
        // notice at, so it would retry, block, and park again. The scheduler
        // backend reached here; the thread backend never did, because
        // `block_until_ready` had no exit at all. Both halves are needed.
        crate::scheduler::Waited::Ready => !crate::current::current(|f| f.gives_up_waiting()),
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

/// Reads whatever has already arrived, and never waits.
///
/// **The read that `shut` needs and `khora_net_recv` cannot be.** A close
/// wants to discard the bytes that were in flight; it does not want more.
/// `khora_net_recv` suspends the fiber when a read would block, which for a
/// peer that is open and silent means waiting for something that is not coming
/// -- and `docs/errata.md` 78 is what that cost: `shut` sat for exactly the
/// platform's FIN_WAIT_2 timeout, 120 seconds on Windows, because a half-closed
/// connection with a quiet peer is abandoned by the kernel rather than closed
/// by anybody.
///
/// So this is one syscall. A read that would have blocked is -1, the same as a
/// read that failed, because the caller does the same thing with both: stop.
///
/// # Safety
///
/// `into` must address `length` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_net_recv_now(
    socket: Socket,
    into: *mut u8,
    length: isize,
) -> isize {
    // SAFETY: the caller guarantees `length` writable bytes at `into`.
    let read = unsafe { raw_recv(socket, into, length) };
    if read >= 0 { read } else { -1 }
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

/// `send`, until every byte has gone or the send has failed.
///
/// **What this prevents: a large write reported as sent when most of it was
/// not.** On a non-blocking socket, `send` takes what fits in the kernel's
/// buffer (about 2.6 MB on Linux loopback) and reports that count. Every
/// caller in `std::net::socket` read any count that was not negative as
/// "sent", and so did the HTTP transport, which promises "the text, all of
/// it, or -1". A Postgres request with a 4 MB parameter went out as its
/// first 2.6 MB. The server waited for the rest of the frame and the driver
/// waited for its reply, so the connection hung.
///
/// So this waits for room and goes on until `length` bytes have gone. It
/// returns `length`, or -1 if a `send` failed or the fiber was cancelled
/// while it waited. After a -1 an unknown prefix may have gone, so the
/// stream is out of step and the caller should give up on the connection.
/// What it costs: a caller that wanted to do something else while a slow
/// peer drained its buffer cannot, which no caller here does.
///
/// # Safety
///
/// `from` must point at `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_net_send(socket: Socket, from: *const u8, length: isize) -> isize {
    let mut sent: isize = 0;
    while sent < length {
        // SAFETY: the caller guarantees `length` readable bytes at `from`,
        // and `sent < length`, so `length - sent` bytes remain from here.
        let written = unsafe { raw_send(socket, from.offset(sent), length - sent) };
        if written > 0 {
            sent += written;
            continue;
        }
        // Zero bytes accepted for a non-empty write is not progress, and no
        // platform promises the next attempt makes any. Treated as a failure
        // rather than retried for ever.
        if written == 0 || !would_block() {
            return -1;
        }
        // A write that cannot proceed is back-pressure from the peer, and the
        // deadline `std::net` sets is a *receive* timeout. Left alone until
        // something asks for a send deadline by name.
        //
        // Cancelled stops here too, for the reason `accept` does above.
        if !wait(socket, Interest::Writable, None) {
            return -1;
        }
    }
    sent
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
        //
        // **The answer is not discarded.** With no deadline, false means this
        // fiber was cancelled, and retrying then parks again on a socket that
        // will never be ready — which is the shape that hung a cancelled
        // listener for ever. A negative return is what every other failure
        // here gives back; `std::net` reads the sign.
        if !wait(socket, Interest::Readable, None) {
            return -1;
        }
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
    /// scheduler's timer has to — and it has to look from Khora exactly like
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

    /// **A send bigger than the socket buffer sends all of it.**
    ///
    /// A non-blocking `send` takes what fits in the kernel's buffer and says
    /// how much that was -- about 2.6 MB on Linux loopback. Every caller in
    /// `std::net::socket` treated any count that was not negative as "sent",
    /// so a Postgres request with a 4 MB parameter went out as its first
    /// 2.6 MB: the server waited for the rest of the frame and the driver
    /// waited for a reply, which is a hang with no receive deadline.
    ///
    /// Sixteen megabytes to a peer that starts reading late and slowly, once
    /// on a worker and once off one. Both must report every byte, and the peer
    /// must receive every byte.
    #[test]
    fn a_send_larger_than_the_socket_buffer_sends_all_of_it() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;
        use std::io::Read;
        use std::sync::{Arc, Mutex};

        const SIZE: usize = 16 << 20;

        fn a_slow_reader(mut peer: std::net::TcpStream) -> std::thread::JoinHandle<usize> {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let mut total = 0;
                let mut chunk = vec![0u8; 64 << 10];
                loop {
                    match peer.read(&mut chunk) {
                        Ok(0) | Err(_) => return total,
                        Ok(n) => total += n,
                    }
                }
            })
        }

        // On a worker.
        let (client, peer) = a_connected_pair();
        let socket = socket_of(&client);
        assert_eq!(khora_net_prepare(socket), 0);
        let reader = a_slow_reader(peer);
        let seen = Arc::new(Mutex::new(None));
        let said = seen.clone();
        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            let bytes = vec![7u8; SIZE];
            // SAFETY: `SIZE` readable bytes.
            let sent = unsafe { khora_net_send(socket, bytes.as_ptr(), SIZE as isize) };
            *said.lock().expect("the outcome") = Some(sent);
            drop(client);
        }));
        pool.drain();
        let sent = seen.lock().expect("the outcome").expect("it ran");
        assert_eq!(sent, SIZE as isize, "on a worker: a short write was reported as the whole send");
        assert_eq!(reader.join().expect("the reader"), SIZE, "on a worker: the peer got less");

        // Off one.
        let (client, peer) = a_connected_pair();
        let socket = socket_of(&client);
        assert_eq!(khora_net_prepare(socket), 0);
        let reader = a_slow_reader(peer);
        let bytes = vec![7u8; SIZE];
        // SAFETY: `SIZE` readable bytes.
        let sent = unsafe { khora_net_send(socket, bytes.as_ptr(), SIZE as isize) };
        drop(client);
        assert_eq!(sent, SIZE as isize, "off a worker: a short write was reported as the whole send");
        assert_eq!(reader.join().expect("the reader"), SIZE, "off a worker: the peer got less");
    }

    /// **A read that resumes on another worker judges its retry by its own
    /// thread's `errno`.**
    ///
    /// The retry loop decides "would block, wait again" from `errno`, which
    /// is per thread. With `would_block` inlined, the compiler took `errno`'s
    /// address once, before the loop, so after a wait that resumed the fiber
    /// on another worker, a retry that would block again was judged by the
    /// errno of the worker it had left. If that worker's last failed call was
    /// anything else, the read returned -1 in the middle of a live stream --
    /// which a Postgres connection took for the server hanging up.
    ///
    /// **The move is forced, not hoped for.** Before each read the reader
    /// queues an occupier on its own worker. Once the reader parks, that
    /// worker runs the occupier, which leaves `EBADF` in the worker's errno
    /// with a failing `close(-1)` and then spins, with no park and no yield,
    /// until the read is over. A wake can then only be taken up by another
    /// worker. A shouter thread wakes the reader over and over while it
    /// waits, so its retries would block again on the new worker and are
    /// judged by errno. The writer sends each byte only after the occupier
    /// has said where it landed, so every such read has a wait to move in.
    /// Left to chance, the reader went back to the worker it parked on: on
    /// the 3-core macOS runner, 30 s of reads never moved once.
    ///
    /// A thief can take the occupier before its own worker gets to it. That
    /// read proves nothing, is not counted, and the next read tries again.
    /// The test fails unless `MOVES` reads were forced within `DEADLINE`,
    /// and fails if a forced read did not move, because then the premise
    /// is wrong.
    ///
    /// **Which worker a read is on is itself a thread-local, and this test
    /// hit the bug it guards against.** With the worker read inline in the
    /// reader, the optimised macOS build looked it up once and reported
    /// every forced read as staying put. So `worker` is out of line, for the
    /// same reason as `would_block`. The mistake fails the test, never
    /// passes it: a stale read gives the same worker before and after.
    ///
    /// What it guards: only the development profile. The optimised runtime
    /// re-reads errno every turn with or without `#[inline(never)]` (see
    /// `would_block`), so under `--release` this passes either way.
    ///
    /// Unix only: the pollution is a failing `close`, and Windows keeps a
    /// socket's error somewhere else.
    #[cfg(not(windows))]
    #[test]
    fn a_read_that_changes_worker_is_not_failed_by_the_old_workers_errno() {
        use crate::coro::Task;
        use crate::scheduler::{Scheduler, Waker, schedule, waker_for_current};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        // Reads that must be forced off their worker. DEADLINE bounds the
        // wait for them; hitting it fails the test below.
        const MOVES: usize = 20;
        const DEADLINE: Duration = Duration::from_secs(30);
        // Occupiers queued per read. A thief takes half a queue per steal, so
        // sixteen outlast the few steals that fit in the moment between the
        // reader queueing them and parking.
        const OCCUPIERS: usize = 16;

        #[derive(Debug, Default)]
        struct Tally {
            reads: usize,
            failed: usize,
            // Reads whose worker an occupier held, and of those, how many
            // resumed on it anyway.
            forced: usize,
            stayed: usize,
            // Reads that changed worker, forced or not.
            moved: usize,
        }

        /// The worker running the caller, read afresh on every call.
        ///
        /// **Out of line, because an inlined read of it hit the bug this test
        /// is about.** `std::thread::current()` reads a thread-local, and as a
        /// closure in the reader's body the optimised aarch64-macOS build
        /// looked up that thread-local's address once, before the read loop,
        /// and used it again after `khora_net_recv` had moved the fiber. It
        /// then reported the worker the read left as the one it came back
        /// on: every forced read counted as staying, while an occupier spun
        /// on that worker. Called here, the address is found on the thread
        /// that makes the call.
        #[inline(never)]
        fn worker() -> std::thread::ThreadId {
            std::thread::current().id()
        }

        // Read numbers start at 1, so 0 is "none yet". `reading` is the read
        // in progress, `finished` the last one to return, and `held` the last
        // one whose worker an occupier took.
        let reading = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(AtomicUsize::new(0));
        let over = Arc::new(AtomicBool::new(false));
        let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let tally = Arc::new(Mutex::new(Tally::default()));

        // Wakes the reader, over and over, until the test is over. A wake
        // while it runs leaves a notification its next park consumes, so
        // each wait ends at once and the read retries: on whichever worker
        // took the wake, and judged by errno.
        let shouter = {
            let (waker, over) = (waker.clone(), over.clone());
            std::thread::spawn(move || {
                while !over.load(Ordering::SeqCst) {
                    if let Some(waker) = waker.lock().unwrap().as_ref() {
                        waker.wake();
                    }
                    std::thread::yield_now();
                }
            })
        };

        // Two workers: holding the reader's worker leaves exactly one other
        // for its wake to go to.
        let pool = Scheduler::new(2);
        let (client, mut peer) = a_connected_pair();
        let socket = socket_of(&client);
        assert_eq!(khora_net_prepare(socket), 0);
        {
            let (reading, finished, held) = (reading.clone(), finished.clone(), held.clone());
            let (over, waker, tally) = (over.clone(), waker.clone(), tally.clone());
            pool.spawn(Task::new(move || {
                let _client = client;
                *waker.lock().unwrap() = waker_for_current();
                let start = Instant::now();
                let mut seen = Tally::default();
                while seen.forced < MOVES && start.elapsed() < DEADLINE {
                    seen.reads += 1;
                    let n = seen.reads;
                    let before = worker();
                    reading.store(n, Ordering::SeqCst);
                    for _ in 0..OCCUPIERS {
                        let (finished, held) = (finished.clone(), held.clone());
                        // Lands on the reader's worker once the reader has
                        // parked, since the reader holds it until then.
                        let queued = schedule(Task::new(move || {
                            // Stolen onto another worker, or too late: this
                            // read is not held, and the next one tries again.
                            if finished.load(Ordering::SeqCst) >= n || worker() != before {
                                return;
                            }
                            held.store(n, Ordering::SeqCst);
                            // SAFETY: closing an invalid descriptor touches
                            // nothing; it fails with EBADF, which is the
                            // stale errno a moved read must not see.
                            unsafe { libc::close(-1) };
                            // No park and no yield, so this worker is not
                            // free again until the read is over.
                            while finished.load(Ordering::SeqCst) < n {
                                std::hint::spin_loop();
                            }
                        }));
                        assert!(queued, "the reader is on a worker, so this cannot fail");
                    }
                    let mut byte = [0u8; 1];
                    // SAFETY: one writable byte.
                    let read = unsafe { khora_net_recv(socket, byte.as_mut_ptr(), 1) };
                    let after = worker();
                    finished.store(n, Ordering::SeqCst);
                    // Sound to read after `finished`: an occupier that saw the
                    // read unfinished on `before` spins there until now, so
                    // the read cannot have come back to `before`.
                    let was_held = held.load(Ordering::SeqCst) == n;
                    seen.failed += usize::from(read != 1);
                    seen.moved += usize::from(after != before);
                    seen.forced += usize::from(was_held);
                    seen.stayed += usize::from(was_held && after == before);
                }
                *tally.lock().unwrap() = seen;
                over.store(true, Ordering::SeqCst);
            }));
        }

        // One byte per read, sent only after an occupier holds the reader's
        // worker (or 20 ms without one), and 10 ms after that, so the moved
        // reader has retried against an empty socket first. 2 ms was too
        // short on two CPUs: the occupier spins on one, and the reader and
        // the shouter share the other, so with `#[inline(never)]` removed
        // 2 of 10 runs saw the byte arrive before any retry and passed.
        let writer = {
            let (reading, finished, held, over) =
                (reading.clone(), finished.clone(), held.clone(), over.clone());
            std::thread::spawn(move || {
                let nap = || std::thread::sleep(Duration::from_micros(200));
                while !over.load(Ordering::SeqCst) {
                    let n = reading.load(Ordering::SeqCst);
                    if n == 0 || finished.load(Ordering::SeqCst) >= n {
                        nap();
                        continue;
                    }
                    let asked = Instant::now();
                    while held.load(Ordering::SeqCst) < n && asked.elapsed() < Duration::from_millis(20) {
                        nap();
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    if peer.write_all(b"x").is_err() {
                        break;
                    }
                    while finished.load(Ordering::SeqCst) < n && !over.load(Ordering::SeqCst) {
                        nap();
                    }
                }
                peer
            })
        };

        pool.drain();
        shouter.join().expect("the shouter");
        let _peer = writer.join().expect("the writer");

        let seen = std::mem::take(&mut *tally.lock().unwrap());
        eprintln!("errno test: {seen:?}");
        assert_eq!(
            seen.stayed, 0,
            "a read whose worker was held resumed on it anyway, so holding it does not force a move: {seen:?}"
        );
        assert!(
            seen.forced >= MOVES,
            "only {} reads were forced to change worker in {DEADLINE:?}, so this proves too little about errno: {seen:?}",
            seen.forced
        );
        assert_eq!(seen.failed, 0, "reads of a live stream failed: {seen:?}");
    }
}
