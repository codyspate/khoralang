//! Declaration grammar: modules, imports, types, functions and top-level lets.

use super::exprs::{block, expr};
use super::patterns::pattern;
use super::types::{name, path, type_, type_params, variant_type};
use super::Parser;
use crate::kind::SyntaxKind::*;

/// Tokens we resynchronise to after a broken declaration.
const DECL_RECOVERY: &[crate::kind::SyntaxKind] =
    &[MODULE_KW, IMPORT_KW, TYPE_KW, FN_KW, LET_KW, PUB_KW];

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
        FN_KW => fn_decl(p),
        LET_KW => let_decl(p),
        PUB_KW => match p.nth(1) {
            TYPE_KW => type_decl(p),
            FN_KW => fn_decl(p),
            LET_KW => let_decl(p),
            _ => p.err_recover("expected `type`, `fn` or `let` after `pub`", DECL_RECOVERY),
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

/// `pub? fn name<Params>?(params) ("->" Type)? ("=" Block)? ";"`
///
/// A body-less form declares a signature only, which is how `std` describes
/// intrinsics and FFI entry points.
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
    if p.eat(EQ) {
        block(p);
    }
    p.expect(SEMICOLON);
    m.complete(p, FN_DECL);
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
