//! The salsa queries a caller actually asks for.
//!
//! Everything above is machinery; this is the surface. `checked` runs a file's
//! bodies and caches what they inferred, and the rest read from it — which is
//! what keeps a body edit from invalidating anything but that body.

use super::*;
use crate::check::Checker;

/// Every type the checker worked out for one body.
///
/// The checker computes these on its way to a verdict, and code generation
/// cannot work without them. Publishing them here is what stops a second
/// implementation of the same rules existing downstream and drifting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BodyTypes {
    exprs: HashMap<ExprId, Type>,
    locals: HashMap<LocalId, Type>,
    /// Which instantiation each mention of a generic function chose.
    ///
    /// Recorded here because the checker is the only place that knows: it
    /// created the variables and solved them. Monomorphization reads it to
    /// find out which specializations a body needs.
    instantiations: HashMap<ExprId, (String, Vec<Type>)>,
    /// Bindings a lambda captures because its body uses them *implicitly*.
    ///
    /// A `with` block lowers to a block of `let`s, so a capability is an
    /// ordinary binding and a lambda that uses one captures it like any other.
    /// But nothing in the body *names* it — `report(n)` needs `ledger` without
    /// saying so — and the capture scan watches names. Which labels a call
    /// needs is the callee's row, which only the checker has read, so the
    /// answer is published here rather than guessed at twice.
    lambda_captures: HashMap<ExprId, Vec<khora_hir::body::LocalId>>,
    /// What each call site asked of the function containing it.
    ///
    /// **The checker computes this and used to drop it.** Row subtraction needs
    /// the callee's `with` and `raises` at every call, so the work is already
    /// done; nothing downstream could see the answer. Three things wanted it
    /// and each was going to derive it again: inlay hints, so a reader can see
    /// what a call costs without opening the callee; `khora-lint`'s
    /// `unused_capabilities`, which goes quiet whenever a body contains a call
    /// because it cannot tell which labels were forwarded; and completion
    /// inside a `with { .. }` row.
    ///
    /// Recorded as demanded, before any `with` block or `catch` discharges it —
    /// what the *call* needs, rather than what survives to the signature.
    call_rows: HashMap<ExprId, CallRows>,
}

/// The effect rows one call site asked for.
///
/// `None` means the row was empty, which is the ordinary case and is why this
/// is two options rather than two rows: a call that needs nothing and cannot
/// fail should render as nothing at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallRows {
    /// Capabilities the callee requires that this call still has to answer.
    ///
    /// Empty where an enclosing `with` block already supplied them, which is
    /// the right answer for checking and the wrong one for showing: at exactly
    /// the call that discharged a capability, this says nothing about it.
    pub requires: Option<Type>,
    /// Errors it may raise.
    pub raises: Option<Type>,
    /// Capabilities the callee's signature asks for, before anything in scope
    /// answered them.
    ///
    /// **The difference between the two is what the enclosing code absorbs**,
    /// which is the one thing a Khora signature never says. A row is
    /// transitive, so a function's `with` clause tells a reader everything it
    /// passes on and nothing about what it takes on itself. Subtracting
    /// `requires` from this gives the rest.
    pub declared: Option<Type>,
}

impl BodyTypes {
    /// The type of an expression. `Unknown` for anything the checker could not
    /// determine, which is also what an id it never visited reports.
    pub fn of(&self, id: ExprId) -> &Type {
        self.exprs.get(&id).unwrap_or(&Type::Unknown)
    }

    /// What the call at `id` asked for, if it asked for anything.
    pub fn call_rows(&self, id: ExprId) -> Option<&CallRows> {
        self.call_rows.get(&id)
    }

    /// Every call site that asked for something, in no particular order.
    pub fn calls_with_rows(&self) -> impl Iterator<Item = (ExprId, &CallRows)> {
        self.call_rows.iter().map(|(id, rows)| (*id, rows))
    }

    pub fn local(&self, id: LocalId) -> &Type {
        self.locals.get(&id).unwrap_or(&Type::Unknown)
    }

    /// The generic function this expression mentions, and at what arguments.
    pub fn instantiation(&self, id: ExprId) -> Option<&(String, Vec<Type>)> {
        self.instantiations.get(&id)
    }

    /// Bindings this lambda captures implicitly. See the field.
    pub fn implicit_captures(&self, id: ExprId) -> &[khora_hir::body::LocalId] {
        self.lambda_captures.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn instantiations(&self) -> impl Iterator<Item = (&ExprId, &(String, Vec<Type>))> {
        self.instantiations.iter()
    }

    /// This body's types with `mapping` applied, which is one specialization.
    pub fn specialized(&self, mapping: &HashMap<&str, Type>) -> BodyTypes {
        BodyTypes {
            exprs: self
                .exprs
                .iter()
                .map(|(k, v)| (*k, unify::substitute(v, mapping)))
                .collect(),
            locals: self
                .locals
                .iter()
                .map(|(k, v)| (*k, unify::substitute(v, mapping)))
                .collect(),
            instantiations: self
                .instantiations
                .iter()
                .map(|(k, (name, args))| {
                    let args = args.iter().map(|a| unify::substitute(a, mapping)).collect();
                    (*k, (name.clone(), args))
                })
                .collect(),
            // Bindings, not types: a specialization captures the same ones the
            // generic body does, and there is nothing in a `LocalId` to
            // substitute.
            lambda_captures: self.lambda_captures.clone(),
            call_rows: self.call_rows.clone(),
        }
    }
}

/// The result of checking one file: the verdict, and the working.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Checked {
    pub errors: Vec<HirError>,
    /// Per function, in declaration order.
    pub bodies: Vec<(String, BodyTypes)>,
}

/// Checks a file, keeping both the diagnostics and the types.
///
/// One query rather than two so the work is done once; the accessors below are
/// what callers normally want.
#[salsa::tracked(returns(ref))]
pub fn checked(db: &dyn Db, file: SourceFile) -> Checked {
    let types = type_map(db, file);
    let mut out = Checked::default();
    // Derived bodies whose `derive` was already refused. They are still
    // inferred — code generation needs the types, and an unpredicted failure
    // in one is a bug in the expander that has to stay visible — but what they
    // have to say is dropped: `derive_report` has said it at the same place,
    // naming the field instead of the expression the field turned into.
    let refused = &derive::derive_report(db, file).refused;
    // A file that did not parse has holes in it, and a hole is an `Unknown`
    // that nothing in *this* pass reported. The syntax error is the message
    // worth reading; see `Checker::check_unknowns`.
    let parsed = khora_db::parse(db, file).errors().is_empty();

    for (name, body) in khora_hir::body::bodies(db, file) {
        let mut signature = types.signatures.get(name).cloned().unwrap_or(Signature {
            is_extern: false,
            generics: Vec::new(),
            bounds: Vec::new(),
            requires: Type::empty_row(),
            raises: Type::empty_row(),
            params: Vec::new(),
            ret: Type::Unknown,
        });
        let mut unifier = Unifier::new().with_assoc(types.traits.assoc_bindings());
        // A test's error row is open: an error escaping a test is a *failing
        // test*, not a program that does not compile. Opened here rather than
        // in the signature because only a unifier can make a flexible tail,
        // and a rigid one would reject the very thing this is for.
        if name.starts_with(khora_hir::TEST_PREFIX) {
            signature.raises = Type::row(Vec::new(), Some(unifier.fresh()));
        }
        let mut checker = Checker {
            types,
            body,
            signature: &signature,
            locals: HashMap::new(),
            exprs: HashMap::new(),
            instantiations: HashMap::new(),
            unifier,
            lambdas: Vec::new(),
            demanded: Vec::new(),
            projections: Vec::new(),
            coverage: Vec::new(),
            enclosing_lambdas: Vec::new(),
            lambda_captures: HashMap::new(),
            call_rows: HashMap::new(),
            installed: Vec::new(),
            loops: Vec::new(),
            open_raises: Vec::new(),
            open_requires: Vec::new(),
            hint: None,
            marked: Vec::new(),
            errors: Vec::new(),
        };
        checker.check_function();
        checker.close_open_rows();
        checker.check_bounds();
        checker.settle_projections();
        // After the projections, so that a scrutinee whose type came through
        // one is settled too.
        checker.settle_coverage();
        checker.check_effects();
        if parsed {
            checker.check_unknowns();
        }
        let reported = std::mem::take(&mut checker.errors);
        if !refused.contains(name) {
            out.errors.extend(reported);
        }
        // Published types are zonked: a consumer should never see a variable,
        // and code generation cannot do anything with one.
        let exprs = checker.exprs.iter().map(|(k, v)| (*k, checker.unifier.zonk(v))).collect();
        let locals = checker.locals.iter().map(|(k, v)| (*k, checker.unifier.zonk(v))).collect();
        let instantiations = checker
            .instantiations
            .iter()
            .map(|(k, (n, args))| {
                let args = args.iter().map(|a| checker.unifier.zonk(a)).collect();
                (*k, (n.clone(), args))
            })
            .collect();
        let lambda_captures = std::mem::take(&mut checker.lambda_captures);
        let call_rows = std::mem::take(&mut checker.call_rows);
        out.bodies.push((
            name.clone(),
            BodyTypes { exprs, locals, instantiations, lambda_captures, call_rows },
        ));
    }
    out
}

/// The type of every expression and binding, per function.
pub fn body_types(db: &dyn Db, file: SourceFile) -> &Vec<(String, BodyTypes)> {
    &checked(db, file).bodies
}

/// Type errors for one file, and nothing else.
///
/// Kept separate from lowering errors so "does this type-check" stays a
/// question with its own answer; [`diagnostics`] is what a driver wants.
pub fn check_file(db: &dyn Db, file: SourceFile) -> &Vec<HirError> {
    &checked(db, file).errors
}

/// Everything wrong with the traits and impls a file declares.
///
/// Separate from `check_file` because none of it depends on a function body:
/// an impl is well-formed or it is not, whatever any caller does with it.
#[salsa::tracked(returns(ref))]
pub fn trait_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let types = type_map(db, file);
    traits::check(
        &types.traits,
        &types.kinds,
        &types.signatures,
        &|ty| types.may_vouch_for(ty),
        &|ty| types.declares(ty),
    )
}
