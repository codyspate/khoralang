//! Function bodies, lowered.
//!
//! The second half of roadmap phase 2.1. Item collection answered "what exists
//! and where"; this answers "what does it do".
//!
//! # Why arenas
//!
//! Expressions and patterns live in side arenas indexed by [`ExprId`] and
//! [`PatId`] rather than in a tree of boxes. Every later pass — type inference,
//! Perceus, codegen — needs to attach its own information to each node, and an
//! index is a key those passes can use without touching this crate's types or
//! holding a borrow of the syntax tree.
//!
//! # Where the decision tree went
//!
//! `docs/roadmap.md` 2.1 says `match` is compiled to a decision tree here. It
//! is not, deliberately. Exhaustiveness and reachability (2.2) are computed by
//! Maranget's usefulness algorithm over a *pattern matrix*, and the decision
//! tree is compiled from that same matrix. Building the tree first would mean
//! reconstructing the matrix to check it, so HIR keeps the arms as written and
//! the tree is compiled later, nearer codegen. This is what rustc does, and it
//! is the reason the two consumers do not fight over one shape.
//!
//! # Scope
//!
//! The phase 2 subset: no effects, rows, generics, closures or records. Syntax
//! outside it lowers to [`Expr::Unsupported`] with a diagnostic rather than
//! being silently dropped, so a later phase can find every site by grepping for
//! one variant.

use khora_db::{Db, SourceFile};
use khora_syntax::ast::{self, AstNode};
use text_size::TextRange;

use crate::{item_map, HirError};

macro_rules! arena_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u32);

        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

arena_id!(ExprId);
arena_id!(PatId);
arena_id!(LocalId);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    Int(String),
    Float(String),
    Str(String),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

impl BinOp {
    fn from_token(text: &str) -> Option<BinOp> {
        let op = match text {
            "+" => BinOp::Add,
            "-" => BinOp::Sub,
            "*" => BinOp::Mul,
            "/" => BinOp::Div,
            "%" => BinOp::Rem,
            "==" => BinOp::Eq,
            "!=" => BinOp::Ne,
            "<" => BinOp::Lt,
            ">" => BinOp::Gt,
            "<=" => BinOp::Le,
            ">=" => BinOp::Ge,
            "&&" => BinOp::And,
            "||" => BinOp::Or,
            _ => return None,
        };
        Some(op)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// A local binding: a parameter, a `let`, or a name bound by a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub name: String,
    pub is_mut: bool,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Let { pat: PatId, init: Option<ExprId> },
    Expr(ExprId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    pub pat: PatId,
    pub guard: Option<ExprId>,
    pub body: ExprId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A hole left by a parse error. Type inference treats it as compatible
    /// with anything, so one syntax error does not cascade into ten type
    /// errors.
    Missing,
    /// Recognized syntax outside the phase 2 subset.
    Unsupported(&'static str),
    Literal(Literal),
    /// A resolved local binding.
    Local(LocalId),
    /// A `::` path resolved against the module graph.
    Path(crate::Resolution),
    /// A name that did not resolve. The error is already reported; this keeps
    /// the tree shaped so later passes still see the surrounding structure.
    Unresolved(String),
    Field {
        base: ExprId,
        name: String,
    },
    Call {
        callee: ExprId,
        args: Vec<ExprId>,
    },
    Binary {
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Unary {
        op: UnOp,
        operand: ExprId,
    },
    Assign {
        target: ExprId,
        value: ExprId,
    },
    If {
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    Block {
        stmts: Vec<Stmt>,
        tail: Option<ExprId>,
    },
    While {
        condition: ExprId,
        body: ExprId,
    },
    Loop {
        body: ExprId,
    },
    Break(Option<ExprId>),
    Continue,
    Return(Option<ExprId>),
    List(Vec<ExprId>),
    Tuple(Vec<ExprId>),
    /// `raise DbError::Timeout` — leaves the function with an error.
    ///
    /// Type `Never`, so it stands wherever an expression can.
    Raise(ExprId),
    /// `f()!` — this call can leave the enclosing function.
    ///
    /// Type-wise the identity: `f()!` has the type `f()` has. Its work is to
    /// mark, and to say where the branch goes. `docs/design/effects.md`
    /// justifies the mark on readability; `docs/design/effect-runtime.md` §2
    /// notes it is also exactly where the check belongs.
    Try(ExprId),
    /// The closure currently executing, inside its own body.
    ///
    /// `let go = fn n => .. go(n - 1) ..` reads as a capture and would be one:
    /// the closure would hold a counted reference to itself, which is a cycle,
    /// and reference counting does not collect cycles. It need not be a
    /// reference at all — a lifted lambda already receives its own closure
    /// object as its first argument, so self-recursion goes through that.
    /// See `docs/design/memory.md` §3.
    LambdaSelf,
    /// `{ x: 1, y: 2 }`, or the operations of a `handler for E { .. }`.
    ///
    /// `owner` is the type when the syntax names one — `handler for Ledger`
    /// does — and `None` for a bare literal, whose type the checker finds from
    /// the labels.
    Record {
        owner: Option<String>,
        fields: Vec<(String, ExprId)>,
    },
    /// `(x) => x + 1`.
    ///
    /// The body lives in the *same* arena as the enclosing function, which is
    /// what lets a captured local keep its identity: `captures` names locals
    /// declared outside the lambda, and the type map and reference-counting
    /// plan cover the lambda's expressions without a second pass over a second
    /// body.
    Lambda {
        params: Vec<PatId>,
        body: ExprId,
        /// Locals from an enclosing scope that the body reads, in first-use
        /// order. Captured **by value**: the closure takes its own reference.
        captures: Vec<LocalId>,
    },
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    Missing,
    Wildcard,
    Bind(LocalId),
    Literal(Literal),
    /// `Type::Case` with no payload.
    Path(crate::Resolution),
    /// `Type::Case(a, b)`.
    TupleStruct {
        resolution: crate::Resolution,
        fields: Vec<PatId>,
    },
    Tuple(Vec<PatId>),
}

/// One lowered function body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Body {
    exprs: Vec<Expr>,
    pats: Vec<Pat>,
    locals: Vec<Local>,
    /// Source range of each expression, for diagnostics.
    expr_ranges: Vec<TextRange>,
    pub params: Vec<PatId>,
    /// The capabilities this function requires, by the label the body calls
    /// them and the binding that holds them.
    ///
    /// `with { ledger: Ledger }` puts `ledger` in scope for the body — there
    /// is no `ask`. They are kept apart from `params` because they are not
    /// written at the call site: code generation appends them, per
    /// `docs/design/effect-runtime.md` §2.
    pub evidence: Vec<(String, PatId)>,
    /// The capabilities each `with` block supplies, by the block's id.
    ///
    /// A `with` block lowers to an ordinary block of `let`s — that is the whole
    /// of installation at runtime. But *which* labels it supplied is not
    /// recoverable from that, and the checker needs it: a requirement raised
    /// inside the block is discharged by it rather than by the signature. This
    /// is row subtraction, kept where it can still be read.
    pub installs: std::collections::HashMap<ExprId, Vec<String>>,
    /// Which binding supplies each capability label at each call site.
    ///
    /// Recorded here because it can only be answered *while* lowering, when
    /// the scope stack is live. Resolving it later by name is wrong the moment
    /// two `with` blocks in one function bind the same label: they are sibling
    /// scopes, and the last declaration is not the one in scope at the first
    /// call.
    pub capabilities: std::collections::HashMap<ExprId, Vec<(String, LocalId)>>,
    pub root: Option<ExprId>,
    pub errors: Vec<HirError>,
}

impl Body {
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.index()]
    }

    pub fn pat(&self, id: PatId) -> &Pat {
        &self.pats[id.index()]
    }

    /// The binding that supplies `label` at the call site `at`.
    ///
    /// Recorded during lowering rather than searched for here: two `with`
    /// blocks in one function are sibling scopes binding the same label, and
    /// nothing about the finished arena says which was in scope where.
    pub fn capability_at(&self, at: ExprId, label: &str) -> Option<LocalId> {
        self.capabilities
            .get(&at)?
            .iter()
            .rev()
            .find(|(l, _)| l == label)
            .map(|(_, local)| *local)
    }

    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.index()]
    }

    pub fn range(&self, id: ExprId) -> TextRange {
        self.expr_ranges[id.index()]
    }

    pub fn exprs(&self) -> impl Iterator<Item = (ExprId, &Expr)> {
        self.exprs.iter().enumerate().map(|(i, e)| (ExprId(i as u32), e))
    }

    pub fn locals(&self) -> impl Iterator<Item = (LocalId, &Local)> {
        self.locals.iter().enumerate().map(|(i, l)| (LocalId(i as u32), l))
    }

    pub fn expr_count(&self) -> usize {
        self.exprs.len()
    }
}

/// Lowers every function body in a file.
///
/// Keyed by file rather than by function so that the query reads one file and
/// no other, the same property `item_map` has.
#[salsa::tracked(returns(ref))]
pub fn bodies(db: &dyn Db, file: SourceFile) -> Vec<(String, Body)> {
    let parse = khora_db::parse(db, file);
    let map = item_map(db, file);
    let scope = crate::file_scope(db, file);

    parse
        .source_file()
        .decls()
        .flat_map(|decl| match decl {
            ast::Decl::Fn(f) => {
                let lowered = (|| {
                    let name = f.name()?.ident()?;
                    let body = f.body()?;
                    Some((name, lower_function(map, scope, &f, &body)))
                })();
                lowered.into_iter().collect::<Vec<_>>()
            }
            // A trait's functions are lowered too: one with a body is a default
            // implementation, and it has to be checked like any other.
            ast::Decl::Trait(t) => {
                let owner = t.name().and_then(|n| n.ident()).unwrap_or_default();
                methods(map, scope, &owner, t.functions())
            }
            ast::Decl::Impl(i) => methods(map, scope, &impl_key(&i), i.functions()),
            _ => Vec::new(),
        })
        .collect()
}

/// The name an impl's bodies are recorded under.
///
/// `Eq#Int`, not the trait alone: one type may implement several traits and one
/// trait many types, so neither half identifies a body on its own. `#` cannot
/// occur in a Khora identifier, so this can never collide with a name a program
/// chose.
pub fn impl_key(decl: &ast::ImplDecl) -> String {
    let trait_name = decl.trait_().as_ref().and_then(type_head).unwrap_or_default();
    let self_name = decl.self_type().as_ref().and_then(type_head).unwrap_or_default();
    // An inherent impl has an empty trait half, so its methods key as `#User`.
    // Still unambiguous, and still impossible to collide with a Khora name.
    format!("{trait_name}#{self_name}")
}

/// The head constructor of a written type: `Option` for `Option<Int>`.
///
/// Instance resolution is nominal, so this is what selects an impl. See
/// `docs/design/typeclasses.md`.
pub fn type_head(ty: &ast::Type) -> Option<String> {
    match ty {
        ast::Type::Path(p) => p.path().map(|p| p.text_path()),
        _ => None,
    }
}

/// Lowers the functions of one trait or impl, keyed `owner::method`.
fn methods(
    map: &crate::ItemMap,
    scope: &crate::FileScope,
    owner: &str,
    functions: impl Iterator<Item = ast::FnDecl>,
) -> Vec<(String, Body)> {
    functions
        .filter_map(|f| {
            let name = f.name()?.ident()?;
            let body = f.body()?;
            Some((format!("{owner}::{name}"), lower_function(map, scope, &f, &body)))
        })
        .collect()
}

fn lower_function(
    map: &crate::ItemMap,
    scope: &crate::FileScope,
    decl: &ast::FnDecl,
    block: &ast::Block,
) -> Body {
    let mut generics: Vec<String> = vec!["Self".to_string()];
    if let Some(params) = decl.type_params() {
        generics.extend(params.params().filter_map(|p| p.name().and_then(|n| n.ident())));
    }

    let mut ctx = Ctx {
        body: Body::default(),
        scopes: vec![Vec::new()],
        map,
        scope,
        generics,
        loop_depth: 0,
        in_scope: Vec::new(),
        lambdas: Vec::new(),
        lambda_names: Vec::new(),
    };

    if let Some(params) = decl.params() {
        for param in params.params() {
            let range = param.syntax().text_range();
            let pat = match param.name().and_then(|n| n.ident()) {
                Some(name) => {
                    let local = ctx.declare(name, false, range);
                    ctx.add_pat(Pat::Bind(local))
                }
                None => ctx.add_pat(Pat::Wildcard),
            };
            ctx.body.params.push(pat);
        }
    }

    // `with { ledger: Ledger }` puts `ledger` in scope for the body, sorted by
    // label so the order matches the row and, through it, the order code
    // generation appends the arguments in.
    let mut labels: Vec<String> = decl
        .with_clause()
        .and_then(|c| c.row())
        .map(|row| capability_labels(&row))
        .unwrap_or_default();
    labels.sort();
    labels.dedup();
    for label in labels {
        let local = ctx.declare(label.clone(), false, decl.syntax().text_range());
        let pat = ctx.add_pat(Pat::Bind(local));
        ctx.in_scope.push((label.clone(), local));
        ctx.body.evidence.push((label, pat));
    }

    let root = ctx.lower_block(block);
    ctx.body.root = Some(root);
    ctx.body
}

/// The labels a capability row names, wherever they are written.
///
/// With a tail they nest inside it rather than beside it, so both places have
/// to be read — the same trap `khora-types` hit.
fn capability_labels(row: &ast::Type) -> Vec<String> {
    let ast::Type::Record(r) = row else { return Vec::new() };
    let nested: Vec<ast::Field> = r.row_tail().map(|t| t.fields().collect()).unwrap_or_default();
    r.fields().chain(nested).filter_map(|f| f.name().and_then(|n| n.ident())).collect()
}

struct Ctx<'a> {
    body: Body,
    /// A stack of lexical scopes; each holds the names it introduced.
    scopes: Vec<Vec<(String, LocalId)>>,
    map: &'a crate::ItemMap,
    /// Names this file imported. Consulted after the file's own items, so a
    /// local declaration shadows an import rather than colliding with it.
    scope: &'a crate::FileScope,
    /// The type parameters the enclosing function declared, plus `Self` inside
    /// a trait or impl. A path whose first segment is one of these names a
    /// trait function reached through that parameter.
    generics: Vec<String>,
    loop_depth: u32,
    /// One entry per lambda currently being lowered, holding the number of
    /// locals that existed when it started. A local below the innermost mark
    /// belongs to an enclosing scope, which is exactly what "captured" means.
    lambdas: Vec<usize>,
    /// Capability bindings in scope, innermost last. A `with` clause seeds
    /// it and a `with` block extends it for its region.
    in_scope: Vec<(String, LocalId)>,
    /// The name each lambda being lowered was bound to, when it was written as
    /// the initializer of a `let`. Inside the innermost one, that name is the
    /// closure itself rather than a capture of it.
    lambda_names: Vec<Option<String>>,
}

impl<'a> Ctx<'a> {
    fn add_expr(&mut self, expr: Expr, range: TextRange) -> ExprId {
        self.body.exprs.push(expr);
        self.body.expr_ranges.push(range);
        ExprId((self.body.exprs.len() - 1) as u32)
    }

    /// Adds a call, remembering which bindings could supply its capabilities.
    ///
    /// Which ones it actually needs is the callee's row, which is not known
    /// here; what is known, and only here, is what is in scope.
    fn add_call(&mut self, expr: Expr, range: TextRange) -> ExprId {
        let id = self.add_expr(expr, range);
        if !self.in_scope.is_empty() {
            // Innermost last, so a later duplicate label shadows an earlier one.
            let mut visible: Vec<(String, LocalId)> = Vec::new();
            for (label, local) in &self.in_scope {
                visible.retain(|(l, _)| l != label);
                visible.push((label.clone(), *local));
            }
            self.body.capabilities.insert(id, visible);
        }
        id
    }

    fn add_pat(&mut self, pat: Pat) -> PatId {
        self.body.pats.push(pat);
        PatId((self.body.pats.len() - 1) as u32)
    }

    fn declare(&mut self, name: String, is_mut: bool, range: TextRange) -> LocalId {
        self.body.locals.push(Local { name: name.clone(), is_mut, range });
        let id = LocalId((self.body.locals.len() - 1) as u32);
        self.scopes.last_mut().expect("a scope is always open").push((name, id));
        id
    }

    /// Innermost binding wins, which is what shadowing means.
    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.iter().rev().find(|(n, _)| n == name).map(|(_, id)| *id))
    }

    fn error(&mut self, message: impl Into<String>, range: TextRange) {
        self.body.errors.push(HirError { message: message.into(), range });
    }

    fn lower_block(&mut self, block: &ast::Block) -> ExprId {
        self.scopes.push(Vec::new());
        let mut stmts = Vec::new();

        for stmt in block.stmts() {
            match stmt {
                ast::Stmt::Let(let_decl) => {
                    // The initializer is lowered before the binding is
                    // declared, so `let x = x;` refers to the outer `x`.
                    //
                    // A lambda is the exception: `let go = fn n => .. go(..)`
                    // has to see its own name, or a recursive closure cannot be
                    // written at all. The name resolves to the closure itself,
                    // not to a binding that does not exist yet.
                    let recursive = match let_decl.initializer() {
                        Some(ast::Expr::Lambda(_)) => {
                            let_decl.pat().and_then(|p| match p {
                                ast::Pat::Ident(i) => i.name().and_then(|n| n.ident()),
                                _ => None,
                            })
                        }
                        _ => None,
                    };
                    let init = let_decl.initializer().map(|e| match (&e, recursive) {
                        (ast::Expr::Lambda(l), Some(name)) => {
                            let range = e.syntax().text_range();
                            self.lower_lambda_named(l, Some(name), range)
                        }
                        _ => self.lower_expr(&e),
                    });
                    let pat = match let_decl.pat() {
                        Some(p) => self.lower_pat(&p, let_decl.is_mut()),
                        None => self.add_pat(Pat::Missing),
                    };
                    stmts.push(Stmt::Let { pat, init });
                }
                ast::Stmt::Expr(expr_stmt) => {
                    if let Some(e) = expr_stmt.expr() {
                        let id = self.lower_expr(&e);
                        stmts.push(Stmt::Expr(id));
                    }
                }
            }
        }

        let tail = block.tail_expr().map(|e| self.lower_expr(&e));
        self.scopes.pop();
        self.add_expr(Expr::Block { stmts, tail }, block.syntax().text_range())
    }

    fn lower_pat(&mut self, pat: &ast::Pat, is_mut: bool) -> PatId {
        let range = pat.syntax().text_range();
        match pat {
            ast::Pat::Wildcard(_) => self.add_pat(Pat::Wildcard),
            ast::Pat::Ident(p) => match p.name().and_then(|n| n.ident()) {
                Some(name) => {
                    let local = self.declare(name, is_mut, range);
                    self.add_pat(Pat::Bind(local))
                }
                None => self.add_pat(Pat::Missing),
            },
            ast::Pat::Literal(p) => match literal_of(p.syntax()) {
                Some(lit) => self.add_pat(Pat::Literal(lit)),
                None => self.add_pat(Pat::Missing),
            },
            ast::Pat::Path(p) => {
                let resolution = self.resolve_pattern_path(p.path().as_ref(), range);
                self.add_pat(Pat::Path(resolution))
            }
            ast::Pat::TupleStruct(p) => {
                let resolution = self.resolve_pattern_path(p.path().as_ref(), range);
                let fields = p.fields().map(|f| self.lower_pat(&f, is_mut)).collect();
                self.add_pat(Pat::TupleStruct { resolution, fields })
            }
            ast::Pat::Tuple(p) => {
                let fields = p.fields().map(|f| self.lower_pat(&f, is_mut)).collect();
                self.add_pat(Pat::Tuple(fields))
            }
            ast::Pat::Record(_) => {
                self.error("record patterns are not supported yet", range);
                self.add_pat(Pat::Missing)
            }
        }
    }

    /// A pattern path names a constructor. Only same-file constructors resolve
    /// in phase 2, which is enough for the vertical slice.
    fn resolve_pattern_path(
        &mut self,
        path: Option<&ast::Path>,
        range: TextRange,
    ) -> crate::Resolution {
        let segments: Vec<String> = path
            .map(|p| p.segments().filter_map(|s| s.ident()).collect())
            .unwrap_or_default();

        if let [type_name, case] = segments.as_slice() {
            if let Some(v) = self
                .map
                .variants_of(type_name)
                .chain(self.scope.variants_of(type_name))
                .find(|v| &v.name == case)
            {
                return crate::Resolution::Variant {
                    module: self.map.module.clone().unwrap_or_else(|| crate::ModulePath::new(vec![])),
                    type_name: v.type_name.clone(),
                    name: v.name.clone(),
                };
            }
        }

        self.error(format!("cannot find constructor `{}`", segments.join("::")), range);
        crate::Resolution::Unsupported("unresolved constructor")
    }

    fn lower_expr(&mut self, expr: &ast::Expr) -> ExprId {
        let range = expr.syntax().text_range();
        match expr {
            ast::Expr::Literal(e) => match literal_of(e.syntax()) {
                Some(lit) => self.add_expr(Expr::Literal(lit), range),
                None => self.add_expr(Expr::Missing, range),
            },
            ast::Expr::Path(e) => self.lower_path_expr(e, range),
            ast::Expr::Block(b) => self.lower_block(b),
            ast::Expr::Paren(e) => match e.syntax().children().find_map(ast::Expr::cast) {
                Some(inner) => self.lower_expr(&inner),
                None => self.add_expr(Expr::Missing, range),
            },
            ast::Expr::Unit(_) => self.add_expr(Expr::Unit, range),
            ast::Expr::Field(e) => {
                let base = match e.base() {
                    Some(b) => self.lower_expr(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                let name = e.field().and_then(|f| f.ident()).unwrap_or_default();
                self.add_expr(Expr::Field { base, name }, range)
            }
            ast::Expr::Call(e) => {
                let callee = match e.callee() {
                    Some(c) => self.lower_expr(&c),
                    None => self.add_expr(Expr::Missing, range),
                };
                let args = e
                    .args()
                    .map(|list| list.args().map(|a| self.lower_expr(&a)).collect())
                    .unwrap_or_default();
                self.add_call(Expr::Call { callee, args }, range)
            }
            ast::Expr::Pipe(e) => self.lower_pipe(e, range),
            ast::Expr::Bin(e) => self.lower_binary(e, range),
            ast::Expr::Assign(e) => {
                let target = match e.target() {
                    Some(t) => self.lower_expr(&t),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.check_assignable(target, range);
                let value = match e.value() {
                    Some(v) => self.lower_expr(&v),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.add_expr(Expr::Assign { target, value }, range)
            }
            ast::Expr::Prefix(e) => {
                let op = match e.syntax().first_token().map(|t| t.text().to_string()).as_deref() {
                    Some("-") => UnOp::Neg,
                    _ => UnOp::Not,
                };
                let operand = match e.operand() {
                    Some(o) => self.lower_expr(&o),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.add_expr(Expr::Unary { op, operand }, range)
            }
            ast::Expr::If(e) => {
                let condition = match e.condition() {
                    Some(c) => self.lower_expr(&c),
                    None => self.add_expr(Expr::Missing, range),
                };
                let then_branch = match e.then_branch() {
                    Some(b) => self.lower_block(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                let else_branch = e.else_branch().map(|b| self.lower_expr(&b));
                self.add_expr(Expr::If { condition, then_branch, else_branch }, range)
            }
            ast::Expr::Match(e) => self.lower_match(e, range),
            ast::Expr::While(e) => {
                let condition = match e.condition() {
                    Some(c) => self.lower_expr(&c),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.loop_depth += 1;
                let body = match e.body() {
                    Some(b) => self.lower_block(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.loop_depth -= 1;
                self.add_expr(Expr::While { condition, body }, range)
            }
            ast::Expr::Loop(e) => {
                self.loop_depth += 1;
                let body = match e.body() {
                    Some(b) => self.lower_block(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.loop_depth -= 1;
                self.add_expr(Expr::Loop { body }, range)
            }
            ast::Expr::Break(e) => {
                if self.loop_depth == 0 {
                    self.error("`break` outside a loop", range);
                }
                let value = e.value().map(|v| self.lower_expr(&v));
                self.add_expr(Expr::Break(value), range)
            }
            ast::Expr::Continue(_) => {
                if self.loop_depth == 0 {
                    self.error("`continue` outside a loop", range);
                }
                self.add_expr(Expr::Continue, range)
            }
            ast::Expr::Return(e) => {
                let value = e.value().map(|v| self.lower_expr(&v));
                self.add_expr(Expr::Return(value), range)
            }
            ast::Expr::List(e) => {
                let items = e.syntax().children().filter_map(ast::Expr::cast).collect::<Vec<_>>();
                let ids = items.iter().map(|i| self.lower_expr(i)).collect();
                self.add_expr(Expr::List(ids), range)
            }
            ast::Expr::Tuple(e) => {
                let items = e.syntax().children().filter_map(ast::Expr::cast).collect::<Vec<_>>();
                let ids = items.iter().map(|i| self.lower_expr(i)).collect();
                self.add_expr(Expr::Tuple(ids), range)
            }
            ast::Expr::Placeholder(_) => {
                self.error("`_` is only meaningful in a pipeline stage", range);
                self.add_expr(Expr::Missing, range)
            }
            // Outside the phase 2 subset. Named individually so a later phase
            // can find every site.
            ast::Expr::For(e) => self.lower_for(e, range),
            ast::Expr::Lambda(e) => self.lower_lambda(e, range),
            ast::Expr::Record(e) => {
                let fields = self.lower_record_fields(e);
                self.add_expr(Expr::Record { owner: None, fields }, range)
            }
            ast::Expr::Raise(e) => {
                let error = match e.value() {
                    Some(v) => self.lower_expr(&v),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.add_expr(Expr::Raise(error), range)
            }
            ast::Expr::Try(e) => {
                let inner = match e.operand() {
                    Some(v) => self.lower_expr(&v),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.add_expr(Expr::Try(inner), range)
            }
            // `handler for Ledger { .. }` is a record literal whose type the
            // syntax names. Nothing about it is special at runtime: it builds
            // the same object a bare literal would.
            ast::Expr::Handler(e) => {
                let owner = e.effect().map(|p| p.text_path());
                let fields = e
                    .operations()
                    .map(|r| self.lower_record_fields(&r))
                    .unwrap_or_default();
                self.add_expr(Expr::Record { owner, fields }, range)
            }
            ast::Expr::Catch(_) => self.unsupported("`catch`", range),
            // `with { ledger: h } { body }` and `body with { ledger: h }`
            // are one thing: a region in which the labels are bound. That is
            // an ordinary block of `let`s, which is why installation needs no
            // runtime of its own and why an inner `with` shadows an outer one.
            ast::Expr::With(e) => {
                let body = e.body();
                self.lower_installation(e.row().as_ref(), body.as_ref(), range)
            }
            ast::Expr::WithBlock(e) => {
                let body = e.body().map(ast::Expr::Block);
                self.lower_installation(e.row().as_ref(), body.as_ref(), range)
            }
        }
    }

    fn unsupported(&mut self, what: &'static str, range: TextRange) -> ExprId {
        self.error(format!("{what} are not supported yet"), range);
        self.add_expr(Expr::Unsupported(what), range)
    }

    fn lower_path_expr(&mut self, e: &ast::PathExpr, range: TextRange) -> ExprId {
        let segments: Vec<String> = e
            .syntax()
            .children()
            .find_map(ast::Path::cast)
            .map(|p| p.segments().filter_map(|s| s.ident()).collect())
            .unwrap_or_default();

        // A bare name is a local first — shadowing is what people expect.
        if let [only] = segments.as_slice() {
            if let Some(local) = self.lookup(only) {
                return self.add_expr(Expr::Local(local), range);
            }
            if self.is_own_lambda(only) {
                return self.add_expr(Expr::LambdaSelf, range);
            }
            if self.is_enclosing_lambda(only) {
                // Reaching an *outer* lambda from an inner one would capture
                // it, and a closure that holds another closure holding it is a
                // cycle. Named functions have no closure object, so recursion
                // through one costs nothing.
                self.error(
                    format!(
                        "`{only}` is a closure being defined further out; only a closure's own \
                         name is in scope inside it. Use a named `fn` for mutual recursion"
                    ),
                    range,
                );
                return self.add_expr(Expr::Unresolved(only.clone()), range);
            }
            if let Some(item) = self.map.item(only) {
                return self.add_expr(
                    Expr::Path(crate::Resolution::Item {
                        module: self
                            .map
                            .module
                            .clone()
                            .unwrap_or_else(|| crate::ModulePath::new(vec![])),
                        name: item.name.clone(),
                        kind: item.kind,
                    }),
                    range,
                );
            }
            if let Some(resolution) = self.scope.get(only) {
                return self.add_expr(Expr::Path(resolution.clone()), range);
            }
            self.error(format!("cannot find `{only}` in this scope"), range);
            return self.add_expr(Expr::Unresolved(only.clone()), range);
        }

        // `F::pure` or `Applicative::pure`. Checked before module paths
        // because a type parameter in scope, or a trait declared here, is a
        // more specific reading than a module that happens to share the name.
        if let [owner, name] = segments.as_slice() {
            let is_param = self.generics.iter().any(|g| g == owner);
            let is_trait =
                self.map.item(owner).is_some_and(|i| i.kind == crate::ItemKind::Trait);
            if is_param || is_trait {
                return self.add_expr(
                    Expr::Path(crate::Resolution::TraitItem {
                        owner: owner.clone(),
                        name: name.clone(),
                    }),
                    range,
                );
            }
        }

        // A `::` path names a constructor or an item in another module.
        if let [type_name, case] = segments.as_slice() {
            if let Some(v) = self
                .map
                .variants_of(type_name)
                .chain(self.scope.variants_of(type_name))
                .find(|v| &v.name == case)
            {
                return self.add_expr(
                    Expr::Path(crate::Resolution::Variant {
                        module: self
                            .map
                            .module
                            .clone()
                            .unwrap_or_else(|| crate::ModulePath::new(vec![])),
                        type_name: v.type_name.clone(),
                        name: v.name.clone(),
                    }),
                    range,
                );
            }
        }

        // Cross-module resolution needs the module graph, which is a source
        // root away; phase 2 programs are single-module.
        self.error(
            format!("cannot resolve `{}` in this scope", segments.join("::")),
            range,
        );
        self.add_expr(Expr::Unresolved(segments.join("::")), range)
    }

    /// `for pat in iter { body }`, desugared here rather than carried further.
    ///
    /// ```text
    /// {
    ///   let mut it = <iter>;
    ///   loop {
    ///     match it.next() {
    ///       Step::Yield(rest, pat) => { it = rest; <body> }
    ///       Step::Done => break,
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// Desugaring in the front end means the checker, the reference-counting
    /// plan and the backend need no notion of `for` at all — it is `loop`,
    /// `match` and assignment, each of which already works and is already
    /// tested. The cost is that the loop depends on `Step` being in scope, the
    /// same way Rust's `for` depends on `IntoIterator`.
    fn lower_for(&mut self, e: &ast::ForExpr, range: TextRange) -> ExprId {
        let iter = match e.iterable() {
            Some(i) => self.lower_expr(&i),
            None => self.add_expr(Expr::Missing, range),
        };

        // The whole loop lives in a scope of its own so the state variable
        // cannot collide with anything the body declares.
        self.scopes.push(Vec::new());
        let state = self.declare("$iter".to_string(), true, range);
        let state_pat = self.add_pat(Pat::Bind(state));

        let rest = self.declare("$rest".to_string(), false, range);
        let rest_pat = self.add_pat(Pat::Bind(rest));

        // The arm binds its own scope: the item pattern belongs to the body.
        self.scopes.push(Vec::new());
        let item_pat = match e.pattern() {
            Some(p) => self.lower_pat(&p, false),
            None => self.add_pat(Pat::Wildcard),
        };

        let advance = {
            let target = self.add_expr(Expr::Local(state), range);
            let value = self.add_expr(Expr::Local(rest), range);
            self.add_expr(Expr::Assign { target, value }, range)
        };
        // Inside the loop before the body is lowered, or a `break` in it would
        // be reported as outside a loop.
        self.loop_depth += 1;
        let body = match e.body() {
            Some(b) => self.lower_block(&b),
            None => self.add_expr(Expr::Missing, range),
        };
        self.loop_depth -= 1;
        self.scopes.pop();

        let arm_body = self.add_expr(
            Expr::Block { stmts: vec![Stmt::Expr(advance)], tail: Some(body) },
            range,
        );

        let yield_pat = self.add_pat(Pat::TupleStruct {
            resolution: self.step_case("Yield", range),
            fields: vec![rest_pat, item_pat],
        });
        let done_pat = self.add_pat(Pat::Path(self.step_case("Done", range)));
        let stop = self.add_expr(Expr::Break(None), range);

        let scrutinee = {
            let receiver = self.add_expr(Expr::Local(state), range);
            let next = self.add_expr(
                Expr::Field { base: receiver, name: "next".to_string() },
                range,
            );
            self.add_call(Expr::Call { callee: next, args: Vec::new() }, range)
        };
        let dispatch = self.add_expr(
            Expr::Match {
                scrutinee,
                arms: vec![
                    MatchArm { pat: yield_pat, guard: None, body: arm_body },
                    MatchArm { pat: done_pat, guard: None, body: stop },
                ],
            },
            range,
        );

        let repeat = self.add_expr(Expr::Loop { body: dispatch }, range);
        self.scopes.pop();

        self.add_expr(
            Expr::Block {
                stmts: vec![Stmt::Let { pat: state_pat, init: Some(iter) }],
                tail: Some(repeat),
            },
            range,
        )
    }

    /// `Step::Yield` or `Step::Done`, as the desugaring needs them.
    fn step_case(&self, case: &str, _range: TextRange) -> crate::Resolution {
        // Through the scope as well as the file: `Step` almost always arrives
        // by `import std::core::{Step}` rather than being declared next to the
        // loop that uses it.
        match self
            .map
            .variants_of("Step")
            .chain(self.scope.variants_of("Step"))
            .find(|v| v.name == case)
        {
            Some(v) => crate::Resolution::Variant {
                module: self.map.module.clone().unwrap_or_else(|| crate::ModulePath::new(vec![])),
                type_name: v.type_name.clone(),
                name: v.name.clone(),
            },
            // `for` needs `Step` the way Rust's needs `IntoIterator`. Saying so
            // beats an unresolved-name error pointing at code nobody wrote.
            None => crate::Resolution::Unsupported(
                "`for` needs the `Step` type in scope; import it from `std::core`",
            ),
        }
    }

    /// `with { ledger: h } { .. }` — the labels bound over a region.
    fn lower_installation(
        &mut self,
        row: Option<&ast::RecordExpr>,
        body: Option<&ast::Expr>,
        range: TextRange,
    ) -> ExprId {
        self.scopes.push(Vec::new());
        let outer = self.in_scope.len();

        let mut stmts = Vec::new();
        let mut labels = Vec::new();
        for field in row.map(|r| r.fields().collect::<Vec<_>>()).unwrap_or_default() {
            let value = match field.value() {
                Some(v) => self.lower_expr(&v),
                None => self.add_expr(Expr::Missing, range),
            };
            // Declared after its own initializer, so `with { ledger: ledger }`
            // means the outer one — the same rule every `let` follows.
            let pat = match field.name().and_then(|n| n.ident()) {
                Some(label) => {
                    labels.push(label.clone());
                    let local = self.declare(label.clone(), false, field.syntax().text_range());
                    self.in_scope.push((label, local));
                    self.add_pat(Pat::Bind(local))
                }
                None => self.add_pat(Pat::Wildcard),
            };
            stmts.push(Stmt::Let { pat, init: Some(value) });
        }

        let tail = body.map(|b| self.lower_expr(b));
        self.scopes.pop();
        self.in_scope.truncate(outer);

        let block = self.add_expr(Expr::Block { stmts, tail }, range);
        self.body.installs.insert(block, labels);
        block
    }

    /// The labelled expressions of a record literal.
    fn lower_record_fields(&mut self, e: &ast::RecordExpr) -> Vec<(String, ExprId)> {
        e.fields()
            .filter_map(|f| {
                let label = f.name()?.ident()?;
                let range = f.syntax().text_range();
                let value = match f.value() {
                    Some(v) => self.lower_expr(&v),
                    None => self.add_expr(Expr::Missing, range),
                };
                Some((label, value))
            })
            .collect()
    }

    /// `(a, b) => body`.
    ///
    /// Captures are found by scanning the expressions the body created rather
    /// than by walking its tree. Lowering is depth-first and the arena is
    /// append-only, so every expression belonging to this lambda — including
    /// ones inside a lambda nested in it — sits above the mark taken here. That
    /// makes nested capture fall out for free: an inner lambda's free variable
    /// is still free in the outer one, and one scan finds both.
    fn lower_lambda(&mut self, e: &ast::LambdaExpr, range: TextRange) -> ExprId {
        self.lower_lambda_named(e, None, range)
    }

    /// As `lower_lambda`, with the name the lambda was bound to if it has one.
    fn lower_lambda_named(
        &mut self,
        e: &ast::LambdaExpr,
        name: Option<String>,
        range: TextRange,
    ) -> ExprId {
        let local_mark = self.body.locals.len();
        let expr_mark = self.body.exprs.len();

        self.scopes.push(Vec::new());
        self.lambdas.push(local_mark);
        self.lambda_names.push(name);

        let params: Vec<PatId> = e
            .params()
            .map(|list| {
                list.params()
                    .map(|p| {
                        let range = p.syntax().text_range();
                        match p.name().and_then(|n| n.ident()) {
                            Some(name) => {
                                let local = self.declare(name, false, range);
                                self.add_pat(Pat::Bind(local))
                            }
                            None => self.add_pat(Pat::Wildcard),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // A lambda is its own function as far as `return` and `break` are
        // concerned: `return` leaves the lambda, and a `break` inside one
        // cannot target a loop outside it.
        let outer_loops = std::mem::take(&mut self.loop_depth);
        let body = match e.body() {
            Some(b) => self.lower_expr(&b),
            None => self.add_expr(Expr::Missing, range),
        };
        self.loop_depth = outer_loops;

        self.lambdas.pop();
        self.lambda_names.pop();
        self.scopes.pop();

        let mut captures: Vec<LocalId> = Vec::new();
        for expr in &self.body.exprs[expr_mark..] {
            if let Expr::Local(local) = expr {
                if local.index() < local_mark && !captures.contains(local) {
                    captures.push(*local);
                }
            }
        }

        self.add_expr(Expr::Lambda { params, body, captures }, range)
    }

    /// Whether `name` is the name of the lambda currently being lowered.
    fn is_own_lambda(&self, name: &str) -> bool {
        matches!(self.lambda_names.last(), Some(Some(own)) if own == name)
    }

    /// Whether `name` is the name of a lambda further out than the innermost.
    fn is_enclosing_lambda(&self, name: &str) -> bool {
        let outer = self.lambda_names.len().saturating_sub(1);
        self.lambda_names[..outer].iter().any(|n| n.as_deref() == Some(name))
    }

    /// Whether `local` belongs to a scope outside the lambda being lowered.
    fn is_captured(&self, local: LocalId) -> bool {
        self.lambdas.last().is_some_and(|mark| local.index() < *mark)
    }

    fn check_assignable(&mut self, target: ExprId, range: TextRange) {
        match self.body.exprs[target.index()] {
            Expr::Local(local) => {
                // A capture is a copy, so writing to it would change the
                // closure's own copy and nothing else — silently, which is the
                // worst way for it to behave. Saying so is better than either
                // lying or quietly promoting the capture to a shared cell.
                if self.is_captured(local) {
                    let name = self.body.locals[local.index()].name.clone();
                    self.error(
                        format!(
                            "cannot assign to `{name}` inside a closure: it is captured by \
                             value, so the assignment would not be visible outside"
                        ),
                        range,
                    );
                    return;
                }
                let local = &self.body.locals[local.index()];
                if !local.is_mut {
                    let name = local.name.clone();
                    self.error(
                        format!("cannot assign to `{name}`, which is not declared `mut`"),
                        range,
                    );
                }
            }
            Expr::Field { .. } => {}
            Expr::Missing => {}
            _ => self.error("this expression cannot be assigned to", range),
        }
    }

    fn lower_binary(&mut self, e: &ast::BinExpr, range: TextRange) -> ExprId {
        let op = e.op().and_then(|t| BinOp::from_token(t.text()));
        let lhs = match e.lhs() {
            Some(l) => self.lower_expr(&l),
            None => self.add_expr(Expr::Missing, range),
        };
        let rhs = match e.rhs() {
            Some(r) => self.lower_expr(&r),
            None => self.add_expr(Expr::Missing, range),
        };
        match op {
            Some(op) => self.add_expr(Expr::Binary { op, lhs, rhs }, range),
            None => self.add_expr(Expr::Missing, range),
        }
    }

    /// `x |> f(a)` becomes `f(x, a)`; `x |> f(_, a)` becomes `f(x, a)` with the
    /// placeholder taking the piped value instead of the leading position.
    ///
    /// The pipeline exists only in the syntax; nothing downstream needs to know
    /// a call was written this way.
    fn lower_pipe(&mut self, e: &ast::PipeExpr, range: TextRange) -> ExprId {
        let piped = match e.lhs() {
            Some(l) => self.lower_expr(&l),
            None => self.add_expr(Expr::Missing, range),
        };

        let Some(rhs) = e.rhs() else {
            return self.add_expr(Expr::Missing, range);
        };

        match &rhs {
            ast::Expr::Call(call) => {
                let callee = match call.callee() {
                    Some(c) => self.lower_expr(&c),
                    None => self.add_expr(Expr::Missing, range),
                };

                let written: Vec<ast::Expr> =
                    call.args().map(|l| l.args().collect()).unwrap_or_default();
                let placeholders: Vec<usize> = written
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| matches!(a, ast::Expr::Placeholder(_)))
                    .map(|(i, _)| i)
                    .collect();

                if placeholders.len() > 1 {
                    self.error(
                        "a pipeline stage may use `_` at most once".to_string(),
                        rhs.syntax().text_range(),
                    );
                }

                let mut args = Vec::with_capacity(written.len() + 1);
                if placeholders.is_empty() {
                    args.push(piped);
                    for a in &written {
                        let id = self.lower_expr(a);
                        args.push(id);
                    }
                } else {
                    let first = placeholders[0];
                    for (i, a) in written.iter().enumerate() {
                        if i == first {
                            args.push(piped);
                        } else {
                            let id = self.lower_expr(a);
                            args.push(id);
                        }
                    }
                }
                self.add_call(Expr::Call { callee, args }, range)
            }
            // `x |> f` is `f(x)`.
            _ => {
                let callee = self.lower_expr(&rhs);
                self.add_call(Expr::Call { callee, args: vec![piped] }, range)
            }
        }
    }

    fn lower_match(&mut self, e: &ast::MatchExpr, range: TextRange) -> ExprId {
        let scrutinee = match e.scrutinee() {
            Some(s) => self.lower_expr(&s),
            None => self.add_expr(Expr::Missing, range),
        };

        let arms = e
            .arms()
            .map(|arm| {
                // Each arm's bindings are scoped to that arm.
                self.scopes.push(Vec::new());
                let pat = match arm.pat() {
                    Some(p) => self.lower_pat(&p, false),
                    None => self.add_pat(Pat::Missing),
                };
                let guard = arm.guard().and_then(|g| g.condition()).map(|c| self.lower_expr(&c));
                let body = match arm.body() {
                    Some(b) => self.lower_expr(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.scopes.pop();
                MatchArm { pat, guard, body }
            })
            .collect();

        self.add_expr(Expr::Match { scrutinee, arms }, range)
    }
}

fn literal_of(node: &khora_syntax::SyntaxNode) -> Option<Literal> {
    use khora_syntax::SyntaxKind::*;
    let token = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind().is_literal())?;
    let text = token.text().to_string();
    let lit = match token.kind() {
        INT_LIT => Literal::Int(text),
        FLOAT_LIT => Literal::Float(text),
        STRING_LIT => Literal::Str(text.trim_matches('"').to_string()),
        TRUE_KW => Literal::Bool(true),
        FALSE_KW => Literal::Bool(false),
        _ => return None,
    };
    Some(lit)
}
