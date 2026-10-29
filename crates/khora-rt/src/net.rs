//! Socket operations that suspend a fiber instead of a worker.
//!
//! [`crate::reactor`] can say when a socket is ready; this is what
//! turns that into a `recv` a Khora program can call and not notice.
//!
//! # What `std::net::socket` sees
//!
//! The same shape it has now. `recv` takes a handle, a buffer and a length, and
//! returns how many bytes arrived — it simply calls one of these instead of the
//! C symbol. No `async`, no `await`, no second color of function, and no
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
/// On Apple's platforms it also sets `SO_NOSIGPIPE` (see [`no_sigpipe`]), so
/// that a peer's reset cannot kill the process through this socket.
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
        #[cfg(target_vendor = "apple")]
        if no_sigpipe(socket) != 0 {
            return -1;
        }
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
/// Whether the compiler hoists is an optimization choice that differs by
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

/// How a [`wait`] ended.
///
/// **Three answers rather than a `bool`, so the cause travels with the
/// failure.** With only "not ready", a caller that wanted to say *why* read
/// the clock afterwards, and a refused connect that happened to finish in the
/// deadline's last millisecond was reported as the deadline: about 2% of
/// refusals under a 1 ms budget came back `TimedOut`, misleading exactly the
/// retry logic that tells the two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Woke {
    /// Worth trying the operation again.
    Ready,
    /// The calling fiber was asked to stop.
    Stopped,
    /// The deadline passed with nothing to report.
    TimedOut,
}

/// Waits for a socket, by suspending the fiber or blocking the thread.
///
/// The choice is not the caller's and is not visible to it: on a worker the
/// fiber is parked and the worker goes on to something else, and off one the
/// thread waits. Both come back when it is worth trying again.
///
/// A `TimedOut` is the wait's account and not the operation's: the socket may
/// have become ready in the same instant. A caller that reports a deadline
/// tries the operation once more first, so that an answer that is there wins.
fn wait(socket: Socket, interest: Interest, deadline: Option<std::time::Instant>) -> Woke {
    let stopped = || crate::current::current(|f| f.gives_up_waiting());
    match crate::scheduler::wait_until_ready_by(socket, interest, deadline) {
        // **A wake is not readiness when it came from a cancellation.**
        // `cancel_fiber` flags and then wakes, which is how a parked fiber gets
        // a chance to notice — but a retry loop has no cancellation point to
        // notice at, so it would retry, block, and park again. The scheduler
        // backend reached here; the thread backend never did, because
        // `block_until_ready` had no exit at all. Both halves are needed.
        crate::scheduler::Waited::Ready if stopped() => Woke::Stopped,
        crate::scheduler::Waited::Ready => Woke::Ready,
        crate::scheduler::Waited::TimedOut if stopped() => Woke::Stopped,
        crate::scheduler::Waited::TimedOut => Woke::TimedOut,
        // No worker to give back, so this thread does the waiting — and has to
        // honor the same deadline, because a program with no scheduler is
        // still a program that asked for one. It answers false for a cancel
        // and for the deadline, and checks the cancel first, so the same
        // order here tells them apart.
        crate::scheduler::Waited::Unscheduled => {
            if crate::reactor::block_until_ready(socket, interest, deadline) {
                Woke::Ready
            } else if stopped() {
                Woke::Stopped
            } else {
                Woke::TimedOut
            }
        }
    }
}

/// What the runtime holds a socket to, by handle: how long one read may wait,
/// and when everything on the socket must be over.
///
/// **The per-read limit is `SO_RCVTIMEO` moved somewhere it can still work.**
/// The kernel's receive timeout applies only to a call that would have
/// blocked, and a socket the reactor drives never has one -- so the option
/// goes silently inert the moment `khora_net_prepare` touches the socket, and
/// a server using it to shed a slow client parks a fiber on that client for
/// ever instead.
///
/// **The deadline is the other kind of bound, and neither replaces the
/// other.** A per-read limit is renewed by every byte, so a peer that sends
/// one byte a second holds a reader for ever without tripping it. A deadline
/// is one instant, fixed when it is set, which every later read and write on
/// the socket answers to.
#[derive(Clone, Copy, Default)]
struct Limits {
    /// Milliseconds one read may wait.
    per_read: Option<u64>,
    /// When every read and write must be done.
    by: Option<std::time::Instant>,
    /// A read or write gave up because `by` passed. Kept until the socket is
    /// forgotten, so a caller can say the deadline ended the exchange rather
    /// than the peer.
    expired: bool,
}

/// [`Limits`], by socket.
///
/// Keyed by the raw handle, which is sound only because the entry is removed
/// when the socket closes: handles are reused, and a stale limit would
/// otherwise be inherited by whatever opened next. `khora_net_forget` is that
/// removal, and `std::net` calls it from `shut`.
static LIMITS: std::sync::Mutex<Option<std::collections::HashMap<usize, Limits>>> =
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

fn limits_of(socket: Socket) -> Limits {
    let guard = LIMITS.lock().expect("the socket limits");
    guard.as_ref().and_then(|table| table.get(&key(socket)).copied()).unwrap_or_default()
}

/// Changes what `socket` is held to, and drops the entry once it holds
/// nothing.
fn change_limits(socket: Socket, change: impl FnOnce(&mut Limits)) {
    let mut guard = LIMITS.lock().expect("the socket limits");
    let table = guard.get_or_insert_with(std::collections::HashMap::new);
    let limits = table.entry(key(socket)).or_default();
    change(limits);
    if limits.per_read.is_none() && limits.by.is_none() && !limits.expired {
        table.remove(&key(socket));
    }
}

/// When a read starting now must give up: the sooner of its own limit and
/// the socket's deadline.
fn deadline_for(socket: Socket) -> Option<std::time::Instant> {
    let limits = limits_of(socket);
    let per_read =
        limits.per_read.map(|millis| std::time::Instant::now() + std::time::Duration::from_millis(millis));
    match (per_read, limits.by) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// When a write on `socket` must give up: its deadline, if it has one.
fn send_deadline_for(socket: Socket) -> Option<std::time::Instant> {
    limits_of(socket).by
}

/// Records that `socket`'s own deadline ended an operation, if it did: the
/// wait timed out and the deadline is behind us. A per-read limit that ran
/// out first is not the deadline, and is left unrecorded.
fn note_if_expired(socket: Socket) {
    if limits_of(socket).by.is_some_and(|by| std::time::Instant::now() >= by) {
        change_limits(socket, |limits| limits.expired = true);
    }
}

/// Whether a read or write on `socket` gave up because the deadline set by
/// [`khora_net_set_deadline`] passed: 1 if so, 0 if not.
///
/// **What this prevents: a deadline reported as a peer that went quiet, and
/// the reverse.** Both are a -1 from the read. A caller that told them apart
/// by the clock afterwards called a reset that arrived in the deadline's last
/// millisecond a timeout; this is the read's own account.
#[unsafe(no_mangle)]
pub extern "C" fn khora_net_expired(socket: Socket) -> i32 {
    i32::from(limits_of(socket).expired)
}

/// Opens an outbound connection, resolving `host` first, and gives up when
/// the calling fiber is canceled or the monotonic clock reaches `at`.
///
/// **What this prevents: a connect nobody can stop.** A blocking `connect(2)`
/// to a host that drops SYNs holds its fiber until the kernel gives up, about
/// two minutes on Linux, and a canceled fiber inside one reached no
/// cancellation point for that long. So a nursery could not stop its
/// siblings, a server could not shut down, and no deadline around an outbound
/// call could fire. Here the socket is non-blocking from the start and the
/// fiber waits on the reactor for it to become writable, which is the wait a
/// cancel wakes. Off a scheduler the thread waits the way `accept` does, in
/// slices that look for a cancel between them.
///
/// **The name is resolved on the blocking pool, and abandoned on a stop.**
/// `getaddrinfo` cannot be waited on or interrupted, so a canceled caller
/// stops waiting for it and the lookup finishes alone on a pool thread; its
/// answer is dropped there. An address written as one (`127.0.0.1`, `::1`)
/// is not looked up at all.
///
/// Every resolved address is tried in the order the resolver gave, and the
/// first that connects wins, as `TcpStream::connect` does. A cancel or the
/// deadline ends the whole attempt, not just the address in hand.
///
/// `at` is a reading of `khora_monotonic_millis`, or negative for no deadline.
/// The deadline bounds connecting only: the socket that comes back carries
/// none, and `khora_net_set_deadline` is how a caller puts one on it.
///
/// Returns the handle, prepared as `khora_net_prepare` would and with Nagle
/// off; -2 if the deadline ended it; or -1: the name did not resolve, nothing
/// accepted, or the fiber was canceled. Whatever was opened is closed first,
/// including a connection that succeeded for a fiber already canceled.
///
/// # Safety
///
/// `host` must point at `host_len` bytes of UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_net_connect(
    host: *const u8,
    host_len: u64,
    port: i64,
    at: i64,
) -> isize {
    // `isize` with -1 for failure, matching `raw_accept`. A `Socket` is a
    // `usize` on Windows and cannot carry the -1 that `invalid_handle` is on
    // both sides of the boundary.
    if host.is_null() || !(0..=65535).contains(&port) {
        return -1;
    }
    // SAFETY: the caller guarantees `host_len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(host, host_len as usize) };
    let Ok(name) = std::str::from_utf8(bytes) else { return -1 };
    let deadline = (at >= 0).then(|| crate::time::instant_of(at));
    match connect_by(name, port as u16, deadline) {
        Ok(socket) => socket as isize,
        Err(Gave::Up | Gave::Stopped) => -1,
        Err(Gave::TimedOut) => -2,
    }
}

/// Holds every later read and write on `socket` to the monotonic clock
/// reaching `at`, or clears that with a negative `at`. Either way
/// [`khora_net_expired`] answers about this deadline from here on.
///
/// For a caller with one budget for a whole exchange: an HTTP call's
/// request, headers and body. Reads still answer to their own limit as well,
/// whichever comes first. A read or write that runs out returns -1, which is
/// what a caller already treats as a connection to give up on.
#[unsafe(no_mangle)]
pub extern "C" fn khora_net_set_deadline(socket: Socket, at: i64) -> i32 {
    let by = (at >= 0).then(|| crate::time::instant_of(at));
    change_limits(socket, |limits| {
        limits.by = by;
        limits.expired = false;
    });
    0
}

/// `socket`'s deadline and whether it has expired, as they stand.
///
/// For a caller that holds a socket it does not own to a deadline of its own
/// for a while: [`put_deadline`] gives the owner back what it had. A TLS
/// upgrade over a caller's socket is the one.
pub(crate) fn deadline_of(socket: Socket) -> (Option<std::time::Instant>, bool) {
    let limits = limits_of(socket);
    (limits.by, limits.expired)
}

/// Puts back what [`deadline_of`] read.
pub(crate) fn put_deadline(socket: Socket, by: Option<std::time::Instant>, expired: bool) {
    change_limits(socket, |limits| {
        limits.by = by;
        limits.expired = expired;
    });
}

/// Why [`connect_by`] has no socket to give.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gave {
    /// The name did not resolve, or no address accepted.
    Up,
    /// The calling fiber was asked to stop.
    Stopped,
    /// The deadline passed.
    TimedOut,
}

/// [`khora_net_connect`], for Rust callers: the TLS client dials through this.
pub(crate) fn connect_by(
    name: &str,
    port: u16,
    deadline: Option<std::time::Instant>,
) -> Result<Socket, Gave> {
    let addresses = resolve(name, port, deadline)?;
    for address in &addresses {
        match connect_one(address, deadline) {
            // **A connection made for a fiber that was canceled meanwhile is
            // closed here.** Its caller will stop at its next cancellation
            // point holding a plain handle nothing has registered, and the
            // socket leaks: a loopback connect completes without waiting, so
            // no wait was there to see the cancel.
            Ok(socket) if crate::current::current(|f| f.gives_up_waiting()) => {
                raw_close(socket);
                return Err(Gave::Stopped);
            }
            Ok(socket) => return Ok(socket),
            // The next address, as `TcpStream::connect` would.
            Err(Gave::Up) => continue,
            Err(stopped @ (Gave::Stopped | Gave::TimedOut)) => return Err(stopped),
        }
    }
    Err(Gave::Up)
}

/// The addresses `name` stands for, in the resolver's order.
fn resolve(
    name: &str,
    port: u16,
    deadline: Option<std::time::Instant>,
) -> Result<Vec<std::net::SocketAddr>, Gave> {
    // An address needs no lookup, and is the common case for a service
    // talking to a sidecar or a database by IP.
    if let Ok(ip) = name.parse::<std::net::IpAddr>() {
        return Ok(vec![std::net::SocketAddr::new(ip, port)]);
    }
    let owned = name.to_string();
    let looked_up = crate::blocking::abandonable_on(
        crate::blocking::lookups(),
        move || {
            use std::net::ToSocketAddrs;
            (owned.as_str(), port).to_socket_addrs().map(|found| found.collect::<Vec<_>>())
        },
        deadline,
    );
    match looked_up {
        Ok(Ok(found)) => Ok(found),
        Ok(Err(_)) => Err(Gave::Up),
        Err(crate::blocking::Abandoned::Stopped) => Err(Gave::Stopped),
        Err(crate::blocking::Abandoned::TimedOut) => Err(Gave::TimedOut),
    }
}

/// One address: a non-blocking socket, `connect`, and a wait for the answer.
///
/// The socket is closed on every path that does not return it.
fn connect_one(address: &std::net::SocketAddr, deadline: Option<std::time::Instant>) -> Result<Socket, Gave> {
    let Some(socket) = raw_socket(address) else { return Err(Gave::Up) };
    match connecting(socket, address, deadline) {
        Ok(()) => {
            // Nagle off: a wire protocol writes a small message and waits for
            // the answer, which is the exact shape Nagle delays for no gain.
            no_delay(socket);
            Ok(socket)
        }
        Err(why) => {
            raw_close(socket);
            Err(why)
        }
    }
}

/// Drives `connect` on a prepared socket to an answer.
fn connecting(socket: Socket, address: &std::net::SocketAddr, deadline: Option<std::time::Instant>) -> Result<(), Gave> {
    if khora_net_prepare(socket) != 0 {
        return Err(Gave::Up);
    }
    match start_connect(socket, address) {
        Started::Connected => return Ok(()),
        Started::Failed => return Err(Gave::Up),
        Started::Pending => {}
    }
    loop {
        let woke = wait(socket, Interest::Writable, slice_until(deadline));
        // Writable is the hint; `SO_ERROR` is the answer. It is asked however
        // the wait ended, so an answer that arrived with the deadline or a
        // slice's end is the answer, and a refusal is never a timeout.
        match (connect_state(socket), woke) {
            (Started::Connected, _) => return Ok(()),
            (Started::Failed, _) => return Err(Gave::Up),
            (Started::Pending, Woke::Stopped) => return Err(Gave::Stopped),
            (Started::Pending, Woke::Ready | Woke::TimedOut) => {
                if deadline.is_some_and(|at| std::time::Instant::now() >= at) {
                    return Err(Gave::TimedOut);
                }
            }
        }
    }
}

/// How long one wait in [`connecting`] may last.
///
/// **On Windows, a tenth of a second at most, because `WSAPoll` may never
/// report a failed connect.** Before Windows 10 2004 (Server 2019 and
/// earlier) a refused non-blocking connect raises no `POLLERR`, no `POLLHUP`
/// and no `POLLWRNORM`: a wait for it ends only at the deadline, and with none
/// -- `connect_to` -- never. So the wait is cut into slices and `SO_ERROR` is
/// read after each, which is right on every version. What it costs: a pending
/// connect wakes ten times a second. `select` reports the failure, and is not
/// used because the reactor is built on `WSAPoll`.
///
/// Elsewhere the reactor reports a failed connect, and the wait is the
/// caller's deadline.
fn slice_until(deadline: Option<std::time::Instant>) -> Option<std::time::Instant> {
    #[cfg(windows)]
    {
        let slice = std::time::Instant::now() + std::time::Duration::from_millis(100);
        Some(deadline.map_or(slice, |at| at.min(slice)))
    }
    #[cfg(not(windows))]
    {
        deadline
    }
}

/// Where a non-blocking connect stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Started {
    Connected,
    Pending,
    Failed,
}

#[cfg(not(windows))]
fn raw_socket(address: &std::net::SocketAddr) -> Option<Socket> {
    let family = match address {
        std::net::SocketAddr::V4(_) => libc::AF_INET,
        std::net::SocketAddr::V6(_) => libc::AF_INET6,
    };
    #[cfg(not(target_vendor = "apple"))]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(target_vendor = "apple")]
    let kind = libc::SOCK_STREAM;
    // SAFETY: `socket` takes three integers and touches no memory.
    let socket = unsafe { libc::socket(family, kind, 0) };
    if socket < 0 {
        return None;
    }
    // Apple has no `SOCK_CLOEXEC`. Without the flag a `process.run` child
    // inherits the descriptor and holds the connection open after this side
    // closes it.
    #[cfg(target_vendor = "apple")]
    // SAFETY: an ordinary `fcntl` on the descriptor just opened.
    unsafe {
        libc::fcntl(socket, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    Some(socket)
}

#[cfg(windows)]
fn raw_socket(address: &std::net::SocketAddr) -> Option<Socket> {
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, INVALID_SOCKET, IPPROTO_TCP, SOCK_STREAM, WSASocketW,
        WSA_FLAG_NO_HANDLE_INHERIT, WSA_FLAG_OVERLAPPED,
    };
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::Networking::WinSock::{WSAGetLastError, WSAEINVAL};
    if !winsock_started() {
        return None;
    }
    let family = match address {
        std::net::SocketAddr::V4(_) => AF_INET,
        std::net::SocketAddr::V6(_) => AF_INET6,
    };
    let open = |flags| {
        // SAFETY: no protocol-info pointer, and integers otherwise.
        unsafe { WSASocketW(i32::from(family), SOCK_STREAM, IPPROTO_TCP, std::ptr::null(), 0, flags) }
    };
    // The flags `std::net` passes: overlapped so the handle could ever be
    // used with IOCP, and not inherited by a child process.
    let socket = open(WSA_FLAG_OVERLAPPED | WSA_FLAG_NO_HANDLE_INHERIT);
    if socket != INVALID_SOCKET {
        return Some(socket);
    }
    // **Windows 7 without SP1 refuses the no-inherit flag** with
    // `WSAEINVAL`, and without this every connect there failed as
    // unreachable. The fallback is `std::net`'s: open without it, then clear
    // inheritance on the handle, which leaves a window in which a child
    // started by another thread inherits the socket.
    // SAFETY: no arguments; it reads this thread's last winsock error.
    if unsafe { WSAGetLastError() } != WSAEINVAL {
        return None;
    }
    let socket = open(WSA_FLAG_OVERLAPPED);
    if socket == INVALID_SOCKET {
        return None;
    }
    // SAFETY: `socket` is a live handle just opened, and a socket is a
    // kernel handle `SetHandleInformation` accepts.
    if unsafe { SetHandleInformation(socket as HANDLE, HANDLE_FLAG_INHERIT, 0) } == 0 {
        raw_close(socket);
        return None;
    }
    Some(socket)
}

/// Winsock started, once per process, whoever else did or did not.
///
/// **What this prevents: a connect that fails only on Windows.** Every socket
/// call there fails with `WSANOTINITIALISED` until `WSAStartup` has run.
/// `std::net::socket::start` does it, and `TcpStream::connect` does it
/// lazily for a program that forgot to. The socket here is opened directly,
/// so it does the same. `WSAStartup` counts, so a second start is harmless.
///
/// Answers whether Winsock is usable. A refusal is remembered rather than
/// retried: it means the system has no usable Winsock 2.2, which a second
/// call does not change, and the connect then fails as unreachable.
#[cfg(windows)]
fn winsock_started() -> bool {
    use windows_sys::Win32::Networking::WinSock::{WSAStartup, WSADATA};
    static STARTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STARTED.get_or_init(|| {
        // SAFETY: `WSADATA` is plain data that the call fills in.
        let mut data: WSADATA = unsafe { std::mem::zeroed() };
        // SAFETY: `data` is a live `WSADATA`; 0x0202 asks for version 2.2.
        unsafe { WSAStartup(0x0202, &raw mut data) == 0 }
    })
}

#[cfg(not(windows))]
fn raw_close(socket: Socket) {
    // SAFETY: `socket` was opened by `raw_socket` and nothing else holds it.
    unsafe { libc::close(socket) };
}

#[cfg(windows)]
fn raw_close(socket: Socket) {
    use windows_sys::Win32::Networking::WinSock::closesocket;
    // SAFETY: `socket` was opened by `raw_socket` and nothing else holds it.
    unsafe { closesocket(socket) };
}

/// Starts a connect and says how it went.
///
/// **`#[inline(never)]` for the reason [`would_block`] is**: the error is
/// read from this thread's `errno` with no suspension between the call and
/// the read.
#[cfg(not(windows))]
#[inline(never)]
fn start_connect(socket: Socket, address: &std::net::SocketAddr) -> Started {
    let (storage, length) = sockaddr_of(address);
    // SAFETY: `storage` holds a `sockaddr` of `length` bytes for the address's
    // family, live for the call.
    let done = unsafe { libc::connect(socket, (&raw const storage).cast(), length) };
    if done == 0 {
        return Started::Connected;
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    // `EINTR` leaves the connect going on in the background, as
    // `EINPROGRESS` does; POSIX says so, and a signal must not lose it.
    if errno == libc::EINPROGRESS || errno == libc::EINTR {
        Started::Pending
    } else {
        Started::Failed
    }
}

#[cfg(windows)]
#[inline(never)]
fn start_connect(socket: Socket, address: &std::net::SocketAddr) -> Started {
    use windows_sys::Win32::Networking::WinSock::{connect, WSAGetLastError, WSAEWOULDBLOCK};
    let (storage, length) = sockaddr_of(address);
    // SAFETY: `storage` holds a `SOCKADDR` of `length` bytes for the address's
    // family, live for the call.
    let done = unsafe { connect(socket, (&raw const storage).cast(), length) };
    if done == 0 {
        return Started::Connected;
    }
    // SAFETY: no arguments; it reads this thread's last winsock error.
    if unsafe { WSAGetLastError() } == WSAEWOULDBLOCK {
        Started::Pending
    } else {
        Started::Failed
    }
}

/// Where a pending connect stands now: the socket's own error if it has
/// one, then whether it has a peer.
///
/// `#[inline(never)]` for the reason [`start_connect`] is.
#[cfg(not(windows))]
#[inline(never)]
fn connect_state(socket: Socket) -> Started {
    let mut error: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `error` is a live `c_int` and `length` its size.
    let read = unsafe {
        libc::getsockopt(socket, libc::SOL_SOCKET, libc::SO_ERROR, (&raw mut error).cast(), &raw mut length)
    };
    if read != 0 || error != 0 {
        return Started::Failed;
    }
    // SAFETY: `peer` is zeroed storage large enough for any address, and
    // `length` says so.
    let mut peer: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: as above.
    if unsafe { libc::getpeername(socket, (&raw mut peer).cast(), &raw mut length) } == 0 {
        return Started::Connected;
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    if errno == libc::ENOTCONN { Started::Pending } else { Started::Failed }
}

#[cfg(windows)]
#[inline(never)]
fn connect_state(socket: Socket) -> Started {
    use windows_sys::Win32::Networking::WinSock::{
        getsockopt, WSAPoll, POLLERR, POLLHUP, POLLWRNORM, SOL_SOCKET, SO_ERROR, WSAPOLLFD,
    };
    let mut error: i32 = 0;
    let mut length = std::mem::size_of::<i32>() as i32;
    // SAFETY: `error` is a live `i32` and `length` its size.
    let read = unsafe { getsockopt(socket, SOL_SOCKET, SO_ERROR, (&raw mut error).cast(), &raw mut length) };
    if read != 0 || error != 0 {
        return Started::Failed;
    }
    // **Not `getpeername`, as elsewhere: on Windows it answers for a connect
    // still in progress**, because the peer's address is recorded when the
    // connect starts. A refused connect is retried for about two seconds
    // before `SO_ERROR` says so, and `getpeername` called it connected. A
    // socket is connected once it is writable, so ask that, without waiting.
    let mut fd = WSAPOLLFD { fd: socket, events: POLLWRNORM, revents: 0 };
    // SAFETY: `fd` is one live entry, and a zero timeout returns at once.
    let ready = unsafe { WSAPoll(&raw mut fd, 1, 0) };
    if ready < 0 || fd.revents & (POLLERR | POLLHUP) != 0 {
        Started::Failed
    } else if fd.revents & POLLWRNORM != 0 {
        Started::Connected
    } else {
        Started::Pending
    }
}

/// An address as the bytes `connect` takes, and how many of them count.
#[cfg(not(windows))]
fn sockaddr_of(address: &std::net::SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: `sockaddr_storage` is plain data; zero is a valid value of it,
    // and every field the call reads is set below.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let length = match address {
        std::net::SocketAddr::V4(v4) => {
            // SAFETY: `sockaddr_storage` is larger than and aligned for any
            // `sockaddr_*`, which is what it exists for.
            let into = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_in>() };
            into.sin_family = libc::AF_INET as libc::sa_family_t;
            into.sin_port = v4.port().to_be();
            into.sin_addr = libc::in_addr { s_addr: u32::from_ne_bytes(v4.ip().octets()) };
            #[cfg(target_vendor = "apple")]
            {
                into.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
            }
            std::mem::size_of::<libc::sockaddr_in>()
        }
        std::net::SocketAddr::V6(v6) => {
            // SAFETY: as above.
            let into = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_in6>() };
            into.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            into.sin6_port = v6.port().to_be();
            into.sin6_flowinfo = v6.flowinfo();
            into.sin6_addr = libc::in6_addr { s6_addr: v6.ip().octets() };
            into.sin6_scope_id = v6.scope_id();
            #[cfg(target_vendor = "apple")]
            {
                into.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
            }
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, length as libc::socklen_t)
}

#[cfg(windows)]
fn sockaddr_of(
    address: &std::net::SocketAddr,
) -> (windows_sys::Win32::Networking::WinSock::SOCKADDR_STORAGE, i32) {
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, IN6_ADDR, IN6_ADDR_0, IN_ADDR, IN_ADDR_0, SOCKADDR_IN, SOCKADDR_IN6,
        SOCKADDR_IN6_0, SOCKADDR_STORAGE,
    };
    // SAFETY: plain data; zero is a valid value, and the part the call reads
    // is written below.
    let mut storage: SOCKADDR_STORAGE = unsafe { std::mem::zeroed() };
    let length = match address {
        std::net::SocketAddr::V4(v4) => {
            let value = SOCKADDR_IN {
                sin_family: AF_INET,
                sin_port: v4.port().to_be(),
                sin_addr: IN_ADDR { S_un: IN_ADDR_0 { S_addr: u32::from_ne_bytes(v4.ip().octets()) } },
                sin_zero: [0; 8],
            };
            // SAFETY: `SOCKADDR_STORAGE` is larger than and aligned for any
            // `SOCKADDR_*`, which is what it exists for.
            unsafe { (&raw mut storage).cast::<SOCKADDR_IN>().write(value) };
            std::mem::size_of::<SOCKADDR_IN>()
        }
        std::net::SocketAddr::V6(v6) => {
            let value = SOCKADDR_IN6 {
                sin6_family: AF_INET6,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                sin6_addr: IN6_ADDR { u: IN6_ADDR_0 { Byte: v6.ip().octets() } },
                Anonymous: SOCKADDR_IN6_0 { sin6_scope_id: v6.scope_id() },
            };
            // SAFETY: as above.
            unsafe { (&raw mut storage).cast::<SOCKADDR_IN6>().write(value) };
            std::mem::size_of::<SOCKADDR_IN6>()
        }
    };
    (storage, length as i32)
}

/// Turns Nagle off. Best effort: a socket that refuses still works.
#[cfg(not(windows))]
fn no_delay(socket: Socket) {
    let on: libc::c_int = 1;
    // SAFETY: `on` is a live `c_int` and the length is its size.
    unsafe {
        libc::setsockopt(
            socket,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            (&raw const on).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
}

#[cfg(windows)]
fn no_delay(socket: Socket) {
    use windows_sys::Win32::Networking::WinSock::{setsockopt, IPPROTO_TCP, TCP_NODELAY};
    let on: i32 = 1;
    // SAFETY: `on` is a live `i32` and the length is its size.
    unsafe { setsockopt(socket, IPPROTO_TCP, TCP_NODELAY, (&raw const on).cast(), 4) };
}

#[unsafe(no_mangle)]
/// How long a receive on `socket` may wait before it reports a timeout.
///
/// Replaces `setsockopt(SO_RCVTIMEO)`, which cannot fire on a socket the
/// reactor drives. Zero clears it.
pub extern "C" fn khora_net_set_timeout(socket: Socket, millis: i64) -> i32 {
    change_limits(socket, |limits| limits.per_read = (millis > 0).then_some(millis as u64));
    0
}

/// Forgets everything the runtime remembers about `socket`.
///
/// Called when a socket is closed. Not optional: handles are reused, and the
/// next connection to be handed this number would inherit the deadline.
#[unsafe(no_mangle)]
pub extern "C" fn khora_net_forget(socket: Socket) {
    if let Some(table) = LIMITS.lock().expect("the socket limits").as_mut() {
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
/// So this is one syscall, and it never waits. A read that would have blocked
/// is -1, and a read that failed for any other reason is -2.
///
/// **The two negatives are different answers because a caller checking a
/// connection needs both.** A pool asking whether an idle connection is still
/// open reads what has arrived: nothing yet (-1) is the healthy answer, and a
/// connection the peer reset (`ECONNRESET`, -2) is not. With both folded into
/// -1, a reset connection looked healthy, the next statement was written to
/// it, and the process died of `SIGPIPE`. A caller that only wants to stop
/// (`shut`'s drain) can go on treating any negative as "stop".
///
/// # Safety
///
/// `into` must address `length` writable bytes.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_net_recv_now(
    socket: Socket,
    into: *mut u8,
    length: isize,
) -> isize {
    // SAFETY: the caller guarantees `length` writable bytes at `into`.
    let read = unsafe { raw_recv(socket, into, length) };
    if read >= 0 {
        read
    } else if would_block() {
        -1
    } else {
        -2
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
// SHARE: writes bytes into a buffer; takes no Khora object.
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
        let woke = wait(socket, Interest::Readable, deadline);
        if woke == Woke::Ready {
            continue;
        }
        // One more look, so bytes or a close that came with the deadline are
        // the answer rather than the deadline.
        // SAFETY: as above.
        let read = unsafe { raw_recv(socket, into, length) };
        if read >= 0 || !would_block() {
            return read;
        }
        if woke == Woke::TimedOut {
            note_if_expired(socket);
        }
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
/// returns `length`, or -1 if a `send` failed or the fiber was canceled
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
        // A write that cannot proceed is back-pressure from the peer. The
        // per-read timeout is about a peer that stopped talking and does not
        // apply; a deadline over the whole connection does, because a caller
        // that set one meant every operation on it.
        //
        // Canceled stops here too, for the reason `accept` does below.
        match wait(socket, Interest::Writable, send_deadline_for(socket)) {
            Woke::Ready => {}
            Woke::Stopped => return -1,
            Woke::TimedOut => {
                note_if_expired(socket);
                return -1;
            }
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
// SHARE: writes bytes into a buffer; takes no Khora object.
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
        // fiber was canceled, and retrying then parks again on a socket that
        // will never be ready — which is the shape that hung a canceled
        // listener for ever. A negative return is what every other failure
        // here gives back; `std::net` reads the sign.
        match wait(socket, Interest::Readable, None) {
            Woke::Ready => {}
            Woke::Stopped | Woke::TimedOut => return -1,
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

/// The flags every `send` in the runtime passes.
///
/// **What this prevents: one client that resets its connection killing the
/// whole process.** After a peer's RST (a killed browser tab, a load
/// balancer, `kill -9`), a `send` on that socket makes the kernel raise
/// `SIGPIPE`, whose default action ends the process, and a Khora binary keeps
/// the default. With `MSG_NOSIGNAL` the same `send` fails with `EPIPE` or
/// `ECONNRESET` instead, and the caller gets the -1 it gets for any other
/// failed send.
///
/// Per call, not per process: a CLI whose stdout goes to `head` still dies of
/// `SIGPIPE` on the pipe, as Unix programs do. Apple's platforms have no
/// `MSG_NOSIGNAL`; [`no_sigpipe`] covers them per socket instead.
#[cfg(all(unix, not(target_vendor = "apple")))]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
#[cfg(target_vendor = "apple")]
const SEND_FLAGS: libc::c_int = 0;

/// Sets `SO_NOSIGPIPE`, so a `send` to a peer that reset the connection fails
/// instead of killing the process.
///
/// Apple's half of [`SEND_FLAGS`]: macOS has no `MSG_NOSIGNAL`, only this
/// per-socket option. It is set in [`khora_net_prepare`] because every socket
/// the runtime sends on passes through it: listening, accepted, connected,
/// and the TLS client's. A socket that never went through it keeps the
/// signal, which is the same boundary as non-blocking mode.
///
/// Returns 0, or -1 with the error in `errno`.
#[cfg(target_vendor = "apple")]
fn no_sigpipe(socket: Socket) -> i32 {
    let on: libc::c_int = 1;
    // SAFETY: `on` is a live `c_int` and the length is its size; `socket` is a
    // descriptor the caller owns.
    unsafe {
        libc::setsockopt(
            socket,
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&raw const on).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    }
}

#[cfg(not(windows))]
unsafe fn raw_send(socket: Socket, from: *const u8, length: isize) -> isize {
    // SAFETY: the caller's guarantee.
    unsafe { libc::send(socket, from.cast(), length as usize, SEND_FLAGS) }
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
        // Handles are reused across the tests in this process, and a limit
        // another test set on this number would make the read time out
        // rather than see the end. This test is about the end.
        khora_net_forget(socket);
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

    /// Set in the copy of the test binary that
    /// [`a_send_to_a_reset_peer_fails_and_the_process_lives`] starts.
    #[cfg(unix)]
    const RESET_PEER_CHILD: &str = "KHORA_RT_RESET_PEER_CHILD";

    /// **A send to a peer that reset the connection is a failed send, not a
    /// dead process.**
    ///
    /// Once the peer has sent an RST (a killed browser tab, a load balancer,
    /// `kill -9`), the first `send` fails with `ECONNRESET` and the next makes
    /// the kernel raise `SIGPIPE`, whose default action ends the process. A
    /// Khora binary keeps that default, so one client that vanished took the
    /// whole server with it, on both fiber backends.
    ///
    /// **The send runs in a fresh copy of this test binary.** Rust's own
    /// start-up ignores `SIGPIPE`, so in-process this test would pass with or
    /// without the fix. The copy puts the default back, the way a Khora binary
    /// has it, and the parent checks it was not killed by a signal. Only the
    /// copy changes the disposition, because it is process-wide and every
    /// other test in this binary shares it.
    #[cfg(unix)]
    #[test]
    fn a_send_to_a_reset_peer_fails_and_the_process_lives() {
        use std::os::unix::process::ExitStatusExt;
        if std::env::var_os(RESET_PEER_CHILD).is_some() {
            sends_to_reset_peers();
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args(["--exact", "net::tests::a_send_to_a_reset_peer_fails_and_the_process_lives"])
            .args(["--nocapture", "--test-threads=1"])
            .env(RESET_PEER_CHILD, "1")
            .status()
            .expect("the copy of the test binary should start");
        assert_eq!(status.signal(), None, "the process was killed by signal {:?} (13 is SIGPIPE)", status.signal());
        assert!(status.success(), "the copy failed: {status:?}");
    }

    /// The half of the test above that runs in the copy, with `SIGPIPE` back
    /// at its default: a reset peer, once off a worker and once on one.
    #[cfg(unix)]
    fn sends_to_reset_peers() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;
        use std::sync::{Arc, Mutex};

        // SAFETY: `signal` with a valid signal number and `SIG_DFL`. The child
        // runs this one test on one thread, so no other test's writes depend
        // on the disposition Rust's start-up chose.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

        /// A connected, prepared socket whose peer has already reset it.
        fn reset_by_its_peer() -> (std::net::TcpStream, Socket) {
            let (client, peer) = a_connected_pair();
            let socket = socket_of(&client);
            assert_eq!(khora_net_prepare(socket), 0);
            let linger = libc::linger { l_onoff: 1, l_linger: 0 };
            // SAFETY: `linger` is a live `struct linger` and the length is its
            // size; `peer` owns the descriptor for the whole call.
            let set = unsafe {
                libc::setsockopt(
                    socket_of(&peer),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    (&raw const linger).cast(),
                    std::mem::size_of::<libc::linger>() as libc::socklen_t,
                )
            };
            assert_eq!(set, 0, "SO_LINGER 0 is what makes the close an RST");
            drop(peer);
            // The RST has to arrive before the sends for them to meet it.
            std::thread::sleep(std::time::Duration::from_millis(50));
            (client, socket)
        }

        /// Sends until one fails; the first failure's `errno` with it. Several
        /// sends, because only the second after an RST raises the signal.
        fn send_until_it_fails(socket: Socket) -> Option<i32> {
            for _ in 0..8 {
                // SAFETY: five readable bytes.
                if unsafe { khora_net_send(socket, b"hello".as_ptr(), 5) } < 0 {
                    let errno = std::io::Error::last_os_error().raw_os_error();
                    // Past the first failure, to the one that signals.
                    // SAFETY: as above.
                    let again = unsafe { khora_net_send(socket, b"hello".as_ptr(), 5) };
                    assert_eq!(again, -1, "a send after a failed one should fail too");
                    return errno;
                }
            }
            None
        }

        // Off a worker, where `errno` is still this thread's to read.
        let (_client, socket) = reset_by_its_peer();
        let errno = send_until_it_fails(socket).expect("off a worker: a send to a reset peer succeeded");
        assert!(
            errno == libc::EPIPE || errno == libc::ECONNRESET,
            "off a worker: the failure should be EPIPE or ECONNRESET, not errno {errno}"
        );

        // On a worker.
        let failed = Arc::new(Mutex::new(None));
        let seen = failed.clone();
        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            let (_client, socket) = reset_by_its_peer();
            *seen.lock().expect("the outcome") = Some(send_until_it_fails(socket).is_some());
        }));
        pool.drain();
        assert_eq!(
            *failed.lock().expect("the outcome"),
            Some(true),
            "on a worker: a send to a reset peer succeeded"
        );
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
    /// reader, the optimized macOS build looked it up once and reported
    /// every forced read as staying put. So `worker` is out of line, for the
    /// same reason as `would_block`. The mistake fails the test, never
    /// passes it: a stale read gives the same worker before and after.
    ///
    /// What it guards: only the development profile. The optimized runtime
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
        /// closure in the reader's body the optimized aarch64-macOS build
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

    /// Runs `body` on a thread of its own and fails, naming `what`, if it has
    /// not finished within ten seconds.
    ///
    /// **What it prevents: a test that hangs instead of failing.** Every test
    /// below is about a wait that must end; with the fix broken, the wait is
    /// the kernel's two minutes or for ever. The thread is left behind when
    /// it overruns, which costs nothing once the test has failed.
    fn within_ten_seconds<T: Send + 'static>(what: &str, body: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(body());
        });
        finished
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("{what} was still waiting after ten seconds"))
    }

    /// A listener whose next connect hangs: its accept queue is full, so the
    /// kernel drops the next SYN. See `tests/connect_cancel.rs` in the LLVM
    /// crate, which uses the same trick against a Khora program. Not on
    /// Windows, which refuses a connect to a full queue instead.
    #[cfg(not(windows))]
    fn never_answering() -> (std::net::TcpListener, Vec<std::net::TcpStream>, std::net::SocketAddr) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = listener.local_addr().expect("an address");
        let mut filled = Vec::new();
        for _ in 0..1000 {
            match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(300)) {
                Ok(stream) => filled.push(stream),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return (listener, filled, address),
                Err(e) => panic!("a full backlog answered {e}, so nothing here can hang"),
            }
        }
        panic!("the backlog never filled");
    }

    /// **A connect to a listening port succeeds and hands back a socket the
    /// peer sees**; one to a closed port fails at once. The plain cases of
    /// [`connect_by`], off a scheduler and on one.
    #[test]
    fn a_connect_reaches_a_listener_and_a_closed_port_is_refused() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;
        use std::sync::{Arc, Mutex};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let closed = {
            let gone = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
            gone.local_addr().expect("an address").port()
        };

        let off = connect_by("127.0.0.1", port, None).expect("a connect off a scheduler");
        let (_peer, _) = listener.accept().expect("the peer sees it");
        raw_close(off);
        // A freed port can be taken again by another test in this process
        // before the connect reaches it, so a connect that lands is retried
        // on a fresh freed port; a real refusal is what has to come back.
        let refused_somewhere = move || {
            let mut answer = connect_by("127.0.0.1", closed, None);
            for _ in 0..5 {
                let Ok(socket) = answer else { break };
                raw_close(socket);
                let fresh = {
                    let gone = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
                    gone.local_addr().expect("an address").port()
                };
                answer = connect_by("127.0.0.1", fresh, None);
            }
            answer.map(raw_close)
        };
        assert_eq!(refused_somewhere(), Err(Gave::Up));
        assert_eq!(connect_by("localhost", port, None).map(raw_close), Ok(()), "by name");
        let _ = listener.accept();

        let seen = Arc::new(Mutex::new(None));
        let said = seen.clone();
        let pool = Scheduler::new(2);
        pool.spawn(Task::new(move || {
            let on = connect_by("127.0.0.1", port, None);
            let refused = refused_somewhere();
            *said.lock().expect("the outcome") = Some((on.map(raw_close), refused));
        }));
        let (_peer, _) = listener.accept().expect("the peer sees the fiber's connect");
        pool.drain();
        assert_eq!(*seen.lock().expect("the outcome"), Some((Ok(()), Err(Gave::Up))));
    }

    /// **A deadline ends a connect nobody answers**, on a worker and off one,
    /// and well before the kernel would.
    #[cfg(not(windows))]
    #[test]
    fn a_connect_nobody_answers_gives_up_at_its_deadline() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        let (_listener, _filled, address) = never_answering();
        let port = address.port();

        let off = within_ten_seconds("a connect off a scheduler with a 100 ms deadline", move || {
            connect_by("127.0.0.1", port, Some(Instant::now() + Duration::from_millis(100)))
        });
        assert_eq!(off, Err(Gave::TimedOut), "off a scheduler");

        let seen = Arc::new(Mutex::new(None));
        let said = seen.clone();
        within_ten_seconds("a connect on a worker with a 100 ms deadline", move || {
            let pool = Scheduler::new(2);
            pool.spawn(Task::new(move || {
                let began = Instant::now();
                let on = connect_by("127.0.0.1", port, Some(Instant::now() + Duration::from_millis(100)));
                *said.lock().expect("the outcome") = Some((on, began.elapsed()));
            }));
            pool.drain();
        });
        let (on, took) = seen.lock().expect("the outcome").expect("it ran");
        assert_eq!(on, Err(Gave::TimedOut), "on a worker");
        assert!(took < Duration::from_secs(5), "on a worker it took {took:?}");
    }

    /// **A cancel ends a connect nobody answers, off a scheduler**: the
    /// thread backend's path, where the thread waits in slices and looks for
    /// the cancel between them.
    #[cfg(not(windows))]
    #[test]
    fn a_canceled_connect_off_a_scheduler_gives_up() {
        use crate::current::{enter, Fiber};
        use std::time::{Duration, Instant};

        let (_listener, _filled, address) = never_answering();
        let port = address.port();
        let fiber = Fiber::spawned();
        let dialer = {
            let fiber = std::sync::Arc::clone(&fiber);
            std::thread::spawn(move || {
                let _in = enter(fiber);
                connect_by("127.0.0.1", port, None)
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        let canceled = Instant::now();
        fiber.cancel();
        let answer = within_ten_seconds("a canceled connect", move || dialer.join().expect("the dialer"));
        let took = canceled.elapsed();
        assert_eq!(answer, Err(Gave::Stopped));
        assert!(took < Duration::from_secs(2), "the cancel took {took:?} to land");
    }

    /// **A lookup that never comes back is abandoned on a cancel and on a
    /// deadline**, and its value goes nowhere. The work here stands in for a
    /// resolver that hangs: it blocks until the test lets it go.
    #[test]
    fn an_abandonable_call_gives_up_on_a_cancel_and_on_a_deadline() {
        use crate::blocking::{abandonable_on, pool, Abandoned};
        let abandonable = |work, by| abandonable_on(pool(), work, by);
        use crate::current::{enter, Fiber};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let release = Arc::new(AtomicBool::new(false));
        let hang = |release: Arc<AtomicBool>| {
            move || {
                while !release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                7
            }
        };

        let began = Instant::now();
        let work = hang(release.clone());
        let timed = within_ten_seconds("an abandonable call with a 50 ms deadline", move || {
            abandonable(work, Some(Instant::now() + Duration::from_millis(50)))
        });
        assert_eq!(timed, Err(Abandoned::TimedOut));
        assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());

        let fiber = Fiber::spawned();
        let waiter = {
            let fiber = Arc::clone(&fiber);
            let work = hang(release.clone());
            std::thread::spawn(move || {
                let _in = enter(fiber);
                abandonable(work, None)
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        let canceled = Instant::now();
        fiber.cancel();
        let answer = within_ten_seconds("a canceled abandonable call", move || waiter.join().expect("the waiter"));
        assert_eq!(answer, Err(Abandoned::Stopped));
        assert!(canceled.elapsed() < Duration::from_secs(2), "{:?}", canceled.elapsed());

        // And one that is allowed to finish gives its value.
        release.store(true, Ordering::SeqCst);
        assert_eq!(abandonable_on(pool(), || 9, None), Ok(9));
    }

    /// **A deadline set on a socket ends a read that would otherwise wait, and
    /// a write to a peer that never reads.** The receive limit alone renews
    /// per read; this one is fixed.
    #[test]
    fn a_socket_deadline_ends_reads_and_writes() {
        let (client, _peer) = a_connected_pair();
        let socket = socket_of(&client);
        assert_eq!(khora_net_prepare(socket), 0);
        let at = crate::time::khora_monotonic_millis() + 100;
        khora_net_set_deadline(socket, at);

        let read = within_ten_seconds("a read past the socket's deadline", move || {
            let mut buffer = [0u8; 4];
            // SAFETY: four writable bytes.
            unsafe { khora_net_recv(socket, buffer.as_mut_ptr(), 4) }
        });
        assert_eq!(read, -1);
        // A write big enough that no send buffer takes it whole. Windows
        // accepts tens of megabytes on loopback before a send waits, so one
        // 64 MiB write can succeed outright there; keep writing until a write
        // gives up, and bound the total so a deadline that never lands fails
        // rather than filling memory.
        let sent = within_ten_seconds("a write past the socket's deadline", move || {
            let bytes = vec![0u8; 8 << 20];
            let mut total: usize = 0;
            loop {
                // SAFETY: `bytes.len()` readable bytes.
                let wrote = unsafe { khora_net_send(socket, bytes.as_ptr(), bytes.len() as isize) };
                if wrote < 0 || total > 2 << 30 {
                    return wrote;
                }
                total += wrote as usize;
            }
        });
        assert_eq!(sent, -1);

        khora_net_forget(socket);
        assert!(deadline_for(socket).is_none() && send_deadline_for(socket).is_none());
    }

    /// **A refused connect is a refusal however late it arrives.** The cause
    /// used to be read off the clock after the wait: a refusal that came back
    /// at or past the deadline was reported as the deadline, about 2% of
    /// refusals under a 1 ms budget. A deadline that has already passed when
    /// the refusal is in makes that every time, so this asks two hundred
    /// times and wants a refusal from each.
    ///
    /// Only on Linux, where a refused loopback connect fails inside `connect`
    /// itself. Windows retries a refusal for about two seconds, and macOS
    /// reports it a moment after `connect` returns, so with a deadline
    /// already passed the connect really is still pending and `TimedOut` is
    /// right. There a refusal well inside its deadline must still be a
    /// refusal.
    #[test]
    fn a_refused_connect_is_not_reported_as_the_deadline() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
            listener.local_addr().expect("an address").port()
        };
        if !cfg!(target_os = "linux") {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            assert_eq!(connect_by("127.0.0.1", port, Some(deadline)).map(raw_close), Err(Gave::Up));
            return;
        }
        let mut timed_out = 0;
        for _ in 0..200 {
            match connect_by("127.0.0.1", port, Some(std::time::Instant::now())) {
                Err(Gave::Up) => {}
                Err(Gave::TimedOut) => timed_out += 1,
                Ok(socket) => {
                    raw_close(socket);
                    panic!("a closed port accepted");
                }
                Err(Gave::Stopped) => panic!("nothing canceled this"),
            }
        }
        assert_eq!(timed_out, 0, "{timed_out} of 200 refusals were reported as the deadline");
    }

    /// **A connection made for a fiber that was canceled meanwhile is closed,
    /// not handed back.** Its caller stops at its next cancellation point
    /// holding a plain handle nothing has registered, which is the socket a
    /// canceled HTTP call leaked.
    #[test]
    fn a_connect_that_succeeds_for_a_canceled_fiber_closes_what_it_opened() {
        use crate::current::{enter, Fiber};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let fiber = Fiber::spawned();
        fiber.cancel();
        let answer = {
            let _in = enter(fiber);
            connect_by("127.0.0.1", port, None)
        };
        assert_eq!(answer, Err(Gave::Stopped));
        // On Linux a loopback connect completes at once, so the connection
        // was made and then closed: the listener has it queued and reading
        // from it finds the end. Elsewhere (macOS) the connect can still be in
        // progress when the cancel closes it, and nothing ever reaches the
        // listener, which is as good. So: whatever arrives within two seconds
        // must already be closed, and nothing arriving is fine.
        listener.set_nonblocking(true).expect("non-blocking");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let accepted = loop {
            match listener.accept() {
                Ok((stream, _)) => break Some(stream),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < until => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break None,
            }
        };
        if cfg!(target_os = "linux") {
            assert!(accepted.is_some(), "on Linux the connection is made before the cancel is seen");
        }
        if let Some(mut accepted) = accepted {
            accepted.set_nonblocking(false).expect("blocking");
            accepted.set_read_timeout(Some(std::time::Duration::from_secs(5))).expect("a timeout");
            let mut byte = [0u8; 1];
            use std::io::Read;
            match accepted.read(&mut byte) {
                Ok(n) => assert_eq!(n, 0, "the connection was left open"),
                Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset, "{e}"),
            }
        }
    }

    /// **A read the deadline ended says so, and one the per-read limit ended
    /// does not.** Both come back -1, and the caller reports the first as
    /// the call running out of time and the second as a peer that went
    /// quiet. Told apart by the clock afterwards, a reset in the deadline's
    /// last millisecond was a timeout.
    #[test]
    fn a_read_says_whether_the_deadline_ended_it() {
        let (client, _peer) = a_connected_pair();
        let socket = socket_of(&client);
        assert_eq!(khora_net_prepare(socket), 0);
        let read = move || {
            let mut buffer = [0u8; 4];
            // SAFETY: four writable bytes.
            unsafe { khora_net_recv(socket, buffer.as_mut_ptr(), 4) }
        };

        // The per-read limit runs out long before the deadline.
        khora_net_set_timeout(socket, 50);
        khora_net_set_deadline(socket, crate::time::khora_monotonic_millis() + 60_000);
        assert_eq!(within_ten_seconds("a read past its own limit", read), -1);
        assert_eq!(khora_net_expired(socket), 0, "the per-read limit was taken for the deadline");

        // Now the deadline comes first.
        khora_net_set_timeout(socket, 0);
        khora_net_set_deadline(socket, crate::time::khora_monotonic_millis() + 50);
        assert_eq!(within_ten_seconds("a read past the deadline", read), -1);
        assert_eq!(khora_net_expired(socket), 1, "the deadline ended the read and nothing says so");

        // Setting the deadline again starts the account again.
        khora_net_set_deadline(socket, -1);
        assert_eq!(khora_net_expired(socket), 0);
        khora_net_forget(socket);
    }
}
