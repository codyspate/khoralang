//! The single `SyntaxKind` enum used for both tokens and CST nodes.
//!
//! Token kinds are produced by [`crate::lexer`]; node kinds are produced by the
//! parser. Keeping them in one enum is what lets `rowan` store a lossless tree
//! where every byte of the original source is reachable.

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    // --- trivia ---------------------------------------------------------
    WHITESPACE = 0,
    LINE_COMMENT,
    BLOCK_COMMENT,

    // --- literals -------------------------------------------------------
    INT_LIT,
    FLOAT_LIT,
    STRING_LIT,

    // --- names ----------------------------------------------------------
    IDENT,
    /// A lifetime-style row variable: `'r`.
    ROW_VAR,

    // --- keywords -------------------------------------------------------
    MODULE_KW,
    IMPORT_KW,
    TYPE_KW,
    FN_KW,
    MATCH_KW,
    LET_KW,
    MUT_KW,
    PUB_KW,
    AS_KW,
    IF_KW,
    FORALL_KW,
    CONST_KW,
    TRUE_KW,
    FALSE_KW,

    // --- punctuation ----------------------------------------------------
    SEMICOLON,
    COMMA,
    DOT,
    COLON,
    UNDERSCORE,
    PIPE,
    /// `|>`
    PIPE_GT,
    /// `->`
    THIN_ARROW,
    /// `=>`
    FAT_ARROW,
    EQ,
    PLUS,
    MINUS,
    STAR,
    SLASH,
    PERCENT,
    BANG,
    AMP_AMP,
    PIPE_PIPE,
    EQ_EQ,
    BANG_EQ,
    LT_EQ,
    GT_EQ,
    LT,
    GT,
    L_PAREN,
    R_PAREN,
    L_BRACE,
    R_BRACE,
    L_BRACK,
    R_BRACK,

    /// Any byte sequence the lexer could not classify.
    LEX_ERROR,

    // --- declarations ---------------------------------------------------
    SOURCE_FILE,
    MODULE_DECL,
    IMPORT_DECL,
    IMPORT_LIST,
    IMPORT_ITEM,
    IMPORT_GLOB,
    TYPE_DECL,
    FN_DECL,
    LET_DECL,
    PARAM_LIST,
    PARAM,

    // --- names / paths --------------------------------------------------
    NAME,
    NAME_REF,
    PATH,

    // --- types ----------------------------------------------------------
    TYPE_PARAMS,
    TYPE_PARAM,
    TYPE_ARGS,
    VARIANT_TYPE,
    VARIANT_CASE,
    FIELD_LIST,
    FIELD,
    TUPLE_FIELD_LIST,
    RECORD_TYPE,
    ROW_TAIL,
    FN_TYPE,
    PATH_TYPE,
    TUPLE_TYPE,
    UNIT_TYPE,
    PAREN_TYPE,
    FORALL_TYPE,
    /// A const-generic argument written as a literal, e.g. `Embedding<1536, F32>`.
    LITERAL_TYPE,
    /// `E1 + E2` — an open union of typed failure channels.
    UNION_TYPE,

    // --- statements -----------------------------------------------------
    BLOCK,
    EXPR_STMT,

    // --- expressions ----------------------------------------------------
    LITERAL_EXPR,
    PATH_EXPR,
    PLACEHOLDER_EXPR,
    /// `:ledger.get_history` — a capability reference resolved from the `R` row.
    CAPABILITY_EXPR,
    RECORD_EXPR,
    RECORD_EXPR_FIELD,
    TUPLE_EXPR,
    UNIT_EXPR,
    PAREN_EXPR,
    LAMBDA_EXPR,
    MATCH_EXPR,
    MATCH_ARM,
    MATCH_GUARD,
    CALL_EXPR,
    ARG_LIST,
    FIELD_EXPR,
    PIPE_EXPR,
    BIN_EXPR,
    PREFIX_EXPR,
    BLOCK_EXPR,

    // --- patterns -------------------------------------------------------
    WILDCARD_PAT,
    IDENT_PAT,
    LITERAL_PAT,
    PATH_PAT,
    TUPLE_STRUCT_PAT,
    RECORD_PAT,
    RECORD_PAT_FIELD,
    TUPLE_PAT,

    /// Placeholder node emitted for unparseable input so the tree stays lossless.
    ERROR,

    /// Sentinel; must stay last.
    EOF,
}

use SyntaxKind::*;

impl SyntaxKind {
    /// Trivia is attached to the tree but ignored by the parser's token stream.
    pub fn is_trivia(self) -> bool {
        matches!(self, WHITESPACE | LINE_COMMENT | BLOCK_COMMENT)
    }

    pub fn is_literal(self) -> bool {
        matches!(self, INT_LIT | FLOAT_LIT | STRING_LIT | TRUE_KW | FALSE_KW)
    }

    /// Keywords that can begin a top-level declaration; used for error recovery.
    pub fn is_decl_start(self) -> bool {
        matches!(self, PUB_KW | TYPE_KW | FN_KW | LET_KW | MODULE_KW | IMPORT_KW)
    }

}

/// Declares every reserved word once, generating both the lexer's lookup and
/// the list the editor grammar is checked against.
macro_rules! keywords {
    ($($text:literal => $kind:ident),* $(,)?) => {
        /// Every reserved word in Khora.
        ///
        /// `editors/vscode/syntaxes/khora.tmLanguage.json` must list exactly
        /// these; the `keywords_match_the_lexer` test enforces it.
        pub const KEYWORDS: &[&str] = &[$($text),*];

        impl SyntaxKind {
            pub fn from_keyword(text: &str) -> Option<SyntaxKind> {
                match text {
                    $($text => Some($kind),)*
                    _ => None,
                }
            }
        }
    };
}

keywords! {
    "module" => MODULE_KW,
    "import" => IMPORT_KW,
    "type" => TYPE_KW,
    "fn" => FN_KW,
    "match" => MATCH_KW,
    "let" => LET_KW,
    "mut" => MUT_KW,
    "pub" => PUB_KW,
    "as" => AS_KW,
    "if" => IF_KW,
    "forall" => FORALL_KW,
    "const" => CONST_KW,
    "true" => TRUE_KW,
    "false" => FALSE_KW,
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        Self(kind as u16)
    }
}

/// The `rowan` language marker for Khora.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Khora {}

impl rowan::Language for Khora {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        assert!(raw.0 <= EOF as u16, "unknown SyntaxKind: {}", raw.0);
        // SAFETY: `SyntaxKind` is `#[repr(u16)]` and the discriminant was just
        // range-checked against the last variant.
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub type SyntaxNode = rowan::SyntaxNode<Khora>;
pub type SyntaxToken = rowan::SyntaxToken<Khora>;
pub type SyntaxElement = rowan::SyntaxElement<Khora>;
