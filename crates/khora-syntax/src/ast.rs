//! Typed view over the CST.
//!
//! Every wrapper is a newtype around [`SyntaxNode`], so casting is free and the
//! original tokens (including trivia) remain reachable. Accessors return
//! `Option` because the tree is built even from broken input.

use crate::kind::{SyntaxKind, SyntaxKind::*, SyntaxNode, SyntaxToken};

pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;

    fn text(&self) -> String {
        self.syntax().text().to_string()
    }
}

fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
}

fn children<N: AstNode>(parent: &SyntaxNode) -> impl Iterator<Item = N> {
    parent.children().filter_map(N::cast)
}

fn token(parent: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|it| it.kind() == kind)
}

macro_rules! ast_node {
    ($(#[$attr:meta])* $name:ident, $kind:ident) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxNode);

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == $kind
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                if node.kind() == $kind {
                    Some($name(node))
                } else {
                    None
                }
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

/// Builds an enum that dispatches over several node kinds.
macro_rules! ast_enum {
    ($name:ident { $($variant:ident($ty:ident)),+ $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant($ty)),+
        }

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                $(<$ty as AstNode>::can_cast(kind))||+
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                $(if let Some(it) = <$ty as AstNode>::cast(node.clone()) {
                    return Some($name::$variant(it));
                })+
                None
            }
            fn syntax(&self) -> &SyntaxNode {
                match self {
                    $($name::$variant(it) => it.syntax()),+
                }
            }
        }
    };
}

// --- declarations --------------------------------------------------------

ast_node!(SourceFile, SOURCE_FILE);
ast_node!(ModuleDecl, MODULE_DECL);
ast_node!(ImportDecl, IMPORT_DECL);
ast_node!(ImportList, IMPORT_LIST);
ast_node!(ImportItem, IMPORT_ITEM);
ast_node!(ImportGlob, IMPORT_GLOB);
ast_node!(TypeDecl, TYPE_DECL);
ast_node!(TraitDecl, TRAIT_DECL);
ast_node!(ImplDecl, IMPL_DECL);
ast_node!(AssocTypeDecl, ASSOC_TYPE_DECL);
ast_node!(TypeBounds, TYPE_BOUNDS);
ast_node!(EffectDecl, EFFECT_DECL);
ast_node!(ContextDecl, CONTEXT_DECL);
ast_node!(TestDecl, TEST_DECL);
ast_node!(BenchDecl, BENCH_DECL);
ast_node!(WithClause, WITH_CLAUSE);
ast_node!(RaisesClause, RAISES_CLAUSE);
ast_node!(RaiseExpr, RAISE_EXPR);
ast_node!(TryExpr, TRY_EXPR);
ast_node!(HandlerExpr, HANDLER_EXPR);
ast_node!(CatchExpr, CATCH_EXPR);
ast_node!(WithExpr, WITH_EXPR);
ast_node!(WithBlock, WITH_BLOCK);
ast_node!(FnDecl, FN_DECL);
ast_node!(LetDecl, LET_DECL);
ast_node!(ParamList, PARAM_LIST);
ast_node!(Param, PARAM);

ast_enum!(Decl {
    Type(TypeDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    Effect(EffectDecl),
    Context(ContextDecl),
    Test(TestDecl),
    Bench(BenchDecl),
    Fn(FnDecl),
    Let(LetDecl),
});

// --- names ---------------------------------------------------------------

ast_node!(Name, NAME);
ast_node!(NameRef, NAME_REF);
ast_node!(Path, PATH);

// --- types ---------------------------------------------------------------

ast_node!(TypeParams, TYPE_PARAMS);
ast_node!(TypeParam, TYPE_PARAM);
ast_node!(TypeArgs, TYPE_ARGS);
ast_node!(VariantType, VARIANT_TYPE);
ast_node!(VariantCase, VARIANT_CASE);
ast_node!(FieldList, FIELD_LIST);
ast_node!(TupleFieldList, TUPLE_FIELD_LIST);
ast_node!(Field, FIELD);
ast_node!(RecordType, RECORD_TYPE);
ast_node!(RowTail, ROW_TAIL);
ast_node!(FnType, FN_TYPE);
ast_node!(PathType, PATH_TYPE);
ast_node!(TupleType, TUPLE_TYPE);
ast_node!(UnitType, UNIT_TYPE);
ast_node!(ParenType, PAREN_TYPE);
ast_node!(ForallType, FORALL_TYPE);
ast_node!(UnionType, UNION_TYPE);
ast_node!(LiteralType, LITERAL_TYPE);

ast_enum!(Type {
    Variant(VariantType),
    Record(RecordType),
    Fn(FnType),
    Path(PathType),
    Tuple(TupleType),
    Unit(UnitType),
    Paren(ParenType),
    Forall(ForallType),
    Union(UnionType),
    Literal(LiteralType),
});

// --- statements and expressions ------------------------------------------

ast_node!(Block, BLOCK);
ast_node!(ExprStmt, EXPR_STMT);
ast_node!(LiteralExpr, LITERAL_EXPR);
ast_node!(PathExpr, PATH_EXPR);
ast_node!(PlaceholderExpr, PLACEHOLDER_EXPR);
ast_node!(RecordExpr, RECORD_EXPR);
ast_node!(RecordExprField, RECORD_EXPR_FIELD);
ast_node!(TupleExpr, TUPLE_EXPR);
ast_node!(ListExpr, LIST_EXPR);
ast_node!(UnitExpr, UNIT_EXPR);
ast_node!(ParenExpr, PAREN_EXPR);
ast_node!(LambdaExpr, LAMBDA_EXPR);
ast_node!(IfExpr, IF_EXPR);
ast_node!(WhileExpr, WHILE_EXPR);
ast_node!(LoopExpr, LOOP_EXPR);
ast_node!(BreakExpr, BREAK_EXPR);
ast_node!(ContinueExpr, CONTINUE_EXPR);
ast_node!(ReturnExpr, RETURN_EXPR);
ast_node!(AssignExpr, ASSIGN_EXPR);
ast_node!(MatchExpr, MATCH_EXPR);
ast_node!(MatchArm, MATCH_ARM);
ast_node!(MatchGuard, MATCH_GUARD);
ast_node!(CallExpr, CALL_EXPR);
ast_node!(ArgList, ARG_LIST);
ast_node!(FieldExpr, FIELD_EXPR);
ast_node!(PipeExpr, PIPE_EXPR);
ast_node!(BinExpr, BIN_EXPR);
ast_node!(PrefixExpr, PREFIX_EXPR);

ast_enum!(Expr {
    Literal(LiteralExpr),
    Path(PathExpr),
    Placeholder(PlaceholderExpr),
    Record(RecordExpr),
    Tuple(TupleExpr),
    List(ListExpr),
    Unit(UnitExpr),
    Paren(ParenExpr),
    Lambda(LambdaExpr),
    If(IfExpr),
    While(WhileExpr),
    Loop(LoopExpr),
    Break(BreakExpr),
    Continue(ContinueExpr),
    Return(ReturnExpr),
    Assign(AssignExpr),
    Raise(RaiseExpr),
    Try(TryExpr),
    Handler(HandlerExpr),
    Catch(CatchExpr),
    With(WithExpr),
    WithBlock(WithBlock),
    Match(MatchExpr),
    Call(CallExpr),
    Field(FieldExpr),
    Pipe(PipeExpr),
    Bin(BinExpr),
    Prefix(PrefixExpr),
    Block(Block),
});

ast_enum!(Stmt {
    Let(LetDecl),
    Expr(ExprStmt),
});

// --- patterns ------------------------------------------------------------

ast_node!(WildcardPat, WILDCARD_PAT);
ast_node!(IdentPat, IDENT_PAT);
ast_node!(LiteralPat, LITERAL_PAT);
ast_node!(PathPat, PATH_PAT);
ast_node!(TupleStructPat, TUPLE_STRUCT_PAT);
ast_node!(RecordPat, RECORD_PAT);
ast_node!(RecordPatField, RECORD_PAT_FIELD);
ast_node!(TuplePat, TUPLE_PAT);

ast_enum!(Pat {
    Wildcard(WildcardPat),
    Ident(IdentPat),
    Literal(LiteralPat),
    TupleStruct(TupleStructPat),
    Record(RecordPat),
    Path(PathPat),
    Tuple(TuplePat),
});

// --- accessors -----------------------------------------------------------

impl SourceFile {
    pub fn module(&self) -> Option<ModuleDecl> {
        child(&self.0)
    }
    pub fn imports(&self) -> impl Iterator<Item = ImportDecl> {
        children(&self.0)
    }
    pub fn decls(&self) -> impl Iterator<Item = Decl> {
        children(&self.0)
    }
}

impl ModuleDecl {
    pub fn path(&self) -> Option<Path> {
        child(&self.0)
    }
}

impl ImportDecl {
    pub fn path(&self) -> Option<Path> {
        child(&self.0)
    }
    pub fn items(&self) -> impl Iterator<Item = ImportItem> {
        child::<ImportList>(&self.0).into_iter().flat_map(|l| children(&l.0.clone()).collect::<Vec<_>>())
    }
    pub fn is_glob(&self) -> bool {
        child::<ImportGlob>(&self.0).is_some()
    }
}

impl ImportItem {
    /// `X as Y` yields `X` here and `Y` from [`ImportItem::alias`].
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn alias(&self) -> Option<Name> {
        children::<Name>(&self.0).nth(1)
    }
}

impl Path {
    pub fn segments(&self) -> impl Iterator<Item = NameRef> {
        children(&self.0)
    }
    /// The path as written, with trivia stripped, e.g. `std::core`.
    pub fn text_path(&self) -> String {
        self.segments().filter_map(|s| s.ident()).collect::<Vec<_>>().join("::")
    }
}

impl Name {
    pub fn ident(&self) -> Option<String> {
        token(&self.0, IDENT).map(|t| t.text().to_string())
    }
}

impl NameRef {
    pub fn ident(&self) -> Option<String> {
        token(&self.0, IDENT).map(|t| t.text().to_string())
    }
}

impl TypeDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.0, PUB_KW).is_some()
    }
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn type_params(&self) -> Option<TypeParams> {
        child(&self.0)
    }
    /// `None` for an opaque declaration such as `pub type Effect<+A, -R, +E>;`.
    pub fn definition(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl EffectDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.0, PUB_KW).is_some()
    }
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn type_params(&self) -> Option<TypeParams> {
        child(&self.0)
    }
    /// The effect's operations, each a named function type.
    pub fn operations(&self) -> impl Iterator<Item = Field> {
        children(&self.0)
    }
}

impl ContextDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.0, PUB_KW).is_some()
    }
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    /// Handler bindings, in order. They are sequential: each may use the ones
    /// above it, which is what keeps service composition flat.
    pub fn bindings(&self) -> impl Iterator<Item = RecordExprField> {
        children(&self.0)
    }
}

impl TestDecl {
    /// The quoted name, with its surrounding quotes removed.
    pub fn name(&self) -> Option<String> {
        token(&self.0, STRING_LIT).map(|t| t.text().trim_matches('"').to_string())
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl BenchDecl {
    pub fn name(&self) -> Option<String> {
        token(&self.0, STRING_LIT).map(|t| t.text().trim_matches('"').to_string())
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl FnDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.0, PUB_KW).is_some()
    }
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn type_params(&self) -> Option<TypeParams> {
        child(&self.0)
    }
    pub fn params(&self) -> Option<ParamList> {
        child(&self.0)
    }
    pub fn return_type(&self) -> Option<Type> {
        child(&self.0)
    }
    /// `None` for a signature-only declaration.
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl ParamList {
    pub fn params(&self) -> impl Iterator<Item = Param> {
        children(&self.0)
    }
}

impl Param {
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn is_wildcard(&self) -> bool {
        token(&self.0, UNDERSCORE).is_some()
    }
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl LetDecl {
    pub fn is_mut(&self) -> bool {
        token(&self.0, MUT_KW).is_some()
    }
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
    pub fn initializer(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl TypeParams {
    pub fn params(&self) -> impl Iterator<Item = TypeParam> {
        children(&self.0)
    }
}

impl TypeParam {
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn is_const(&self) -> bool {
        token(&self.0, CONST_KW).is_some()
    }
    /// `+` covariant, `-` contravariant, `None` invariant.
    pub fn variance(&self) -> Option<Variance> {
        if token(&self.0, PLUS).is_some() {
            Some(Variance::Covariant)
        } else if token(&self.0, MINUS).is_some() {
            Some(Variance::Contravariant)
        } else {
            None
        }
    }
    /// The traits this parameter requires, or `None` when it is unbounded.
    ///
    /// A const parameter has no bounds: what follows its `:` is the type of the
    /// value, which [`TypeParam::const_type`] returns instead.
    pub fn bounds(&self) -> Option<TypeBounds> {
        child(&self.0)
    }
    /// The type of a `const N: Int` parameter.
    pub fn const_type(&self) -> Option<Type> {
        if !self.is_const() {
            return None;
        }
        child(&self.0)
    }
}

impl TypeBounds {
    pub fn types(&self) -> impl Iterator<Item = Type> {
        children(&self.0)
    }
}

impl TraitDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.0, PUB_KW).is_some()
    }
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn type_params(&self) -> Option<TypeParams> {
        child(&self.0)
    }
    /// The supertraits after `trait Ord: Eq`.
    pub fn supertraits(&self) -> Option<TypeBounds> {
        child(&self.0)
    }
    pub fn functions(&self) -> impl Iterator<Item = FnDecl> {
        children(&self.0)
    }
    pub fn assoc_types(&self) -> impl Iterator<Item = AssocTypeDecl> {
        children(&self.0)
    }
}

impl ImplDecl {
    pub fn type_params(&self) -> Option<TypeParams> {
        child(&self.0)
    }
    /// The trait being implemented: the type before `for`.
    pub fn trait_(&self) -> Option<Type> {
        children::<Type>(&self.0).next()
    }
    /// The implementing type: the type after `for`.
    pub fn self_type(&self) -> Option<Type> {
        children::<Type>(&self.0).nth(1)
    }
    pub fn functions(&self) -> impl Iterator<Item = FnDecl> {
        children(&self.0)
    }
    pub fn assoc_types(&self) -> impl Iterator<Item = AssocTypeDecl> {
        children(&self.0)
    }
}

impl AssocTypeDecl {
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn bounds(&self) -> Option<TypeBounds> {
        child(&self.0)
    }
    /// The type an impl assigns, as in `type Item = Int;`.
    pub fn definition(&self) -> Option<Type> {
        child(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variance {
    Covariant,
    Contravariant,
}

impl TypeArgs {
    pub fn args(&self) -> impl Iterator<Item = Type> {
        children(&self.0)
    }
}

impl VariantType {
    pub fn cases(&self) -> impl Iterator<Item = VariantCase> {
        children(&self.0)
    }
}

impl VariantCase {
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    /// Named payload: `Some(value: T)`.
    pub fn fields(&self) -> Option<FieldList> {
        child(&self.0)
    }
    /// Positional payload: `Pair(Int, Int)`.
    pub fn tuple_fields(&self) -> Option<TupleFieldList> {
        child(&self.0)
    }
}

impl FieldList {
    pub fn fields(&self) -> impl Iterator<Item = Field> {
        children(&self.0)
    }
}

impl TupleFieldList {
    pub fn types(&self) -> impl Iterator<Item = Type> {
        children(&self.0)
    }
}

impl Field {
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl RecordType {
    pub fn fields(&self) -> impl Iterator<Item = Field> {
        children(&self.0)
    }
    /// The `| 'r` part of an open row, if present.
    pub fn row_tail(&self) -> Option<RowTail> {
        child(&self.0)
    }
    /// True for the closed empty row `{}`.
    pub fn is_empty_row(&self) -> bool {
        self.fields().next().is_none() && self.row_tail().is_none()
    }
}

impl RowTail {
    pub fn types(&self) -> impl Iterator<Item = Type> {
        children(&self.0)
    }
}

impl FnType {
    pub fn param_type(&self) -> Option<Type> {
        child(&self.0)
    }
    pub fn return_type(&self) -> Option<Type> {
        children::<Type>(&self.0).nth(1)
    }
}

impl PathType {
    pub fn path(&self) -> Option<Path> {
        child(&self.0)
    }
    pub fn type_args(&self) -> Option<TypeArgs> {
        child(&self.0)
    }
    /// Set for a bare row variable such as `'r`.
    pub fn row_var(&self) -> Option<SyntaxToken> {
        token(&self.0, ROW_VAR)
    }
}

impl TupleType {
    pub fn elements(&self) -> impl Iterator<Item = Type> {
        children(&self.0)
    }
}

impl LiteralType {
    /// The integer this const-generic argument names.
    pub fn value(&self) -> Option<i64> {
        token(&self.0, INT_LIT)?.text().replace('_', "").parse().ok()
    }
}

impl UnionType {
    pub fn operands(&self) -> impl Iterator<Item = Type> {
        children(&self.0)
    }
}

impl ForallType {
    pub fn type_params(&self) -> Option<TypeParams> {
        child(&self.0)
    }
    pub fn body(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl Block {
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> {
        children(&self.0)
    }
    /// The final expression, which is the block's value.
    pub fn tail_expr(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).last()
    }
}

impl ExprStmt {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl CallExpr {
    pub fn callee(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn args(&self) -> Option<ArgList> {
        child(&self.0)
    }
}

impl ArgList {
    pub fn args(&self) -> impl Iterator<Item = Expr> {
        children(&self.0)
    }
    /// True when any argument is the `_` pipe placeholder.
    pub fn has_placeholder(&self) -> bool {
        self.args().any(|a| matches!(a, Expr::Placeholder(_)))
    }
}

impl FieldExpr {
    pub fn base(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn field(&self) -> Option<NameRef> {
        child(&self.0)
    }
}

impl PipeExpr {
    pub fn lhs(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn rhs(&self) -> Option<Expr> {
        children::<Expr>(&self.0).nth(1)
    }
}

impl BinExpr {
    pub fn lhs(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn rhs(&self) -> Option<Expr> {
        children::<Expr>(&self.0).nth(1)
    }
    pub fn op(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|t| !t.kind().is_trivia())
    }
}

impl PrefixExpr {
    pub fn operand(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl LambdaExpr {
    pub fn params(&self) -> Option<ParamList> {
        child(&self.0)
    }
    pub fn body(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl IfExpr {
    pub fn condition(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn then_branch(&self) -> Option<Block> {
        child(&self.0)
    }
    /// Either a `Block` or, for `else if`, another `IfExpr`.
    pub fn else_branch(&self) -> Option<Expr> {
        let mut blocks = children::<Block>(&self.0);
        blocks.next();
        if let Some(b) = blocks.next() {
            return Some(Expr::Block(b));
        }
        children::<IfExpr>(&self.0).next().map(Expr::If)
    }
}

impl WhileExpr {
    pub fn condition(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl LoopExpr {
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl BreakExpr {
    /// `break expr` carries a value out of a `loop`.
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl ReturnExpr {
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl AssignExpr {
    pub fn target(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn value(&self) -> Option<Expr> {
        children::<Expr>(&self.0).nth(1)
    }
}

impl MatchExpr {
    pub fn scrutinee(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn arms(&self) -> impl Iterator<Item = MatchArm> {
        children(&self.0)
    }
}

impl MatchArm {
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }
    pub fn guard(&self) -> Option<MatchGuard> {
        child(&self.0)
    }
    pub fn body(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl MatchGuard {
    pub fn condition(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl RecordExpr {
    pub fn fields(&self) -> impl Iterator<Item = RecordExprField> {
        children(&self.0)
    }
}

impl RecordExprField {
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl LiteralExpr {
    pub fn token(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|t| t.kind().is_literal())
    }
}

impl IdentPat {
    pub fn name(&self) -> Option<Name> {
        child(&self.0)
    }
}

impl PathPat {
    pub fn path(&self) -> Option<Path> {
        child(&self.0)
    }
}

impl TupleStructPat {
    pub fn path(&self) -> Option<Path> {
        child(&self.0)
    }
    pub fn fields(&self) -> impl Iterator<Item = Pat> {
        children(&self.0)
    }
}

impl RecordPat {
    pub fn path(&self) -> Option<Path> {
        child(&self.0)
    }
    pub fn fields(&self) -> impl Iterator<Item = RecordPatField> {
        children(&self.0)
    }
}

impl TuplePat {
    pub fn fields(&self) -> impl Iterator<Item = Pat> {
        children(&self.0)
    }
}
