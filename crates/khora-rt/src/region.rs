//! Regions, and the finalizers they run.
//!
//! A region is an ordinary counted object whose release runs a list of
//! closures. That the list can grow is why its release is written here rather
//! than generated: nothing in Khora grows a value in place, so the `Vec` has to
//! live on this side. Everything else about a region is ordinary, which is what
//! makes its finalizers run on exactly the paths that already release a local —
//! including a raise passing through. `docs/design/memory.md`.

use super::*;
use crate::heap::{khora_alloc, khora_drop, khora_dup};
use std::sync::Mutex;

/// One deferred finalizer: the closure to call, and the glue to release it
/// with.
///
/// The glue travels with the closure because the runtime cannot work it out.
/// A closure's drop routine is *generated* — one shared routine switching on
/// the site tag — so the only thing that knows the pointer is the code that
/// built the closure, which is exactly the code that defers it.
#[repr(C)]
struct Finalizer {
    closure: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
}

/// A region's finalizers, in the order they were deferred.
///
/// Held Rust-side rather than as a Khora list because deferring *grows* it, and
/// nothing in Khora can grow a value in place. The Khora object is a handle:
/// one field holding a pointer to this.
type Finalizers = Mutex<Vec<Finalizer>>;

/// The tag every region object carries. Regions are not an ADT, so no variant
/// index competes for it.
const REGION_TAG: u32 = 0;

/// The region that ends when the program does.
///
/// One per program, created on first use and released by the generated entry
/// point after `main` returns — on the failing path as well as the ordinary
/// one, because a finalizer that only runs when nothing went wrong is not a
/// finalizer.
static mut ROOT: *mut u8 = std::ptr::null_mut();

/// A reference to the root region.
///
/// # Safety
///
/// Single-threaded, like everything else here: fibers running across cores
/// (A5) will need this behind the same lock the refcounts eventually go
/// behind. `docs/roadmap.md` D10.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_root() -> *mut u8 {
    // SAFETY: single-threaded per the note above.
    unsafe {
        if ROOT.is_null() {
            ROOT = khora_region_open();
        }
        khora_dup(ROOT);
        ROOT
    }
}

/// Releases the root region, running whatever was deferred to it.
///
/// Called once by the generated entry point. A second call is a no-op, so a
/// program that never touched the root region costs nothing.
///
/// # Safety
///
/// Must be called after every other Khora frame has returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_close_root() {
    // SAFETY: single-threaded, and the caller guarantees nothing else is
    // still running.
    unsafe {
        let root = ROOT;
        ROOT = std::ptr::null_mut();
        if !root.is_null() {
            khora_drop(root, Some(release_shim));
        }
    }
}

/// [`khora_region_release`] as a `drop_fields` callback.
///
/// The callback type is a *safe* `extern "C" fn`, because that is what
/// generated code passes and generated code has no notion of Rust's `unsafe`.
/// The release itself keeps its contract, so the shim is where the claim that
/// the contract holds is made — once, here, rather than at every drop site.
extern "C" fn release_shim(region: *mut u8) {
    // SAFETY: only ever reached through `khora_drop`, which calls it with the
    // object whose last reference it just released.
    unsafe { khora_region_release(region) };
}

/// Opens a region, returning a Khora object that owns it.
///
/// The object is ordinary in every way that matters — reference counted,
/// dropped by [`khora_drop`] — which is the whole design. Its release runs the
/// finalizers, so a region ends exactly when the binding holding it does: at
/// the end of a block, at an early `return`, or on a raise passing through.
/// Every one of those paths already releases a boxed local, so none of them
/// needed a new rule.
#[unsafe(no_mangle)]
pub extern "C" fn khora_region_open() -> *mut u8 {
    let object = khora_alloc(std::mem::size_of::<*mut Finalizers>(), REGION_TAG);
    let list: Box<Finalizers> = Box::default();
    // SAFETY: `khora_alloc` returned an object with one field's worth of
    // space, zeroed and aligned, and nothing else holds this pointer yet.
    unsafe {
        object.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>().write(Box::into_raw(list));
    }
    object
}

/// Registers a finalizer to run when `region` ends.
///
/// Takes ownership of `closure`: the region releases it after calling it, so
/// the caller hands over a reference of its own rather than lending one.
///
/// # Safety
///
/// `region` must be a live object from [`khora_region_open`], and `closure` a
/// live Khora closure of type `() -> ()` whose drop routine is `glue`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_defer(
    region: *mut u8,
    closure: *mut u8,
    glue: Option<extern "C" fn(*mut u8)>,
) {
    if region.is_null() {
        fatal("deferring a finalizer to a null region");
    }
    // SAFETY: the caller guarantees a live region, whose field holds the
    // pointer `khora_region_open` wrote there.
    let list = unsafe { *region.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>() };
    if list.is_null() {
        fatal("deferring a finalizer to a region that has already been released");
    }
    // Locked, because a region is shareable and so two fibers may defer to one
    // at the same moment: a fiber that acquires a connection wants it released
    // by the scope that outlives it, which is the whole point of handing a
    // `Scope` across. `std::core::Share`.
    //
    // Uncontended almost always, and a region is not a hot path — it is
    // touched when a resource is acquired, not when one is used.
    //
    // SAFETY: as above; the box is alive until the region is released, and the
    // field is the only handle to it.
    unsafe { (*list).lock().unwrap_or_else(|e| e.into_inner()).push(Finalizer { closure, glue }) };
}

/// Runs a region's finalizers and frees its list.
///
/// This is a `drop_fields` callback: [`khora_drop`] calls it when the last
/// reference to the region goes, and frees the object itself afterwards.
///
/// **Reverse order.** A finalizer deferred later may depend on one deferred
/// earlier — a transaction rolled back before the connection it ran on is
/// closed — so the last acquired is the first released, the same rule a stack
/// of scopes follows.
///
/// A finalizer that itself defers is deferring to a region that is already
/// releasing, which [`khora_region_defer`] rejects rather than silently
/// dropping.
///
/// # Safety
///
/// `region` must be a live object from [`khora_region_open`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_region_release(region: *mut u8) {
    if region.is_null() {
        return;
    }
    let slot = unsafe { region.add(KHORA_FIELD_OFFSET).cast::<*mut Finalizers>() };
    // SAFETY: the caller guarantees a live region; the field holds what
    // `khora_region_open` wrote, and nothing else reads it after this.
    let list = unsafe { *slot };
    if list.is_null() {
        return;
    }
    // Cleared before running anything, so a finalizer that reaches this region
    // again finds it released rather than re-entering the list being drained.
    unsafe { slot.write(std::ptr::null_mut()) };

    // SAFETY: the pointer came from `Box::into_raw` in `khora_region_open` and
    // has not been freed — the null check above is what guarantees that.
    let list = unsafe { Box::from_raw(list) };
    let list = list.into_inner().unwrap_or_else(|e| e.into_inner());
    for finalizer in list.into_iter().rev() {
        // SAFETY: a closure's first field is its code pointer, and a `() -> ()`
        // closure is called with its own object as the only argument. This is
        // the same convention generated code uses to call one.
        unsafe {
            let code = *finalizer.closure.add(KHORA_FIELD_OFFSET).cast::<*const u8>();
            let call: extern "C" fn(*mut u8) = std::mem::transmute(code);
            call(finalizer.closure);
            khora_drop(finalizer.closure, finalizer.glue);
        }
    }
}
