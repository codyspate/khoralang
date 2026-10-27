//! Which lower-case names are capabilities, and how a function gets one.
//!
//! **What this prevents: a missing `with` reported as a typo.** A function
//! that calls `clock.monotonic_millis()` without `with { clock: Clock }` was
//! told ``cannot find `clock` in this scope; did you mean `Clock`?``, which
//! sends the reader to the type and away from the clause. `clock` is not
//! misspelled. It is the label every `std` function that reads the time uses,
//! so it is a capability, and a capability arrives through a `with` clause or
//! a parameter.
//!
//! "Known" means written as a field of a `with` clause or a `row` somewhere in
//! the program, `std` included, with an `effect` for its type. `std`'s own
//! signatures are where the conventional labels live (`clock`, `reads`,
//! `writes`, `env`), so they need no list of their own here, and a label a
//! program chose for itself counts the same way.
//!
//! **What it costs:** reading every file's syntax and scope, but only for a
//! file that has an unresolved lower-case name. A file without one never
//! asks, so it takes no dependency on the rest of the program.

use khora_db::{Db, SourceFile};
use khora_syntax::ast::{self, AstNode};
use khora_syntax::SyntaxKind;

use crate::{file_scope, item_map, ItemKind, ModulePath};

/// A label some `with` clause or `row` gives a capability.
struct Known {
    /// The effect, as the declaring file names it.
    effect: String,
    /// The module that declares the effect, for an import line.
    home: Option<ModulePath>,
}

/// Where the name went unresolved, which decides which spelling compiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// An ordinary function: a `with` clause or a parameter brings it in.
    Function,
    /// `main`, a `test` or a `bench`: nothing calls one to supply a
    /// capability, so the only spelling that compiles is installing it.
    EntryPoint,
}

/// What to say about `label`, a lower-case name `file` could not resolve,
/// when the program knows it as a capability; `None` when it does not.
///
/// Not asked for a name with a close local in reach (`log` beside `logs`):
/// lowering keeps "did you mean" for that, because a typo of something the
/// body holds is the likelier mistake.
///
/// The message shows the spellings that compile at `site`, and the import
/// line too when the effect is not in scope in `file`.
pub fn missing_capability(
    db: &dyn Db,
    file: SourceFile,
    label: &str,
    site: Site,
) -> Option<String> {
    let known = known_label(db, file, label)?;
    let effect = known.effect;
    let in_scope = item_map(db, file).item(&effect).is_some()
        || file_scope(db, file).names.iter().any(|(name, _)| *name == effect);
    let import = match (&known.home, in_scope) {
        (Some(home), false) => {
            format!("`import {}::{{{effect}}};` at the top of the file, then ", home.segments().join("::"))
        }
        _ => String::new(),
    };
    Some(match site {
        Site::Function => format!(
            "`{label}` is a capability, and nothing here provides it. A function gets one from \
             its `with` clause or as a parameter: {import}`with {{ {label}: {effect} }}` after \
             the return type, or `{label}: {effect}` among the parameters"
        ),
        // **Not the clause or the parameter, which an entry point cannot
        // have**: nothing calls `main` to supply either, and following that
        // advice met a second refusal. An entry point installs the handler.
        Site::EntryPoint => {
            let handler = if has_real(db, file, &known.home, &effect) {
                format!("{effect}::real()")
            } else {
                format!("a handler for `{effect}`")
            };
            let wrap = if handler.ends_with("()") {
                format!("`with {{ {label}: {handler} }} {{ .. }}`")
            } else {
                format!("`with {{ {label}: .. }} {{ .. }}`, with {handler}")
            };
            format!(
                "`{label}` is a capability, and nothing here provides it. An entry point \
                 installs its capabilities rather than asking for them: {import}wrap the body \
                 in {wrap}"
            )
        }
    })
}

/// Whether the effect has a `real()` handler: `pub fn real() -> Effect` in
/// an `impl Effect` of the module that declares it.
fn has_real(db: &dyn Db, file: SourceFile, home: &Option<ModulePath>, effect: &str) -> bool {
    let source = match home {
        Some(home) => {
            let Some(root) = khora_db::source_root(db) else { return false };
            match crate::module_graph(db, root).file(home) {
                Some(source) => source,
                None => return false,
            }
        }
        None => file,
    };
    khora_db::parse(db, source).source_file().decls().any(|decl| {
        let ast::Decl::Impl(imp) = decl else { return false };
        if !imp.is_inherent()
            || imp.self_type().and_then(|t| crate::body::type_head(&t)).as_deref() != Some(effect)
        {
            return false;
        }
        imp.functions().any(|f| {
            f.name().and_then(|n| n.ident()).as_deref() == Some("real")
                && f.params().is_none_or(|p| p.params().next().is_none())
                && f.return_type().and_then(|t| crate::body::type_head(&t)).as_deref()
                    == Some(effect)
        })
    })
}

/// The first `with` or `row` field named `label` that holds an effect,
/// looking in `file` first and then in every other file by path, so the same
/// program always gets the same answer.
fn known_label(db: &dyn Db, file: SourceFile, label: &str) -> Option<Known> {
    if let Some(found) = labels_in(db, file, label) {
        return Some(found);
    }
    let root = khora_db::source_root(db)?;
    let mut others: Vec<SourceFile> = root.files(db).iter().copied().filter(|f| *f != file).collect();
    others.sort_by_key(|f| f.path(db).clone());
    others.into_iter().find_map(|other| labels_in(db, other, label))
}

/// A field named `label` in one of `file`'s `with` clauses or `row`
/// declarations, whose type is an effect.
fn labels_in(db: &dyn Db, file: SourceFile, label: &str) -> Option<Known> {
    let parsed = khora_db::parse(db, file);
    let root = parsed.source_file();
    root.syntax()
        .descendants()
        .filter(|n| matches!(n.kind(), SyntaxKind::WITH_CLAUSE | SyntaxKind::ROW_DECL))
        .flat_map(|n| n.descendants().filter_map(ast::Field::cast).collect::<Vec<_>>())
        .filter(|field| field.name().and_then(|n| n.ident()).as_deref() == Some(label))
        .find_map(|field| {
            let ast::Type::Path(written) = field.ty()? else { return None };
            if written.row_var().is_some() {
                return None;
            }
            let effect = written.path()?.text_path();
            let home = effect_home(db, file, &effect)?;
            Some(Known { effect, home })
        })
}

/// Where `file` gets the effect it calls `name`, if `name` is an effect
/// there: `Some(home)` for a declaration or an import, with `home` absent only
/// for a file that declares no module.
fn effect_home(db: &dyn Db, file: SourceFile, name: &str) -> Option<Option<ModulePath>> {
    let items = item_map(db, file);
    if let Some(item) = items.item(name) {
        return (item.kind == ItemKind::Effect).then(|| items.module.clone());
    }
    file_scope(db, file)
        .origins
        .iter()
        .find(|o| o.local == name && o.kind == ItemKind::Effect)
        .map(|o| Some(o.module.clone()))
}
