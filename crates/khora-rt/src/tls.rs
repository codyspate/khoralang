//! TLS, bound rather than written.
//!
//! `docs/design/ecosystem.md` decides this outright — "TLS, crypto:
//! correctness is a specialist matter and a bug is a breach" — so the only
//! question was what to bind and where. It is `rustls` here in the runtime,
//! for the same reason `memmem` and float formatting are: the runtime is
//! already the place where Rust does the work Khora should not.
//!
//! **Not OpenSSL through `extern fn`.** That would be the more literal reading
//! of "bound", and it fails the test that matters: a Khora program would then
//! need `libssl` present to link and to run, which on Windows means most people
//! cannot build it. `rustls` is compiled into the runtime that every executable
//! already carries, so TLS works wherever Khora does.
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
//! A connection **takes** the socket it is handed and closes it. Khora's
//! `shut` must not also close it, or the second close is on a descriptor the
//! operating system may have reissued — a bug that shows up as one connection
//! reading another's bytes, months later. `std::net::tls` says so where the
//! handle is handed over.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// A handshaken connection, and the socket underneath it.
struct Secured {
    stream: StreamOwned<ServerConnection, TcpStream>,
}

/// Turns a raw handle into the `TcpStream` that owns it.
///
/// The two platforms spell it differently and mean the same thing. Wrong on
/// macOS in the same way `std::net::socket` is, and absent for the same reason.
#[cfg(windows)]
fn adopt(handle: i64) -> TcpStream {
    use std::os::windows::io::FromRawSocket;
    // SAFETY: the caller hands over a socket it will not close again, which is
    // the contract `std::net::tls` states where it calls this.
    unsafe { TcpStream::from_raw_socket(handle as u64 as std::os::windows::io::RawSocket) }
}

#[cfg(not(windows))]
fn adopt(handle: i64) -> TcpStream {
    use std::os::fd::FromRawFd;
    // SAFETY: as above.
    unsafe { TcpStream::from_raw_fd(handle as i32) }
}

/// A server's certificate chain and key, ready to accept connections.
///
/// PEM in both cases, because that is what a certificate arrives as: from a
/// file, from a secret store, from Let's Encrypt. Taking DER would make every
/// caller convert.
///
/// Returns null if either is unreadable or they do not agree, which the caller
/// reports as its own error — the reason is not passed back because there are
/// exactly two ("the certificate" and "the key") and a `rustls` message is not
/// something a Khora program should be reading.
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
    Box::into_raw(Box::new(Secured { stream: StreamOwned::new(connection, stream) })).cast()
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
    match secured.stream.read(buffer) {
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
    match secured.stream.write_all(bytes).and_then(|()| secured.stream.flush()) {
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
    secured.stream.conn.send_close_notify();
    let _ = secured.stream.flush();
    // Dropping the `TcpStream` inside closes the socket.
}
