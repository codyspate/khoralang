//! What may cross into another fiber.
//!
//! The rule is one property checked in one place — the captures of the closure
//! handed to `spawn` — because there are no references in Khora and so nothing
//! else escapes. `docs/design/sharing.md` argues why that is enough, and why
//! the `Share` assertion is restricted to the module that declares the type.

use super::*;

impl<'a> Checker<'a> {
    /// What a fiber's body may close over.
    ///
    /// A mutable value handed to another fiber is a data race, and this is the
    /// only place one can cross: a fiber touches exactly what its thunk
    /// captured. `docs/design/memory.md` §5a.
    ///
    /// So the thunk has to be one whose captures are visible here: a lambda
    /// written at the call, or a named function, which captures nothing.
    /// Anything else is refused, because a check that cannot see what it is
    /// checking is not a check — and the rule is worth having anyway, since
    /// **a fiber's body is written where it starts**.
    pub(super) fn check_spawnable(&mut self, args: &[ExprId], range: TextRange) {
        let Some(body) = args.first().copied() else { return };
        self.check_raises_stay_home(body, range);
        let captures: Vec<khora_hir::body::LocalId> = match self.body.expr(body) {
            Expr::Lambda { captures, .. } => captures
                .iter()
                .copied()
                .chain(self.lambda_captures.get(&body).into_iter().flatten().copied())
                .collect(),
            // A named function captures nothing. Its own `with` clause is
            // checked at the call like any other.
            Expr::Path(_) => return,
            _ => {
                self.error(
                    "this has to be a closure written here or a named function, so that \
                     what it closes over can be checked — a closure that arrived under a \
                     name captured whatever it captured somewhere else"
                        .to_string(),
                    range,
                );
                return;
            }
        };

        for local in captures {
            let ty = self.unifier.zonk(self.locals.get(&local).unwrap_or(&Type::Unknown));
            if self.types.is_shareable(&ty, &self.shared_params()) {
                if holds_a_variable(&ty) {
                    self.unsettled_captures.push((local, Captor::Spawn, range));
                }
                continue;
            }
            self.refuse_capture(local, &ty, range);
        }
    }

    /// Asks again about each capture whose type still held a variable at its
    /// spawn, now that the body's types are settled.
    ///
    /// **What this prevents: a `mut` record or a region handed to a fiber
    /// through a type solved after the spawn.** In
    /// `let go = fn x => Fiber::spawn(fn () => { let _k = x; 3 }); go(h)`
    /// the capture `x` is a variable at the spawn, a variable answers
    /// "shareable", and `go(h)` solves it only afterwards. The child then
    /// held the parent's `h` with nothing said: a race on its fields, or for
    /// a region, a finalizer run on the child, which only the runtime's
    /// release check stopped.
    ///
    /// Run where `check_bounds` is, and for the same reason: a question about
    /// a solved type, asked once everything that solves it has run. Only the
    /// captures that were unsettled are asked, so a capture refused at the
    /// spawn is not reported twice. One still unsolved here is shareable,
    /// because no value of it exists: nothing ever pinned it, so nothing was
    /// ever passed in.
    ///
    /// A handler's captures are asked again the same way, for the same
    /// reason: a handler is shareable only because its captures were checked
    /// where it is written, and a variable there answered "shareable".
    pub(crate) fn check_unsettled_captures(&mut self) {
        for (local, captor, range) in std::mem::take(&mut self.unsettled_captures) {
            let ty = self.unifier.zonk(self.locals.get(&local).unwrap_or(&Type::Unknown));
            if self.types.is_shareable(&ty, &self.shared_params()) {
                continue;
            }
            match captor {
                Captor::Spawn => self.refuse_capture(local, &ty, range),
                Captor::Handler { owner, label } => {
                    self.refuse_handler_capture(&owner, &label, local, &ty, range)
                }
            }
        }
    }

    fn refuse_capture(&mut self, local: khora_hir::body::LocalId, ty: &Type, range: TextRange) {
        let name = self.body.local(local).name.clone();
        let why = self.types.why_unshareable(ty);
        self.error(format!("`{name}` cannot be handed to another fiber: {why}"), range);
    }

    /// A fiber's error may not hold a `Region` or `Scope`.
    ///
    /// **What this prevents: a child's finalizer run on its parent.** An error
    /// a fiber raises is caught by whoever joins it, so a region inside one
    /// reaches that fiber, and its last reference -- and with it the child's
    /// finalizers, over captures the child may have been writing -- goes
    /// there. The answer is asked for `Share` by `Fiber`'s own bound; the
    /// error row is not, and this closes the region half of that gap only.
    ///
    /// A `mut` record in an error is marked at the handover and is not this
    /// rule's business, so asking the whole row for `Share` would refuse
    /// programs that are correct.
    ///
    /// Asked of the body's type as inference has it at the spawn, so a raises
    /// row still open there is not seen: the same limit the capture check has.
    fn check_raises_stay_home(&mut self, body: ExprId, range: TextRange) {
        let Some(ty) = self.exprs.get(&body).map(|t| self.unifier.zonk(t)) else { return };
        let Type::Fn { raises, .. } = ty else { return };
        let Type::Row { fields, .. } = *raises else { return };
        for (label, error) in fields {
            if let Some(fiber_bound) = self.types.fiber_bound_inside(&error) {
                let why = crate::map::stays_on_its_fiber_because(&fiber_bound);
                self.error(
                    format!(
                        "`{label}`, which this fiber can raise, cannot be handed to another \
                         fiber: {why}"
                    ),
                    range,
                );
            }
        }
    }

    /// Every operation of a handler must be safe to hand to another fiber.
    ///
    /// **This is what buys an effect its shareability.** A capability has to be
    /// able to cross into a fiber, and an effect is a record of closures that
    /// nothing at the type level can see inside — so the question is asked
    /// here, at the one place a handler comes into existence and its lambdas
    /// are written. Answered once where it is answerable, rather than at every
    /// spawn where it is not. `docs/design/sharing.md`.
    ///
    /// The cost is real: a handler may not capture something writable, so a
    /// test double counting its calls in a `mut` field is refused. The error
    /// says which binding and why.
    pub(super) fn check_handler_is_shareable(&mut self, owner: &str, fields: &[(String, ExprId)]) {
        // **A `Scope` handler never crosses**, so what it captures need not
        // either: it captures the region it defers into, which is exactly
        // what must stay on this fiber. `scoped` and `Scope::root` are both
        // this shape. `crate::REGION_TYPE`.
        if crate::stays_on_its_fiber(owner) {
            return;
        }
        for (label, value) in fields {
            let range = self.body.range(*value);
            // **The closure has to be written here.** A binding holding one
            // was written somewhere else, and its captures went with it:
            //
            // ```
            // let leak = fn () => bump(tally);
            // let h = handler for Counting { tick: leak };
            // ```
            //
            // Nothing at this line can see what `leak` took, so accepting it
            // lets any closure through by naming it first — and the exception
            // that makes an effect shareable rests on this check.
            if !matches!(self.body.expr(*value), Expr::Lambda { .. } | Expr::Path(_)) {
                self.error(
                    format!(
                        "`{owner}`'s `{label}` has to be a closure written here or a named \
                         function: a handler is safe to hand to another fiber only because \
                         what its operations captured is checked at this line, and a \
                         closure that arrived under a name captured it somewhere else"
                    ),
                    range,
                );
                continue;
            }
            for local in self.captures_of(*value) {
                let ty = self.unifier.zonk(self.locals.get(&local).unwrap_or(&Type::Unknown));
                if self.types.is_shareable(&ty, &self.shared_params()) {
                    // A variable answers "shareable" here and may be solved
                    // to a `mut` record by a later call, so it is asked again.
                    if holds_a_variable(&ty) {
                        let captor = Captor::Handler { owner: owner.to_string(), label: label.clone() };
                        self.unsettled_captures.push((local, captor, range));
                    }
                    continue;
                }
                self.refuse_handler_capture(owner, label, local, &ty, range);
            }
        }
    }

    fn refuse_handler_capture(
        &mut self,
        owner: &str,
        label: &str,
        local: khora_hir::body::LocalId,
        ty: &Type,
        range: TextRange,
    ) {
        let name = self.body.local(local).name.clone();
        let why = self.types.why_unshareable(ty);
        self.error(
            format!(
                "`{owner}`'s `{label}` captures `{name}`, and a handler has to be safe to hand \
                 to another fiber: {why}"
            ),
            range,
        );
    }

    /// What the expression behind a handler's operation closes over.
    ///
    /// A lambda's captures are recorded; a named function has none. Anything
    /// else is a closure this expression did not create, whose captures were
    /// decided elsewhere — and "elsewhere" is exactly what cannot be checked,
    /// so it is refused by having no answer rather than by pretending to one.
    pub(super) fn captures_of(&self, value: ExprId) -> Vec<khora_hir::body::LocalId> {
        match self.body.expr(value) {
            Expr::Lambda { captures, .. } => captures
                .iter()
                .copied()
                .chain(self.lambda_captures.get(&value).into_iter().flatten().copied())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The type parameters this function declared `Share` for.
    pub(super) fn shared_params(&self) -> Vec<String> {
        self.signature
            .generics
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                self.signature.bounds.get(*i).is_some_and(|b| b.iter().any(|t| t.name == SHARE))
            })
            .map(|(_, g)| g.clone())
            .collect()
    }
}

/// What captured a value whose type was not yet settled, so the re-check in
/// [`Checker::check_unsettled_captures`] refuses it in that one's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Captor {
    /// A body handed to `Fiber::spawn` or `SharedFn::of`.
    Spawn,
    /// An operation of a `handler for` `owner`, the field `label`.
    Handler { owner: String, label: String },
}

/// Whether a zonked type still holds an inference variable anywhere a value
/// could be: the variables that answer "shareable" before they are solved.
///
/// Exhaustive rather than `_ => false`, so that a new type form has to say
/// whether it can hide one. A function type answers false: it is refused
/// whatever it holds. A row carries no value.
fn holds_a_variable(ty: &Type) -> bool {
    match ty {
        Type::Var(_) => true,
        Type::Adt { args, .. } | Type::Tuple(args) => args.iter().any(holds_a_variable),
        Type::Applied { head, args } => holds_a_variable(head) || args.iter().any(holds_a_variable),
        Type::Assoc { owner, .. } => holds_a_variable(owner),
        Type::Fn { .. } | Type::Row { .. } => false,
        Type::Int
        | Type::Fixed(_)
        | Type::Float
        | Type::Bool
        | Type::Str
        | Type::Unit
        | Type::Ptr
        | Type::Char
        | Type::Param(_)
        | Type::Const(_)
        | Type::Never
        | Type::Unknown => false,
    }
}
