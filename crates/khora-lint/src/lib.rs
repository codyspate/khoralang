//! Lints that need types.
//!
//! An unreachable `match` arm is deliberately *not* here: it is a type error,
//! reported by `khora-types` out of the same usefulness algorithm that decides
//! exhaustiveness, and making it a lint as well would give one mistake two
//! voices.
//!
//! # What separates a lint from an error here
//!
//! An error is a program the compiler will not compile. A lint is one it will
//! compile and that somebody probably did not mean, and everything here is on
//! that side of the line for the same reason: each is *legal and occasionally
//! deliberate*. A capability may be declared to keep a signature uniform across
//! a family of handlers; a statement that computes nothing may be a placeholder
//! mid-edit.
//!
//! Each is also chosen to have **no false positives**. A warning people learn
//! to ignore is worse than no warning, and the way that starts is one that is
//! wrong about real code — so where a judgement was available, this takes the
//! quiet side.
//!
//! # Levels
//!
//! Each lint has a kebab-case name, which is what `[lints]` in `khora.toml`
//! addresses:
//!
//! ```toml
//! [lints]
//! unused-capability = "deny"
//! ```
//!
//! Reading that table is the caller's job. This crate reports what it finds and
//! has no opinion about how loud it is — the manifest is one project's policy
//! and these are facts about a file.

#![deny(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

mod allow;
mod exported;

pub use crate::allow::MARKER;

use khora_db::{Db, SourceFile};
use khora_types::{BodyTypes, Type};
use khora_hir::body::{Body, Expr, LocalId, Pat, Stmt};
use text_size::TextRange;

/// Something a program does that it probably did not mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The kebab-case name `[lints]` addresses.
    pub lint: &'static str,
    /// What to tell the reader, in one sentence.
    pub message: String,
    /// Where in the file to point.
    pub range: TextRange,
}

/// Every lint's name, so that a manifest naming one that does not exist can be
/// told what does.
pub const LINTS: &[&str] = &[
    DANGLING_EXPRESSION,
    DISCARDED_RESULT,
    INCONSISTENT_CONSTRUCTOR,
    MISPLACED_MAIN,
    REFERENCE_CYCLE,
    UNDOCUMENTED_EXPORT,
    UNKNOWN_ALLOW,
    UNREACHABLE_CODE,
    UNUSED_BINDING,
    UNUSED_CAPABILITY,
    UNUSED_IMPORT,
    USELESS_ALLOW,
];

/// A `main` in a file that is not an entry point.
///
/// **A package has one program, and one file that starts it.** Nothing said
/// so, so a `main` in `src/helpers.kh` was picked up and run as the program's
/// entry point, and a second one somewhere else meant whichever the compiler
/// reached first won. A reader looking for what a package does had to search
/// every file to find out where it begins.
///
/// `src/main.kh` is that file. A package with more than one program puts each
/// under `src/bin/`, the way Cargo does, and each becomes its own executable.
///
/// Only inside a `src` directory: a loose `.kh` file run directly is a script
/// and has no layout to disagree with.
pub const MISPLACED_MAIN: &str = "misplaced-main";

/// A constructor whose name disagrees with what it takes.
///
/// `new`, `empty`, `root` and `of` follow a rule `std` already keeps and had
/// written down nowhere until `docs/design/naming.md`. Roadmap 14.25.
pub const INCONSISTENT_CONSTRUCTOR: &str = "inconsistent-constructor";

/// A `pub` item nobody described in one line.
///
/// **Off by default**, for the reason Rust's `missing_docs` is: a young
/// package gets forty warnings on the first build, and the response to forty
/// warnings is not to write forty doc comments. Switch it on with
/// `[lints] undocumented-export = "warn"` when the package decides its
/// surface is a promise. Roadmap 14.26.
pub const UNDOCUMENTED_EXPORT: &str = "undocumented-export";

/// An imported name the file never mentions.
///
/// The most-fired lint in every language that has one. Roadmap 14.22.
pub const UNUSED_IMPORT: &str = "unused-import";

/// A binding nothing reads.
///
/// Locals, parameters, and the names a pattern binds. `_` is the escape, and
/// so is a leading underscore — see [`unused_bindings`]. Roadmap 14.23.
pub const UNUSED_BINDING: &str = "unused-binding";

/// A statement that cannot run, because the one before it left the block.
///
/// Cheap and always a mistake: nobody writes a line after a `return` on
/// purpose. Roadmap 14.24.
pub const UNREACHABLE_CODE: &str = "unreachable-code";

/// A `// @klint allow` naming something that is not a lint.
///
/// **This is what makes the pragma safe to have.** A misspelled name in a
/// comment would otherwise suppress nothing and say nothing, and the reader
/// would believe the line was handled. `docs/design/lint-hatch.md`.
pub const UNKNOWN_ALLOW: &str = "unknown-allow";

/// A `// @klint allow` that suppressed nothing.
///
/// Stale suppression is real debt: it hides the next finding on that line, and
/// it tells a reader that something was considered when it no longer is.
///
/// **Off by default**, unlike every other lint here. It fires on exactly the
/// lines somebody is already editing to satisfy a new lint, so switching it on
/// while lints are still being added would produce churn in the files under
/// the most pressure. Turn it on with `[lints] useless-allow = "warn"` once
/// they have settled.
pub const USELESS_ALLOW: &str = "useless-allow";

/// How loud a lint is when the manifest does not say.
///
/// Warn for everything except [`USELESS_ALLOW`], for the reason on it. A
/// function rather than a constant so that both the CLI and the language
/// server ask the same question -- they each had `unwrap_or(Warn)` written out
/// before this existed, which is two places to forget.
pub fn default_level(lint: &str) -> khora_manifest::LintLevel {
    if lint == USELESS_ALLOW || lint == UNDOCUMENTED_EXPORT {
        khora_manifest::LintLevel::Allow
    } else {
        khora_manifest::LintLevel::Warn
    }
}

/// A capability a signature asks for that its body cannot be using.
pub const UNUSED_CAPABILITY: &str = "unused-capability";
/// A statement that computes something and does nothing with it.
pub const DANGLING_EXPRESSION: &str = "dangling-expression";
/// A statement that produces a `Result` and drops it on the floor.
pub const DISCARDED_RESULT: &str = "discarded-result";
/// Named for the problem rather than for the fix.
///
/// The advice this gives will change — today it is "restructure or accept the
/// leak", and when weak references exist it becomes "make this field weak".
///
/// A lint's name goes in somebody's `khora.toml`, so a name that describes the
/// *remedy* would have to be renamed when the remedy changes, and renaming one
/// breaks every manifest that mentions it. `reference-cycle` is true either
/// way. `docs/roadmap.md` Phase 13.
pub const REFERENCE_CYCLE: &str = "reference-cycle";

/// What the lints find in one file.
///
/// Sorted by position, because a reader goes through a file from the top and a
/// list in pass order makes them jump.
#[salsa::tracked(returns(ref))]
pub fn findings(db: &dyn Db, file: SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    let checked = khora_types::checked(db, file);
    for (name, body) in khora_hir::body::bodies(db, file) {
        dangling_expressions(body, &mut out);
        // Paired by name, which is how `Checked` keys them. A body with no
        // types — one whose `derive` was refused, say — is skipped rather than
        // guessed at: `reference_cycles` needs to know what is on the heap and
        // has nothing useful to say without it.
        unreachable_code(body, &mut out);
        unused_bindings(body, &mut out);
        if let Some((_, types)) = checked.bodies.iter().find(|(n, _)| n == name) {
            unused_capabilities(body, types, &mut out);
            reference_cycles(body, types, &mut out);
            discarded_results(body, types, &mut out);
        }
    }

    unused_imports(db, file, checked, &mut out);
    undocumented_exports(db, file, &mut out);
    misplaced_main(db, file, &mut out);

    // **Here rather than in each consumer.** The CLI, the language server and
    // the MCP server all read this, and a suppression one of them honoured and
    // another did not would be the worst kind of inconsistency: the editor
    // says the line is fine and the build does not.
    let text = file.text(db);
    out = suppress(text, out);

    out.sort_by_key(|f| (f.range.start(), f.range.end()));
    out
}

/// Drops what the pragmas allow, and reports on the pragmas themselves.
fn suppress(text: &str, found: Vec<Finding>) -> Vec<Finding> {
    let mut allows = allow::allows(text);
    if allows.is_empty() {
        return found;
    }
    let starts = line_starts(text);

    let mut kept = Vec::new();
    for finding in found {
        let line = line_of(&starts, u32::from(finding.range.start())) as u32;
        let hit = allows
            .iter_mut()
            .find(|allow| allow.line == line && allow.lint == finding.lint);
        match hit {
            Some(allow) => allow.used = true,
            None => kept.push(finding),
        }
    }

    for allow in &allows {
        if !LINTS.contains(&allow.lint.as_str()) {
            kept.push(Finding {
                lint: UNKNOWN_ALLOW,
                message: format!(
                    "`{}` is not a lint, so this allows nothing. What there is: {}",
                    allow.lint,
                    LINTS.join(", ")
                ),
                range: allow.range,
            });
        } else if !allow.used {
            kept.push(Finding {
                lint: USELESS_ALLOW,
                message: format!("nothing here reports `{}`, so this allows nothing", allow.lint),
                range: allow.range,
            });
        }
    }
    kept
}

/// The byte offset each line starts at.
fn line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(at as u32 + 1);
        }
    }
    starts
}

/// Which line an offset is on, zero-based.
fn line_of(starts: &[u32], offset: u32) -> usize {
    match starts.binary_search(&offset) {
        Ok(exact) => exact,
        Err(after) => after - 1,
    }
}

/// Reports a `main` in a file that is not an entry point.
///
/// The rule is the layout's, so it is asked of the path rather than the tree:
/// `src/main.kh` is the program, `src/bin/anything.kh` is one of the others,
/// and anywhere else under `src` the function is either dead or the program's
/// real entry point hiding in a file nobody would look in.
///
/// **`src/bin/` was allowed here before it existed.** For a while the lint, its
/// message and the backend disagreed three ways: this exempted `src/bin/*.kh`,
/// the message told people to put a second program there, and the backend
/// compiled every `main` it found into one program and refused — so
/// `khora check` passed on the layout the message recommended and `khora build`
/// then failed with the error the message was trying to help with. The exemption
/// was withdrawn until the layout was real. It is real now: each file in
/// `src/bin` is its own compilation, with the package's modules and not with the
/// other programs.
fn misplaced_main(db: &dyn Db, file: SourceFile, out: &mut Vec<Finding>) {
    let path = file.path(db);
    let parts: Vec<String> =
        path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    // **Only where there is a layout to disagree with.** A `.kh` file run
    // directly is a script, and telling somebody their one-file program is in
    // the wrong place would be worse than saying nothing.
    let Some(src) = parts.iter().rposition(|part| part == "src") else { return };
    let after: &[String] = &parts[src + 1..];
    let allowed = match after {
        [only] => only == "main.kh",
        [folder, _file] => folder == "bin",
        _ => false,
    };
    if allowed {
        return;
    }

    let parse = khora_db::parse(db, file);
    if !parse.errors().is_empty() {
        return;
    }
    for found in exported::functions_named(&parse.syntax(), "main") {
        out.push(Finding {
            lint: MISPLACED_MAIN,
            message: "`main` is the program's entry point and belongs in `src/main.kh`. \
                      A package's other programs go one per file in `src/bin/`"
                .to_string(),
            range: found,
        });
    }
}

/// Public declarations with no `///` above them.
fn undocumented_exports(db: &dyn Db, file: SourceFile, out: &mut Vec<Finding>) {
    let parse = khora_db::parse(db, file);
    if !parse.errors().is_empty() {
        // A file that does not parse has a tree full of holes, and reporting a
        // missing doc comment on a node that is really a syntax error is noise
        // on top of a message the reader already has.
        return;
    }
    for bad in exported::misnamed_constructors(&parse.syntax()) {
        out.push(Finding {
            lint: INCONSISTENT_CONSTRUCTOR,
            message: bad.message,
            range: bad.range,
        });
    }

    for export in exported::undocumented(&parse.syntax()) {
        let named = match &export.name {
            Some(name) => format!("`{name}`"),
            None => format!("this {}", export.what),
        };
        out.push(Finding {
            lint: UNDOCUMENTED_EXPORT,
            message: format!(
                "{named} is exported and has no `///` line. Describe what it is for, or stop exporting it"
            ),
            range: export.range,
        });
    }
}

/// Imported names the file never mentions.
///
/// # Two ways a name can be used, and the second is not lexical
///
/// **Written down.** The name appears as an identifier token somewhere outside
/// the import statements. Not "resolves to the import" — that would mean
/// enumerating every position a name can be resolved from: expressions,
/// patterns, type annotations, trait bounds, `impl` heads. Missing one is a
/// false report, and a false report here is expensive, because this is the
/// lint people meet first and the fix it suggests is deleting a line.
///
/// **Or never written at all.** This one cost a corpus-wide revert to find:
///
/// ```khora
/// import postgres::conn::{Answer, ask};
/// // ...
/// Result::Ok(answer) => Result::Ok(answer.rows),
/// ```
///
/// `Answer` appears nowhere but the import, and deleting it breaks the file —
/// `answer.rows` needs the type in scope to know it has fields. The type is
/// *inferred*, so nothing writes its name.
///
/// So a type also counts as used when **some expression or binding in the file
/// has that type**, which the checker knows and nothing was asking. Types are
/// compared by their rendering, which over-matches — a type parameter named
/// `Answer` would count — and over-matching means a miss, which is the safe
/// direction.
///
/// The lexical half is wrong in the same direction: a local variable sharing a
/// name with an unused import silences it. A miss annoys nobody; a false
/// report gets the lint switched off.
///
/// # What it will not touch
///
/// **A glob import.** `import a.*` names nothing, so there is nothing to check
/// and nothing to suggest deleting.
///
/// **The last used name from an import statement.** `khora_types::map`'s
/// `import_inherent` runs once per imported *origin* and copies the defining
/// module's inherent methods into this file's view — so an import statement
/// can be load-bearing for `value.method()` without any of its names being
/// mentioned. Reporting the last name of a statement could therefore suggest
/// deleting a line that a method call depends on invisibly.
///
/// So a name is only reported when **another name in the same statement is
/// still used**, which keeps the statement — and its methods — in place. The
/// whole-statement case is left alone until `import_inherent` is keyed on the
/// type rather than on the module, which its own doc comment says is the
/// intent: methods should arrive "whether or not the file imported `Params`".
/// Whether the file contains a `for` loop.
///
/// Asked of the tree rather than the token stream, because `for` is a
/// contextual keyword: `handler for Ledger` is not a loop, and counting it
/// would silence the lint on two imports the file may genuinely not use.
fn has_for_loop(db: &dyn Db, file: SourceFile) -> bool {
    khora_db::parse(db, file)
        .syntax()
        .descendants()
        .any(|node| node.kind() == khora_syntax::SyntaxKind::FOR_EXPR)
}

fn unused_imports(
    db: &dyn Db,
    file: SourceFile,
    checked: &khora_types::Checked,
    out: &mut Vec<Finding>,
) {
    let items = khora_hir::item_map(db, file);
    if items.imports.is_empty() {
        return;
    }

    // Every identifier in the file, and where. Import statements are excluded
    // by range, so an import naming itself does not count as using itself.
    let text = file.text(db);
    let lexed = khora_syntax::LexedStr::new(text);
    let mut mentioned: Vec<&str> = Vec::new();
    // **And every identifier inside a `${..}` hole**, which the loop above
    // cannot see: a hole's contents are ordinary Khora living inside one
    // `STRING_LIT`, so `"${quoted(c)}"` is a string to a token walk and a call
    // to a reader. Three imports in `examples/khq` were used exactly once,
    // each in a hole, and all three were reported as never used.
    //
    // Owned, because they are lexed out of a temporary rather than borrowed
    // from the file. `used` checks both lists.
    let mut in_holes: Vec<String> = Vec::new();
    for index in 0..lexed.len() {
        let at = lexed.range(index);
        if items.imports.iter().any(|import| import.range.contains_range(at)) {
            continue;
        }
        match lexed.kind(index) {
            khora_syntax::SyntaxKind::IDENT => mentioned.push(lexed.text(index)),
            khora_syntax::SyntaxKind::STRING_LIT => {
                for hole in khora_hir::body::interpolation_holes(lexed.text(index)) {
                    let inner = khora_syntax::LexedStr::new(&hole);
                    for i in 0..inner.len() {
                        if inner.kind(i) == khora_syntax::SyntaxKind::IDENT {
                            in_holes.push(inner.text(i).to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // **A `for` loop uses two names it does not write.** It expands into a
    // `match` on `Step` and a call to `Iterator::next`, both resolved through
    // the ordinary scope, so both have to be imported -- and neither appears
    // in the token stream, so both looked unused. Reporting them told the
    // reader to delete the imports that make the loop compile. Errata 58.
    if has_for_loop(db, file) {
        mentioned.push("Step");
        mentioned.push("Iterator");
    }

    // **A method call uses the trait that declares the method, and never
    // writes its name.** `r.area()` mentions `area`; nothing in the file
    // mentions `Area`, and the type of the call is whatever `area` returns —
    // so the trait that has to be in scope for the call to resolve looked
    // unused from both of the places above.
    //
    // Reporting it was worse than a wrong warning. Following the advice broke
    // the program, and what came back was the compiler's own "this is a gap in
    // the compiler worth reporting" message — so a reader who did as they were
    // told was then told to file a bug about it.
    //
    // Every trait declaring a method of that name counts, not the one the call
    // actually resolved to. Over-counting makes this lint quieter and cannot
    // make it wrong: the failure it exists to avoid is calling a live import
    // dead, and a name that only *might* be the one in use is not evidence
    // that it is not.
    // Names in method position: an identifier whose previous meaningful token
    // is a `.`. Narrower than "every identifier", which would let a trait with
    // a method called `show` be kept alive by any use of the word anywhere.
    let traits = &khora_types::type_map(db, file).traits.traits;
    let mut called: Vec<&str> = Vec::new();
    let mut previous = None;
    for index in 0..lexed.len() {
        let kind = lexed.kind(index);
        if kind.is_trivia() {
            continue;
        }
        if kind == khora_syntax::SyntaxKind::IDENT
            && previous == Some(khora_syntax::SyntaxKind::DOT)
        {
            let at = lexed.range(index);
            if !items.imports.iter().any(|import| import.range.contains_range(at)) {
                called.push(lexed.text(index));
            }
        }
        previous = Some(kind);
    }
    let mut through_methods: Vec<String> = Vec::new();
    for (name, def) in traits {
        if def.methods.iter().any(|m| called.contains(&m.name.as_str())) {
            through_methods.push(name.clone());
        }
    }

    // Every name that shows up in the *type* of anything in this file. See
    // the doc comment: a type reached only through a value is never written.
    let mut in_types: Vec<String> = Vec::new();
    for (name, body) in khora_hir::body::bodies(db, file) {
        let Some((_, types)) = checked.bodies.iter().find(|(n, _)| n == name) else { continue };
        let mut rendered = String::new();
        for (id, _) in body.exprs() {
            rendered.push_str(&types.of(id).to_string());
            rendered.push(' ');
        }
        for (id, _) in body.locals() {
            rendered.push_str(&types.local(id).to_string());
            rendered.push(' ');
        }
        for word in rendered.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if !word.is_empty() {
                in_types.push(word.to_string());
            }
        }
    }

    for import in &items.imports {
        let khora_hir::ImportKind::Named(names) = &import.kind else { continue };
        let used = |name: &khora_hir::ImportedName| {
            mentioned.contains(&name.alias.as_str())
                || in_holes.iter().any(|seen| seen == &name.alias)
                || in_types.iter().any(|seen| seen == &name.alias)
                || through_methods.iter().any(|seen| seen == &name.alias)
        };
        // See the doc comment: without a surviving name, deleting the reported
        // one could take the statement's methods with it.
        if !names.iter().any(used) {
            continue;
        }
        for name in names {
            if used(name) {
                continue;
            }
            out.push(Finding {
                lint: UNUSED_IMPORT,
                message: format!("`{}` is imported and never used", name.alias),
                range: name.range,
            });
        }
    }
}

/// Bindings nothing reads.
///
/// # What counts as reading one
///
/// Any mention. **Including the target of an assignment**, which means
/// `let mut n = 1; n = 2;` with no read does not fire. That is a miss rather
/// than a false report, and deliberately: "assigned and never read" is a
/// different lint about a different mistake, and catching it here by accident
/// would mean reporting it with this one's message.
///
/// # Two escapes, and why there are two
///
/// A `_` pattern binds nothing at all, so it never reaches this — that is the
/// escape the language already had, from `let _ = f()`.
///
/// A name *starting* with `_` also silences it, and that is a convention this
/// lint introduces. The reason is parameters: a function implementing a trait
/// or matching a callback shape often cannot use an argument and still wants
/// to *name* it, because the name is what tells the next reader what the
/// argument is. Forcing `_` there throws away documentation to satisfy a lint,
/// which is the wrong trade.
///
/// # What it leaves alone
///
/// Capability bindings from a `with` row. Those are [`UNUSED_CAPABILITY`]'s,
/// and reporting one thing twice under two names is worse than reporting it
/// once under the right one.
fn unused_bindings(body: &Body, out: &mut Vec<Finding>) {
    let mut read: Vec<LocalId> = Vec::new();
    for (_, expr) in body.exprs() {
        if let Expr::Local(local) = expr {
            read.push(*local);
        }
    }

    // Whatever the `with` row bound, by the pattern it bound it through.
    let mut capabilities: Vec<LocalId> = Vec::new();
    for (_, pat) in &body.evidence {
        if let Pat::Bind(local) = body.pat(*pat) {
            capabilities.push(*local);
        }
    }

    // **And whatever a `with` *block* installs.** Such a block lowers to
    // ordinary `let`s, so its labels look like bindings -- but a capability is
    // used by calling something that requires it, not by naming it, so a
    // `with { clock: Clock::real() }` around code that reads the clock has no
    // mention of `clock` anywhere. Reporting those was this lint's one class
    // of false positive, and it fired on every reference application.
    //
    // By name rather than by binding, because `installs` records the labels
    // and the `let`s they lower to are not distinguishable afterwards. That
    // over-excludes an ordinary `let clock = ...` in a body that also installs
    // `clock`, which is a miss and not a wrong report.
    let installed: Vec<&str> =
        body.installs.values().flatten().map(String::as_str).collect();

    for (id, local) in body.locals() {
        if local.name.starts_with('_')
            || read.contains(&id)
            || capabilities.contains(&id)
            || installed.contains(&local.name.as_str())
        {
            continue;
        }
        out.push(Finding {
            lint: UNUSED_BINDING,
            message: format!(
                "`{}` is bound and never read. Delete it, or rename it to `_{}` if it has \
                 to stay",
                local.name, local.name
            ),
            range: local.range,
        });
    }
}

/// Statements after the one that leaves the block.
///
/// **One finding per block, not one per statement.** Three lines after a
/// `return` are one mistake, and three warnings about it is the kind of
/// output people learn to scroll past. The range covers the first dead
/// statement, which is where the reader has to start deleting.
///
/// `break` and `continue` count as leaving. They do not leave the *function*,
/// but they leave the block, and the statement after one is just as dead.
///
/// **The caret is narrow when the dead statement is a `let`.** `Body` carries
/// a range per *expression* and none per statement, so the best available
/// anchor is the initializer -- right line, narrow mark. Widening it wants
/// statement ranges in the HIR, which several diagnostics would use and none
/// has needed enough to add.
fn unreachable_code(body: &Body, out: &mut Vec<Finding>) {
    for (_, expr) in body.exprs() {
        let Expr::Block { stmts, tail } = expr else { continue };

        // The statement that ends the block, and what it was.
        let mut ends_at: Option<(usize, &'static str)> = None;
        for (at, stmt) in stmts.iter().enumerate() {
            let Stmt::Expr(id) = stmt else { continue };
            if let Some(how) = leaves(body.expr(*id)) {
                ends_at = Some((at, how));
                break;
            }
        }
        let Some((at, how)) = ends_at else { continue };

        // What follows it: the rest of the statements, then the tail.
        let dead = stmts.get(at + 1).and_then(|stmt| match stmt {
            Stmt::Expr(id) => Some(*id),
            Stmt::Let { init, .. } => *init,
        });
        let dead = dead.or(if at + 1 == stmts.len() { *tail } else { None });
        let Some(dead) = dead else { continue };

        out.push(Finding {
            lint: UNREACHABLE_CODE,
            message: format!(
                "this cannot run: the `{how}` above it always leaves the block first"
            ),
            range: body.range(dead),
        });
    }
}

/// How an expression leaves the block it is a statement of, if it does.
///
/// Deliberately only the four that always do. An `if` where both arms return
/// also leaves, and a `match` where every arm does, and working that out is a
/// reachability analysis rather than a look at one node — worth having, and
/// worth having as its own thing rather than smuggled into a lint. Until then
/// this is quiet about them, which is the right direction to be wrong in:
/// a lint that misses a case annoys nobody, and one that reports live code
/// gets switched off.
fn leaves(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Return(_) => Some("return"),
        Expr::Raise(_) => Some("raise"),
        Expr::Break(_) => Some("break"),
        Expr::Continue => Some("continue"),
        // Not written by anybody, so there is no source construct to name.
        Expr::Shown(_) => None,
        _ => None,
    }
}

/// A capability the signature asks for and the body cannot be using.
///
/// **Forwarding is what makes this hard.** A capability can be read outright —
/// `rng.int()` — or passed along without being named, since calling a function
/// that itself requires `Random` hands this one over with no `Expr::Local`
/// anywhere. A pass that looked only at reads would report every pass-through
/// function in the standard library.
///
/// This used to give up entirely whenever a body contained a call, which meant
/// it was silent about almost every real function. Roadmap 14.27 named the fix
/// and it is now in place: `BodyTypes::call_rows` records, per call site, the
/// labels the callee required — before any `with` block or `catch` discharges
/// them, so it is what the *call* needed rather than what survived to the
/// signature.
///
/// So "used" is now **read outright, or required by something this body
/// calls**, and a body full of calls is no longer beyond reach:
///
/// ```khora
/// // Reported: nothing here needs a clock.
/// fn area(r: Int) -> Int with { clock: Clock } { r * r }
///
/// // Not reported: `read` requires `fs`, so this forwards it.
/// fn load(p: Path) -> String with { fs: Fs } { read(p) }
///
/// // Reported again: `parse` needs nothing, so `fs` buys its callers nothing.
/// fn count(t: String) -> Int with { fs: Fs } { parse(t).length() }
/// ```
///
/// # What it still gives up on
///
/// A call whose required row is *open* — a type variable tail, which is what a
/// generic function's row looks like before it is instantiated — could require
/// anything. Those are treated as forwarding every label, because the
/// alternative is reporting a capability that a caller's instantiation does
/// need. Quiet is the right direction to be wrong in here: this lint costs a
/// caller nothing when it misses and costs them a signature change when it is
/// wrong.
fn unused_capabilities(body: &Body, types: &BodyTypes, out: &mut Vec<Finding>) {
    if body.evidence.is_empty() {
        return;
    }

    let mut read: Vec<LocalId> = Vec::new();
    for (_, expr) in body.exprs() {
        if let Expr::Local(local) = expr {
            read.push(*local);
        }
    }

    // Every label some call in this body demanded, and whether any call's
    // demand was open-ended.
    let mut forwarded: Vec<&str> = Vec::new();
    let mut anything_possible = false;
    for (_, rows) in types.calls_with_rows() {
        let Some(Type::Row { fields, tail }) = rows.requires.as_ref() else { continue };
        for (label, _) in fields {
            forwarded.push(label);
        }
        if tail.is_some() {
            anything_possible = true;
        }
    }
    if anything_possible {
        return;
    }

    for (label, pat) in &body.evidence {
        let Pat::Bind(local) = body.pat(*pat) else { continue };
        if read.contains(local) || forwarded.contains(&label.as_str()) {
            continue;
        }
        out.push(Finding {
            lint: UNUSED_CAPABILITY,
            message: format!(
                "`{label}` is required by this signature and this body cannot be using it. \
                 Every caller has to supply it, so asking for it costs them something and \
                 buys nothing"
            ),
            range: body.local(*local).range,
        });
    }
}

/// A statement that computes a value and throws it away.
///
/// `x + 1;` in the middle of a block is almost always a line somebody meant to
/// bind, return, or finish. It is legal, and it does nothing at all.
///
/// **Deliberately syntactic, and deliberately narrow.** Only an expression that
/// *cannot* do anything is reported: no call, no assignment, no `!`, nothing
/// that could raise. That rules out the interesting judgement calls — a call
/// whose result is ignored is often exactly right, and deciding which ones are
/// not needs to know whether the callee does anything, which is a purity
/// analysis rather than a lint. This is the subset where being wrong is
/// impossible, and a lint people trust is worth more than a lint that catches
/// everything.
///
/// The tail expression of a block is never reported: that is the block's value.
fn dangling_expressions(body: &Body, out: &mut Vec<Finding>) {
    for (_, expr) in body.exprs() {
        let Expr::Block { stmts, .. } = expr else { continue };
        for stmt in stmts {
            let Stmt::Expr(id) = stmt else { continue };
            if !is_inert(body, *id) {
                continue;
            }
            out.push(Finding {
                lint: DANGLING_EXPRESSION,
                message: "this computes a value and then discards it, so the line does \
                          nothing. Bind it with `let`, return it, or delete it"
                    .to_string(),
                range: body.range(*id),
            });
        }
    }
}

/// Whether an expression is incapable of doing anything but produce a value.
///
/// Conservative in one direction only: a `false` here is always safe, and it is
/// what every unlisted form gets. See [`dangling_expressions`].
fn is_inert(body: &Body, id: khora_hir::body::ExprId) -> bool {
    match body.expr(id) {
        // A literal or a read. Nothing observable happens.
        Expr::Literal(_) | Expr::Local(_) => true,
        // Reading a field cannot run anything: there are no property accessors.
        Expr::Field { base, .. } => is_inert(body, *base),
        // Arithmetic and comparison over inert operands. `&&` and `||` are
        // included: they are lazy, but what they are lazy about is also inert.
        Expr::Binary { lhs, rhs, .. } => is_inert(body, *lhs) && is_inert(body, *rhs),
        Expr::Unary { operand, .. } => is_inert(body, *operand),
        // Everything else — calls, assignment, `if`, `match`, `while`, `with`,
        // `!` — either does something or might. `Missing` and `Unresolved` are
        // parse and resolution failures, and a lint on top of an error is
        // noise.
        _ => false,
    }
}

// --- a `Result` nobody looked at --------------------------------------------

/// A statement that produces a `Result` and drops it on the floor.
///
/// **`expr!` is a mark on the effect row and the identity on values**, so
/// `db.execute(sql, binds)!` as a statement does nothing about the `Result` it
/// returns. That has twice reported success against a database that had aborted
/// the transaction or was not running at all: the outer half of the answer read
/// fine, and the half saying what happened went on the floor.
///
/// A lint rather than an error, because dropping one is occasionally deliberate
/// and the language already says so with `let _ = db.rollback();` — which
/// `std::db`'s rollback path uses, so the engine's complaint about the rollback
/// cannot hide the reason for it.
///
/// # What it sees
///
/// A statement-position expression whose type is a `Result`, anywhere but the
/// tail of a block — the tail is the block's value and is not discarded.
/// Matched on the name rather than on the declaring module: a `Result` a
/// program declared itself is the same mistake, and the checker has already
/// agreed the name refers to one type.
fn discarded_results(body: &Body, types: &BodyTypes, out: &mut Vec<Finding>) {
    for (_, expr) in body.exprs() {
        let Expr::Block { stmts, .. } = expr else { continue };
        for stmt in stmts {
            let Stmt::Expr(id) = stmt else { continue };
            let Type::Adt { name, .. } = types.of(*id) else { continue };
            if name != "Result" {
                continue;
            }
            out.push(Finding {
                lint: DISCARDED_RESULT,
                message: "this produces a `Result` and nothing looks at it, so a failure here is silent. `match` it, mark it with `!` in a function that can raise, or write `let _ =` to say the answer was considered"
                    .to_string(),
                range: body.range(*id),
            });
        }
    }
}

// --- reference cycles ------------------------------------------------------

/// A field assignment that closes a loop in the heap.
///
/// **Why this is worth a lint at all.** Reference counting works for every
/// shape of data except a loop: in `a.next = b; b.next = a` each object holds
/// the other, so neither count reaches zero. `docs/design/memory.md` §4 rules
/// out a tracing collector and names weak references as what breaks a cycle —
/// and weak references do not exist yet, while mutable fields do, so the cycle
/// compiles today with nothing to reach for instead.
///
/// The failure is **silent**: nothing is freed early, nothing is read after
/// free, the memory is simply never returned. This is the diagnostic
/// `memory.md` §4 asks for.
///
/// # What it sees, and what it does not
///
/// One function body, and reachability built from what that body does:
/// constructing a value out of a local, and assigning a local into a field.
/// It warns when a field assignment stores something that can already reach
/// the object being assigned into.
///
/// It does **not** see across function boundaries, through a `Shared` cell, or
/// through a collection. A cycle built in two functions is invisible to it.
/// That is the honest limit of a syntactic pass and the reason this is a
/// warning rather than an error: it finds the accident, and says nothing about
/// what it cannot see.
///
/// **No false positives is the harder half.** A lint people learn to ignore is
/// worse than no lint, and the way that starts is one that is wrong about real
/// code — so where a judgement was available this takes the quiet side, as the
/// other two passes here do.
fn reference_cycles(body: &Body, types: &BodyTypes, out: &mut Vec<Finding>) {
    let Some(root) = body.root else { return };
    let mut walk = Cycles { body, types, reaches: BTreeMap::new(), out };
    walk.expr(root);
}

/// The walk `reference_cycles` runs.
///
/// **Structural, from the root, and not a scan of the arena.** The first
/// version iterated `body.exprs()`, which is allocation order rather than
/// program order: lowering is depth-first and append-only, so a block is
/// created *after* every statement inside it. An assignment was therefore seen
/// before the `let` two lines above it, and the edge that made the cycle had
/// not been recorded yet — the pass missed the shape it exists to catch and
/// reported nothing, which is the worst way for a lint to be wrong.
struct Cycles<'a> {
    body: &'a Body,
    types: &'a BodyTypes,
    /// What each local can reach, as far as the walk has got.
    ///
    /// `BTreeMap` and `BTreeSet` rather than the hashed pair: a `HashSet`'s
    /// per-process seed leaking into compiler output is a bug this repository
    /// has already had once, in `khora-perceus`, and findings are ordered.
    reaches: BTreeMap<LocalId, BTreeSet<LocalId>>,
    out: &'a mut Vec<Finding>,
}

impl Cycles<'_> {
    fn expr(&mut self, id: khora_hir::body::ExprId) {
        match self.body.expr(id).clone() {
            Expr::Block { stmts, tail } => {
                for stmt in &stmts {
                    match stmt {
                        Stmt::Let { pat, init, .. } => {
                            let Some(init) = init else { continue };
                            self.expr(*init);
                            // Recorded *after* the initializer is walked, so a
                            // binding cannot reach itself through its own
                            // right-hand side.
                            if let Pat::Bind(local) = self.body.pat(*pat) {
                                let mut named = BTreeSet::new();
                                locals_in(self.body, *init, &mut named);
                                self.reaches.entry(*local).or_default().extend(named);
                            }
                        }
                        Stmt::Expr(e) => self.expr(*e),
                    }
                }
                if let Some(tail) = tail {
                    self.expr(tail);
                }
            }
            Expr::Assign { target, value } => {
                self.expr(target);
                self.expr(value);
                self.assignment(target, value);
            }
            Expr::Call { callee, args } => {
                self.expr(callee);
                for arg in args {
                    self.expr(arg);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Unary { operand, .. } => self.expr(operand),
            Expr::Field { base, .. } => self.expr(base),
            Expr::If { condition, then_branch, else_branch } => {
                self.expr(condition);
                self.expr(then_branch);
                if let Some(other) = else_branch {
                    self.expr(other);
                }
            }
            Expr::While { condition, body } => {
                self.expr(condition);
                self.expr(body);
            }
            Expr::Loop { body } => self.expr(body),
            Expr::Match { scrutinee, arms } | Expr::Catch { inner: scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in &arms {
                    if let Some(guard) = arm.guard {
                        self.expr(guard);
                    }
                    self.expr(arm.body);
                }
            }
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::Record { fields, .. } => {
                for (_, value) in &fields {
                    self.expr(*value);
                }
            }
            Expr::Tuple(items) => {
                for item in &items {
                    self.expr(*item);
                }
            }
            Expr::Return(inner) | Expr::Break(inner) => {
                if let Some(inner) = inner {
                    self.expr(inner);
                }
            }
            Expr::Try(inner) | Expr::Raise(inner) => self.expr(inner),
            _ => {}
        }
    }

    /// `target.field = value`: does it close a loop?
    fn assignment(&mut self, target: khora_hir::body::ExprId, value: khora_hir::body::ExprId) {
        let Expr::Field { base, name } = self.body.expr(target).clone() else { return };
        let Some(into) = root_local(self.body, base) else { return };

        // **Only a heap value can be part of a loop**, and this is where the
        // first version was wrong about real code. `self.wanted = held`, with
        // `held` an `Int` from `Array::length`, was reported as a cycle in
        // `std/core.kh`: a scalar is copied, not pointed at, and copying one
        // into a field can no more make a loop than adding two numbers can.
        //
        // Two false positives out of twenty-one files is exactly the failure a
        // lint cannot have — the module documentation above says a warning
        // people learn to ignore is worse than no warning, and this is how
        // that starts.
        if !khora_perceus::is_boxed(self.types.of(value)) {
            return;
        }

        let mut stored = BTreeSet::new();
        locals_in(self.body, value, &mut stored);

        let closes =
            stored.iter().any(|from| *from == into || can_reach(&self.reaches, *from, into));
        if closes {
            self.out.push(Finding {
                lint: REFERENCE_CYCLE,
                message: format!(
                    "this stores something that already reaches `{}`, which makes a loop in \
                     the heap. Reference counting cannot free a loop, so the memory is never \
                     returned — and nothing else will say so. Break the link by storing an \
                     identifier instead of the object, or by keeping the back-reference \
                     outside the structure; `khora_live_count` shows the leak if you want to \
                     see it",
                    field_of(self.body, base, &name)
                ),
                range: self.body.range(target),
            });
        }

        // Recorded whether or not it was reported: the edge exists either way,
        // and a later assignment may be the one that closes the loop.
        self.reaches.entry(into).or_default().extend(stored);
    }
}

/// How to name the thing being assigned into, for the message.
fn field_of(body: &Body, base: khora_hir::body::ExprId, field: &str) -> String {
    match body.expr(base) {
        Expr::Local(local) => format!("{}.{field}", body.local(*local).name),
        _ => field.to_string(),
    }
}

/// The local a place expression is rooted at: `a.b.c` is rooted at `a`.
fn root_local(body: &Body, id: khora_hir::body::ExprId) -> Option<LocalId> {
    match body.expr(id) {
        Expr::Local(local) => Some(*local),
        Expr::Field { base, .. } => root_local(body, *base),
        _ => None,
    }
}

/// Every local an expression mentions.
///
/// Deliberately shallow about *how* they are mentioned: a local inside a record
/// literal, a constructor call or a tuple is reachable from the result, and one
/// inside an arbitrary call might be. Treating them alike is what keeps this
/// from needing an escape analysis, and the cost is that it can only be a
/// warning.
fn locals_in(body: &Body, id: khora_hir::body::ExprId, out: &mut BTreeSet<LocalId>) {
    match body.expr(id) {
        Expr::Local(local) => {
            out.insert(*local);
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                locals_in(body, *value, out);
            }
        }
        Expr::Tuple(items) => {
            for item in items {
                locals_in(body, *item, out);
            }
        }
        // **Only a constructor.** `List::Cons(head, tail)` produces a thing
        // that holds its arguments; `advance(pending, n)` produces a thing
        // computed *from* one, which is not the same and is the overwhelming
        // majority of calls.
        //
        // Treating every call as the first was wrong, and wrong in the
        // direction that matters: the first real Khora written after this
        // landed — `packages/postgres`, `c.pending = advance(c.pending, n)`,
        // a function that builds a new array and returns it — was reported
        // twice as a cycle. A lint people learn to ignore is worse than no
        // lint, and this file's own header says where a judgement is available
        // to take the quiet side. It had not.
        Expr::Call { callee, args } if constructs(body, *callee) => {
            for arg in args {
                locals_in(body, *arg, out);
            }
        }
        // Everything else contributes nothing. A field read is the interesting
        // omission: `b.next` is a thing `b` points *at*, so the value does not
        // contain `b` and saying it does was the same mistake pointing the
        // other way. Following it properly needs per-field reachability, which
        // is more than a warning is worth.
        _ => {}
    }
}

/// Whether a callee builds a value out of its arguments.
///
/// A variant constructor does — `Option::Some(x)` holds `x`. A function does
/// not, whatever it happens to do inside.
fn constructs(body: &Body, callee: khora_hir::body::ExprId) -> bool {
    matches!(
        body.expr(callee),
        Expr::Path(khora_hir::Resolution::Variant { .. })
    )
}

/// Whether `from` can already reach `to`, following the edges recorded so far.
fn can_reach(
    reaches: &BTreeMap<LocalId, BTreeSet<LocalId>>,
    from: LocalId,
    to: LocalId,
) -> bool {
    let mut seen: BTreeSet<LocalId> = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(here) = stack.pop() {
        if here == to {
            return true;
        }
        if !seen.insert(here) {
            continue;
        }
        if let Some(next) = reaches.get(&here) {
            stack.extend(next.iter().copied());
        }
    }
    false
}
