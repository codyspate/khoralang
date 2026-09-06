//! Turning what the checker found into what a reader sees.
//!
//! Also the small translations `usefulness` needs, which live here because they
//! exist only to feed a diagnostic: exhaustiveness reports a missing pattern,
//! and a missing pattern has to be printed.

use super::*;


/// expand nested patterns to the right column types.
pub(crate) fn ctor_for(_types: &TypeMap, variant: &VariantInfo) -> Ctor {
    ctor_for_instance(variant, &std::collections::HashMap::new())
}

/// The same, with the owning type's parameters filled in.
///
/// **A variant's field types are written in terms of its type's parameters**,
/// and until this they were read as written. `Result<A, E>`'s `Err` carries an
/// `E`, which is a `Type::Param` -- and [`field_type`] answers `Opaque` for
/// one, meaning "never reported on". So the column inside `Err` could not be
/// expanded, `Result::Err(UserError::NotFound(id))` was not seen to cover it,
/// and a `match` with an arm for every case of `UserError` was told
/// "pattern `Err(_)` not covered". Errata 58.
///
/// The scrutinee knows what `E` is -- it is `Result<Int, UserError>` and
/// carries its arguments -- so substituting them before asking is the whole
/// fix.
pub(crate) fn ctor_for_instance(
    variant: &VariantInfo,
    mapping: &std::collections::HashMap<&str, Type>,
) -> Ctor {
    Ctor::Variant {
        name: variant.name.clone(),
        fields: variant
            .fields
            .iter()
            .map(|f| field_type(&crate::unify::substitute(f, mapping)))
            .collect(),
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
        Type::Adt { name, home, args } => {
            let variants = types.variants_of(home.as_ref(), name);
            if variants.is_empty() {
                ColumnType::Unknown
            } else {
                // The declared parameters, paired with what this scrutinee
                // supplied for them. Absent for a type with none, which is the
                // ordinary case and costs an empty map.
                let params = types.adts.get(name.as_str()).cloned().unwrap_or_default();
                let mapping: std::collections::HashMap<&str, Type> =
                    params.iter().map(String::as_str).zip(args.iter().cloned()).collect();
                ColumnType::Finite(
                    variants.iter().map(|v| ctor_for_instance(v, &mapping)).collect(),
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
    all.extend(malformed_with_clause_errors(db, file));
    all.extend(row_fields_must_be_effects(db, file));
    all.extend(crate::unresolved::unresolved_type_errors(db, file));
    all.extend(crate::exports::export_errors(db, file));
    all.extend(check_file(db, file).iter().cloned());
    all
}

/// A `row` whose fields are not capabilities.
///
/// **This is the reason a row is its own declaration.** `type Deps = { db: Db }`
/// could have been reused in `with` position and spliced, and nothing could
/// then have asked whether `Db` was an effect -- a record's fields are ordinary
/// types and `{ db: Int }` is a perfectly good record. A row is not: every
/// entry is a capability, so `row Deps = { db: Int }` is refused here rather
/// than becoming a requirement no handler can satisfy and failing at each call
/// site with a message about `Int`.
///
/// A field whose type resolves to nothing is left alone; that is
/// [`crate::unresolved::unresolved_type_errors`]'s to report, and saying it
/// twice helps nobody.
pub(crate) fn row_fields_must_be_effects(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let items = khora_hir::item_map(db, file);
    let scope = khora_hir::file_scope(db, file);
    let kind_of = |name: &str| -> Option<khora_hir::ItemKind> {
        items
            .items
            .iter()
            .find(|i| i.name == name)
            .map(|i| i.kind)
            .or_else(|| scope.origins.iter().find(|o| o.local == name).map(|o| o.kind))
    };

    let mut found = Vec::new();
    for decl in khora_db::parse(db, file).source_file().decls() {
        let ast::Decl::Row(r) = decl else { continue };
        let Some(body) = r.definition() else { continue };
        for field in body.fields() {
            let (Some(label), Some(ast::Type::Path(p))) = (
                field.name().and_then(|n| n.ident()),
                field.ty(),
            ) else {
                continue;
            };
            let Some(name) = p.path().map(|path| path.text_path()) else { continue };
            // A builtin is declared nowhere, so `kind_of` cannot see it and
            // `row Deps = { count: Int }` would have passed in silence.
            let described = match kind_of(&name) {
                Some(khora_hir::ItemKind::Effect) => continue,
                Some(other) => other.describe(),
                None if crate::COMPILER_KNOWN.contains(&name.as_str()) => "built-in type",
                // Unresolved is somebody else's error.
                None => continue,
            };
            found.push(HirError {
                message: format!(
                    "`{label}: {name}` is not a capability: a `row` names the effects a \
                     function requires, and `{name}` is a {described}. Every field of a row \
                     is something a `with` block supplies a handler for"
                ),
                range: field.syntax().text_range(),
            });
        }
    }
    found
}

/// A `with` clause that names a type rather than a row.
///
/// **A capability is supplied under a label**, so a `with` clause wants
/// `{ name: Type }` or a row variable. `row_of_syntax` shares its fallback arm
/// with `raises`, and that arm labels an entry after its own type -- right for
/// `raises DbError`, and for `with` it produces an entry whose label is the
/// type as written.
///
/// When the type is a bare name that is writable: `with Ledger` becomes an
/// entry called `Ledger`, and `with { Ledger: handler }` supplies it.
/// Unconventional, since capabilities are lowercase by habit, but not wrong.
///
/// When it is a *path* it is not writable by anybody. `with Self::Effects` --
/// the shape somebody writing a `Stream` reaches for first -- becomes an entry
/// labelled `Self::Effects`, and no `with` block can name it. The call site
/// says so now (`check.rs`, `Clause::label_is_well_formed`), but only for the
/// callee; a function declaring one is the mistake itself and is reported here,
/// against the clause, whether or not anybody calls it.
///
/// **Both this and the call-site message exist, and neither is redundant.**
/// This one fires against the declaration, so it is what the author of the bad
/// clause sees. The call-site one fires in whichever file *calls* it, which is
/// where the declaration is somebody else's and this error is not in view.
///
/// `docs/design/effect-survey.md` 3.4 has how this was found.
pub(crate) fn malformed_with_clause_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let parsed = khora_db::parse(db, file);
    let homes = crate::type_homes(db, file);
    let mut found = Vec::new();
    for decl in parsed.source_file().decls() {
        for node in decl.syntax().descendants() {
            let Some(clause) = ast::WithClause::cast(node) else { continue };
            let Some(ast::Type::Path(path_type)) = clause.row() else { continue };
            // `with 'r` is the whole row and is exactly right.
            if path_type.row_var().is_some() {
                continue;
            }
            let Some(path) = path_type.path() else { continue };
            if path.segments().count() <= 1 {
                continue;
            }
            let written = path.text_path();
            // `with Deps` where `Deps` is a `row` declaration is the whole
            // point of the feature: the fields are spliced in.
            if homes.row(&written).is_some() {
                continue;
            }
            let segments: Vec<String> = path.segments().filter_map(|s| s.ident()).collect();
            // `with { name: Effects }` is good advice for `m::Ledger` and bad
            // advice for `Self::Effects`, where the last segment is an
            // associated type and not an effect at all.
            let fix = match segments.first().map(String::as_str) {
                Some("Self") => "Give it a label: `with { name: Type }`".to_string(),
                _ => format!(
                    "Give it a label: `with {{ name: {} }}`",
                    segments.last().cloned().unwrap_or_default()
                ),
            };
            found.push(HirError {
                message: format!(
                    "`with {written}` names a type, not a row. A capability is supplied under \
                     a label, so this asks for one called `{written}`, which no `with` block \
                     can write. {fix}"
                ),
                range: clause.syntax().text_range(),
            });
        }
    }
    found
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
