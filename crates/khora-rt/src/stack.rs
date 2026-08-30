//! What happens when a program runs out of stack.
//!
//! **It used to be nothing at all.** A `List` past about eight thousand
//! elements killed the process with no message on either stream: `List` was
//! walked recursively everywhere in `std` then, so a log analyser that read a
//! hundred and twenty-two thousand lines died while *reporting* on them, and
//! the only evidence was a shell prompt and an exit status nobody reads.
//!
//! Every other way a Khora program stops says why. Dividing past the
//! significand says `Decimal division overflowed`; running off an array says
//! `index 9 is outside an array of 3`, and `docs/reference/traps.md` lists both
//! with the status they exit. Stack exhaustion was in neither the list nor the
//! output.
//!
//! # Why nothing was installed
//!
//! A Rust binary gets this for free: `lang_start` installs a handler before
//! `main`, which is what prints `thread 'main' has overflowed its stack`. A
//! Khora executable has no Rust prologue — the runtime is a static archive
//! linked into a C `main` that generated code writes — so none of that runs.
//! `khora_begin` is where it does now, called first thing by every entry point
//! shape.
//!
//! # What a handler may do
//!
//! Almost nothing. It runs with the stack already exhausted (Windows) or on a
//! borrowed one (Unix), so it must not allocate, must not lock, and must not
//! call anything that might. That rules out `std::io`, `format!` and the
//! reporting in [`crate::trap`], which all do at least one of the three. So
//! the message is a constant and the write is the raw system call.
//!
//! The process still dies, and dies the same way it did: the handler reports
//! and declines to handle. A stack that is gone cannot be unwound onto.

/// What both platforms print. A constant because a handler cannot format one.
///
/// **It used to send the reader to three functions that are fine.** The note
/// named `List::sort`, `fold` and `length` as the shape to look at, which was
/// true when every walk in `std` was a recursion. They are loops now, and so
/// are `String::split`, `join` and `repeat`, so a note pointing at them sends
/// somebody to read code that cannot be the cause. What is left is a function
/// somebody wrote, or a `derive` walking a value that is nested rather than
/// long -- and those are worth naming instead.
const MESSAGE: &[u8] = b"khora: the stack ran out\nnote: a function that recurses as deep as its input will do this. `std` walks\n      lists and strings with loops, so it is most likely a function of your\n      own -- or a derived `Eq`, `Ord` or `Show` on a value nested tens of\n      thousands deep.\n";

/// Installs the stack guard, once, before anything else runs.
///
/// Called by generated code at the top of every entry point — the ordinary
/// one, the test harness and the bench harness — because a program that
/// exhausts its stack should say so however it was started.
///
/// Idempotent and cheap: a second call on a platform that has already
/// installed does nothing, which matters because a test binary and the program
/// under it can both reach here.
#[unsafe(no_mangle)]
pub extern "C" fn khora_begin() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(install);
}

/// Writes `MESSAGE` to standard error with the smallest call that will do it.
///
/// Async-signal-safe on Unix and allocation-free on Windows, which is the
/// whole requirement. A short write is not retried: there is no stack left to
/// be careful on, and a truncated message is better than a loop.
fn report() {
    #[cfg(unix)]
    // SAFETY: `write` on a file descriptor with a constant buffer, which is on
    // the list of calls a signal handler may make.
    unsafe {
        libc::write(2, MESSAGE.as_ptr().cast(), MESSAGE.len());
    }

    #[cfg(windows)]
    // SAFETY: `WriteFile` on the process's own standard error handle with a
    // constant buffer. Neither call allocates or takes a lock this thread
    // could already hold.
    unsafe {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};

        let handle: HANDLE = GetStdHandle(STD_ERROR_HANDLE);
        let mut written: u32 = 0;
        WriteFile(
            handle,
            MESSAGE.as_ptr(),
            MESSAGE.len() as u32,
            &raw mut written,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(windows)]
fn install() {
    use windows_sys::Win32::Foundation::EXCEPTION_STACK_OVERFLOW;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_POINTERS,
    };

    /// Reports a stack overflow and declines to handle it.
    ///
    /// `EXCEPTION_CONTINUE_SEARCH` is the point: the process must still die of
    /// this, with the status it always died of. Handling it would mean
    /// continuing on a stack that is gone.
    ///
    /// Windows gives a guard-page fault one page of slack before the hard
    /// limit, which is what makes it possible to write anything at all here.
    unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
        const CONTINUE_SEARCH: i32 = 0;
        if info.is_null() {
            return CONTINUE_SEARCH;
        }
        // SAFETY: the system passes a valid record for the duration of the
        // call, and this reads one field of it.
        let code = unsafe {
            let record = (*info).ExceptionRecord;
            if record.is_null() {
                return CONTINUE_SEARCH;
            }
            (*record).ExceptionCode
        };
        if code == EXCEPTION_STACK_OVERFLOW {
            report();
        }
        CONTINUE_SEARCH
    }

    // First in the chain, so nothing installed later can swallow it.
    // SAFETY: `handler` has the signature the system requires and outlives the
    // process.
    unsafe {
        AddVectoredExceptionHandler(1, Some(handler));
    }
}

#[cfg(unix)]
fn install() {
    // A handler for a stack overflow cannot run on the stack that overflowed,
    // so it gets one of its own. `SIGSTKSZ` is the platform's own answer to
    // how big that has to be.
    const ALT_STACK: usize = 64 * 1024;
    static mut ALT: [u8; ALT_STACK] = [0; ALT_STACK];

    /// Reports, restores the default disposition, and returns.
    ///
    /// Returning from a `SIGSEGV` handler re-executes the faulting
    /// instruction, which faults again -- and now with the default handler in
    /// place, so the process dies of `SIGSEGV` exactly as it did before. That
    /// is deliberate: this says why, it does not rescue.
    ///
    /// **Every `SIGSEGV` is reported this way**, not only the ones from an
    /// exhausted stack. Telling them apart means comparing the fault address
    /// against the thread's own guard page, which is a different lookup on
    /// each platform and needs the stack bounds a fiber switch keeps moving.
    /// A Khora program cannot produce a wild pointer through the language --
    /// `Ptr` is opaque and never dereferenced -- so in practice a segmentation
    /// fault here *is* the stack, and a message that is occasionally too
    /// specific beats one that never appears.
    unsafe extern "C" fn handler(_signal: i32) {
        report();
        // SAFETY: `signal` with `SIG_DFL` is async-signal-safe and is what
        // restores the behaviour the process had before this was installed.
        unsafe {
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
        }
    }

    // SAFETY: `ALT` is a static buffer that outlives the process, and the
    // sigaction is filled in completely before it is installed.
    unsafe {
        let mut stack: libc::stack_t = std::mem::zeroed();
        stack.ss_sp = (&raw mut ALT).cast();
        stack.ss_size = ALT_STACK;
        stack.ss_flags = 0;
        libc::sigaltstack(&raw const stack, std::ptr::null_mut());

        let mut action: libc::sigaction = std::mem::zeroed();
        // Through a pointer rather than straight to an integer: casting a
        // function item to `usize` in one step is refused, and `sa_sigaction`
        // is an integer-shaped field holding an address.
        action.sa_sigaction = handler as *const () as usize;
        action.sa_flags = libc::SA_ONSTACK;
        libc::sigemptyset(&raw mut action.sa_mask);
        libc::sigaction(libc::SIGSEGV, &raw const action, std::ptr::null_mut());
    }
}

/// Nothing to install where there is no operating system to fault.
#[cfg(not(any(unix, windows)))]
fn install() {}
