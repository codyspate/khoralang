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
use std::net::TcpStream;
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

/// Puts the handle back into the `TcpStream` that will close it.
///
/// The one thing `Wire` still borrows from `std`: closing a socket is spelled
/// differently on each platform and dropping a `TcpStream` over it is the
/// version already written.
#[cfg(windows)]
fn reclaim(socket: Socket) -> TcpStream {
    use std::os::windows::io::FromRawSocket;
    // SAFETY: `Wire` owns the handle and is being dropped, so nothing else
    // will close it.
    unsafe { TcpStream::from_raw_socket(socket as u64 as std::os::windows::io::RawSocket) }
}

#[cfg(not(windows))]
fn reclaim(socket: Socket) -> TcpStream {
    use std::os::fd::FromRawFd;
    // SAFETY: as above.
    unsafe { TcpStream::from_raw_fd(socket) }
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
        drop(reclaim(self.0));
    }
}

/// Takes ownership of a raw handle handed over by `std::net::tls`.
fn adopt(handle: i64) -> Wire {
    Wire(as_socket(handle))
}

/// Takes the handle out of a `TcpStream`, so that closing becomes ours.
#[cfg(windows)]
fn surrender(stream: TcpStream) -> Socket {
    use std::os::windows::io::IntoRawSocket;
    stream.into_raw_socket() as Socket
}

#[cfg(not(windows))]
fn surrender(stream: TcpStream) -> Socket {
    use std::os::fd::IntoRawFd;
    stream.into_raw_fd()
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
pub unsafe extern "C" fn khora_tls_server_close(server: *mut u8) {
    if server.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    drop(unsafe { Box::from_raw(server.cast::<Arc<ServerConfig>>()) });
}

/// Completes a handshake over `socket`, giving a connection to read and write.
///
/// **Takes the socket.** [`khora_tls_close`] closes it, and the caller must
/// not. Returns null if the handshake fails, having closed the socket itself —
/// a client that cannot agree on a cipher has already been answered as far as
/// it is going to be.
///
/// The handshake happens here rather than lazily on the first read, so that a
/// caller can tell "this is not a TLS client" from "this client sent nothing".
///
/// # Safety
///
/// `server` must have come from [`khora_tls_server_open`], and `socket` must be
/// an accepted socket that nothing else will close.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_tls_accept(server: *mut u8, socket: i64) -> *mut u8 {
    if server.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per the contract above; the configuration outlives this call.
    let config = unsafe { &*server.cast::<Arc<ServerConfig>>() };

    let Ok(mut connection) = ServerConnection::new(Arc::clone(config)) else {
        return std::ptr::null_mut();
    };
    let mut stream = adopt(socket);
    if connection.complete_io(&mut stream).is_err() {
        // `stream` owns the socket and closing it is what dropping it does.
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(Secured::Serving(StreamOwned::new(connection, stream)))).cast()
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
/// # Safety
///
/// `extra` must address `extra_len` readable bytes, or be null with a length of
/// zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_tls_client_open(extra: *const u8, extra_len: usize) -> *mut u8 {
    let mut roots = RootCertStore::empty();
    for certificate in rustls_native_certs::load_native_certs().certs {
        // A store may hold one the parser dislikes; the rest are still good.
        let _ = roots.add(certificate);
    }

    if !extra.is_null() && extra_len > 0 {
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
    }

    if roots.is_empty() {
        return std::ptr::null_mut();
    }
    let config = ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    Box::into_raw(Box::new(Arc::new(config))).cast()
}

/// Releases a client's configuration.
///
/// # Safety
///
/// `client` must be null or have come from [`khora_tls_client_open`], and must
/// not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_tls_client_close(client: *mut u8) {
    if client.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    drop(unsafe { Box::from_raw(client.cast::<Arc<ClientConfig>>()) });
}

/// Connects to `host:port` and handshakes, verifying the certificate names
/// `host`.
///
/// **The TCP connection is made here too**, which is not scope creep: it needs
/// a name resolved, and `getaddrinfo` across `extern fn` would be a `struct
/// addrinfo` crossing the ABI — which errata 35 forbids, and which nobody
/// should hand-roll twice. `TcpStream::connect` takes a name and a port.
///
/// Null if the name does not resolve, the connection is refused, or the
/// certificate does not verify. **There is no argument that turns verification
/// off.** A caller who must trust something unusual adds it as a root when the
/// client is built, where a reviewer can see which certificate and why.
///
/// # Safety
///
/// `client` must have come from [`khora_tls_client_open`], and `host` must
/// address `host_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_tls_connect(
    client: *mut u8,
    host: *const u8,
    host_len: usize,
    port: i64,
) -> *mut u8 {
    if client.is_null() || host.is_null() || !(1..=65535).contains(&port) {
        return std::ptr::null_mut();
    }
    // SAFETY: per the contract above.
    let config = unsafe { &*client.cast::<Arc<ClientConfig>>() };
    let bytes = unsafe { std::slice::from_raw_parts(host, host_len) };
    let Ok(host) = std::str::from_utf8(bytes) else { return std::ptr::null_mut() };

    let Ok(name) = ServerName::try_from(host.to_string()) else {
        return std::ptr::null_mut();
    };
    let Ok(mut connection) = ClientConnection::new(Arc::clone(config), name) else {
        return std::ptr::null_mut();
    };
    // **Still a blocking connect**, and that is a gap rather than a decision:
    // resolving a name and completing a three-way handshake both hold the
    // worker. It belongs in the blocking pool that 11E built, and wants a
    // caller that has noticed it before it is worth the churn.
    let Ok(connected) = TcpStream::connect((host, port as u16)) else {
        return std::ptr::null_mut();
    };
    // The handle is taken out of the `TcpStream` so that `Wire` owns it and
    // closes it, and prepared so that the handshake and everything after it
    // suspend the fiber rather than the worker.
    let mut stream = Wire(surrender(connected));
    crate::net::khora_net_prepare(stream.0);
    if connection.complete_io(&mut stream).is_err() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(Secured::Calling(StreamOwned::new(connection, stream)))).cast()
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
    // Dropping the `TcpStream` inside closes the socket.
}
