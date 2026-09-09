//! The type checker: one `Checker` per function body.
//!
//! Hindley-Milner with row unification, and the state below is what one body
//! needs — the substitution, what each expression and local resolved to, the
//! demands still owed, and the diagnostics. `unify` does the solving.
//!
//! Split across the modules named here, one per cluster of methods: inferring
//! an expression form, resolving a call, moving a row, deciding what may cross
//! a fiber, instantiating a parameter, taking a pattern apart. Rust lets an
//! inherent impl live in several modules of one crate, so each opens
//! `impl<'a> Checker<'a>` again.

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

    /// Whether an entry of this row could have come from a well-formed clause.
    ///
    /// **A `with` entry is labelled by the programmer and a `raises` entry is
    /// labelled after its own type**, so only the first can be malformed --
    /// and it is malformed exactly when the label is not an identifier.
    ///
    /// Not when the label is merely *unusual*: `with Ledger` comes out as an
    /// entry labelled `Ledger` of type `Ledger`, and `with { Ledger: handler }`
    /// supplies it. That is writable, so it is a style to dislike rather than
    /// an error to report. A label carrying `::` or braces is not writable by
    /// anybody, which is the line.
    ///
    /// `row_of_syntax` shares one fallback arm between the two clauses, and
    /// that arm labels an entry after its own type. For `raises DbError` that
    /// is the intended reading. For `with Self::Effects` it is not: the entry
    /// comes out labelled `Self::Effects` of type `Self::Effects`, which no
    /// handler can supply because no handler can be given that name. The
    /// message said `needs `{ tick: Tick }: { tick: Tick }`` and sent the
    /// reader looking for a capability instead of at their clause.
    fn label_is_well_formed(self, label: &str) -> bool {
        match self {
            Clause::Raises => true,
            Clause::Requires => {
                let mut chars = label.chars();
                chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
                    && chars.all(|c| c.is_alphanumeric() || c == '_')
            }
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
    /// Requirements flow *upward*: a function calling something that needs
    /// `ledger` needs `ledger` too, unless a `with` block supplies it. Rows are
    /// checked against the declaration rather than inferred into it, because an
    /// exported signature is a promise and inferring one would let a body widen
    /// it silently. `docs/design/effects.md`.
    pub(crate) demanded: Vec<Demand>,
    /// Where each deferred projection was written, in the order the unifier
    /// deferred them, so `settle_projections` can report against the source.
    pub(crate) projections: Vec<(TextRange, String)>,
    /// Every `match` whose coverage is still to be checked.
    ///
    /// **Collected during inference and checked afterwards**, because whether
    /// a `match` is exhaustive is a question about the scrutinee's *settled*
    /// type and inference has not settled it yet. See [`Self::settle_coverage`].
    pub(crate) coverage: Vec<(Type, Vec<khora_hir::body::MatchArm>, TextRange)>,
    /// The patterns [`Checker::bind_pattern`] has already reported on.
    ///
    /// A pattern that does not fit its scrutinee binds `Unknown` and then
    /// describes a shape the scrutinee's type does not have, so coverage —
    /// which asks what shapes are left over — answers about that invented
    /// shape. `for (k, v) in Dict::entries(d)` was told the truth once and the
    /// desugaring's own business afterwards:
    ///
    /// ```text
    /// error: this pattern takes a value apart into 2 pieces, but
    ///        `Pair<String, Int>` is not a tuple
    /// error: this `match` is not exhaustive: pattern `Yield(_, Pair(_, _))`
    ///        not covered
    /// ```
    ///
    /// naming a `match` the source does not contain and a `Yield` from
    /// `lower_for`'s expansion. The second error is the first one seen from
    /// the other end, so the first one suppresses it.
    pub(crate) broken_pats: HashSet<PatId>,
    /// The lambdas currently being inferred, innermost last, each with the
    /// bindings it has been found to use implicitly.
    pub(crate) enclosing_lambdas: Vec<(ExprId, Vec<khora_hir::body::LocalId>)>,
    /// The finished answer, moved out as each lambda closes.
    pub(crate) lambda_captures: HashMap<ExprId, Vec<khora_hir::body::LocalId>>,
    /// What each call site asked for, published as [`crate::CallRows`].
    ///
    /// Filled where the demand is raised rather than reconstructed afterwards,
    /// because afterwards the row has been through subtraction and no longer
    /// says what the *call* wanted.
    pub(crate) call_rows: HashMap<ExprId, crate::CallRows>,
    /// The capabilities in scope from enclosing `with` blocks.
    ///
    /// A call inside one is served by it, so its labels never reach the
    /// signature. That is row subtraction: `with` *discharges* a requirement
    /// rather than forwarding it.
    pub(crate) installed: Vec<String>,
    /// The loops currently being inferred, innermost last.
    ///
    /// Each holds the type its `break`s agree on, whether any `break` carried a
    /// value, and whether there was a `break` at all. Two `break`s carrying
    /// different types is a mismatch reported at the second.
    ///
    /// **Three outcomes, not two.** A `break` with a value gives what the
    /// breaks carry; a `break` without one gives `()`; and *no `break`* gives
    /// `Never`, because a loop with no way out does not finish and so has no
    /// value to be of any type. The last was `()` here, which made
    /// `fn f() -> Int { loop { .. } }` a type error against a body that cannot
    /// return at all -- the same mistake #127 fixed for a diverging branch,
    /// left behind in the one construct whose whole purpose is not to end.
    pub(crate) loops: Vec<(Type, bool, bool)>,
    /// The open tail of every lambda's inferred `raises` row, in the order the
    /// lambdas were seen.
    ///
    /// A lambda's error row is a **lower bound**: the body raises at least
    /// these, and the context may ask it to be declared as raising more. That
    /// is what makes a mock that never fails satisfy `raises IoError`.
    ///
    /// The tail is a variable, filled in by whatever the lambda is checked
    /// against. One still unsolved when the body is done was never asked for
    /// anything and defaults to closed-empty — leaving it open makes the row
    /// fallible to the code generator, and every lambda returns a tagged pair
    /// for nothing.
    pub(crate) open_raises: Vec<Type>,
    /// The same, for what a lambda *requires*.
    ///
    /// **A capability offered to a closure is not a capability it has to
    /// use.** `nursery(fn () => 1)` was refused with ``nursery: Nursery is
    /// required here but not provided``, which is exactly backwards: the
    /// nursery is being provided and the body simply does not want it. The row
    /// on the body came out closed, so it could not absorb a label nobody
    /// asked for.
    ///
    /// Left open for the same reason the error row is, and closed the same way
    /// by [`Checker::close_open_rows`]: what a body needs is a *lower* bound,
    /// and a tail nothing ever widened is empty.
    pub(crate) open_requires: Vec<Type>,
    /// The type the surrounding expression is asking for, when there is one.
    ///
    /// Three things read it. An integer literal, to decide which integer it
    /// is: `let b: U8 = 65` has to work, and 65 alone is an `Int`. A record
    /// literal, to decide which record it is when two share its labels. A
    /// lambda, to learn its parameter and result types before its body is
    /// inferred. A *hint*, not a demand — `require` still runs afterwards, so
    /// a wrong one changes which error is reported and never whether one is.
    ///
    /// Consumed by the first `infer` that sees it, and re-armed only where a
    /// type flows through unchanged: the branches of an `if`, the tail of a
    /// block, the arms of a `match`, and the root of a function body, which
    /// is what the declared return type describes. Anywhere else it leaks
    /// into a subexpression that means something different — the `0` in
    /// `array[0]` is an index, whatever the result is being used as.
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
        let expected = self.signature.ret.clone();
        // **The declared return type is what the body is for**, so it is the
        // hint for the root the way an annotation is the hint for a `let`.
        // Without it `fn f() -> U8 { 200 }` was refused, because the literal
        // decided it was an `Int` before anything mentioned `U8`, and a record
        // literal in tail position had to be found by its labels even though
        // the signature had already said which record it was.
        self.hint = Some(self.unifier.zonk(&expected));
        let actual = self.infer(root);
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
            // Too wide for even an i128, so certainly too wide for this.
            // [`Self::check_int_literal`] is the `Int` path's version.
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

    /// Whether a literal with no fixed-width hint fits in an `Int`.
    ///
    /// `Int` is 64 bits, so this is `i64::from_str`. Written separately from
    /// [`Self::check_literal_fits`] because the fixed-width one has a range to
    /// name and this one does not: every `Int` has the same bounds, and
    /// printing them is what tells a reader whether their number is close or
    /// absurd.
    ///
    /// The code generator keeps its own copy, which is now an assertion about
    /// what reaches it rather than the only place the rule lives.
    fn check_int_literal(&mut self, text: &str, range: TextRange) {
        let cleaned = text.replace('_', "");
        if cleaned.parse::<i64>().is_err() {
            self.error(
                format!(
                    "`{text}` does not fit in an `Int`, which holds {} to {}",
                    i64::MIN,
                    i64::MAX
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
    /// The deferred-projection bookkeeping still happens: `settle_projections`
    /// pairs the unifier's deferred list with `self.projections` by position, so
    /// leaving one out slides every later diagnostic onto the wrong range.
    fn hint_at(&mut self, expected: &Type, found: &Type, range: TextRange) {
        let before = self.unifier.deferred_len();
        let _ = self.unifier.unify(expected, found);
        for _ in before..self.unifier.deferred_len() {
            self.projections.push((range, "this call".to_string()));
        }
    }

    /// Reports any type the checker finished without working out.
    ///
    /// **`Unknown` is a silence, not a type.** Being compatible with everything
    /// is what makes it useful downstream of an error — one mistake should not
    /// become five — and what makes it invisible when nothing went wrong.
    /// Errata 24, 26, 27, 30 and 40 are the same sentence about different
    /// holes, the last found by the *code generator* three layers away.
    ///
    /// So a body the checker finished cleanly must have no `Unknown` left in
    /// it. One that is there means either an ambiguity nothing reported or a
    /// gap in the checker, and both are worth a sentence where they happened.
    ///
    /// Run **only when the body is otherwise clean**: after an error `Unknown`
    /// is doing its job. "Clean" means more than this pass being quiet — an
    /// unresolved name or an unparsed fragment leaves one behind too, and those
    /// were reported by a different pass whose errors are not in this list.
    /// Refuses a mention of a generic whose type arguments nothing decided.
    ///
    /// `decode(Raw::Absent)` names `decode<A: Decode>` and nothing in the
    /// program says which `A`. Every use of `A` is behind the bound, so
    /// unification has nothing to work from and leaves the variable free --
    /// which is not an `Unknown` and so was invisible to every check here.
    ///
    /// Monomorphization then has to pick an impl for a type it does not have,
    /// and picked the trait's own method, whose body is the declaration.
    ///
    /// **A `Param` is not undetermined.** Inside a generic function an
    /// instantiation at that function's own parameter is exactly right: `A` is
    /// decided by whoever calls it. Only a unification variable that survived
    /// the body is a question nobody answered.
    fn refuse_undetermined_instantiations(&mut self) {
        let mut blamed: Vec<(TextRange, String)> = Vec::new();
        for (id, (name, args)) in &self.instantiations {
            // **Only a *bounded* parameter, because only a bound dispatches.**
            // `Router::bound<'ef>` declares a row nothing in its signature
            // mentions, so its argument is undetermined for ever and harmless:
            // an unconstrained row is the empty one, and no code is chosen by
            // it. A parameter with a bound is the opposite -- the bound is how
            // the body reaches an impl, and not knowing the type means not
            // having one.
            let Some(signature) = self.types.signatures.get(name.as_str()) else { continue };
            let bounded = args.iter().enumerate().any(|(at, arg)| {
                signature.bounds.get(at).is_some_and(|traits| !traits.is_empty())
                    && undetermined(&self.unifier.zonk(arg))
            });
            if !bounded {
                continue;
            }
            blamed.push((self.body.range(*id), name.clone()));
        }
        // Narrowest first, and then by position, so one program reports the
        // same way twice: `instantiations` is a hash map and its order is not
        // the program's. Errata 33 is the same argument about a different map.
        blamed.sort_by_key(|(range, name)| (range.len(), range.start(), name.clone()));
        let Some((range, name)) = blamed.first().cloned() else { return };
        self.error(self.why_undetermined(&name), range);
    }

    /// Why nothing decided a bounded type argument — which is two situations
    /// with two different fixes, and one of them cannot be fixed at the call
    /// at all.
    ///
    /// Usually the call is under-annotated, and saying the type fixes it. But
    /// a *trait method that never mentions `Self`* can never be decided from
    /// outside: no argument carries it, no result carries it, and an
    /// annotation has nothing to attach to. That is what a receiver renamed
    /// from `self` to `_self` produces — the trait still compiles, the impls
    /// still compile, and every call site in files nobody touched reports
    /// this. `unused-binding` used to suggest exactly that rename and does not
    /// any more; a hand-written `_self` still reaches here, and now hears what
    /// is wrong instead of being told to annotate, which would not have helped.
    fn why_undetermined(&self, name: &str) -> String {
        let declared = name.split_once("::").and_then(|(owner, method)| {
            self.types.traits.traits.get(owner).and_then(|def| def.method(method))
        });
        if let Some(method) = declared {
            let uses_self = method
                .signature
                .params
                .iter()
                .chain(std::iter::once(&method.signature.ret))
                .any(|ty| crate::traits::mentions_param(ty, "Self"));
            if !uses_self {
                return format!(
                    "`{name}` never mentions `Self`, so no call can say which impl to use \
                     and no annotation can either. A method's first parameter has to be \
                     `self`; anything else — `_self` included — is an ordinary parameter, \
                     and it takes the method away"
                );
            }
        }
        format!(
            "nothing here decides what type `{name}` is used at, and its bound is \
             the only thing that would -- so there is no impl to call. Annotate it: \
             `let value: TheType = {name}(..)`, or say it at the call"
        )
    }

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

        // **A generic call whose type argument nothing decided.** This is not
        // an `Unknown` -- inference made a variable for it and simply never
        // solved it -- so the walk below cannot see it, and it used to reach
        // the backend, which picked the trait's own bodyless method and said
        // ``Decode::schema` has no body` against the blank line after the end
        // of the program.
        //
        // That is the check/build split this repository has closed twice: an
        // ambiguity is a type error, and a type error belongs to `khora check`.
        // Reported before the walk because it is the cause; an undetermined
        // argument usually leaves nothing else to see.
        self.refuse_undetermined_instantiations();
        if !self.errors.is_empty() {
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
        let Some(range) = found.first().copied() else { return };

        // The const is looked for across *every* unknown expression rather
        // than only the narrowest, because it is usually not the narrowest.
        // `with { clock: fixed_clock }` binds a local from the constant, and
        // the shortest range with no type is the later use of `clock` — a
        // symptom two lines below the cause. Reporting the cause is the whole
        // point of the special case.
        if let Some((at, name)) = found.iter().find_map(|r| self.const_at(*r).map(|n| (*r, n))) {

        // **A `const` from another module is a known gap, not a mystery.** A
        // constant's type comes from inference over its initializer, and the
        // type map is built from syntax before any inference runs — so nothing
        // records what an exported `const` is, and a file that imports one
        // finds a name with no type behind it.
        //
        // The generic message below ends "this is a gap in the compiler worth
        // reporting", which for this case is both true and useless: it *is* a
        // gap, it is a known one, and sending somebody to write it up costs
        // them an hour and tells nobody anything. The cookbook shows
        // `const fixed_clock = handler for Clock { .. }` as the way to write a
        // test double, so this is met by people following the documentation.
            self.error(
                format!(
                    "`{name}` is a `const`, and nothing worked out its type. A constant \
                     declared in *another* module is the usual cause: its type comes from \
                     inferring over its initializer, and the type map is built from syntax \
                     before anything is inferred, so nothing records what an exported one \
                     is. Move it into this file, or wrap it in a function — \
                     `pub fn {name}() -> ..` has a signature, and a signature is what \
                     travels"
                ),
                at,
            );
            return;
        }

        self.error(
            "the type of this expression was never worked out, and nothing else was \
             reported — so either it needs an annotation, or this is a gap in the \
             compiler worth reporting"
                .to_string(),
            range,
        );
    }

    /// The name of the `const` at `range`, if that is what is there.
    ///
    /// By range rather than by id because that is what [`Self::check_unknowns`]
    /// has left by the time it reports: the ids were consumed picking the
    /// narrowest one.
    ///
    /// No check that the constant is from another module, because the checker
    /// does not know which module it is in — and it does not need to. One
    /// declared *here* is typed by the ordinary body pass and never reaches
    /// this point with `Unknown`, so anything that does is either the
    /// cross-module case or an initializer nothing could work out. The message
    /// names the first as the usual cause and is true of both.
    fn const_at(&self, range: TextRange) -> Option<String> {
        self.body.exprs().find_map(|(id, expr)| {
            if self.body.range(id) != range {
                return None;
            }
            match expr {
                khora_hir::body::Expr::Path(khora_hir::Resolution::Item {
                    name,
                    kind: khora_hir::ItemKind::Const,
                    ..
                }) => Some(name.clone()),
                _ => None,
            }
        })
    }

    /// Why a field read did not find its field.
    ///
    /// Usually the plain answer, but not always. `type_of_syntax` reads a name
    /// out of a signature without checking that anything answers to it, while
    /// the type's *fields* arrive only with the import. So a file annotating
    /// `List<Pair<K, V>>` without importing `Pair` checks the annotation and
    /// then reports that `Pair` has no field `key` — a sentence about the wrong
    /// thing, since the fields are not missing, the type is.
    ///
    /// The two halves should agree, and until they do this at least says which
    /// of them went wrong.
    fn why_no_field(&self, owner: &Type, name: &str) -> String {
        if let Some(type_name) = self.undeclared_adt(owner) {
            return format!(
                "`{type_name}` is not in scope here, so nothing is known about its fields \
                 — add it to an `import`"
            );
        }
        format!("`{owner}` has no field `{name}`")
    }

    /// The first name in `ty` that a signature mentions and this module never
    /// imported, if there is one.
    ///
    /// The same split [`Self::why_no_field`] describes: `type_of_syntax` reads
    /// a name out of a signature whether or not anything here answers to it,
    /// while the *declaration* — fields, impls, everything else that is known
    /// about it — arrives only with the import. Every question asked of such a
    /// type gets the answer "no", and every one of those answers is a sentence
    /// about the wrong thing.
    ///
    /// The head only, deliberately. An argument that is not in scope is the
    /// caller's business when the container's impl needs it, and asking about
    /// one here would name a type the message is not about.
    pub(crate) fn undeclared_adt<'t>(&self, ty: &'t Type) -> Option<&'t str> {
        let Type::Adt { name, .. } = ty else { return None };
        (!self.types.adts.contains_key(name)
            && !self.types.variants.iter().any(|v| &v.type_name == name))
        .then_some(name.as_str())
    }

    /// Whether these two are one name, one of which did not resolve.
    ///
    /// [`Type::Adt`]'s `home` is `None` for a name nothing answered to, and
    /// that failure is reported where it happened. When such a phantom is
    /// then compared against the real declaration it stands in for, both
    /// sides print the same word, the mismatch qualifies them to tell them
    /// apart, and the reader is shown a type disagreeing with itself.
    pub(crate) fn is_phantom_of(&self, left: &Type, right: &Type) -> bool {
        let (
            Type::Adt { name: left_name, home: left_home, .. },
            Type::Adt { name: right_name, home: right_home, .. },
        ) = (left, right)
        else {
            return false;
        };
        left_name == right_name && (left_home.is_none() || right_home.is_none())
    }

    /// Whether `ty` mentions anywhere a name that did not resolve.
    ///
    /// [`Type::Adt`]'s `home` is `None` for exactly that, and its own note
    /// says the failure is "an error already reported" — so a caller here is
    /// deciding whether to stay quiet, not whether to look further.
    ///
    /// **Every shape, not just the head.** The phantom is usually somewhere
    /// inside: `raises EnvError` without the import puts it in a row, and the
    /// row is what the call site compares against.
    pub(crate) fn unresolved_adt(&self, ty: &Type) -> bool {
        match ty {
            Type::Adt { home, args, .. } => {
                home.is_none() || args.iter().any(|arg| self.unresolved_adt(arg))
            }
            Type::Row { fields, tail } => {
                fields.iter().any(|(_, t)| self.unresolved_adt(t))
                    || tail.as_deref().is_some_and(|t| self.unresolved_adt(t))
            }
            Type::Tuple(items) => items.iter().any(|t| self.unresolved_adt(t)),
            Type::Applied { head, args } => {
                self.unresolved_adt(head) || args.iter().any(|t| self.unresolved_adt(t))
            }
            Type::Fn { params, ret, requires, raises } => {
                params.iter().any(|t| self.unresolved_adt(t))
                    || self.unresolved_adt(ret)
                    || self.unresolved_adt(requires)
                    || self.unresolved_adt(raises)
            }
            Type::Assoc { owner, .. } => self.unresolved_adt(owner),
            _ => false,
        }
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

    /// Checks every `match` for coverage, now that the types are settled.
    ///
    /// **Run after the body rather than during it**, and that ordering is the
    /// whole of the fix. Exhaustiveness is a question about the scrutinee's
    /// type: to know that `Err(NotFound(id))` covers every `Err`, the checker
    /// has to know the error type is `UserError` and that `NotFound` is its
    /// only case. Asked mid-inference, the answer was `Result<String, ?12>` --
    /// an unsolved variable has no constructors, so the arm covered part of
    /// `Err`'s space and the rest was reported missing.
    ///
    /// That made the idiom `testing.md` teaches fail to compile:
    ///
    /// ```khora
    /// let result = attempt(fn () => load_user(999)!);
    /// match result {
    ///   Result::Ok(_) => assert(false),
    ///   Result::Err(UserError::NotFound(id)) => assert(id == 999),
    /// }
    /// ```
    ///
    /// `pattern Err(_) not covered`, for a type with one variant. The error
    /// row reaches `?12` through `attempt`'s signature and a lambda's
    /// `raises`, which is a deferred constraint -- so it was still a variable
    /// at the `match` and was `UserError` a few lines later. Annotating the
    /// `let` made it compile, which is what told everybody it was a bug rather
    /// than a rule.
    ///
    /// After `settle_projections`, for the same reason that one runs after the
    /// body: `?A` in `extract(Num::spec())` is settled by the call it sits in.
    pub(crate) fn settle_coverage(&mut self) {
        for (scrutinee, arms, range) in std::mem::take(&mut self.coverage) {
            let settled = self.unifier.zonk(&scrutinee);
            self.report_match_coverage(&settled, &arms, range);
        }
    }
}

/// Whether a type still holds a unification variable nothing solved.
///
/// Distinct from `settled`: a `Param` is settled from this angle -- it is a
/// type somebody else chooses, which is an answer -- while a `Var` that
/// survived the body is a question that was never put to anybody.
fn undetermined(ty: &Type) -> bool {
    match ty {
        Type::Var(_) | Type::Unknown => true,
        Type::Adt { args, .. } | Type::Applied { args, .. } => args.iter().any(undetermined),
        Type::Tuple(items) => items.iter().any(undetermined),
        Type::Fn { params, ret, .. } => params.iter().any(undetermined) || undetermined(ret),
        _ => false,
    }
}
