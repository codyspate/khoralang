//! Type grammar: rows, variants, records, function types and const generics.

use super::{CompletedMarker, Parser};
use crate::kind::SyntaxKind::*;

/// `Type ::= UnionType ( "->" Type ( WithClause | RaisesClause )* )?`
///
/// Function arrows are right-associative. Effect clauses bind to an arrow, and
/// only to an arrow: in `fn f() -> Report with { .. }` the return type is just
/// `Report`, and the `with` belongs to the declaration, not to the type. That
/// distinction is what lets both spellings coexist unambiguously.
pub(super) fn type_(p: &mut Parser<'_>) {
    let m = union_type(p);
    if p.at(THIN_ARROW) {
        let fn_m = m.precede(p);
        p.bump(THIN_ARROW);
        type_(p);
        effect_clauses(p);
        fn_m.complete(p, FN_TYPE);
    }
}

/// `with <row>` and `raises <type>`, in any order, zero or more times.
///
/// Shared by function types and function declarations so the two can never
/// drift apart.
pub(super) fn effect_clauses(p: &mut Parser<'_>) {
    while p.at(WITH_KW) || p.at(RAISES_KW) {
        if !p.tick() {
            break;
        }
        let m = p.start();
        if p.eat(WITH_KW) {
            type_(p);
            m.complete(p, WITH_CLAUSE);
        } else {
            p.bump(RAISES_KW);
            type_(p);
            m.complete(p, RAISES_CLAUSE);
        }
    }
}

/// `UnionType ::= PrimaryType ( "+" PrimaryType )*` — the open union used for
/// the `E` channel of `Effect<A, R, E>`.
fn union_type(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = primary_type(p);
    while p.at(PLUS) {
        let m = lhs.precede(p);
        p.bump(PLUS);
        primary_type(p);
        lhs = m.complete(p, UNION_TYPE);
    }
    lhs
}

fn primary_type(p: &mut Parser<'_>) -> CompletedMarker {
    match p.current() {
        IDENT => path_type(p),
        L_BRACE => record_type(p),
        L_PAREN => paren_or_tuple_type(p),
        ROW_VAR => {
            let m = p.start();
            p.bump(ROW_VAR);
            m.complete(p, PATH_TYPE)
        }
        INT_LIT => {
            let m = p.start();
            p.bump(INT_LIT);
            m.complete(p, LITERAL_TYPE)
        }
        FORALL_KW => forall_type(p),
        PIPE => variant_type(p),
        _ => {
            let m = p.start();
            p.error("expected a type");
            if !p.at(EOF) && !p.at_any(&[SEMICOLON, COMMA, R_PAREN, R_BRACE, GT, EQ]) {
                p.bump_any();
            }
            m.complete(p, ERROR)
        }
    }
}

/// `forall <T, const N: Int> . Type`
fn forall_type(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(FORALL_KW);
    if p.at(LT) {
        type_params(p);
    } else {
        p.error("expected `<` after `forall`");
    }
    p.expect(DOT);
    type_(p);
    m.complete(p, FORALL_TYPE)
}

/// `PathType ::= Path TypeArgs?`
fn path_type(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    path(p);
    if p.at(LT) {
        type_args(p);
    }
    m.complete(p, PATH_TYPE)
}

/// `Path ::= Ident ( "::" Ident )*`
///
/// `::` separates compile-time namespaces — module paths, types, associated
/// items and enum constructors. `.` is reserved for runtime projection: record
/// fields and method calls. Splitting them is what lets the parser build a real
/// path here rather than deferring every dotted name to name resolution, and it
/// is why a regex can colour `Foo::bar` differently from `foo.bar`.
pub(super) fn path(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    name_ref(p);
    while p.at(COLON_COLON) && p.nth_at(1, IDENT) {
        p.bump(COLON_COLON);
        name_ref(p);
    }
    m.complete(p, PATH)
}

pub(super) fn name_ref(p: &mut Parser<'_>) {
    let m = p.start();
    if !p.expect(IDENT) {
        m.abandon(p);
        return;
    }
    m.complete(p, NAME_REF);
}

pub(super) fn name(p: &mut Parser<'_>) {
    let m = p.start();
    if !p.expect(IDENT) {
        m.abandon(p);
        return;
    }
    m.complete(p, NAME);
}

/// `TypeArgs ::= "<" TypeArg ( "," TypeArg )* ">"`
///
/// `<` is unambiguous here: Khora has no comparison operators in type position,
/// so no turbofish is required.
pub(super) fn type_args(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(LT);
    while !p.at(GT) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        type_(p);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(GT);
    m.complete(p, TYPE_ARGS);
}

/// `TypeParams ::= "<" TypeParam ( "," TypeParam )* ">"`
///
/// A parameter may carry a variance marker (`+A`, `-R`), a kind/trait bound
/// (`D: Device`) or be a const generic (`const Dim: Int`).
pub(super) fn type_params(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(LT);
    while !p.at(GT) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        type_param(p);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(GT);
    m.complete(p, TYPE_PARAMS);
}

fn type_param(p: &mut Parser<'_>) {
    let m = p.start();
    if p.at(CONST_KW) {
        p.bump(CONST_KW);
        name(p);
        p.expect(COLON);
        type_(p);
        m.complete(p, TYPE_PARAM);
        return;
    }
    // Variance annotation on the parameter itself: `+A` covariant, `-R`
    // contravariant, bare `A` invariant.
    if p.at(PLUS) || p.at(MINUS) {
        p.bump_any();
    }
    if p.at(ROW_VAR) {
        p.bump(ROW_VAR);
        m.complete(p, TYPE_PARAM);
        return;
    }
    name(p);
    if p.eat(COLON) {
        type_(p);
    }
    m.complete(p, TYPE_PARAM);
}

/// `RecordType ::= "{" Field* ( "|" RowTail )? "}"`
///
/// The same production covers records (`{ role: String }`), capability rows
/// (`{ ledger: Ledger | 'r }`), row merges (`{ 'r1 | 'r2 }`) and the closed
/// empty row `{}`.
fn record_type(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(L_BRACE);
    loop {
        if p.at(R_BRACE) || p.at(EOF) || !p.tick() {
            break;
        }
        if p.at(PIPE) {
            p.bump(PIPE);
            row_tail(p);
            break;
        }
        if p.at(IDENT) && p.nth_at(1, COLON) {
            field(p);
            if !p.eat(COMMA) {
                if p.at(PIPE) {
                    p.bump(PIPE);
                    row_tail(p);
                }
                break;
            }
            continue;
        }
        // Not `name:` — this is a bare row being merged in, e.g. `{ 'r1 | 'r2 }`.
        row_tail(p);
        break;
    }
    p.expect(R_BRACE);
    m.complete(p, RECORD_TYPE)
}

/// Everything after the first `|` in a record type: row variables, whole rows
/// being merged in, and further labelled fields — `{ R1 | R2 | scope: Scope }`
/// is all three at once.
fn row_tail(p: &mut Parser<'_>) {
    let m = p.start();
    loop {
        if !p.tick() {
            break;
        }
        if p.at(IDENT) && p.nth_at(1, COLON) {
            field(p);
        } else {
            type_(p);
        }
        if !p.eat(PIPE) && !p.eat(COMMA) {
            break;
        }
    }
    m.complete(p, ROW_TAIL);
}

pub(super) fn field(p: &mut Parser<'_>) {
    let m = p.start();
    name(p);
    p.expect(COLON);
    type_(p);
    m.complete(p, FIELD);
}

fn paren_or_tuple_type(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(L_PAREN);
    if p.eat(R_PAREN) {
        return m.complete(p, UNIT_TYPE);
    }
    let mut arity = 0usize;
    let mut trailing_comma = false;
    while !p.at(R_PAREN) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        type_(p);
        arity += 1;
        if p.eat(COMMA) {
            trailing_comma = true;
        } else {
            trailing_comma = false;
            break;
        }
    }
    p.expect(R_PAREN);
    // `(T)` is a grouping; `(T,)` and `(A, B)` are tuples. A one-element shape
    // like `(Dim)` is written without a comma in the spec, so treat a
    // parenthesised type in argument position as a 1-tuple at lowering time.
    if arity == 1 && !trailing_comma {
        m.complete(p, PAREN_TYPE)
    } else {
        m.complete(p, TUPLE_TYPE)
    }
}

/// `VariantType ::= ( "|" Ident ( "(" Fields ")" )? )+`
pub(super) fn variant_type(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    while p.at(PIPE) {
        if !p.tick() {
            break;
        }
        variant_case(p);
    }
    m.complete(p, VARIANT_TYPE)
}

fn variant_case(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(PIPE);
    name(p);
    if p.at(L_PAREN) {
        // A case payload is either named (`Some(value: T)`) or positional.
        if p.nth_at(1, IDENT) && p.nth_at(2, COLON) {
            let fields = p.start();
            p.bump(L_PAREN);
            while !p.at(R_PAREN) && !p.at(EOF) {
                if !p.tick() {
                    break;
                }
                field(p);
                if !p.eat(COMMA) {
                    break;
                }
            }
            p.expect(R_PAREN);
            fields.complete(p, FIELD_LIST);
        } else {
            let fields = p.start();
            p.bump(L_PAREN);
            while !p.at(R_PAREN) && !p.at(EOF) {
                if !p.tick() {
                    break;
                }
                type_(p);
                if !p.eat(COMMA) {
                    break;
                }
            }
            p.expect(R_PAREN);
            fields.complete(p, TUPLE_FIELD_LIST);
        }
    }
    m.complete(p, VARIANT_CASE);
}
