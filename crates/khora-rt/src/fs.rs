//! File operations that suspend a fiber rather than a worker.
//!
//! Phase 11E's caller. `std::fs` used to declare `fopen`, `fread`, `fwrite` and
//! `fclose` as foreign functions and call them directly, which is correct and
//! was fine while a fiber was a thread. Once a fiber is one of many on a
//! worker, a read of a cold file holds that worker — and everything queued
//! behind it — for as long as the disk takes.
//!
//! So `std::fs` calls these instead. Each one hands the real C call to
//! [`crate::blocking`] and suspends; off a scheduler there is no worker to
//! protect, and the pool is not involved at all.
//!
//! # What is not here
//!
//! `fseek` and `ftell` stay direct foreign calls in `std::fs`. They adjust a
//! buffer rather than touch a disk in the ordinary case, and both take or
//! return C's `long` — thirty-two bits on Windows and sixty-four on Linux —
//! which is a width question worth settling on its own rather than inside a
//! change about scheduling.
//!
//! # The pointers
//!
//! Every one of these takes a pointer from Khora and gives it to another
//! thread, which raw pointers are not `Send` for good reasons. It is sound
//! here for one specific reason, and only that one: **the fiber that owns the
//! memory is suspended for exactly as long as the pool thread holds it.** It
//! cannot read the buffer, free it, resize it, or hand it to another fiber
//! until the call has returned. The addresses cross as `usize` so that the
//! reasoning has to be written down at the cast rather than hidden in a
//! blanket `unsafe impl Send`.

use crate::blocking::blocking;
use core::ffi::c_void;

unsafe extern "C" {
    fn fopen(path: *const u8, mode: *const u8) -> *mut c_void;
    fn fread(into: *mut u8, size: usize, count: usize, file: *mut c_void) -> usize;
    fn fwrite(from: *const u8, size: usize, count: usize, file: *mut c_void) -> usize;
    fn fclose(file: *mut c_void) -> i32;
}

/// `fopen`, off the worker.
///
/// Opening is the slowest of these on a cold path — a name lookup, a directory
/// walk, possibly a network filesystem — and the one most worth not doing on a
/// scheduler thread.
#[unsafe(no_mangle)]
pub extern "C" fn khora_fs_open(path: *const u8, mode: *const u8) -> *mut c_void {
    let (path, mode) = (path as usize, mode as usize);
    blocking(move || {
        // SAFETY: both strings belong to a fiber that is suspended until this
        // returns, so neither can move or be freed while it runs.
        unsafe { fopen(path as *const u8, mode as *const u8) as usize }
    }) as *mut c_void
}

/// `fread`, off the worker.
#[unsafe(no_mangle)]
pub extern "C" fn khora_fs_read(
    into: *mut u8,
    size: usize,
    count: usize,
    file: *mut c_void,
) -> usize {
    let (into, file) = (into as usize, file as usize);
    blocking(move || {
        // SAFETY: the destination belongs to a suspended fiber, which cannot
        // read it or give it away before this returns.
        unsafe { fread(into as *mut u8, size, count, file as *mut c_void) }
    })
}

/// `fwrite`, off the worker.
#[unsafe(no_mangle)]
pub extern "C" fn khora_fs_write(
    from: *const u8,
    size: usize,
    count: usize,
    file: *mut c_void,
) -> usize {
    let (from, file) = (from as usize, file as usize);
    blocking(move || {
        // SAFETY: as above; the source outlives the call because its owner is
        // suspended for the whole of it.
        unsafe { fwrite(from as *const u8, size, count, file as *mut c_void) }
    })
}

/// `fclose`, off the worker.
///
/// Closing flushes, so it writes, so it blocks — which is easy to forget when
/// reading a program that only ever seems to read.
#[unsafe(no_mangle)]
pub extern "C" fn khora_fs_close(file: *mut c_void) -> i32 {
    let file = file as usize;
    // SAFETY: the handle belongs to a suspended fiber and is not closed twice
    // — `std::fs` checks for null and drops its reference after this returns.
    blocking(move || unsafe { fclose(file as *mut c_void) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shims work off a scheduler, which is the path every ordinary
    /// program takes and the one where the pool must stay out of the way.
    #[test]
    fn a_file_round_trips_without_a_scheduler() {
        let mut path = std::env::temp_dir();
        path.push(format!("khora-fs-{}.txt", std::process::id()));
        let name = format!("{}\0", path.display());

        let file = khora_fs_open(name.as_ptr(), c"wb".as_ptr().cast());
        assert!(!file.is_null(), "could not open {}", path.display());
        let text = b"the pool stays out of the way";
        assert_eq!(khora_fs_write(text.as_ptr(), 1, text.len(), file), text.len());
        assert_eq!(khora_fs_close(file), 0);

        let file = khora_fs_open(name.as_ptr(), c"rb".as_ptr().cast());
        assert!(!file.is_null());
        let mut into = vec![0u8; text.len()];
        assert_eq!(khora_fs_read(into.as_mut_ptr(), 1, into.len(), file), text.len());
        assert_eq!(khora_fs_close(file), 0);
        assert_eq!(&into, text);

        let _ = std::fs::remove_file(&path);
    }

    /// And on a worker they go through the pool, giving the worker back.
    #[test]
    fn a_read_on_a_worker_goes_through_the_pool() {
        use crate::coro::Task;
        use crate::scheduler::Scheduler;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let mut path = std::env::temp_dir();
        path.push(format!("khora-fs-fiber-{}.txt", std::process::id()));
        std::fs::write(&path, b"read me").expect("the fixture");
        let name = format!("{}\0", path.display());

        let read = Arc::new(AtomicUsize::new(0));
        let count = read.clone();
        let before = crate::blocking::pool().stats().ran;

        let scheduler = Scheduler::new(2);
        scheduler.spawn(Task::new(move || {
            let file = khora_fs_open(name.as_ptr(), c"rb".as_ptr().cast());
            assert!(!file.is_null());
            let mut into = [0u8; 7];
            count.store(khora_fs_read(into.as_mut_ptr(), 1, 7, file), Ordering::SeqCst);
            khora_fs_close(file);
        }));
        scheduler.drain();

        assert_eq!(read.load(Ordering::SeqCst), 7);
        assert!(
            crate::blocking::pool().stats().ran >= before + 3,
            "open, read and close should each have gone to the pool"
        );
        let _ = std::fs::remove_file(&path);
    }
}
