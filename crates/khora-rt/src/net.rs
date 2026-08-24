//! Socket operations that suspend a fiber instead of a worker.
//!
//! Phase 11C.3. [`crate::reactor`] can say when a socket is ready; this is what
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
fn wait(socket: Socket, interest: Interest) {
    if crate::scheduler::wait_until_ready(socket, interest) {
        return;
    }
    crate::reactor::block_until_ready(socket, interest);
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
    loop {
        // SAFETY: the caller guarantees `length` writable bytes at `into`.
        let read = unsafe { raw_recv(socket, into, length) };
        if read >= 0 || !would_block() {
            return read;
        }
        wait(socket, Interest::Readable);
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
        wait(socket, Interest::Writable);
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
        wait(socket, Interest::Readable);
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
    use crate::reactor::{connected_pair, socket_of};
    use std::io::Write;

    /// Off a scheduler this blocks the thread, so the bytes are there when it
    /// returns — which is what a program that never spawns a fiber expects,
    /// and what it got before any of this existed.
    #[test]
    fn a_read_off_a_scheduler_blocks_until_the_bytes_arrive() {
        let (client, mut server) = connected_pair();
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
        let (client, server) = connected_pair();
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
        let (client, mut server) = connected_pair();
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
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let done = Arc::new(AtomicUsize::new(0));
        let mut peers = Vec::new();
        let pool = Scheduler::new(1);

        for n in 0..2usize {
            let (client, peer) = connected_pair();
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
                counter.fetch_add(1, Ordering::SeqCst);
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
        assert_eq!(done.load(Ordering::SeqCst), 2);
    }

    /// A socket nobody prepared still works — it blocks, as it always did.
    #[test]
    fn an_unprepared_socket_still_reads() {
        let (client, mut server) = connected_pair();
        server.write_all(b"z").expect("writing");

        let mut buffer = [0u8; 1];
        // SAFETY: one writable byte.
        let read = unsafe { khora_net_recv(socket_of(&client), buffer.as_mut_ptr(), 1) };
        assert_eq!(read, 1);
        assert_eq!(buffer[0], b'z');
    }
}
