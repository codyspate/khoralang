//! Digests, message authentication, comparison, encodings and the operating
//! system's random bytes: what `std::crypto` is bound to.
//!
//! **Bound rather than written.** `ring` is already linked for rustls, so
//! SHA-2, HMAC and PBKDF2 are in every non-wasm binary already. A program that
//! needed a webhook signature otherwise left Khora to get one: through
//! hand-written FFI to OpenSSL, or through a SHA-256 written out by hand, and
//! a hand-written digest is wrong in ways no test of its own catches.
//!
//! **What it costs is a promise.** Every function here is one Khora keeps
//! correct from now on, which is why the list is short and why ciphers,
//! signatures and key exchange are not on it; `std/crypto_native.kh` says why.
//!
//! Every entry point takes and fills caller-owned buffers, so nothing here
//! allocates a Khora object and nothing has to be freed. An output buffer
//! arrives with its size, and a size that does not match what the operation
//! writes stops the program: the sizes are computed on the Khora side, and a
//! disagreement between the two halves must be a trap rather than a write past
//! an array's end.

use ring::{digest, hmac, pbkdf2, rand::SecureRandom};
use std::num::NonZeroU32;
use subtle::ConstantTimeEq;

/// The bytes at `data`, or the empty slice when there are none.
///
/// # Safety
///
/// `data` must be valid for `length` bytes for as long as the slice is used,
/// unless `length` is zero. A null or dangling pointer with a zero length is
/// how an empty array is lent, and is not read.
unsafe fn input<'a>(data: *const u8, length: i64) -> &'a [u8] {
    if length <= 0 || data.is_null() {
        &[]
    } else {
        // SAFETY: the caller's guarantee, for a positive length and a
        // non-null pointer.
        unsafe { std::slice::from_raw_parts(data, length as usize) }
    }
}

/// The buffer at `data`, writable, or the empty slice when there are none.
///
/// # Safety
///
/// As [`input`], and nothing else may read or write the bytes while the slice
/// lives.
unsafe fn output<'a>(data: *mut u8, length: i64) -> &'a mut [u8] {
    if length <= 0 || data.is_null() {
        &mut []
    } else {
        // SAFETY: the caller's guarantee, for a positive length and a
        // non-null pointer.
        unsafe { std::slice::from_raw_parts_mut(data, length as usize) }
    }
}

/// Writes `bytes` into `out`, which must be exactly as long.
///
/// **What it prevents: a write past the end of a Khora array.** `std` sizes
/// the buffer from its own copy of the length, and the two copies are allowed
/// to disagree only loudly.
fn fill_exactly(out: &mut [u8], bytes: &[u8]) {
    if out.len() != bytes.len() {
        super::fatal("std::crypto sized an output buffer wrongly; this is a bug in std");
    }
    out.copy_from_slice(bytes);
}

/// How many bytes a SHA-256 digest occupies, so `std` sizes its buffer from
/// the same number `ring` writes.
#[unsafe(no_mangle)]
pub extern "C" fn khora_crypto_sha256_length() -> i64 {
    digest::SHA256_OUTPUT_LEN as i64
}

/// How many bytes a SHA-512 digest occupies.
#[unsafe(no_mangle)]
pub extern "C" fn khora_crypto_sha512_length() -> i64 {
    digest::SHA512_OUTPUT_LEN as i64
}

/// SHA-256 of `length` bytes at `data`, written to `out`.
///
/// # Safety
///
/// `data` must be valid for `length` bytes and `out` for `out_length`, and
/// `out_length` must be [`khora_crypto_sha256_length`].
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_sha256(data: *const u8, length: i64, out: *mut u8, out_length: i64) {
    // SAFETY: the caller's guarantee on both buffers.
    let (bytes, into) = unsafe { (input(data, length), output(out, out_length)) };
    fill_exactly(into, digest::digest(&digest::SHA256, bytes).as_ref());
}

/// SHA-512 of `length` bytes at `data`, written to `out`.
///
/// # Safety
///
/// As [`khora_crypto_sha256`], with [`khora_crypto_sha512_length`].
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_sha512(data: *const u8, length: i64, out: *mut u8, out_length: i64) {
    // SAFETY: the caller's guarantee on both buffers.
    let (bytes, into) = unsafe { (input(data, length), output(out, out_length)) };
    fill_exactly(into, digest::digest(&digest::SHA512, bytes).as_ref());
}

/// The HMAC of `message` under `key` with `algorithm`, into `out`.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
unsafe fn sign(
    algorithm: hmac::Algorithm,
    key: *const u8,
    key_length: i64,
    message: *const u8,
    message_length: i64,
    out: *mut u8,
    out_length: i64,
) {
    // SAFETY: the caller's guarantee on all three buffers.
    let (key, message, into) =
        unsafe { (input(key, key_length), input(message, message_length), output(out, out_length)) };
    fill_exactly(into, hmac::sign(&hmac::Key::new(algorithm, key), message).as_ref());
}

/// Whether `tag` is the HMAC of `message` under `key` with `algorithm`.
///
/// **`ring::hmac::verify`, and not a comparison written here.** It computes
/// the tag and compares in time that does not depend on where the two first
/// differ, which is the whole difference between checking a signature and
/// teaching an attacker it one byte at a time. A tag of the wrong length,
/// including an empty one, is refused.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
unsafe fn verify(
    algorithm: hmac::Algorithm,
    key: *const u8,
    key_length: i64,
    message: *const u8,
    message_length: i64,
    tag: *const u8,
    tag_length: i64,
) -> u8 {
    // SAFETY: the caller's guarantee on all three buffers.
    let (key, message, tag) =
        unsafe { (input(key, key_length), input(message, message_length), input(tag, tag_length)) };
    u8::from(hmac::verify(&hmac::Key::new(algorithm, key), message, tag).is_ok())
}

/// HMAC-SHA-256 of `message` under `key`, written to `out`.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it, and
/// `out_length` must be [`khora_crypto_sha256_length`].
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_hmac_sha256(
    key: *const u8,
    key_length: i64,
    message: *const u8,
    message_length: i64,
    out: *mut u8,
    out_length: i64,
) {
    // SAFETY: forwarded unchanged; the caller's guarantee covers it.
    unsafe { sign(hmac::HMAC_SHA256, key, key_length, message, message_length, out, out_length) }
}

/// HMAC-SHA-512 of `message` under `key`, written to `out`.
///
/// # Safety
///
/// As [`khora_crypto_hmac_sha256`], with [`khora_crypto_sha512_length`].
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_hmac_sha512(
    key: *const u8,
    key_length: i64,
    message: *const u8,
    message_length: i64,
    out: *mut u8,
    out_length: i64,
) {
    // SAFETY: forwarded unchanged; the caller's guarantee covers it.
    unsafe { sign(hmac::HMAC_SHA512, key, key_length, message, message_length, out, out_length) }
}

/// One if `tag` is the HMAC-SHA-256 of `message` under `key`, zero if not.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
#[unsafe(no_mangle)]
// SHARE: reads bytes; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_hmac_verify_sha256(
    key: *const u8,
    key_length: i64,
    message: *const u8,
    message_length: i64,
    tag: *const u8,
    tag_length: i64,
) -> u8 {
    // SAFETY: forwarded unchanged; the caller's guarantee covers it.
    unsafe { verify(hmac::HMAC_SHA256, key, key_length, message, message_length, tag, tag_length) }
}

/// One if `tag` is the HMAC-SHA-512 of `message` under `key`, zero if not.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
#[unsafe(no_mangle)]
// SHARE: reads bytes; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_hmac_verify_sha512(
    key: *const u8,
    key_length: i64,
    message: *const u8,
    message_length: i64,
    tag: *const u8,
    tag_length: i64,
) -> u8 {
    // SAFETY: forwarded unchanged; the caller's guarantee covers it.
    unsafe { verify(hmac::HMAC_SHA512, key, key_length, message, message_length, tag, tag_length) }
}

/// One if the two buffers hold the same bytes, zero if not, in time that does
/// not depend on where they first differ.
///
/// `subtle` rather than `ring`: `ring` marks its own comparison deprecated and
/// "with no promises regarding side channels", and `subtle` is the crate
/// rustls already uses for exactly this, so it is linked either way. **The
/// lengths are compared first and in the open**: buffers of different lengths
/// answer zero at once, so the time reveals whether the lengths matched. A
/// tag's length is public in every protocol this is for.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
#[unsafe(no_mangle)]
// SHARE: reads bytes; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_equal(a: *const u8, a_length: i64, b: *const u8, b_length: i64) -> u8 {
    // SAFETY: the caller's guarantee on both buffers.
    let (a, b) = unsafe { (input(a, a_length), input(b, b_length)) };
    a.ct_eq(b).unwrap_u8()
}

/// Fills `length` bytes at `out` from the operating system's CSPRNG.
///
/// `ring::rand::SystemRandom`: `getrandom` on Linux, `getentropy` on macOS,
/// `ProcessPrng` on Windows. **A failure stops the program.** There is no
/// fallback worth having: a key made from anything weaker is a key an attacker
/// can make too, and a caller handed an error here has nothing better to do
/// than stop.
///
/// # Safety
///
/// `out` must be valid for `length` bytes.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_random_fill(out: *mut u8, length: i64) {
    // SAFETY: the caller's guarantee.
    let into = unsafe { output(out, length) };
    if ring::rand::SystemRandom::new().fill(into).is_err() {
        super::fatal("the operating system's random number generator did not answer");
    }
}

/// PBKDF2-HMAC-SHA-256 of `password` and `salt`, `iterations` rounds, written
/// to `out`. Zero when the count is not one `ring` takes.
///
/// Answers zero for a zero or out-of-range count rather than stopping the
/// program: the count arrives from a server in the SCRAM exchange, and a peer
/// sending nonsense should fail its connection rather than the process. `ring`
/// treats a zero count as a programming error and panics, which across this
/// boundary would be a trap the Khora caller cannot catch.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_pbkdf2_sha256(
    password: *const u8,
    password_length: i64,
    salt: *const u8,
    salt_length: i64,
    iterations: i64,
    out: *mut u8,
    out_length: i64,
) -> u8 {
    let Some(iterations) = u32::try_from(iterations).ok().and_then(NonZeroU32::new) else {
        return 0;
    };
    if out_length <= 0 {
        return 0;
    }
    // SAFETY: the caller's guarantee on all three buffers.
    let (password, salt, into) =
        unsafe { (input(password, password_length), input(salt, salt_length), output(out, out_length)) };
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, iterations, salt, password, into);
    1
}

// --- encodings --------------------------------------------------------------
//
// Here rather than in Khora because a per-byte loop in Khora passes a bounds
// check and a cancellation point per byte, and these run over whole request
// bodies. The decoders are strict, because they are mostly fed signatures and
// keys: a decoder that skips what it does not recognize accepts two spellings
// of one tag, and an attacker gets to pick which one a log shows.

const HEX: &[u8; 16] = b"0123456789abcdef";
const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Writes `length` bytes at `data` as lowercase hex into `out`, which must be
/// twice as long.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_hex_encode(data: *const u8, length: i64, out: *mut u8, out_length: i64) {
    // SAFETY: the caller's guarantee on both buffers.
    let (bytes, into) = unsafe { (input(data, length), output(out, out_length)) };
    if into.len() != bytes.len() * 2 {
        super::fatal("std::crypto sized a hex buffer wrongly; this is a bug in std");
    }
    for (pair, byte) in into.chunks_exact_mut(2).zip(bytes) {
        pair[0] = HEX[usize::from(byte >> 4)];
        pair[1] = HEX[usize::from(byte & 0x0f)];
    }
}

/// The value of one hex digit, either case.
fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// Decodes `length` hex digits at `data` into `out`, which must be half as
/// long. Answers how many bytes were written, or -1 for an odd count or a
/// character that is not a hex digit, in which case `out` holds nothing useful.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_hex_decode(data: *const u8, length: i64, out: *mut u8, out_length: i64) -> i64 {
    // SAFETY: the caller's guarantee on both buffers.
    let (text, into) = unsafe { (input(data, length), output(out, out_length)) };
    if text.len() % 2 != 0 {
        return -1;
    }
    if into.len() != text.len() / 2 {
        super::fatal("std::crypto sized a hex buffer wrongly; this is a bug in std");
    }
    for (byte, pair) in into.iter_mut().zip(text.chunks_exact(2)) {
        match (nibble(pair[0]), nibble(pair[1])) {
            (Some(high), Some(low)) => *byte = (high << 4) | low,
            (None, _) | (_, None) => return -1,
        }
    }
    into.len() as i64
}

/// How long base64 of `length` bytes is.
///
/// Padded: every three bytes, or the one or two left over, become four
/// characters. Unpadded: the `=` are not written, so one or two bytes left over
/// become two or three characters.
#[unsafe(no_mangle)]
pub extern "C" fn khora_crypto_base64_length(length: i64, url_safe: u8) -> i64 {
    let whole = length / 3 * 4;
    match (length % 3, url_safe != 0) {
        (0, _) => whole,
        (_, false) => whole + 4,
        (rest, true) => whole + rest + 1,
    }
}

/// Writes `length` bytes at `data` as base64 into `out`, which must be
/// [`khora_crypto_base64_length`] long.
///
/// `url_safe` zero is RFC 4648 §4, `+` and `/` with `=` padding. Non-zero is
/// §5, `-` and `_`, **without padding**: the URL-safe alphabet is used in
/// URLs, JWTs and cookie values, where a `=` would itself need escaping.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_base64_encode(
    data: *const u8,
    length: i64,
    out: *mut u8,
    out_length: i64,
    url_safe: u8,
) {
    // SAFETY: the caller's guarantee on both buffers.
    let (bytes, into) = unsafe { (input(data, length), output(out, out_length)) };
    if into.len() as i64 != khora_crypto_base64_length(bytes.len() as i64, url_safe) {
        super::fatal("std::crypto sized a base64 buffer wrongly; this is a bug in std");
    }
    let table = if url_safe != 0 { URL_SAFE } else { STANDARD };
    let mut at = 0;
    for group in bytes.chunks(3) {
        let b0 = group[0];
        let b1 = group.get(1).copied().unwrap_or(0);
        let b2 = group.get(2).copied().unwrap_or(0);
        let digits = [b0 >> 2, ((b0 & 0x03) << 4) | (b1 >> 4), ((b1 & 0x0f) << 2) | (b2 >> 6), b2 & 0x3f];
        // One byte is two characters, two are three, three are four.
        let written = group.len() + 1;
        for digit in &digits[..written] {
            into[at] = table[usize::from(*digit)];
            at += 1;
        }
        if url_safe == 0 {
            for _ in written..4 {
                into[at] = b'=';
                at += 1;
            }
        }
    }
}

/// The six bits one base64 character stands for, in `table`'s alphabet.
fn sextet(table: &[u8; 64], character: u8) -> Option<u8> {
    table.iter().position(|c| *c == character).map(|p| p as u8)
}

/// Decodes `length` characters of base64 at `data` into `out`. Answers how
/// many bytes were written, or -1 for text that is not base64 in the variant
/// asked for.
///
/// **Strict, and one spelling per value.** Padded text must be a multiple of
/// four characters with its `=` where RFC 4648 puts them; unpadded text must
/// have none. A character outside the alphabet, whitespace included, is
/// refused, and so are leftover bits that are not zero: `Zh==` and `Zg==` would
/// otherwise both decode to `f`, and a signature with two spellings is one an
/// attacker can alter without invalidating.
///
/// # Safety
///
/// Each pointer must be valid for the length passed beside it, and `out` must
/// have room for at least three bytes per four characters.
#[unsafe(no_mangle)]
// SHARE: writes bytes into a buffer; takes no Khora object.
pub unsafe extern "C" fn khora_crypto_base64_decode(
    data: *const u8,
    length: i64,
    out: *mut u8,
    out_length: i64,
    url_safe: u8,
) -> i64 {
    // SAFETY: the caller's guarantee on both buffers.
    let (text, into) = unsafe { (input(data, length), output(out, out_length)) };
    let table = if url_safe != 0 { URL_SAFE } else { STANDARD };
    let body = if url_safe != 0 {
        text
    } else {
        if text.len() % 4 != 0 {
            return -1;
        }
        let padding = text.iter().rev().take(2).take_while(|c| **c == b'=').count();
        &text[..text.len() - padding]
    };
    if body.len() % 4 == 1 {
        return -1;
    }
    if into.len() < body.len() / 4 * 3 + 2 {
        super::fatal("std::crypto sized a base64 buffer wrongly; this is a bug in std");
    }
    let mut at = 0;
    for group in body.chunks(4) {
        let mut sextets = [0u8; 4];
        for (slot, character) in sextets.iter_mut().zip(group) {
            match sextet(table, *character) {
                Some(value) => *slot = value,
                None => return -1,
            }
        }
        let bytes = [
            (sextets[0] << 2) | (sextets[1] >> 4),
            (sextets[1] << 4) | (sextets[2] >> 2),
            (sextets[2] << 6) | sextets[3],
        ];
        let kept = group.len() - 1;
        // The bits after the last whole byte must be zero, or two texts
        // decode to one value.
        let leftover = match kept {
            1 => sextets[1] & 0x0f,
            2 => sextets[2] & 0x03,
            _ => 0,
        };
        if leftover != 0 {
            return -1;
        }
        into[at..at + kept].copy_from_slice(&bytes[..kept]);
        at += kept;
    }
    at as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn sha256(bytes: &[u8]) -> String {
        let mut out = [0u8; 32];
        // SAFETY: a live slice with its own length, and a 32-byte output.
        unsafe { khora_crypto_sha256(bytes.as_ptr(), bytes.len() as i64, out.as_mut_ptr(), 32) };
        hex(&out)
    }

    fn encode64(bytes: &[u8], url_safe: u8) -> String {
        let mut out = vec![0u8; khora_crypto_base64_length(bytes.len() as i64, url_safe) as usize];
        // SAFETY: a live slice, and an output sized by the length function.
        unsafe {
            khora_crypto_base64_encode(bytes.as_ptr(), bytes.len() as i64, out.as_mut_ptr(), out.len() as i64, url_safe)
        };
        String::from_utf8(out).expect("ASCII")
    }

    fn decode64(text: &str, url_safe: u8) -> Option<Vec<u8>> {
        let mut out = vec![0u8; text.len() + 2];
        // SAFETY: a live string and a buffer longer than any decoding of it.
        let n = unsafe {
            khora_crypto_base64_decode(text.as_ptr(), text.len() as i64, out.as_mut_ptr(), out.len() as i64, url_safe)
        };
        (n >= 0).then(|| out[..n as usize].to_vec())
    }

    /// FIPS 180-4's `abc` and the empty string, so a wrong binding fails here
    /// rather than inside a handshake where the only symptom is a server
    /// saying no.
    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    /// RFC 7677's SCRAM-SHA-256 example: password `pencil`, salt
    /// `W22ZaJ0SNY7soEsUEjb6gQ==`, 4096 iterations, which is the exact
    /// computation the PostgreSQL handshake performs.
    #[test]
    fn pbkdf2_matches_the_scram_example() {
        let salt = decode64("W22ZaJ0SNY7soEsUEjb6gQ==", 0).expect("the RFC's salt");
        let mut out = [0u8; 32];
        // SAFETY: the password, salt and output are live, each passed with
        // its own length.
        let ok = unsafe {
            khora_crypto_pbkdf2_sha256(b"pencil".as_ptr(), 6, salt.as_ptr(), salt.len() as i64, 4096, out.as_mut_ptr(), 32)
        };
        assert_eq!(ok, 1);
        assert_eq!(hex(&out), "c4a49510323ab4f952cac1fa99441939e78ea74d6be81ddf7096e87513dc615d");
    }

    /// Zero iterations comes back zero rather than panicking across the FFI
    /// boundary: the count arrives from a peer.
    #[test]
    fn zero_iterations_is_refused_rather_than_a_trap() {
        let mut out = [0u8; 32];
        // SAFETY: one-byte literals and a live output buffer, each with its
        // own length.
        let ok = unsafe { khora_crypto_pbkdf2_sha256(b"p".as_ptr(), 1, b"s".as_ptr(), 1, 0, out.as_mut_ptr(), 32) };
        assert_eq!(ok, 0, "a zero iteration count must not reach `ring`");
    }

    /// RFC 4648 §10, both alphabets.
    #[test]
    fn base64_matches_rfc_4648() {
        let vectors = [
            ("", "", ""),
            ("f", "Zg==", "Zg"),
            ("fo", "Zm8=", "Zm8"),
            ("foo", "Zm9v", "Zm9v"),
            ("foob", "Zm9vYg==", "Zm9vYg"),
            ("fooba", "Zm9vYmE=", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy", "Zm9vYmFy"),
        ];
        for (plain, padded, url) in vectors {
            assert_eq!(encode64(plain.as_bytes(), 0), padded);
            assert_eq!(encode64(plain.as_bytes(), 1), url);
            assert_eq!(decode64(padded, 0).as_deref(), Some(plain.as_bytes()));
            assert_eq!(decode64(url, 1).as_deref(), Some(plain.as_bytes()));
        }
        assert_eq!(encode64(&[0xfb, 0xff, 0xbf], 0), "+/+/");
        assert_eq!(encode64(&[0xfb, 0xff, 0xbf], 1), "-_-_");
    }

    /// Every length from 0 to 300 round-trips, which crosses every remainder
    /// against every alphabet many times over.
    #[test]
    fn every_length_round_trips() {
        let mut state = 0x1234_5678_u64;
        for length in 0..300 {
            let bytes: Vec<u8> = (0..length)
                .map(|_| {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (state >> 56) as u8
                })
                .collect();
            for url_safe in [0, 1] {
                assert_eq!(decode64(&encode64(&bytes, url_safe), url_safe).as_deref(), Some(&bytes[..]));
            }
        }
    }

    /// One spelling per value: the decoders refuse what a lenient one accepts.
    #[test]
    fn base64_refuses_the_second_spellings() {
        assert_eq!(decode64("Zh==", 0), None, "non-zero leftover bits");
        assert_eq!(decode64("Zg", 0), None, "standard base64 without its padding");
        assert_eq!(decode64("Zg==", 1), None, "padding on the URL-safe variant");
        assert_eq!(decode64("Zm9v\n", 0), None, "whitespace");
        assert_eq!(decode64("Z===", 0), None, "too much padding");
        assert_eq!(decode64("Zm9v+/+/", 1), None, "the other alphabet");
        assert_eq!(decode64("Z", 1), None, "a single character is no byte");
    }

    #[test]
    fn hex_round_trips_and_refuses_what_is_not_hex() {
        let decode = |text: &str| {
            let mut out = vec![0u8; text.len() / 2];
            // SAFETY: a live string and a buffer half its length.
            let n = unsafe { khora_crypto_hex_decode(text.as_ptr(), text.len() as i64, out.as_mut_ptr(), out.len() as i64) };
            (n >= 0).then_some(out)
        };
        assert_eq!(decode("00ff7FaB"), Some(vec![0x00, 0xff, 0x7f, 0xab]));
        assert_eq!(decode(""), Some(vec![]));
        assert_eq!(decode("abc"), None, "an odd count");
        assert_eq!(decode("zz"), None, "not a digit");
        let mut out = [0u8; 8];
        // SAFETY: a live four-byte input and an eight-byte output.
        unsafe { khora_crypto_hex_encode([0u8, 0xff, 0x7f, 0xab].as_ptr(), 4, out.as_mut_ptr(), 8) };
        assert_eq!(&out, b"00ff7fab");
    }

    #[test]
    fn equal_answers_by_content_and_length() {
        // SAFETY, for every call below: literals passed with their own lengths.
        let eq = |a: &[u8], b: &[u8]| unsafe { khora_crypto_equal(a.as_ptr(), a.len() as i64, b.as_ptr(), b.len() as i64) };
        assert_eq!(eq(b"abc", b"abc"), 1);
        assert_eq!(eq(b"abc", b"abd"), 0);
        assert_eq!(eq(b"abc", b"ab"), 0);
        assert_eq!(eq(b"", b""), 1);
    }

    #[test]
    fn random_fill_fills() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        // SAFETY: two live 32-byte buffers.
        unsafe {
            khora_crypto_random_fill(a.as_mut_ptr(), 32);
            khora_crypto_random_fill(b.as_mut_ptr(), 32);
        }
        // 2^-256 of a false failure, which is no chance at all.
        assert_ne!(a, b);
    }
}
