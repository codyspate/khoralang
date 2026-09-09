//! Nothing bracketed reaches the page without somewhere to go.
//!
//! `std` is written with rustdoc's shortcut references -- a name in backticks
//! inside square brackets. CommonMark resolves one only against a link
//! reference definition in the same document, and a generated page has none,
//! so for a long time ninety-one of them rendered as literal brackets: the
//! reader saw the markup and had nothing to click. It read as a broken page
//! because it was one.
//!
//! So this asks two things of every page `khora doc` would write for `std`.
//! Nothing dangling: every bracketed reference is either a link or has lost
//! its brackets and become plain code. And nothing lying: every link it does
//! emit points at an anchor that heading order actually produces, worked out
//! here by a second implementation of the slug rule rather than by asking the
//! one under test.
//!
//! Reads the sources rather than `website/`, because a check against the
//! committed pages passes for as long as nobody regenerates them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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

/// Every page `khora doc` would write for `std`, keyed by module path.
fn pages() -> Vec<(String, String)> {
    let mut files = Vec::new();
    sources(&std_root(), &mut files);
    assert!(files.len() > 10, "expected to find `std`, found {} files", files.len());

    let mut read = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).expect("readable");
        let parsed = khora_syntax::parse(&text);
        assert!(parsed.ok(), "{} does not parse: {:?}", path.display(), parsed.errors());
        read.push(khora_doc::module_of(&parsed.source_file()));
    }

    khora_doc::merge(read)
        .iter()
        .filter_map(|m| Some((m.path.clone()?, khora_doc::markdown(m))))
        .collect()
}

/// The heading ids a page gets on the built site.
///
/// Written out again here, on purpose. The generator has its own copy, and a
/// test that called it would pass whatever the two agreed to be wrong about.
/// Astro gives a heading its id with `github-slugger`: the rendered text,
/// lowercased, ASCII punctuation dropped except `-` and `_`, spaces
/// hyphenated, and `-1`, `-2` appended to a slug the page has already used.
fn anchors(page: &str) -> Vec<String> {
    let mut taken: HashSet<String> = HashSet::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    let mut fenced = false;
    for line in page.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || !line.starts_with('#') {
            continue;
        }
        let mut base = String::new();
        for ch in strip_markup(line.trim_start_matches('#').trim()).chars() {
            if ch == ' ' {
                base.push('-');
            } else if ch.is_ascii_control()
                || (ch.is_ascii_punctuation() && ch != '-' && ch != '_')
            {
                continue;
            } else {
                base.extend(ch.to_lowercase());
            }
        }
        let mut anchor = base.clone();
        while taken.contains(&anchor) {
            let n = counts.entry(base.clone()).or_insert(0);
            *n += 1;
            anchor = format!("{base}-{n}");
        }
        taken.insert(anchor.clone());
        out.push(anchor);
    }
    out
}

/// A heading's text, with the link destinations and backticks taken off.
fn strip_markup(heading: &str) -> String {
    let mut out = String::new();
    let mut rest = heading;
    while let Some(at) = rest.find("](") {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at..].find(')') else { break };
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    out.replace('`', "")
}

/// Every bracketed code span outside a fence, from its `[` to end of line.
fn bracketed(page: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in page.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut at = 0;
        while let Some(found) = line[at..].find("[`") {
            out.push(&line[at + found..]);
            at += found + 2;
        }
    }
    out
}

#[test]
fn no_page_carries_a_reference_with_nowhere_to_go() {
    let mut dangling: Vec<String> = Vec::new();
    let mut links = 0usize;

    for (module, page) in pages() {
        for span in bracketed(&page) {
            // A resolved reference is `[`Name`](#anchor)`. Anything else that
            // opens a bracketed code span is markup the reader will see.
            let resolved = span[2..]
                .find('`')
                .is_some_and(|end| span[2 + end..].starts_with("`](#"));
            if resolved {
                links += 1;
            } else {
                let line: String = span.chars().take(60).collect();
                dangling.push(format!("{module}: {line}"));
            }
        }
    }

    assert!(
        dangling.is_empty(),
        "{} bracketed reference(s) reach the page as markup rather than as a \
         link:\n  {}\n\nA reference `khora doc` cannot resolve should lose its \
         brackets and render as code. See `khora_doc::links`.",
        dangling.len(),
        dangling.join("\n  ")
    );
    // The cheapest way to pass the check above is to link nothing, so the
    // floor is here. `std` carried ninety-one of these when the resolver was
    // written and all but the few naming private helpers now resolve.
    assert!(links > 60, "expected `std`'s references to resolve, {links} did");
}

#[test]
fn every_link_a_page_emits_lands_on_a_heading() {
    let mut broken: Vec<String> = Vec::new();
    for (module, page) in pages() {
        let anchors: HashSet<String> = anchors(&page).into_iter().collect();
        let mut rest = page.as_str();
        while let Some(at) = rest.find("](#") {
            rest = &rest[at + 3..];
            let end = rest.find(')').unwrap_or(rest.len());
            let anchor = &rest[..end];
            if !anchors.contains(anchor) {
                broken.push(format!("{module}: #{anchor}"));
            }
            rest = &rest[end..];
        }
    }
    assert!(
        broken.is_empty(),
        "{} link(s) point at an anchor no heading produces:\n  {}",
        broken.len(),
        broken.join("\n  ")
    );
}
