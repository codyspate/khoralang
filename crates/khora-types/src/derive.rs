//! What a `derive` is allowed to ask for.
//!
//! `khora_hir::derive` writes the impls; this decides whether it should have.
//! The split is not tidiness — that pass runs before anything in the compiler
//! knows what a trait is, and every question worth asking about a `derive`
//! ("does `Foo` implement `Eq`?", "does `Ord` require something else?") needs
//! exactly that knowledge.
//!
//! # Refusing rather than guessing
//!
//! A derived `Eq` is a promise that two values are equal when their fields are.
//! It can only keep that promise if each field can answer, so a field whose
//! type has no `Eq` is refused — by name, and at the `derive`, which is the
//! line the author has to change. The alternative, comparing what can be
//! compared and ignoring the rest, is an `eq` that returns true for values that
//! differ, and a `Map` that loses entries because of it.
//!
//! The impl is still generated for a refused derive. Withdrawing it would turn
//! one accurate sentence into that sentence plus one "`Point` does not
//! implement `Eq`" for every call in the program, and the reader has to scroll
//! past all of them to find the one that says what to do. What is withdrawn
//! instead is the *derived body's* diagnostics: they would say the same thing
//! in worse words, at the same place.

use khora_db::{Db, SourceFile};
use khora_hir::derive::DerivedImpl;
use khora_hir::HirError;

use crate::{type_map, Type, TypeMap, VariantInfo};

/// Everything the `derive` clauses of one file got wrong.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeriveReport {
    pub errors: Vec<HirError>,
    /// The body keys of derives that were refused.
    ///
    /// Their bodies are still lowered and still checked — a derived impl the
    /// checker rejects for a reason this pass did *not* predict is a bug in
    /// the expander, and that has to stay visible — but their diagnostics are
    /// dropped, because this pass has already said the same thing better.
    pub refused: Vec<String>,
}

/// Checks a file's `derive` clauses against what its traits actually are.
#[salsa::tracked(returns(ref))]
pub fn derive_report(db: &dyn Db, file: SourceFile) -> DeriveReport {
    let types = type_map(db, file);
    let mut out = DeriveReport::default();

    for derived in &khora_hir::derive::derived(db, file).impls {
        // Not a trait in scope. `traits::check` already says so about the impl
        // this expanded to, and says it at the `derive`, so there is nothing to
        // add — but there is plenty to suppress: without the trait, every field
        // fails the test below and the reader gets one error per field for a
        // missing import.
        let Some(def) = types.traits.traits.get(&derived.trait_name) else {
            out.refused.push(derived.body_key());
            continue;
        };

        // `trait Ord: Eq` and `trait Hash: Eq` are not decoration. A `Map`
        // finds a key by hashing it and then comparing, so a type that hashes
        // and cannot compare is a key that can be inserted and never found.
        //
        // Required rather than implied. `derive(Ord)` quietly writing an `Eq`
        // as well means a reader of the declaration cannot see what the type
        // implements, and someone who wanted their own `Eq` would find it
        // conflicting with one they did not ask for. Naming both is one extra
        // word and no surprises.
        let self_type = Type::adt(&derived.type_name);
        for supertrait in &def.supertraits {
            if types.traits.satisfies(supertrait, &self_type) {
                continue;
            }
            out.errors.push(HirError {
                message: format!(
                    "`{}` requires `{supertrait}`, and `{}` does not implement it. Add \
                     `{supertrait}` to this `derive`, or write `impl {supertrait} for {}` \
                     by hand",
                    derived.trait_name, derived.type_name, derived.type_name
                ),
                range: derived.at,
            });
        }

        let mut refused = false;
        for variant in types.variants.iter().filter(|v| v.type_name == derived.type_name) {
            for (index, field) in variant.fields.iter().enumerate() {
                if implements(types, &derived.trait_name, field) {
                    continue;
                }
                refused = true;
                out.errors.push(HirError {
                    message: format!(
                        "`derive({})` needs every field to implement `{}`, and {} has type \
                         `{field}`, which does not",
                        derived.trait_name,
                        derived.trait_name,
                        describe(derived, variant, index)
                    ),
                    range: derived.at,
                });
            }
        }
        if refused {
            out.refused.push(derived.body_key());
        }
    }

    out
}

/// Whether a field's type can answer for `trait_name`.
fn implements(types: &TypeMap, trait_name: &str, field: &Type) -> bool {
    match field {
        // The type's own parameter, which the generated impl bounded by the
        // trait it is deriving — `impl<A: Eq> Eq for Box<A>`. The obligation
        // moves to whoever supplies the `A`, which is exactly where it belongs
        // and where the ordinary bound machinery already reports it.
        Type::Param(_) => true,
        // A hole a *different* error already accounts for. Saying "this field
        // does not implement `Eq`" about a type name that did not resolve
        // sends the reader after the wrong problem.
        Type::Unknown => true,
        _ => types.traits.satisfies(trait_name, field),
    }
}

/// How to name the field that failed, in the words its declaration used.
fn describe(derived: &DerivedImpl, variant: &VariantInfo, index: usize) -> String {
    match variant.labels.get(index) {
        Some(label) if derived.is_record => format!("the field `{label}`"),
        Some(label) => format!("the field `{label}` of `{}`", variant.name),
        // A positional payload has no name to use, so its position is what the
        // reader has to count to.
        None => format!("field {index} of `{}`", variant.name),
    }
}
