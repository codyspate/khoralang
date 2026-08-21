//! Expression grammar.
//!
//! Binary operators are parsed with a Pratt loop; `|>` sits at the bottom of
//! the table so a pipeline binds looser than everything it carries.
//!
//! Dotted access is deliberately *not* folded into a single path node here.
//! `Effect.map`, `report.risk` and `RiskLevel.Low` are all `FIELD_EXPR` chains
//! at the CST level, exactly as the "universal dot" rule describes; deciding
//! which is a module path, a constructor or a record projection is name
//! resolution's job during HIR lowering.

use super::decls::param_list;
use super::patterns::pattern;
use super::types::{name, name_ref, path};
use super::{CompletedMarker, Parser};
use crate::kind::SyntaxKind::{self, *};

/// Left and right binding powers. Left < right means left-associative.
fn bin_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    let bp = match kind {
        PIPE_GT => (1, 2),
        PIPE_PIPE => (3, 4),
        AMP_AMP => (5, 6),
        EQ_EQ | BANG_EQ | LT | GT | LT_EQ | GT_EQ => (7, 8),
        PLUS | MINUS => (9, 10),
        STAR | SLASH | PERCENT => (11, 12),
        _ => return None,
    };
    Some(bp)
}

pub(super) fn expr(p: &mut Parser) -> Option<CompletedMarker> {
    expr_bp(p, 0)
}

fn expr_bp(p: &mut Parser, min_bp: u8) -> Option<CompletedMarker> {
    let mut lhs = unary_expr(p)?;
    loop {
        let op = p.current();
        let Some((l_bp, r_bp)) = bin_power(op) else { break };
        if l_bp <= min_bp {
            break;
        }
        if !p.tick() {
            break;
        }
        let m = lhs.precede(p);
        p.bump(op);
        let kind = if op == PIPE_GT { PIPE_EXPR } else { BIN_EXPR };
        if expr_bp(p, r_bp).is_none() {
            p.error("expected an expression after the operator");
        }
        lhs = m.complete(p, kind);
    }
    Some(lhs)
}

fn unary_expr(p: &mut Parser) -> Option<CompletedMarker> {
    if p.at(MINUS) || p.at(BANG) {
        let m = p.start();
        p.bump_any();
        if unary_expr(p).is_none() {
            p.error("expected an operand");
        }
        return Some(m.complete(p, PREFIX_EXPR));
    }
    postfix_expr(p)
}

fn postfix_expr(p: &mut Parser) -> Option<CompletedMarker> {
    let mut lhs = primary_expr(p)?;
    loop {
        if !p.tick() {
            break;
        }
        lhs = match p.current() {
            L_PAREN => {
                let m = lhs.precede(p);
                arg_list(p);
                m.complete(p, CALL_EXPR)
            }
            DOT if p.nth_at(1, IDENT) => {
                let m = lhs.precede(p);
                p.bump(DOT);
                name_ref(p);
                m.complete(p, FIELD_EXPR)
            }
            _ => break,
        };
    }
    Some(lhs)
}

fn arg_list(p: &mut Parser) {
    let m = p.start();
    p.bump(L_PAREN);
    // Record literals are always fine inside parentheses; the suppression
    // applies only to an unparenthesised `match` scrutinee.
    p.with_record_literals(|p| {
        while !p.at(R_PAREN) && !p.at(EOF) {
            if !p.tick() {
                break;
            }
            if expr(p).is_none() {
                p.err_and_bump("expected an argument");
            }
            if !p.eat(COMMA) {
                break;
            }
        }
    });
    p.expect(R_PAREN);
    m.complete(p, ARG_LIST);
}

fn primary_expr(p: &mut Parser) -> Option<CompletedMarker> {
    let cm = match p.current() {
        INT_LIT | FLOAT_LIT | STRING_LIT | TRUE_KW | FALSE_KW => {
            let m = p.start();
            p.bump_any();
            m.complete(p, LITERAL_EXPR)
        }
        UNDERSCORE => {
            let m = p.start();
            p.bump(UNDERSCORE);
            m.complete(p, PLACEHOLDER_EXPR)
        }
        IDENT => {
            let m = p.start();
            name_ref(p);
            m.complete(p, PATH_EXPR)
        }
        COLON => capability_expr(p),
        L_PAREN => paren_or_tuple_expr(p),
        L_BRACE => {
            if at_record_literal(p) {
                record_expr(p)
            } else {
                block(p)
            }
        }
        FN_KW => lambda_expr(p),
        IF_KW => if_expr(p),
        MATCH_KW => match_expr(p),
        _ => return None,
    };
    Some(cm)
}

/// `:ledger.get_history` — names a capability held in the effect's `R` row.
/// Not part of the published EBNF; see `docs/errata.md`.
fn capability_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(COLON);
    path(p);
    m.complete(p, CAPABILITY_EXPR)
}

/// `{` is a record literal when it is empty or starts with `name:`; otherwise
/// it opens a block. Inside a `match` scrutinee it always opens the arm list.
fn at_record_literal(p: &mut Parser) -> bool {
    p.record_literals_allowed()
        && p.at(L_BRACE)
        && (p.nth_at(1, R_BRACE) || (p.nth_at(1, IDENT) && p.nth_at(2, COLON)))
}

fn record_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(L_BRACE);
    while !p.at(R_BRACE) && !p.at(EOF) {
        if !p.tick() {
            break;
        }
        let f = p.start();
        name(p);
        p.expect(COLON);
        if expr(p).is_none() {
            p.error("expected a field value");
        }
        f.complete(p, RECORD_EXPR_FIELD);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(R_BRACE);
    m.complete(p, RECORD_EXPR)
}

fn paren_or_tuple_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(L_PAREN);
    if p.eat(R_PAREN) {
        return m.complete(p, UNIT_EXPR);
    }
    let mut arity = 0usize;
    let mut saw_comma = false;
    p.with_record_literals(|p| {
        while !p.at(R_PAREN) && !p.at(EOF) {
            if !p.tick() {
                break;
            }
            if expr(p).is_none() {
                p.err_and_bump("expected an expression");
            }
            arity += 1;
            if p.eat(COMMA) {
                saw_comma = true;
            } else {
                break;
            }
        }
    });
    p.expect(R_PAREN);
    if arity == 1 && !saw_comma {
        m.complete(p, PAREN_EXPR)
    } else {
        m.complete(p, TUPLE_EXPR)
    }
}

/// `fn x => e`, `fn _ => e`, `fn (a, b) => e`
fn lambda_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(FN_KW);
    if p.at(L_PAREN) {
        param_list(p);
    } else {
        let list = p.start();
        let param = p.start();
        match p.current() {
            IDENT => name(p),
            UNDERSCORE => p.bump(UNDERSCORE),
            _ => p.error("expected a lambda parameter"),
        }
        param.complete(p, PARAM);
        list.complete(p, PARAM_LIST);
    }
    p.expect(FAT_ARROW);
    lambda_or_arm_body(p);
    m.complete(p, LAMBDA_EXPR)
}

/// After `=>` a `{` means a block unless it looks like a record literal.
fn lambda_or_arm_body(p: &mut Parser) {
    p.with_record_literals(|p| {
        if p.at(L_BRACE) && !at_record_literal(p) {
            block(p);
        } else if expr(p).is_none() {
            p.err_and_bump("expected an expression");
        }
    });
}

/// `if cond { … } else if cond { … } else { … }`
///
/// The condition is parsed with record literals suppressed, for the same reason
/// as a `match` scrutinee: the `{` that follows opens the branch, not a record.
fn if_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(IF_KW);
    p.without_record_literals(|p| {
        if expr(p).is_none() {
            p.error("expected a condition");
        }
    });

    if p.at(L_BRACE) {
        block(p);
    } else {
        p.error("expected `{` after the condition");
    }

    if p.eat(ELSE_KW) {
        match p.current() {
            IF_KW => {
                if_expr(p);
            }
            L_BRACE => {
                block(p);
            }
            _ => p.error("expected `{` or `if` after `else`"),
        }
    }
    m.complete(p, IF_EXPR)
}

fn match_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(MATCH_KW);
    p.without_record_literals(|p| {
        if expr(p).is_none() {
            p.error("expected a scrutinee expression");
        }
    });
    p.expect(L_BRACE);
    p.with_record_literals(|p| {
        while !p.at(R_BRACE) && !p.at(EOF) {
            if !p.tick() {
                break;
            }
            match_arm(p);
        }
    });
    p.expect(R_BRACE);
    m.complete(p, MATCH_EXPR)
}

fn match_arm(p: &mut Parser) {
    let m = p.start();
    pattern(p);
    if p.at(IF_KW) {
        let g = p.start();
        p.bump(IF_KW);
        p.without_record_literals(|p| {
            if expr(p).is_none() {
                p.error("expected a guard expression");
            }
        });
        g.complete(p, MATCH_GUARD);
    }
    p.expect(FAT_ARROW);
    lambda_or_arm_body(p);
    p.eat(COMMA);
    m.complete(p, MATCH_ARM);
}

/// `BlockExpr ::= "{" Statement* Expr? "}"` — the trailing expression, if any,
/// is the block's value.
pub(super) fn block(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.expect(L_BRACE);
    p.with_record_literals(|p| {
        loop {
            if p.at(R_BRACE) || p.at(EOF) || !p.tick() {
                break;
            }
            if p.at(LET_KW) {
                super::decls::let_decl(p);
                continue;
            }
            let stmt = p.start();
            if expr(p).is_none() {
                stmt.abandon(p);
                p.err_and_bump("expected a statement or expression");
                continue;
            }
            if p.eat(SEMICOLON) {
                stmt.complete(p, EXPR_STMT);
            } else {
                // No `;`: this is the block's tail expression.
                stmt.abandon(p);
                break;
            }
        }
    });
    p.expect(R_BRACE);
    m.complete(p, BLOCK)
}
