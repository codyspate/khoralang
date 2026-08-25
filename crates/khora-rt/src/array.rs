//! Arrays: a length, an element width, and the bytes.
//!
//! The layout is a contract with the code generator, which reads and writes
//! elements directly — reading one is a bounds check and a load rather than a
//! call. What the runtime owns is allocation and release, both of which need
//! the length at run time, and releasing an element needs drop glue that
//! travels in the object because the runtime does not know what an element is.

use super::*;
use crate::heap::{khora_alloc, khora_drop, khora_dup};

/// Field index of an array's length.
///
/// These four are `pub` because generated code reaches the elements directly —
/// an array read is a GEP and a load, not a call — so the layout is a contract
/// with the code generator exactly as the object header is, and it is written
/// down once here rather than twice.
pub const ARRAY_LEN_FIELD: usize = 0;
/// Field index of the routine that releases one element, or null.
pub const ARRAY_GLUE_FIELD: usize = 1;
/// Field index of the flag saying whether elements are counted at all.
pub const ARRAY_BOXED_FIELD: usize = 2;
/// Field index of the element size in bytes: 1, 2, 4 or 8.
///
/// An `Array<U8>` is a byte per element, not a word per element. A byte buffer
/// that costs eight bytes a byte is not a byte buffer, and every wire format,
/// file and string is one.
pub const ARRAY_STRIDE_FIELD: usize = 3;
/// How many fields precede the elements.
///
/// A whole number of words, so element zero is word-aligned and every stride
/// that divides a word is aligned along with it.
pub const ARRAY_HEADER_FIELDS: usize = 4;

/// The tag every array carries. Arrays are not an ADT, so nothing competes.
const ARRAY_TAG: u32 = 0;

/// Bytes of one field slot. Every Khora value is one word wide.
const FIELD_WORD: usize = std::mem::size_of::<usize>();

fn array_word(array: *const u8, index: usize) -> usize {
    // SAFETY: the caller guarantees a live array, whose first fields were
    // written by `khora_array_new`.
    unsafe { *array.add(KHORA_FIELD_OFFSET + index * FIELD_WORD).cast::<usize>() }
}

/// Allocates an array of `len` zeroed elements.
///
/// `boxed` says whether the elements are reference counted, and `glue` is what
/// releases one — the same pair every drop site already passes, kept in the
/// object because the length is only known at run time and the loop that
/// releases the elements therefore has to be too.
///
/// Zeroed matters: a null slot is what makes releasing an array that was never
/// filled a no-op rather than a wild free, the same reason `khora_alloc` zeroes.
#[unsafe(no_mangle)]
pub extern "C" fn khora_array_new(
    len: i64,
    fill: u64,
    stride: u8,
    boxed: u8,
    glue: Option<extern "C" fn(*mut u8)>,
) -> *mut u8 {
    if len < 0 {
        fatal("an array cannot have a negative length");
    }
    if !matches!(stride, 1 | 2 | 4 | 8) {
        fatal("an array element must be 1, 2, 4 or 8 bytes wide");
    }
    let len = len as usize;
    let stride = stride as usize;
    // Header first, then `len * stride` bytes rounded up to a whole word so the
    // allocation stays word-aligned and the next object after it does too.
    let Some(bytes) = len.checked_mul(stride) else {
        fatal("an array that large cannot be allocated");
    };
    let Some(fields) = bytes
        .checked_add(FIELD_WORD - 1)
        .map(|b| b / FIELD_WORD)
        .and_then(|words| words.checked_add(ARRAY_HEADER_FIELDS))
        .and_then(|n| n.checked_mul(FIELD_WORD))
    else {
        fatal("an array that large cannot be allocated");
    };

    let object = khora_alloc(fields as u64, ARRAY_TAG);
    // SAFETY: `khora_alloc` returned space for the header fields and the
    // elements, zeroed and aligned.
    unsafe {
        let base = object.add(KHORA_FIELD_OFFSET).cast::<usize>();
        base.add(ARRAY_LEN_FIELD).write(len);
        base.add(ARRAY_GLUE_FIELD).write(glue.map_or(0, |g| g as usize));
        base.add(ARRAY_BOXED_FIELD).write(usize::from(boxed != 0));
        base.add(ARRAY_STRIDE_FIELD).write(stride);

        // Every slot holds the same value and every slot owns it, so the count
        // goes up once per slot. The caller keeps its own reference and
        // releases it after the call, which is why this does not take one.
        //
        // The fill arrives as a whole word and only its low `stride` bytes are
        // written, which is the same thing on a little-endian target — and both
        // targets Khora has are little-endian. A big-endian port would take the
        // *high* bytes, and this is where it would say so.
        let elements = base.add(ARRAY_HEADER_FIELDS).cast::<u8>();
        let source = fill.to_le_bytes();
        for index in 0..len {
            elements.add(index * stride).copy_from_nonoverlapping(source.as_ptr(), stride);
            if boxed != 0 {
                khora_dup(fill as *mut u8);
            }
        }
    }
    object
}

/// Releases every element of an array, then the array.
///
/// A `drop_fields` callback. The loop is here rather than generated because
/// the length is a run-time value; what to do with one element is generated,
/// and travels in the object as `glue`.
///
/// # Safety
///
/// `array` must be a live object from [`khora_array_new`] whose refcount has
/// reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn khora_array_release(array: *mut u8) {
    if array.is_null() {
        return;
    }
    if array_word(array, ARRAY_BOXED_FIELD) == 0 {
        return;
    }
    let len = array_word(array, ARRAY_LEN_FIELD);
    let glue = array_word(array, ARRAY_GLUE_FIELD);
    // SAFETY: a null glue is `None`, which is how a boxed element that owns
    // nothing is released — the same convention `khora_drop` already uses.
    let glue: Option<extern "C" fn(*mut u8)> =
        if glue == 0 {
            None
        } else {
            unsafe { Some(std::mem::transmute::<usize, extern "C" fn(*mut u8)>(glue)) }
        };

    for index in 0..len {
        debug_assert_eq!(
            array_word(array, ARRAY_STRIDE_FIELD),
            FIELD_WORD,
            "a counted element is a pointer, so it is always a whole word wide"
        );
        // SAFETY: every element slot is within the allocation, and holds
        // either null or a live object this array owned a reference to.
        unsafe {
            let slot = array
                .add(KHORA_FIELD_OFFSET + (ARRAY_HEADER_FIELDS + index) * FIELD_WORD)
                .cast::<*mut u8>();
            khora_drop(*slot, glue);
        }
    }
}
