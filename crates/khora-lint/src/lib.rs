//! Lints that need types.
//!
//! Roadmap phase 10.3. Two passes, and the third the roadmap asks for turned
//! out to exist already: a `match` arm that cannot be reached is a *type
//! error*, not a lint, reported by `khora-types` out of the same usefulness
//! algorithm that decides exhaustiveness. Making it a lint as well would give
//! one mistake two voices.
//!
//! # What separates a lint from an error here
//!
//! An error is a program the compiler will not compile. A lint is a program it
//! will compile and that somebody probably did not mean. The two below are on
//! the lint side of that line for the same reason: each is *legal and
//! occasionally deliberate*. A capability may be declared because a signature
//! is being kept uniform across a family of handlers; a statement that computes
//! nothing may be a placeholder mid-edit. Neither is worth refusing to build.
//!
//! Both are also chosen to have **no false positives**, which matters more for
//! a lint than for an error: a warning people learn to ignore is worse than no
//! warning, and the way that starts is one that is wrong about real code.
//! Where a judgement was available, this takes the quiet side.
//!
//! # Levels
//!
//! Each lint has a kebab-case name, which is what `[lints]` in `khora.toml`
//! addresses:
//!
//! ```toml
//! [lints]
//! unused-capability = "deny"
//! ```
//!
//! Reading that table is the caller's job. This crate reports what it finds and
//! has no opinion about how loud it is — the manifest is one project's policy
//! and these are facts about a file.

use khora_db::{Db, SourceFile};
use khora_hir::body::{Body, Expr, LocalId, Pat, Stmt};
use text_size::TextRange;

/// Something a program does that it probably did not mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The kebab-case name `[lints]` addresses.
    pub lint: &'static str,
    pub message: String,
    pub range: TextRange,
}

/// Every lint's name, so that a manifest naming one that does not exist can be
/// told what does.
pub const LINTS: &[&str] = &[DANGLING_EXPRESSION, UNUSED_CAPABILITY];

pub const UNUSED_CAPABILITY: &str = "unused-capability";
pub const DANGLING_EXPRESSION: &str = "dangling-expression";

/// What the lints find in one file.
///
/// Sorted by position, because a reader goes through a file from the top and a
/// list in pass order makes them jump.
#[salsa::tracked(returns(ref))]
pub fn findings(db: &dyn Db, file: SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    for (_, body) in khora_hir::body::bodies(db, file) {
        unused_capabilities(body, &mut out);
        dangling_expressions(body, &mut out);
    }
    out.sort_by_key(|f| (f.range.start(), f.range.end()));
    out
}

/// A capability the signature asks for and the body cannot be using.
///
/// **Forwarding is what makes this hard.** A capability can be read outright —
/// `rng.int()` — but it can also be passed along without being named: calling a
/// function that itself requires `Random` hands this one over, and there is no
/// `Expr::Local` anywhere for that. A pass that looked only at reads would
/// report every pass-through function in the standard library.
///
/// Which labels a given call needs is the callee's row. The checker reads it —
/// it has to, to do the row subtraction — and does not keep it.
/// `Body::capabilities` is the nearest thing on hand and is not it: lowering
/// records every label *in scope* at each call site, because lowering runs
/// before the checker and cannot know which ones the callee wanted.
///
/// So this stays quiet whenever the body contains a call at all, which is the
/// same conservatism [`is_inert`] uses and for the same reason. What it still
/// catches is the case worth catching: a signature that demands a capability
/// over a body that could not possibly use one.
///
/// ```khora
/// fn area(r: Int) -> Int with { clock: Clock } { r * r }
/// ```
///
/// **To sharpen it**, `BodyTypes` needs a per-call-site record of the labels
/// the callee required — the checker computes exactly that in
/// `check/effects.rs` and drops it on the floor. With that, "used" becomes
/// "read, or required by something this body calls", and the call-free
/// restriction goes away. `lambda_captures` is the same fact published for a
/// different consumer, and is the shape to copy.
fn unused_capabilities(body: &Body, out: &mut Vec<Finding>) {
    if body.evidence.is_empty() {
        return;
    }

    let mut read: Vec<LocalId> = Vec::new();
    let mut calls = false;
    for (_, expr) in body.exprs() {
        match expr {
            Expr::Local(local) => read.push(*local),
            Expr::Call { .. } => calls = true,
            _ => {}
        }
    }
    if calls {
        return;
    }

    for (label, pat) in &body.evidence {
        let Pat::Bind(local) = body.pat(*pat) else { continue };
        if read.contains(local) {
            continue;
        }
        out.push(Finding {
            lint: UNUSED_CAPABILITY,
            message: format!(
                "`{label}` is required by this signature and this body cannot be using it. \
                 Every caller has to supply it, so asking for it costs them something and \
                 buys nothing"
            ),
            range: body.local(*local).range,
        });
    }
}

/// A statement that computes a value and throws it away.
///
/// `x + 1;` in the middle of a block is almost always a line somebody meant to
/// bind, return, or finish. It is legal, and it does nothing at all.
///
/// **Deliberately syntactic, and deliberately narrow.** Only an expression that
/// *cannot* do anything is reported: no call, no assignment, no `!`, nothing
/// that could raise. That rules out the interesting judgement calls — a call
/// whose result is ignored is often exactly right, and deciding which ones are
/// not needs to know whether the callee does anything, which is a purity
/// analysis rather than a lint. This is the subset where being wrong is
/// impossible, and a lint people trust is worth more than a lint that catches
/// everything.
///
/// The tail expression of a block is never reported: that is the block's value.
fn dangling_expressions(body: &Body, out: &mut Vec<Finding>) {
    for (_, expr) in body.exprs() {
        let Expr::Block { stmts, .. } = expr else { continue };
        for stmt in stmts {
            let Stmt::Expr(id) = stmt else { continue };
            if !is_inert(body, *id) {
                continue;
            }
            out.push(Finding {
                lint: DANGLING_EXPRESSION,
                message: "this computes a value and then discards it, so the line does \
                          nothing. Bind it with `let`, return it, or delete it"
                    .to_string(),
                range: body.range(*id),
            });
        }
    }
}

/// Whether an expression is incapable of doing anything but produce a value.
///
/// Conservative in one direction only: a `false` here is always safe, and it is
/// what every unlisted form gets. See [`dangling_expressions`].
fn is_inert(body: &Body, id: khora_hir::body::ExprId) -> bool {
    match body.expr(id) {
        // A literal or a read. Nothing observable happens.
        Expr::Literal(_) | Expr::Local(_) => true,
        // Reading a field cannot run anything: there are no property accessors.
        Expr::Field { base, .. } => is_inert(body, *base),
        // Arithmetic and comparison over inert operands. `&&` and `||` are
        // included: they are lazy, but what they are lazy about is also inert.
        Expr::Binary { lhs, rhs, .. } => is_inert(body, *lhs) && is_inert(body, *rhs),
        Expr::Unary { operand, .. } => is_inert(body, *operand),
        // Everything else — calls, assignment, `if`, `match`, `while`, `with`,
        // `!` — either does something or might. `Missing` and `Unresolved` are
        // parse and resolution failures, and a lint on top of an error is
        // noise.
        _ => false,
    }
}
