//! Pattern grammar for `match` arms, `let` bindings and function parameters.

use super::types::{name, path};
use super::Parser;
use crate::kind::SyntaxKind::*;

pub(super) fn pattern(p: &mut Parser<'_>) {
    match p.current() {
        UNDERSCORE => {
            let m = p.start();
            p.bump(UNDERSCORE);
            m.complete(p, WILDCARD_PAT);
        }
        INT_LIT | FLOAT_LIT | STRING_LIT | TRUE_KW | FALSE_KW => {
            let m = p.start();
            p.bump_any();
            m.complete(p, LITERAL_PAT);
        }
        L_PAREN => tuple_pattern(p),
        IDENT => path_like_pattern(p),
        _ => p.err_recover("expected a pattern", |p| {
            p.at_any(&[FAT_ARROW, EQ, COMMA, R_PAREN, SEMICOLON])
        }),
    }
}

/// A bare identifier binds; a `::` path or a payload makes it a constructor.
/// Which one a single uppercase identifier is (binding vs nullary constructor)
/// is a name-resolution question, settled in HIR lowering rather than here.
fn path_like_pattern(p: &mut Parser<'_>) {
    if !p.nth_at(1, COLON_COLON) && !p.nth_at(1, L_PAREN) && !p.nth_at(1, L_BRACE) {
        let m = p.start();
        name(p);
        m.complete(p, IDENT_PAT);
        return;
    }

    let m = p.start();
    path(p);
    match p.current() {
        L_PAREN => {
            p.bump(L_PAREN);
            while !p.at(R_PAREN) && !p.at(EOF) {
                if !p.tick() {
                    break;
                }
                pattern(p);
                if !p.eat(COMMA) {
                    break;
                }
            }
            p.expect(R_PAREN);
            m.complete(p, TUPLE_STRUCT_PAT);
        }
        L_BRACE => {
            let brace = p.open(L_BRACE);
            while !p.at(R_BRACE) && !p.at(EOF) {
                if !p.tick() {
                    break;
                }
                record_pat_field(p);
                if !p.eat(COMMA) {
                    break;
                }
            }
            p.close(R_BRACE, brace);
            m.complete(p, RECORD_PAT);
        }
        _ => {
            m.complete(p, PATH_PAT);
        }
    }
}

/// `name` (shorthand) or `name: Pattern`.
fn record_pat_field(p: &mut Parser<'_>) {
    let m = p.start();
    name(p);
    if p.eat(COLON) {
        pattern(p);
    }
    m.complete(p, RECORD_PAT_FIELD);
}

fn tuple_pattern(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(L_PAREN);
    while !p.at(R_PAREN) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        pattern(p);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(R_PAREN);
    m.complete(p, TUPLE_PAT);
}
