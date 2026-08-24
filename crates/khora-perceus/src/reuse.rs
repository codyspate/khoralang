//! Which arms may build their result in the cell they matched.
//!
//! Deliberately syntactic. The token `khora_drop_reuse` hands back is memory
//! with no owner, so the one thing that must be true is that the constructor is
//! reached — and requiring the arm's body to *be* the constructor makes that
//! visible in one place. Everything this declines is a missed optimization;
//! anything it wrongly accepted would be a leak. `docs/design/reuse.md` §2.

use super::*;

impl<'a> Planner<'a> {
    /// Finds the `match` arms that may build their result in the matched cell.
    ///
    /// Deliberately syntactic. The token `khora_drop_reuse` hands back is
    /// memory with no owner — nothing will free it and no counter is watching
    /// it — so the one thing that must be true is that the constructor is
    /// reached. Requiring the arm's body to *be* the constructor makes that
    /// visible in one place, which is what `docs/design/reuse.md` §2 asks for.
    ///
    /// Everything this declines is a missed optimization. Everything it
    /// wrongly accepted would be a leak.
    pub(super) fn plan_reuse(&mut self) {
        let Some(root) = self.body.root else { return };
        let mut found = Vec::new();
        self.collect_reuse(root, &mut found);
        for (arm, site) in found {
            self.plan.reuse.insert(arm, site);
        }
    }

    pub(super) fn collect_reuse(&self, id: ExprId, found: &mut Vec<(ExprId, ExprId)>) {
        if let Expr::Match { arms, .. } = self.body.expr(id) {
            for arm in arms {
                if let Some(site) = self.reusable_site(arm.body) {
                    found.push((arm.body, site));
                }
            }
        }
        self.each_child(id, &mut |child| self.collect_reuse(child, found));
    }

    /// The constructor an arm may build in the matched cell, if this arm may.
    pub(super) fn reusable_site(&self, body: ExprId) -> Option<ExprId> {
        let builds = match self.body.expr(body) {
            Expr::Record { .. } => true,
            Expr::Call { callee, .. } => {
                matches!(self.body.expr(*callee), Expr::Path(khora_hir::Resolution::Variant { .. }))
            }
            _ => false,
        };
        if !builds || self.may_leave_early(body) {
            return None;
        }
        Some(body)
    }

    /// Whether anything inside `id` can leave the frame without reaching the
    /// end of it.
    ///
    /// `!` and `raise` unwind, `return` leaves, and `break` and `continue` jump
    /// past whatever follows. Each of them is a path from the arm's head that
    /// never reaches the arm's constructor, and a token on such a path is
    /// leaked memory.
    pub(super) fn may_leave_early(&self, id: ExprId) -> bool {
        if matches!(
            self.body.expr(id),
            Expr::Raise(_)
                | Expr::Try(_)
                | Expr::Return(_)
                | Expr::Break(_)
                | Expr::Continue
                | Expr::Catch { .. }
        ) {
            return true;
        }
        let mut found = false;
        self.each_child(id, &mut |child| found = found || self.may_leave_early(child));
        found
    }
}
