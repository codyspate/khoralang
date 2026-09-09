//! Where a bracketed ``[`Name`]`` reference in a doc comment lands.
//!
//! `std` is written with rustdoc's shortcut references -- ``[`Raw`]``,
//! ``[`Schema::record`]``, ``[`Array::from_fn`]`` -- and CommonMark resolves a
//! shortcut reference only against a *link reference definition* somewhere in
//! the same document. There was never one, so ninety-one of them reached the
//! site as literal brackets around a code span: the reader saw ``[`Raw`]`` and
//! there was nothing to click.
//!
//! Two outcomes and no third. A reference whose target is a heading on the page
//! becomes an inline link to it; a reference to something the page does not
//! have -- a private helper, a name from another module -- loses its brackets
//! and renders as the code span it already looked like. Nothing dangling is
//! emitted either way, and `tests/intra_doc_links.rs` fails the build over
//! every page in `std` if one ever is again.
//!
//! **A private item never becomes a link.** `exit_status` in
//! `std::process::shell` is `fn` and not `pub fn`, so it has no heading and no
//! anchor; linking it to the nearest similarly named thing would send a reader
//! somewhere they did not ask to go. It degrades to code, which is what the
//! prose around it means anyway.

use std::collections::{HashMap, HashSet};

/// The anchor a heading gets on the built site.
///
/// Astro gives every heading an id with `github-slugger`, over the heading's
/// rendered text: lowercased, punctuation dropped, spaces hyphenated, and
/// `-1`, `-2` appended to a slug the page has already used. The numbering is
/// not a corner case here -- `std::core` has four `### List<A>` headings, one
/// per `impl` block -- so a generator that linked to `#lista` four times would
/// be wrong three times.
#[derive(Default)]
pub struct Slugger {
    taken: HashSet<String>,
    counts: HashMap<String, usize>,
}

impl Slugger {
    /// The anchor for one markdown heading line, in the order it is emitted.
    ///
    /// Order is the whole of it: the same heading text gets a different anchor
    /// depending on what came before it, so this has to be called for *every*
    /// heading the page emits, including the section headings and the ones an
    /// author wrote inside a doc comment.
    pub fn slug(&mut self, heading: &str) -> String {
        let base = slug(&rendered(heading));
        let mut candidate = base.clone();
        while self.taken.contains(&candidate) {
            let n = self.counts.entry(base.clone()).or_insert(0);
            *n += 1;
            candidate = format!("{base}-{n}");
        }
        self.taken.insert(candidate.clone());
        candidate
    }
}

/// A heading line as a reader sees it, with the markdown taken off.
///
/// The slug is taken from the *rendered* text, which is what makes the two
/// passes in `markdown` agree: ``[`Raw`]``, `` `Raw` `` and ``[`Raw`](#raw)``
/// all render as `Raw`, so resolving a reference inside a heading cannot move
/// that heading's anchor. Only the link destination has to be cut out by hand;
/// every other marker is punctuation that [`slug`] drops anyway.
fn rendered(heading: &str) -> String {
    let text = heading.trim_start_matches('#').trim();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        out.push_str(&rest[..at]);
        match rest[at..].find(')') {
            Some(end) => rest = &rest[at + end + 1..],
            None => {
                rest = &rest[at..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `github-slugger` for the characters a heading on this site contains.
///
/// Lowercased, spaces hyphenated, ASCII punctuation dropped except `-` and
/// `_`. `Result<A, E>` becomes `resulta-e`, which looks like nothing but is
/// what the built page uses.
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == ' ' {
            out.push('-');
        } else if ch.is_ascii_control() || dropped(ch) {
            continue;
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out
}

fn dropped(ch: char) -> bool {
    ch.is_ascii_punctuation() && ch != '-' && ch != '_'
}

/// Every heading a reference can name, and the anchor it landed on.
///
/// Keyed the way an author writes the reference: `Raw` for a declaration,
/// `Raw::to_json` for one of its members. **The first heading to claim a key
/// keeps it**, and the page emits declarations before `impl` blocks, so
/// ``[`Raw`]`` reaches `pub type Raw` rather than `impl Raw` -- the reader
/// asking what a `Raw` is wants the type.
#[derive(Default)]
pub struct Targets {
    anchors: HashMap<String, String>,
    weak: HashMap<String, String>,
}

impl Targets {
    /// Records `key` as living at `anchor`, unless something already does.
    pub fn record(&mut self, key: String, anchor: &str) {
        self.anchors.entry(key).or_insert_with(|| anchor.to_string());
    }

    /// Records `key` as reachable at `anchor`, but only for want of better.
    ///
    /// A case or a field with nothing said about it gets no heading of its
    /// own, so the nearest true thing is the declaration that lists it. That
    /// is worth a link, and it is not worth outranking a heading somebody
    /// wrote: `secret` is a field of `Rejection` *and* the combinator that
    /// sets the field, and a reference from inside `impl Rejection` means the
    /// combinator.
    pub fn record_weak(&mut self, key: String, anchor: &str) {
        self.weak.entry(key).or_insert_with(|| anchor.to_string());
    }

    /// Where `reference` points, if anywhere on this page.
    ///
    /// A bare name is looked for among `scope`'s members first and then among
    /// the page's declarations, which is what an author means: ``[`slice`]``
    /// written inside `impl String` is `String::slice`, and ``[`Ordering`]``
    /// written anywhere is the type.
    pub fn anchor(&self, reference: &str, scope: Option<&str>) -> Option<&str> {
        let scoped = scope.map(|scope| format!("{scope}::{reference}"));
        let qualified = reference.contains("::");
        let strong = if qualified {
            self.anchors.get(reference)
        } else {
            scoped
                .as_ref()
                .and_then(|key| self.anchors.get(key))
                .or_else(|| self.anchors.get(reference))
        };
        strong
            .or_else(|| if qualified { self.weak.get(reference) } else { None })
            .or_else(|| scoped.as_ref().and_then(|key| self.weak.get(key)))
            .map(String::as_str)
    }
}

/// One block of doc text with its shortcut references settled.
///
/// Fenced blocks are left exactly as written -- a `[` in a code sample is code
/// -- which is the same rule the heading shift uses and for the same reason.
pub fn resolved(text: &str, targets: &Targets, scope: Option<&str>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fenced = false;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            out.push_str(line);
            continue;
        }
        if fenced {
            out.push_str(line);
            continue;
        }
        out.push_str(&line_resolved(line, targets, scope));
    }
    out
}

fn line_resolved(line: &str, targets: &Targets, scope: Option<&str>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("[`") {
        let after = &rest[at + 2..];
        let Some(end) = after.find("`]") else { break };
        let name = &after[..end];
        let tail = &after[end + 2..];
        // `[`x`](url)` and `[`x`][y]` are already links. Neither appears in
        // `std` today, and neither is this function's to rewrite.
        if name.contains('`') || tail.starts_with('(') || tail.starts_with('[') {
            out.push_str(&rest[..at + 2]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..at]);
        match targets.anchor(name, scope) {
            Some(anchor) => out.push_str(&format!("[`{name}`](#{anchor})")),
            None => out.push_str(&format!("`{name}`")),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}
