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
                continue;
            }
            let name = self.body.local(local).name.clone();
            let why = self.types.why_unshareable(&ty);
            self.error(format!("`{name}` cannot be handed to another fiber: {why}"), range);
        }
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
                    continue;
                }
                let name = self.body.local(local).name.clone();
                let why = self.types.why_unshareable(&ty);
                self.error(
                    format!(
                        "`{owner}`'s `{label}` captures `{name}`, and a handler has to be safe \
                         to hand to another fiber: {why}"
                    ),
                    range,
                );
            }
        }
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
