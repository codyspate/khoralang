//! Declaration grammar: modules, imports, types, functions and top-level lets.

use super::exprs::{block, expr};
use super::patterns::pattern;
use super::types::{
    bounds, effect_clauses, field, name, name_ref, path, type_, type_params, variant_type,
};
use super::Parser;
use crate::kind::SyntaxKind::*;

pub(super) fn source_file_contents(p: &mut Parser<'_>) {
    // `module` must come first, but accepting it out of order here and
    // diagnosing it later gives better editor behavior than bailing out.
    while !p.at(EOF) {
        if !p.tick() {
            break;
        }
        declaration(p);
    }
}

/// Declaration position is where `context`, `test` and `bench` are keywords.
///
/// Nothing is given up by recognizing them here: no declaration may begin with
/// a bare identifier, so an `IDENT` in this position is either one of these
/// three words or a syntax error either way.
fn declaration(p: &mut Parser<'_>) {
    match p.current() {
        MODULE_KW => module_decl(p),
        IMPORT_KW => import_decl(p),
        TYPE_KW => type_decl(p),
        TRAIT_KW => trait_decl(p),
        IMPL_KW => impl_decl(p),
        EFFECT_KW => effect_decl(p),
        FN_KW => fn_decl(p),
        LET_KW => let_decl(p),
        IDENT if p.at_contextual(CONTEXT_KW) => context_decl(p),
        IDENT if p.at_contextual(TEST_KW) || p.at_contextual(BENCH_KW) => test_decl(p),
        IDENT if p.at_contextual(EXTERN_KW) => fn_decl(p),
        EXPORT_KW => match p.nth(1) {
            TYPE_KW => type_decl(p),
            TRAIT_KW => trait_decl(p),
            EFFECT_KW => effect_decl(p),
            FN_KW => fn_decl(p),
            LET_KW => let_decl(p),
            IDENT if p.nth_at_contextual(1, CONTEXT_KW) => context_decl(p),
            IDENT if p.nth_at_contextual(1, EXTERN_KW) => fn_decl(p),
            _ => p.err_recover(
                "expected `type`, `trait`, `effect`, `context`, `fn`, `extern` or `let` \
                 after `export`",
                Parser::at_decl_start,
            ),
        },
        SEMICOLON => p.err_and_bump("stray `;`"),
        _ => p.err_recover("expected a declaration", Parser::at_decl_start),
    }
}

fn module_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(MODULE_KW);
    path(p);
    p.expect(SEMICOLON);
    m.complete(p, MODULE_DECL);
}

/// `import a::b::{X, Y as Z};` or `import a::b::*;`
fn import_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(IMPORT_KW);
    path(p);
    if p.at(COLON_COLON) && p.nth_at(1, L_BRACE) {
        p.bump(COLON_COLON);
        import_list(p);
    } else if p.at(COLON_COLON) && p.nth_at(1, STAR) {
        let glob = p.start();
        p.bump(COLON_COLON);
        p.bump(STAR);
        glob.complete(p, IMPORT_GLOB);
    } else {
        p.error("expected `::{...}` or `::*` after the module path");
    }
    p.expect(SEMICOLON);
    m.complete(p, IMPORT_DECL);
}

fn import_list(p: &mut Parser<'_>) {
    let m = p.start();
    let brace = p.open(L_BRACE);
    while !p.at(R_BRACE) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        let item = p.start();
        name(p);
        if p.eat(AS_KW) {
            name(p);
        }
        item.complete(p, IMPORT_ITEM);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.close(R_BRACE, brace);
    m.complete(p, IMPORT_LIST);
}

/// `export? type Name<Params>? ( "=" TypeDef )? DeriveClause? ";"`
///
/// The right-hand side is optional: the standard library declares opaque types
/// such as `export type Fibers;` whose representation is compiler internal.
///
/// The clause lives *inside* the `TYPE_DECL`, at the end. A reader of the tree
/// that asks a type what it implements should not have to look at what came
/// beside it in the file, and a reader of the source meets the name first.
fn type_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(EXPORT_KW);
    p.bump(TYPE_KW);
    name(p);
    if p.at(LT) {
        type_params(p);
    }
    if p.eat(EQ) {
        if p.at(PIPE) {
            variant_type(p);
        } else {
            type_(p);
        }
    }
    if p.at(IMPL_KW) {
        derive_clause(p);
    }
    p.expect(SEMICOLON);
    m.complete(p, TYPE_DECL);
}

/// `impl Name ( "," Name )*`, trailing.
///
/// ```khora
/// export type Point = { x: Int, y: Int } impl Eq, Ord, Show;
/// ```
///
/// **`impl` rather than a word of its own.** It already means "here are
/// implementations for this type", which is exactly what is being asked for,
/// so this spelling costs the language no new keyword — not even a contextual
/// one — and reads as the short form of what it expands into: `impl Eq for
/// Point { .. }` with the body and the `for` left off because both are
/// obvious.
///
/// It was `derive(Eq, Ord)` on the line above, which is Rust's spelling and
/// which every Rust reader recognizes on sight. It went because it was
/// *attribute-shaped* in a language that has no attributes: the one place in
/// the grammar hinting at a syntax family that does not exist, and the thing
/// the next feature wanting to annotate a declaration would have had to either
/// join or duplicate.
///
/// Trailing, because Khora already puts a declaration's clauses after it —
/// `fn listen(..) -> () with { .. } raises E` — and because the name is what a
/// reader is looking for.
///
/// Which traits may appear is not a grammar question. `impl Frobnicate` parses;
/// it is refused later, by the pass that knows what can be derived and can say
/// so in a sentence.
fn derive_clause(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(IMPL_KW);
    while !p.at(SEMICOLON) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        if !p.at(IDENT) {
            p.error("expected the name of a trait, as `impl Eq, Ord`");
            break;
        }
        name_ref(p);
        if !p.eat(COMMA) {
            break;
        }
    }
    m.complete(p, DERIVE_CLAUSE);
}

/// `pub? trait Name<Params>? (":" Bounds)? "{" TraitItem* "}"`
///
/// Rust's spelling, per `docs/design/typeclasses.md`: the concept is Rust's
/// trait, so it gets Rust's word rather than Haskell's `class`, which means
/// something else entirely in two of the three languages the audience arrives
/// from.
fn trait_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(EXPORT_KW);
    p.bump(TRAIT_KW);
    name(p);
    if p.at(LT) {
        type_params(p);
    }
    // Supertraits: `trait Ord: Eq`.
    if p.eat(COLON) {
        bounds(p);
    }
    trait_body(p);
    m.complete(p, TRAIT_DECL);
}

/// `impl Trait for Type "{" .. "}"`, or `impl Type "{" .. "}"`.
///
/// Without `for` the block declares the type's *own* methods, needing no trait.
/// That is the ordinary first thing a developer does in Go, TypeScript and Rust
/// alike, and requiring an abstraction for it was a behavioral surprise on a
/// daily action — see `docs/design/keywords.md`.
fn impl_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(IMPL_KW);
    if p.at(LT) {
        type_params(p);
    }
    type_(p);
    if p.at(FOR_KW) {
        p.bump(FOR_KW);
        type_(p);
    } else if !p.at(L_BRACE) && !p.at(EOF) {
        // Two types with nothing between them: `impl Eq Int { .. }`. The
        // inherent form makes this parse far enough to produce a confusing
        // "expected `{`", so name the actual mistake instead.
        p.error("expected `for` between the trait and the type, as `impl Eq for Int`");
        type_(p);
    }
    trait_body(p);
    m.complete(p, IMPL_DECL);
}

/// The braced item list shared by `trait` and `impl`.
///
/// Both hold the same two things — associated types and functions — and a
/// function with a body is exactly how a trait states a default and how an impl
/// supplies one, so there is nothing to distinguish here. What is *allowed*
/// (an impl may not leave a function without a body) is a rule the checker
/// states with a real diagnostic, not one the grammar enforces by shape.
fn trait_body(p: &mut Parser<'_>) {
    let brace = p.open(L_BRACE);
    if brace.is_none() {
        return;
    }
    while !p.at(R_BRACE) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        match p.current() {
            TYPE_KW => assoc_type_decl(p),
            FN_KW | EXPORT_KW => fn_decl(p),
            _ => {
                p.err_recover("expected `fn` or `type`", |p| {
                    p.at_any(&[R_BRACE, FN_KW, TYPE_KW]) || p.at_decl_start()
                });
                if !p.at_any(&[R_BRACE, FN_KW, TYPE_KW]) {
                    break;
                }
            }
        }
    }
    p.close(R_BRACE, brace);
}

/// `type Item;` in a trait, `type Item = Int;` in an impl.
fn assoc_type_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(TYPE_KW);
    name(p);
    if p.eat(COLON) {
        bounds(p);
    }
    if p.eat(EQ) {
        type_(p);
    }
    p.expect(SEMICOLON);
    m.complete(p, ASSOC_TYPE_DECL);
}

/// `pub? effect Name<Params>? "{" ( Field "," )* "}"`
///
/// An effect is a named set of operations, shaped exactly like the record of
/// functions a capability already was under the monadic design — which is why
/// the dependency-injection model survived decision A8 unchanged.
fn effect_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(EXPORT_KW);
    p.bump(EFFECT_KW);
    name(p);
    if p.at(LT) {
        type_params(p);
    }
    let brace = p.open(L_BRACE);
    if brace.is_some() {
        while !p.at(R_BRACE) && !p.at(EOF) {
            if !p.tick() {
                break;
            }
            field(p);
            if !p.eat(COMMA) {
                break;
            }
        }
        p.close(R_BRACE, brace);
    }
    m.complete(p, EFFECT_DECL);
}

/// `pub? context Name "{" ( Ident ":" Expr "," )* "}"`
///
/// A named bundle of handlers. Bindings are sequential: each may use the ones
/// above it, which is what keeps service composition flat instead of nesting
/// one `with` per layer.
fn context_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(EXPORT_KW);
    p.bump_contextual(CONTEXT_KW);
    name(p);
    let brace = p.open(L_BRACE);
    if brace.is_some() {
        while !p.at(R_BRACE) && !p.at(EOF) {
            if !p.tick() {
                break;
            }
            let f = p.start();
            name(p);
            p.expect(COLON);
            if expr(p).is_none() {
                p.error("expected a handler expression");
            }
            f.complete(p, RECORD_EXPR_FIELD);
            if !p.eat(COMMA) {
                break;
            }
        }
        p.close(R_BRACE, brace);
    }
    m.complete(p, CONTEXT_DECL);
}

/// `pub? fn name<Params>?(params) ("->" Type)? EffectClause* ( Block | ";" )`
///
/// No `=` before the body, and no semicolon after it. The rule is simply:
/// `{` introduces a definition, `;` declares a signature only — which is how
/// `std` describes intrinsics and FFI entry points.
fn fn_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(EXPORT_KW);
    // `extern fn` says the body is a C symbol found at link time. Without it a
    // function with no body is a declaration nobody has kept yet, which the
    // checker allows — a signature ahead of its implementation is a useful
    // thing to write — and the code generator refuses to call.
    if p.at_contextual(EXTERN_KW) {
        p.bump_contextual(EXTERN_KW);
    }
    p.bump(FN_KW);
    name(p);
    if p.at(LT) {
        type_params(p);
    }
    if p.at(L_PAREN) {
        param_list(p);
    } else {
        p.error("expected a parameter list");
    }
    if p.eat(THIN_ARROW) {
        type_(p);
    }
    effect_clauses(p);
    if p.at(L_BRACE) {
        block(p);
    } else if p.at(EQ) {
        // The published grammar used `= body;`. Point at it specifically rather
        // than emitting a bare "expected `;`" that hides the real problem.
        p.error("a function body is a block: write `fn f() { .. }`, not `fn f() = { .. };`");
        p.bump(EQ);
        if p.at(L_BRACE) {
            block(p);
        }
        p.eat(SEMICOLON);
    } else {
        p.expect(SEMICOLON);
    }
    m.complete(p, FN_DECL);
}

/// `test "name" { .. }` and `bench "name" { .. }`
///
/// Tests are declarations rather than a convention over function names, per
/// section 6.4, so the runner does not have to guess what is a test.
fn test_decl(p: &mut Parser<'_>) {
    let m = p.start();
    let (keyword, kind) =
        if p.at_contextual(TEST_KW) { (TEST_KW, TEST_DECL) } else { (BENCH_KW, BENCH_DECL) };
    p.bump_contextual(keyword);
    if !p.eat(STRING_LIT) {
        p.error("expected a name string");
    }
    if p.at(L_BRACE) {
        block(p);
    } else {
        p.error("expected a block");
    }
    m.complete(p, kind);
}

pub(super) fn param_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.expect(L_PAREN);
    while !p.at(R_PAREN) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        param(p);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(R_PAREN);
    m.complete(p, PARAM_LIST);
}

fn param(p: &mut Parser<'_>) {
    let m = p.start();
    match p.current() {
        IDENT => name(p),
        UNDERSCORE => p.bump(UNDERSCORE),
        _ => {
            m.abandon(p);
            p.err_recover("expected a parameter name", |p| p.at_any(&[COMMA, R_PAREN]));
            return;
        }
    }
    if p.eat(COLON) {
        type_(p);
    }
    m.complete(p, PARAM);
}

/// `let mut? Pattern (":" Type)? "=" Expr ";"`
pub(super) fn let_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(EXPORT_KW);
    p.bump(LET_KW);
    p.eat(MUT_KW);
    pattern(p);
    if p.eat(COLON) {
        type_(p);
    }
    if p.expect(EQ) && expr(p).is_none() {
        p.error("expected an initializer expression");
    }
    p.expect(SEMICOLON);
    m.complete(p, LET_DECL);
}
