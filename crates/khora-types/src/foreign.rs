//! What may cross the C ABI, and why anything else may not.
//!
//! **Scalars and pointers only.** `docs/design/ffi.md` §1 has the contract and
//! errata 35 has the reason: a 16-byte aggregate crossed between generated
//! code and the runtime, LLVM and rustc disagreed about how one comes back on
//! x86-64 Windows, and every failing test reported as passing.
//!
//! Here rather than in the backend, where this began. The rule is a fact about
//! *types*, and it is now needed in two places that answer to different
//! audiences: the backend refuses to generate a call that would cross badly,
//! and the checker refuses an `pub extern fn` whose signature could not be
//! called from C at all. The second is why it moved — an export is part of a
//! library's published ABI whether or not any Khora code calls it, so it has
//! to be reported at the declaration and therefore by `khora check`.

use crate::{Signature, Type};

/// Why this type cannot cross the C ABI, or `None` if it can.
///
/// **Scalars and pointers only.** The rule comes from errata 35, where a
/// 16-byte aggregate crossed between generated code and the runtime and the
/// two sides disagreed about how one comes back — silently, in the direction
/// that made every failing test report as passing. The runtime is only the
/// first foreign library; a binding the user writes is the same boundary.
///
/// `docs/design/ffi.md` has the full contract.
pub fn foreign_obstacle(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Int | Type::Fixed(_) | Type::Float | Type::Bool | Type::Ptr => None,
        Type::Str => Some(
            "a `String` is a reference-counted heap object with a header the C side knows \
             nothing about; pass its bytes and length instead",
        ),
        Type::Adt { .. } | Type::Tuple(_) => Some(
            "a Khora object is a reference-counted heap allocation, so the foreign side \
             would get a pointer it cannot read and a reference it cannot release",
        ),
        Type::Fn { .. } => Some(
            "a closure is a heap object holding its captures, and C expects a bare function \
             pointer",
        ),
        Type::Param(_) | Type::Applied { .. } | Type::Assoc { .. } | Type::Var(_) => Some(
            "a generic function has no single machine signature, and there is no body to \
             specialize",
        ),
        Type::Unit => Some("`()` is not a value; a foreign function may only *return* it"),
        Type::Row { .. } | Type::Const(_) | Type::Never | Type::Unknown => {
            Some("it is not a type the C ABI has")
        }
    }
}

/// Why this whole signature cannot be a foreign function's, if it cannot.
///
/// Checked where the call is generated rather than at the declaration, so an
/// unused binding to something this target does not have is not an error on a
/// target that does not need it.
pub fn foreign_signature_obstacle(signature: &Signature) -> Option<String> {
    if !signature.generics.is_empty() {
        return Some(
            "it is generic, and a generic function has no single machine signature".to_string(),
        );
    }
    if can_raise(signature) {
        return Some(
            "it can raise, and a fallible function returns a tagged pair — which is exactly \
             the aggregate that must not cross (errata 35). C reports failure in its return \
             value, and the wrapper that turns that into a raise belongs in Khora"
                .to_string(),
        );
    }
    for param in &signature.params {
        if let Some(why) = foreign_obstacle(param) {
            return Some(format!("its parameter of type `{param}` cannot cross: {why}"));
        }
    }
    // `()` and `Never` are the two returns that are not values. `()` because
    // it carries nothing, and `Never` because there is no return: a symbol
    // declared `-> Never` is one the C side never comes back from, so nothing
    // crosses and there is nothing for the boundary rule to be about. Without
    // this, `khora_bounds_fail` -- which is `-> !` in Rust and diverges -- had
    // to be declared `-> ()` and lie about what it does.
    if !matches!(signature.ret, Type::Unit | Type::Never) {
        if let Some(why) = foreign_obstacle(&signature.ret) {
            return Some(format!("its return type `{}` cannot cross: {why}", signature.ret));
        }
    }
    None
}


/// Whether a signature's `raises` row has anything in it.
pub fn can_raise(signature: &Signature) -> bool {
    match &signature.raises {
        Type::Row { fields, tail } => !fields.is_empty() || tail.is_some(),
        _ => false,
    }
}

