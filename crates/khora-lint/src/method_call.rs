//! `method-call`: `x.m(a)`, where `T::m(x, a)` says it.
//!
//! Khora has two spellings of one call, and the owner chose the qualified one:
//! it names where the function lives, it is the form `|>` composes with, and
//! it is the one that always works (a trait-qualified call is how an ambiguity
//! is settled). The dotted form stays accepted; this lint reports it, in the
//! `idiomatic` group.
//!
//! # Why the fix is the same call
//!
//! The checker resolves `x.m(..)` to a signature key, and the fix is read off
//! that key rather than guessed from the receiver's type:
//!
//! - `#Head::m`, a method a type declares for itself, becomes `Head::m(x, ..)`.
//!   `Head::m` is looked up by the same head name through the same
//!   `inherent_method`, so it finds the same method under the same key.
//! - `Trait::m`, a trait's method (reached through an impl or through a bound
//!   on a type parameter), becomes `Trait::m(x, ..)`. `Trait::m` resolves to
//!   that trait's declaration when no type is named `Trait` with an `m` of its
//!   own; that is checked, and the fix withheld if one is.
//!
//! In both, the receiver becomes argument 1 and the written arguments follow
//! in order, labels and all. A call evaluates its arguments left to right and
//! a method call evaluates its receiver first, so side effects keep their
//! order. A label names a parameter by position after the receiver; in the
//! qualified call the receiver is written, so each label lands on the same
//! parameter. A call through `|>` keeps the piped value in its slot: an
//! explicit `_` is kept, and a stage with none gets one written where the
//! value went, after the receiver.
//!
//! # Where the fix is withheld, and the finding kept
//!
//! - the owner's name is taken in this file by a different declaration --
//!   another type of that name, declared here or imported, or a function --
//!   so neither `T::m` nor an import of `T` would reach it;
//! - the owner is not in scope and the module that declares it does not
//!   export it, or more than one module exports a trait of that name;
//! - the owner's name would resolve to something else as a path: a type
//!   parameter of that name, a constructor of that name, or (for a trait) a
//!   type named like the trait that has an `m` of its own;
//! - the receiver's type was not settled when the call was checked;
//! - a comment sits between the receiver and the argument list, which the
//!   edit would drop.
//!
//! Not reported: a call through a field holding a function (`r.f(x)` where
//! `f` is a field), which is not a method call at all; and a call inside a
//! `${..}` hole, which is part of one string token and not a node the lints
//! walk.
//!
//! What it costs: the fix may add an `import`, so it can carry two edits, and
//! the qualified form is longer for a chain -- `a.f().g()` becomes
//! `B::g(A::f(a))`, one pass per link, since each link's edit contains the
//! next.

use khora_db::{Db, SourceFile};
use khora_hir::body::{Body, Expr, ExprId};
use khora_hir::{ImportKind, ItemKind, ModulePath};
use khora_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use khora_types::{BodyTypes, Type, TypeMap};
use text_size::{TextRange, TextSize};

use crate::idiomatic::{Edit, Fix};
use crate::Finding;

/// `x.m(a)`, where `T::m(x, a)` says it.
pub const METHOD_CALL: &str = "method-call";

/// Every method call in `body`.
pub(crate) fn method_calls(
    db: &dyn Db,
    file: SourceFile,
    body: &Body,
    types: &BodyTypes,
    out: &mut Vec<Finding>,
) {
    let parse = khora_db::parse(db, file);
    // An edit computed across a hole in the tree is an edit to text nobody
    // can see the shape of.
    if !parse.errors().is_empty() {
        return;
    }
    let tree = parse.syntax();
    let text = file.text(db);
    let map = khora_types::type_map(db, file);
    for (id, expr) in body.exprs() {
        let Expr::Call { callee, args } = expr else { continue };
        let Expr::Field { base, name } = body.expr(*callee) else { continue };
        // Only a call the checker resolved to a method has a key. A field
        // holding a function is called through `apply` and records none.
        let Some((key, _)) = types.instantiation(*callee) else { continue };
        let Some(call) = call_node(&tree, body.range(id), name) else { continue };
        let Some(owner) = owner_of(key, name) else { continue };
        let fix = fix_for(db, file, &tree, text, map, &call, &owner, name, *base, args.len(), types);
        out.push(Finding {
            lint: METHOD_CALL,
            message: format!(
                "a method is called through its owner in Khora: write `{}::{name}(receiver, ..)`",
                owner.name
            ),
            range: call.text_range(),
            fix,
        });
    }
}

/// Whose `m` the checker chose: a type's own, or a trait's.
struct Owner {
    name: String,
    is_trait: bool,
}

/// The owner a signature key names. `None` for a key of another shape, which
/// no method call records today; a guess would be a fix to the wrong owner.
fn owner_of(key: &str, method: &str) -> Option<Owner> {
    let suffix = format!("::{method}");
    match key.split_once('#') {
        Some(("", rest)) => {
            let head = rest.strip_suffix(&suffix)?;
            Some(Owner { name: head.to_string(), is_trait: false })
        }
        Some(_) => None,
        None => {
            let name = key.strip_suffix(&suffix)?;
            Some(Owner { name: name.to_string(), is_trait: true })
        }
    }
}

/// The written call a lowered one came from: a `CALL_EXPR` whose callee is
/// `.name`, or, for `x |> r.name` with no brackets, the `FIELD_EXPR` itself.
///
/// A call through `|>` is lowered with the whole pipeline's range, and one
/// marked `!` with the mark's, so both are looked through to the stage.
/// `None` for a call the lowering made up (a `for` loop's `next`) or one
/// inside a string's hole, which has no node of its own.
fn call_node(tree: &SyntaxNode, range: TextRange, name: &str) -> Option<SyntaxNode> {
    let start = match tree.covering_element(range) {
        SyntaxElement::Node(node) => node,
        SyntaxElement::Token(token) => token.parent()?,
    };
    let mut node = start
        .ancestors()
        .take_while(|n| n.text_range() == range)
        .find(|n| matches!(n.kind(), SyntaxKind::CALL_EXPR | SyntaxKind::PIPE_EXPR | SyntaxKind::TRY_EXPR))?;
    loop {
        node = match node.kind() {
            SyntaxKind::PIPE_EXPR => node.children().last()?,
            SyntaxKind::TRY_EXPR => node.children().next()?,
            _ => break,
        };
    }
    let field = match node.kind() {
        SyntaxKind::CALL_EXPR => node.children().next().filter(|n| n.kind() == SyntaxKind::FIELD_EXPR)?,
        SyntaxKind::FIELD_EXPR => node.clone(),
        _ => return None,
    };
    let written = field.children().filter(|n| n.kind() == SyntaxKind::NAME_REF).last()?;
    (written.text().to_string().trim() == name).then_some(node)
}

#[allow(clippy::too_many_arguments)]
fn fix_for(
    db: &dyn Db,
    file: SourceFile,
    tree: &SyntaxNode,
    text: &str,
    map: &TypeMap,
    call: &SyntaxNode,
    owner: &Owner,
    method: &str,
    base: ExprId,
    lowered_args: usize,
    types: &BodyTypes,
) -> Option<Fix> {
    if !settled(types.of(base)) {
        return None;
    }
    // `x |> r.m` is a stage with no argument list: the piped value is the
    // one argument after the receiver.
    let (field, args) = match call.kind() {
        SyntaxKind::FIELD_EXPR => (call.clone(), None),
        _ => (
            call.children().find(|n| n.kind() == SyntaxKind::FIELD_EXPR)?,
            Some(call.children().find(|n| n.kind() == SyntaxKind::ARG_LIST)?),
        ),
    };
    let receiver = field.children().next()?;
    // A comment between the receiver and the `(` has nowhere to go.
    let name_end = field.text_range().end();
    let between = TextRange::new(receiver.text_range().end(), args.as_ref().map_or(name_end, |a| a.text_range().start()));
    let comment_between = call
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .any(|t| matches!(t.kind(), SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT) && between.contains_range(t.text_range()));
    if comment_between {
        return None;
    }
    if !resolves_to_the_same(map, call, owner, method) {
        return None;
    }
    let import = match in_scope(db, file, owner, types.of(base)) {
        Scope::Same => None,
        Scope::Clash => return None,
        Scope::Absent => Some(import_edit(tree, text, &home_to_import(db, file, owner, types.of(base))?, &owner.name)?),
    };

    let receiver_text = receiver_text(&receiver, text);
    let Some(args) = args else {
        if lowered_args != 1 {
            return None;
        }
        let replacement = format!("{}::{method}({receiver_text}, _)", owner.name);
        let mut edits = vec![Edit { range: call.text_range(), replacement }];
        edits.extend(import);
        return Some(Fix { edits });
    };
    let written: Vec<SyntaxNode> = args.children().collect();
    let has_placeholder = written.iter().any(|n| n.kind() == SyntaxKind::PLACEHOLDER_EXPR);
    // A pipeline stage with no `_` puts the piped value first after the
    // receiver; the qualified call has to say where it goes.
    let piped_without_placeholder = !has_placeholder && lowered_args == written.len() + 1;
    if !has_placeholder && !piped_without_placeholder && lowered_args != written.len() {
        return None;
    }

    let inside = TextRange::new(
        args.first_token()?.text_range().end(),
        args.last_token().filter(|t| t.kind() == SyntaxKind::R_PAREN)?.text_range().start(),
    );
    let inner = &text[std::ops::Range::<usize>::from(inside)];
    let mut first = receiver_text;
    if piped_without_placeholder {
        first.push_str(", _");
    }
    let replacement = if inner.trim().is_empty() {
        format!("{}::{method}({first})", owner.name)
    } else if let Some(broken) = inner.strip_prefix('\n') {
        // Laid out one argument per line: the receiver takes a line of its
        // own at the arguments' indentation.
        let indent: String = broken.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        format!("{}::{method}(\n{indent}{first},\n{broken})", owner.name)
    } else {
        format!("{}::{method}({first}, {})", owner.name, inner.trim_start())
    };
    let mut edits = vec![Edit { range: call.text_range(), replacement }];
    edits.extend(import);
    Some(Fix { edits })
}

/// Whether a type is known all the way down. A receiver whose type still has
/// a hole in it could be pinned by the dotted form in a way the fix would not
/// repeat; nothing is fixed on a guess.
fn settled(ty: &Type) -> bool {
    match ty {
        Type::Var(_) | Type::Unknown => false,
        Type::Int
        | Type::Fixed(_)
        | Type::Float
        | Type::Bool
        | Type::Str
        | Type::Unit
        | Type::Ptr
        | Type::Char
        | Type::Param(_)
        | Type::Const(_)
        | Type::Never => true,
        Type::Adt { args, .. } => args.iter().all(settled),
        Type::Applied { head, args } => settled(head) && args.iter().all(settled),
        Type::Tuple(items) => items.iter().all(settled),
        Type::Assoc { owner, .. } => settled(owner),
        Type::Fn { params, ret, .. } => params.iter().all(settled) && settled(ret),
        Type::Row { fields, .. } => fields.iter().all(|(_, t)| settled(t)),
    }
}

/// The receiver as the first argument: one layer of brackets dropped, since
/// an argument needs none, unless what they hold is a lambda, whose body
/// would otherwise run on into the next argument.
fn receiver_text(receiver: &SyntaxNode, text: &str) -> String {
    let written = text[std::ops::Range::<usize>::from(receiver.text_range())].to_string();
    if receiver.kind() != SyntaxKind::PAREN_EXPR {
        return written;
    }
    let inner: Vec<SyntaxNode> = receiver.children().collect();
    let has_comment = receiver
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .any(|t| matches!(t.kind(), SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT));
    match inner.as_slice() {
        [only] if only.kind() != SyntaxKind::LAMBDA_EXPR && !has_comment => {
            text[std::ops::Range::<usize>::from(only.text_range())].to_string()
        }
        _ => written,
    }
}

/// Whether `Owner::method` written in this file reaches the method the
/// checker chose for the dotted call.
///
/// The path is lowered as a trait item when the owner is a type parameter,
/// and as a constructor when the owner has a case named `method`: either
/// makes it a different thing. For a trait's method, the checker then looks
/// for a *type* named like the owner before it looks for the trait
/// (`type_of_trait_item`), so a type named `Show` with a `show` of its own
/// would take the call.
fn resolves_to_the_same(map: &TypeMap, call: &SyntaxNode, owner: &Owner, method: &str) -> bool {
    if names_a_type_parameter(call, &owner.name) {
        return false;
    }
    if map.variants.iter().any(|v| v.type_name == owner.name && v.name == method) {
        return false;
    }
    if owner.is_trait {
        let as_type = Type::adt(&owner.name);
        if map.traits.inherent_method(&as_type, method).is_some()
            || map.traits.inherent_hidden(&as_type, method).is_some()
        {
            return false;
        }
        let has_impl = map.traits.impls.iter().any(|i| {
            khora_types::traits::head_of(&i.self_type).as_deref() == Some(owner.name.as_str())
                && i.methods.iter().any(|m| m == method)
        });
        if has_impl {
            return false;
        }
    }
    true
}

/// Whether a function or impl around `at` declares a type parameter `name`.
fn names_a_type_parameter(at: &SyntaxNode, name: &str) -> bool {
    at.ancestors()
        .filter_map(|scope| scope.children().find(|n| n.kind() == SyntaxKind::TYPE_PARAMS))
        .any(|params| {
            params
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::NAME)
                .any(|n| n.text().to_string().trim() == name)
        })
}

enum Scope {
    /// `Name::f` here already reaches the owner the checker chose.
    Same,
    /// Nothing here goes by the name, so an import can bring the owner in.
    Absent,
    /// Something else here goes by the name: another type, a function, or an
    /// import of a different module's type of that name.
    Clash,
}

fn type_like(kind: ItemKind) -> bool {
    match kind {
        ItemKind::Type | ItemKind::Trait | ItemKind::Effect => true,
        ItemKind::Function | ItemKind::Const | ItemKind::Context | ItemKind::Row => false,
    }
}

/// How the owner's name stands in `file`, as `Name::f` would see it.
///
/// **A name in scope is not enough: it has to be the same declaration.** Two
/// modules may each declare an `Entry`, and a file holding one and importing
/// the other would have `Entry::m(x)` reach the imported one. So a type's
/// own method needs the name in scope to be declared where the receiver's
/// type is, and a trait's method needs the name to be a trait.
fn in_scope(db: &dyn Db, file: SourceFile, owner: &Owner, receiver: &Type) -> Scope {
    let items = khora_hir::item_map(db, file);
    let fits = |kind: ItemKind, module: Option<&ModulePath>| {
        if owner.is_trait {
            kind == ItemKind::Trait
        } else {
            matches!(kind, ItemKind::Type | ItemKind::Effect)
                && khora_types::traits::head_of(receiver).as_deref() == Some(owner.name.as_str())
                && khora_types::traits::home_of(receiver).as_ref() == module
        }
    };
    if let Some(item) = items.item(&owner.name) {
        return if fits(item.kind, items.module.as_ref()) { Scope::Same } else { Scope::Clash };
    }
    let imported = items.imports.iter().find_map(|import| match &import.kind {
        ImportKind::Named(names) => names.iter().find(|n| n.alias == owner.name).map(|n| (import, n.name.clone())),
        ImportKind::Glob => None,
    });
    let builtin = || {
        !owner.is_trait
            && khora_types::traits::home_of(receiver).is_none()
            && khora_types::traits::head_of(receiver).as_deref() == Some(owner.name.as_str())
    };
    if let Some((import, original)) = imported {
        let kind = module_file(db, &import.path)
            .and_then(|target| khora_hir::module_api(db, target).item(&original).map(|i| i.kind));
        return match kind {
            Some(kind) if original == owner.name && type_like(kind) && fits(kind, Some(&import.path)) => Scope::Same,
            // **An import of a built-in is a no-op**: the resolver accepts
            // `import std::core::{String}` and binds nothing, since no module
            // declares `String`. The name still means the built-in, so a
            // `String` receiver's method is `String::m` here as anywhere. An
            // alias onto a built-in's name (`{Int as String}`) is accepted
            // too and binds nothing either; it is left unfixed anyway, since
            // whoever wrote it meant something by it. What it costs: that
            // one shape keeps its finding.
            None if original == owner.name && khora_hir::is_builtin_type(&original) && builtin() => Scope::Same,
            _ => Scope::Clash,
        };
    }
    if khora_hir::BUILTIN_TYPES.contains(&owner.name.as_str()) {
        return if builtin() { Scope::Same } else { Scope::Clash };
    }
    Scope::Absent
}

/// The file declaring `module`, if the compilation has one.
fn module_file(db: &dyn Db, module: &ModulePath) -> Option<SourceFile> {
    let root = khora_db::source_root(db)?;
    root.files(db).iter().copied().find(|f| khora_hir::module_api(db, *f).module.as_ref() == Some(module))
}

/// The module to import the owner from: the receiver type's home for a
/// type's own method, and the one module exporting a trait of that name for
/// a trait's. `None` when that module does not export it, or when two do.
fn home_to_import(db: &dyn Db, file: SourceFile, owner: &Owner, receiver: &Type) -> Option<String> {
    let exported = |module: &ModulePath, kinds: &[ItemKind]| {
        module_file(db, module).is_some_and(|f| {
            khora_hir::module_api(db, f)
                .item(&owner.name)
                .is_some_and(|i| i.is_public && kinds.contains(&i.kind))
        })
    };
    let here = khora_hir::item_map(db, file).module.clone();
    let home = if owner.is_trait {
        let root = khora_db::source_root(db)?;
        let mut found: Vec<ModulePath> = root
            .files(db)
            .iter()
            .filter_map(|f| khora_hir::module_api(db, *f).module.clone())
            .filter(|m| exported(m, &[ItemKind::Trait]))
            .collect();
        found.sort_by_key(|m| m.segments().to_vec());
        found.dedup();
        match found.as_slice() {
            [only] => only.clone(),
            _ => return None,
        }
    } else {
        let home = khora_types::traits::home_of(receiver)?;
        if khora_types::traits::head_of(receiver).as_deref() != Some(owner.name.as_str())
            || !exported(&home, &[ItemKind::Type, ItemKind::Effect])
        {
            return None;
        }
        home
    };
    if Some(&home) == here.as_ref() {
        return None;
    }
    Some(home.segments().join("::"))
}

/// The edit bringing `name` in from `module`: an entry in the braces of an
/// import of that module, or a line of its own among the imports, sorted,
/// where `khora fmt` would put it.
fn import_edit(tree: &SyntaxNode, text: &str, module: &str, name: &str) -> Option<Edit> {
    let imports: Vec<SyntaxNode> = tree.children().filter(|n| n.kind() == SyntaxKind::IMPORT_DECL).collect();
    let path_of = |decl: &SyntaxNode| {
        decl.children()
            .find(|n| n.kind() == SyntaxKind::PATH)
            .map(|p| p.text().to_string().split_whitespace().collect::<String>())
            .unwrap_or_default()
    };
    for decl in &imports {
        if path_of(decl) != module {
            continue;
        }
        let list = decl.children().find(|n| n.kind() == SyntaxKind::IMPORT_LIST)?;
        let mut names: Vec<String> = list
            .children()
            .filter(|n| n.kind() == SyntaxKind::IMPORT_ITEM)
            .map(|n| n.text().to_string().split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        if names.iter().any(|seen| seen == name) {
            return None;
        }
        names.push(name.to_string());
        names.sort();
        return Some(Edit { range: list.text_range(), replacement: format!("{{{}}}", names.join(", ")) });
    }
    let mut line = format!("import {module}::{{{name}}};\n");
    let line_start = |at: TextSize| TextSize::from(text[..usize::from(at)].rfind('\n').map_or(0, |n| n + 1) as u32);
    let after_line = |at: TextSize| {
        let rest = &text[usize::from(at)..];
        at + TextSize::from(rest.find('\n').map_or(rest.len(), |n| n + 1) as u32)
    };
    let at = match imports.iter().find(|decl| path_of(decl).as_str() > module) {
        Some(later) => line_start(later.text_range().start()),
        None => match imports.last() {
            Some(last) => after_line(last.text_range().end()),
            None => {
                line.push('\n');
                let decl = tree.children().find(|n| n.kind() == SyntaxKind::MODULE_DECL)?;
                let mut at = after_line(decl.text_range().end());
                if text[usize::from(at)..].starts_with('\n') {
                    at += TextSize::from(1u32);
                }
                at
            }
        },
    };
    Some(Edit { range: TextRange::empty(at), replacement: line })
}
