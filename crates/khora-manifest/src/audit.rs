//! The unknown-key audit.
//!
//! The typed deserialize in [`crate::model`] drops keys it does not know, which
//! is the behavior we want -- a manifest written for a newer toolchain must
//! still build with an older one -- but it leaves nothing to report. So the
//! document is read a second time as a plain tree of keys and compared against
//! [`ROOT`], and every key the schema does not mention becomes a [`Warning`].
//!
//! [`ROOT`] duplicates the field names in [`crate::model`]. Keeping the two in
//! step is what the "every documented key is recognized" test is for.

use crate::error::ManifestError;
use crate::warning::Warning;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use std::fmt;
use std::ops::Range;
use toml::Spanned;

/// What this toolchain understands at one point in the document.
enum Schema {
    /// A table whose keys this crate fixes.
    Fields(&'static [(&'static str, &'static Schema)]),
    /// A table whose keys the manifest author chooses; every value is described
    /// by the same schema.
    Map(&'static Schema),
    /// Anything at all. Scalars, arrays, and tables whose keys are defined by
    /// something other than the manifest schema.
    Open,
}

static OPEN: Schema = Schema::Open;

static ROOT: Schema = Schema::Fields(&[
    ("package", &PACKAGE),
    ("permissions", &PERMISSIONS),
    ("fmt", &FMT),
    ("lints", &LINTS),
    ("dependencies", &DEPENDENCIES),
    ("build", &BUILD),
    ("tasks", &TASKS),
    ("toolchain", &TOOLCHAIN),
]);

static PACKAGE: Schema = Schema::Fields(&[
    ("name", &OPEN),
    ("version", &OPEN),
    ("authors", &OPEN),
    ("edition", &OPEN),
    ("publish", &OPEN),
]);

static PERMISSIONS: Schema = Schema::Fields(&[
    ("default", &OPEN),
    ("network", &OPEN),
    ("fs", &OPEN),
    ("env", &OPEN),
    ("extern", &OPEN),
]);

static TOOLCHAIN: Schema = Schema::Fields(&[("version", &OPEN)]);

static FMT: Schema = Schema::Fields(&[
    ("indent-style", &OPEN),
    ("indent-width", &OPEN),
    ("explicit-semicolons", &OPEN),
]);

// Open on purpose: a lint's options are declared by the lint, not by the
// manifest format, so `max = 15` must not read as a mistake.
static LINTS: Schema = Schema::Map(&OPEN);

static DEPENDENCIES: Schema = Schema::Map(&DEPENDENCY);
static DEPENDENCY: Schema = Schema::Fields(&[
    ("version", &OPEN),
    ("path", &OPEN),
    ("git", &OPEN),
    ("rev", &OPEN),
    ("tag", &OPEN),
    ("subdir", &OPEN),
]);

static BUILD: Schema = Schema::Fields(&[("target", &OPEN), ("plugin", &OPEN)]);

static TASKS: Schema = Schema::Map(&TASK);
static TASK: Schema = Schema::Fields(&[("description", &OPEN), ("depends_on", &OPEN)]);

/// Collects a warning for every key in `text` that [`ROOT`] does not describe.
///
/// `text` must already have parsed as a manifest; this is the second read.
pub(crate) fn unknown_keys(text: &str) -> Result<Vec<Warning>, ManifestError> {
    let mut warnings = Vec::new();
    walk(&document(text)?, &ROOT, &mut String::new(), text, &mut warnings);
    // `toml` hands back a table's keys sorted, not in the order they were
    // written. Diagnostics are read top to bottom, so put them back.
    warnings.sort_by_key(|warning| warning.span().map_or(usize::MAX, |span| span.start));
    Ok(warnings)
}

/// Reads the document as a tree of keys, keeping source positions where it can.
fn document(text: &str) -> Result<Node, ManifestError> {
    // The spanned read fails on any value `toml` models as a private one-key
    // table -- a date or time -- because those synthetic keys carry no span.
    // That can only happen under a key we do not recognize, which is precisely
    // the forward-compatible manifest this audit exists to accept, so drop to a
    // position-free read rather than turn a warning into a hard error.
    if let Ok(node) = toml::from_str::<Node>(text) {
        return Ok(node);
    }
    let value =
        toml::from_str::<toml::Value>(text).map_err(|error| ManifestError::from_toml(error, text))?;
    Ok(Node::from_value(&value))
}

fn walk(node: &Node, schema: &Schema, path: &mut String, text: &str, out: &mut Vec<Warning>) {
    let Node::Table(entries) = node else { return };

    for entry in entries {
        let below = match schema {
            // Whatever keys sit below belong to something other than the
            // manifest schema, so there is nothing here to check them against.
            Schema::Open => return,
            Schema::Map(values) => Some(*values),
            Schema::Fields(fields) => {
                fields.iter().find(|(name, _)| *name == entry.key).map(|(_, values)| *values)
            }
        };

        let restore = path.len();
        push_key(path, &entry.key);
        match below {
            Some(values) => walk(&entry.value, values, path, text, out),
            // Reported at the unknown key itself and not descended into: a
            // warning per key underneath an unknown table would bury the one
            // line worth reading.
            None => out.push(Warning::unknown_key(path.clone(), entry.span.clone(), text)),
        }
        path.truncate(restore);
    }
}

/// Appends one key to a dotted path.
///
/// Quotes the way TOML does, so `dependencies."std.effect"` cannot be misread as
/// three nested tables -- which matters here, because dotted dependency names
/// are the norm.
fn push_key(path: &mut String, key: &str) {
    if !path.is_empty() {
        path.push('.');
    }
    let bare = !key.is_empty()
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        path.push_str(key);
    } else {
        path.push('"');
        path.push_str(&key.replace('\\', "\\\\").replace('"', "\\\""));
        path.push('"');
    }
}

/// One node of the document as the audit sees it.
enum Node {
    Table(Vec<Entry>),
    /// A scalar or an array: nothing below it is keyed, so the walk stops.
    Leaf,
}

struct Entry {
    key: String,
    /// Absent when the document had to be re-read without spans.
    span: Option<Range<usize>>,
    value: Node,
}

impl Node {
    fn from_value(value: &toml::Value) -> Node {
        match value {
            toml::Value::Table(table) => Node::Table(
                table
                    .iter()
                    .map(|(key, value)| Entry {
                        key: key.clone(),
                        span: None,
                        value: Node::from_value(value),
                    })
                    .collect(),
            ),
            _ => Node::Leaf,
        }
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Node, D::Error> {
        deserializer.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any TOML value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Node, E> {
        Ok(Node::Leaf)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Node, E> {
        Ok(Node::Leaf)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Node, E> {
        Ok(Node::Leaf)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Node, E> {
        Ok(Node::Leaf)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Node, E> {
        Ok(Node::Leaf)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Node, E> {
        Ok(Node::Leaf)
    }

    fn visit_none<E: de::Error>(self) -> Result<Node, E> {
        Ok(Node::Leaf)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Node, D::Error> {
        Node::deserialize(deserializer)
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        Node::deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        // Arrays hold no keys, but they must still be drained for the
        // deserializer to stay in step with the document.
        while seq.next_element::<de::IgnoredAny>()?.is_some() {}
        Ok(Node::Leaf)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut entries = Vec::new();
        while let Some(key) = map.next_key::<Spanned<String>>()? {
            let span = key.span();
            entries.push(Entry {
                key: key.into_inner(),
                span: Some(span),
                value: map.next_value::<Node>()?,
            });
        }
        Ok(Node::Table(entries))
    }
}
