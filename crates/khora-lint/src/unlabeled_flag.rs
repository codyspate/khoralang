//! `unlabeled-flag`: a `true` or `false` passed bare to a parameter declared
//! `Bool` that is not the first.
//!
//! `reply(connection, response, false)` compiles, and a reader cannot tell
//! what `false` switches off without opening `reply`. A label at the call
//! says it, and the checker holds the label to the declaration.
//!
//! # Why the rule is this narrow
//!
//! "A `Bool` literal at a call" was measured over the repository first: 25
//! sites, and 19 of them were right as written. Two conditions separate the
//! six real flags from the rest, and each costs a known miss:
//!
//! - **Declared `Bool`, not a type variable instantiated at `Bool`.**
//!   `list.fold(true, step)` seeds an accumulator; the parameter is `B`, and
//!   `true` is a value of it, not a switch. The miss: a generic function
//!   whose `B` really is a flag. None exists in `std`.
//! - **Not the first parameter.** `assert_that(false, "..")` is about its
//!   first argument, and `condition:` would add nothing. The miss: a function
//!   whose *only* flag is first, such as `trace::flag(value)` -- which the
//!   function's own name already describes.
//!
//! What it does not see: a call through a value (a function type has no
//! names, so there is no label to suggest), a constructor's payload, and a
//! literal that arrives through `|>` (it is not in the argument list, so a
//! label cannot be written against it).

use khora_db::{Db, SourceFile};
use khora_hir::body::{Body, Expr, ExprId, Literal};
use khora_types::{BodyTypes, Type, TypeMap};

use crate::Finding;

/// A `true` or `false` passed unlabeled to a declared `Bool` parameter that
/// is not the first.
///
/// **Off by default**, and a member of the `idiomatic` group: the call it
/// reports is correct, and says less than it could.
pub const UNLABELED_FLAG: &str = "unlabeled-flag";

/// Every unlabeled flag in `body`.
pub(crate) fn unlabeled_flags(
    db: &dyn Db,
    file: SourceFile,
    body: &Body,
    types: &BodyTypes,
    out: &mut Vec<Finding>,
) {
    let map = khora_types::type_map(db, file);
    for (_, expr) in body.exprs() {
        let Expr::Call { callee, args } = expr else { continue };
        report_call(map, body, types, *callee, args, out);
    }
}

fn report_call(
    map: &TypeMap,
    body: &Body,
    types: &BodyTypes,
    callee: ExprId,
    args: &[ExprId],
    out: &mut Vec<Finding>,
) {
    // Only a callee the checker resolved to a declaration has names. A local,
    // a parameter or a record field holding a function records nothing here.
    let Some((key, _)) = types.instantiation(callee) else { return };
    let Some(signature) = map.signatures.get(key.as_str()) else { return };
    // `x.f(a)` is lowered with the receiver outside the argument list, so the
    // written arguments start at parameter 2. `Type::f(x, a)` writes it.
    let skip = usize::from(matches!(body.expr(callee), Expr::Field { .. }));
    let labeled: Vec<usize> = body
        .labels
        .get(&callee)
        .map(|labels| labels.iter().map(|(position, _, _)| *position).collect())
        .unwrap_or_default();
    let written_from = body.range(callee).end();

    for (position, arg) in args.iter().enumerate() {
        let Expr::Literal(Literal::Bool(value)) = body.expr(*arg) else { continue };
        let index = position + skip;
        if index == 0 || labeled.contains(&position) {
            continue;
        }
        // A piped value sits in the list at its slot but was written before
        // the callee, so no label can be written against it.
        if body.range(*arg).start() < written_from {
            continue;
        }
        if signature.params.get(index) != Some(&Type::Bool) {
            continue;
        }
        let Some(Some(name)) = signature.names.get(index) else { continue };
        out.push(Finding {
            lint: UNLABELED_FLAG,
            message: format!(
                "`{value}` is passed to `{name}`, parameter {} of `{}`, and nothing at the \
                 call says what it means. Write `{name}: {value}`",
                index + 1,
                khora_types::traits::readable_key(key)
            ),
            range: body.range(*arg),
            // No edit in this change: the message names the label to write.
            fix: None,
        });
    }
}
