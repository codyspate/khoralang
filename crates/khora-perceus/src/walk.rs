//! Asking questions about a body's tree.
//!
//! What a subtree reads, what it takes, what it binds, whether one expression
//! contains another. The passes above are decisions; this is what they consult.
//!
//! `each_child` is the one to keep correct: a missing arm here does not fail,
//! it silently under-reports, and an under-reported read is a binding handed
//! away while something still needs it.

use super::*;

impl<'a> Planner<'a> {
    /// Every binding a call site reads without the body saying so.
    ///
    /// **A capability is read where nothing mentions it.** `with { clock:
    /// Clock }` puts `clock` in scope, and a call to something that also wants
    /// a `Clock` is handed the evidence by code generation — there is no
    /// `Expr::Local` for that read, so a backward pass over the expressions
    /// cannot see it. `health` in the link shortener mentions `clock` exactly
    /// once and forwards it twice afterwards; taking it at the mention left the
    /// two forwards reading a binding that had been handed away.
    ///
    /// `Body::capabilities` is the record of precisely those reads — which
    /// binding supplies each label at each call site — so it is what decides
    /// this rather than a guess about which locals look like capabilities.
    ///
    /// This was wrong before the last-use pass reached bodies that can unwind,
    /// and was survivable: the binding kept pointing at a handler the enclosing
    /// `with` block still held, so the count was one short rather than the
    /// pointer being wrong. Clearing the slot at a take is what turned it into
    /// a crash, which is the better failure and how it was found.
    pub(super) fn forwarded_capabilities(&self) -> Live {
        let mut found = Live::new();
        for supplied in self.body.capabilities.values() {
            found.extend(supplied.iter().map(|(_, local)| *local));
        }
        // The evidence this body was handed, whether or not it forwards any on.
        for (_, pat) in &self.body.evidence {
            found.extend(self.bound_by(*pat));
        }
        found
    }

    /// Every binding that holds a reference belonging to somebody else.
    ///
    /// A `match` arm's are no longer among them — `arm_binds` gives the arm a
    /// copy of what its body reads, so those are ordinary owning locals. A
    /// binding the body does not read gets no copy and stays borrowed, and so
    /// does everything a `catch` arm binds: the error object is released by
    /// `lower_catch` by its runtime type, not by a plan.
    pub(super) fn projected_bindings(&self, id: ExprId) -> Live {
        let mut found = Live::new();
        self.collect_projected(id, &mut found);
        found
    }

    pub(super) fn collect_projected(&self, id: ExprId, found: &mut Live) {
        match self.body.expr(id) {
            Expr::Match { arms, .. } => {
                for arm in arms {
                    let owned = self.plan.arm_binds_for(arm.body);
                    found.extend(
                        self.bound_by(arm.pat).into_iter().filter(|l| !owned.contains(l)),
                    );
                }
            }
            Expr::Catch { arms, .. } => {
                for arm in arms {
                    found.extend(self.bound_by(arm.pat));
                }
            }
            _ => {}
        }
        self.each_child(id, &mut |child| self.collect_projected(child, found));
    }

    /// Every binding introduced anywhere inside `id`.
    pub(super) fn bindings_in(&self, id: ExprId) -> Live {
        let mut found = Live::new();
        self.collect_bindings(id, &mut found);
        found
    }

    pub(super) fn collect_bindings(&self, id: ExprId, found: &mut Live) {
        match self.body.expr(id) {
            Expr::Block { stmts, .. } => {
                for stmt in stmts {
                    if let Stmt::Let { pat, .. } = stmt {
                        found.extend(self.bound_by(*pat));
                    }
                }
            }
            Expr::Match { arms, .. } | Expr::Catch { arms, .. } => {
                for arm in arms {
                    found.extend(self.bound_by(arm.pat));
                }
            }
            Expr::Lambda { params, .. } => {
                for param in params.clone() {
                    found.extend(self.bound_by(param));
                }
            }
            _ => {}
        }
        self.each_child(id, &mut |child| self.collect_bindings(child, found));
    }

    /// Every boxed binding read anywhere inside `id`.
    pub(super) fn reads_in(&self, id: ExprId) -> Live {
        let mut found = Live::new();
        self.collect_reads(id, &mut found);
        found
    }

    /// Every boxed binding whose reference is *taken* inside `id`.
    pub(super) fn takes_in(&self, id: ExprId) -> Live {
        let mut found = Live::new();
        for read in &self.reads {
            if !self.plan.dups.contains(&read.at)
                && !read.borrowed
                && self.within(id, read.at)
            {
                found.insert(read.local);
            }
        }
        found
    }

    /// Whether `needle` is `haystack` or somewhere inside it.
    pub(super) fn within(&self, haystack: ExprId, needle: ExprId) -> bool {
        if haystack == needle {
            return true;
        }
        let mut found = false;
        self.each_child(haystack, &mut |child| {
            found = found || self.within(child, needle);
        });
        found
    }

    /// Puts back the copies a take removed, for a binding a branch cannot
    /// consume after all.
    pub(super) fn restore_dups(&mut self, arm: ExprId, local: LocalId) {
        let places: Vec<ExprId> = self
            .reads
            .iter()
            .filter(|read| read.local == local && !read.borrowed && self.within(arm, read.at))
            .map(|read| read.at)
            .collect();
        for at in places {
            self.plan.dups.insert(at);
            self.plan.takes.remove(&at);
        }
        // A branch nested inside may have granted an arm release for this
        // binding on the strength of a take that is now a copy again. Left in
        // place it would release something the block still releases.
        let inner: Vec<ExprId> = self
            .plan
            .arm_drops
            .keys()
            .copied()
            .filter(|at| self.within(arm, *at))
            .collect();
        for at in inner {
            if let Some(releases) = self.plan.arm_drops.get_mut(&at) {
                releases.retain(|held| *held != local);
            }
        }
        self.plan.arm_drops.retain(|_, releases| !releases.is_empty());
    }

    pub(super) fn collect_reads(&self, id: ExprId, found: &mut Live) {
        if let Expr::Local(local) = self.body.expr(id) {
            if self.plan.boxed.contains(local) {
                found.insert(*local);
            }
        }
        self.each_child(id, &mut |child| self.collect_reads(child, found));
    }

    /// Every expression `id` immediately contains.
    pub(super) fn each_child(&self, id: ExprId, visit: &mut dyn FnMut(ExprId)) {
        match self.body.expr(id) {
            Expr::Block { stmts, tail } => {
                for stmt in stmts {
                    match stmt {
                        Stmt::Expr(e) => visit(*e),
                        Stmt::Let { init, .. } => {
                            if let Some(init) = init {
                                visit(*init);
                            }
                        }
                    }
                }
                if let Some(tail) = tail {
                    visit(*tail);
                }
            }
            Expr::Call { callee, args } => {
                visit(*callee);
                for arg in args {
                    visit(*arg);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                visit(*lhs);
                visit(*rhs);
            }
            Expr::Assign { target, value } => {
                visit(*target);
                visit(*value);
            }
            Expr::Unary { operand, .. } => visit(*operand),
            Expr::Field { base, .. } => visit(*base),
            Expr::If { condition, then_branch, else_branch } => {
                visit(*condition);
                visit(*then_branch);
                if let Some(otherwise) = else_branch {
                    visit(*otherwise);
                }
            }
            Expr::While { condition, body } => {
                visit(*condition);
                visit(*body);
            }
            Expr::Loop { body } => visit(*body),
            Expr::Match { scrutinee, arms } => {
                visit(*scrutinee);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        visit(guard);
                    }
                    visit(arm.body);
                }
            }
            Expr::Catch { inner, arms } => {
                visit(*inner);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        visit(guard);
                    }
                    visit(arm.body);
                }
            }
            Expr::Lambda { body, .. } => visit(*body),
            Expr::Record { fields, .. } => {
                for (_, value) in fields {
                    visit(*value);
                }
            }
            Expr::Tuple(items) => {
                for item in items {
                    visit(*item);
                }
            }
            // `Shown` holds the value a `${..}` hole was written around, and
            // that value is an argument to `Show::show` like any other. Left
            // out of this walk it was invisible to the reference-count plan,
            // which is not a compile error and is a crash.
            Expr::Raise(inner) | Expr::Try(inner) | Expr::Shown(inner) => visit(*inner),
            Expr::Break(Some(v)) | Expr::Return(Some(v)) => visit(*v),
            _ => {}
        }
    }

    /// The locals a pattern binds.
    pub(super) fn bound_by(&self, pat: PatId) -> Vec<LocalId> {
        let mut found = Vec::new();
        self.gather_bound(pat, &mut found);
        found
    }

    pub(super) fn gather_bound(&self, pat: PatId, found: &mut Vec<LocalId>) {
        match self.body.pat(pat) {
            Pat::Bind(local) => found.push(*local),
            Pat::TupleStruct { fields, .. } | Pat::Tuple(fields) => {
                for field in fields.clone() {
                    self.gather_bound(field, found);
                }
            }
            _ => {}
        }
    }
}
