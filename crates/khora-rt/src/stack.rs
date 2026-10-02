//! What happens when a program runs out of stack.
//!
//! **It used to be nothing at all.** A `List` past about eight thousand
//! elements killed the process with no message on either stream: `List` was
//! walked recursively everywhere in `std` then, so a log analyzer that read a
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
/// somebody to read code that cannot be the cause.
///
/// **And then it claimed too much in the other direction.** "`std` walks lists
/// and strings with loops, so it is most likely a function of your own" is
/// false for `std::json::parse`, which recurses once per character of a string
/// literal: a 50 KB document kills the process on the main thread and an 11 KB
/// one inside a request fiber, with no user code in the frame at all. A reader
/// told to look at their own handlers spends an afternoon there.
///
/// So the note names both possibilities and neither as "most likely". It
/// cannot say which without a backtrace, and the thing it must not do is rule
/// one out.
const MESSAGE: &[u8] = b"khora: the stack ran out, or the runtime touched memory that is not the program's (a segmentation fault; please report it)\nnote: a function that recurses as deep as its input will do this -- one you\n      wrote, a derived `Eq`, `Ord` or `Show` on a deeply nested value, or\n      `std::json::parse` on a document with a very long string in it, which\n      recurses per character. Most of `std` walks with loops and is not the\n      cause; `json` is the exception.\n";

/// How much stack a fiber gets, on either backend: eight megabytes.
///
/// **What it prevents: a program whose recursion limit depends on where it
/// runs.** A recursive walk of twenty thousand list cells finished in `main`,
/// finished on a fiber's thread, and died on the scheduler with a bare
/// `SIGSEGV`, because the three had eight, two and one megabytes -- the
/// process's `ulimit -s`, Rust's default for a spawned thread, and
/// `corosensei`'s default for a coroutine. Eight is what `main` gets from a
/// default `ulimit -s` on Linux and macOS, so a function that runs in `main`
/// runs on a fiber, and a fiber runs the same on both backends.
///
/// **It costs address space, not memory.** Both kinds of stack are reserved
/// and committed a page at a time as they are touched, so a fiber resident
/// for 8 KB of stack is resident for 8 KB at any size; `fibers_at_scale` in
/// `soak.rs` measured the same bytes per fiber at a hundred thousand fibers
/// with one megabyte and with eight. Where it does cost:
///
/// - a host with `vm.overcommit_memory=2` charges the whole reservation, so
///   it runs out of commit at an eighth of the fibers it did;
/// - a hundred thousand fibers reserve 800 GB, which x86-64's 128 TB of user
///   address space holds and a tight `RLIMIT_AS` does not;
/// - transparent huge pages could make a used stack resident in 2 MB pieces,
///   which a one-megabyte stack could never hold. `crate::coro::fiber_stack`
///   declines them for a coroutine, for about two and a half microseconds per
///   spawn. A fiber's *thread*, on the default backend, is not advised: on a
///   host with THP set to `always` a thread that once recursed deep may stay
///   resident for 2 MB more than it touched.
///
/// A program that needs more is one that should loop instead: there is no
/// knob, and growing a stack by moving it is ruled out because the scheduler
/// does not know where Khora's pointers are.
pub const FIBER_STACK: usize = 8 * 1024 * 1024;

/// The size of the stack the report runs on, where the platform needs one.
///
/// A handler for a stack overflow cannot run on the stack that overflowed, so
/// it gets one of its own; the handler writes one constant and makes one
/// call, and this is far more than that needs.
#[cfg(unix)]
const ALT_STACK: usize = 64 * 1024;

/// Installs the stack guard and the signal watcher, once, before anything else
/// runs.
///
/// Called by generated code at the top of every entry point — the ordinary
/// one, the test harness and the bench harness — because a program that
/// exhausts its stack should say so however it was started.
///
/// **The signal watcher is here for the same reason the stack guard is**, and
/// for one more: it blocks `SIGTERM` and `SIGINT` in the process mask, which
/// only works before any thread exists to inherit a different one. `khora_rt`
/// spawns nothing until a fiber is spawned, and this runs first.
///
/// Idempotent and cheap: a second call on a platform that has already
/// installed does nothing, which matters because a test binary and the program
/// under it can both reach here.
#[unsafe(no_mangle)]
pub extern "C" fn khora_begin() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(install);
    static SIGNALS: std::sync::Once = std::sync::Once::new();
    SIGNALS.call_once(crate::signals::install);
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
/// How much of the faulting stack is kept back for the handler.
///
/// **Windows gives a handler no room unless it is asked.** The one guard page
/// of slack is spent by the dispatch itself -- `RtlDispatchException` builds a
/// context record and an exception record on the faulting stack before the
/// first vectored handler is entered -- so a handler installed without a
/// guarantee is one the process faults again on the way into, and dies with
/// nothing written. The guarantee moves the guard page out by this much, and
/// the dispatch takes its frames from the gap.
///
/// It costs this much address space per thread, reserved rather than
/// committed. There is no way to ask what the dispatch needs, so the number is
/// taken from the Rust runtime, which reserves the same for the same job, and
/// is not derived from anything here.
const GUARANTEE: u32 = 0x5000;

#[cfg(windows)]
/// Sets aside the room the handler runs in, for the calling thread.
///
/// Per *thread*, not per process: the guarantee lives in the thread
/// environment block, so a thread that never calls this reports nothing even
/// though the handler is installed process-wide.
///
/// A failure is not worth stopping for. It means the request was larger than
/// the stack, and the outcome is the silence that came before this -- not
/// worse.
fn reserve() {
    use windows_sys::Win32::System::Threading::SetThreadStackGuarantee;

    let mut wanted = GUARANTEE;
    // SAFETY: the call reads and writes the one `u32` it is given, which is a
    // local of this frame, and touches nothing else.
    unsafe {
        SetThreadStackGuarantee(&raw mut wanted);
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

    // **Before the handler, and the reason the handler reports anything.**
    // The page of slack a guard-page fault leaves is not the handler's to
    // spend; the exception dispatch spends it first. Without this the handler
    // is reached on some runs and not others -- whichever way the faulting
    // frame happened to divide the last page -- and a message that appears
    // intermittently is one nobody can rely on.
    //
    // **It covers this thread only.** Every other thread that runs Khora code
    // -- a fiber's thread, a scheduler worker, a test's thread -- asks for its
    // own with `guard_this_thread`. A coroutine's stack is not a thread's, and
    // `corosensei` copies this thread's guarantee into each one it builds; a
    // coroutine built on a worker that has not reserved gets none.
    reserve();

    // First in the chain, so nothing installed later can swallow it.
    // SAFETY: `handler` has the signature the system requires and outlives the
    // process.
    unsafe {
        AddVectoredExceptionHandler(1, Some(handler));
    }
}

#[cfg(unix)]
fn install() {
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
    /// `Ptr` is opaque and never dereferenced -- but the runtime can: a
    /// use-after-free in `nursery::cancel_open_crews` faulted at address `0x20`
    /// on the `khora-deadlines` thread and was reported here as the stack
    /// running out. So the message names both.
    unsafe extern "C" fn handler(signal: i32) {
        report();
        // SAFETY: `signal` with `SIG_DFL` is async-signal-safe and is what
        // restores the behavior the process had before this was installed.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
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
        // **Both signals, because macOS overflows with the other one.** A
        // thread's stack there ends in a guard page whose fault the kernel
        // delivers as `SIGBUS`, not `SIGSEGV` -- only the main thread's is
        // `SIGSEGV` -- so with one handler a fiber's overflow on macOS died
        // with status 10 and said nothing. Rust's own runtime installs for
        // the same two, for the same reason.
        for signal in OVERFLOW_SIGNALS {
            libc::sigaction(signal, &raw const action, std::ptr::null_mut());
        }
    }
}

/// The signals an exhausted stack can arrive as: `SIGSEGV` everywhere, and
/// `SIGBUS` for a thread's guard page on macOS (and some BSDs).
#[cfg(unix)]
const OVERFLOW_SIGNALS: [i32; 2] = [libc::SIGSEGV, libc::SIGBUS];

/// Turns this thread's alternate stack off.
///
/// **The size goes in even though it is ignored.** POSIX says `ss_sp` and
/// `ss_size` are ignored when disabling, but macOS's `sigaltstack` checks the
/// size first and refuses anything under `MINSIGSTKSZ` with `ENOMEM` -- so a
/// zeroed `stack_t` left the stack installed. `ThreadGuard`'s drop then
/// unmapped memory the kernel would still deliver a signal onto. Rust's
/// runtime passes the size for the same bug.
#[cfg(unix)]
fn disable_alternate_stack() {
    // SAFETY: installs nothing; the struct is fully initialized and only read.
    unsafe {
        let mut off: libc::stack_t = std::mem::zeroed();
        off.ss_flags = libc::SS_DISABLE;
        off.ss_size = ALT_STACK;
        libc::sigaltstack(&raw const off, std::ptr::null_mut());
    }
}

/// Nothing to install where there is no operating system to fault.
#[cfg(not(any(unix, windows)))]
fn install() {}

/// How much stack is touched before a thread or a process is started.
///
/// Measured on Linux x86-64 with glibc 2.41, from the caller of
/// `std::thread::Builder::spawn` down: about 1.4 KB for a thread and 1.5 KB
/// for `std::process::Command::spawn` (a stack painted with a pattern before
/// the call and read back after it). Sixteen kilobytes is ten times that, so a
/// libc that needs more still fits.
const START_ROOM: usize = 16 * 1024;

/// Faults here, where the report can run, rather than inside libc where it
/// cannot -- if the stack is about to run out.
///
/// **What it prevents: a stack overflow that ends the process with nothing
/// on stderr.** glibc's `pthread_create` blocks every signal while it builds
/// the new thread, and `posix_spawn` blocks them while
/// it forks. A stack fault in that window is a `SIGSEGV` delivered to a thread
/// that has it blocked, and Linux does not queue a fault: it restores the
/// default action, unblocks it and kills the process, so the handler in this
/// module never runs. A Khora HTTP server whose accept loop recursed once per
/// connection died exactly there, after about 130,000 connections at an 8 MB
/// stack, with empty streams: the deepest point of each connection on its
/// main thread was the `pthread_create` for that connection's fiber. With a
/// release runtime it was silent at every stack size tried, 256 KB to 8 MB.
///
/// Every thread the runtime starts goes through [`spawn_thread`], which calls
/// this, and a process start calls it before `Command` does. It takes
/// [`START_ROOM`] of stack and writes to it a kilobyte at a time, top down,
/// with signals as the caller has them: if the stack ends within that
/// distance, the fault is here, the handler runs, and the report is printed.
/// Otherwise the start that follows has the room it was measured to need.
///
/// **What it costs:** sixteen writes, to memory the start was about to use,
/// per thread or process started; nothing per fiber on the scheduler, which
/// starts its threads once. And a program that would have *just* survived --
/// one that starts a thread with between 2 and 16 KB of stack left -- now dies
/// there, of the same overflow it was one frame away from, and says so.
///
/// **What it does not cover:** a thread or process started by anything other
/// than the runtime -- a C library linked into the program, for one.
///
/// On macOS and Windows it is not needed and is harmless. macOS's
/// `pthread_create` does not block signals around the thread's creation, so
/// a fault there reaches the handler like any other; a Windows overflow is a
/// structured exception, which no signal mask defers. Neither claim has been
/// tested by a run on those platforms here.
#[inline(never)]
pub(crate) fn before_a_start() {
    const STEP: usize = 1024;
    let mut room = std::mem::MaybeUninit::<[u8; START_ROOM]>::uninit();
    let base = room.as_mut_ptr().cast::<u8>();
    let mut at = START_ROOM;
    while at >= STEP {
        at -= STEP;
        // SAFETY: `at` is less than `START_ROOM`, so the byte written is
        // inside `room`, a local of this frame that nothing else can see.
        // Writing a `u8` into uninitialized bytes is sound; nothing reads
        // them. Volatile so the writes are not removed as dead.
        unsafe { std::ptr::write_volatile(base.add(at), 0) };
    }
    std::hint::black_box(&room);
}

/// `builder.spawn(body)`, after [`before_a_start`].
///
/// The one way the runtime starts a thread, so that none of them can start
/// one inside libc's blocked window with the stack nearly gone. Rust's own
/// `std::thread::spawn` does not go through here; a thread started with it
/// from a deep stack is the silence [`before_a_start`] describes.
pub(crate) fn spawn_thread<F, T>(
    builder: std::thread::Builder,
    body: F,
) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    before_a_start();
    builder.spawn(body)
}

/// Room to report from, on the calling thread, for as long as the guard lives.
///
/// **What it prevents: a fiber that runs out of stack and says nothing.**
/// `khora_begin` sets room aside on the thread that calls it, and the room is
/// per thread on both platforms -- an alternate signal stack on Unix, a stack
/// guarantee on Windows. A fiber's thread, a scheduler worker (whose
/// coroutines' overflows are delivered to the worker) and a test's thread had
/// none, so an overflow on any of them faulted again on the way into the
/// handler and the process ended with a status and empty streams.
///
/// Called first thing by each of those threads, which is every thread that
/// runs Khora code. A thread that already has an alternate stack keeps it:
/// the Rust test harness gives its threads one, and replacing it would free
/// memory the harness still points at.
///
/// It costs one 64 KB mapping per such thread, released when the guard is
/// dropped; a hundred thousand fibers on the scheduler share a handful of
/// workers, so the cost scales with threads, not fibers.
#[must_use = "the room is given back when the guard is dropped"]
pub(crate) fn guard_this_thread() -> ThreadGuard {
    #[cfg(unix)]
    {
        ThreadGuard { alt: alternate_stack() }
    }
    #[cfg(windows)]
    {
        reserve();
        ThreadGuard {}
    }
    #[cfg(not(any(unix, windows)))]
    {
        ThreadGuard {}
    }
}

/// The room [`guard_this_thread`] set aside, given back on drop.
pub(crate) struct ThreadGuard {
    /// The mapping this guard installed, or null when the thread already had
    /// one or none could be had.
    #[cfg(unix)]
    alt: *mut libc::c_void,
}

#[cfg(unix)]
impl Drop for ThreadGuard {
    fn drop(&mut self) {
        if self.alt.is_null() {
            return;
        }
        // Disabling comes first, so the kernel never delivers onto memory
        // that has been unmapped; a signal handler cannot be running on it
        // here, because the thread is running this.
        disable_alternate_stack();
        // SAFETY: `alt` is the mapping `alternate_stack` made and installed on
        // this thread -- a guard is neither `Send` nor `Sync`, so it is
        // dropped where it was made -- and it was just uninstalled.
        unsafe {
            libc::munmap(self.alt, ALT_STACK);
        }
    }
}

/// Maps and installs an alternate signal stack for this thread, unless it
/// has one. Null when it had one, or when the mapping failed: a failure is
/// the silence that came before, not worse, and not a reason to stop a fiber.
#[cfg(unix)]
fn alternate_stack() -> *mut libc::c_void {
    // SAFETY: a query (null for the value to install) into a local, then an
    // anonymous mapping nothing else knows about, installed as this thread's
    // alternate stack with its exact size.
    unsafe {
        let mut current: libc::stack_t = std::mem::zeroed();
        libc::sigaltstack(std::ptr::null(), &raw mut current);
        if current.ss_flags & libc::SS_DISABLE == 0 {
            return std::ptr::null_mut();
        }
        let alt = libc::mmap(
            std::ptr::null_mut(),
            ALT_STACK,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        if alt == libc::MAP_FAILED {
            return std::ptr::null_mut();
        }
        let mut stack: libc::stack_t = std::mem::zeroed();
        stack.ss_sp = alt;
        stack.ss_size = ALT_STACK;
        stack.ss_flags = 0;
        if libc::sigaltstack(&raw const stack, std::ptr::null_mut()) != 0 {
            libc::munmap(alt, ALT_STACK);
            return std::ptr::null_mut();
        }
        alt
    }
}

#[cfg(test)]
mod tests {
    /// **Every thread that runs Khora code has room to report from, and a
    /// thread that already had room keeps its own.**
    ///
    /// A fresh thread gets an alternate stack it did not have, and loses it
    /// again when the guard goes, so a thread pool does not collect them. A
    /// thread with one -- the Rust test harness installs one on each of its
    /// threads -- is left exactly as it was, because unmapping that stack on
    /// drop would free memory its owner still points at.
    #[cfg(unix)]
    #[test]
    fn a_thread_running_khora_code_has_room_to_report_from() {
        fn installed() -> libc::stack_t {
            // SAFETY: a pure query (null for the value to install) into a
            // local of this frame.
            unsafe {
                let mut stack: libc::stack_t = std::mem::zeroed();
                libc::sigaltstack(std::ptr::null(), &raw mut stack);
                stack
            }
        }
        fn none() -> bool {
            installed().ss_flags & libc::SS_DISABLE != 0
        }

        std::thread::Builder::new()
            .spawn(|| {
                // A thread std spawns may already have one; this one must not,
                // or the first half proves nothing. The memory is std's and
                // stays mapped; nothing here is a signal handler. Through the
                // same call the guard's drop uses, so this half of the test
                // also fails on macOS if the call can't disable.
                super::disable_alternate_stack();
                assert!(none(), "could not take the thread's alternate stack away");
                {
                    let _room = super::guard_this_thread();
                    assert!(!none(), "a thread running Khora code has no room to report from");
                    assert_eq!(installed().ss_size, super::ALT_STACK);
                }
                assert!(none(), "the room outlived the guard");
            })
            .expect("a thread")
            .join()
            .expect("the thread");

        std::thread::Builder::new()
            .spawn(|| {
                let mine = installed();
                if mine.ss_flags & libc::SS_DISABLE != 0 {
                    return;
                }
                {
                    let _room = super::guard_this_thread();
                    assert_eq!(installed().ss_sp, mine.ss_sp, "a thread's own room was replaced");
                }
                assert_eq!(installed().ss_sp, mine.ss_sp, "a thread's own room was taken away");
                assert_eq!(installed().ss_flags & libc::SS_DISABLE, 0);
            })
            .expect("a thread")
            .join()
            .expect("the thread");
    }

    /// **The handler has somewhere to run.**
    ///
    /// Both platforms report from a stack that is already gone, so both have
    /// to set room aside *before* the fault -- there is none to be had
    /// afterwards. Unix sets aside a separate stack with `sigaltstack` and a
    /// `SIGSEGV` disposition that says to use it; Windows sets aside a slice
    /// of the faulting one with `SetThreadStackGuarantee`. Neither is the
    /// default, and a handler installed without its room reports nothing: the
    /// dispatch faults again on the way in and the process dies silently,
    /// which is the state this module exists to end.
    ///
    /// **It asserts the room and not the message**, because the room is what a
    /// process that has not overflowed can read back from the platform.
    /// `khora-codegen-llvm::debugging::running_out_of_stack_says_so` asserts
    /// the message, and needs a child process to die to see it -- so it can
    /// only say that reporting worked on the one run it took, where this says
    /// the precondition holds at all.
    #[test]
    fn the_handler_has_somewhere_to_run() {
        super::khora_begin();

        #[cfg(unix)]
        // SAFETY: both calls take a null pointer for the value to install,
        // which is how each is spelled as a pure query, and write only through
        // pointers to locals of this frame.
        unsafe {
            let mut stack: libc::stack_t = std::mem::zeroed();
            libc::sigaltstack(std::ptr::null(), &raw mut stack);
            assert!(!stack.ss_sp.is_null(), "no alternate stack to report on");
            assert_eq!(stack.ss_flags & libc::SS_DISABLE, 0, "the alternate stack is off");

            for signal in super::OVERFLOW_SIGNALS {
                let mut action: libc::sigaction = std::mem::zeroed();
                libc::sigaction(signal, std::ptr::null(), &raw mut action);
                assert_ne!(
                    action.sa_flags & libc::SA_ONSTACK,
                    0,
                    "the handler for signal {signal} would run on the stack that overflowed"
                );
            }
        }

        #[cfg(windows)]
        // SAFETY: the call reads the current guarantee into the word it is
        // given and raises it only if that word is larger, so passing zero is
        // how it is spelled as a pure query.
        unsafe {
            use windows_sys::Win32::System::Threading::SetThreadStackGuarantee;

            let mut current: u32 = 0;
            assert_ne!(SetThreadStackGuarantee(&raw mut current), 0, "the query failed");
            assert!(
                current >= super::GUARANTEE,
                "only {current} bytes are set aside for the handler"
            );
        }
    }
}
