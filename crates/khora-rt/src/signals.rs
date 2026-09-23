//! What a program observes when the operating system asks it to stop.
//!
//! **Without this, every deploy drops every in-flight computation.** A
//! `SIGTERM` ended the process at its default disposition: no unwinding, no
//! finalizers, and so no `ROLLBACK` for the transaction `std::db` spent two
//! pages promising would be rolled back when its fiber is cancelled. The
//! promise held for a cancellation the program delivered to itself and not for
//! the one the platform delivers on every restart, which is the one that
//! happens.
//!
//! So a signal becomes a cancellation on the program's own computation,
//! through the machinery a nursery already uses. A program observes it as the
//! cancellation it already knows how to observe: no capability, no `raises`
//! row on `main`, nothing new to spell.
//!
//! # Not a signal handler, which is the whole reason this is small
//!
//! The signals are blocked in the process mask before any thread exists — the
//! mask is inherited, so every later thread has them blocked too — and one
//! dedicated thread sits in `sigwait`. Nothing here runs *in* a handler, so
//! nothing here has to be async-signal-safe: it is ordinary code on an
//! ordinary thread, free to take the locks `Fiber::cancel` and
//! `cancel_open_crews` take. Neither is on the list of calls a real handler
//! may make. Contrast [`crate::stack`]'s `SIGSEGV` handler, which calls
//! `libc::write` directly precisely because it *is* one.
//!
//! # What this does not promise
//!
//! **A cancellation is only observed at a `!` in a function that can raise.**
//! A `main` with no `raises` row has no channel to be interrupted on, so a
//! program shaped that way does not stop here — it waits for the operator's
//! second signal or for `SIGKILL`. That is `docs/design/effect-runtime.md` §6
//! meeting the entry point rather than a defect, and it is the condition the
//! promise has to be written with rather than rounded off: your finalizers run
//! *if the path from `main` down to your work carries a `raises` row*.
//!
//! **The grace period is your orchestrator's.** `systemd` has
//! `TimeoutStopSec`, Kubernetes `terminationGracePeriodSeconds`, `docker stop`
//! `--stop-timeout`, and all three end in `SIGKILL`. A second deadline inside
//! the runtime would be a second number to keep in sync, and the one that
//! actually fired would be the one nobody configured. There is a mechanical
//! reason too: `khora_region_release` wraps its finalizers in
//! [`crate::cancel::Shielded`], so a runtime deadline could not *interrupt* a
//! slow rollback — it could only abort the process, which is what `SIGKILL`
//! already is, arriving from something that knows the real number. What Khora
//! contributes is that the time gets used.

#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::OnceLock;

/// Whether the entry point has a `raises` row, and so a channel a cancellation
/// can travel on.
///
/// **Without this the simplest program anybody writes stops answering
/// `SIGTERM`.** A `main` with no `raises` row has no cancellation point
/// anywhere on the path from the entry point down, so the watcher's cancel
/// reaches nothing and the program runs until `SIGKILL` — where before the
/// watcher existed it died at once. A guarantee that is kept for some programs
/// and silently inverted for others is worse than the gap it closes, so the
/// watcher reads this and, when it is false, gets out of the way.
///
/// False until generated code says otherwise, which is the safe direction: a
/// program whose shape the compiler did not describe keeps the behaviour every
/// other program on the machine has.
#[cfg(unix)]
static ROOT_CAN_RAISE: AtomicBool = AtomicBool::new(false);

/// Records that the entry point can carry a cancellation. From generated code.
///
/// Called before [`crate::stack::khora_begin`], so the watcher thread does not
/// exist yet and cannot read a half-told story.
#[unsafe(no_mangle)]
pub extern "C" fn khora_root_can_raise() {
    #[cfg(unix)]
    ROOT_CAN_RAISE.store(true, Ordering::SeqCst);
}

/// The program's own computation, for the watcher to cancel.
///
/// Held rather than borrowed, and set before the watcher thread is spawned, so
/// the thread never observes it empty.
#[cfg(unix)]
static MAIN: OnceLock<std::sync::Arc<crate::current::Fiber>> = OnceLock::new();

/// Blocks the signals and starts the watcher. Once, from `khora_begin`.
///
/// **Before any thread exists**, which is the precondition the mask depends
/// on: a thread spawned earlier would not have inherited the block and would
/// keep the default disposition, so a signal delivered to *it* would kill the
/// process while the watcher waited. `khora_begin` runs first thing in every
/// entry point, which is why this is called from there and not from wherever
/// the first fiber is spawned.
#[cfg(unix)]
pub(crate) fn install() {
    // A causality check needs a way to turn this off: a test that passes with
    // the watcher disabled guards nothing, and the only way to know is to run
    // it both ways. Read once, at startup, before any thread exists.
    //
    // **The switch is `cfg(debug_assertions)` because a shipped program must
    // not honour it.** It reaches every compiled binary otherwise, so a
    // process whose environment happens to carry the name loses graceful
    // shutdown on `SIGTERM` silently and with no way to notice -- measured as
    // exit 143 with no finalizer, against 130 with one. A release build
    // ignores the variable; the tests that need it run against a debug
    // runtime, which is the only place it was ever read on purpose.
    #[cfg(debug_assertions)]
    if std::env::var_os("KHORA_NO_SIGNALS").is_some() {
        return;
    }
    let root = crate::current::this_root();
    let id = root.id();
    if MAIN.set(root).is_err() {
        // Already installed. A second watcher would race the first for the
        // signal and only one of them would get it.
        return;
    }

    // SAFETY: `sigemptyset` and `sigaddset` initialise and fill a `sigset_t`
    // this frame owns, and `pthread_sigmask` reads it and writes nothing
    // (the old mask is discarded through a null pointer, which is the
    // documented way to say "do not tell me"). Called before any thread is
    // spawned, so the mask this sets is the one every later thread inherits.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigaddset(&raw mut set, libc::SIGTERM);
        libc::sigaddset(&raw mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
        set
    };

    // **A failure to spawn is not fatal and is not silent.** The program can
    // still run; what it loses is the graceful stop, and an operator who is
    // told that can plan around it. Dying here would turn a resource shortage
    // into an outage.
    if std::thread::Builder::new()
        .name("khora-signals".to_string())
        .spawn(move || watch(set, id))
        .is_err()
    {
        // SAFETY: unblocking the same set this frame filled. Restoring the
        // default disposition is strictly better than leaving the signals
        // blocked with nobody in `sigwait`, which would make the process
        // ignore `kill` altogether.
        unsafe {
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut());
        }
        let _ = std::io::Write::write_all(
            &mut std::io::stderr(),
            b"khora: the signal watcher could not be started, so SIGTERM and SIGINT \
              end this program the way they did before -- at once, with no finalizers \
              and no rollback.\n",
        );
    }
}

/// Waits for a signal, for as long as the process lives.
///
/// The first one asks and the second one insists, which is the distinction an
/// operator is entitled to: having asked once and waited, sending another is
/// how they say the deadline is theirs.
///
/// **A first signal that nobody can hear is the second signal.** When the root
/// has no `raises` row the cancellation below would travel nowhere, so rather
/// than ask and be ignored the watcher restores the default disposition
/// straight away and the program dies exactly as it did before any of this
/// existed. What is lost in that case is the finalizers — which were never
/// running for that program shape — and what is not lost is the process
/// answering `kill`.
#[cfg(unix)]
fn watch(set: libc::sigset_t, id: usize) {
    let mut seen = 0_u32;
    loop {
        let mut signo: i32 = 0;
        // SAFETY: a filled set this frame owns and a writable `int`.
        // `sigwait` blocks rather than returning a pending signal, so this
        // thread spends its life here and costs nothing until one arrives.
        let failed = unsafe { libc::sigwait(&raw const set, &raw mut signo) };
        if failed != 0 {
            // **Leaving quietly would make the process ignore `kill`.** The
            // signals are still blocked -- this thread filled that mask -- so
            // returning with nobody in `sigwait` is exactly the state the
            // spawn-failure path above refuses to leave the process in. Undo
            // the block so the default disposition applies again, and say so:
            // a program that stops honouring SIGTERM has changed its contract
            // with whatever supervises it, and silence there is the worst of
            // the available outcomes.
            //
            // SAFETY: unblocking the same set this frame filled, on the thread
            // that filled it.
            unsafe {
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut());
            }
            let _ = std::io::Write::write_all(
                &mut std::io::stderr(),
                b"khora: the signal watcher stopped waiting, so SIGTERM and SIGINT \
                  end this program the way they did before -- at once, with no \
                  finalizers and no rollback.\n",
            );
            return;
        }
        seen += 1;
        if seen == 1 && ROOT_CAN_RAISE.load(Ordering::SeqCst) {
            // Exactly what cancelling a nursery does, and in that order: the
            // children first, so a parent blocked joining one is not left
            // waiting on a child nobody told to stop; then the flag and the
            // wake; then the pool, for a fiber asleep on a deadline or a
            // socket that the flag alone would not reach.
            crate::nursery::cancel_open_crews(id, crate::current::Stop::Cancel);
            if let Some(main) = MAIN.get() {
                main.cancel();
            }
            crate::fiber::cancel_by_id(id);
        } else {
            die_the_way_the_platform_says(signo);
        }
    }
}

/// Restores the default disposition and re-raises, so the process dies the way
/// the platform says it should.
///
/// **Not an `exit(128 + signo)` that imitates it.** A supervisor reading
/// `WTERMSIG` gets a real signal death and the status table stays true; an
/// `exit` would report a *normal* exit of 128 + signo, which is a different
/// thing that happens to print the same number.
///
/// Nothing returns past this: the disposition is the kernel's default, which
/// terminates.
#[cfg(unix)]
fn die_the_way_the_platform_says(signo: i32) -> ! {
    let _ = std::io::Write::flush(&mut std::io::stderr());
    // SAFETY: `signal` with `SIG_DFL` and `raise` on the signal this thread
    // just received, plus an unblock of a single-signal set this frame owns.
    // All three are async-signal-safe, which they do not have to be here, and
    // none can fail in a way this frame could act on.
    unsafe {
        libc::signal(signo, libc::SIG_DFL);
        let mut just_this: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut just_this);
        libc::sigaddset(&raw mut just_this, signo);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const just_this, std::ptr::null_mut());
        libc::raise(signo);
    }
    // `raise` on a default-disposition terminating signal does not come back
    // with the signal unblocked; if the kernel ever disagrees, spinning here
    // is wrong and so is returning into `watch`.
    unreachable!("a re-raised SIGTERM or SIGINT at SIG_DFL terminates the process")
}

/// **Windows has no `SIGTERM`, and this does not pretend otherwise.**
///
/// What is portable is the sentence, not the mechanism: *the thing the
/// operating system uses to ask this program to stop becomes a cancellation at
/// the root.* Windows has three of those and they are console events rather
/// than signals — `CTRL_C_EVENT`, `CTRL_BREAK_EVENT` and `CTRL_CLOSE_EVENT`,
/// delivered to a `SetConsoleCtrlHandler` callback the system runs on a thread
/// it creates, which is the same "not a handler, may take locks" property the
/// Unix half relies on.
///
/// What Windows has **not** got is any way for an arbitrary process to ask
/// another to stop. `TerminateProcess` is `SIGKILL`: no notice, no unwinding,
/// nothing to observe. A Windows service's `SERVICE_CONTROL_STOP` needs a
/// service host Khora does not have. So the case this exists for — an
/// orchestrator restarting a service — has no Windows equivalent here, and
/// `deployment/containers.md` is where that has to be said rather than here.
///
/// This is an empty function rather than an unimplemented one because a
/// program should still run; what it must not do is claim the guarantee.
#[cfg(not(unix))]
pub(crate) fn install() {}
