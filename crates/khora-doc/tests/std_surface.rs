//! Every public item in `std` is a promise. This one asks whether it is
//! written down.
//!
//! `docs/design/compatibility.md` lists "the `std` audit" among the things 1.0
//! requires and does not have: *every public item in `std` is a promise at
//! 1.0, and the set has never been reviewed with that in mind — several exist
//! because a reference application needed them at the time.* Roadmap 13.11.
//!
//! A review is a person's job. What a test can do is hold the floor: an item
//! nobody could be bothered to describe in one line is an item nobody has
//! decided to promise, and it should not reach 1.0 by default. So this fails
//! on an undocumented public item, and the fix is to write the line or to
//! stop exporting the thing — which is the decision the audit is asking for.
//!
//! Reads the files rather than a generated page, because the generated page is
//! downstream of exactly what is being checked.

use std::path::{Path, PathBuf};

use khora_doc::{Item, Kind};

/// `std/`, from this crate.
fn std_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a readable directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
    out.sort();
}

/// Whether an item of this kind has to carry a line of its own.
///
/// **A field and a variant do not.** A record whose type is documented and
/// whose field is called `port` is described; demanding a sentence per field
/// produces `/// The port.`, which is worse than nothing because it looks like
/// documentation. The types, functions, traits and effects are the surface a
/// person reads about, and those must say something.
///
/// **Nor a method of a trait impl.** `impl Show for Decimal { fn show }` is
/// the trait's `show`, and the sentence describing what `show` means belongs
/// on the trait, once, rather than on each of the fourteen types that have
/// one. The impl *block* is excluded here and its methods by `parent` below.
fn must_be_described(kind: Kind, parent: Option<Kind>) -> bool {
    if parent == Some(Kind::TraitImpl) {
        return false;
    }
    matches!(
        kind,
        Kind::Type | Kind::Trait | Kind::Effect | Kind::Context | Kind::Function | Kind::Const
    )
}

/// Every described-or-not item, flattened, with the path a reader would take
/// to it and the kind of whatever declared it.
fn walk(
    prefix: &str,
    parent: Option<Kind>,
    items: &[Item],
    out: &mut Vec<(String, Kind, Option<Kind>, bool)>,
) {
    for item in items {
        let name =
            if prefix.is_empty() { item.name.clone() } else { format!("{prefix}::{}", item.name) };
        out.push((name.clone(), item.kind, parent, !item.doc.is_empty()));
        // A `Methods` block is not itself a promise; its functions are.
        walk(&name, Some(item.kind), &item.members, out);
    }
}

#[test]
fn every_public_item_in_std_says_what_it_is() {
    let mut files = Vec::new();
    sources(&std_root(), &mut files);
    assert!(files.len() > 10, "expected to find `std`, found {} files", files.len());

    let mut missing: Vec<String> = Vec::new();
    let mut counted = 0usize;

    for path in &files {
        let text = std::fs::read_to_string(path).expect("readable");
        let parsed = khora_syntax::parse(&text);
        if !parsed.ok() {
            panic!("{} does not parse: {:?}", path.display(), parsed.errors());
        }
        let module = khora_doc::module_of(&parsed.source_file());
        let where_ = module.path.clone().unwrap_or_else(|| path.display().to_string());

        let mut flat = Vec::new();
        walk("", None, &module.items, &mut flat);
        for (name, kind, parent, described) in flat {
            if !must_be_described(kind, parent) {
                continue;
            }
            counted += 1;
            if !described {
                missing.push(format!("{where_}::{name} ({})", kind.describe()));
            }
        }
    }

    assert!(counted > 100, "expected a public surface, counted {counted} items");
    assert!(
        missing.is_empty(),
        "{} of {counted} public items in `std` carry no `///`:\n  {}\n\n\
         Write the line, or stop exporting it. `docs/design/compatibility.md` \
         calls this the `std` audit.",
        missing.len(),
        missing.join("\n  ")
    );
}
