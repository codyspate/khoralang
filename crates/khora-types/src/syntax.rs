//! Written syntax to a `Type`.
//!
//! One direction only, and no inference in it — this is what a type annotation
//! *says*, resolved against the names in scope. Rows are here too, because a
//! `with` or `raises` clause is written syntax like any other.

use super::*;

/// The labels and types of a record type's fields, in written order.
pub(crate) fn record_fields(
    r: &ast::RecordType,
    generics: &[String],
    homes: &TypeHomes,
) -> (Vec<String>, Vec<Type>) {
    let mut labels = Vec::new();
    let mut fields = Vec::new();
    for f in r.fields() {
        let Some(label) = f.name().and_then(|n| n.ident()) else { continue };
        labels.push(label);
        fields.push(type_of_syntax(f.ty().as_ref(), generics, homes));
    }
    (labels, fields)
}

/// Every leaf of a `+` chain, however the parser nested them.
///
/// **`A + B + C` parses as `(A + B) + C`**, so a reader that takes a union's
/// direct operands sees two: a nested union, and `C`. A nested union is not a
/// shape `type_of_syntax` answers for, so it became `Unknown` — `A` and `B`
/// collapsed into one entry labelled after nothing, and the row carried `C`
/// and a ghost.
///
/// It failed loudly rather than silently: the call site was told the function
/// does not raise `A`, which is true of the row that was built and a message
/// about the wrong thing entirely. Nothing in the corpus had a three-error row
/// until `Router::listen_tls` needed `'e + HttpError + TlsError`.
pub(crate) fn union_operands(ty: &ast::Type) -> Vec<ast::Type> {
    let ast::Type::Union(u) = ty else { return vec![ty.clone()] };
    u.operands().flat_map(|operand| union_operands(&operand)).collect()
}

/// Reads a `with` or `raises` clause into a row./// Reads a `with` or `raises` clause into a row.
///
/// Absent means the closed empty row: a function with no clause requires
/// nothing and raises nothing, which is what makes those the safe defaults and
/// what an entry point has to reduce to.
pub(crate) fn row_of_syntax(clause: Option<&ast::Type>, generics: &[String], homes: &TypeHomes) -> Type {
    let Some(clause) = clause else { return Type::empty_row() };
    match clause {
        // `with { ledger: Ledger | 'e }`
        ast::Type::Record(r) => {
            // With a tail, the labels after the `|` are nested inside it
            // rather than beside it, so both places have to be read.
            let after_tail: Vec<ast::Field> =
                r.row_tail().map(|t| t.fields().collect()).unwrap_or_default();
            let fields: Vec<(String, Type)> = r
                .fields()
                .chain(after_tail)
                .filter_map(|f| {
                    let label = f.name()?.ident()?;
                    Some((label, type_of_syntax(f.ty().as_ref(), generics, homes)))
                })
                .collect();
            let tail = r
                .row_tail()
                .and_then(|t| t.types().next())
                .map(|t| type_of_syntax(Some(&t), generics, homes));
            Type::row(fields, tail)
        }
        // `raises DbError + ModelError`. An error row labels each entry with
        // the error's own type name: two errors of one type cannot be told
        // apart, and by name is how they are handled.
        //
        // A row variable among the operands is the row's *tail*, not an entry
        // in it: `raises 'e + HttpError` means "whatever the caller's handler
        // can raise, and also this". Reading it as a label gave the row an
        // entry called `'e`, which no `raises` clause could ever satisfy.
        ast::Type::Union(u) => {
            let mut fields = Vec::new();
            let mut tail = None;
            // Flattened, because `A + B + C` parses as `(A + B) + C` and the
            // direct operands of the outer union are a *union* and `C`.
            for operand in union_operands(&ast::Type::Union(u.clone())) {
                match type_of_syntax(Some(&operand), generics, homes) {
                    Type::Param(name) if name.starts_with('\'') => {
                        tail = Some(Type::Param(name));
                    }
                    resolved => fields.push((label_of(&resolved), resolved)),
                }
            }
            Type::row(fields, tail)
        }
        // `raises DbError`, or a bare `'r`.
        other => {
            let ty = type_of_syntax(Some(other), generics, homes);
            match &ty {
                // A bare row variable is the whole row.
                Type::Param(name) if name.starts_with('\'') => Type::row(Vec::new(), Some(ty)),
                _ => match error_label(other, generics, homes) {
                    Some(entry) => Type::row(vec![entry], None),
                    None => Type::empty_row(),
                },
            }
        }
    }
}

/// One entry of an error row, labelled by the error type's own name.
pub(crate) fn error_label(
    ty: &ast::Type,
    generics: &[String],
    homes: &TypeHomes,
) -> Option<(String, Type)> {
    let resolved = type_of_syntax(Some(ty), generics, homes);
    let label = match &resolved {
        Type::Adt { name, .. } => name.clone(),
        Type::Param(name) => name.clone(),
        other => other.to_string(),
    };
    Some((label, resolved))
}

/// Maps written syntax to a type.
///
/// `generics` are the names in scope as rigid parameters — a bare `A` inside
/// `fn f<A>(..)` is [`Type::Param`], not an undeclared ADT. Anything else
/// unrecognized becomes [`Type::Unknown`], which suppresses follow-on errors.
pub(crate) fn type_of_syntax(ty: Option<&ast::Type>, generics: &[String], homes: &TypeHomes) -> Type {
    let Some(ty) = ty else { return Type::Unknown };
    match ty {
        ast::Type::Unit(_) => Type::Unit,
        // `(Int, Bool) -> Int`. The parameter list parses as whatever shape the
        // parentheses made of it: a tuple for several, a paren for one, a unit
        // for none. All three mean the same thing here.
        ast::Type::Fn(f) => {
            let params = match f.param_type() {
                Some(ast::Type::Tuple(t)) => {
                    t.elements().map(|e| type_of_syntax(Some(&e), generics, homes)).collect()
                }
                Some(ast::Type::Unit(_)) | None => Vec::new(),
                Some(ast::Type::Paren(p)) => {
                    vec![type_of_syntax(p.inner().as_ref(), generics, homes)]
                }
                Some(other) => vec![type_of_syntax(Some(&other), generics, homes)],
            };
            let ret = type_of_syntax(f.return_type().as_ref(), generics, homes);
            Type::Fn {
                params,
                ret: Box::new(ret),
                requires: Box::new(row_of_syntax(
                    f.with_clause().and_then(|c| c.row()).as_ref(),
                    generics,
                    homes,
                )),
                raises: Box::new(row_of_syntax(
                    f.raises_clause().and_then(|c| c.row()).as_ref(),
                    generics,
                    homes,
                )),
            }
        }
        // A bare integer in type position is a const-generic argument.
        ast::Type::Literal(l) => l.value().map(Type::Const).unwrap_or(Type::Unknown),
        ast::Type::Tuple(t) => {
            let items: Vec<Type> =
                t.elements().map(|e| type_of_syntax(Some(&e), generics, homes)).collect();
            if items.is_empty() { Type::Unit } else { Type::Tuple(items) }
        }
        ast::Type::Path(p) => {
            // A bare `'r`. It has no `Path` of its own — it is one token — so
            // without this it read as the empty name and became `Unknown`,
            // which then absorbed whatever it was unified with and made every
            // row-polymorphic signature pass by saying nothing.
            if let Some(row_var) = p.row_var() {
                return Type::Param(row_var.text().to_string());
            }
            let name = p.path().map(|p| p.text_path()).unwrap_or_default();
            let args: Vec<Type> = p
                .type_args()
                .map(|a| a.args().map(|t| type_of_syntax(Some(&t), generics, homes)).collect())
                .unwrap_or_default();
            named_type(&name, args, generics, homes)
        }
        // `{}`, `{ db: Db }`, `{ 'ef | db: Db }` -- a **row**, in a position
        // that is not a `with` or `raises` clause. The only place one can be
        // written is as a type argument, because a type may take a row
        // parameter: `Fiber<A, 'er>` carries the row its body raises, and
        // `Fibers::adopt` names the empty one to say a child it adopts must
        // have settled its failures already.
        //
        // **This fell through to `Unknown`, and `Unknown` absorbs whatever it
        // meets.** So `Fiber<(), {}>` meant `Fiber<(), ?>` and accepted a fiber
        // that could still fail; `adopt`'s promise was a comment. Errata 59,
        // and the same shape as errata 30 three lines above -- a type the
        // converter did not recognise became the one that agrees with
        // everything, so the signature passed by saying nothing.
        ast::Type::Record(_) => row_of_syntax(Some(ty), generics, homes),
        // **Parentheses around a type mean grouping and nothing else.** This
        // fell to the arm below, and `Unknown` agrees with everything -- so
        // `fn f(xs: List<(Int)>)` accepted a `List<String>` and said nothing.
        // A reader adding parentheses to a type is clarifying it, and what
        // they got was the checking of that position switched off.
        ast::Type::Paren(p) => type_of_syntax(p.inner().as_ref(), generics, homes),
        // **`Unknown` is a permissive default and this is the third time.**
        // Errata 60 named the shape -- "a permissive default is not a small
        // bug, and it hides in the arm nobody wrote" -- about `_ =>
        // Type::Unknown` in two other matches. What is left here is `Union`,
        // `Variant` and `Forall` in a position that is not a row, and each is
        // a construct with no meaning as a type argument rather than one whose
        // meaning is "anything".
        //
        // Still `Unknown`, because this function has no channel to report on
        // and inventing a nominal type here would produce a second, worse
        // message. `crate::unresolved` walks the syntax and reports them at
        // the range they were written, which is the only place that can.
        _ => Type::Unknown,
    }
}

/// A type written as a name and some arguments, resolved against the type
/// parameters in scope.
///
/// The single place a type *name* means something. Reached from the syntax
/// above and from a [`TypeRef`] below, because two interpreters of one name is
/// how the two come to disagree — and the disagreement is silent.
pub(crate) fn named_type(
    name: &str,
    args: Vec<Type>,
    generics: &[String],
    homes: &TypeHomes,
) -> Type {
    // `T::Item` where `T` is a parameter in scope is a projection, not a type
    // whose name happens to contain `::`.
    if let Some((owner, assoc)) = name.split_once("::") {
        if generics.iter().any(|g| g == owner) {
            return Type::Assoc {
                owner: Box::new(Type::Param(owner.to_string())),
                name: assoc.to_string(),
            };
        }
    }

    match name {
        "Int" | "I64" => Type::Int,
        "Float" => Type::Float,
        "Bool" => Type::Bool,
        "String" => Type::Str,
        "Ptr" => Type::Ptr,
        // **The bottom type, and it was not reaching it.** `std::core` declares
        // `pub type Never;`, so a mention of the name resolved through
        // `homes.of` to an ordinary opaque `Type::Adt` -- while the solver's
        // own `Type::Never`, the type a `raise` already has, sat next to it
        // unrelated. Everything a bottom type needs was built and working:
        // `raise` in one arm of a `match` discharges against a caller's `A`
        // today. The name simply never arrived.
        //
        // So this is a binding rather than a feature. What it buys is that a
        // function which stops the program can be written in the branch of an
        // `if` whose other branch produces a generic -- `Vector::at` is the
        // first, and every later `std` function that traps on a type it does
        // not choose is the rest.
        //
        // The declaration in `core.kh` stays: it is what `Never` is documented
        // on and what an `import` can name, the same way `pub fn print` is
        // declared without a body. Nothing resolves *to* it any more.
        "Never" => Type::Never,
        "" => Type::Unknown,
        other if IntKind::parse(other).is_some() => {
            Type::Fixed(IntKind::parse(other).expect("just checked"))
        }
        other if generics.iter().any(|g| g == other) => {
            if args.is_empty() {
                Type::Param(other.to_string())
            } else {
                Type::Applied { head: Box::new(Type::Param(other.to_string())), args }
            }
        }
        other => match homes.of(other) {
            // The declared name, not the local spelling. An alias renames a
            // mention and not a type: `import other::{Point as Other}` used to
            // key the import under `Other`, and a rename invented a type.
            Some((home, declared)) => Type::Adt { name: declared, home: Some(home), args },
            // Nothing declares it. Already an error where the name was
            // resolved, and `home: None` keeps it from quietly unifying with a
            // real type that happens to share the spelling.
            None => Type::Adt { name: other.to_string(), home: None, args },
        },
    }
}

/// The same, for a type a *body* wrote down: a `let` annotation.
///
/// [`TypeRef::Opaque`] becomes `Unknown`, which unifies with everything and so
/// checks nothing — the shapes it stands for are the ones the echo does not
/// carry yet, and saying nothing about them is what every annotation used to
/// get.
pub fn type_of_ref(
    ty: &khora_hir::body::TypeRef,
    generics: &[String],
    homes: &TypeHomes,
) -> Type {
    use khora_hir::body::TypeRef;
    match ty {
        TypeRef::Unit => Type::Unit,
        TypeRef::Const(value) => Type::Const(*value),
        TypeRef::Opaque => Type::Unknown,
        TypeRef::Tuple(items) => {
            let items: Vec<Type> = items.iter().map(|t| type_of_ref(t, generics, homes)).collect();
            if items.is_empty() { Type::Unit } else { Type::Tuple(items) }
        }
        TypeRef::Named { name, args } => {
            let args = args.iter().map(|t| type_of_ref(t, generics, homes)).collect();
            named_type(name, args, generics, homes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `khora-hir` decides whether importing a name is allowed; this file
    /// decides whether the name means a builtin. They have to be the same set.
    ///
    /// **They cannot share a function.** `khora-types` depends on `khora-hir`,
    /// so the list lives over there and this is the only side that can compare
    /// them. Without this, adding a width to `IntKind` would leave
    /// `import std::core::{I128}` rejected for a type the language has, and
    /// nothing would notice until somebody wrote it.
    #[test]
    fn builtin_names_agree() {
        let homes = TypeHomes::default();
        let candidates = [
            "Int", "I64", "Float", "Bool", "String", "Ptr", "Never", "I8", "I16", "I32", "U8",
            "U16", "U32", "U64", "I128", "U128", "I7", "Integer", "Str", "List", "Option", "Foo",
            "", "I", "U",
        ];
        for name in candidates {
            // A name this file knows is anything it does not hand to `homes`,
            // which is what produces an `Adt`.
            let known = !matches!(
                named_type(name, Vec::new(), &[], &homes),
                Type::Adt { .. } | Type::Unknown
            );
            assert_eq!(
                known,
                khora_hir::is_builtin_type(name),
                "`{name}`: this file says builtin={known}, khora-hir disagrees"
            );
        }
    }
}
