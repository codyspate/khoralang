//! Hashes, for the authentication a database driver has to speak.
//!
//! **Bound rather than written**, the same argument `docs/design/ecosystem.md`
//! makes about TLS. `ring` is already linked for rustls, so SHA-256, HMAC and
//! PBKDF2 are in every non-wasm binary already; what was missing was a way for
//! Khora to reach them. A driver that has to hash a password otherwise cannot
//! be written in Khora at all, which is what kept `packages/postgres` from
//! connecting to a default PostgreSQL install — the server asks for
//! SCRAM-SHA-256 and the driver could only refuse by name.
//!
//! **Not a general crypto surface, and deliberately narrow.** These three are
//! what SCRAM needs. Anything else — a cipher, a signature, a key exchange —
//! is a decision about what Khora promises to keep correct for a decade, and
//! belongs in a design note rather than in the runtime because a driver needed
//! it on a Tuesday.
//!
//! Every entry point takes and fills caller-owned buffers, so nothing here
//! allocates and nothing has to be freed. The lengths are the caller's
//! promise; see each function's safety note.

use ring::{digest, hmac, pbkdf2};
use std::num::NonZeroU32;

/// How many bytes a SHA-256 digest occupies.
///
/// Exported so Khora can size its own buffer without hard-coding 32 in a
/// second place — the pair have to agree, and a constant that is written twice
/// is a constant that eventually is not.
#[unsafe(no_mangle)]
pub extern "C" fn khora_hash_sha256_length() -> i64 {
    digest::SHA256_OUTPUT_LEN as i64
}

/// SHA-256 of `length` bytes at `input`, written to `out`.
///
/// # Safety
///
/// `input` must be valid for `length` bytes and `out` for at least
/// [`khora_hash_sha256_length`] bytes. Both are caller-owned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_hash_sha256(input: *const u8, length: i64, out: *mut u8) {
    if length < 0 || out.is_null() || (input.is_null() && length != 0) {
        return;
    }
    // SAFETY: the caller guarantees `length` readable bytes at `input`. An
    // empty hash is a legitimate question, and a null pointer with a zero
    // length is how C spells it, so that case reads from a dangling-but-unused
    // slice rather than being refused.
    let bytes = if length == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(input, length as usize) }
    };
    let digest = digest::digest(&digest::SHA256, bytes);
    // SAFETY: the caller guarantees the output buffer's size.
    unsafe {
        std::ptr::copy_nonoverlapping(digest.as_ref().as_ptr(), out, digest::SHA256_OUTPUT_LEN);
    }
}

/// HMAC-SHA-256 of `message` under `key`, written to `out`.
///
/// # Safety
///
/// `key` must be valid for `key_length` bytes, `message` for
/// `message_length`, and `out` for at least [`khora_hash_sha256_length`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_hash_hmac_sha256(
    key: *const u8,
    key_length: i64,
    message: *const u8,
    message_length: i64,
    out: *mut u8,
) {
    if key_length < 0 || message_length < 0 || out.is_null() {
        return;
    }
    // SAFETY: the caller's guarantee on both inputs; the zero cases are spelled
    // the same way as in `khora_hash_sha256` and for the same reason.
    let (key_bytes, message_bytes) = unsafe {
        (
            if key_length == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(key, key_length as usize)
            },
            if message_length == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(message, message_length as usize)
            },
        )
    };
    let signing = hmac::Key::new(hmac::HMAC_SHA256, key_bytes);
    let tag = hmac::sign(&signing, message_bytes);
    // SAFETY: the caller guarantees the output buffer's size.
    unsafe {
        std::ptr::copy_nonoverlapping(tag.as_ref().as_ptr(), out, digest::SHA256_OUTPUT_LEN);
    }
}

/// PBKDF2-HMAC-SHA-256 of `password` and `salt`, `iterations` rounds, written
/// to `out`.
///
/// Answers false for zero iterations rather than stopping the program: the
/// count arrives from a server in the SCRAM exchange, and a peer sending
/// nonsense should fail this connection rather than the process. `ring` treats
/// a zero count as a programming error and panics, which across an FFI
/// boundary would be a trap the Khora caller cannot catch.
///
/// # Safety
///
/// `password` must be valid for `password_length` bytes, `salt` for
/// `salt_length`, and `out` for `out_length`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_hash_pbkdf2_sha256(
    password: *const u8,
    password_length: i64,
    salt: *const u8,
    salt_length: i64,
    iterations: i64,
    out: *mut u8,
    out_length: i64,
) -> u8 {
    if password_length < 0 || salt_length < 0 || out_length <= 0 || out.is_null() {
        return 0;
    }
    let Ok(iterations) = u32::try_from(iterations) else {
        return 0;
    };
    let Some(iterations) = NonZeroU32::new(iterations) else {
        return 0;
    };
    // SAFETY: the caller's guarantee on all three buffers.
    unsafe {
        let password_bytes = if password_length == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(password, password_length as usize)
        };
        let salt_bytes = if salt_length == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(salt, salt_length as usize)
        };
        let into = std::slice::from_raw_parts_mut(out, out_length as usize);
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            iterations,
            salt_bytes,
            password_bytes,
            into,
        );
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6234's `abc`, so a wrong binding fails here rather than inside a
    /// handshake where the only symptom is a server saying no.
    #[test]
    fn sha256_of_abc_is_the_published_vector() {
        let mut out = [0u8; 32];
        let input = b"abc";
        // SAFETY: `input` is a live array and `out` is 32 bytes, which is
        // `khora_hash_sha256_length`.
        unsafe { khora_hash_sha256(input.as_ptr(), input.len() as i64, out.as_mut_ptr()) };
        let expected = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(out, expected);
    }

    /// RFC 4231 test case 2: the key is `Jefe`, the message `what do ya want
    /// for nothing?`.
    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        let mut out = [0u8; 32];
        let key = b"Jefe";
        let message = b"what do ya want for nothing?";
        // SAFETY: both inputs are live arrays passed with their own lengths,
        // and `out` is a full digest wide.
        unsafe {
            khora_hash_hmac_sha256(
                key.as_ptr(),
                key.len() as i64,
                message.as_ptr(),
                message.len() as i64,
                out.as_mut_ptr(),
            )
        };
        let expected = [
            0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e, 0x6a, 0x04, 0x24, 0x26, 0x08, 0x95,
            0x75, 0xc7, 0x5a, 0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83, 0x9d, 0xec, 0x58, 0xb9,
            0x64, 0xec, 0x38, 0x43,
        ];
        assert_eq!(out, expected);
    }

    /// RFC 7677's SCRAM-SHA-256 example: password `pencil`, salt
    /// `W22ZaJ0SNY7soEsUEjb6gQ==`, 4096 iterations. This is the exact
    /// computation the PostgreSQL handshake performs, so a driver that agrees
    /// with this vector agrees with the server.
    #[test]
    fn pbkdf2_matches_the_scram_example() {
        // Decoded from the RFC's base64 rather than transcribed: writing the
        // hex by hand put `0x4d` where `0x6d` belongs, and the test then
        // disagreed with `ring` over a defect that was entirely its own.
        let salt = [
            0x5b, 0x6d, 0x99, 0x68, 0x9d, 0x12, 0x35, 0x8e, 0xec, 0xa0, 0x4b, 0x14, 0x12, 0x36,
            0xfa, 0x81,
        ];
        let password = b"pencil";
        let mut out = [0u8; 32];
        // SAFETY: the password, salt and output are live arrays, each passed
        // with its own length.
        let ok = unsafe {
            khora_hash_pbkdf2_sha256(
                password.as_ptr(),
                password.len() as i64,
                salt.as_ptr(),
                salt.len() as i64,
                4096,
                out.as_mut_ptr(),
                out.len() as i64,
            )
        };
        assert_eq!(ok, 1);
        let expected = [
            0xc4, 0xa4, 0x95, 0x10, 0x32, 0x3a, 0xb4, 0xf9, 0x52, 0xca, 0xc1, 0xfa, 0x99, 0x44,
            0x19, 0x39, 0xe7, 0x8e, 0xa7, 0x4d, 0x6b, 0xe8, 0x1d, 0xdf, 0x70, 0x96, 0xe8, 0x75,
            0x13, 0xdc, 0x61, 0x5d,
        ];
        assert_eq!(out, expected);
    }

    /// Zero iterations comes back false rather than panicking across the FFI
    /// boundary: the count arrives from a peer.
    #[test]
    fn zero_iterations_is_refused_rather_than_a_trap() {
        let mut out = [0u8; 32];
        // SAFETY: one-byte literals and a live output buffer, each with its
        // own length.
        let ok = unsafe {
            khora_hash_pbkdf2_sha256(
                b"p".as_ptr(),
                1,
                b"s".as_ptr(),
                1,
                0,
                out.as_mut_ptr(),
                out.len() as i64,
            )
        };
        assert_eq!(ok, 0, "a zero iteration count must not reach `ring`");
    }
}
