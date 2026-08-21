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

/// Left and right binding powers. Left < right means left-associative;
/// left > right means right-associative.
fn bin_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    let bp = match kind {
        // Assignment is right-associative and binds loosest, so `x = a |> b`
        // assigns the whole pipeline. `let` handles its own `=` before calling
        // in here, so there is no ambiguity with a binding.
        EQ => (2, 1),
        PIPE_GT => (3, 4),
        PIPE_PIPE => (5, 6),
        AMP_AMP => (7, 8),
        EQ_EQ | BANG_EQ | LT | GT | LT_EQ | GT_EQ => (9, 10),
        PLUS | MINUS => (11, 12),
        STAR | SLASH | PERCENT => (13, 14),
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
        let kind = match op {
            PIPE_GT => PIPE_EXPR,
            EQ => ASSIGN_EXPR,
            _ => BIN_EXPR,
        };
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
            // `expr!` — this call can abort the enclosing function. Marking it
            // is a deliberate divergence from Koka; see docs/design/effects.md.
            BANG => {
                let m = lhs.precede(p);
                p.bump(BANG);
                m.complete(p, TRY_EXPR)
            }
            CATCH_KW => {
                let m = lhs.precede(p);
                p.bump(CATCH_KW);
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
                m.complete(p, CATCH_EXPR)
            }
            WITH_KW => {
                let m = lhs.precede(p);
                p.bump(WITH_KW);
                context_row(p);
                m.complete(p, WITH_EXPR)
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
        L_PAREN => paren_or_tuple_expr(p),
        L_BRACK => list_expr(p),
        L_BRACE => {
            if at_record_literal(p) {
                record_expr(p)
            } else {
                block(p)
            }
        }
        FN_KW => lambda_expr(p),
        RAISE_KW => raise_expr(p),
        WHILE_KW => while_expr(p),
        LOOP_KW => loop_expr(p),
        BREAK_KW => jump_expr(p, BREAK_KW, BREAK_EXPR),
        CONTINUE_KW => jump_expr(p, CONTINUE_KW, CONTINUE_EXPR),
        RETURN_KW => jump_expr(p, RETURN_KW, RETURN_EXPR),
        HANDLER_KW => handler_expr(p),
        WITH_KW => with_block(p),
        IF_KW => if_expr(p),
        MATCH_KW => match_expr(p),
        _ => return None,
    };
    Some(cm)
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

fn list_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(L_BRACK);
    p.with_record_literals(|p| {
        while !p.at(R_BRACK) && !p.at(EOF) {
            if !p.tick() {
                break;
            }
            if expr(p).is_none() {
                p.err_and_bump("expected a list element");
            }
            if !p.eat(COMMA) {
                break;
            }
        }
    });
    p.expect(R_BRACK);
    m.complete(p, LIST_EXPR)
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

/// The row supplied to a handler installation: a record literal, a named
/// context, or a named context with overrides.
///
/// Because contexts are rows, `Production { ai: stub }` is row update — the
/// same operation the type system already performs on capability rows.
fn context_row(p: &mut Parser) {
    p.with_record_literals(|p| {
        let mut found = false;
        if p.at(IDENT) {
            let m = p.start();
            path(p);
            m.complete(p, PATH_EXPR);
            found = true;
        }
        // A named context may stand alone (`expr with Mock`), carry overrides
        // (`expr with Mock { ai: stub }`), or be replaced by a bare row.
        if at_record_literal(p) {
            record_expr(p);
            found = true;
        }
        if !found {
            p.error("expected a handler row or a named context");
        }
    });
}

/// Expressions that end in a block, and so stand as statements without a
/// trailing `;` — the same rule Rust uses. Without this, `if c { .. }` in the
/// middle of a block would be read as the block's tail expression and
/// everything after it would be orphaned.
fn is_block_like(kind: SyntaxKind) -> bool {
    matches!(kind, IF_EXPR | MATCH_EXPR | WHILE_EXPR | LOOP_EXPR | WITH_BLOCK | BLOCK)
}

/// True where an expression cannot continue, so `break`, `continue` and
/// `return` know whether a value follows them.
fn at_expr_end(p: &Parser) -> bool {
    p.at_any(&[SEMICOLON, R_BRACE, R_PAREN, R_BRACK, COMMA, EOF])
}

/// `while cond { .. }`
fn while_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(WHILE_KW);
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
    m.complete(p, WHILE_EXPR)
}

/// `loop { .. }` — exited with `break`, which may carry the loop's value.
fn loop_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(LOOP_KW);
    if p.at(L_BRACE) {
        block(p);
    } else {
        p.error("expected `{` after `loop`");
    }
    m.complete(p, LOOP_EXPR)
}

/// `break`, `break expr`, `continue`, `return`, `return expr`.
///
/// These transfer control non-locally, so crossing a handler boundary has to
/// unwind and run finalisers exactly as a raise does — see D1.
fn jump_expr(p: &mut Parser, keyword: SyntaxKind, kind: SyntaxKind) -> CompletedMarker {
    let m = p.start();
    p.bump(keyword);
    if !at_expr_end(p) && keyword != CONTINUE_KW {
        if expr(p).is_none() {
            p.error("expected a value");
        }
    }
    m.complete(p, kind)
}

/// `raise expr` — performs an operation of the error row. Its type is `Never`,
/// so it may appear wherever an expression may.
fn raise_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(RAISE_KW);
    if expr(p).is_none() {
        p.error("expected an error value to raise");
    }
    m.complete(p, RAISE_EXPR)
}

/// `handler for Ledger { get_history: fn id => .., .. }`
fn handler_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(HANDLER_KW);
    p.expect(FOR_KW);
    path(p);
    if at_record_literal(p) || p.at(L_BRACE) {
        record_expr(p);
    } else {
        p.error("expected `{` with the effect's operations");
    }
    m.complete(p, HANDLER_EXPR)
}

/// `with { ledger: live_ledger } { .. }` — installs handlers over a region.
///
/// Handlers must lexically enclose the computation they serve: in direct style
/// a call evaluates immediately, so a `|> provide(h)` pipeline cannot work.
fn with_block(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(WITH_KW);
    context_row(p);
    if p.at(L_BRACE) {
        block(p);
    } else {
        p.error("expected a block after the handler row");
    }
    m.complete(p, WITH_BLOCK)
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

pub(super) fn match_arm(p: &mut Parser) {
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
            let Some(parsed) = expr(p) else {
                stmt.abandon(p);
                p.err_and_bump("expected a statement or expression");
                continue;
            };
            if p.eat(SEMICOLON) {
                stmt.complete(p, EXPR_STMT);
            } else if is_block_like(parsed.kind()) && !p.at(R_BRACE) {
                // A block-like expression followed by more code is a statement.
                stmt.complete(p, EXPR_STMT);
            } else {
                // Otherwise this is the block's tail expression.
                stmt.abandon(p);
                break;
            }
        }
    });
    p.expect(R_BRACE);
    m.complete(p, BLOCK)
}
