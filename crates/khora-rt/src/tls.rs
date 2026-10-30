//! TLS, bound rather than written.
//!
//! `docs/design/ecosystem.md` decides this outright — "TLS, crypto:
//! correctness is a specialist matter and a bug is a breach" — leaving only
//! what to bind and where. `rustls`, here in the runtime, for the same reason
//! `memmem` and float formatting are.
//!
//! **Not OpenSSL through `extern fn`**, which is the more literal reading of
//! "bound" and fails the test that matters: a Khora program would need `libssl`
//! present to link and to run, which on Windows means most people cannot build
//! it. `rustls` compiles into the runtime every executable already carries.
//!
//! # What crosses the boundary
//!
//! Nothing but scalars and pointers, per errata 35. A configuration and a
//! connection are each an opaque `void *` — a `Ptr` on the Khora side — and
//! their lifetimes are tied to a scope with `acquire`, exactly as `std::fs`
//! ties an open file. There is no new compiler-known type and no change to the
//! code generator.
//!
//! # The socket
//!
//! A connection **takes** the socket it is handed and closes it. Khora's `shut`
//! must not also close it, or the second close lands on a descriptor the
//! operating system may have reissued — which shows up as one connection
//! reading another's bytes, months later.

use std::io::{Read, Write};
use crate::reactor::Socket;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
};

/// A handshaken connection, and the socket underneath it.
///
/// Two shapes, because `rustls` has two connection types and they share no
/// trait worth naming. Everything past the handshake — read, write, close — is
/// the same on both, which is why the Khora side has one `TlsConnection` and
/// not two.
enum Secured {
    Serving(StreamOwned<ServerConnection, Wire>),
    Calling(StreamOwned<ClientConnection, Wire>),
}

impl Secured {
    fn stream(&mut self) -> &mut dyn Talking {
        match self {
            Secured::Serving(stream) => stream,
            Secured::Calling(stream) => stream,
        }
    }

    /// The socket underneath, which is what a deadline is set on.
    fn socket(&self) -> Socket {
        match self {
            Secured::Serving(stream) => stream.sock.0,
            Secured::Calling(stream) => stream.sock.0,
        }
    }
}

/// What either side needs once the handshake is done.
trait Talking: Read + Write {
    fn say_goodbye(&mut self);
}

impl Talking for StreamOwned<ServerConnection, Wire> {
    fn say_goodbye(&mut self) {
        self.conn.send_close_notify();
    }
}

impl Talking for StreamOwned<ClientConnection, Wire> {
    fn say_goodbye(&mut self) {
        self.conn.send_close_notify();
    }
}

/// The socket under a TLS connection.
///
/// **Not a `TcpStream`, and it cannot be one.** `std::net::socket` prepares
/// every socket it accepts, so a connection reaching here is non-blocking — and
/// `TcpStream::read` on one answers `WouldBlock` rather than waiting, which
/// rustls, written against a transport that blocks, reads as an error.
///
/// So the transport is the runtime's own: `khora_net_recv` and `khora_net_send`
/// make the same call and, when it would block, suspend the fiber and wait on
/// the reactor rather than holding the worker. rustls gets the blocking
/// transport it expects, a TLS connection stops costing a worker while it
/// waits, and the handshake gets both for free, since `complete_io` goes
/// through the same two functions.
struct Wire(Socket);

/// The raw handle, as this platform's socket type.
///
/// One cast for both, because `Socket` is the alias that differs — a `usize`
/// on Windows and an `i32` elsewhere — and `as` narrows to whichever it is.
#[allow(clippy::unnecessary_cast)]
fn as_socket(handle: i64) -> Socket {
    handle as Socket
}

impl Read for Wire {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: `into` is a live slice of exactly the length passed.
        let read = unsafe {
            crate::net::khora_net_recv(self.0, into.as_mut_ptr(), into.len() as isize)
        };
        match read {
            0.. => Ok(read as usize),
            // **Not `last_os_error`.** `errno` is thread-local and a fiber is
            // not, so by the time this reads it the fiber may be on another
            // worker — `docs/design/ffi.md` §2. rustls needs to know that the
            // read failed and not which number the failure had, and a kind it
            // might mistake for `WouldBlock` would send it round a loop these
            // shims have already been round.
            _ => Err(std::io::Error::other("the socket read failed")),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, from: &[u8]) -> std::io::Result<usize> {
        // SAFETY: `from` is a live slice of exactly the length passed.
        let sent =
            unsafe { crate::net::khora_net_send(self.0, from.as_ptr(), from.len() as isize) };
        match sent {
            0.. => Ok(sent as usize),
            _ => Err(std::io::Error::other("the socket write failed")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Nothing is buffered here; rustls does its own.
        Ok(())
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        // The runtime remembers this socket's receive deadline, and handles
        // are reused.
        crate::net::khora_net_forget(self.0);
        // **Closed by number and counted, not by dropping a `TcpStream`.**
        // A socket something else had closed already fails here, and that is
        // the symptom of two owners closing one socket: counted, a test can
        // assert there were none. Dropped as a `TcpStream`, a debug runtime
        // aborted the process on it ("IO Safety violation") and a release one
        // said nothing.
        crate::net::close_counted(self.0);
    }
}

impl Wire {
    /// Gives the socket back to whoever lent it, open, without the close
    /// dropping a `Wire` does: a failed [`khora_tls_connect_over`] leaves the
    /// socket with the caller that registered its close.
    fn hand_back(self) {
        std::mem::forget(self);
    }
}

/// Takes ownership of a raw handle handed over by `std::net::tls`.
fn adopt(handle: i64) -> Wire {
    Wire(as_socket(handle))
}

/// A server's certificate chain and key, ready to accept connections.
///
/// PEM in both cases, because that is what a certificate arrives as: from a
/// file, from a secret store, from Let's Encrypt. Taking DER would make every
/// caller convert.
///
/// Returns null if either is unreadable or they do not agree. The reason is not
/// passed back: there are exactly two, and a `rustls` message is not something
/// a Khora program should be reading.
///
/// # Safety
///
/// Both pointers must address their stated length in readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_tls_server_open(
    certificate: *const u8,
    certificate_len: usize,
    key: *const u8,
    key_len: usize,
) -> *mut u8 {
    if certificate.is_null() || key.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees both lengths are readable.
    let (mut certificate, mut key) = unsafe {
        (
            std::slice::from_raw_parts(certificate, certificate_len),
            std::slice::from_raw_parts(key, key_len),
        )
    };

    let chain: Vec<CertificateDer<'static>> =
        match rustls_pemfile::certs(&mut certificate).collect::<Result<_, _>>() {
            Ok(chain) => chain,
            Err(_) => return std::ptr::null_mut(),
        };
    if chain.is_empty() {
        return std::ptr::null_mut();
    }
    let Ok(Some(key)) = rustls_pemfile::private_key(&mut key) else {
        return std::ptr::null_mut();
    };
    let key: PrivateKeyDer<'static> = key;

    let Ok(config) = ServerConfig::builder().with_no_client_auth().with_single_cert(chain, key)
    else {
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(Arc::new(config))).cast()
}

/// Releases a configuration.
///
/// # Safety
///
/// `server` must be null or have come from [`khora_tls_server_open`], and must
/// not be used afterwards.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_server_close(server: *mut u8) {
    if server.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    drop(unsafe { Box::from_raw(server.cast::<Arc<ServerConfig>>()) });
}

/// Completes a handshake over `socket`, giving a connection to read and write.
///
/// **Takes the socket, on every path.** [`khora_tls_close`] closes it, and the
/// caller must not. Returns null if the handshake fails, or cannot start,
/// having closed the socket itself — a client that cannot agree on a cipher
/// has already been answered as far as it is going to be. Closed on every
/// null, so a caller that hands the socket over before this call never has
/// to ask which null it got: one that closed it after some nulls and not
/// others either leaked a socket or closed a handle the system had already
/// given to somebody else.
///
/// The handshake happens here rather than lazily on the first read, so that a
/// caller can tell "this is not a TLS client" from "this client sent nothing".
///
/// # Safety
///
/// `server` must be null or have come from [`khora_tls_server_open`], and
/// `socket` must be an accepted socket that nothing else will close.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_accept(server: *mut u8, socket: i64) -> *mut u8 {
    // Owned from the first line: every return below that does not hand it
    // on drops it, and dropping it closes the socket.
    let mut stream = adopt(socket);
    if server.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per the contract above; the configuration outlives this call.
    let config = unsafe { &*server.cast::<Arc<ServerConfig>>() };

    let Ok(mut connection) = ServerConnection::new(Arc::clone(config)) else {
        return std::ptr::null_mut();
    };
    if connection.complete_io(&mut stream).is_err() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(Secured::Serving(StreamOwned::new(connection, stream)))).cast()
}

/// The machine's roots, loaded once for the life of the process.
///
/// **What this prevents: every connection paying to read the system's
/// certificate store.** `load_native_certs` reads and parses every root on
/// the machine, about 10 ms in a debug build, and a PostgreSQL pool built a
/// client per connection: a quarter of each TLS open was rebuilding the same
/// store. It also made a canceled open expensive to get wrong, since what a
/// cancel could strand was the whole store.
///
/// What it costs: a root added to or removed from the machine while the
/// program runs is not seen until it restarts. Clients built with extra roots
/// copy this store and add to the copy, so they cost a copy, not a load.
fn native_roots() -> &'static RootCertStore {
    static ROOTS: std::sync::OnceLock<RootCertStore> = std::sync::OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        for certificate in rustls_native_certs::load_native_certs().certs {
            // A store may hold one the parser dislikes; the rest are still good.
            let _ = roots.add(certificate);
        }
        roots
    })
}

/// The client every caller with no extra roots shares: built on first use.
fn native_client() -> Option<Arc<ClientConfig>> {
    static CLIENT: std::sync::OnceLock<Option<Arc<ClientConfig>>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let roots = native_roots().clone();
            (!roots.is_empty()).then(|| {
                Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
            })
        })
        .clone()
}

/// How many clients from [`khora_tls_client_open`] are not yet closed.
static CLIENTS_OPEN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// How many TLS clients are open: made by [`khora_tls_client_open`] and not
/// yet given to [`khora_tls_client_close`].
///
/// **For a leak test that `khora_live_count` cannot be**: a client is a Rust
/// allocation the Khora heap never sees, so a client whose close was never
/// registered showed in nothing but the process's resident size, which is
/// Linux-only to read and noisy. This is exact and works on every system.
/// What it costs: one relaxed atomic add per open and per close.
#[unsafe(no_mangle)]
pub extern "C" fn khora_tls_clients_open() -> i64 {
    CLIENTS_OPEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// A client's trust: the machine's roots, plus `extra` if any is given.
///
/// **The operating system's store rather than a list compiled in here.** A
/// program should trust what its machine trusts — a corporate CA is the
/// ordinary case, not an exotic one — and a bundled root list is a snapshot
/// that goes stale between releases without saying so.
///
/// `extra` is PEM and may be empty. It is for an internal CA the machine does
/// not know about, and for a test. The alternative every other library offers
/// is a switch that turns verification off, which solves the same problem by
/// removing the point of the exercise.
///
/// Null if `extra` is unreadable, or if there are no roots at all — a client
/// that trusts nothing fails every connection later, and failing here says why.
///
/// The machine's roots are read once per process ([`native_roots`]); with no
/// `extra`, every client is the same shared configuration, so opening one
/// costs an allocation.
///
/// # Safety
///
/// `extra` must address `extra_len` readable bytes, or be null with a length of
/// zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_tls_client_open(extra: *const u8, extra_len: usize) -> *mut u8 {
    let config = if extra.is_null() || extra_len == 0 {
        let Some(shared) = native_client() else { return std::ptr::null_mut() };
        shared
    } else {
        let mut roots = native_roots().clone();
        // SAFETY: the caller guarantees the length is readable.
        let mut pem = unsafe { std::slice::from_raw_parts(extra, extra_len) };
        let parsed: Result<Vec<CertificateDer<'static>>, _> =
            rustls_pemfile::certs(&mut pem).collect();
        let Ok(added) = parsed else { return std::ptr::null_mut() };
        if added.is_empty() {
            return std::ptr::null_mut();
        }
        for certificate in added {
            if roots.add(certificate).is_err() {
                return std::ptr::null_mut();
            }
        }
        Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
    };
    CLIENTS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Box::into_raw(Box::new(config)).cast()
}

/// Releases a client's configuration.
///
/// # Safety
///
/// `client` must be null or have come from [`khora_tls_client_open`], and must
/// not be used afterwards.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_client_close(client: *mut u8) {
    if client.is_null() {
        return;
    }
    CLIENTS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: per the contract above.
    drop(unsafe { Box::from_raw(client.cast::<Arc<ClientConfig>>()) });
}

/// Connects to `host:port` and handshakes, verifying the certificate names
/// `host`, giving up when the fiber is canceled or the monotonic clock
/// reaches `at` (negative for never).
///
/// **The TCP connection is made here too**, which is not scope creep: it needs
/// a name resolved, and `getaddrinfo` across `extern fn` would be a `struct
/// addrinfo` crossing the ABI — which errata 35 forbids, and which nobody
/// should hand-roll twice. It goes through the same reactor-driven connect as
/// `khora_net_connect`, so a canceled fiber inside it stops at once rather
/// than when the kernel gives up on a host that never answers.
///
/// **`at` bounds the handshake as well**, by holding the socket to it while
/// the handshake runs; it is cleared once the session is up, and
/// [`khora_tls_set_deadline`] is how a caller keeps one on the session.
///
/// Null if the name does not resolve, the connection is refused, the
/// certificate does not verify, the deadline passes or the fiber is
/// canceled; `why` is set to 1 when the deadline was the cause and 0
/// otherwise, so the caller need not guess it from the clock. **There is no argument that turns verification off.** A caller
/// who must trust something unusual adds it as a root when the client is
/// built, where a reviewer can see which certificate and why.
///
/// # Safety
///
/// `client` must have come from [`khora_tls_client_open`], `host` must
/// address `host_len` readable bytes, and `why` must be null or address one
/// writable `i64`.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_connect(
    client: *mut u8,
    host: *const u8,
    host_len: usize,
    port: i64,
    at: i64,
    why: *mut i64,
) -> *mut u8 {
    let say = |deadline: bool| {
        if !why.is_null() {
            // SAFETY: the caller guarantees `why` is null or one writable
            // `i64`, and it is not null here.
            unsafe { why.write(i64::from(deadline)) };
        }
    };
    say(false);
    if client.is_null() || host.is_null() || !(1..=65535).contains(&port) {
        return std::ptr::null_mut();
    }
    // SAFETY: per the contract above.
    let config = unsafe { &*client.cast::<Arc<ClientConfig>>() };
    // SAFETY: per the contract above.
    let bytes = unsafe { std::slice::from_raw_parts(host, host_len) };
    let Ok(host) = std::str::from_utf8(bytes) else { return std::ptr::null_mut() };

    let Ok(name) = ServerName::try_from(host.to_string()) else {
        return std::ptr::null_mut();
    };
    let Ok(mut connection) = ClientConnection::new(Arc::clone(config), name) else {
        return std::ptr::null_mut();
    };
    let deadline = (at >= 0).then(|| crate::time::instant_of(at));
    let socket = match crate::net::connect_by(host, port as u16, deadline) {
        Ok(socket) => socket,
        Err(crate::net::Gave::TimedOut) => {
            say(true);
            return std::ptr::null_mut();
        }
        Err(crate::net::Gave::Up | crate::net::Gave::Stopped) => return std::ptr::null_mut(),
    };
    // `Wire` owns the handle from here and closes it on every path out,
    // the failed handshake included.
    let mut stream = Wire(socket);
    crate::net::khora_net_set_deadline(stream.0, at);
    if connection.complete_io(&mut stream).is_err() {
        say(crate::net::khora_net_expired(stream.0) != 0);
        return std::ptr::null_mut();
    }
    // A session finished for a fiber canceled meanwhile goes the way a bare
    // connection does in `connect_by`: closed here, because its caller stops
    // before it can register it.
    if crate::current::current(|f| f.gives_up_waiting()) {
        return std::ptr::null_mut();
    }
    crate::net::khora_net_set_deadline(stream.0, -1);
    Box::into_raw(Box::new(Secured::Calling(StreamOwned::new(connection, stream)))).cast()
}

/// Runs a client handshake over `socket`, a connection the caller already
/// made, verifying the certificate names `host` exactly as
/// [`khora_tls_connect`] does, with the same trust; giving up when the fiber
/// is canceled or the monotonic clock reaches `at` (negative for never).
///
/// **For a protocol that negotiates TLS in the clear first.** PostgreSQL's
/// client sends `SSLRequest` on a plain connection and starts the handshake
/// only once the server has answered `S`, so the socket exists before any
/// TLS does, and [`khora_tls_connect`], which makes its own, cannot be used.
///
/// **On success the session takes the socket**, as an accepted one does:
/// [`khora_tls_close`] closes it and the caller must not. **On failure the
/// socket is still the caller's**, open, and holding the deadline it had
/// before. The caller registered the close of a socket it made, and a
/// failure that closed it as well would leave that close to land on a
/// descriptor the system may already have given to somebody else. A session
/// finished for a fiber canceled meanwhile is a failure in this sense: it is
/// let go of and the socket handed back, because the caller stops before it
/// can register the session.
///
/// `at` holds the socket's reads and writes while the handshake runs, and
/// the caller's own deadline is put back afterwards on both paths. `why` is
/// set to 1 when the deadline was what ended it, 2 when the certificate
/// chains to no trusted root, 3 when it is not for `host`, and 0 otherwise.
///
/// Null if `host` is not a name a certificate can be for, the certificate
/// does not verify, the peer is not speaking TLS, the deadline passes or the
/// fiber is canceled. **There is no argument that turns verification off**,
/// for the reason [`khora_tls_connect`] gives.
///
/// What it costs: nothing past the handshake itself.
///
/// # Safety
///
/// `client` must have come from [`khora_tls_client_open`], `socket` must be an
/// open, connected socket prepared by `khora_net_prepare`, `host` must address
/// `host_len` readable bytes, and `why` must be null or address one writable
/// `i64`.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_connect_over(
    client: *mut u8,
    socket: i64,
    host: *const u8,
    host_len: usize,
    at: i64,
    why: *mut i64,
) -> *mut u8 {
    let say = |deadline: bool| {
        if !why.is_null() {
            // SAFETY: the caller guarantees `why` is null or one writable
            // `i64`, and it is not null here.
            unsafe { why.write(i64::from(deadline)) };
        }
    };
    say(false);
    if client.is_null() || host.is_null() || socket < 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: per the contract above.
    let config = unsafe { &*client.cast::<Arc<ClientConfig>>() };
    // SAFETY: per the contract above.
    let bytes = unsafe { std::slice::from_raw_parts(host, host_len) };
    let Ok(host) = std::str::from_utf8(bytes) else { return std::ptr::null_mut() };
    let Ok(name) = ServerName::try_from(host.to_string()) else {
        return std::ptr::null_mut();
    };
    let Ok(mut connection) = ClientConnection::new(Arc::clone(config), name) else {
        return std::ptr::null_mut();
    };

    let mut stream = adopt(socket);
    let (was_by, was_expired) = crate::net::deadline_of(stream.0);
    crate::net::khora_net_set_deadline(stream.0, at);
    let shook = connection.complete_io(&mut stream);
    let ran_out = crate::net::khora_net_expired(stream.0) != 0;
    crate::net::put_deadline(stream.0, was_by, was_expired);
    if shook.is_err() || crate::current::current(|f| f.gives_up_waiting()) {
        if !why.is_null() {
            let cause = match &shook {
                Err(_) if ran_out => Cause::Deadline,
                Err(e) => cause_of(e),
                Ok(_) => Cause::Other,
            };
            // SAFETY: the caller guarantees `why` is null or one writable
            // `i64`, and it is not null here.
            unsafe { why.write(cause as i64) };
        }
        // Handed back unclosed: see above.
        stream.hand_back();
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(Secured::Calling(StreamOwned::new(connection, stream)))).cast()
}

/// Why a client handshake failed, as `why` carries it to `std::net::tls`.
///
/// **What this prevents: one message listing three causes.** A refusal
/// said the certificate was untrusted, or for another name, or the peer had
/// stopped speaking TLS, and left the reader to guess; the first two have
/// different fixes (trust the authority, connect by the name it names), and
/// rustls knows which it was. Anything else is `Other`: rustls's own text is
/// not something a Khora program should be parsing.
#[derive(Clone, Copy)]
enum Cause {
    Other = 0,
    Deadline = 1,
    UnknownIssuer = 2,
    WrongName = 3,
}

/// The [`Cause`] behind a failed `complete_io`.
fn cause_of(error: &std::io::Error) -> Cause {
    use rustls::{CertificateError, Error};
    match error.get_ref().and_then(|e| e.downcast_ref::<Error>()) {
        Some(Error::InvalidCertificate(CertificateError::UnknownIssuer)) => Cause::UnknownIssuer,
        Some(Error::InvalidCertificate(
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
        )) => Cause::WrongName,
        _ => Cause::Other,
    }
}

/// Holds every later read and write on this session to the monotonic clock
/// reaching `at`, or clears that with a negative `at`.
///
/// The session's half of `khora_net_set_deadline`, set on the socket
/// underneath for the reason [`khora_tls_set_timeout`] is. Answers 0, or -1
/// if the connection is null.
///
/// # Safety
///
/// `connection` must be null or have come from [`khora_tls_accept`] or
/// [`khora_tls_connect`].
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_set_deadline(connection: *mut u8, at: i64) -> i32 {
    if connection.is_null() {
        return -1;
    }
    // SAFETY: per the contract above.
    let secured = unsafe { &*connection.cast::<Secured>() };
    crate::net::khora_net_set_deadline(secured.socket(), at)
}

/// Whether a read or write on this session gave up because the deadline set
/// by [`khora_tls_set_deadline`] passed: 1 if so, 0 if not, or for a null
/// connection.
///
/// # Safety
///
/// `connection` must be null or have come from [`khora_tls_accept`] or
/// [`khora_tls_connect`].
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_expired(connection: *mut u8) -> i32 {
    if connection.is_null() {
        return 0;
    }
    // SAFETY: per the contract above.
    let secured = unsafe { &*connection.cast::<Secured>() };
    crate::net::khora_net_expired(secured.socket())
}

/// How long a read on this session may wait before it reports a timeout.
///
/// **The gap this closes.** `khora_net_set_timeout` takes a socket, and a TLS
/// session owns its socket rather than handing it back -- so until this
/// existed, `https` had no deadline at all and a server that accepted a
/// connection and then said nothing held a fiber until the process ended. The
/// plain path has had `set_receive_timeout` since the reactor did.
///
/// Set on the socket rather than on the session, because that is where the
/// reactor's timer lives; rustls never learns about it and does not need to. A
/// read that runs out fails, which is what every caller already treats as a
/// connection to give up on.
///
/// Answers 0 on success and -1 if the connection is null, matching the socket
/// call it forwards to.
///
/// # Safety
///
/// `connection` must be null or have come from [`khora_tls_accept`] or
/// [`khora_tls_connect`].
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_set_timeout(connection: *mut u8, millis: i64) -> i32 {
    if connection.is_null() {
        return -1;
    }
    // SAFETY: per the contract above.
    let secured = unsafe { &*connection.cast::<Secured>() };
    crate::net::khora_net_set_timeout(secured.socket(), millis)
}

/// Reads at most `len` plaintext bytes, or 0 at the end, or -1 on failure.
///
/// Zero means the peer closed, matching `recv` — the caller's loop already ends
/// on it and does not need a third case to learn.
///
/// # Safety
///
/// `connection` must have come from [`khora_tls_accept`], and `into` must
/// address `len` writable bytes.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_read(
    connection: *mut u8,
    into: *mut u8,
    len: usize,
) -> i64 {
    if connection.is_null() || into.is_null() || len == 0 {
        return -1;
    }
    // SAFETY: per the contract above.
    let secured = unsafe { &mut *connection.cast::<Secured>() };
    let buffer = unsafe { std::slice::from_raw_parts_mut(into, len) };
    match secured.stream().read(buffer) {
        Ok(read) => read as i64,
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => 0,
        Err(_) => -1,
    }
}

/// Plaintext that has already arrived, and never waits: how many bytes, 0
/// when the peer has closed, -1 when nothing has arrived yet, or -2 for a
/// failure. The answers `khora_net_recv_now` gives on a plain socket.
///
/// **What this prevents: a pool lending a secured connection the server has
/// already closed.** The check before a lease reads what has arrived without
/// waiting, and [`khora_tls_read`] would wait. A TLS peer that says nothing
/// in plaintext still sends records: a TLS 1.3 server sends its session
/// tickets after the handshake, unasked. Those are taken in and processed
/// here, and answer -1, so a quiet healthy session is not taken for one with
/// something to say. A `close_notify`, or the socket closing under the
/// session, answers 0; a record that does not decrypt answers -2.
///
/// # Safety
///
/// `connection` must have come from [`khora_tls_accept`],
/// [`khora_tls_connect`] or [`khora_tls_connect_over`], and `into` must
/// address `len` writable bytes.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_read_now(
    connection: *mut u8,
    into: *mut u8,
    len: usize,
) -> i64 {
    if connection.is_null() || into.is_null() || len == 0 {
        return -2;
    }
    // SAFETY: per the contract above.
    let secured = unsafe { &mut *connection.cast::<Secured>() };
    // SAFETY: per the contract above.
    let buffer = unsafe { std::slice::from_raw_parts_mut(into, len) };
    match secured {
        Secured::Serving(stream) => read_now(&mut stream.conn, stream.sock.0, buffer),
        Secured::Calling(stream) => read_now(&mut stream.conn, stream.sock.0, buffer),
    }
}

/// [`khora_tls_read_now`], for either side.
fn read_now<S: rustls::SideData>(
    conn: &mut rustls::ConnectionCommon<S>,
    socket: Socket,
    into: &mut [u8],
) -> i64 {
    loop {
        match conn.reader().read(into) {
            Ok(read) => return read as i64,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return 0,
            Err(_) => return -2,
        }
        // Nothing decrypted is waiting: take in what the socket holds, and
        // stop when that is nothing. Each turn consumes bytes the socket
        // already had, so the loop ends when they do.
        match conn.read_tls(&mut Arrived(socket)) {
            Ok(0) => return 0,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return -1,
            Err(_) => return -2,
        }
        if conn.process_new_packets().is_err() {
            return -2;
        }
    }
}

/// A socket read that never waits, as `rustls` reads a transport: what has
/// arrived, or `WouldBlock`.
struct Arrived(Socket);

impl Read for Arrived {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: `into` is a live slice of exactly the length passed.
        let read = unsafe {
            crate::net::khora_net_recv_now(self.0, into.as_mut_ptr(), into.len() as isize)
        };
        match read {
            0.. => Ok(read as usize),
            -1 => Err(std::io::ErrorKind::WouldBlock.into()),
            _ => Err(std::io::Error::other("the socket read failed")),
        }
    }
}

/// Writes `len` bytes, all of them, or -1.
///
/// All or nothing, because a partial TLS write is not something a caller can do
/// anything useful with: the record is already encrypted and the rest of it has
/// to go.
///
/// # Safety
///
/// `connection` must have come from [`khora_tls_accept`], and `from` must
/// address `len` readable bytes.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_write(
    connection: *mut u8,
    from: *const u8,
    len: usize,
) -> i64 {
    if connection.is_null() || from.is_null() {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    // SAFETY: per the contract above.
    let secured = unsafe { &mut *connection.cast::<Secured>() };
    let bytes = unsafe { std::slice::from_raw_parts(from, len) };
    let stream = secured.stream();
    match stream.write_all(bytes).and_then(|()| stream.flush()) {
        Ok(()) => len as i64,
        Err(_) => -1,
    }
}

/// Sends `close_notify`, then closes the socket.
///
/// # Safety
///
/// `connection` must be null or have come from [`khora_tls_accept`], and must
/// not be used afterwards.
#[unsafe(no_mangle)]
// SHARE: a runtime-owned TLS object, not a Khora value.
pub unsafe extern "C" fn khora_tls_close(connection: *mut u8) {
    if connection.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    let mut secured = unsafe { Box::from_raw(connection.cast::<Secured>()) };
    // Politeness that is also protocol: without it a peer cannot tell a closed
    // connection from a truncated one, which is the whole of the truncation
    // attack TLS 1.3 closes.
    let stream = secured.stream();
    stream.say_goodbye();
    let _ = stream.flush();
    // Dropping the `Wire` inside closes the socket.
}

#[cfg(test)]
mod tests {
    //! The upgrade over a caller's socket, against a `rustls` server on a
    //! thread: the shape PostgreSQL's `SSLRequest` gives it, a plaintext
    //! exchange first and the handshake on the same connection after.

    use super::*;
    use crate::reactor::socket_of;
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    const LEAF: &str = include_str!("../../khora-codegen-llvm/tests/fixtures/localhost.cert.pem");
    const KEY: &str = include_str!("../../khora-codegen-llvm/tests/fixtures/localhost.key.pem");
    const CA: &str = include_str!("../../khora-codegen-llvm/tests/fixtures/ca.cert.pem");

    fn server_config() -> Arc<ServerConfig> {
        let chain: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut LEAF.as_bytes()).collect::<Result<_, _>>().expect("the leaf");
        let key = rustls_pemfile::private_key(&mut KEY.as_bytes()).expect("the key").expect("a key");
        Arc::new(
            ServerConfig::builder().with_no_client_auth().with_single_cert(chain, key).expect("a config"),
        )
    }

    /// A client trusting `pem` besides the machine's store, and the release.
    struct Client(*mut u8);

    impl Client {
        fn trusting(pem: &str) -> Client {
            // SAFETY: `pem` is live for the call and its length is passed.
            let client = unsafe { khora_tls_client_open(pem.as_ptr(), pem.len()) };
            assert!(!client.is_null(), "the client should build");
            Client(client)
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            // SAFETY: opened by `khora_tls_client_open` and not used again.
            unsafe { khora_tls_client_close(self.0) };
        }
    }

    /// What the server thread does once the plaintext `ask` has arrived and
    /// it has answered `S`.
    #[derive(Clone, Copy)]
    enum Then {
        /// Handshake, echo one message, then send `close_notify` and close.
        Echo,
        /// Handshake, then say in the clear that the socket is still open.
        /// The client's handshake fails, so this is all it can read.
        SayStillOpen,
        /// Say nothing until the client hangs up.
        Silent,
        /// Handshake, then say nothing until the client hangs up.
        Quiet,
    }

    /// A server that reads four plaintext bytes, answers `S`, then `then`.
    ///
    /// The receiver hears once, at the state a test waits for instead of a
    /// sleep: for `Silent`, the client's first TLS record has arrived, so its
    /// handshake is under way and waiting; for `Quiet`, the server's last
    /// handshake flight and what follows it have been written.
    fn serve(
        then: Then,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = listener.local_addr().expect("an address");
        let config = server_config();
        let (reached, waiting) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a connection");
            stream.set_read_timeout(Some(Duration::from_secs(20))).expect("a timeout");
            let mut ask = [0u8; 4];
            stream.read_exact(&mut ask).expect("the plaintext request");
            assert_eq!(&ask, b"tls?");
            stream.write_all(b"S").expect("the answer");
            match then {
                Then::Echo => {
                    let mut conn = ServerConnection::new(config).expect("a session");
                    conn.complete_io(&mut stream).expect("the server's handshake");
                    let mut tls = rustls::Stream::new(&mut conn, &mut stream);
                    let mut heard = [0u8; 5];
                    tls.read_exact(&mut heard).expect("a message");
                    tls.write_all(&heard).expect("the echo");
                    tls.flush().expect("the echo, sent");
                    // Unasked, after the echo: what `read_now` must hand over.
                    tls.write_all(b"later").expect("more");
                    tls.flush().expect("more, sent");
                    conn.send_close_notify();
                    let _ = conn.complete_io(&mut stream);
                }
                Then::SayStillOpen => {
                    // The server's whole first flight. Then it waits for the
                    // client to speak in the clear on the same socket, after
                    // its refusal, and only then answers. Writing straight
                    // after the flight let the clear text arrive in the same
                    // read as the flight on macOS, where rustls took it into
                    // its own buffer and the caller never saw it.
                    let mut conn = ServerConnection::new(config).expect("a session");
                    while !conn.wants_write() {
                        conn.read_tls(&mut stream).expect("the client's hello");
                        conn.process_new_packets().expect("a readable hello");
                    }
                    while conn.wants_write() {
                        conn.write_tls(&mut stream).expect("the server's flight");
                    }
                    let mut heard = Vec::new();
                    let mut chunk = [0u8; 512];
                    while !heard.windows(4).any(|w| w == b"ping") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => heard.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let _ = stream.write_all(b"still open");
                    let mut rest = Vec::new();
                    let _ = stream.read_to_end(&mut rest);
                }
                Then::Silent => {
                    let mut header = [0u8; 5];
                    if stream.read_exact(&mut header).is_ok() {
                        let _ = reached.send(());
                    }
                    let mut rest = Vec::new();
                    let _ = stream.read_to_end(&mut rest);
                }
                Then::Quiet => {
                    let mut conn = ServerConnection::new(config).expect("a session");
                    conn.complete_io(&mut stream).expect("the server's handshake");
                    let _ = reached.send(());
                    let mut rest = Vec::new();
                    let _ = stream.read_to_end(&mut rest);
                }
            }
        });
        (address, server, waiting)
    }

    /// A connected, prepared socket that has sent the plaintext request and
    /// read the `S`: what a PostgreSQL client holds when it upgrades.
    fn asked(address: std::net::SocketAddr) -> Socket {
        let stream = TcpStream::connect(address).expect("a connection");
        let socket = socket_of(&stream);
        // The socket is the test's from here, as a Khora program's would be:
        // closed by the session, or by `close_counted` below when there is none.
        std::mem::forget(stream);
        assert_eq!(crate::net::khora_net_prepare(socket), 0);
        // SAFETY: four readable bytes.
        assert_eq!(unsafe { crate::net::khora_net_send(socket, b"tls?".as_ptr(), 4) }, 4);
        let mut answer = [0u8; 1];
        // SAFETY: one writable byte.
        assert_eq!(unsafe { crate::net::khora_net_recv(socket, answer.as_mut_ptr(), 1) }, 1);
        assert_eq!(&answer, b"S");
        socket
    }

    fn upgrade(client: &Client, socket: Socket, host: &str, at: i64) -> (*mut u8, i64) {
        let mut why: i64 = -7;
        // SAFETY: a client from `khora_tls_client_open`, a prepared socket,
        // `host` live for the call, and `why` one writable `i64`.
        let session = unsafe {
            khora_tls_connect_over(client.0, socket as i64, host.as_ptr(), host.len(), at, &raw mut why)
        };
        (session, why)
    }

    /// Reads in the clear until `wanted` has arrived, or the read fails.
    fn read_until(socket: Socket, wanted: &[u8]) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buffer = [0u8; 4096];
        while !got.windows(wanted.len()).any(|w| w == wanted) {
            // SAFETY: `buffer` is writable for its length.
            let read = unsafe { crate::net::khora_net_recv(socket, buffer.as_mut_ptr(), 4096) };
            if read <= 0 {
                break;
            }
            got.extend_from_slice(&buffer[..read as usize]);
        }
        got
    }

    /// **The handshake runs on the socket the caller made, after a
    /// plaintext exchange on it, and the session carries the conversation
    /// on.** Then a read that never waits: -1 while nothing has arrived
    /// (the server's session tickets are not plaintext), the bytes once they
    /// have, and 0 once the server has said goodbye.
    #[test]
    fn a_handshake_over_a_socket_already_in_use_verifies_and_talks() {
        let (address, server, _) = serve(Then::Echo);
        let client = Client::trusting(CA);
        let socket = asked(address);
        let (session, why) = upgrade(&client, socket, "localhost", -1);
        assert!(!session.is_null(), "the upgrade should succeed");
        assert_eq!(why, 0);

        // SAFETY: a live session and five readable bytes.
        assert_eq!(unsafe { khora_tls_write(session, b"hello".as_ptr(), 5) }, 5);
        let mut heard = [0u8; 5];
        let mut at = 0;
        while at < 5 {
            // SAFETY: a live session, and `5 - at` writable bytes from `at`.
            let read = unsafe { khora_tls_read(session, heard[at..].as_mut_ptr(), 5 - at) };
            assert!(read > 0, "the echo should arrive");
            at += read as usize;
        }
        assert_eq!(&heard, b"hello");

        // Whatever the server sent after the echo, read without waiting: the
        // tickets it sent unasked count for nothing, `later` arrives whole
        // and then the goodbye is the end. Bounded, so a read that never
        // sees the end fails rather than spinning.
        let mut later = Vec::new();
        let mut ended = false;
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(10) {
            let mut buffer = [0u8; 64];
            // SAFETY: a live session and 64 writable bytes.
            let read = unsafe { khora_tls_read_now(session, buffer.as_mut_ptr(), 64) };
            match read {
                0 => {
                    ended = true;
                    break;
                }
                -1 => std::thread::sleep(Duration::from_millis(2)),
                n if n > 0 => later.extend_from_slice(&buffer[..n as usize]),
                n => panic!("read_now failed with {n}"),
            }
        }
        assert_eq!(later, b"later");
        assert!(ended, "the goodbye should read as the end");
        // SAFETY: a live session, not used again.
        unsafe { khora_tls_close(session) };
        server.join().expect("the server");
    }

    /// **A quiet healthy session reads as nothing yet, not as something to
    /// say.** The server's session tickets arrive after the handshake,
    /// unasked; a read that handed them over as bytes, or took the records
    /// for plaintext, would mark every idle pooled connection broken.
    #[test]
    fn a_quiet_session_reads_as_nothing_yet() {
        let (address, server, ready) = serve(Then::Quiet);
        let client = Client::trusting(CA);
        let socket = asked(address);
        let (session, _) = upgrade(&client, socket, "localhost", -1);
        assert!(!session.is_null());
        // The server has written its tickets, which are what is under test.
        ready.recv_timeout(Duration::from_secs(10)).expect("the server should finish its handshake");
        let mut buffer = [0u8; 64];
        for _ in 0..3 {
            // SAFETY: a live session and 64 writable bytes.
            assert_eq!(unsafe { khora_tls_read_now(session, buffer.as_mut_ptr(), 64) }, -1);
        }
        // SAFETY: a live session, not used again.
        unsafe { khora_tls_close(session) };
        server.join().expect("the server");
    }

    /// **A certificate for another name is refused, and the socket stays
    /// the caller's and open.** The caller registered its close; a failure
    /// that closed it too would leave that close to land on a reissued
    /// descriptor. The server, its handshake refused, says so in the clear
    /// on the same socket, which only an open socket can read.
    #[test]
    fn a_wrong_name_is_refused_and_the_socket_is_handed_back() {
        let (address, server, _) = serve(Then::SayStillOpen);
        let client = Client::trusting(CA);
        let socket = asked(address);
        let (session, why) = upgrade(&client, socket, "127.0.0.1", -1);
        assert!(session.is_null(), "a certificate for `localhost` must not do for 127.0.0.1");
        assert_eq!(why, 3, "a refusal for the name, said to be the name");
        // Speak first, in the clear, on the socket handed back: only an open
        // socket can send this, and the server answers only once it arrives.
        // SAFETY: four readable bytes.
        assert_eq!(unsafe { crate::net::khora_net_send(socket, b"ping".as_ptr(), 4) }, 4);
        let got = read_until(socket, b"still open");
        assert!(
            got.windows(10).any(|w| w == b"still open"),
            "the socket should still be open and readable: {got:?}"
        );
        assert!(crate::net::close_counted(socket), "the socket handed back should still be open");
        server.join().expect("the server");
    }

    /// **A server whose certificate no trusted authority signed is
    /// refused.** The leaf itself is the only extra root here, which is
    /// not the authority that signed it.
    #[test]
    fn an_untrusted_authority_is_refused() {
        let (address, server, _) = serve(Then::SayStillOpen);
        let client = Client::trusting(LEAF);
        let socket = asked(address);
        let (session, why) = upgrade(&client, socket, "localhost", -1);
        assert!(session.is_null(), "an untrusted chain must be refused");
        assert_eq!(why, 2, "a refusal for the authority, said to be the authority");
        assert!(crate::net::close_counted(socket), "the socket handed back should still be open");
        server.join().expect("the server");
    }

    /// **A server that never starts its half ends at the deadline, said to
    /// be the deadline, and the socket keeps the deadline its owner set.**
    #[test]
    fn a_silent_server_ends_at_the_deadline_and_the_old_one_is_put_back() {
        let (address, server, _) = serve(Then::Silent);
        let client = Client::trusting(CA);
        let socket = asked(address);
        let owners = crate::time::khora_monotonic_millis() + 60_000;
        crate::net::khora_net_set_deadline(socket, owners);
        let before = crate::net::deadline_of(socket);
        let at = crate::time::khora_monotonic_millis() + 100;
        let began = Instant::now();
        let (session, why) = upgrade(&client, socket, "localhost", at);
        let took = began.elapsed();
        assert!(session.is_null());
        assert_eq!(why, 1, "the deadline ended it");
        assert!(took < Duration::from_secs(5), "the deadline took {took:?} to land");
        assert_eq!(crate::net::deadline_of(socket), before, "the owner's deadline should be back");
        crate::net::khora_net_forget(socket);
        assert!(crate::net::close_counted(socket), "the socket handed back should still be open");
        server.join().expect("the server");
    }

    /// **A cancel ends a handshake the server never answers**, at once, not
    /// at a deadline: there is none here. Off a scheduler, the thread
    /// backend's path.
    #[test]
    fn a_canceled_upgrade_gives_up_at_once() {
        use crate::current::{enter, Fiber};
        let (address, server, hello) = serve(Then::Silent);
        let socket = asked(address);
        let fiber = Fiber::spawned();
        let (done, finished) = std::sync::mpsc::channel();
        {
            let fiber = Arc::clone(&fiber);
            std::thread::spawn(move || {
                let client = Client::trusting(CA);
                let _in = enter(fiber);
                let (session, why) = upgrade(&client, socket, "localhost", -1);
                let _ = done.send((session.is_null(), why));
            });
        }
        // The ClientHello has arrived, so the handshake is waiting for an
        // answer that is not coming.
        hello.recv_timeout(Duration::from_secs(10)).expect("the client's hello should arrive");
        let canceled = Instant::now();
        fiber.cancel();
        let (refused, why) =
            finished.recv_timeout(Duration::from_secs(10)).expect("the canceled upgrade should end");
        let took = canceled.elapsed();
        assert!(refused, "a canceled upgrade has no session");
        assert_eq!(why, 0, "a cancel is not the deadline");
        assert!(took < Duration::from_secs(2), "the cancel took {took:?} to land");
        assert!(crate::net::close_counted(socket), "the socket handed back should still be open");
        server.join().expect("the server");
    }
}
