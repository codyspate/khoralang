//! Starting another program without a shell between.
//!
//! `std::process` reached the machine through ISO C's `system` and `popen`,
//! which hand a *command line* to `cmd.exe` or `/bin/sh` and let it decide what
//! the words mean. That is fine when the line is a literal and is a command
//! injection when any part of it came from somewhere else: a file name with a
//! `;` in it ends the command and starts another one, and no amount of quoting
//! in Khora fixes it because the two shells disagree about what quoting means.
//!
//! `std/process_native.kh` said so in its own header and named the fix —
//! *"`spawn(program, arguments)`, where the arguments are a list Khora holds
//! and no shell ever parses"* — and said it needed `CreateProcess` on Windows
//! and `posix_spawn` on Unix, neither reachable from Khora because errata 35
//! says no struct crosses the C ABI.
//!
//! It is reachable from *here*. Rust's `std::process::Command` is those two
//! calls with one interface over them, and this file is the twenty lines that
//! carry a list of arguments across.
//!
//! # What crosses
//!
//! One buffer and a count. Each argument is NUL-terminated inside the buffer,
//! program first, so `["git", "log", "--oneline"]` arrives as
//! `git\0log\0--oneline\0` with a count of 3. A length-prefixed list would need
//! a second array and an agreement about its width; NUL is what the platform
//! already uses for exactly this and there is nothing to disagree about.
//!
//! **A NUL cannot appear inside an argument**, which is true of the operating
//! system too — `execve` takes NUL-terminated strings, so an argument
//! containing one is not expressible on any target this runs on. The split
//! here loses nothing that could have been passed anyway.
//!
//! # Why the output comes back through a handle
//!
//! A captured stdout is bytes of a length nobody knows until the child has
//! finished, and generated code cannot allocate against a length it has not
//! been told. So the run leaves its result here, answers how big it is, and
//! the caller comes back for it with somewhere to put it. Three calls rather
//! than one, and no `Array<U8>` built by a hand that does not know the layout.

use std::process::Command;

/// What a finished child left behind.
///
/// The status is not here: `capture` writes it through `out_status` before it
/// returns, so by the time anybody collects the bytes the status has already
/// been read. Keeping a second copy would be a second thing to keep in step.
struct Finished {
    output: Vec<u8>,
}

/// The arguments in `buffer`, split on the NULs that separate them.
///
/// # Safety
///
/// `buffer` must point at `len` readable bytes.
unsafe fn arguments(buffer: *const u8, len: i64, count: i64) -> Option<Vec<Vec<u8>>> {
    if buffer.is_null() || len < 0 || count < 1 {
        return None;
    }
    // SAFETY: the caller promised `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(buffer, len as usize) };
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut start = 0usize;
    for (at, byte) in bytes.iter().enumerate() {
        if *byte == 0 {
            out.push(bytes[start..at].to_vec());
            start = at + 1;
        }
    }
    // Exactly the promised number, or the buffer and the count disagree and
    // the safe answer is to run nothing.
    if out.len() == count as usize { Some(out) } else { None }
}

/// A `Command` for `parts`, program first.
///
/// **No shell, and that is the whole point.** The program is looked up on
/// `PATH` by the operating system, and every other element is one argument
/// however many spaces, quotes or semicolons are in it.
fn command_of(parts: &[Vec<u8>]) -> Option<Command> {
    let program = String::from_utf8(parts[0].clone()).ok()?;
    let mut command = Command::new(program);
    for part in &parts[1..] {
        command.arg(String::from_utf8(part.clone()).ok()?);
    }
    Some(command)
}

/// Runs a program with the arguments in `buffer`, and waits.
///
/// The exit status, or -1 if the program could not be started at all — which
/// is a different thing from a program that started and exited non-zero, the
/// same distinction `std::process` already draws for the shell.
///
/// A child killed by a signal reports 128 plus the signal, which is the
/// convention every Unix shell already uses and what `$?` would have said.
///
/// # Safety
///
/// `buffer` must point at `len` readable bytes holding `count` NUL-terminated
/// arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_spawn_status(buffer: *const u8, len: i64, count: i64) -> i64 {
    // SAFETY: passed straight through; see this function's contract.
    let Some(parts) = (unsafe { arguments(buffer, len, count) }) else { return -1 };
    let Some(mut command) = command_of(&parts) else { return -1 };
    match command.status() {
        Err(_) => -1,
        Ok(status) => exit_status(status),
    }
}

/// Runs a program and keeps its standard output.
///
/// Answers how many bytes are waiting, or -1 if the program could not be
/// started. `out_status` receives the exit status. The bytes are collected
/// with [`khora_spawn_take`], which must be called before the next run on this
/// thread — the result is held per thread, so two fibers capturing at once do
/// not read each other's output.
///
/// # Safety
///
/// `buffer` as for [`khora_spawn_status`], and `out_status` a writable word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_spawn_capture(
    buffer: *const u8,
    len: i64,
    count: i64,
    out_status: *mut i64,
) -> i64 {
    // SAFETY: the caller promised a writable word.
    unsafe { out_status.write(-1) };

    // SAFETY: passed straight through; see the contract.
    let Some(parts) = (unsafe { arguments(buffer, len, count) }) else { return -1 };
    let Some(mut command) = command_of(&parts) else { return -1 };

    // The child's standard *error* is left alone, so it goes wherever this
    // process's does -- the same choice `open_pipe` made, and for the same
    // reason: merging the two is a decision only the caller can make.
    match command.output() {
        Err(_) => -1,
        Ok(done) => {
            let status = exit_status(done.status);
            // SAFETY: as above.
            unsafe { out_status.write(status) };
            let size = done.stdout.len() as i64;
            HELD.with(|held| {
                *held.borrow_mut() = Some(Finished { output: done.stdout });
            });
            size
        }
    }
}

/// Copies what the last capture on this thread left, and lets go of it.
///
/// # Safety
///
/// `into` must point at `len` writable bytes, and `len` must be the size the
/// capture answered.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_spawn_take(into: *mut u8, len: i64) {
    let taken = HELD.with(|held| held.borrow_mut().take());
    let Some(finished) = taken else { return };
    if into.is_null() || len <= 0 {
        return;
    }
    let wanted = (len as usize).min(finished.output.len());
    // SAFETY: the caller promised `len` writable bytes, and `wanted` is no
    // more than that.
    unsafe { std::ptr::copy_nonoverlapping(finished.output.as_ptr(), into, wanted) };
}

thread_local! {
    /// What the last capture on this thread produced.
    ///
    /// Per thread rather than one global, because a fiber is a thread here and
    /// two of them capturing at once would otherwise read each other's bytes.
    /// Cleared by `take`, so a caller that never collects leaks one output
    /// until the next run on the same thread replaces it.
    static HELD: std::cell::RefCell<Option<Finished>> = const { std::cell::RefCell::new(None) };
}

/// An `ExitStatus` as the one number the rest of the library speaks in.
///
/// The same mapping `std/process_posix.kh` already does by hand for a wait
/// status: an exit code as itself, and a child killed by a signal as 128 plus
/// the signal.
fn exit_status(status: std::process::ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        return i64::from(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + i64::from(signal);
        }
    }
    -1
}
