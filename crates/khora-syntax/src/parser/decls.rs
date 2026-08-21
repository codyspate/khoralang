//! Declaration grammar: modules, imports, types, functions and top-level lets.

use super::exprs::{block, expr};
use super::patterns::pattern;
use super::types::{effect_clauses, field, name, path, type_, type_params, variant_type};
use super::Parser;
use crate::kind::SyntaxKind::*;

/// Tokens we resynchronise to after a broken declaration.
const DECL_RECOVERY: &[crate::kind::SyntaxKind] =
    &[MODULE_KW, IMPORT_KW, TYPE_KW, EFFECT_KW, CONTEXT_KW, FN_KW, LET_KW, PUB_KW, TEST_KW, BENCH_KW];

pub(super) fn source_file_contents(p: &mut Parser) {
    // `module` must come first, but accepting it out of order here and
    // diagnosing it later gives better editor behaviour than bailing out.
    while !p.at(EOF) {
        if !p.tick() {
            break;
        }
        declaration(p);
    }
}

fn declaration(p: &mut Parser) {
    match p.current() {
        MODULE_KW => module_decl(p),
        IMPORT_KW => import_decl(p),
        TYPE_KW => type_decl(p),
        EFFECT_KW => effect_decl(p),
        CONTEXT_KW => context_decl(p),
        FN_KW => fn_decl(p),
        LET_KW => let_decl(p),
        TEST_KW | BENCH_KW => test_decl(p),
        PUB_KW => match p.nth(1) {
            TYPE_KW => type_decl(p),
            EFFECT_KW => effect_decl(p),
            CONTEXT_KW => context_decl(p),
            FN_KW => fn_decl(p),
            LET_KW => let_decl(p),
            _ => p.err_recover(
                "expected `type`, `effect`, `context`, `fn` or `let` after `pub`",
                DECL_RECOVERY,
            ),
        },
        SEMICOLON => p.err_and_bump("stray `;`"),
        _ => p.err_recover("expected a declaration", DECL_RECOVERY),
    }
}

fn module_decl(p: &mut Parser) {
    let m = p.start();
    p.bump(MODULE_KW);
    path(p);
    p.expect(SEMICOLON);
    m.complete(p, MODULE_DECL);
}

/// `import a.b.{X, Y as Z};` or `import a.b.*;`
fn import_decl(p: &mut Parser) {
    let m = p.start();
    p.bump(IMPORT_KW);
    path(p);
    if p.at(DOT) && p.nth_at(1, L_BRACE) {
        p.bump(DOT);
        import_list(p);
    } else if p.at(DOT) && p.nth_at(1, STAR) {
        let glob = p.start();
        p.bump(DOT);
        p.bump(STAR);
        glob.complete(p, IMPORT_GLOB);
    } else {
        p.error("expected `.{...}` or `.*` after the module path");
    }
    p.expect(SEMICOLON);
    m.complete(p, IMPORT_DECL);
}

fn import_list(p: &mut Parser) {
    let m = p.start();
    p.bump(L_BRACE);
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
    p.expect(R_BRACE);
    m.complete(p, IMPORT_LIST);
}

/// `pub? type Name<Params>? ( "=" TypeDef )? ";"`
///
/// The right-hand side is optional: the standard library declares opaque types
/// such as `pub type Effect<+A, -R, +E>;` whose representation is compiler
/// internal.
fn type_decl(p: &mut Parser) {
    let m = p.start();
    p.eat(PUB_KW);
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
    p.expect(SEMICOLON);
    m.complete(p, TYPE_DECL);
}

/// `pub? effect Name<Params>? "{" ( Field "," )* "}"`
///
/// An effect is a named set of operations, shaped exactly like the record of
/// functions a capability already was under the monadic design — which is why
/// the dependency-injection model survived decision A8 unchanged.
fn effect_decl(p: &mut Parser) {
    let m = p.start();
    p.eat(PUB_KW);
    p.bump(EFFECT_KW);
    name(p);
    if p.at(LT) {
        type_params(p);
    }
    if p.expect(L_BRACE) {
        while !p.at(R_BRACE) && !p.at(EOF) {
            if !p.tick() {
                break;
            }
            field(p);
            if !p.eat(COMMA) {
                break;
            }
        }
        p.expect(R_BRACE);
    }
    m.complete(p, EFFECT_DECL);
}

/// `pub? context Name "{" ( Ident ":" Expr "," )* "}"`
///
/// A named bundle of handlers. Bindings are sequential: each may use the ones
/// above it, which is what keeps service composition flat instead of nesting
/// one `with` per layer.
fn context_decl(p: &mut Parser) {
    let m = p.start();
    p.eat(PUB_KW);
    p.bump(CONTEXT_KW);
    name(p);
    if p.expect(L_BRACE) {
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
        p.expect(R_BRACE);
    }
    m.complete(p, CONTEXT_DECL);
}

/// `pub? fn name<Params>?(params) ("->" Type)? EffectClause* ( Block | ";" )`
///
/// No `=` before the body, and no semicolon after it. The rule is simply:
/// `{` introduces a definition, `;` declares a signature only — which is how
/// `std` describes intrinsics and FFI entry points.
fn fn_decl(p: &mut Parser) {
    let m = p.start();
    p.eat(PUB_KW);
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
fn test_decl(p: &mut Parser) {
    let m = p.start();
    let kind = if p.at(TEST_KW) { TEST_DECL } else { BENCH_DECL };
    p.bump_any();
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

pub(super) fn param_list(p: &mut Parser) {
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

fn param(p: &mut Parser) {
    let m = p.start();
    match p.current() {
        IDENT => name(p),
        UNDERSCORE => p.bump(UNDERSCORE),
        _ => {
            m.abandon(p);
            p.err_recover("expected a parameter name", &[COMMA, R_PAREN]);
            return;
        }
    }
    if p.eat(COLON) {
        type_(p);
    }
    m.complete(p, PARAM);
}

/// `let mut? Pattern (":" Type)? "=" Expr ";"`
pub(super) fn let_decl(p: &mut Parser) {
    let m = p.start();
    p.eat(PUB_KW);
    p.bump(LET_KW);
    p.eat(MUT_KW);
    pattern(p);
    if p.eat(COLON) {
        type_(p);
    }
    if p.expect(EQ) && expr(p).is_none() {
        p.error("expected an initialiser expression");
    }
    p.expect(SEMICOLON);
    m.complete(p, LET_DECL);
}
