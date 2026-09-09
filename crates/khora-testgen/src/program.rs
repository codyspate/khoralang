//! A generator of **syntactically valid** Khora programs.
//!
//! Everything [`program`] emits parses with no diagnostics. That is the whole
//! contract, and it is what the formatter's properties need: `format` refuses
//! input that does not parse, so a generator that produced errors would be
//! testing nothing but the refusal.
//!
//! It is *not* a generator of well-typed programs. Names come from a small
//! pool and are reused freely, so a generated program calls functions that do
//! not exist and adds strings to integers. Nothing downstream of the parser
//! can be driven by this as it stands; see "What this does not do".
//!
//! # How to extend it
//!
//! Every construct is one function taking `&mut Entropy` and returning a
//! `String`, and every alternation is a `src.choice(n)` whose **arm 0 is the
//! simplest** — [`crate::entropy`] says why that ordering is what makes
//! `proptest` shrink a failure down to something readable. A recursive
//! construct goes through [`Entropy::descend`] and emits a leaf when it hands
//! back `None`, so there is no depth counter to remember to decrement.
//!
//! # What this does not do
//!
//! Written down rather than left to be discovered, because the value of a
//! generator is knowing which combinations it has actually been trying.
//!
//! * **No `extern fn`, no `pub` inside a trait body, no bare `pub`.** All
//!   three break the parser today — see `KNOWN_BAD_TOKENS` in [`crate::soup`]
//!   and the ignored tests in `crates/khora-syntax/tests/parser_properties.rs`.
//! * **No `forall` types, no variance markers (`<+A, -R>`), no const generics
//!   (`<const N: Int>`), no associated-type bounds.** Bounds are generated,
//!   but only as `T: Trait + Trait` on a type parameter.
//! * **No `with Path { .. }` context expression**, only `with { .. } { .. }`.
//!   The named form is genuinely ambiguous — the brace after the path is read
//!   as an override record, so whether a block follows depends on the first
//!   two tokens inside it — and a generator that walked into it would keep
//!   reporting the ambiguity instead of finding anything.
//! * **No comments, no doc comments, no blank lines.** This is the most
//!   valuable gap to close next: four of the formatter's fixed-input
//!   regression tests are about where a comment lands, which is exactly the
//!   evidence that generated comments would find a fifth.
//! * **Nothing that needs a second file**: no import of a path that exists, no
//!   cross-module reference.
//! * **No `\` escapes in string literals**, and none unterminated. The parser
//!   validates escapes, so an arbitrary backslash is a diagnostic rather than
//!   a program; escapes are [`crate::soup`]'s half of the job. What is covered
//!   here is the other half, `${..}` holes, which are not escapes.
//! * **Nothing deeper than [`crate::entropy::DEFAULT_MAX_DEPTH`]**, which is
//!   far below where the parser overflows its stack. That overflow is a known
//!   bug and reaching it aborts the run rather than failing a case.
//! * **No `raises` row mixing a boxed and an unboxed payload.** That
//!   distinction does not exist in the syntax — it is a `khora-types` and
//!   `khora-perceus` property — so reaching it means generating programs that
//!   *type-check*, which is the next thing this should grow into and is a much
//!   larger job than what is here.

use crate::entropy::Entropy;

/// A whole module. Always parses; never type-checks except by accident.
pub fn program(src: &mut Entropy<'_>) -> String {
    let mut out = String::from("module m;\n");
    let paths = src.count(IMPORT_PATHS.len());
    for path in IMPORT_PATHS.iter().take(paths) {
        out.push_str(&import(src, path));
    }
    // At least one declaration, so the smallest generated program is still a
    // program rather than a header.
    for _ in 0..=src.count(4) {
        out.push('\n');
        out.push_str(&declaration(src));
    }
    out
}

// --- names -----------------------------------------------------------------

/// Lowercase names, for values. Small and reused on purpose: a collision costs
/// nothing at this level, and a generator that invents a fresh identifier per
/// position produces programs in which nothing is ever mentioned twice.
const LOWER: &[&str] = &["a", "b", "x", "y", "f", "g", "total", "run"];
/// Uppercase names, for types, traits, effects and variants.
const UPPER: &[&str] = &["T", "A", "B", "Foo", "Bar", "Log", "Db"];
/// The types that exist without being declared.
const BUILTIN: &[&str] = &["Int", "String", "Bool", "Float"];

fn lower(src: &mut Entropy<'_>) -> &'static str {
    LOWER[src.choice(LOWER.len())]
}

fn upper(src: &mut Entropy<'_>) -> &'static str {
    UPPER[src.choice(UPPER.len())]
}

// --- declarations ----------------------------------------------------------

/// The module paths an import may name.
///
/// One per generated import declaration and never repeated, because the
/// formatter merges and deduplicates import lists by design — two imports of
/// the same path come back as one, which is correct and would look like a lost
/// declaration to a property that compares tokens.
const IMPORT_PATHS: &[&str] = &["std::core", "std::io", "std::net::http", "a::b::c"];

fn import(src: &mut Entropy<'_>, path: &str) -> String {
    // The `::{..}` or `::*` tail is mandatory. A bare `import std::core;` is a
    // diagnostic, not a shorter spelling.
    if src.chance(32) {
        return format!("import {path}::*;\n");
    }
    let mut items: Vec<String> = Vec::new();
    for _ in 0..=src.count(2) {
        let name = if src.chance(128) { lower(src) } else { upper(src) };
        // Deduplicated for the same reason the paths are distinct: an import
        // list is sorted and deduplicated by the formatter, so a repeated name
        // is a token the output is *supposed* to lose.
        let item = if src.chance(48) {
            format!("{name} as {}", lower(src))
        } else {
            name.to_string()
        };
        if !items.iter().any(|existing| existing == &item) {
            items.push(item);
        }
    }
    format!("import {path}::{{{}}};\n", items.join(", "))
}

fn declaration(src: &mut Entropy<'_>) -> String {
    match src.choice(8) {
        0 => const_decl(src),
        1 => type_alias(src),
        2 => fn_decl(src),
        3 => sum_type(src),
        4 => effect_decl(src),
        5 => row_decl(src),
        6 => trait_decl(src),
        _ => impl_decl(src),
    }
}

fn vis(src: &mut Entropy<'_>) -> &'static str {
    if src.chance(96) {
        "pub "
    } else {
        ""
    }
}

fn const_decl(src: &mut Entropy<'_>) -> String {
    let v = vis(src);
    let name = lower(src);
    let annotation = if src.chance(160) { format!(": {}", ty(src)) } else { String::new() };
    format!("{v}const {name}{annotation} = {};\n", expr(src, Records::Allowed))
}

fn type_alias(src: &mut Entropy<'_>) -> String {
    let v = vis(src);
    let name = upper(src);
    let params = type_params(src);
    format!("{v}type {name}{params} = {};\n", ty(src))
}

fn sum_type(src: &mut Entropy<'_>) -> String {
    let derives = if src.chance(64) { "derive(Eq, Ord, Show)\n" } else { "" };
    let v = vis(src);
    let name = upper(src);
    let mut cases = String::new();
    // Every case carries its own leading `|`, the first one included. `= A | B`
    // is not a shorter spelling of a sum type; it does not parse at all.
    for _ in 0..=src.count(3) {
        cases.push_str("\n  | ");
        cases.push_str(upper(src));
        match src.choice(3) {
            0 => {}
            1 => cases.push_str(&format!("({})", ty(src))),
            // A named payload, which the parser tells from a positional one by
            // looking for `IDENT :` after the paren.
            _ => cases.push_str(&format!("({}: {})", lower(src), ty(src))),
        }
    }
    format!("{derives}{v}type {name} ={cases};\n")
}

fn effect_decl(src: &mut Entropy<'_>) -> String {
    let v = vis(src);
    let name = upper(src);
    let mut ops = String::new();
    for _ in 0..=src.count(2) {
        ops.push_str(&format!("\n  {}: ({}) -> {},", lower(src), ty(src), ty(src)));
    }
    format!("{v}effect {name} {{{ops}\n}}\n")
}

fn row_decl(src: &mut Entropy<'_>) -> String {
    let v = vis(src);
    let name = upper(src);
    let mut fields = Vec::new();
    for _ in 0..=src.count(2) {
        fields.push(format!("{}: {}", lower(src), upper(src)));
    }
    format!("{v}row {name} = {{ {} }};\n", fields.join(", "))
}

fn trait_decl(src: &mut Entropy<'_>) -> String {
    let v = vis(src);
    let name = upper(src);
    let supertrait = if src.chance(64) { format!(": {}", upper(src)) } else { String::new() };
    let mut body = String::new();
    if src.chance(96) {
        body.push_str(&format!("\n  type {};", upper(src)));
    }
    for _ in 0..=src.count(2) {
        // No `pub` in here, and not because it would be redundant: `pub`
        // inside a trait body routes straight to `fn_decl`, which asserts on
        // the `fn` it was promised, so `pub type Item;` panics the parser.
        body.push_str(&format!("\n  fn {}(self) -> {};", lower(src), ty(src)));
    }
    format!("{v}trait {name}{supertrait} {{{body}\n}}\n")
}

fn impl_decl(src: &mut Entropy<'_>) -> String {
    // No `pub`: an impl takes no visibility, and `pub impl` is one of the two
    // shapes that make the file loop spin to its step limit and drop the rest
    // of the file out of the tree.
    let params = type_params(src);
    let subject = format!("{}{}", upper(src), type_args(src));
    let head = if src.chance(128) {
        format!("impl{params} {} for {subject}", upper(src))
    } else {
        format!("impl{params} {subject}")
    };
    let mut body = String::new();
    for _ in 0..=src.count(2) {
        body.push_str(&format!("\n  {}", indent(fn_decl(src).trim_end())));
    }
    format!("{head} {{{body}\n}}\n")
}

fn fn_decl(src: &mut Entropy<'_>) -> String {
    let v = vis(src);
    let name = lower(src);
    let params = type_params(src);
    let mut args = Vec::new();
    for _ in 0..src.count(3) {
        // An untyped parameter is legal, and is what `self` is.
        if src.chance(48) {
            args.push(lower(src).to_string());
        } else {
            args.push(format!("{}: {}", lower(src), ty(src)));
        }
    }
    let ret = if src.chance(192) { format!(" -> {}", ty(src)) } else { String::new() };

    // `with` and `raises` come after the return type, in either order and any
    // number of times. Broken onto their own lines about half the time,
    // because that is how a signature carrying both is written in `std` and it
    // is a shape the formatter has to put back where it found it.
    let mut clauses = String::new();
    let sep = if src.chance(128) { "\n  " } else { " " };
    if src.chance(96) {
        clauses.push_str(sep);
        clauses.push_str(&format!("with {}", effect_row(src)));
    }
    if src.chance(96) {
        clauses.push_str(sep);
        clauses.push_str(&format!("raises {}", raises_row(src)));
    }

    // A signature with no body ends in `;`, and is a promise the checker
    // accepts ahead of the function that keeps it.
    if src.chance(24) {
        return format!("{v}fn {name}{params}({}){ret}{clauses};\n", args.join(", "));
    }
    format!("{v}fn {name}{params}({}){ret}{clauses} {}\n", args.join(", "), block(src))
}

/// `<A, B: Show + Eq, 'ef>`, or nothing.
fn type_params(src: &mut Entropy<'_>) -> String {
    if !src.chance(96) {
        return String::new();
    }
    let mut params = Vec::new();
    for _ in 0..=src.count(2) {
        match src.choice(3) {
            0 => params.push(upper(src).to_string()),
            // A bound, which is where nested generics and trait bounds meet.
            1 => params.push(format!("{}: {}", upper(src), bounds(src))),
            _ => params.push(row_var(src).to_string()),
        }
    }
    format!("<{}>", params.join(", "))
}

fn type_args(src: &mut Entropy<'_>) -> String {
    if !src.chance(64) {
        return String::new();
    }
    let mut args = Vec::new();
    for _ in 0..=src.count(1) {
        args.push(ty(src));
    }
    format!("<{}>", args.join(", "))
}

fn bounds(src: &mut Entropy<'_>) -> String {
    let mut out = String::from(upper(src));
    for _ in 0..src.count(2) {
        out.push_str(" + ");
        out.push_str(upper(src));
    }
    out
}

/// The row variables a signature may mention.
///
/// Fixed rather than generated: a signature that binds `'ef` and then mentions
/// `'zz` still parses, and using only the ones it binds is what makes a shrunk
/// failing program readable.
const ROW_VARS: &[&str] = &["'ef", "'er", "'r"];

fn row_var(src: &mut Entropy<'_>) -> &'static str {
    ROW_VARS[src.choice(ROW_VARS.len())]
}

/// What follows `with`: a row variable, a named row, or a record of
/// capabilities, optionally with a variable tail.
fn effect_row(src: &mut Entropy<'_>) -> String {
    match src.choice(4) {
        0 => row_var(src).to_string(),
        1 => upper(src).to_string(),
        2 => format!("{{ {}: {} }}", lower(src), upper(src)),
        _ => format!("{{ {} | {}: {} }}", row_var(src), lower(src), upper(src)),
    }
}

/// What follows `raises`: an open union of failure channels.
///
/// The union is the point of generating this at all. A row of one is the
/// common case; a row of several with a variable tail is where the
/// combinations that have historically bitten actually live.
fn raises_row(src: &mut Entropy<'_>) -> String {
    let mut out = String::from(upper(src));
    for _ in 0..src.count(2) {
        out.push_str(" + ");
        if src.chance(64) {
            out.push_str(row_var(src));
        } else {
            out.push_str(upper(src));
        }
    }
    out
}

// --- types -----------------------------------------------------------------

fn ty(src: &mut Entropy<'_>) -> String {
    let Some(mut src) = src.descend() else {
        return BUILTIN[0].to_string();
    };
    match src.choice(9) {
        0 => BUILTIN[src.choice(BUILTIN.len())].to_string(),
        1 => upper(&mut src).to_string(),
        2 => "()".to_string(),
        3 => format!("{}<{}>", upper(&mut src), ty(&mut src)),
        // Nested generics: a type argument that is itself a generic
        // application, which is one half of a combination the audit named.
        4 => format!("{}<{}, {}>", upper(&mut src), ty(&mut src), ty(&mut src)),
        5 => format!("({}, {})", ty(&mut src), ty(&mut src)),
        6 => {
            // A function type carries its own effect clauses, which is the one
            // place `with` and `raises` appear inside a type rather than after
            // a signature.
            let mut out = format!("({}) -> {}", ty(&mut src), ty(&mut src));
            if src.chance(80) {
                out.push_str(&format!(" with {}", row_var(&mut src)));
            }
            if src.chance(80) {
                out.push_str(&format!(" raises {}", row_var(&mut src)));
            }
            out
        }
        7 => format!("{{ {}: {} }}", lower(&mut src), ty(&mut src)),
        _ => format!("{} + {}", upper(&mut src), upper(&mut src)),
    }
}

// --- statements and expressions --------------------------------------------

/// Whether this position may hold an expression that begins with a `{`.
///
/// Two rules share one flag, because they forbid the same set of forms.
///
/// The parser decides what a `{` opens by lookahead, and clears its own flag
/// while reading an `if` or `while` condition, a `for` iterable, a `match`
/// scrutinee and a match guard. In those positions `{ x: 1 }` is not a record
/// — it is the block or the arm list that follows.
///
/// And a block-like expression is a *statement* wherever a statement could
/// begin, so it cannot be the left operand of anything: `{ 0 } + 0` is a block
/// followed by a `+` with nothing before it. Left operands are therefore
/// always `Forbidden`, whatever the position allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Records {
    Allowed,
    Forbidden,
}

fn block(src: &mut Entropy<'_>) -> String {
    let Some(mut src) = src.descend() else {
        return "{ 0 }".to_string();
    };
    let mut pieces = Vec::new();
    for _ in 0..src.count(3) {
        pieces.push(stmt(&mut src));
    }
    // The tail expression, which is what makes a block a value.
    pieces.push(expr(&mut src, Records::Allowed));

    let mut out = String::from("{\n");
    for (i, piece) in pieces.iter().enumerate() {
        // **A `;` after a block-like statement, when what follows it starts
        // with `!`.** Postfix `!` has no `is_block_like` guard, so
        // `if c { .. } else { .. }` followed by `!x` parses as *try the `if`*
        // and then breaks on the `x`. Postfix `with` guards against exactly
        // this and `!` does not — see
        // `parser_properties.rs::a_try_after_a_block_like_statement_swallows_it`
        // in `khora-syntax` has the whole of it.
        // A `;` there is legal whatever follows; it is only written where it
        // is needed, so that the no-semicolon rule stays generated too.
        if i > 0 && pieces[i - 1].ends_with('}') && piece.starts_with('!') {
            out.push_str(";\n");
        }
        out.push_str("  ");
        out.push_str(&indent(piece));
        out.push('\n');
    }
    out.push('}');
    out
}

/// Re-indents every line but the first by two spaces.
///
/// Generated source is nested by construction, and a block written flat is a
/// block whose failing case cannot be read. The formatter rewrites all of it
/// anyway; this is for the person looking at the shrunk seed.
fn indent(text: &str) -> String {
    text.replace('\n', "\n  ")
}

fn stmt(src: &mut Entropy<'_>) -> String {
    let Some(mut src) = src.descend() else {
        return "let a = 0;".to_string();
    };
    match src.choice(8) {
        0 => format!("let {} = {};", pattern(&mut src), expr(&mut src, Records::Allowed)),
        1 => format!(
            "let mut {}: {} = {};",
            lower(&mut src),
            ty(&mut src),
            expr(&mut src, Records::Allowed)
        ),
        2 => format!("{};", expr(&mut src, Records::Allowed)),
        3 => format!("{} = {};", lower(&mut src), expr(&mut src, Records::Allowed)),
        // A block-like statement needs no `;`, which is a rule of the block
        // loop rather than of the expressions themselves, so both sides of it
        // are worth generating.
        4 => format!("if {} {} else {}", cond(&mut src), block(&mut src), block(&mut src)),
        5 => format!("while {} {}", cond(&mut src), block(&mut src)),
        6 => format!(
            "for {} in {} {}",
            pattern(&mut src),
            expr(&mut src, Records::Forbidden),
            block(&mut src)
        ),
        // `break`'s value is read with braces meaning a record, so it is drawn
        // from the brace-free half of `expr` to keep the shape unambiguous.
        _ => format!("loop {{ break {}; }}", expr(&mut src, Records::Forbidden)),
    }
}

/// An expression in a position where `{` opens a block rather than a record.
fn cond(src: &mut Entropy<'_>) -> String {
    expr(src, Records::Forbidden)
}

fn expr(src: &mut Entropy<'_>, records: Records) -> String {
    let Some(mut src) = src.descend() else {
        return "0".to_string();
    };
    // Arms 0..=6 are leaves and every recursive arm is above them, so a seed
    // of zeroes is the literal `0` and shrinking walks back down this list.
    // The last three arms all contain a `{`, so they are only offered where a
    // brace is unambiguous.
    let arms = if records == Records::Allowed { 20 } else { 17 };
    match src.choice(arms) {
        0 => "0".to_string(),
        1 => format!("{}", src.choice(1000)),
        2 => lower(&mut src).to_string(),
        3 => string_lit(&mut src),
        4 => ["true", "false"][src.choice(2)].to_string(),
        5 => ["1.5", "1.5d", "'a'", "()", "_"][src.choice(5)].to_string(),
        6 => format!("{}::{}", upper(&mut src), upper(&mut src)),
        // The **left** operand is drawn from the brace-free half whatever the
        // caller allows. A block-like expression is a statement wherever a
        // statement could start, so `{ 0 } + 0` is a block and then a `+` with
        // nothing before it — the same rule that lets `if c { .. }` stand
        // without a `;`, seen from the other side. Parenthesizing is how a
        // program means the other thing, and arm 13 is where that comes from.
        7 => format!(
            "{} {} {}",
            expr(&mut src, Records::Forbidden),
            binop(&mut src),
            expr(&mut src, records)
        ),
        8 => format!("{}{}", ["-", "!"][src.choice(2)], expr(&mut src, records)),
        9 => format!("{}({})", lower(&mut src), args(&mut src)),
        10 => format!("{}.{}", expr(&mut src, Records::Forbidden), lower(&mut src)),
        11 => {
            format!("({}, {})", expr(&mut src, Records::Allowed), expr(&mut src, Records::Allowed))
        }
        12 => format!("[{}]", args(&mut src)),
        13 => format!("({})", expr(&mut src, Records::Allowed)),
        14 => format!(
            "{} |> {}({})",
            expr(&mut src, Records::Forbidden),
            lower(&mut src),
            args(&mut src)
        ),
        // The flow operator only ever starts a unary expression, so it is
        // written inside parentheses rather than after anything.
        15 => format!(
            "(||> {}({}) |> {}({}))",
            lower(&mut src),
            args(&mut src),
            lower(&mut src),
            args(&mut src)
        ),
        16 => format!(
            "fn ({}) => {}",
            (0..src.count(2)).map(|_| lower(&mut src)).collect::<Vec<_>>().join(", "),
            expr(&mut src, Records::Allowed)
        ),
        17 => block_like(&mut src),
        18 => record_lit(&mut src),
        _ => effectful(&mut src),
    }
}

/// The expression forms written with braces that are still values.
fn block_like(src: &mut Entropy<'_>) -> String {
    match src.choice(3) {
        0 => format!("if {} {} else {}", cond(src), block(src), block(src)),
        1 => format!("match {} {{{}\n}}", cond(src), arms(src)),
        _ => block(src),
    }
}

fn record_lit(src: &mut Entropy<'_>) -> String {
    // There is no `T { .. }` production: a typed record literal parses as a
    // path followed by a block and breaks two tokens later. Untyped is the
    // only form, and it is the form `std` is written in.
    let base = if src.chance(64) { format!("..{}, ", lower(src)) } else { String::new() };
    let mut fields = Vec::new();
    for _ in 0..=src.count(2) {
        fields.push(format!("{}: {}", lower(src), expr(src, Records::Allowed)));
    }
    format!("{{ {base}{} }}", fields.join(", "))
}

/// `raise`, `!`, `catch`, `with` and `handler` — the effect and failure forms.
///
/// Grouped because they are the combination the audit is about: a value that
/// came out of a `catch` inside a `with` inside a `handler` is a shape nobody
/// writes a fixed test for, and the bug that prompted this needed three such
/// things at once.
fn effectful(src: &mut Entropy<'_>) -> String {
    match src.choice(5) {
        0 => format!("{}({})!", lower(src), args(src)),
        1 => format!("raise {}::{}({})", upper(src), upper(src), args(src)),
        2 => format!("{}({}) catch {{{}\n}}", lower(src), args(src), arms(src)),
        // Only the record form of `with`; see the module header for why the
        // named form is left alone.
        3 => format!("with {{ {}: {} }} {}", lower(src), expr(src, Records::Allowed), block(src)),
        _ => format!(
            "handler for {} {{ {}: fn ({}) => {} }}",
            upper(src),
            lower(src),
            lower(src),
            expr(src, Records::Allowed)
        ),
    }
}

fn arms(src: &mut Entropy<'_>) -> String {
    let mut out = String::new();
    for _ in 0..src.count(2) {
        // A guard is a condition, so braces in it open a block, not a record.
        let guard =
            if src.chance(48) { format!(" if {}", cond(src)) } else { String::new() };
        out.push_str(&format!(
            "\n  {}{} => {},",
            pattern(src),
            guard,
            indent(&expr(src, Records::Allowed))
        ));
    }
    // A wildcard arm last, so an arm list is never empty and a generated match
    // is never one the checker would call inexhaustive for a silly reason.
    out.push_str(&format!("\n  _ => {},", indent(&expr(src, Records::Allowed))));
    out
}

fn args(src: &mut Entropy<'_>) -> String {
    let mut out = Vec::new();
    for _ in 0..src.count(3) {
        out.push(expr(src, Records::Allowed));
    }
    out.join(", ")
}

const BINOPS: &[&str] = &["+", "-", "*", "/", "%", "==", "!=", "<", ">", "<=", ">=", "&&", "||"];

fn binop(src: &mut Entropy<'_>) -> &'static str {
    BINOPS[src.choice(BINOPS.len())]
}

/// A string literal, with `${..}` holes and without escapes.
///
/// Escapes are validated by the parser, so an arbitrary backslash here would
/// be a diagnostic rather than a program — [`crate::soup::interpolated_string`]
/// is where those belong. What this covers is the other half of the same
/// feature: a hole is part of the literal token, so a hole containing a string
/// containing a hole is a single token as far as the formatter is concerned,
/// and that is a shape worth round-tripping.
fn string_lit(src: &mut Entropy<'_>) -> String {
    let quote = if src.chance(48) { '`' } else { '"' };
    let mut out = String::new();
    out.push(quote);
    for _ in 0..src.count(3) {
        match src.choice(5) {
            0 => out.push_str("text "),
            1 => out.push_str(&format!("${{{}}}", lower(src))),
            2 => out.push_str(&format!("${{{}({})}}", lower(src), lower(src))),
            // A hole holding a string holding a hole. The scanner has to track
            // the nested quote to find the end of the outer literal, and a
            // version of this scan in `khora-hir` got it wrong as recently as
            // this week.
            3 => out.push_str(&format!("${{f(\"${{{}}}\")}}", lower(src))),
            // Braces that are not a hole. Harmless, and the reason the scanner
            // counts rather than searching for the first `}`.
            _ => out.push_str("{}"),
        }
    }
    out.push(quote);
    out
}

// --- patterns --------------------------------------------------------------

fn pattern(src: &mut Entropy<'_>) -> String {
    let Some(mut src) = src.descend() else {
        return "_".to_string();
    };
    match src.choice(8) {
        0 => "_".to_string(),
        1 => lower(&mut src).to_string(),
        // No `DECIMAL_LIT`: `1.5d` is a literal in every position but this one.
        2 => format!("{}", src.choice(100)),
        3 => "\"text\"".to_string(),
        4 => ["true", "false", "'a'"][src.choice(3)].to_string(),
        5 => format!("{}::{}", upper(&mut src), upper(&mut src)),
        6 => format!("{}::{}({})", upper(&mut src), upper(&mut src), pattern(&mut src)),
        _ => format!("({}, {})", pattern(&mut src), pattern(&mut src)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The empty seed is the shrink target**, so it is worth pinning: this
    /// is the program `proptest` reports when it has shrunk a failure all the
    /// way down, and every alternative in this file is ordered to reach it.
    #[test]
    fn an_empty_seed_is_the_smallest_program() {
        let mut src = Entropy::new(&[]);
        assert_eq!(program(&mut src), "module m;

const a = 0;
");
    }

    /// A short seed produces a short program. The fuel budget only bounds the
    /// top end; what keeps the typical case small is that the seed runs out
    /// and every choice after that takes arm 0.
    #[test]
    fn a_long_seed_still_produces_a_bounded_program() {
        let seed: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let mut src = Entropy::new(&seed);
        let text = program(&mut src);
        assert!(text.len() < 64 * 1024, "{} bytes is more than the fuel allows", text.len());
    }
}
