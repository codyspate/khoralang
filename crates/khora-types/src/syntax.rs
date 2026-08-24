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
