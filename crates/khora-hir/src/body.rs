//! Function bodies, lowered.
//!
//! Item collection answers "what exists and where"; this answers "what does it
//! do".
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
//! Not here, deliberately. Exhaustiveness and reachability are computed by
//! Maranget's usefulness algorithm over a *pattern matrix*, and the decision
//! tree is compiled from that same matrix — so building the tree first would
//! mean reconstructing the matrix to check it. HIR keeps the arms as written
//! and the tree is compiled later, nearer codegen, which is what rustc does and
//! why the two consumers do not fight over one shape.
//!
//! # Scope
//!
//! Every expression form in the grammar lowers, so there is no
//! `Expr::Unsupported`. [`Expr::Missing`] marks a hole a *parse* error left,
//! which is a different thing and always will be.

use khora_db::{Db, SourceFile};
use khora_syntax::ast::{self, AstNode};
use text_size::{TextRange, TextSize};

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
    Let {
        pat: PatId,
        /// The annotation, when one was written. Checked against the
        /// initializer rather than ignored — see [`TypeRef`].
        ty: Option<TypeRef>,
        init: Option<ExprId>,
    },
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
    Tuple(Vec<ExprId>),
    /// The value of a `${..}` hole, as text.
    ///
    /// **Written by the interpolation desugaring and by nothing else.** It is
    /// a call to `Show::show` that the compiler makes on the author's behalf,
    /// which is why it is a node rather than an ordinary method call: a method
    /// call resolves through the names in scope, and interpolating a value
    /// should not require importing the trait that prints it. The hole is the
    /// use; `Show` is not named in the source and asking for it to be would
    /// reintroduce exactly the import trap that trait bounds just came out of
    /// — a lint calling the import unused, and removing it breaking the build.
    ///
    /// Every hole is wrapped, including one that is already a `String`:
    /// `impl Show for String` is the identity, so uniformity costs a call the
    /// optimizer removes rather than a check the desugaring cannot make. It
    /// runs before types exist and has no way to know.
    ///
    /// `"n = ${n}"` where `n: Int` used to be `string concatenation: expected
    /// String, found Int`, so every number in a message needed an explicit
    /// `Int::to_string` — which three documented examples in the guide quietly
    /// left out, and which the first person to print a table called the single
    /// biggest tax in the language.
    Shown(ExprId),
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
    /// `f()! catch { .. }` — handles part of the error row.
    ///
    /// The arms are patterns over error *values*, and the set of error types
    /// their constructors belong to is exactly what the row loses: an error
    /// type nobody matched still leaves through the `!`. So this is not sugar
    /// for a `match` on a `Result`; the branch it compiles to is the one `!`
    /// already emits, with the handled types diverted to the arms instead of
    /// returned onward.
    Catch {
        inner: ExprId,
        arms: Vec<MatchArm>,
    },
    /// The closure currently executing, inside its own body.
    ///
    /// `let go = fn n => .. go(n - 1) ..` reads as a capture and would be one:
    /// the closure would hold a counted reference to itself, which is a cycle,
    /// and reference counting does not collect cycles. It need not be a
    /// reference at all — a lifted lambda already receives its own closure
    /// object as its first argument, so self-recursion goes through that.
    ///
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
        /// `{ ..old, field: value }` — where the fields nobody named come
        /// from.
        ///
        /// A new record either way: this says which values are carried over,
        /// not that anything is written in place. `old` is unchanged and
        /// still whatever it was.
        base: Option<ExprId>,
    },
    /// `(x) => x + 1`.
    ///
    /// The body lives in the *same* arena as the enclosing function, which is
    /// what lets a captured local keep its identity: `captures` names locals
    /// declared outside the lambda, and the type map and reference-counting
    /// plan cover the lambda's expressions without a second pass over a second
    /// body.
    Lambda {
        /// Capabilities this lambda names and cannot resolve lexically, so it
        /// requires them instead — the label and the binding that holds it.
        ///
        /// **`nursery(fn () => nursery.adopt(f))` used to be the one thing a
        /// lambda could not do.** `capability-passing.md`'s rule is "resolve a
        /// capability lexically if you can and require it if you cannot", and
        /// a lambda could already *require* one it never mentioned — a call
        /// inside it carries the row — but not one it named, because a bare
        /// name is resolved by ordinary lookup and there was nothing to find.
        /// Every group of children needed a named top-level function to hold
        /// it, and three of one program's existed for no other reason.
        ///
        /// The objection recorded against fixing it was that inventing a
        /// binding for any unresolved name turns a typo into a requirement and
        /// loses "cannot find `x` in this scope". The binding is invented here
        /// and the *error* is deferred: the checker knows the lambda's expected
        /// row, and a label that row does not name is reported at the span
        /// this pattern carries. The message moves rather than disappearing.
        evidence: Vec<(String, PatId)>,
        params: Vec<PatId>,
        /// What each parameter was *annotated* with, positionally, and `None`
        /// where it was not.
        ///
        /// Carried for the same reason [`TypeRef`] exists at all: without it
        /// `fn (s: String) => s + "b"` was lowered as though the annotation had
        /// never been written, the parameter got a bare inference variable, and
        /// `+` defaulted the variable to `Int` and reported `expected Int,
        /// found String` about a line that says `String` on its face. An
        /// annotation that is only a comment is worse than no annotation,
        /// because it is believed.
        param_types: Vec<Option<TypeRef>>,
        body: ExprId,
        /// Locals from an enclosing scope that the body reads, in first-use
        /// order. Captured **by value**: the closure takes its own reference.
        captures: Vec<LocalId>,
    },
    Unit,
}

/// A type as *written*, kept for the places a body needs one.
///
/// A `let` annotation is the only one so far, and until this existed it was
/// parsed and then dropped on the floor — `let x: Bool = 5` compiled clean,
/// which is errata 36 and the reason this is here.
///
/// An echo of `ast::Type` rather than a `khora_types::Type`, because HIR sits
/// below the type system and cannot name one. It is deliberately *not* a
/// second interpreter: `khora_types` resolves this through the same
/// `named_type` that resolves the syntax, so a name means one thing however it
/// arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// `Map<String, Int>`, `Int`, `T`, `T::Item`.
    Named { name: String, args: Vec<TypeRef> },
    Tuple(Vec<TypeRef>),
    Unit,
    /// A bare integer in type position: the `3` in `Matrix<3, 4>`.
    Const(i64),
    /// A shape this echo does not carry — a function type with effect
    /// clauses, so far. Checked as `Unknown`, which is to say not checked,
    /// which is what every annotation used to get.
    Opaque,
}

impl TypeRef {
    /// Reads one off the syntax, or `Opaque` for a shape not carried yet.
    pub fn of_syntax(ty: &ast::Type) -> TypeRef {
        match ty {
            ast::Type::Unit(_) => TypeRef::Unit,
            ast::Type::Paren(p) => {
                p.inner().as_ref().map_or(TypeRef::Opaque, TypeRef::of_syntax)
            }
            ast::Type::Literal(l) => l.value().map_or(TypeRef::Opaque, TypeRef::Const),
            ast::Type::Tuple(t) => {
                TypeRef::Tuple(t.elements().map(|e| TypeRef::of_syntax(&e)).collect())
            }
            ast::Type::Path(p) => {
                // A bare `'r` is one token with no `Path` under it. Reading it
                // as the empty name is errata 30, and once was enough.
                if let Some(row_var) = p.row_var() {
                    return TypeRef::Named { name: row_var.text().to_string(), args: Vec::new() };
                }
                TypeRef::Named {
                    name: p.path().map(|p| p.text_path()).unwrap_or_default(),
                    args: p
                        .type_args()
                        .map(|a| a.args().map(|t| TypeRef::of_syntax(&t)).collect())
                        .unwrap_or_default(),
                }
            }
            // A function type carries `with` and `raises` rows, and an echo of
            // those is a second row interpreter. Left opaque until something
            // needs it, which is honest rather than half-right.
            ast::Type::Fn(_)
            | ast::Type::Record(_)
            | ast::Type::Union(_)
            | ast::Type::Variant(_)
            | ast::Type::Forall(_) => TypeRef::Opaque,
        }
    }
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
    /// The same for patterns, so a diagnostic about one can point at it.
    ///
    /// Added because a tuple pattern against a value that is not a tuple was
    /// accepted in silence -- the bindings got `Unknown` and the program was
    /// refused later, by the code generator, against a line with nothing wrong
    /// with it. Saying so needs somewhere to say it.
    pat_ranges: Vec<TextRange>,
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
    /// How many locals existed when each lambda started.
    ///
    /// A local below the mark was declared outside the lambda, which is what
    /// "captured" means. The capture scan here uses it directly; the checker
    /// needs it too, because a capability used *implicitly* is captured on the
    /// same terms and only the checker knows which labels those are.
    pub lambda_marks: std::collections::HashMap<ExprId, usize>,
    /// Which binding supplies each capability label at each call site.
    ///
    /// Recorded here because it can only be answered *while* lowering, when
    /// the scope stack is live. Resolving it later by name is wrong the moment
    /// two `with` blocks in one function bind the same label: they are sibling
    /// scopes, and the last declaration is not the one in scope at the first
    /// call.
    pub capabilities: std::collections::HashMap<ExprId, Vec<(String, LocalId)>>,
    /// Bindings installed by *type* rather than by label: `with MyDatabase`.
    ///
    /// The name they are bound under is the path that was written, which no
    /// callee asks for by name -- so a lookup in [`Body::capability_at`] will
    /// never find one, and that is deliberate. What satisfies a requirement
    /// from one of these is its **type**, which HIR does not have.
    ///
    /// So this records only *which* locals are open to that matching, and the
    /// checker does the matching. `docs/design/capability-installation.md`.
    pub by_type: Vec<LocalId>,
    pub root: Option<ExprId>,
    pub errors: Vec<HirError>,
}

impl Body {
    /// Points every range in this body at `at`.
    ///
    /// For a body the compiler wrote rather than read. See the note in
    /// [`bodies`]: the ranges it was lowered with belong to a generated string
    /// and mean nothing in the file the reader has open.
    fn blame(&mut self, at: TextRange) {
        self.expr_ranges.iter_mut().for_each(|range| *range = at);
        self.pat_ranges.iter_mut().for_each(|range| *range = at);
        self.locals.iter_mut().for_each(|local| local.range = at);
        self.errors.iter_mut().for_each(|error| error.range = at);
    }

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

    /// The bindings in scope at `at` that were installed by type, innermost
    /// first.
    ///
    /// Innermost first because that is the order the checker should try them
    /// in: an inner `with` shadows an outer one, the same as every other
    /// binding.
    pub fn by_type_at(&self, at: ExprId) -> Vec<LocalId> {
        let Some(visible) = self.capabilities.get(&at) else { return Vec::new() };
        visible
            .iter()
            .rev()
            .filter(|(_, local)| self.by_type.contains(local))
            .map(|(_, local)| *local)
            .collect()
    }

    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.index()]
    }

    pub fn range(&self, id: ExprId) -> TextRange {
        self.expr_ranges[id.index()]
    }

    /// Where a pattern was written.
    pub fn pat_range(&self, id: PatId) -> TextRange {
        self.pat_ranges[id.index()]
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

    // `with Mock { .. }` is `with { <Mock's bindings> } { .. }`, so the
    // bindings have to be reachable from inside a body. Collected once here
    // rather than searched for at each mention.
    let contexts: Vec<(String, ast::ContextDecl)> = parse
        .source_file()
        .decls()
        .filter_map(|d| match d {
            ast::Decl::Context(c) => Some((c.name()?.ident()?, c)),
            _ => None,
        })
        .collect();

    // A `const` is a named expression, lowered wherever it is mentioned.
    // Rust's `const` rather than its `static`, and
    // for the same reasons — there is no initialization order to get wrong, no
    // global to release at exit, and no shared state for two fibers to reach.
    //
    // Collected once here rather than searched for at each mention, exactly as
    // the contexts above are.
    let constants: Vec<(String, ast::ConstDecl)> = parse
        .source_file()
        .decls()
        .filter_map(|d| match d {
            ast::Decl::Const(c) => match c.pat()? {
                ast::Pat::Ident(p) => Some((p.name()?.ident()?, c)),
                _ => None,
            },
            _ => None,
        })
        .collect();

    let mut out: Vec<(String, Body)> = parse
        .source_file()
        .decls()
        .flat_map(|decl| match decl {
            ast::Decl::Fn(f) => {
                let lowered = (|| {
                    let name = f.name()?.ident()?;
                    let body = f.body()?;
                    Some((name, lower_function(map, scope, &contexts, &constants, &f, &body)))
                })();
                lowered.into_iter().collect::<Vec<_>>()
            }
            // A trait's functions are lowered too: one with a body is a default
            // implementation, and it has to be checked like any other.
            ast::Decl::Trait(t) => {
                let owner = t.name().and_then(|n| n.ident()).unwrap_or_default();
                methods(map, scope, &contexts, &constants, &owner, t.functions())
            }
            ast::Decl::Impl(i) => methods(map, scope, &contexts, &constants, &impl_key(&i), i.functions()),
            _ => Vec::new(),
        })
        .collect();

    // The impls a `derive` clause expanded to. Lowered here, with this file's
    // items and scope, so that a derived body resolves `Ordering::Less` and
    // `self.x.eq(..)` through exactly the same machinery a written one does —
    // which is the point of expanding to source in the first place.
    //
    // Every range in them is then rewritten to the `derive` that asked for
    // them. The offsets a generated body carries are offsets into a string
    // that is not this file, so a diagnostic holding one would underline an
    // arbitrary span of the source, or a span past its end. The `derive` is
    // both a safe place to point and the right one: it is the line whose
    // author has something to change.
    for (imp, from) in crate::derive::derived(db, file).declarations() {
        for (key, mut body) in
            methods(map, scope, &contexts, &constants, &impl_key(&imp), imp.functions())
        {
            body.blame(from.at);
            out.push((key, body));
        }
    }

    // A test's body is a function body like any other: it is checked, it is
    // reference counted, and `khora test` compiles it. Numbered by position
    // rather than by name, because nothing stops two tests sharing a name.
    for (index, decl) in parse.source_file().decls().enumerate() {
        // A bench's body is lowered identically. What it is *for* differs --
        // one is checked once, the other timed many times -- and nothing about
        // that is visible until the runner has it.
        let body = match &decl {
            ast::Decl::Test(t) => t.body(),
            ast::Decl::Bench(b) => b.body(),
            _ => continue,
        };
        let Some(block) = body else { continue };
        out.push((crate::test_key(index), lower_test(map, scope, &contexts, &constants, &block)));
    }
    out
}

/// Lowers a test's block. It takes no parameters and returns nothing; what it
/// can do is fail, which is what `assert` is for.
fn lower_test(
    map: &crate::ItemMap,
    scope: &crate::FileScope,
    contexts: &[(String, ast::ContextDecl)],
    constants: &[(String, ast::ConstDecl)],
    block: &ast::Block,
) -> Body {
    let mut ctx = Ctx {
        range_shift: 0,
        body: Body::default(),
        scopes: vec![Vec::new()],
        map,
        scope,
        contexts,
        constants,
        expanding: Vec::new(),
        generics: vec!["Self".to_string()],
        loop_depth: 0,
        in_scope: Vec::new(),
        lambdas: Vec::new(),
        lambda_names: Vec::new(),
        lambda_evidence: Vec::new(),
    };
    let root = ctx.lower_expr(&ast::Expr::Block(block.clone()));
    ctx.body.root = Some(root);
    ctx.body
}

/// A record literal's fields, or none when the literal is missing.
fn fields_of(row: Option<&ast::RecordExpr>) -> Vec<ast::RecordExprField> {
    row.map(|r| r.fields().collect()).unwrap_or_default()
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
    contexts: &[(String, ast::ContextDecl)],
    constants: &[(String, ast::ConstDecl)],
    owner: &str,
    functions: impl Iterator<Item = ast::FnDecl>,
) -> Vec<(String, Body)> {
    functions
        .filter_map(|f| {
            let name = f.name()?.ident()?;
            let body = f.body()?;
            Some((
                format!("{owner}::{name}"),
                lower_function(map, scope, contexts, constants, &f, &body),
            ))
        })
        .collect()
}

fn lower_function(
    map: &crate::ItemMap,
    scope: &crate::FileScope,
    contexts: &[(String, ast::ContextDecl)],
    constants: &[(String, ast::ConstDecl)],
    decl: &ast::FnDecl,
    block: &ast::Block,
) -> Body {
    let mut generics: Vec<String> = vec!["Self".to_string()];
    if let Some(params) = decl.type_params() {
        generics.extend(params.params().filter_map(|p| p.name().and_then(|n| n.ident())));
    }

    let mut ctx = Ctx {
        range_shift: 0,
        body: Body::default(),
        scopes: vec![Vec::new()],
        map,
        scope,
        contexts,
        constants,
        expanding: Vec::new(),
        generics,
        loop_depth: 0,
        in_scope: Vec::new(),
        lambdas: Vec::new(),
        lambda_names: Vec::new(),
        lambda_evidence: Vec::new(),
    };

    if let Some(params) = decl.params() {
        for param in params.params() {
            let range = param.syntax().text_range();
            let pat = match param.name().and_then(|n| n.ident()) {
                Some(name) => {
                    let local = ctx.declare(name, false, range);
                    ctx.add_pat(Pat::Bind(local), range)
                }
                None => ctx.add_pat(Pat::Wildcard, range),
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
        let pat = ctx.add_pat(Pat::Bind(local), decl.syntax().text_range());
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
    /// The `context` declarations of this file, by name. `with Mock { .. }`
    /// installs the bindings of the one it names.
    contexts: &'a [(String, ast::ContextDecl)],
    /// The `let` declarations of this file, by name. A mention of one is
    /// lowered into the initializer, so a module-level `let` is a constant
    /// rather than a global.
    constants: &'a [(String, ast::ConstDecl)],
    /// How far the tree being lowered sits from the start of the file.
    ///
    /// Zero for the file's own tree. An interpolated `${..}` is parsed as a
    /// little source file of its own, so its ranges start again at zero and
    /// have to be moved back to where the text actually is.
    range_shift: u32,
    /// The constants currently being expanded, innermost last.
    ///
    /// `let a = b; let b = a;` is a cycle, and inlining is what turns a cycle
    /// into a stack overflow rather than a diagnostic. Naming the loop is
    /// cheaper than discovering it.
    expanding: Vec<String>,
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
    /// The capability labels each open lambda has named and could not resolve,
    /// one list per lambda on the stack. See `Expr::Lambda`'s `evidence`.
    lambda_evidence: Vec<Vec<(String, PatId)>>,
}

// One module per lowering responsibility. Rust lets an inherent impl be split
// across modules of one crate, so each opens `impl<'a> Ctx<'a>` again; the
// arena, the scope stack and the helpers that touch both stay here.
// Roadmap 9.6.4.
mod context;
mod desugar;
mod exprs;
mod lambda;
mod patterns;

impl<'a> Ctx<'a> {
    fn add_expr(&mut self, expr: Expr, range: TextRange) -> ExprId {
        self.body.exprs.push(expr);
        self.body.expr_ranges.push(self.shifted(range));
        ExprId((self.body.exprs.len() - 1) as u32)
    }

    /// Moves a range from the tree it was measured in to the file it belongs to.
    ///
    /// Zero everywhere except inside `${..}`, whose expression is parsed on its
    /// own — see [`Ctx::lower_interpolation`]. Without this a diagnostic about
    /// an interpolated expression points at the top of the file.
    fn shifted(&self, range: TextRange) -> TextRange {
        match self.range_shift {
            0 => range,
            shift => range + TextSize::from(shift),
        }
    }

    /// Adds a call, remembering which bindings could supply its capabilities.
    ///
    /// Which ones it actually needs is the callee's row, which is not known
    /// here; what is known, and only here, is what is in scope.
    fn add_call(&mut self, expr: Expr, range: TextRange) -> ExprId {
        let callee = match &expr {
            Expr::Call { callee, .. } => Some(*callee),
            _ => None,
        };
        let id = self.add_expr(expr, range);
        if !self.in_scope.is_empty() {
            // Innermost last, so a later duplicate label shadows an earlier one.
            let mut visible: Vec<(String, LocalId)> = Vec::new();
            for (label, local) in &self.in_scope {
                visible.retain(|(l, _)| l != label);
                visible.push((label.clone(), *local));
            }
            // Filed under the call *and* its callee, because the two halves of
            // the compiler reach for different ones: code generation has the
            // call, and the checker records a demand against the callee, which
            // is what carries the signature. Errata 28 is the same pair
            // disagreeing about which key to use; one entry under each is
            // cheaper than remembering.
            if let Some(callee) = callee {
                self.body.capabilities.insert(callee, visible.clone());
            }
            self.body.capabilities.insert(id, visible);
        }
        id
    }

    fn add_pat(&mut self, pat: Pat, range: TextRange) -> PatId {
        self.body.pats.push(pat);
        self.body.pat_ranges.push(self.shifted(range));
        PatId((self.body.pats.len() - 1) as u32)
    }

    fn declare(&mut self, name: String, is_mut: bool, range: TextRange) -> LocalId {
        let range = self.shifted(range);
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

    /// The module that *declares* `type_name`, not the one mentioning it.
    ///
    /// A constructor resolution carries a module, and it used to carry this
    /// file's — so `List::Nil` written in `std::ai` claimed `List` was declared
    /// in `std::ai`. Nothing read it closely enough to mind until a type became
    /// an identity rather than a name, at which point every imported
    /// constructor resolved to a type that did not exist. Errata 46.
    ///
    /// A declaration here wins over an import, which is what shadowing means.
    fn home_of_type(&self, type_name: &str) -> crate::ModulePath {
        let here = || self.map.module.clone().unwrap_or_else(|| crate::ModulePath::new(vec![]));
        if self.map.item(type_name).is_some() {
            return here();
        }
        self.scope
            .origins
            .iter()
            .find(|o| o.local == type_name)
            .map(|o| o.module.clone())
            .unwrap_or_else(here)
    }

    fn error(&mut self, message: impl Into<String>, range: TextRange) {
        let range = self.shifted(range);
        self.body.errors.push(HirError { message: message.into(), range });
    }
}


/// One piece of an interpolated string, and where in the literal it began.
struct Piece {
    text: String,
    /// Bytes from the start of the literal's *body*, past the opening quote.
    at: u32,
}

enum Part {
    Text(Piece),
    Hole(Piece),
}

/// Whether a literal has a `${` that is not escaped.
///
/// Scanning for the escape matters: `"\\${x}"` is a literal dollar followed by
/// a brace, which is what somebody writing a shell snippet or a JSON template
/// into a Khora string means.
fn has_interpolation(text: &str) -> bool {
    let body = strip_quotes(text);
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'$' if bytes.get(i + 1) == Some(&b'{') => return true,
            _ => i += 1,
        }
    }
    false
}

/// A literal's body, whichever delimiter it used.
///
/// **One funnel, deliberately.** `has_interpolation`, the `${..}` splitter and
/// `unescape` all start here, so a backtick literal reaches every one of them
/// as an ordinary body and none of them had to learn a second spelling.
fn strip_quotes(text: &str) -> &str {
    if text.starts_with('`') {
        let inner = text.strip_prefix('`').unwrap_or(text);
        return inner.strip_suffix('`').unwrap_or(inner);
    }
    let inner = text.strip_prefix('"').unwrap_or(text);
    inner.strip_suffix('"').unwrap_or(inner)
}

/// A backtick literal's body, with the source's indentation taken off.
///
/// A multiline literal is written inside a function, indented to match the
/// code around it, and those spaces are not part of the string. Java's text
/// blocks, Swift and `indoc` all strip them; so does this.
///
/// **The common prefix, measured over the non-blank lines**, so relative
/// indentation inside the string survives — a nested `create table (` body
/// stays nested. A blank line contributes nothing to the measurement and comes
/// out empty rather than as leftover spaces.
///
/// A first line that is empty is dropped, so the delimiter can sit on its own
/// line where it reads best, and so can the closing one.
pub(crate) fn dedent(body: &str) -> String {
    let trimmed = trim_close(trim_open(body));
    let common = common_indent(trimmed);
    let out: Vec<String> =
        trimmed.split('\n').map(|l| strip_indent(l, common)).collect();
    out.join("\n")
}

/// A leading blank line removed, so an opening delimiter can sit on its own.
///
/// The newline after the backtick is punctuation, not content -- which is what
/// lets the literal be written where it reads best.
pub(crate) fn trim_open(text: &str) -> &str {
    match text.find('\n') {
        Some(at) if text[..at].trim().is_empty() => &text[at + 1..],
        _ => text,
    }
}

/// A trailing blank line removed, so a closing delimiter can sit on its own.
pub(crate) fn trim_close(text: &str) -> &str {
    match text.rfind('\n') {
        Some(at) if text[at + 1..].trim().is_empty() => &text[..at],
        _ => text,
    }
}

/// The indentation every non-blank line shares.
///
/// A blank line contributes nothing: it has no content to be indented, and
/// counting its zero would make every literal with a paragraph break in it
/// strip nothing at all.
pub(crate) fn common_indent(body: &str) -> usize {
    body.split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0)
}

/// One line with `common` leading bytes removed, or emptied if it is blank.
///
/// A line shorter than the common indent can only be one made of spaces, and
/// it comes out empty rather than panicking on the slice.
pub(crate) fn strip_indent(line: &str, common: usize) -> String {
    if line.trim().is_empty() {
        return String::new();
    }
    // A text piece from `split_interpolation` may begin mid-line -- everything
    // after a `${..}` hole -- and that fragment has no indentation to take
    // off. Only a line that actually starts with the shared prefix loses it.
    let mut out = String::with_capacity(line.len());
    for (i, part) in line.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if part.len() >= common && part.chars().take(common).all(|c| c == ' ') {
            out.push_str(&part[common..]);
        } else {
            out.push_str(part);
        }
    }
    out
}

/// Splits a literal's body into the text around its `${..}` holes.
///
/// **Braces are counted, and strings inside are skipped**, so
/// `${f("}", x)}` and `${g({ a: 1 })}` both end where they should rather than
/// at the first `}`. An unclosed hole runs to the end of the literal and is
/// reported by `lower_fragment`, which cannot parse it.
fn split_interpolation(body: &str) -> Vec<Part> {
    let bytes = body.as_bytes();
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut text_at = 0u32;
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            text.push_str(&body[i..i + 2]);
            i += 2;
            continue;
        }
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'{') {
            if !text.is_empty() {
                parts.push(Part::Text(Piece { text: std::mem::take(&mut text), at: text_at }));
            }
            let start = i + 2;
            let mut depth = 1usize;
            let mut j = start;
            let mut quoted = false;
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' if quoted => j += 1,
                    b'"' => quoted = !quoted,
                    b'{' if !quoted => depth += 1,
                    b'}' if !quoted => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            let end = j.min(bytes.len());
            parts.push(Part::Hole(Piece {
                text: body[start..end].to_string(),
                at: start as u32,
            }));
            i = (end + 1).min(bytes.len());
            text_at = i as u32;
            continue;
        }
        text.push_str(&body[i..i + 1]);
        i += 1;
    }
    if !text.is_empty() {
        parts.push(Part::Text(Piece { text, at: text_at }));
    }
    parts
}

/// [`unescape`] for a body whose quotes are already off.
fn unescape_body(inner: &str) -> String {
    unescape_inner(inner)
}

/// A string literal's actual bytes.
///
/// `"a\nb"` is three characters and a newline, not four characters and a
/// backslash. Nothing did this until an HTTP response went out with a literal
/// `\r\n` in it, four bytes where two were meant, and a client that read the
/// status line as the whole message.
///
/// The set is the small one every language has, and an unrecognised escape
/// keeps its backslash: `\d` stays `\d`, which is what a regular expression
/// written in a string wants and what the alternative — silently dropping the
/// backslash — is worst at.
///
/// Only the outermost pair of quotes is removed. `trim_matches` took them all,
/// so `""""` lost more than it should have.
fn unescape(text: &str) -> String {
    let inner = strip_quotes(text);
    if text.starts_with('`') {
        return unescape_inner(&dedent(inner));
    }
    unescape_inner(inner)
}

fn unescape_inner(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('`') => out.push('`'),
            // A literal dollar, so a template for another tool still fits in a
            // Khora string now that `${` means something.
            Some('$') => out.push('$'),
            // `\u{1F600}`. The parser has already refused a malformed one, so
            // what is left here is either well-formed or a value that is not a
            // character -- a lone surrogate, or past the last code point --
            // which becomes the replacement character rather than nothing.
            Some('u') => out.push(unicode_escape(&mut chars).unwrap_or('\u{FFFD}')),
            // A line continuation. The newline goes, and so does the
            // indentation that follows it -- otherwise a message written over
            // three lines of a nested block arrives with the block's indent in
            // the middle of a sentence, which is what this was doing silently
            // before the escape existed.
            Some('\n') | Some('\r') => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            // Anything else. The parser reports it, so this never runs for a
            // program that compiles; keeping the two characters is what makes
            // the rest of the string still readable in the error's neighbours.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            // A trailing backslash. The lexer should not produce one, and
            // keeping it beats losing it.
            None => out.push('\\'),
        }
    }
    out
}

/// The character in `{..}` after a `\u`, if it is one.
fn unicode_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<char> {
    if chars.peek() != Some(&'{') {
        return None;
    }
    chars.next();
    let mut value: u32 = 0;
    let mut digits = 0;
    while let Some(c) = chars.peek() {
        let Some(digit) = c.to_digit(16) else { break };
        value = value * 16 + digit;
        digits += 1;
        chars.next();
    }
    if digits == 0 || chars.peek() != Some(&'}') {
        return None;
    }
    chars.next();
    char::from_u32(value)
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
        STRING_LIT => Literal::Str(unescape(&text)),
        // Interpolated literals do not come through here -- `lower_expr`
        // catches them first -- so the dedent for those is in `parts_of`.
        TRUE_KW => Literal::Bool(true),
        FALSE_KW => Literal::Bool(false),
        _ => return None,
    };
    Some(lit)
}
