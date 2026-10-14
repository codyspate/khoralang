//! What a wasm build has where the native runtime has fibers: one thread of
//! control, so nothing to cancel and one owner for every region.
//!
//! **What this prevents: a wasm module that does not link because it has a
//! loop in it.** Generated code is the same on every target. Every loop
//! back-edge and every `!` loads [`khora_poll`] and may call
//! [`khora_canceled`] or [`khora_back_edge`], and every `Region` and `scoped`
//! calls `khora_region_*`. On the native targets those live in `poll`,
//! `cancel` and `region`, which need the scheduler and `crate::current`; a
//! wasm module that reached any of them failed in `wasm-ld` naming the
//! symbol.
//!
//! What it costs, and does not deliver: nothing inside a wasm module can
//! cancel anything, so the poll word is zero for good and a runaway loop is
//! stopped only by whatever the host uses to stop a module (fuel, an epoch
//! deadline, a Worker's CPU limit). Finalizers do run, in reverse order, when
//! their region is released -- that part is not optional on any target.

use super::*;
use crate::heap::{khora_alloc, khora_drop, khora_dup};
use std::cell::Cell;
use std::sync::atomic::AtomicU64;

/// The word every back-edge and `!` loads first. Zero for good: nothing in a
/// wasm module is ever canceled, so generated code never takes the slow path.
///
/// Exported under this name because generated code loads it directly.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static khora_poll: AtomicU64 = AtomicU64::new(0);

/// Whether a cancellation should be acted on here: never, on wasm.
///
/// Reached only if [`khora_poll`] is non-zero, which it never is; defined so
/// the reference generated code makes to it resolves.
#[unsafe(no_mangle)]
pub extern "C" fn khora_canceled() -> u8 {
    0
}

/// A loop's back-edge: no scheduler to give the thread back to, and nothing
/// to cancel. Reached under the same condition as [`khora_canceled`].
#[unsafe(no_mangle)]
pub extern "C" fn khora_back_edge() -> u8 {
    0
}

/// A function's entry poll: nothing to cancel. Reached under the same
/// condition as [`khora_canceled`].
#[unsafe(no_mangle)]
pub extern "C" fn khora_entry_canceled() -> u8 {
    0
}

/// One deferred finalizer: the closure, its glue, and how to call it. The
/// same three the native `region` keeps, for the same reasons.
struct Finalizer {
    closure: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
    call: Option<Trampoline1>,
}

/// The tag every region object carries. Regions are not an ADT.
const REGION_TAG: u32 = 0;

thread_local! {
    /// The region that ends when the program does, opened on first use.
    ///
    /// One thread of control, so no owner check and no lock: the native
    /// region's owner field exists to trap a region that reached another
    /// fiber, and here there is no other fiber for it to reach.
    static ROOT: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
}

/// Opens a region: a counted object whose one field points at its list of
/// finalizers. Released through [`khora_drop`] like any other object, which
/// is what runs the list.
#[unsafe(no_mangle)]
pub extern "C" fn khora_region_open() -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Vec<Finalizer>>() as u64, REGION_TAG);
    let list: Box<Vec<Finalizer>> = Box::default();
    // SAFETY: `khora_alloc` returned one field's worth of zeroed, aligned
    // space, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Vec<Finalizer>>().write(Box::into_raw(list));
    }
    object
}

/// A reference to the root region, opened on the first call.
///
/// # Safety
///
/// None beyond the native function's: the caller owns the reference returned
/// and releases it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_root() -> *mut u8 {
    ROOT.with(|root| {
        if root.get().is_null() {
            root.set(khora_region_open());
        }
        let region = root.get();
        // SAFETY: a live region, held by the slot until `close_root`.
        unsafe { khora_dup(region) };
        region
    })
}

/// Releases the root region, running whatever was deferred to it. A second
/// call is a no-op.
///
/// # Safety
///
/// Must be called after every other Khora frame has returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_close_root() {
    let root = ROOT.with(|root| root.replace(std::ptr::null_mut()));
    if !root.is_null() {
        // SAFETY: the reference the slot held, given up here and nowhere else.
        unsafe { khora_drop(root, Some(release_shim)) };
    }
}

/// [`khora_region_release`] as the safe `drop_fields` callback generated code
/// and [`khora_drop`] pass.
// SHARE: releases a region; see `khora_region_release`.
pub(crate) extern "C" fn release_shim(region: *mut u8) {
    // SAFETY: reached only through `khora_drop`, with the object whose last
    // reference it has just released.
    unsafe { khora_region_release(region) };
}

/// Registers a finalizer to run when `region` ends. Takes ownership of
/// `closure`.
///
/// # Safety
///
/// `region` must be a live object from [`khora_region_open`], `closure` a live
/// `() -> ()` closure whose drop routine is `glue`, and `call`, when given,
/// its trampoline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_defer(
    region: *mut u8,
    closure: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
    call: Option<Trampoline1>,
) {
    if region.is_null() {
        fatal("deferring a finalizer to a null region");
    }
    // SHARE: stores the closure; not a crossing. A single-fibered target has
    // one owner for every region, so nothing that runs the closure is on
    // another thread, and it is never marked.
    // SAFETY: the caller guarantees a live region, whose field holds what
    // `khora_region_open` wrote there.
    let list = unsafe { *region.add(KHORA_FIELD_OFFSET).cast::<*mut Vec<Finalizer>>() };
    if list.is_null() {
        fatal("deferring a finalizer to a region that has already been released");
    }
    // SAFETY: non-null, so the box `khora_region_open` made is alive, and one
    // thread of control means nothing else holds a reference into it.
    unsafe { (*list).push(Finalizer { closure, glue, call }) };
}

/// Runs a region's finalizers, last deferred first, and frees its list.
///
/// # Safety
///
/// `region` must be a live object from [`khora_region_open`] whose count has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_release(region: *mut u8) {
    // SHARE: releases, and runs finalizers on the only fiber there is, so
    // nothing here crosses.
    if region.is_null() {
        return;
    }
    // SAFETY: a live region has a field's worth of space past the header.
    let slot = unsafe { region.add(KHORA_FIELD_OFFSET).cast::<*mut Vec<Finalizer>>() };
    // SAFETY: the field holds what `khora_region_open` wrote, or null once
    // released.
    let list = unsafe { *slot };
    if list.is_null() {
        return;
    }
    // Cleared first, so a finalizer that defers to this region again is
    // refused rather than pushing onto the list being drained.
    //
    // SAFETY: the same field, still inside the live object.
    unsafe { slot.write(std::ptr::null_mut()) };
    // SAFETY: from `Box::into_raw` in `khora_region_open`, and the null just
    // written means nothing frees it twice.
    let list = unsafe { Box::from_raw(list) };

    for finalizer in list.into_iter().rev() {
        // SAFETY: a closure's first field is its code pointer, called with
        // the closure itself, through the trampoline generated code passed
        // to `khora_region_defer`, or directly for a Rust closure, which
        // returns nothing. The isolated drain is the native release's: a
        // scope that ends inside the finalizer ends there.
        unsafe {
            let code = *finalizer.closure.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
            let isolated = crate::heap::Isolated::new();
            match finalizer.call {
                Some(call) => {
                    let mut answer: u64 = 0;
                    let _which = call(code, finalizer.closure, &raw mut answer);
                }
                None => {
                    let call: extern "C" fn(*mut u8) = std::mem::transmute(code);
                    call(finalizer.closure);
                }
            }
            drop(isolated);
            khora_drop(finalizer.closure, finalizer.glue);
        }
    }
}
