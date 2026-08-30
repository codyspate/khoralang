//! Where a program stops because it was wrong.
//!
//! Arithmetic that overflowed and an index outside its array. Both are the
//! same decision, taken in `docs/design/numbers.md`: a program that runs off
//! the end of its own array is wrong, and continuing with whatever was next in
//! memory is the least useful possible response.

use std::io::Write;

/// Reports an error that reached the entry point, and does not stop.
///
/// **A program that fails said nothing at all.** An uncaught raise out of
/// `main` exited 1 with both streams empty, which is the least useful thing a
/// failure can do: the first person to hit it had `main() raises IoError`, ran
/// it on a missing file, got no output whatsoever, and went looking at their
/// own `print` calls. The payload was right there and carried the path.
///
/// Not `_Noreturn`, and not a trap. The program is *ending*, correctly, with
/// the exit status an entry point that raised is supposed to have; this only
/// says why on the way out. So there is no backtrace hint and no talk of a
/// bug, because neither is true -- an error nobody handled is a program
/// behaving as written.
///
/// The type's name rather than the value. Generated code knows which error
/// type the tag stands for and could reach a `Show` for it, but only if one
/// exists and only by forcing an instance nothing else asked for; the name is
/// always available and answers the question that was actually being asked,
/// which is what happened rather than to what.
///
/// # Safety
///
/// `name` must point at `len` bytes naming the error type -- generated code
/// passes a string literal in `.rodata`, live for the program's whole run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_unhandled(name: *const u8, len: u64) {
    let bytes = if len == 0 {
        &[][..]
    } else {
        // SAFETY: the caller's contract, above.
        unsafe { std::slice::from_raw_parts(name, len as usize) }
    };
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "khora: `{}` reached the entry point and nothing handled it",
        String::from_utf8_lossy(bytes)
    );
    let _ = writeln!(
        err,
        "note: `main` has nowhere to hand an error, so the program ends here \
         with status 1. Catch it in `main`, or return a `Result`"
    );
}

/// Reports arithmetic that did not fit, and stops.
///
/// Overflow traps in every build. A program that passes its tests and then
/// wraps in production is the failure worth spending a branch to prevent, and
/// two behaviours — one for testing, one for shipping — put the difference
/// exactly where it is most expensive to find. `docs/roadmap.md` 6.2.
///
/// `Int::wrapping_add` and its siblings are how you ask for the other thing,
/// in the places that genuinely want it: a hash, a checksum, a PRNG.
///
/// # Safety
///
/// `what` must point at `len` bytes saying what happened, as a whole
/// sentence: it is printed as given, and the runtime adds only the fiber
/// clause and the backtrace note. Generated code passes a string literal in
/// `.rodata`, live for the program's whole run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_overflow(what: *const u8, len: u64) -> ! {
    let bytes = if len == 0 { &[][..] } else {
        // SAFETY: the caller passes a string literal in `.rodata`, live for the
        // program's whole run.
        unsafe { std::slice::from_raw_parts(what, len as usize) }
    };
    // **The block matters, and it is not style.** On Windows `longjmp` is a
    // real unwind: it runs SEH cleanup on every frame between the jump and the
    // landing point, which includes the destructors Rust emits. A `StderrLock`
    // held across the jump aborted the process with "panic in a function that
    // cannot unwind" — containment working exactly as designed and the host
    // dying anyway. So everything with a `Drop` is confined to a block that
    // ends before `stop` is reached.
    let contained = {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "khora: {}{}",
            String::from_utf8_lossy(bytes),
            on_which_fiber()
        );
        where_from(&mut err);
        say_what_happens_next(&mut err)
    };
    stop(contained)
}

/// Reports an index that was not in range, and stops.
///
/// A trap rather than a wrapped value or a poisoned read, for the same reason
/// integer overflow traps: a program that reads past its own array is wrong,
/// and the useful thing to do is say where.
#[unsafe(no_mangle)]
pub extern "C" fn khora_bounds_fail(index: i64, len: i64) -> ! {
    // The same block, for the same reason as `khora_overflow`.
    let contained = {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "khora: index {index} is outside an array of {len}{}",
            on_which_fiber()
        );
        where_from(&mut err);
        say_what_happens_next(&mut err)
    };
    stop(contained)
}

/// Decides what happens after the message, does the freeing, and says so.
///
/// Returns whether the call is being contained. **Everything that allocates or
/// holds a lock happens here**, inside the caller's block, so that by the time
/// `stop` jumps there is nothing left for an unwind to run.
///
/// **The default is unchanged**: a trap ends the process, which is what
/// `docs/design/traps.md` decided and what a program wants. Containment
/// happens only when a host has asked for it with `khora_set_trap_policy` and
/// only inside an exported call, where `crate::contain` documents why
/// discarding every allocation the call made is sound.
///
/// The message is printed either way. A contained trap is still a bug, and a
/// library that swallowed one in silence would be worse than one that died.
fn say_what_happens_next(err: &mut impl Write) -> bool {
    if !crate::contain::can_contain() {
        return false;
    }
    let freed = crate::contain::discard();
    let _ = writeln!(
        err,
        "khora: the call was discarded and {freed} object(s) released; the host is still \
         running. `khora_trapped()` reports this until `khora_clear_trap()`"
    );
    true
}

/// Leaves, one way or the other.
///
/// Takes a `bool` rather than doing the deciding, because on Windows `longjmp`
/// is a real unwind — it runs SEH cleanup on every frame between here and the
/// landing point — and this frame must have nothing for it to run. See the
/// comment in [`khora_overflow`].
fn stop(contained: bool) -> ! {
    let _ = std::io::stdout().flush();
    if contained {
        crate::contain::khora_guard_jump()
    }
    std::process::exit(134)
}

/// Which fiber trapped, when it is not the one the program started on.
///
/// **A trap takes the whole process down** — see `docs/design/traps.md` for why
/// that is the current answer and what containing it would cost. Until it is
/// contained, the least a server can be told is which of its concurrent pieces
/// of work was the one that was wrong: on a machine handling a thousand
/// requests, "some addition overflowed" and "fiber 4,102's addition overflowed"
/// are a different amount of help, because the second can be matched against a
/// request log.
///
/// Empty on the root fiber, where there is nothing to disambiguate and the
/// number would be noise on every single-threaded program's worst day.
#[cfg(not(target_family = "wasm"))]
fn on_which_fiber() -> String {
    crate::current::current(|fiber| {
        if fiber.is_spawned() {
            format!(" on fiber {}", fiber.id())
        } else {
            String::new()
        }
    })
}

/// Nothing to disambiguate: a Worker has one instance and no fibers in it.
#[cfg(target_family = "wasm")]
fn on_which_fiber() -> String {
    String::new()
}

/// Prints where the trap came from, if the program was built to know.
///
/// **This is the half of a trap that was missing.** `khora_bounds_fail`'s own
/// doc comment said the useful thing to do is say where, and until the compiler
/// emitted line tables there was no way for it to. Both messages named what had
/// happened, in a program of any size, with nothing to connect it to a line.
///
/// Off unless asked for. A trap is a bug and the first thing anybody does with
/// a bug is re-run it, so a switch costs one attempt and a default costs every
/// well-behaved program a page of stack on the way out.
///
/// **`KHORA_BACKTRACE`, and `RUST_BACKTRACE` as well.** It used to be only the
/// second, on the argument that it is the switch every Rust binary on the
/// machine already answers to — which is true of a machine that has Rust
/// binaries on it, and a Khora user may not. Being told to set a variable named
/// after a different language reads as a leak of what the compiler happens to
/// be written in, and it is one.
///
/// So the message names the Khora one, and both are honoured: somebody who
/// already exports `RUST_BACKTRACE=1` for everything still gets a backtrace
/// without being asked twice.
///
/// The frames are symbolized from the debug information the executable carries,
/// so this is only as good as `KHORA_DEBUG` left it. With debug info off the
/// backtrace is addresses, which is worth printing anyway: an address plus the
/// binary is still something a symbolizer can be pointed at later.
fn where_from(err: &mut impl Write) {
    let asked = std::env::var_os("KHORA_BACKTRACE").is_some()
        || std::env::var_os("RUST_BACKTRACE").is_some();
    if !asked {
        let _ = writeln!(err, "note: re-run with KHORA_BACKTRACE=1 to see where");
        return;
    }
    let captured = std::backtrace::Backtrace::force_capture().to_string();
    let _ = writeln!(err, "{}", from_khora_down(&captured));
}

/// Drops the runtime's own frames from the top of a captured backtrace.
///
/// Six frames of `backtrace_rs` and `Backtrace::force_capture` sit above the
/// line that actually overflowed, and the top of a backtrace is the part
/// anybody reads first. What is wanted is the Khora frame that trapped, on the
/// first line.
///
/// **A text filter over the formatted backtrace**, which is worth being plain
/// about: `std::backtrace` exposes no way to skip frames, and depending on the
/// `backtrace` crate directly to get one would put an unwinder in the runtime's
/// dependency graph to save six lines. The rule is to cut after the last frame
/// naming this module, and if that frame is ever not found the whole capture is
/// returned unchanged — so the failure mode is the noisy output this replaced,
/// never a backtrace with something real missing from it.
fn from_khora_down(captured: &str) -> &str {
    const MINE: &str = "khora_rt::trap::";
    let Some(last) = captured.rfind(MINE) else { return captured };
    // The frame *after* the trap handler: the next line that **opens** a
    // frame. A frame's own `at <file>:<line>` line is indented too, so
    // "newline then spaces" finds the wrong one and cuts a frame in half —
    // what distinguishes an opener is the number.
    let start = captured[last..].match_indices('\n').find_map(|(i, _)| {
        let after = last + i + 1;
        opens_a_frame(&captured[after..]).then_some(after)
    });
    match start {
        Some(start) => &captured[start..],
        None => captured,
    }
}

/// Whether `line` begins a numbered backtrace frame — spaces, digits, `:`.
fn opens_a_frame(line: &str) -> bool {
    let rest = line.trim_start_matches(' ');
    let digits = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    digits.len() < rest.len() && digits.starts_with(':')
}

#[cfg(test)]
mod tests {
    use super::from_khora_down;

    /// A capture with the shape the real one has: the runtime's frames, the
    /// trap handler, then the Khora frames that are the whole point.
    const CAPTURE: &str = concat!(
        "   0: std::backtrace_rs::backtrace::win64::trace\n",
        "             at library/std/src/backtrace.rs:85\n",
        "   5: khora_rt::trap::khora_overflow\n",
        "             at ./crates/khora-rt/src/trap.rs:33\n",
        "   6: deep\n",
        "             at main.kh:6\n",
        "   7: main\n",
        "             at main.kh:15\n",
    );

    #[test]
    fn the_runtimes_own_frames_come_off_the_top() {
        let trimmed = from_khora_down(CAPTURE);
        assert!(trimmed.starts_with("   6: deep"), "got {trimmed:?}");
        assert!(trimmed.contains("main.kh:15"), "the rest is kept");
        assert!(!trimmed.contains("backtrace_rs"), "the runtime's frames are gone");
    }

    /// The one thing this must never do is eat a frame it did not recognise.
    #[test]
    fn a_capture_without_the_trap_handler_is_left_alone() {
        let other = "   0: something\n             at elsewhere.rs:1\n";
        assert_eq!(from_khora_down(other), other);
    }

    /// A trap frame with nothing after it — the whole program was one function
    /// — leaves the capture rather than returning an empty string.
    #[test]
    fn a_trap_with_no_frames_below_it_is_left_alone() {
        let only = "   5: khora_rt::trap::khora_overflow\n             at trap.rs:33\n";
        assert_eq!(from_khora_down(only), only);
    }
}
