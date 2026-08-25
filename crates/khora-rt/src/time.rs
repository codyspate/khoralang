//! Clocks. One that a user can set and one that only goes forward.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

//
// ISO C offers `time`, which is whole seconds, and nothing finer that is
// portable: milliseconds are `GetSystemTimeAsFileTime` on Windows and
// `clock_gettime` on Unix, two different calls with two different epochs and
// two different headers. Rust's `std::time` has already made that choice on
// every target this runtime builds for, so binding it here is cheaper and more
// correct than a `#[cfg]` ladder in Khora would be — and it is the reason this
// pair lives in the runtime rather than in `std/env_native.kh` beside `getenv`.
//
// **Two clocks, because they measure two different things**, and the effect
// exposes both rather than picking one. The wall clock is what a timestamp on a
// log line means; it can jump — NTP steps it, an administrator sets it, a
// virtual machine resumes with a stale one — and it can jump *backwards*, so a
// duration computed from two readings of it can be negative or wildly wrong.
// The monotonic clock cannot go backwards and is what "how long did this take"
// actually wants; it has no epoch anybody outside the process can name, so it
// is useless for a timestamp. Neither one substitutes for the other, and a
// single `millis` would silently be the wrong one half the time.

/// Milliseconds since 1970, from the wall clock.
///
/// Negative before 1970, which a machine with a dead battery will report and
/// which is a truer answer than clamping to zero would be.
///
/// Saturates rather than wrapping at the far end. `i64` milliseconds run out in
/// the year 292,278,994; a clock claiming to be past that is broken, and the
/// useful response is a number that is still ordered rather than one that has
/// gone negative.
#[unsafe(no_mangle)]
pub extern "C" fn khora_unix_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_millis().min(i64::MAX as u128) as i64,
        Err(before) => -(before.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// Milliseconds on a clock that only goes forwards.
///
/// The origin is the first call, so the first reading is zero and everything
/// after it is "how long since the program started asking". An arbitrary origin
/// is not a shortcut: a monotonic clock's zero is arbitrary on every platform,
/// and pinning it here means a Khora program never sees a boot-time or an
/// uptime that differs between targets.
///
/// `Instant` is `QueryPerformanceCounter` on Windows and `CLOCK_MONOTONIC` on
/// Unix, and Rust guarantees the difference of two of them never goes
/// backwards even where the underlying counter misbehaves across cores.
#[unsafe(no_mangle)]
pub extern "C" fn khora_monotonic_millis() -> i64 {
    /// Written once, by whichever fiber reads the clock first. `OnceLock`
    /// rather than a `static mut` because fibers are threads and the first
    /// read can genuinely be a race between two of them; they then agree on
    /// the winner's origin, which is the whole point of a shared timeline.
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = *ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_millis().min(i64::MAX as u128) as i64
}
