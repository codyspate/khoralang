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
    //
    // `HANDLER_KW`, `FOR_KW`, `CONTEXT_KW`, `TEST_KW` and `BENCH_KW` are
    // *contextual*: the lexer never produces them, the parser remaps an `IDENT`
    // to one of them in the single position where the word is a keyword. See
    // `CONTEXTUAL_KEYWORDS` below.
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
    ELSE_KW,
    FORALL_KW,
    CONST_KW,
    EFFECT_KW,
    WITH_KW,
    RAISES_KW,
    RAISE_KW,
    HANDLER_KW,
    FOR_KW,
    CONTEXT_KW,
    CATCH_KW,
    TEST_KW,
    BENCH_KW,
    WHILE_KW,
    LOOP_KW,
    BREAK_KW,
    CONTINUE_KW,
    RETURN_KW,
    TRUE_KW,
    FALSE_KW,

    // --- punctuation ----------------------------------------------------
    SEMICOLON,
    COMMA,
    DOT,
    COLON,
    /// `::` — the path separator, for compile-time namespaces.
    COLON_COLON,
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
    EFFECT_DECL,
    CONTEXT_DECL,
    TEST_DECL,
    BENCH_DECL,
    FN_DECL,
    LET_DECL,
    PARAM_LIST,
    PARAM,
    /// `with { ledger: Ledger }` on a signature or a function type.
    WITH_CLAUSE,
    /// `raises DbError + ModelError` on a signature or a function type.
    RAISES_CLAUSE,

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
    RECORD_EXPR,
    RECORD_EXPR_FIELD,
    TUPLE_EXPR,
    /// `[a, b, c]`
    LIST_EXPR,
    UNIT_EXPR,
    PAREN_EXPR,
    LAMBDA_EXPR,
    IF_EXPR,
    WHILE_EXPR,
    LOOP_EXPR,
    BREAK_EXPR,
    CONTINUE_EXPR,
    RETURN_EXPR,
    /// `x = expr` — an expression of type `()`, as in Rust.
    ASSIGN_EXPR,
    /// `raise DbError.QueryFailed(e)` — performs an operation of the error row.
    RAISE_EXPR,
    /// `expr!` — marks a call that can abort the enclosing function.
    TRY_EXPR,
    /// `handler for Ledger { .. }`
    HANDLER_EXPR,
    /// `expr catch { .. }` — handles part of the error row.
    CATCH_EXPR,
    /// `expr with { .. }` — installs handlers over a single expression.
    WITH_EXPR,
    /// `with { .. } { .. }` — installs handlers over a region.
    WITH_BLOCK,
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

    /// Reserved words that can begin a top-level declaration; used for error
    /// recovery.
    ///
    /// `context`, `test` and `bench` also begin declarations but are contextual
    /// keywords, so they arrive as `IDENT` and cannot be recognised by kind
    /// alone — the parser's `at_decl_start` covers those.
    pub fn is_decl_start(self) -> bool {
        matches!(self, PUB_KW | TYPE_KW | FN_KW | LET_KW | MODULE_KW | IMPORT_KW | EFFECT_KW)
    }

    /// True for the token kinds the parser produces by remapping an `IDENT`.
    pub fn is_contextual_keyword(self) -> bool {
        self.contextual_keyword_text().is_some()
    }
}

/// Declares every reserved word once, generating both the lexer's lookup and
/// the list the editor grammar is checked against.
macro_rules! keywords {
    ($($text:literal => $kind:ident),* $(,)?) => {
        /// Every *hard* reserved word in Khora: a word that can never be used
        /// as an identifier.
        ///
        /// `editors/vscode/syntaxes/khora.tmLanguage.json` must list exactly
        /// these; the `keywords_match_the_lexer` test enforces it.
        pub const KEYWORDS: &[&str] = &[$($text),*];

        impl SyntaxKind {
            /// The token kind for a hard keyword, or `None` for an identifier.
            ///
            /// Contextual keywords deliberately return `None`: they must reach
            /// the parser as `IDENT` so they stay usable as ordinary names.
            pub fn from_keyword(text: &str) -> Option<SyntaxKind> {
                match text {
                    $($text => Some($kind),)*
                    _ => None,
                }
            }
        }
    };
}

/// Declares the contextual keywords, generating the spelling lookup the parser
/// uses to recognise one and the list the editor grammar is checked against.
macro_rules! contextual_keywords {
    ($($text:literal => $kind:ident),* $(,)?) => {
        /// Words that are keywords in exactly one position and ordinary
        /// identifiers everywhere else.
        ///
        /// `editors/vscode/syntaxes/khora.tmLanguage.json` must list exactly
        /// these in its `contextual-keywords` rules; the
        /// `contextual_keywords_match_the_lexer` test enforces it.
        pub const CONTEXTUAL_KEYWORDS: &[&str] = &[$($text),*];

        impl SyntaxKind {
            /// The spelling of a contextual keyword kind, or `None` for every
            /// other kind. This is the parser's only reason to look at text.
            pub fn contextual_keyword_text(self) -> Option<&'static str> {
                match self {
                    $($kind => Some($text),)*
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
    "else" => ELSE_KW,
    "forall" => FORALL_KW,
    "const" => CONST_KW,
    "effect" => EFFECT_KW,
    "with" => WITH_KW,
    "raises" => RAISES_KW,
    "raise" => RAISE_KW,
    "catch" => CATCH_KW,
    "while" => WHILE_KW,
    "loop" => LOOP_KW,
    "break" => BREAK_KW,
    "continue" => CONTINUE_KW,
    "return" => RETURN_KW,
    "true" => TRUE_KW,
    "false" => FALSE_KW,
}

// Reserving these four would cost more than it buys: they are the obvious names
// for a request callback, a dependency bundle, a variable under test and a
// benchmark input, and `std/net/http.kh` already had to rename a parameter away
// from `handler` once. Rust keeps `test` usable as an identifier for the same
// reason. Each is a keyword in one position only, and in that position an
// ordinary identifier is not grammatical, so there is nothing to disambiguate.
//
// `for` is here only because `handler for E { .. }` needs it, and it is
// recognised solely in that position. The `for` loop of phase 3 will make it a
// hard keyword; do not rely on it as an identifier.
contextual_keywords! {
    "handler" => HANDLER_KW,
    "for" => FOR_KW,
    "context" => CONTEXT_KW,
    "test" => TEST_KW,
    "bench" => BENCH_KW,
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
