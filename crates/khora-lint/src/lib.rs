//! Lints that need types.
//!
//! An unreachable `match` arm is deliberately *not* here: it is a type error,
//! reported by `khora-types` out of the same usefulness algorithm that decides
//! exhaustiveness, and making it a lint as well would give one mistake two
//! voices.
//!
//! # What separates a lint from an error here
//!
//! An error is a program the compiler will not compile. A lint is one it will
//! compile and that somebody probably did not mean, and everything here is on
//! that side of the line for the same reason: each is *legal and occasionally
//! deliberate*. A capability may be declared to keep a signature uniform across
//! a family of handlers; a statement that computes nothing may be a placeholder
//! mid-edit.
//!
//! Each is also chosen to have **no false positives**. A warning people learn
//! to ignore is worse than no warning, and the way that starts is one that is
//! wrong about real code — so where a judgement was available, this takes the
//! quiet side.
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

#![deny(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

mod allow;

pub use crate::allow::MARKER;

use khora_db::{Db, SourceFile};
use khora_types::{BodyTypes, Type};
use khora_hir::body::{Body, Expr, LocalId, Pat, Stmt};
use text_size::TextRange;

/// Something a program does that it probably did not mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The kebab-case name `[lints]` addresses.
    pub lint: &'static str,
    /// What to tell the reader, in one sentence.
    pub message: String,
    /// Where in the file to point.
    pub range: TextRange,
}

/// Every lint's name, so that a manifest naming one that does not exist can be
/// told what does.
pub const LINTS: &[&str] = &[
    DANGLING_EXPRESSION,
    DISCARDED_RESULT,
    REFERENCE_CYCLE,
    UNKNOWN_ALLOW,
    UNUSED_CAPABILITY,
    USELESS_ALLOW,
];

/// A `// @klint allow` naming something that is not a lint.
///
/// **This is what makes the pragma safe to have.** A misspelled name in a
/// comment would otherwise suppress nothing and say nothing, and the reader
/// would believe the line was handled. `docs/design/lint-hatch.md`.
pub const UNKNOWN_ALLOW: &str = "unknown-allow";

/// A `// @klint allow` that suppressed nothing.
///
/// Stale suppression is real debt: it hides the next finding on that line, and
/// it tells a reader that something was considered when it no longer is.
///
/// **Off by default**, unlike every other lint here. It fires on exactly the
/// lines somebody is already editing to satisfy a new lint, so switching it on
/// while lints are still being added would produce churn in the files under
/// the most pressure. Turn it on with `[lints] useless-allow = "warn"` once
/// they have settled.
pub const USELESS_ALLOW: &str = "useless-allow";

/// How loud a lint is when the manifest does not say.
///
/// Warn for everything except [`USELESS_ALLOW`], for the reason on it. A
/// function rather than a constant so that both the CLI and the language
/// server ask the same question -- they each had `unwrap_or(Warn)` written out
/// before this existed, which is two places to forget.
pub fn default_level(lint: &str) -> khora_manifest::LintLevel {
    if lint == USELESS_ALLOW {
        khora_manifest::LintLevel::Allow
    } else {
        khora_manifest::LintLevel::Warn
    }
}

/// A capability a signature asks for that its body cannot be using.
pub const UNUSED_CAPABILITY: &str = "unused-capability";
/// A statement that computes something and does nothing with it.
pub const DANGLING_EXPRESSION: &str = "dangling-expression";
/// A statement that produces a `Result` and drops it on the floor.
pub const DISCARDED_RESULT: &str = "discarded-result";
/// Named for the problem rather than for the fix.
///
/// The advice this gives will change — today it is "restructure or accept the
/// leak", and when weak references exist it becomes "make this field weak".
/// A lint's name goes in somebody's `khora.toml`, so a name that describes the
/// *remedy* would have to be renamed when the remedy changes, and renaming one
/// breaks every manifest that mentions it. `reference-cycle` is true either
/// way. `docs/roadmap.md` Phase 13.
pub const REFERENCE_CYCLE: &str = "reference-cycle";

/// What the lints find in one file.
///
/// Sorted by position, because a reader goes through a file from the top and a
/// list in pass order makes them jump.
#[salsa::tracked(returns(ref))]
pub fn findings(db: &dyn Db, file: SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    let checked = khora_types::checked(db, file);
    for (name, body) in khora_hir::body::bodies(db, file) {
        unused_capabilities(body, &mut out);
        dangling_expressions(body, &mut out);
        // Paired by name, which is how `Checked` keys them. A body with no
        // types — one whose `derive` was refused, say — is skipped rather than
        // guessed at: `reference_cycles` needs to know what is on the heap and
        // has nothing useful to say without it.
        if let Some((_, types)) = checked.bodies.iter().find(|(n, _)| n == name) {
            reference_cycles(body, types, &mut out);
            discarded_results(body, types, &mut out);
        }
    }

    // **Here rather than in each consumer.** The CLI, the language server and
    // the MCP server all read this, and a suppression one of them honoured and
    // another did not would be the worst kind of inconsistency: the editor
    // says the line is fine and the build does not.
    let text = file.text(db);
    out = suppress(text, out);

    out.sort_by_key(|f| (f.range.start(), f.range.end()));
    out
}

/// Drops what the pragmas allow, and reports on the pragmas themselves.
fn suppress(text: &str, found: Vec<Finding>) -> Vec<Finding> {
    let mut allows = allow::allows(text);
    if allows.is_empty() {
        return found;
    }
    let starts = line_starts(text);

    let mut kept = Vec::new();
    for finding in found {
        let line = line_of(&starts, u32::from(finding.range.start())) as u32;
        let hit = allows
            .iter_mut()
            .find(|allow| allow.line == line && allow.lint == finding.lint);
        match hit {
            Some(allow) => allow.used = true,
            None => kept.push(finding),
        }
    }

    for allow in &allows {
        if !LINTS.contains(&allow.lint.as_str()) {
            kept.push(Finding {
                lint: UNKNOWN_ALLOW,
                message: format!(
                    "`{}` is not a lint, so this allows nothing. What there is: {}",
                    allow.lint,
                    LINTS.join(", ")
                ),
                range: allow.range,
            });
        } else if !allow.used {
            kept.push(Finding {
                lint: USELESS_ALLOW,
                message: format!("nothing here reports `{}`, so this allows nothing", allow.lint),
                range: allow.range,
            });
        }
    }
    kept
}

/// The byte offset each line starts at.
fn line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(at as u32 + 1);
        }
    }
    starts
}

/// Which line an offset is on, zero-based.
fn line_of(starts: &[u32], offset: u32) -> usize {
    match starts.binary_search(&offset) {
        Ok(exact) => exact,
        Err(after) => after - 1,
    }
}

/// A capability the signature asks for and the body cannot be using.
///
/// **Forwarding is what makes this hard.** A capability can be read outright —
/// `rng.int()` — or passed along without being named, since calling a function
/// that itself requires `Random` hands this one over with no `Expr::Local`
/// anywhere. A pass that looked only at reads would report every pass-through
/// function in the standard library.
///
/// Which labels a call needs is the callee's row, which the checker reads and
/// does not keep. `Body::capabilities` is not it: lowering runs before the
/// checker, so it records every label *in scope* at each call site rather than
/// the ones the callee wanted.
///
/// So this stays quiet whenever the body contains a call at all. What it still
/// catches is the case worth catching — a signature demanding a capability over
/// a body that could not possibly use one:
///
/// ```khora
/// fn area(r: Int) -> Int with { clock: Clock } { r * r }
/// ```
///
/// **To sharpen it**, `BodyTypes` needs a per-call-site record of the labels
/// the callee required; `check/effects.rs` computes exactly that and drops it.
/// With it, "used" becomes "read, or required by something this body calls" and
/// the call-free restriction goes away. `lambda_captures` is the same fact
/// published for a different consumer, and is the shape to copy.
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

// --- a `Result` nobody looked at --------------------------------------------

/// A statement that produces a `Result` and drops it on the floor.
///
/// **`expr!` is a mark on the effect row and the identity on values**, so
/// `db.execute(sql, binds)!` as a statement does nothing about the `Result` it
/// returns. That has twice reported success against a database that had aborted
/// the transaction or was not running at all: the outer half of the answer read
/// fine, and the half saying what happened went on the floor.
///
/// A lint rather than an error, because dropping one is occasionally deliberate
/// and the language already says so with `let _ = db.rollback();` — which
/// `std::db`'s rollback path uses, so the engine's complaint about the rollback
/// cannot hide the reason for it.
///
/// # What it sees
///
/// A statement-position expression whose type is a `Result`, anywhere but the
/// tail of a block — the tail is the block's value and is not discarded.
/// Matched on the name rather than on the declaring module: a `Result` a
/// program declared itself is the same mistake, and the checker has already
/// agreed the name refers to one type.
fn discarded_results(body: &Body, types: &BodyTypes, out: &mut Vec<Finding>) {
    for (_, expr) in body.exprs() {
        let Expr::Block { stmts, .. } = expr else { continue };
        for stmt in stmts {
            let Stmt::Expr(id) = stmt else { continue };
            let Type::Adt { name, .. } = types.of(*id) else { continue };
            if name != "Result" {
                continue;
            }
            out.push(Finding {
                lint: DISCARDED_RESULT,
                message: "this produces a `Result` and nothing looks at it, so a failure here is silent. `match` it, mark it with `!` in a function that can raise, or write `let _ =` to say the answer was considered"
                    .to_string(),
                range: body.range(*id),
            });
        }
    }
}

// --- reference cycles ------------------------------------------------------

/// A field assignment that closes a loop in the heap.
///
/// **Why this is worth a lint at all.** Reference counting works for every
/// shape of data except a loop: in `a.next = b; b.next = a` each object holds
/// the other, so neither count reaches zero. `docs/design/memory.md` §4 rules
/// out a tracing collector and names weak references as what breaks a cycle —
/// and weak references do not exist yet, while mutable fields do, so the cycle
/// compiles today with nothing to reach for instead.
///
/// The failure is **silent**: nothing is freed early, nothing is read after
/// free, the memory is simply never returned. This is the diagnostic
/// `memory.md` §4 asks for.
///
/// # What it sees, and what it does not
///
/// One function body, and reachability built from what that body does:
/// constructing a value out of a local, and assigning a local into a field.
/// It warns when a field assignment stores something that can already reach
/// the object being assigned into.
///
/// It does **not** see across function boundaries, through a `Shared` cell, or
/// through a collection. A cycle built in two functions is invisible to it.
/// That is the honest limit of a syntactic pass and the reason this is a
/// warning rather than an error: it finds the accident, and says nothing about
/// what it cannot see.
///
/// **No false positives is the harder half.** A lint people learn to ignore is
/// worse than no lint, and the way that starts is one that is wrong about real
/// code — so where a judgement was available this takes the quiet side, as the
/// other two passes here do.
fn reference_cycles(body: &Body, types: &BodyTypes, out: &mut Vec<Finding>) {
    let Some(root) = body.root else { return };
    let mut walk = Cycles { body, types, reaches: BTreeMap::new(), out };
    walk.expr(root);
}

/// The walk `reference_cycles` runs.
///
/// **Structural, from the root, and not a scan of the arena.** The first
/// version iterated `body.exprs()`, which is allocation order rather than
/// program order: lowering is depth-first and append-only, so a block is
/// created *after* every statement inside it. An assignment was therefore seen
/// before the `let` two lines above it, and the edge that made the cycle had
/// not been recorded yet — the pass missed the shape it exists to catch and
/// reported nothing, which is the worst way for a lint to be wrong.
struct Cycles<'a> {
    body: &'a Body,
    types: &'a BodyTypes,
    /// What each local can reach, as far as the walk has got.
    ///
    /// `BTreeMap` and `BTreeSet` rather than the hashed pair: a `HashSet`'s
    /// per-process seed leaking into compiler output is a bug this repository
    /// has already had once, in `khora-perceus`, and findings are ordered.
    reaches: BTreeMap<LocalId, BTreeSet<LocalId>>,
    out: &'a mut Vec<Finding>,
}

impl Cycles<'_> {
    fn expr(&mut self, id: khora_hir::body::ExprId) {
        match self.body.expr(id).clone() {
            Expr::Block { stmts, tail } => {
                for stmt in &stmts {
                    match stmt {
                        Stmt::Let { pat, init, .. } => {
                            let Some(init) = init else { continue };
                            self.expr(*init);
                            // Recorded *after* the initializer is walked, so a
                            // binding cannot reach itself through its own
                            // right-hand side.
                            if let Pat::Bind(local) = self.body.pat(*pat) {
                                let mut named = BTreeSet::new();
                                locals_in(self.body, *init, &mut named);
                                self.reaches.entry(*local).or_default().extend(named);
                            }
                        }
                        Stmt::Expr(e) => self.expr(*e),
                    }
                }
                if let Some(tail) = tail {
                    self.expr(tail);
                }
            }
            Expr::Assign { target, value } => {
                self.expr(target);
                self.expr(value);
                self.assignment(target, value);
            }
            Expr::Call { callee, args } => {
                self.expr(callee);
                for arg in args {
                    self.expr(arg);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Unary { operand, .. } => self.expr(operand),
            Expr::Field { base, .. } => self.expr(base),
            Expr::If { condition, then_branch, else_branch } => {
                self.expr(condition);
                self.expr(then_branch);
                if let Some(other) = else_branch {
                    self.expr(other);
                }
            }
            Expr::While { condition, body } => {
                self.expr(condition);
                self.expr(body);
            }
            Expr::Loop { body } => self.expr(body),
            Expr::Match { scrutinee, arms } | Expr::Catch { inner: scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in &arms {
                    if let Some(guard) = arm.guard {
                        self.expr(guard);
                    }
                    self.expr(arm.body);
                }
            }
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::Record { fields, .. } => {
                for (_, value) in &fields {
                    self.expr(*value);
                }
            }
            Expr::Tuple(items) => {
                for item in &items {
                    self.expr(*item);
                }
            }
            Expr::Return(inner) | Expr::Break(inner) => {
                if let Some(inner) = inner {
                    self.expr(inner);
                }
            }
            Expr::Try(inner) | Expr::Raise(inner) => self.expr(inner),
            _ => {}
        }
    }

    /// `target.field = value`: does it close a loop?
    fn assignment(&mut self, target: khora_hir::body::ExprId, value: khora_hir::body::ExprId) {
        let Expr::Field { base, name } = self.body.expr(target).clone() else { return };
        let Some(into) = root_local(self.body, base) else { return };

        // **Only a heap value can be part of a loop**, and this is where the
        // first version was wrong about real code. `self.wanted = held`, with
        // `held` an `Int` from `Array::length`, was reported as a cycle in
        // `std/core.kh`: a scalar is copied, not pointed at, and copying one
        // into a field can no more make a loop than adding two numbers can.
        //
        // Two false positives out of twenty-one files is exactly the failure a
        // lint cannot have — the module documentation above says a warning
        // people learn to ignore is worse than no warning, and this is how
        // that starts.
        if !khora_perceus::is_boxed(self.types.of(value)) {
            return;
        }

        let mut stored = BTreeSet::new();
        locals_in(self.body, value, &mut stored);

        let closes =
            stored.iter().any(|from| *from == into || can_reach(&self.reaches, *from, into));
        if closes {
            self.out.push(Finding {
                lint: REFERENCE_CYCLE,
                message: format!(
                    "this stores something that already reaches `{}`, which makes a loop in \
                     the heap. Reference counting cannot free a loop, so the memory is never \
                     returned — and nothing else will say so. Break the link by storing an \
                     identifier instead of the object, or by keeping the back-reference \
                     outside the structure; `khora_live_count` shows the leak if you want to \
                     see it",
                    field_of(self.body, base, &name)
                ),
                range: self.body.range(target),
            });
        }

        // Recorded whether or not it was reported: the edge exists either way,
        // and a later assignment may be the one that closes the loop.
        self.reaches.entry(into).or_default().extend(stored);
    }
}

/// How to name the thing being assigned into, for the message.
fn field_of(body: &Body, base: khora_hir::body::ExprId, field: &str) -> String {
    match body.expr(base) {
        Expr::Local(local) => format!("{}.{field}", body.local(*local).name),
        _ => field.to_string(),
    }
}

/// The local a place expression is rooted at: `a.b.c` is rooted at `a`.
fn root_local(body: &Body, id: khora_hir::body::ExprId) -> Option<LocalId> {
    match body.expr(id) {
        Expr::Local(local) => Some(*local),
        Expr::Field { base, .. } => root_local(body, *base),
        _ => None,
    }
}

/// Every local an expression mentions.
///
/// Deliberately shallow about *how* they are mentioned: a local inside a record
/// literal, a constructor call or a tuple is reachable from the result, and one
/// inside an arbitrary call might be. Treating them alike is what keeps this
/// from needing an escape analysis, and the cost is that it can only be a
/// warning.
fn locals_in(body: &Body, id: khora_hir::body::ExprId, out: &mut BTreeSet<LocalId>) {
    match body.expr(id) {
        Expr::Local(local) => {
            out.insert(*local);
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                locals_in(body, *value, out);
            }
        }
        Expr::Tuple(items) => {
            for item in items {
                locals_in(body, *item, out);
            }
        }
        // **Only a constructor.** `List::Cons(head, tail)` produces a thing
        // that holds its arguments; `advance(pending, n)` produces a thing
        // computed *from* one, which is not the same and is the overwhelming
        // majority of calls.
        //
        // Treating every call as the first was wrong, and wrong in the
        // direction that matters: the first real Khora written after this
        // landed — `packages/postgres`, `c.pending = advance(c.pending, n)`,
        // a function that builds a new array and returns it — was reported
        // twice as a cycle. A lint people learn to ignore is worse than no
        // lint, and this file's own header says where a judgement is available
        // to take the quiet side. It had not.
        Expr::Call { callee, args } if constructs(body, *callee) => {
            for arg in args {
                locals_in(body, *arg, out);
            }
        }
        // Everything else contributes nothing. A field read is the interesting
        // omission: `b.next` is a thing `b` points *at*, so the value does not
        // contain `b` and saying it does was the same mistake pointing the
        // other way. Following it properly needs per-field reachability, which
        // is more than a warning is worth.
        _ => {}
    }
}

/// Whether a callee builds a value out of its arguments.
///
/// A variant constructor does — `Option::Some(x)` holds `x`. A function does
/// not, whatever it happens to do inside.
fn constructs(body: &Body, callee: khora_hir::body::ExprId) -> bool {
    matches!(
        body.expr(callee),
        Expr::Path(khora_hir::Resolution::Variant { .. })
    )
}

/// Whether `from` can already reach `to`, following the edges recorded so far.
fn can_reach(
    reaches: &BTreeMap<LocalId, BTreeSet<LocalId>>,
    from: LocalId,
    to: LocalId,
) -> bool {
    let mut seen: BTreeSet<LocalId> = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(here) = stack.pop() {
        if here == to {
            return true;
        }
        if !seen.insert(here) {
            continue;
        }
        if let Some(next) = reaches.get(&here) {
            stack.extend(next.iter().copied());
        }
    }
    false
}
