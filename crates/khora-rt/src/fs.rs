//! File operations that suspend a fiber rather than a worker.
//!
//! `std::fs` used to declare `fopen`, `fread`, `fwrite` and `fclose` as foreign
//! functions and call them directly, which is correct and was fine while a
//! fiber was a thread. Once a fiber is one of many on a worker, a read of a
//! cold file holds that worker — and everything queued behind it — for as long
//! as the disk takes.
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
//!
//! **All four are `unsafe fn`**, which they were not until the phase 13
//! soundness audit. Each already carried a `SAFETY` comment discharging the
//! dereference — but the obligation those comments discharge was never placed
//! on anybody, because a safe `extern "C" fn` says it has no preconditions.
//! Generated code cannot tell the difference; a Rust caller could, and the
//! other fifty-six exported functions that take a pointer say so.

use crate::blocking::blocking;
use core::ffi::c_void;

unsafe extern "C" {
    fn fopen(path: *const u8, mode: *const u8) -> *mut c_void;
    fn fread(into: *mut u8, size: usize, count: usize, file: *mut c_void) -> usize;
    fn fwrite(from: *const u8, size: usize, count: usize, file: *mut c_void) -> usize;
    fn fclose(file: *mut c_void) -> i32;
    fn remove(path: *const u8) -> i32;
    fn rename(from: *const u8, to: *const u8) -> i32;
}

/// `fopen`, off the worker.
///
/// Opening is the slowest of these on a cold path — a name lookup, a directory
/// walk, possibly a network filesystem — and the one most worth not doing on a
/// scheduler thread.
///
/// # Safety
///
/// `path` and `mode` must be NUL-terminated strings that stay valid until this
/// returns, which they do because their owner is the fiber suspended for the
/// whole call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_open(path: *const u8, mode: *const u8) -> *mut c_void {
    let (path, mode) = (path as usize, mode as usize);
    blocking(move || {
        // SAFETY: both strings belong to a fiber that is suspended until this
        // returns, so neither can move or be freed while it runs.
        unsafe { fopen(path as *const u8, mode as *const u8) as usize }
    }) as *mut c_void
}

/// `fread`, off the worker.
///
/// # Safety
///
/// `into` must be writable for `size * count` bytes and `file` must be a live
/// handle from [`khora_fs_open`]. Both stay valid for the call because their
/// owner is suspended.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_read(
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
///
/// # Safety
///
/// `from` must be readable for `size * count` bytes and `file` must be a live
/// handle from [`khora_fs_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_write(
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
///
/// # Safety
///
/// `file` must be a live handle from [`khora_fs_open`], and must not be closed
/// twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_close(file: *mut c_void) -> i32 {
    let file = file as usize;
    // SAFETY: the handle belongs to a suspended fiber and is not closed twice
    // — `std::fs` checks for null and drops its reference after this returns.
    blocking(move || unsafe { fclose(file as *mut c_void) })
}

/// `remove`, off the worker.
///
/// A directory entry disappearing is a write to the directory, so it reaches
/// the disk and blocks like the rest of them.
///
/// # Safety
///
/// `path` must be a NUL-terminated string that stays valid until this returns,
/// which it does because its owner is the fiber suspended for the whole call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_remove(path: *const u8) -> i32 {
    let path = path as usize;
    // SAFETY: the string belongs to a suspended fiber, which cannot free it or
    // hand it to another fiber before this returns.
    blocking(move || unsafe { remove(path as *const u8) })
}

/// `rename`, off the worker.
///
/// # Safety
///
/// Both paths must be NUL-terminated strings valid for the call, as above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_rename(from: *const u8, to: *const u8) -> i32 {
    let (from, to) = (from as usize, to as usize);
    // SAFETY: as above, for both strings.
    blocking(move || unsafe { rename(from as *const u8, to as *const u8) })
}

// ---------------------------------------------------------------------------
// Directories
// ---------------------------------------------------------------------------
//
// **These are Rust rather than C, and the reason is not the scheduler.** The
// four above wrap ISO C because `fopen` and friends are the same call on every
// platform Khora targets. There is no such call for a directory: POSIX has
// `opendir`/`readdir` and Windows has `FindFirstFile`, and `readdir` hands back
// a `struct dirent` whose `d_name` sits at an offset that differs between
// Linux and macOS. Binding that from Khora means encoding three layouts and
// being wrong about one of them on a machine nobody here owns.
//
// So the platform seam is here, where `std::fs` in Rust has already made the
// choice correctly, and `std::fs` in Khora stays one module rather than three.
// The blocking pool still applies: a directory read reaches a disk.

/// Turns a NUL-terminated C string into a path.
///
/// # Safety
///
/// `path` must be NUL-terminated and valid for the call.
unsafe fn path_of(path: *const u8) -> Option<std::path::PathBuf> {
    if path.is_null() {
        return None;
    }
    // SAFETY: the caller promises a NUL-terminated string that outlives this.
    let text = unsafe { core::ffi::CStr::from_ptr(path as *const core::ffi::c_char) };
    text.to_str().ok().map(std::path::PathBuf::from)
}

/// `mkdir`, off the worker. Zero for success.
///
/// The parents are **not** created. A caller that wants that can walk the path
/// itself, and one that did not ask for it should be told the parent is
/// missing rather than have four directories appear.
///
/// # Safety
///
/// `path` must be a NUL-terminated string valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_create_dir(path: *const u8) -> i32 {
    let path = path as usize;
    blocking(move || {
        // SAFETY: the string belongs to a fiber suspended for the whole call.
        let Some(path) = (unsafe { path_of(path as *const u8) }) else { return 1 };
        if std::fs::create_dir(path).is_ok() { 0 } else { 1 }
    })
}

/// `rmdir`, off the worker. Zero for success.
///
/// Empty directories only, which is `rmdir`'s own rule and worth keeping: a
/// recursive delete that removed the wrong tree is the one file system mistake
/// with no undo.
///
/// # Safety
///
/// As above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_remove_dir(path: *const u8) -> i32 {
    let path = path as usize;
    blocking(move || {
        // SAFETY: as above.
        let Some(path) = (unsafe { path_of(path as *const u8) }) else { return 1 };
        if std::fs::remove_dir(path).is_ok() { 0 } else { 1 }
    })
}

/// Whether the path is a directory. One for yes, zero for anything else.
///
/// # Safety
///
/// As above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_is_dir(path: *const u8) -> i32 {
    let path = path as usize;
    blocking(move || {
        // SAFETY: as above.
        let Some(path) = (unsafe { path_of(path as *const u8) }) else { return 0 };
        i32::from(path.is_dir())
    })
}

/// Opens a directory for reading, or null.
///
/// The handle is a boxed [`std::fs::ReadDir`]. It is opaque to Khora exactly
/// as `FILE *` is, and has to be closed with [`khora_fs_dir_close`].
///
/// # Safety
///
/// As above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_dir_open(path: *const u8) -> *mut c_void {
    let path = path as usize;
    blocking(move || {
        // SAFETY: as above.
        let Some(path) = (unsafe { path_of(path as *const u8) }) else { return 0 };
        match std::fs::read_dir(path) {
            Ok(entries) => Box::into_raw(Box::new(entries)) as usize,
            Err(_) => 0,
        }
    }) as *mut c_void
}

/// The next entry's name, written into `into`.
///
/// Returns the number of bytes written, `0` when the directory is exhausted,
/// and `-1` when something went wrong -- including a name too long for the
/// buffer. **The entry is consumed either way**, so a caller cannot retry with
/// a bigger buffer; the buffer is sized once, generously, by the caller, and a
/// name longer than that is an error rather than a partial read. `NAME_MAX` is
/// 255 on ext4 and 255 UTF-16 units on NTFS, so a few kilobytes is not a
/// limit anybody meets by accident.
///
/// Only the name, never the directory it is in: joining is the caller's, and
/// `std::fs::join` is right there.
///
/// # Safety
///
/// `dir` must be a live handle from [`khora_fs_dir_open`], and `into` writable
/// for `cap` bytes. Both belong to the suspended fiber for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_dir_next(dir: *mut c_void, into: *mut u8, cap: usize) -> isize {
    if dir.is_null() || into.is_null() {
        return -1;
    }
    let (dir, into) = (dir as usize, into as usize);
    blocking(move || {
        // SAFETY: the handle came from `dir_open` and is not used concurrently:
        // its owner is one fiber, suspended for this call.
        let entries = unsafe { &mut *(dir as *mut std::fs::ReadDir) };
        let Some(entry) = entries.next() else { return 0 };
        let Ok(entry) = entry else { return -1 };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { return -1 };
        let bytes = name.as_bytes();
        if bytes.len() > cap {
            return -1;
        }
        // SAFETY: the destination is writable for `cap` bytes and `bytes` is
        // no longer than that, checked above.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), into as *mut u8, bytes.len()) };
        bytes.len() as isize
    })
}

/// Closes a directory handle.
///
/// # Safety
///
/// `dir` must be a live handle from [`khora_fs_dir_open`], and must not be
/// closed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_fs_dir_close(dir: *mut c_void) {
    if dir.is_null() {
        return;
    }
    // SAFETY: the handle came from `dir_open`, which boxed it, and the caller
    // promises this is its only close.
    drop(unsafe { Box::from_raw(dir as *mut std::fs::ReadDir) });
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

        let text = b"the pool stays out of the way";
        // SAFETY: `name` is NUL-terminated and outlives every call, the mode
        // strings are literals, each buffer is sized by the length passed with
        // it, and each handle is used only between its open and its one close.
        unsafe {
            let file = khora_fs_open(name.as_ptr(), c"wb".as_ptr().cast());
            assert!(!file.is_null(), "could not open {}", path.display());
            assert_eq!(khora_fs_write(text.as_ptr(), 1, text.len(), file), text.len());
            assert_eq!(khora_fs_close(file), 0);
        }

        let mut into = vec![0u8; text.len()];
        // SAFETY: as above.
        unsafe {
            let file = khora_fs_open(name.as_ptr(), c"rb".as_ptr().cast());
            assert!(!file.is_null());
            assert_eq!(khora_fs_read(into.as_mut_ptr(), 1, into.len(), file), text.len());
            assert_eq!(khora_fs_close(file), 0);
        }
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
            let mut into = [0u8; 7];
            // SAFETY: `name` is NUL-terminated and owned by this fiber, `into`
            // is seven bytes and seven are asked for, and the handle is closed
            // once. The fiber suspends inside each call, which is the property
            // the module note rests on -- it cannot touch `into` while the pool
            // thread has it.
            unsafe {
                let file = khora_fs_open(name.as_ptr(), c"rb".as_ptr().cast());
                assert!(!file.is_null());
                count.store(khora_fs_read(into.as_mut_ptr(), 1, 7, file), Ordering::SeqCst);
                khora_fs_close(file);
            }
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
