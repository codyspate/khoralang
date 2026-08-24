//! The type checker: one `Checker` per function body.
//!
//! Hindley-Milner with row unification, and the state below is what one body
//! needs — the substitution, what each expression and local resolved to, the
//! demands still owed, and the diagnostics. `unify` does the solving.
//!
//! Split across the modules named here because a `Checker` grew fifty methods
//! and they cluster: inferring an expression form, resolving a call, moving a
//! row, deciding what may cross a fiber, instantiating a parameter, and taking
//! a pattern apart. Rust lets an inherent impl live in several modules of one
//! crate, so each opens `impl<'a> Checker<'a>` again. Roadmap 9.6.3.

use super::*;

mod bounds;
mod calls;
mod effects;
mod expr;
mod patterns;
mod sharing;

/// Which clause a requirement came from.
///
/// Recorded rather than guessed. The two rows look alike — both are sets of
/// labels — and the only reliable difference is which clause wrote them, since
/// a capability's label is a field name and an error's is a type name only by
/// convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clause {
    Requires,
    Raises,
}

impl Clause {
    fn verb(self) -> &'static str {
        match self {
            Clause::Requires => "require",
            Clause::Raises => "raise",
        }
    }

    /// How to name one entry of this kind of row in a message.
    fn describe(self, label: &str, ty: &Type) -> String {
        match self {
            // A capability is supplied under a label, so both halves matter.
            Clause::Requires => format!("{label}: {ty}"),
            // An error is labelled by its own type name, and printing
            // `DbError: DbError` reads as a mistake.
            Clause::Raises => format!("{ty}"),
        }
    }
}

/// One effect a body's call sites asked of the function containing them.
pub(crate) struct Demand {
    /// Whether the callee was *known* to be fallible when this was recorded.
    ///
    /// Kept because the row does not survive to say so: a `catch` empties it,
    /// and so does a closure absorbing it, and neither of those excuses the
    /// mark. The row answers "what can leave"; this answers "was there
    /// anything to mark", which is a different question the moment something
    /// discharges the first one.
    fallible: bool,
    clause: Clause,
    row: Type,
    range: TextRange,
    callee: String,
    /// The call this came from, for checking that a fallible one is marked.
    /// `None` for a `raise`, which is its own mark.
    site: Option<ExprId>,
}

pub(crate) struct Checker<'a> {
    pub(crate) types: &'a TypeMap,
    pub(crate) body: &'a Body,
    pub(crate) signature: &'a Signature,
    pub(crate) locals: HashMap<LocalId, Type>,
    pub(crate) exprs: HashMap<ExprId, Type>,
    pub(crate) instantiations: HashMap<ExprId, (String, Vec<Type>)>,
    pub(crate) unifier: Unifier,
    /// The type of each lambda currently being inferred, innermost last, so
    /// that a recursive closure can refer to itself before its body is done.
    pub(crate) lambdas: Vec<Type>,
    /// What this body has demanded of its caller so far, accumulated as calls
    /// are checked and compared against the signature at the end.
    ///
    /// Requirements flow *upward*: a function that calls something needing
    /// `ledger` needs `ledger` too, unless a `with` block supplies it. Rows
    /// are checked against the declaration rather than inferred into it,
    /// because an exported signature is a promise and inferring one silently
    /// would let a body widen it. `docs/design/effects.md`.
    pub(crate) demanded: Vec<Demand>,
    /// Where each deferred projection was written, in the order the unifier
    /// deferred them, so `settle_projections` can report against the source.
    pub(crate) projections: Vec<(TextRange, String)>,
    /// The lambdas currently being inferred, innermost last, each with the
    /// bindings it has been found to use implicitly.
    pub(crate) enclosing_lambdas: Vec<(ExprId, Vec<khora_hir::body::LocalId>)>,
    /// The finished answer, moved out as each lambda closes.
    pub(crate) lambda_captures: HashMap<ExprId, Vec<khora_hir::body::LocalId>>,
    /// The capabilities in scope from enclosing `with` blocks.
    ///
    /// A call inside one is served by it, so its labels never reach the
    /// signature. That is row subtraction: `with` *discharges* a requirement
    /// rather than forwarding it.
    pub(crate) installed: Vec<String>,
    /// The loops currently being inferred, innermost last.
    ///
    /// Each holds the type its `break`s agree on and whether any `break`
    /// carried a value at all. A loop nobody breaks out of with a value
    /// produces `()`; one that does produces what they carry, and two `break`s
    /// carrying different types is a mismatch reported at the second.
    pub(crate) loops: Vec<(Type, bool)>,
    /// The open tail of every lambda's inferred `raises` row, in the order the
    /// lambdas were seen.
    ///
    /// A lambda's error row is a **lower bound**, not an exact answer: the body
    /// raises at least these, and the context may reasonably ask it to be
    /// declared as raising more. That is what makes a stub usable where a
    /// fallible operation is expected — a mock that never fails satisfies
    /// `raises IoError`, because raising fewer things is always safe.
    ///
    /// The tail is a variable, so it is filled in by whatever the lambda is
    /// checked against. Anything still unsolved when the body is done was never
    /// asked for anything, and defaults to closed-empty: nothing said this
    /// could fail, so it cannot. Without that default an unconstrained tail
    /// would leave the row *open*, and an open row is a fallible one to the
    /// code generator — every lambda would return a tagged pair for nothing.
    pub(crate) open_raises: Vec<Type>,
    /// The type the surrounding expression is asking for, when there is one.
    ///
    /// Only integer literals read it, and only to decide which integer they
    /// are: `let b: U8 = 65` has to work, and 65 on its own is an `Int`. This
    /// is a *hint*, not a demand — `require` still runs afterwards, so a wrong
    /// hint changes which error is reported and never whether one is.
    ///
    /// Consumed by the first `infer` that sees it, and re-armed only where a
    /// type flows through unchanged: the branches of an `if`, the tail of a
    /// block, the arms of a `match`. Anywhere else it would leak into a
    /// subexpression that means something different — the `0` in `array[0]`
    /// is an index, not a `U8`, however the result is being used.
    pub(crate) hint: Option<Type>,
    /// Calls written with `!`.
    ///
    /// A call that can leave the function has to say so at the call site —
    /// that is the whole justification for the mark in
    /// `docs/design/effects.md`. Recorded rather than checked inline because
    /// the inner expression is inferred before its parent is known.
    pub(crate) marked: Vec<ExprId>,
    pub(crate) errors: Vec<HirError>,
}

impl<'a> Checker<'a> {
    fn error(&mut self, message: impl Into<String>, range: TextRange) {
        self.errors.push(HirError { message: message.into(), range });
    }

    pub(crate) fn check_function(&mut self) {
        for (i, pat) in self.body.params.iter().enumerate() {
            let ty = self.signature.params.get(i).cloned().unwrap_or(Type::Unknown);
            self.bind_pattern(*pat, &ty);
        }
        // `with { ledger: Ledger }` binds `ledger` for the body at the type the
        // row gave it.
        let required = match self.signature.requires.clone() {
            Type::Row { fields, .. } => fields,
            _ => Vec::new(),
        };
        for (label, pat) in self.body.evidence.clone() {
            let ty = required
                .iter()
                .find(|(l, _)| *l == label)
                .map(|(_, t)| t.clone())
                .unwrap_or(Type::Unknown);
            self.bind_pattern(pat, &ty);
        }

        let Some(root) = self.body.root else { return };
        let actual = self.infer(root);
        let expected = self.signature.ret.clone();
        if let Err(why) = self.unifier.unify(&expected, &actual) {
            let expected = self.unifier.zonk(&expected);
            let actual = self.unifier.zonk(&actual);
            let range = self.body.range(root);
            // The plain mismatch would read "expected `Int`, found `Bool`",
            // which repeats what the sentence already said.
            let message = match why {
                Mismatch::Types { expected: inner, found: got } => {
                    let inner = self.unifier.zonk(&inner);
                    let got = self.unifier.zonk(&got);
                    let detail = disagreement((&expected, &actual), (&inner, &got));
                    let head = format!("this function returns `{expected}`,");
                    format!("{head} but its body has type `{actual}`{detail}")
                }
                // The other mismatches are whole sentences of their own, so
                // they are joined rather than folded into "but its body ...",
                // which produced "but its body `A` is a type the caller
                // chooses".
                other => format!("this function returns `{expected}`; {other}"),
            };
            self.error(message, range);
        }
    }

    /// Infers `id` and requires it to fit `expected`.
    fn expect(&mut self, id: ExprId, expected: &Type, context: &str) -> Type {
        // Armed for the literal case and cleared by the `infer` below whatever
        // it turns out to be, so it can never be read by an unrelated later
        // expression.
        self.hint = Some(self.unifier.zonk(expected));
        let actual = self.infer(id);
        let range = self.body.range(id);
        self.require(expected, &actual, context, range);
        actual
    }

    /// Reports a literal that cannot be the fixed-width integer being asked of
    /// it.
    ///
    /// A compile-time version of the overflow trap, and the same reasoning:
    /// `let b: U8 = 300` is a mistake with one right answer, and truncating it
    /// silently to 44 is the kind of thing that is found in production.
    fn check_literal_fits(&mut self, text: &str, kind: IntKind, range: TextRange) {
        let cleaned = text.replace('_', "");
        let Ok(value) = cleaned.parse::<i128>() else {
            // Too wide for even an i128, so certainly too wide for this. The
            // `Int` path reports its own version of this.
            self.error(format!("`{text}` does not fit in `{}`", kind.name()), range);
            return;
        };
        let (lo, hi) = kind.range();
        if value < lo || value > hi {
            self.error(
                format!(
                    "`{text}` does not fit in `{}`, which holds {lo} to {hi}",
                    kind.name()
                ),
                range,
            );
        }
    }

    /// Unifies two types for the information, not for the verdict.
    ///
    /// Used to push an expected type into a call before its arguments are
    /// checked. A failure is dropped: the caller is speculating, and the real
    /// check happens where the expectation came from.
    ///
    /// The deferred-projection bookkeeping still has to happen, because
    /// `settle_projections` pairs the unifier's deferred list with
    /// `self.projections` by position — leaving one out slides every later
    /// diagnostic onto the wrong range.
    fn hint_at(&mut self, expected: &Type, found: &Type, range: TextRange) {
        let before = self.unifier.deferred_len();
        let _ = self.unifier.unify(expected, found);
        for _ in before..self.unifier.deferred_len() {
            self.projections.push((range, "this call".to_string()));
        }
    }

    /// Reports any type the checker finished without working out.
    ///
    /// **`Unknown` is a silence, not a type.** It is compatible with
    /// everything, which is exactly what makes it useful downstream of an error
    /// — one mistake should not become five — and exactly what makes it
    /// invisible when nothing went wrong. Four errata are the same sentence
    /// about different holes (24, 26, 27, 30), and the fifth, entry 40, was
    /// found by the *code generator* three layers away, in a message naming a
    /// variable the author never wrote.
    ///
    /// So: a body the checker finished cleanly must have no `Unknown` left in
    /// it. If one is there, either the program is ambiguous in a way nothing
    /// reported, or the checker has a gap — and both are worth a sentence
    /// where they happened rather than a symptom somewhere else.
    ///
    /// Run **only when the body is otherwise clean**, because after an error
    /// `Unknown` is doing its job and saying so again would bury the real
    /// message. "Clean" means more than this pass being quiet: a name that did
    /// not resolve or a fragment that did not parse leaves an `Unknown` behind
    /// too, and both were already reported — by a different pass, whose errors
    /// are not in this list.
    pub(crate) fn check_unknowns(&mut self) {
        if !self.errors.is_empty() || !self.body.errors.is_empty() {
            return;
        }
        let visited: Vec<ExprId> = self.exprs.keys().copied().collect();
        if visited
            .iter()
            .any(|id| matches!(self.body.expr(*id), Expr::Missing | Expr::Unresolved(_)))
        {
            return;
        }

        let mut found: Vec<TextRange> = Vec::new();
        for id in visited {
            let ty = self.exprs[&id].clone();
            if matches!(self.unifier.zonk(&ty), Type::Unknown) {
                found.push(self.body.range(id));
            }
        }
        // One report, at the *narrowest* expression. They cascade — an
        // expression of unknown type makes the block around it one too — and
        // the smallest range is the innermost, which is where the trail starts.
        found.sort_by_key(|r| (r.len(), r.start()));
        if let Some(range) = found.first().copied() {
            self.error(
                "the type of this expression was never worked out, and nothing else was \
                 reported — so either it needs an annotation, or this is a gap in the \
                 compiler worth reporting"
                    .to_string(),
                range,
            );
        }
    }

    /// Why a field read did not find its field.
    ///
    /// Usually the plain answer, but not always. A type name is written into a
    /// signature whether or not the file imported it — `type_of_syntax` reads
    /// the syntax and does not check that anything answers to the name — while
    /// its *fields* arrive only with the import. So a file annotating
    /// `List<Pair<K, V>>` without importing `Pair` type checked the annotation
    /// and then reported that `Pair` has no field `key`, which is a sentence
    /// about the wrong thing: the fields are not missing, the type is.
    ///
    /// The two halves should agree — an unimported name has no business
    /// resolving in an annotation either — and until they do, this at least
    /// says which of the two went wrong.
    fn why_no_field(&self, owner: &Type, name: &str) -> String {
        if let Type::Adt { name: type_name, .. } = owner {
            if !self.types.adts.contains_key(type_name)
                && !self.types.variants.iter().any(|v| &v.type_name == type_name)
            {
                return format!(
                    "`{type_name}` is not in scope here, so nothing is known about its fields \
                     — add it to an `import`"
                );
            }
        }
        format!("`{owner}` has no field `{name}`")
    }

    /// Whether an assignment's target may be written.
    ///
    /// Lowering already rejects the targets that are wrong on their face — a
    /// literal, a call, a binding that is not `mut`. What is left is a *field*,
    /// and whether that may be written is a question about its record's
    /// declaration, which only the checker has read.
    fn check_writable(&mut self, target: ExprId, range: TextRange) {
        let Expr::Field { base, name } = self.body.expr(target).clone() else { return };
        let owner = self.infer(base);
        let owner = self.unifier.zonk(&owner);
        let Some(variant) = self.types.record_of(&owner) else { return };
        if variant.field(&name).is_none() || variant.is_mut(&name) {
            return;
        }
        self.error(
            format!(
                "cannot assign to `{name}`, which `{}` does not declare `mut`",
                variant.type_name
            ),
            range,
        );
    }

    /// Retries the projections that were waiting on their owner.
    ///
    /// Run after the body, for the same reason `check_effects` is: the fact
    /// that settles `?A` in `extract(Num::spec())` is the call's return type,
    /// and that is not known until the expression it sits in has been
    /// inferred. `docs/design/associated-items.md` decides this (D3).
    pub(crate) fn settle_projections(&mut self) {
        let sites = std::mem::take(&mut self.projections);
        for ((_, why), (range, context)) in self.unifier.settle().into_iter().zip(sites) {
            let Some(why) = why else { continue };
            self.error(format!("{context}: {why}"), range);
        }
    }
}
