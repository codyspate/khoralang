//! Reference counting: where `dup` and `drop` go.
//!
//! Roadmap phase 2.3. This inserts *correct* reference counting, not yet
//! *minimal* reference counting. Perceus proper earns its name by removing
//! pairs that provably cancel and by fusing a `drop` with a following
//! allocation into an in-place `reuse` — that is phase 6 (FBIP), and it is an
//! optimisation over exactly this output. Getting the conservative version
//! right first means phase 6 has something to prove itself against.
//!
//! # The scheme
//!
//! Only *boxed* values are counted: `Int` and `Bool` are machine words with
//! nothing to own. Strings and ADTs live behind the header in `khora-rt`.
//!
//! - A local holding a boxed value **owns** one reference.
//! - Reading such a local yields a value that outlives the read, so the read
//!   `dup`s.
//! - A block `drop`s every boxed local it declared, on the way out.
//! - Parameters are owned by the callee, so they are dropped like locals.
//!
//! # The interface
//!
//! The output is a side table keyed by [`ExprId`] and [`LocalId`] rather than a
//! new IR. Code generation walks the same HIR the type checker did and consults
//! this as it goes, so there is no third representation to keep in step — which
//! matters most while the three passes are all still moving.

use std::collections::{HashMap, HashSet};

use khora_db::{Db, SourceFile};
use khora_hir::body::{Body, Expr, ExprId, LocalId, Pat, PatId, Stmt};
use khora_types::Type;

/// Where reference-counting operations belong in one function body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RcPlan {
    /// Local reads whose result must be `dup`ed, by the id of the reading
    /// expression.
    pub dups: HashSet<ExprId>,
    /// Locals to `drop` when a block exits, keyed by the block's id.
    pub drops: HashMap<ExprId, Vec<LocalId>>,
    /// Locals holding a boxed value. Everything else is a machine word.
    pub boxed: HashSet<LocalId>,
}

impl RcPlan {
    pub fn needs_dup(&self, expr: ExprId) -> bool {
        self.dups.contains(&expr)
    }

    pub fn drops_for(&self, block: ExprId) -> &[LocalId] {
        self.drops.get(&block).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn is_boxed(&self, local: LocalId) -> bool {
        self.boxed.contains(&local)
    }
}

/// Whether values of this type carry a reference count.
///
/// `Unknown` counts as unboxed: it only appears downstream of an error, and a
/// spurious `drop` on a machine word would be a wild free.
pub fn is_boxed(ty: &Type) -> bool {
    matches!(ty, Type::Str | Type::Adt(_))
}

/// Plans reference counting for every function body in a file.
#[salsa::tracked(returns(ref))]
pub fn rc_plans(db: &dyn Db, file: SourceFile) -> Vec<(String, RcPlan)> {
    let types = khora_types::type_map(db, file);
    khora_hir::body::bodies(db, file)
        .iter()
        .map(|(name, body)| {
            let signature = types.signatures.get(name);
            let mut planner = Planner {
                body,
                plan: RcPlan::default(),
                types,
                local_types: HashMap::new(),
            };
            planner.plan_function(signature);
            (name.clone(), planner.plan)
        })
        .collect()
}

struct Planner<'a> {
    body: &'a Body,
    plan: RcPlan,
    types: &'a khora_types::TypeMap,
    local_types: HashMap<LocalId, Type>,
}

impl<'a> Planner<'a> {
    fn plan_function(&mut self, signature: Option<&khora_types::Signature>) {
        let mut owned = Vec::new();
        for (i, pat) in self.body.params.iter().enumerate() {
            let ty = signature
                .and_then(|s| s.params.get(i).cloned())
                .unwrap_or(Type::Unknown);
            self.bind(*pat, &ty, &mut owned);
        }

        let Some(root) = self.body.root else { return };
        self.walk(root);

        // Parameters are owned by the callee, so the outermost block releases
        // them along with whatever it declared itself.
        if !owned.is_empty() {
            self.plan.drops.entry(root).or_default().extend(owned);
        }
    }

    /// Records a pattern's bindings and collects the boxed ones.
    fn bind(&mut self, pat: PatId, ty: &Type, owned: &mut Vec<LocalId>) {
        match self.body.pat(pat).clone() {
            Pat::Bind(local) => {
                self.local_types.insert(local, ty.clone());
                if is_boxed(ty) {
                    self.plan.boxed.insert(local);
                    owned.push(local);
                }
            }
            Pat::TupleStruct { resolution, fields } => {
                let variant = match &resolution {
                    khora_hir::Resolution::Variant { name, .. } => {
                        self.types.variants.iter().find(|v| &v.name == name).cloned()
                    }
                    _ => None,
                };
                for (i, field) in fields.iter().enumerate() {
                    let field_ty = variant
                        .as_ref()
                        .and_then(|v| v.fields.get(i).cloned())
                        .unwrap_or(Type::Unknown);
                    self.bind(*field, &field_ty, owned);
                }
            }
            Pat::Tuple(fields) => {
                for field in fields {
                    self.bind(field, &Type::Unknown, owned);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => {}
        }
    }

    fn walk(&mut self, id: ExprId) {
        match self.body.expr(id).clone() {
            Expr::Local(local) => {
                // The value outlives the read, so it needs its own reference.
                if self.plan.boxed.contains(&local) {
                    self.plan.dups.insert(id);
                }
            }
            Expr::Block { stmts, tail } => {
                let mut declared = Vec::new();
                for stmt in &stmts {
                    match stmt {
                        Stmt::Let { pat, init } => {
                            if let Some(init) = init {
                                self.walk(*init);
                            }
                            let ty = init
                                .map(|e| self.type_of(e))
                                .unwrap_or(Type::Unknown);
                            self.bind(*pat, &ty, &mut declared);
                        }
                        Stmt::Expr(e) => self.walk(*e),
                    }
                }
                if let Some(tail) = tail {
                    self.walk(tail);
                }
                if !declared.is_empty() {
                    self.plan.drops.entry(id).or_default().extend(declared);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.walk(scrutinee);
                let scrutinee_ty = self.type_of(scrutinee);
                for arm in &arms {
                    // Arm bindings borrow out of the scrutinee, which the arm
                    // does not own, so they are recorded but not dropped.
                    let mut ignored = Vec::new();
                    self.bind(arm.pat, &scrutinee_ty, &mut ignored);
                    if let Some(guard) = arm.guard {
                        self.walk(guard);
                    }
                    self.walk(arm.body);
                }
            }
            Expr::Call { callee, args } => {
                self.walk(callee);
                for arg in args {
                    self.walk(arg);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.walk(lhs);
                self.walk(rhs);
            }
            Expr::Assign { target, value } => {
                self.walk(target);
                self.walk(value);
            }
            Expr::Unary { operand, .. } => self.walk(operand),
            Expr::Field { base, .. } => self.walk(base),
            Expr::If { condition, then_branch, else_branch } => {
                self.walk(condition);
                self.walk(then_branch);
                if let Some(e) = else_branch {
                    self.walk(e);
                }
            }
            Expr::While { condition, body } => {
                self.walk(condition);
                self.walk(body);
            }
            Expr::Loop { body } => self.walk(body),
            Expr::Break(Some(v)) | Expr::Return(Some(v)) => self.walk(v),
            Expr::List(items) | Expr::Tuple(items) => {
                for item in items {
                    self.walk(item);
                }
            }
            Expr::Break(None)
            | Expr::Return(None)
            | Expr::Continue
            | Expr::Literal(_)
            | Expr::Path(_)
            | Expr::Unit
            | Expr::Missing
            | Expr::Unsupported(_)
            | Expr::Unresolved(_) => {}
        }
    }

    /// Enough of the type to decide boxedness. Full inference already ran; this
    /// only needs to distinguish a pointer from a machine word.
    fn type_of(&self, id: ExprId) -> Type {
        match self.body.expr(id) {
            Expr::Literal(khora_hir::body::Literal::Str(_)) => Type::Str,
            Expr::Local(local) => self.local_types.get(local).cloned().unwrap_or(Type::Unknown),
            Expr::Call { callee, .. } => match self.body.expr(*callee) {
                Expr::Path(khora_hir::Resolution::Variant { type_name, .. }) => {
                    Type::Adt(type_name.clone())
                }
                Expr::Path(khora_hir::Resolution::Item { name, .. }) => self
                    .types
                    .signatures
                    .get(name)
                    .map(|s| s.ret.clone())
                    .unwrap_or(Type::Unknown),
                _ => Type::Unknown,
            },
            Expr::Path(khora_hir::Resolution::Variant { type_name, .. }) => {
                Type::Adt(type_name.clone())
            }
            _ => Type::Unknown,
        }
    }
}
