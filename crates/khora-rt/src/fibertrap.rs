//! Containing a trap at a `contain` frame on the trapping fiber.
//!
//! **What this prevents: one bad request ending every request a server is
//! serving.** A trap -- an overflow, an index out of range, a `todo()` --
//! ended the process, so a handler that overflowed on one hostile amount took
//! down the forty requests in flight beside it. `std::core::contain` turns a
//! trap in its body into a value, on the trapping fiber only.
//!
//! **Nothing here frees anything.** A contained trap leaves every frame
//! between the trap and the `contain` through the tagged return a
//! cancellation already takes, under its own tag, [`crate::TRAPPED_WHICH`]:
//! each frame releases what it owns, its regions run their finalizers, and a
//! `Shared::update` change function that comes back with a tag leaves the
//! cell as it was (`crate::shared::khora_shared_update`). This module only
//! decides *whether* a trap is contained, and keeps what `contain` hands back.
//!
//! # When a trap is contained
//!
//! Only when **all** of these hold; otherwise it ends the process with status
//! 134, exactly as a trap does with no `contain` anywhere:
//!
//! - the fiber has a `contain` frame open. The depth is the fiber's own, so a
//!   child spawned inside a `contain` is not covered by its parent's and needs
//!   one of its own;
//! - the fiber is not running a finalizer. The runtime ignores the tag a
//!   finalizer returns, so a trap there would cut cleanup short in silence;
//! - the trap site was compiled to leave. The compiler calls the `*_contain`
//!   entries below only from a frame that has a tag to leave with; the `-> !`
//!   entries in `crate::trap` keep their meaning;
//! - the process has not spent its trap budget ([`spend_the_budget`]).
//!
//! Never contained, whatever is on the stack: running out of stack, a
//! compiler invariant (`trap_fatal` in the code generator), and the runtime's
//! own `fatal(..)`, which says the runtime's state cannot be trusted.
//!
//! **What it costs.** A trap site is a way out of its frame, so a function
//! with checked arithmetic, an index or a narrowing carries a tag where it
//! did not, and its callers branch on it. The exit itself is a cold block; a
//! runtime call that can trap (`Decimal` arithmetic) is followed by one load
//! of [`khora_trap_pending`] and a branch.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(not(target_family = "wasm"))]
use crate::current::current;

/// Runtime traps contained and not yet taken by the code that called them.
///
/// **What this prevents: a call into the runtime per `Decimal` operation to
/// ask whether it trapped.** A runtime function that hits a trap inside a
/// `contain` answers a meaningless value and sets its thread's flag
/// ([`crate::decimal`] keeps it, beside the high word); the generated code
/// after it has to leave before using that value. Asking the thread-local
/// directly is a call each time, which on a `Decimal::add` loop was 13% of
/// the loop. So generated code reads this word with one relaxed load and asks
/// [`khora_trap_taken`] only when it is not zero, which is the instant
/// between a contained runtime trap and the line after it.
///
/// Exported under this name because generated code loads it directly.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static khora_trap_pending: AtomicU64 = AtomicU64::new(0);

/// How many traps the process may contain in [`BUDGET_WINDOW`] before the
/// next one is fatal, unless `KHORA_TRAP_BUDGET` says otherwise.
const BUDGET_TRAPS: usize = 1000;

/// The window [`BUDGET_TRAPS`] is counted over.
const BUDGET_WINDOW: Duration = Duration::from_secs(60);

/// The budget, read once: `KHORA_TRAP_BUDGET` as `N` (traps per 60 s) or
/// `N/SECONDS`, or the default. `0` contains nothing.
///
/// **A value that does not parse says so, once.** It falls back to the
/// default, which is what the documentation promises; but a typo in a safety
/// limit -- `100/0`, `-1`, `1e3` -- silently became 1,000 a minute, and the
/// operator who set it believed the limit they wrote. One line on stderr
/// names the value and what stands instead, the first time the budget is
/// asked: at the first contained trap, not at start-up, because a program
/// that never traps never reads it.
fn budget() -> (usize, Duration) {
    static READ: std::sync::OnceLock<(usize, Duration)> = std::sync::OnceLock::new();
    *READ.get_or_init(|| {
        let Ok(text) = std::env::var("KHORA_TRAP_BUDGET") else {
            return (BUDGET_TRAPS, BUDGET_WINDOW);
        };
        parse_budget(&text).unwrap_or_else(|| {
            let _ = std::io::stdout().flush();
            let _ = writeln!(std::io::stderr(), "{}", unreadable_budget(&text));
            (BUDGET_TRAPS, BUDGET_WINDOW)
        })
    })
}

/// What [`budget`] says about a `KHORA_TRAP_BUDGET` it cannot read.
fn unreadable_budget(text: &str) -> String {
    format!(
        "khora: KHORA_TRAP_BUDGET={text:?} is not a count (`N`) or a count over seconds \
         (`N/SECONDS`), so the default stands: {BUDGET_TRAPS} traps in any {} s",
        BUDGET_WINDOW.as_secs()
    )
}

/// `N` or `N/SECONDS`, both whole numbers.
fn parse_budget(text: &str) -> Option<(usize, Duration)> {
    let (count, seconds) = match text.trim().split_once('/') {
        Some((count, seconds)) => (count, seconds.trim().parse::<u64>().ok()?),
        None => (text, BUDGET_WINDOW.as_secs()),
    };
    if seconds == 0 {
        return None;
    }
    Some((count.trim().parse().ok()?, Duration::from_secs(seconds)))
}

/// When each trap contained in the current window was contained, oldest
/// first. At most the budget's count long.
static SPENT: Mutex<std::collections::VecDeque<Instant>> =
    Mutex::new(std::collections::VecDeque::new());

/// Takes one trap from the budget, or says it is spent.
///
/// **What this prevents: a bug that degrades to 500s forever.** A contained
/// trap is still a bug, and one a request can reach repeatedly is one a
/// supervisor should see. Past the budget the next trap ends the process with
/// 134, as an uncontained one does, and says why.
///
/// Process-wide and exact over a sliding window: the instant of each of the
/// last `N` traps is kept, so `N` traps in any `window` are allowed and the
/// next is not. A lock on the trap path only.
fn spend_the_budget() -> bool {
    let (most, window) = budget();
    let now = Instant::now();
    let mut spent = SPENT.lock().unwrap_or_else(|held| held.into_inner());
    while spent.front().is_some_and(|at| now.duration_since(*at) >= window) {
        spent.pop_front();
    }
    if spent.len() >= most {
        return false;
    }
    spent.push_back(now);
    true
}

/// Whether a trap on this fiber, here, may be contained: a `contain` frame of
/// its own is open and it is not running a finalizer. Spends nothing.
#[cfg(not(target_family = "wasm"))]
fn covered() -> bool {
    current(|fiber| fiber.contained().load(Ordering::Relaxed) > 0 && !fiber.is_shielded())
}

/// **Never, on wasm.** A Worker restarts the isolate, which is the
/// containment boundary there (`crate::contain` says why), and a module has
/// no fibers to keep a depth on. `contain` runs its body and a trap in it
/// ends the instance.
#[cfg(target_family = "wasm")]
fn covered() -> bool {
    false
}

/// Whether to contain this trap: covered, and the budget allows it. Says why
/// on stderr when only the budget refuses, before the fatal path's message.
fn contains(sentence: &str) -> bool {
    if !covered() {
        return false;
    }
    if spend_the_budget() {
        return true;
    }
    let (most, window) = budget();
    let _ = std::io::stdout().flush();
    let _ = writeln!(
        std::io::stderr(),
        "khora: {most} traps were contained in the last {} s, which is the trap budget \
         (KHORA_TRAP_BUDGET); this one ends the process: {sentence}",
        window.as_secs()
    );
    false
}

/// Reports a contained trap and keeps its sentence for `contain`.
///
/// The same sentence the fatal path prints, and the fiber, so a log reads the
/// same either way and can be matched against a request log.
#[cfg(not(target_family = "wasm"))]
fn record(sentence: &str) {
    {
        let _ = std::io::stdout().flush();
        let fiber = crate::trap::on_which_fiber();
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "khora: {sentence}{fiber} (contained; the fiber continues)");
    }
    current(|fiber| {
        let mut held = fiber.trap_message().lock().unwrap_or_else(|held| held.into_inner());
        held.clear();
        held.push_str(sentence);
    });
}

/// Unreachable on wasm, where nothing is [`covered`].
#[cfg(target_family = "wasm")]
fn record(_sentence: &str) {}

/// Reads the last contained sentence on this fiber.
#[cfg(not(target_family = "wasm"))]
fn with_sentence<T>(read: impl FnOnce(&str) -> T) -> T {
    current(|fiber| read(&fiber.trap_message().lock().unwrap_or_else(|held| held.into_inner())))
}

/// None on wasm.
#[cfg(target_family = "wasm")]
fn with_sentence<T>(read: impl FnOnce(&str) -> T) -> T {
    read("")
}

/// A runtime function's trap: `true` when it is contained, and the caller
/// answers a meaningless value that the generated code after it never reads.
pub(crate) fn runtime_trap(sentence: &str) -> bool {
    if !contains(sentence) {
        return false;
    }
    record(sentence);
    true
}

/// Opens a `contain` frame on this fiber. Nothing on wasm.
#[unsafe(no_mangle)]
pub extern "C" fn khora_contain_enter() {
    #[cfg(not(target_family = "wasm"))]
    current(|fiber| fiber.contained().fetch_add(1, Ordering::Relaxed));
}

/// Closes one. Saturating, for the reason `Fiber::unshield` gives.
#[unsafe(no_mangle)]
pub extern "C" fn khora_contain_exit() {
    #[cfg(not(target_family = "wasm"))]
    current(|fiber| {
        let depth = fiber.contained().load(Ordering::Relaxed);
        fiber.contained().store(depth.saturating_sub(1), Ordering::Relaxed);
    });
}

/// Arithmetic or a conversion that did not fit, at a site compiled to leave.
///
/// Returns only when the trap is contained, and the caller then leaves with
/// [`crate::TRAPPED_WHICH`]. Otherwise this is `khora_overflow`.
///
/// # Safety
///
/// As `khora_overflow`: `what` points at `len` bytes, live for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_overflow_contain(what: *const u8, len: u64) {
    let bytes = if len == 0 {
        &[][..]
    } else {
        // SAFETY: the caller's contract, above: a string literal in `.rodata`.
        unsafe { std::slice::from_raw_parts(what, len as usize) }
    };
    let sentence = String::from_utf8_lossy(bytes);
    if !contains(&sentence) {
        // SAFETY: the caller's contract is `khora_overflow`'s.
        unsafe { crate::trap::khora_overflow(what, len) }
    }
    record(&sentence);
}

/// An index out of range, at a site compiled to leave. As
/// [`khora_overflow_contain`].
#[unsafe(no_mangle)]
pub extern "C" fn khora_bounds_contain(index: i64, len: i64) {
    let sentence = format!("index {index} is outside an array of {len}");
    if !contains(&sentence) {
        crate::trap::khora_bounds_fail(index, len)
    }
    record(&sentence);
}

/// `todo()`, at a site compiled to leave. As [`khora_overflow_contain`].
#[unsafe(no_mangle)]
pub extern "C" fn khora_todo_contain() {
    if !runtime_trap("this is not written yet") {
        crate::trap::khora_todo()
    }
}

/// `race` with no branches, at a site compiled to leave. As
/// [`khora_overflow_contain`].
#[unsafe(no_mangle)]
pub extern "C" fn khora_race_empty_contain() {
    if !runtime_trap("`race` was given no branches, so there is nothing to wait for") {
        crate::trap::khora_race_empty()
    }
}

/// Whether the runtime call just made contained a trap. Clears it.
///
/// Asked only when [`khora_trap_pending`] is not zero, and on the line after
/// the call: nothing between the two can suspend, so the thread-local answers
/// for this fiber.
#[unsafe(no_mangle)]
pub extern "C" fn khora_trap_taken() -> u8 {
    let taken = crate::decimal::take_trapped();
    if taken {
        khora_trap_pending.fetch_sub(1, Ordering::Relaxed);
    }
    u8::from(taken)
}

/// Marks this thread's last runtime call as trapped, for [`khora_trap_taken`].
pub(crate) fn pend() {
    crate::decimal::mark_trapped();
    khora_trap_pending.fetch_add(1, Ordering::Relaxed);
}

/// The length of the last contained trap's sentence on this fiber, in bytes.
#[unsafe(no_mangle)]
pub extern "C" fn khora_trap_message_length() -> i64 {
    with_sentence(|held| held.len() as i64)
}

/// Copies the last contained trap's sentence on this fiber into `into`, and
/// answers how many bytes it wrote.
///
/// # Safety
///
/// `into` must address `capacity` writable bytes.
///
/// SHARE: `into` is a byte buffer the caller lent for this call; nothing is
/// published, only bytes written into it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_trap_message_copy(into: *mut u8, capacity: i64) -> i64 {
    with_sentence(|held| {
        let n = held.len().min(usize::try_from(capacity).unwrap_or(0));
        // SAFETY: the caller's contract, and `n` is within it; the source is
        // the fiber's own string, which cannot overlap a Khora array.
        unsafe { into.copy_from_nonoverlapping(held.as_ptr(), n) };
        n as i64
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two spellings `KHORA_TRAP_BUDGET` takes, and the ones it refuses
    /// in favor of the default.
    #[test]
    fn a_budget_is_a_count_or_a_count_over_seconds() {
        assert_eq!(parse_budget("5"), Some((5, Duration::from_secs(60))));
        assert_eq!(parse_budget(" 7/2 "), Some((7, Duration::from_secs(2))));
        assert_eq!(parse_budget("0"), Some((0, Duration::from_secs(60))));
        assert_eq!(parse_budget("3/0"), None, "no window is no budget at all");
        assert_eq!(parse_budget("many"), None);
        assert_eq!(parse_budget("-1"), None);
        assert_eq!(parse_budget("2/1x"), None);
        assert_eq!(parse_budget("18446744073709551616"), None, "past a usize");
    }

    /// The sentence names the value as written and the default that stands.
    #[test]
    fn an_unreadable_budget_names_itself_and_the_default() {
        assert_eq!(
            unreadable_budget("3/0"),
            "khora: KHORA_TRAP_BUDGET=\"3/0\" is not a count (`N`) or a count over seconds \
             (`N/SECONDS`), so the default stands: 1000 traps in any 60 s"
        );
    }

    /// Outside any `contain`, nothing is contained and nothing is spent.
    #[test]
    fn a_trap_with_no_contain_frame_is_not_covered() {
        assert!(!covered());
        khora_contain_enter();
        assert!(covered());
        khora_contain_exit();
        assert!(!covered());
        // Saturating: an exit with nothing open does not wrap round to
        // \"covered for ever\".
        khora_contain_exit();
        assert!(!covered());
    }

    /// The sentence `contain` hands back is the last one recorded on this
    /// fiber, copied out whole.
    #[test]
    fn the_last_sentence_is_the_one_handed_back() {
        record("first");
        record("Int addition overflowed");
        let length = khora_trap_message_length();
        assert_eq!(length, 23);
        let mut out = vec![0u8; 40];
        // SAFETY: `out` holds 40 writable bytes.
        let wrote = unsafe { khora_trap_message_copy(out.as_mut_ptr(), 40) };
        assert_eq!(&out[..wrote as usize], b"Int addition overflowed");
    }
}
