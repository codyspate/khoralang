//! `extern fn` without a body -- a C function this program calls.
//!
//! **What this prevents: `khora check` passing a declaration `khora build`
//! refuses.** Applied only where a call is generated, which `khora check`
//! never reaches, a declaration that can raise checks clean and fails to
//! build, and a second declaration of a C symbol `std` already declares checks
//! clean and fails to build as invalid LLVM IR, reported as a compiler bug.
//!
//! Neither rule depends on the target. Whether a symbol *exists* is the
//! linker's question, and is not asked here: a binding to a symbol one
//! platform lacks is fine on a platform that never calls it. Whether a
//! signature can cross the C ABI, and whether two declarations of one symbol
//! agree, are questions about the source. The backend keeps its own refusals
//! as a backstop.
//!
//! Cost: one pass over every file's declarations per root, cached by salsa,
//! and a lookup per `extern fn`.

use std::collections::HashMap;

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::HirError;
use khora_syntax::ast::{self, AstNode};
use text_size::TextRange;

use crate::{foreign_signature_obstacle, type_map, Signature, Type};

/// One declaration of a C symbol, anywhere in the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignDecl {
    /// The file it is written in.
    pub file: SourceFile,
    /// The module it was written in, as a reader would name it.
    pub module: String,
    /// The package it belongs to: the module path's first segment.
    pub package: String,
    /// The whole declaration, which is where a clash is reported.
    pub range: TextRange,
    /// What the declaration says the C function takes and returns.
    pub signature: Signature,
    /// `pub extern fn` with a body: this program *defines* the symbol.
    pub defines: bool,
    /// The targets whose build includes the file: a `_linux` file and a
    /// `_windows` one may each bind `socket` their own way, because no build
    /// reads both.
    pub targets: Vec<&'static str>,
}

/// Every target family a file name can select for.
const FAMILIES: [&str; 4] = ["linux", "macos", "windows", "wasm"];

/// Every `extern fn` in the program, by the C symbol it names.
///
/// One query over the whole root, so each file's check looks its symbols up
/// rather than reparsing every other file.
#[salsa::tracked(returns(ref))]
pub fn foreign_symbols(db: &dyn Db, root: SourceRoot) -> HashMap<String, Vec<ForeignDecl>> {
    let mut out: HashMap<String, Vec<ForeignDecl>> = HashMap::new();
    let mut files: Vec<SourceFile> = root.files(db).to_vec();
    // Sorted, so which declaration is named as "the other one" does not
    // depend on the order the files were read in.
    files.sort_by_key(|file| file.path(db).clone());
    for file in files {
        let module = module_of(db, file);
        let targets = targets_of(db, file);
        let signatures = &type_map(db, file).signatures;
        for decl in khora_db::parse(db, file).source_file().decls() {
            let ast::Decl::Fn(f) = decl else { continue };
            if !f.is_extern() {
                continue;
            }
            let Some(name) = f.name().and_then(|n| n.ident()) else { continue };
            let Some(signature) = signatures.get(&name) else { continue };
            out.entry(name).or_default().push(ForeignDecl {
                file,
                module: module.clone(),
                package: package_of(db, file),
                range: f.syntax().text_range(),
                signature: signature.clone(),
                defines: f.body().is_some(),
                targets: targets.clone(),
            });
        }
    }
    out
}

/// Everything wrong with this file's `extern fn` imports, and with any
/// `extern fn` here whose symbol another declaration also names.
pub(crate) fn foreign_import_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let mut found = Vec::new();
    let signatures = &type_map(db, file).signatures;
    let symbols = khora_db::source_root(db).map(|root| foreign_symbols(db, root));
    let no_roots = std::collections::HashSet::new();
    let roots = match khora_db::source_root(db) {
        Some(root) => root_packages(db, root),
        None => &no_roots,
    };

    for decl in khora_db::parse(db, file).source_file().decls() {
        let ast::Decl::Fn(f) = decl else { continue };
        if !f.is_extern() {
            continue;
        }
        let Some(name) = f.name().and_then(|n| n.ident()) else { continue };
        let Some(signature) = signatures.get(&name) else { continue };
        let range = f.syntax().text_range();

        // An export's own shape is `exports.rs`'s, which says it in the
        // export's terms; this is the import's.
        if f.body().is_none() {
            if let Some(why) = foreign_signature_obstacle(signature) {
                found.push(HirError {
                    message: format!(
                        "`{name}` is a C function, so its signature crosses the C ABI, and {why}. \
                         Only scalars and pointers cross"
                    ),
                    range,
                });
                continue;
            }
        }

        let Some(others) = symbols.and_then(|s| s.get(&name)) else { continue };
        let me = |d: &&ForeignDecl| d.file == file && d.range == range;
        // **Told where somebody can act on it.** Another package's declaration
        // -- `std`'s, or a dependency's -- is not the reader's to change, so a
        // clash between packages is reported only in a root package, the one
        // being built, and a dependency stays quiet. Within one package both
        // declarations are told: they are that package's own mistake.
        let here = package_of(db, file);
        let here_is_root = roots.contains(&here);
        let targets = targets_of(db, file);
        let Some(other) = others.iter().find(|d| {
            !me(d)
                && (d.package == here || here_is_root)
                && d.targets.iter().any(|t| targets.contains(t))
                && clashes(signature, f.body().is_some(), d)
        }) else {
            continue;
        };
        found.push(HirError { message: clash_message(&name, f.body().is_some(), other), range });
    }
    if roots.contains(&package_of(db, file)) {
        found.extend(dependency_clash_errors(db, file, symbols, roots));
    }
    found
}

/// Two other packages declaring one C symbol differently, told at this root
/// file's `import` of one of them.
///
/// Neither declaration is the reader's to edit, so neither is where the error
/// goes; the import is where this program chose to have both. Reported once
/// per pair of packages per file, at the import of whichever of the two this
/// file names later. A file that imports neither says nothing, and a pair
/// reached only through other dependencies is not reported here: no line of
/// the program's own names it.
fn dependency_clash_errors(
    db: &dyn Db,
    file: SourceFile,
    symbols: Option<&HashMap<String, Vec<ForeignDecl>>>,
    roots: &std::collections::HashSet<String>,
) -> Vec<HirError> {
    let Some(symbols) = symbols else { return Vec::new() };
    let here = package_of(db, file);
    let targets = targets_of(db, file);
    // The last import of each package this file names.
    let mut imported: HashMap<String, TextRange> = HashMap::new();
    for import in khora_db::parse(db, file).source_file().imports() {
        let Some(path) = import.path() else { continue };
        let Some(package) = path.text_path().split("::").next().map(str::to_string) else {
            continue;
        };
        imported.insert(package, import.syntax().text_range());
    }

    let mut names: Vec<&String> = symbols.keys().collect();
    names.sort();
    let mut told: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut found = Vec::new();
    for name in names {
        let decls = &symbols[name];
        for (i, a) in decls.iter().enumerate() {
            for b in &decls[i + 1..] {
                let (pa, pb) = (&a.package, &b.package);
                if pa == pb || pa == &here || pb == &here || roots.contains(pa) || roots.contains(pb) {
                    continue;
                }
                let both_built =
                    targets.iter().any(|t| a.targets.contains(t) && b.targets.contains(t));
                if !both_built || !clashes(&a.signature, a.defines, b) {
                    continue;
                }
                let at = match (imported.get(pa), imported.get(pb)) {
                    (Some(x), Some(y)) => Some(if x.start() > y.start() { *x } else { *y }),
                    (Some(x), None) | (None, Some(x)) => Some(*x),
                    (None, None) => None,
                };
                let Some(at) = at else { continue };
                let pair = if pa < pb { (pa.clone(), pb.clone()) } else { (pb.clone(), pa.clone()) };
                if !told.insert(pair) {
                    continue;
                }
                found.push(HirError {
                    message: format!(
                        "the packages `{pa}` and `{pb}` both declare the C symbol `{name}`, and \
                         disagree: `{}` has `extern fn {name}{}` and `{}` has `extern fn \
                         {name}{}`. A C symbol has no module, so the two are one C function \
                         and only one of the signatures can be right. Neither is in this \
                         package; until one of them is changed to match the other, depend on \
                         only one of them",
                        a.module,
                        written(&a.signature),
                        b.module,
                        written(&b.signature),
                    ),
                    range: at,
                });
            }
        }
    }
    found
}

/// The package a file belongs to: the first segment of its module path, or
/// its path for a file that declares no module.
fn package_of(db: &dyn Db, file: SourceFile) -> String {
    match khora_hir::item_map(db, file).module.as_ref() {
        Some(module) => module.segments().first().cloned().unwrap_or_default(),
        None => file.path(db).display().to_string(),
    }
}

/// The packages being built, as against the ones they depend on.
///
/// Read from the imports, because a source root does not say which of its
/// packages was asked for: a package another package imports is a
/// dependency, and the rest are roots. `std` is imported by every program and
/// is never a root unless it is all there is, which is how `std` is checked
/// on its own.
#[salsa::tracked(returns(ref))]
pub fn root_packages(db: &dyn Db, root: SourceRoot) -> std::collections::HashSet<String> {
    let mut all = std::collections::HashSet::new();
    let mut imported = std::collections::HashSet::new();
    for file in root.files(db) {
        let here = package_of(db, *file);
        for import in khora_db::parse(db, *file).source_file().imports() {
            let Some(path) = import.path() else { continue };
            if let Some(package) = path.text_path().split("::").next() {
                if package != here {
                    imported.insert(package.to_string());
                }
            }
        }
        all.insert(here);
    }
    all.retain(|package| !imported.contains(package));
    // A program that imports nothing from `std` still has it in its source
    // root, and `std` is never what is being built unless it is all there is.
    if all.len() > 1 {
        all.remove("std");
    }
    all
}

/// The module a file declares, as it is written in source (`std::net::socket`);
/// its path if it declares none.
fn module_of(db: &dyn Db, file: SourceFile) -> String {
    khora_hir::item_map(db, file)
        .module
        .as_ref()
        .map(|module| module.segments().join("::"))
        .unwrap_or_else(|| file.path(db).display().to_string())
}

/// The target families whose build reads `file`, by its name's suffix.
fn targets_of(db: &dyn Db, file: SourceFile) -> Vec<&'static str> {
    let path = file.path(db);
    FAMILIES.into_iter().filter(|family| khora_db::selected_for_target(path, family)).collect()
}

/// Whether a declaration of `name` here and `other` cannot both stand.
///
/// Two imports clash when they disagree about the signature: they are one C
/// function, and a call compiled against either is wrong for the other. A
/// definition clashes with any other declaration of its symbol, because the
/// program's own function would replace whatever the other one meant.
fn clashes(mine: &Signature, i_define: bool, other: &ForeignDecl) -> bool {
    if i_define || other.defines {
        return true;
    }
    // Generic or otherwise uncrossable: refused on its own account, and its
    // shape is not one to compare against.
    if foreign_signature_obstacle(&other.signature).is_some() {
        return false;
    }
    mine.params != other.signature.params || returns(mine) != returns(&other.signature)
}

/// A C return: `()` and `Never` both return nothing a caller reads.
fn returns(signature: &Signature) -> &Type {
    match signature.ret {
        Type::Never => &Type::Unit,
        ref other => other,
    }
}

fn clash_message(name: &str, i_define: bool, other: &ForeignDecl) -> String {
    let theirs = format!(
        "{}extern fn {name}{}",
        if other.defines { "pub " } else { "" },
        written(&other.signature)
    );
    if i_define && other.defines {
        return format!(
            "two functions are exported to C as `{name}`: this one and the one in `{}`. A C \
             symbol has no module to be qualified by, so one of them has to be renamed",
            other.module
        );
    }
    if i_define || other.defines {
        return format!(
            "`{name}` is a C symbol, and `{}` already declares it as `{theirs}`. A C \
             symbol has no module to tell two apart, so a program can define one only if \
             nothing else names it. Rename the exported function",
            other.module
        );
    }
    format!(
        "`{name}` is a C symbol, and `{}` already declares it as `{theirs}`. A C symbol has \
         no module, so the two declarations are one C function and must agree. Declare it \
         as `{theirs}`, and convert in a Khora function that calls it",
        other.module
    )
}

/// `(handle: I32) -> I32`, as the declaration would be written.
fn written(signature: &Signature) -> String {
    let params: Vec<String> = signature
        .params
        .iter()
        .enumerate()
        .map(|(index, ty)| match signature.names.get(index).cloned().flatten() {
            Some(name) => format!("{name}: {ty}"),
            None => format!("_: {ty}"),
        })
        .collect();
    format!("({}) -> {}", params.join(", "), signature.ret)
}
