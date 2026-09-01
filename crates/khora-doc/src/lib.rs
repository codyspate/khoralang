//! API documentation, read out of the syntax tree.
//!
//! Two halves, deliberately separate: [`module_of`] turns one parsed file into
//! a [`Module`], and [`markdown`] turns a `Module` into a page. Nothing here
//! reads or writes a file, which is what lets every test below be a string in
//! and a string out.
//!
//! # Why the syntax tree and not the HIR
//!
//! Because the HIR does not have the API in it. `khora_hir::collect_decl`
//! returns early on `Decl::Impl` — an impl has no name of its own, and
//! recording one would give two impls for the same type a spurious duplicate —
//! so **every method in `std` is absent from `item_map`**. `std/core.kh`
//! declares nine top-level functions and two hundred and thirty-seven methods;
//! a reference built on the HIR would document the nine.
//!
//! The syntax tree has the other thing the HIR throws away, which is the
//! comments. Rowan keeps every byte, so a `///` block is still sitting in the
//! tree as trivia in front of the declaration it belongs to.
//!
//! # What is documented
//!
//! Exported items, and the members of exported types and traits.
//!
//! **`pub` on a method is read.** A method without it may only be called by
//! the module that declares it, so it is not part of anybody's API and does not
//! belong in a reference.
//!
//! **A trait impl's methods are not filtered.** `impl Show for Decimal` is
//! reachable through `Show` wherever `Decimal` is, and what makes those public
//! is the trait rather than the keyword.

#![deny(missing_docs)]

mod markdown;
mod signature;

pub use markdown::markdown;

use khora_syntax::ast::{self, AstNode};
use khora_syntax::{SyntaxKind, SyntaxNode};

/// One module's public API.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    /// `std::decimal`, or `None` in a file with no module declaration.
    pub path: Option<String>,
    /// The `//!` block, one entry per line, comment markers removed.
    pub doc: Vec<String>,
    /// Everything the module documents, in the order it was declared.
    pub items: Vec<Item>,
}

/// A documented declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What it is called. For an impl, the type or `Trait for Type`.
    pub name: String,
    /// Which kind of declaration it is, which decides how it is headed.
    pub kind: Kind,
    /// The declaration as it should be read: a signature for a function, the
    /// whole declaration for a type. See `signature`.
    pub signature: String,
    /// The `///` block, one entry per line, comment markers removed.
    pub doc: Vec<String>,
    /// A trait's or an impl's functions, a variant type's cases, a record's
    /// fields. Empty for everything else.
    pub members: Vec<Item>,
}

/// What kind of declaration an [`Item`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `type Decimal = ..`, record or variant.
    Type,
    /// `trait Show { .. }`.
    Trait,
    /// `effect Ledger { .. }`.
    Effect,
    /// `context Testing { .. }`.
    Context,
    /// A free function.
    Function,
    /// `const MAX: Int = ..`.
    Const,
    /// `impl Writer { .. }` -- the methods of a type.
    Methods,
    /// `impl Show for Decimal { .. }`.
    TraitImpl,
    /// One case of a variant type.
    Variant,
    /// One field of a record type.
    Field,
    /// One operation of an effect.
    ///
    /// **Its own kind rather than a `Field`**, because that is what the
    /// declaration calls it and what a reader is looking for: `nursery.adopt`
    /// is something the capability *does*, and heading the list "Fields" would
    /// describe the record it happens to be built from instead.
    Operation,
    /// `type Item;` in a trait, or `type Item = Int;` in an impl.
    AssocType,
}

impl Kind {
    /// The word a reader sees, singular.
    pub fn describe(self) -> &'static str {
        match self {
            Kind::Type => "type",
            Kind::Trait => "trait",
            Kind::Effect => "effect",
            Kind::Context => "context",
            Kind::Function => "function",
            Kind::Const => "constant",
            Kind::Methods => "methods",
            Kind::TraitImpl => "trait implementation",
            Kind::Variant => "variant",
            Kind::Field => "field",
            Kind::Operation => "operation",
            Kind::AssocType => "associated type",
        }
    }

    /// The heading a group of these sits under, plural.
    pub fn heading(self) -> &'static str {
        match self {
            Kind::Type => "Types",
            Kind::Trait => "Traits",
            Kind::Effect => "Effects",
            Kind::Context => "Contexts",
            Kind::Function => "Functions",
            Kind::Const => "Constants",
            Kind::Methods => "Methods",
            Kind::TraitImpl => "Trait implementations",
            Kind::Variant => "Variants",
            Kind::Field => "Fields",
            Kind::Operation => "Operations",
            Kind::AssocType => "Associated types",
        }
    }
}

/// The order sections appear on a page.
///
/// Types first because a reader looking up a function needs its argument types
/// to mean something, and constants last because they are the smallest thing
/// on the page.
pub const SECTIONS: &[Kind] = &[
    Kind::Type,
    Kind::Trait,
    Kind::Effect,
    Kind::Context,
    Kind::Methods,
    Kind::TraitImpl,
    Kind::Function,
    Kind::Const,
];

/// Combines modules that share a path into one.
///
/// **One module, several files.** `std::net::socket` is written three times —
/// `socket_linux.kh`, `socket_macos.kh`, `socket_windows.kh` — and exactly one
/// is compiled. They are the same module offering the same API, so documenting
/// whichever the directory walk reached last would make the reference change
/// when somebody renamed a file.
///
/// Items are keyed by name and the earliest wins. Callers pass the modules in
/// a stable order -- `doc` sorts by path -- so "earliest" is a fact about the
/// source tree and not about the filesystem. A member that only one platform
/// has still appears, which is the right answer: it is part of the API on that
/// platform and silence would be worse than a line saying so.
///
/// **Every distinct `//!` block is kept, not just the first.** Each variant's
/// block describes *that* variant -- "Sockets, on Linux", "Sockets, on
/// Windows" -- so taking one and dropping the rest would publish one platform's
/// notes as though they were the module's, and which one would depend on
/// alphabetical order. Identical blocks are not repeated, so a module whose
/// variants say the same thing reads as though it were one file.
///
/// The result is sorted by module path.
pub fn merge(modules: Vec<Module>) -> Vec<Module> {
    let mut order: Vec<String> = Vec::new();
    let mut by_path: std::collections::HashMap<String, Module> = std::collections::HashMap::new();

    for module in modules {
        let key = module.path.clone().unwrap_or_default();
        match by_path.get_mut(&key) {
            None => {
                order.push(key.clone());
                by_path.insert(key, module);
            }
            Some(into) => {
                if into.doc.is_empty() {
                    into.doc = module.doc;
                } else if !module.doc.is_empty()
                    && !into.doc.windows(module.doc.len()).any(|w| w == module.doc)
                {
                    into.doc.push(String::new());
                    into.doc.extend(module.doc);
                }
                for item in module.items {
                    match into.items.iter_mut().find(|i| i.name == item.name && i.kind == item.kind)
                    {
                        None => into.items.push(item),
                        Some(seen) => {
                            if seen.doc.is_empty() {
                                seen.doc = item.doc;
                            }
                            for member in item.members {
                                if !seen.members.iter().any(|m| m.name == member.name) {
                                    seen.members.push(member);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut out: Vec<Module> = order.into_iter().filter_map(|k| by_path.remove(&k)).collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Reads one parsed file's public API.
pub fn module_of(file: &ast::SourceFile) -> Module {
    let path = file
        .module()
        .and_then(|m| m.path())
        .map(|p| p.syntax().text().to_string().split_whitespace().collect::<String>());

    Module { path, doc: module_doc(file.syntax()), items: items_of(file) }
}

fn items_of(file: &ast::SourceFile) -> Vec<Item> {
    let mut out = Vec::new();
    for decl in file.decls() {
        if let Some(item) = item_of(&decl) {
            out.push(item);
        }
    }
    out
}

fn item_of(decl: &ast::Decl) -> Option<Item> {
    match decl {
        ast::Decl::Type(t) if t.is_exported() => Some(Item {
            name: named(t.name())?,
            kind: Kind::Type,
            signature: signature::declaration(t.syntax()),
            doc: doc_of(t.syntax()),
            members: definition_members(t),
        }),
        ast::Decl::Trait(t) if t.is_exported() => Some(Item {
            name: named(t.name())?,
            kind: Kind::Trait,
            signature: signature::header(t.syntax()),
            doc: doc_of(t.syntax()),
            members: assoc_types(t.assoc_types()).chain(functions(t.functions())).collect(),
        }),
        ast::Decl::Effect(e) if e.is_exported() => Some(Item {
            name: named(e.name())?,
            kind: Kind::Effect,
            signature: signature::declaration(e.syntax()),
            doc: doc_of(e.syntax()),
            members: operations(e.operations()),
        }),
        ast::Decl::Context(c) if c.is_exported() => Some(Item {
            name: named(c.name())?,
            kind: Kind::Context,
            signature: signature::declaration(c.syntax()),
            doc: doc_of(c.syntax()),
            members: Vec::new(),
        }),
        ast::Decl::Fn(f) if f.is_exported() => Some(Item {
            name: named(f.name())?,
            kind: Kind::Function,
            signature: signature::function(f),
            doc: doc_of(f.syntax()),
            members: Vec::new(),
        }),
        ast::Decl::Const(c) if c.is_exported() => Some(Item {
            name: const_name(c)?,
            kind: Kind::Const,
            signature: signature::declaration(c.syntax()),
            doc: doc_of(c.syntax()),
            members: Vec::new(),
        }),
        ast::Decl::Impl(i) => impl_item(i),
        _ => None,
    }
}

/// An `impl` block, when its self type is one this module exports.
///
/// The name is the self type for an inherent impl and `Trait for Type` for a
/// trait one, because that is how a reader refers to them and neither has a
/// name of its own in the grammar.
fn impl_item(i: &ast::ImplDecl) -> Option<Item> {
    let self_type = i.self_type()?;
    let self_name = signature::type_text(&self_type);
    let base = base_name(&self_name);

    // An impl belongs to this module when this module declares one of the two
    // things it names. An impl of somebody else's trait for somebody else's
    // type is not this module's API — and coherence refuses it anyway, so the
    // only file where that combination appears is one that owns a side.
    //
    // **A primitive is the exception, because it belongs to nobody.** A
    // builtin has no `pub type` to be exported by, so requiring the *type* to
    // be declared dropped every `impl String`, `impl Int` and `impl Float` in
    // `std::core` — and `String`, the type with the most methods anybody will
    // look up, had no reference page at all. Its trait impls came back on
    // their own once the trait counted, since `Show` and `Ord` are declared in
    // the same file; the inherent ones needed [`is_primitive`].
    let (name, kind) = match i.trait_() {
        Some(t) => {
            let trait_name = signature::type_text(&t);
            if !exported_here(i.syntax(), base)
                && !exported_here(i.syntax(), base_name(&trait_name))
            {
                return None;
            }
            (format!("{trait_name} for {self_name}"), Kind::TraitImpl)
        }
        None => {
            if !exported_here(i.syntax(), base) && !is_primitive(base) {
                return None;
            }
            (self_name, Kind::Methods)
        }
    };

    // An inherent method is API only if it says so. A trait impl's methods are
    // the trait's, and are reachable wherever the trait is — see the note at
    // the head of this module.
    let inherent = kind == Kind::Methods;
    let members: Vec<Item> = assoc_types(i.assoc_types())
        .chain(functions(i.functions().filter(|f| !inherent || f.is_exported())))
        .collect();
    if members.is_empty() {
        return None;
    }

    Some(Item {
        name,
        kind,
        signature: signature::header(i.syntax()),
        doc: doc_of(i.syntax()),
        members,
    })
}

/// Whether the file containing `node` exports a type or trait called `name`.
fn exported_here(node: &SyntaxNode, name: &str) -> bool {
    let Some(file) = node.ancestors().last().and_then(ast::SourceFile::cast) else {
        return false;
    };
    file.decls().any(|d| match d {
        ast::Decl::Type(t) => t.is_exported() && named(t.name()).as_deref() == Some(name),
        ast::Decl::Trait(t) => t.is_exported() && named(t.name()).as_deref() == Some(name),
        // **Effects and contexts, without which no capability had a
        // constructor on its page.** `impl FsRead { pub fn real() -> FsRead }`
        // was dropped here: `FsRead` is a `pub effect`, so it was neither an
        // exported type nor an exported trait, and the whole `impl` went with
        // it. The same for `Clock::real`, `Env::real`, `Random::seeded` and
        // `HttpClient::real`.
        //
        // Which is every capability constructor in `std` -- the one function a
        // reader needs before they can write a program that does anything, and
        // the reference had none of them. They were findable in cookbook code
        // blocks, by somebody who already knew the name to search for.
        ast::Decl::Effect(e) => e.is_exported() && named(e.name()).as_deref() == Some(name),
        ast::Decl::Context(c) => c.is_exported() && named(c.name()).as_deref() == Some(name),
        _ => false,
    })
}

/// Whether `name` is a builtin the compiler knows without a declaration.
///
/// These have no `pub type` anywhere, so [`exported_here`] cannot see them and
/// the module that gives them their methods has to be allowed to document
/// them. `std::core` is that module for all of these and there is no other
/// candidate: an `impl String` in somebody's package is refused by coherence.
///
/// The fixed-width names mirror `khora_types::IntKind::parse`, where `I64` is
/// deliberately absent because it is a second spelling of `Int`. Written out
/// rather than depending on `khora-types` for one list — this crate reads the
/// syntax tree and nothing else, which is what keeps it fast enough to run on
/// every save.
fn is_primitive(name: &str) -> bool {
    matches!(
        name,
        "Int"
            | "Float"
            | "Bool"
            | "Char"
            | "String"
            | "I8"
            | "I16"
            | "I32"
            | "I64"
            | "U8"
            | "U16"
            | "U32"
            | "U64"
    )
}

/// `List` out of `List<A>`, which is what the declaration is called.
fn base_name(text: &str) -> &str {
    text.split(['<', ' ']).next().unwrap_or(text)
}

fn functions(decls: impl Iterator<Item = ast::FnDecl>) -> impl Iterator<Item = Item> {
    decls.filter_map(|f| {
        Some(Item {
            name: named(f.name())?,
            kind: Kind::Function,
            signature: signature::function(&f),
            doc: doc_of(f.syntax()),
            members: Vec::new(),
        })
    })
}

/// The operations an effect declares.
///
/// **This was `Vec::new()`**, and it is the whole of why an effect's page was a
/// signature block with nothing under it. Every other kind that has members
/// collects them; effects were the one that did not, and they are the kind
/// whose members carry the most documentation -- an operation is where the
/// decision about what a capability may do is written down. Twelve effects and
/// 155 lines of it never reached the site.
fn operations(fields: impl Iterator<Item = ast::Field>) -> Vec<Item> {
    fields
        .filter_map(|field| {
            Some(Item {
                name: named(field.name())?,
                kind: Kind::Operation,
                signature: signature::one_line(field.syntax()),
                doc: doc_of(field.syntax()),
                members: Vec::new(),
            })
        })
        .collect()
}

fn assoc_types(decls: impl Iterator<Item = ast::AssocTypeDecl>) -> impl Iterator<Item = Item> {
    decls.filter_map(|a| {
        Some(Item {
            name: named(a.name())?,
            kind: Kind::AssocType,
            signature: signature::declaration(a.syntax()),
            doc: doc_of(a.syntax()),
            members: Vec::new(),
        })
    })
}

/// The cases of a variant type, or the fields of a record.
fn definition_members(t: &ast::TypeDecl) -> Vec<Item> {
    let mut out = Vec::new();
    let Some(definition) = t.definition() else { return out };
    match definition {
        ast::Type::Variant(v) => {
            for case in v.cases() {
                let Some(name) = named(case.name()) else { continue };
                out.push(Item {
                    name,
                    kind: Kind::Variant,
                    signature: signature::one_line(case.syntax()),
                    doc: doc_of(case.syntax()),
                    members: Vec::new(),
                });
            }
        }
        ast::Type::Record(r) => {
            for field in r.fields() {
                let Some(name) = named(field.name()) else { continue };
                out.push(Item {
                    name,
                    kind: Kind::Field,
                    signature: signature::one_line(field.syntax()),
                    doc: doc_of(field.syntax()),
                    members: Vec::new(),
                });
            }
        }
        _ => {}
    }
    out
}

fn named(name: Option<ast::Name>) -> Option<String> {
    name.and_then(|n| n.ident())
}

fn const_name(c: &ast::ConstDecl) -> Option<String> {
    match c.pat() {
        Some(ast::Pat::Ident(p)) => named(p.name()),
        _ => None,
    }
}

// --- comments ---------------------------------------------------------------

/// The `///` block immediately above a declaration.
///
/// **A blank line ends it.** Two comment blocks separated by one are two
/// different things -- the upper one is almost always about the item above, or
/// about the section -- and running them together produces documentation that
/// starts mid-thought. Anything that is not whitespace or a `///` line ends it
/// too, which is what stops a plain `//` note from being published.
fn doc_of(node: &SyntaxNode) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cursor = preceding(node);
    while let Some(element) = cursor {
        let Some(token) = element.as_token() else { break };
        match token.kind() {
            SyntaxKind::WHITESPACE => {
                if token.text().matches('\n').count() > 1 {
                    break;
                }
            }
            SyntaxKind::LINE_COMMENT => match strip(token.text(), "///") {
                Some(text) => lines.push(text),
                None => break,
            },
            _ => break,
        }
        cursor = element.prev_sibling_or_token();
    }
    lines.reverse();
    lines
}

/// What comes immediately before `node` in the file, looking outward.
///
/// **The comment for a first child is written outside its parent.** The `///`
/// above the first case of a variant type sits between the `=` and the
/// `VARIANT_TYPE` node -- a sibling of the whole list, not of the case -- so
/// asking the case for its previous sibling gets nothing. Climbing while the
/// node starts where its parent does finds it, and stops as soon as there is
/// anything else in between.
fn preceding(node: &SyntaxNode) -> Option<khora_syntax::SyntaxElement> {
    let mut here = node.clone();
    loop {
        if let Some(found) = here.prev_sibling_or_token() {
            return Some(found);
        }
        let parent = here.parent()?;
        if parent.text_range().start() != here.text_range().start() {
            return None;
        }
        here = parent;
    }
}

/// Every `//!` line before the first declaration.
///
/// Position within that is not fixed on purpose: `std` writes its module
/// narration after the imports and a reader would reasonably write it before
/// them. What ends the block is the first declaration, because a `//!` after
/// one is describing something narrower than the module and there is no sense
/// in which it could be the module's own text.
fn module_doc(file: &SyntaxNode) -> Vec<String> {
    let mut lines = Vec::new();
    for element in file.children_with_tokens() {
        match element {
            khora_syntax::SyntaxElement::Node(node) => {
                if ast::Decl::cast(node).is_some() {
                    break;
                }
            }
            khora_syntax::SyntaxElement::Token(token) => {
                if token.kind() == SyntaxKind::LINE_COMMENT {
                    if let Some(text) = strip(token.text(), "//!") {
                        lines.push(text);
                    }
                }
            }
        }
    }
    lines
}

/// The text of a doc line, or `None` if the comment is not one.
///
/// One leading space after the marker is removed and no more, so an indented
/// code block keeps its indentation.
fn strip(text: &str, marker: &str) -> Option<String> {
    let rest = text.trim_end_matches(['\r', '\n']).strip_prefix(marker)?;
    // `////` is a divider somebody drew, not four slashes of documentation.
    if marker == "///" && rest.starts_with('/') {
        return None;
    }
    Some(rest.strip_prefix(' ').unwrap_or(rest).to_string())
}

#[cfg(test)]
mod tests;
