//! Turning what the checker found into what a reader sees.
//!
//! Also the small translations `usefulness` needs, which live here because they
//! exist only to feed a diagnostic: exhaustiveness reports a missing pattern,
//! and a missing pattern has to be printed.

use super::*;


/// expand nested patterns to the right column types.
pub(crate) fn ctor_for(_types: &TypeMap, variant: &VariantInfo) -> Ctor {
    Ctor::Variant {
        name: variant.name.clone(),
        fields: variant.fields.iter().map(field_type).collect(),
    }
}

pub(crate) fn field_type(ty: &Type) -> FieldType {
    match ty {
        Type::Adt { name, .. } => FieldType::Named(name.clone()),
        Type::Bool => FieldType::Named(BOOL_TYPE.to_string()),
        Type::Int | Type::Str => FieldType::Unbounded,
        _ => FieldType::Opaque,
    }
}

pub(crate) fn column_type(types: &TypeMap, ty: &Type) -> ColumnType {
    match ty {
        Type::Bool => ColumnType::Finite(vec![Ctor::Bool(true), Ctor::Bool(false)]),
        Type::Int | Type::Str => ColumnType::Unbounded,
        Type::Adt { name, home, .. } => {
            let variants = types.variants_of(home.as_ref(), name);
            if variants.is_empty() {
                ColumnType::Unknown
            } else {
                ColumnType::Finite(
                    variants.iter().map(|v| ctor_for(types, v)).collect(),
                )
            }
        }
        _ => ColumnType::Unknown,
    }
}


/// `Bool` has constructors but is not an ADT, so the resolver needs a name for
/// it. Lowercase, which no declared type can be.
pub(crate) const BOOL_TYPE: &str = "bool";


/// The type a constructor belongs to, and the constructor's own name.
///
/// Always prefer this to [`variant_name`] when looking a constructor up: the
/// name alone is ambiguous across types.
pub(crate) fn variant_case(
    resolution: &khora_hir::Resolution,
) -> Option<(Option<khora_hir::ModulePath>, String, String)> {
    match resolution {
        // The module comes along. The resolver has already decided which
        // declaration this is, and dropping that here was how two `Point`s
        // became one again three lines later.
        khora_hir::Resolution::Variant { module, type_name, name } => {
            Some((Some(module.clone()), type_name.clone(), name.clone()))
        }
        _ => None,
    }
}

/// Every semantic diagnostic for one file: name resolution and lowering
/// errors from `khora-hir`, then type errors.
///
/// Lowering errors come first because a name that did not resolve makes the
/// type error that follows it noise.
#[salsa::tracked(returns(ref))]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let mut all: Vec<HirError> = khora_hir::item_map(db, file).errors.clone();
    // An import that resolved to nothing is the most useful thing to say about
    // a file full of "cannot find" errors downstream of it.
    all.extend(khora_hir::file_scope(db, file).errors.iter().cloned());
    // What the `derive` clauses asked for, before what they expanded to. A
    // `derive` that cannot be honoured makes everything after it about the
    // impl the compiler wrote rather than the line the reader wrote.
    all.extend(khora_hir::derive::derived(db, file).errors.iter().cloned());
    all.extend(derive::derive_report(db, file).errors.iter().cloned());
    for (_, body) in khora_hir::body::bodies(db, file) {
        all.extend(body.errors.iter().cloned());
    }
    all.extend(trait_errors(db, file).iter().cloned());
    all.extend(shadowed_name_errors(db, file));
    all.extend(check_file(db, file).iter().cloned());
    all
}

/// Refuses a declaration that takes a name the compiler already means.
///
/// Reported against the declaration rather than against a use, because the use
/// is not the mistake and there may be no use at all — a `type Array` that is
/// never mentioned still produces one with the runtime's array layout the
/// moment somebody does mention it.
///
/// Refusing is the blunt answer and it is deliberately blunt. What the phase
/// asked for is that a lookalike receive no privilege, which wants a type to
/// know the declaration it came from; a `Type::Adt` knows a `String` and
/// nothing else, so there is no way to tell the two apart downstream. Between
/// a program that is refused with a reason and a program that corrupts memory,
/// the choice is not close — and the restriction lifts by itself once identity
/// is real.
pub(crate) fn shadowed_name_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let mut found = Vec::new();
    for decl in khora_db::parse(db, file).source_file().decls() {
        let ast::Decl::Type(t) = decl else { continue };
        // No right-hand side is a declaration of the builtin rather than a
        // competing definition of it. See `collides_with_a_builtin`.
        if t.definition().is_none() {
            continue;
        }
        let Some(name) = t.name().and_then(|n| n.ident()) else { continue };
        if !collides_with_a_builtin(&name) {
            continue;
        }
        found.push(HirError {
            message: format!(
                "`{name}` is a name the compiler already means, so this definition would \
                 be ignored in favour of the built-in one — and the value would still be \
                 given the built-in's layout, which is memory corruption rather than a \
                 shadowed name. Rename it, or drop the `=` to declare the built-in \
                 instead"
            ),
            range: t.syntax().text_range(),
        });
    }
    found
}

/// Whether `==` on this type has to go through an `Eq` impl.
///
/// The scalars compare with one instruction and `String` by its bytes, so those
/// are primitive. Everything with a shape needs a decision about what equality
/// *means* for it, and the type is the only thing that can make it.
///
/// A type still being inferred is not asked: whatever it turns out to be, the
/// question is answered where it is answered, and guessing here would report
/// against whichever expression happened to be visited first.
pub(crate) fn needs_an_eq_impl(ty: &Type) -> bool {
    !matches!(
        ty,
        Type::Int
            | Type::Fixed(_)
            | Type::Float
            | Type::Bool
            | Type::Str
            | Type::Unit
            | Type::Var(_)
            | Type::Never
            | Type::Unknown
    )
}

/// Whether `<` on this type has to go through an `Ord` impl.
///
/// Nearly [`needs_an_eq_impl`], and `String` is the difference. Two strings
/// compare for *equality* by their bytes, which is one runtime call and the
/// only answer anybody wants; which of them comes *first* is a different
/// question with several defensible answers — bytes, code points, a locale —
/// and the one a program means belongs in an impl it can read.
pub(crate) fn needs_an_ord_impl(ty: &Type) -> bool {
    matches!(ty, Type::Str) || needs_an_eq_impl(ty)
}
