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
        for (arm, sites) in found {
            self.plan.reuse.insert(arm, sites);
        }
    }

    pub(super) fn collect_reuse(&self, id: ExprId, found: &mut Vec<(ExprId, Vec<ExprId>)>) {
        if let Expr::Match { arms, .. } = self.body.expr(id) {
            for arm in arms {
                if let Some(sites) = self.reusable_site(arm.body) {
                    found.push((arm.body, sites));
                }
            }
        }
        self.each_child(id, &mut |child| self.collect_reuse(child, found));
    }

    /// The constructor an arm may build in the matched cell, if this arm may.
    pub(super) fn reusable_site(&self, body: ExprId) -> Option<Vec<ExprId>> {
        if self.may_leave_early(body) {
            return None;
        }
        let mut sites = Vec::new();
        self.constructors_on_every_path(body, &mut sites).then_some(sites)
    }

    /// Whether every path through `id` ends at a constructor, collecting them.
    ///
    /// **The token has no owner, so it must be spent on every path**, and that
    /// is the whole of the rule. Requiring the arm's body to *be* the
    /// constructor made it true by making there be one path; descending
    /// through a branch keeps it true as long as no arm of that branch is
    /// anything else. One arm that is not sinks the whole attempt, because a
    /// path that reaches no constructor is memory nothing frees.
    ///
    /// This is what `Range::next` and `Filtered::next` needed: both write an
    /// `if` where the rule was looking for a constructor, so neither could
    /// ever build in the cell it had just taken apart.
    fn constructors_on_every_path(&self, id: ExprId, sites: &mut Vec<ExprId>) -> bool {
        match self.body.expr(id) {
            Expr::Record { .. } => {
                sites.push(id);
                true
            }
            // **A case with no payload is still an allocation**: it has a tag,
            // and a tag lives in a header. `Step::Done` is one, and it is the
            // other half of `Range::next`'s `if`.
            Expr::Path(khora_hir::Resolution::Variant { .. }) => {
                sites.push(id);
                true
            }
            Expr::Call { callee, .. }
                if matches!(
                    self.body.expr(*callee),
                    Expr::Path(khora_hir::Resolution::Variant { .. })
                ) =>
            {
                sites.push(id);
                true
            }
            Expr::If { then_branch, else_branch: Some(otherwise), .. } => {
                let (then_branch, otherwise) = (*then_branch, *otherwise);
                self.constructors_on_every_path(then_branch, sites)
                    && self.constructors_on_every_path(otherwise, sites)
            }
            // Only an `if`, and deliberately not a nested `match`: a `match`
            // makes a token of its own for each of its arms, and two live at
            // once is a question this does not need to answer to reach the
            // shapes that wanted it.
            //
            // A block reaches its tail, and what its statements do on the way
            // is `may_leave_early`'s question rather than this one.
            Expr::Block { tail: Some(tail), .. } => {
                let tail = *tail;
                self.constructors_on_every_path(tail, sites)
            }
            _ => false,
        }
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
