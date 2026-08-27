//! Fields a member takes from its workspace root.
//!
//! ```toml
//! # the root
//! [workspace]
//! members = ["packages/*"]
//!
//! [workspace.package]
//! version = "0.4.0"
//! edition = "2026"
//! authors = ["A Name <a@example.com>"]
//!
//! # a member
//! [package]
//! name = "postgres"
//! version.workspace = true
//! edition.workspace = true
//! ```
//!
//! # Why the resolved manifest has no trace of this
//!
//! [`crate::Manifest`] holds a `version: String`, not a "version or a promise
//! of one". Inheritance is resolved before a `Manifest` exists, so the twelve
//! places that read a version cannot forget the case where it has not arrived
//! yet — because after [`crate::Manifest::load`] there is no such case.
//!
//! The cost is that [`crate::Manifest::parse`], which takes text and no path,
//! *cannot* resolve `workspace = true` and says so as an error rather than
//! guessing. Everything that reads a manifest off disk uses `load`.
//!
//! # `workspace = true` and nothing else
//!
//! `workspace = false` is refused rather than read as "no". A field written out
//! only to say it is not inherited is a field somebody will read as inherited,
//! and the way to not inherit is to write the value or leave it out.

use std::fmt;
use std::marker::PhantomData;

use serde::de::value::{
    BoolDeserializer, F64Deserializer, I64Deserializer, SeqAccessDeserializer, StrDeserializer,
    U64Deserializer,
};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A manifest field written here, or taken from the workspace root.
///
/// Only for fields whose value is never a table — a version, an edition, a list
/// of authors, a `publish` flag. `{ workspace = true }` *is* a table, so there
/// is nothing to disambiguate and no need to peek at a key. The tables that are
/// inheritable — `[lints]`, `[fmt]`, `[permissions]` — carry a `workspace` key
/// of their own instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Maybe<T> {
    /// Written in this manifest.
    Own(T),
    /// `field.workspace = true`.
    FromWorkspace,
}

impl<T> Maybe<T> {
    /// The value if it was written here, resolving an inherited one from
    /// `root`.
    ///
    /// `None` means the manifest said nothing *and* the root said nothing,
    /// which is only an error for a field that is required.
    pub(crate) fn resolve(this: Option<Maybe<T>>, root: Option<T>) -> Resolved<T> {
        match this {
            Some(Maybe::Own(value)) => Resolved::Own(value),
            Some(Maybe::FromWorkspace) => match root {
                Some(value) => Resolved::Inherited(value),
                None => Resolved::Missing,
            },
            None => Resolved::Absent,
        }
    }
}

/// What a field turned out to be, once the root was consulted.
pub(crate) enum Resolved<T> {
    /// Written in this manifest.
    Own(T),
    /// Taken from the root.
    Inherited(T),
    /// Not written here at all.
    Absent,
    /// `workspace = true`, and the root does not have one.
    Missing,
}

impl<T> Resolved<T> {
    /// The value, or `None` for a field nobody supplied.
    ///
    /// `Missing` collapses to `None` here; the caller distinguishes it first
    /// when it wants to say *why* — "the root has no `version`" is a much
    /// better message than "no version", and only reachable when somebody
    /// asked to inherit one.
    pub(crate) fn into_option(self) -> Option<T> {
        match self {
            Resolved::Own(value) | Resolved::Inherited(value) => Some(value),
            Resolved::Absent | Resolved::Missing => None,
        }
    }

    /// Whether this asked the root for something the root does not have.
    pub(crate) fn is_missing(&self) -> bool {
        matches!(self, Resolved::Missing)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Maybe<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Maybe<T>, D::Error> {
        // `deserialize_any` rather than an untagged enum, for the reason
        // `Lint` gives: untagged buffers the value first, which costs the spans
        // that make a bad version reportable at its own line. It also costs the
        // error message — untagged reports "data did not match any variant".
        deserializer.deserialize_any(MaybeVisitor(PhantomData))
    }
}

struct MaybeVisitor<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Visitor<'de> for MaybeVisitor<T> {
    type Value = Maybe<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a value, or `{ workspace = true }` to take the workspace root's")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Maybe<T>, E> {
        T::deserialize(StrDeserializer::new(value)).map(Maybe::Own)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Maybe<T>, E> {
        T::deserialize(BoolDeserializer::new(value)).map(Maybe::Own)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Maybe<T>, E> {
        T::deserialize(I64Deserializer::new(value)).map(Maybe::Own)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Maybe<T>, E> {
        T::deserialize(U64Deserializer::new(value)).map(Maybe::Own)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Maybe<T>, E> {
        T::deserialize(F64Deserializer::new(value)).map(Maybe::Own)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Maybe<T>, A::Error> {
        T::deserialize(SeqAccessDeserializer::new(seq)).map(Maybe::Own)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Maybe<T>, A::Error> {
        // A table here is the inheritance form and nothing else: none of the
        // fields using `Maybe` has a table for a value, so `{ ... }` that is
        // not `{ workspace = true }` is a mistake worth naming.
        let Some(key) = map.next_key::<String>()? else {
            return Err(de::Error::custom("an empty table; write `workspace = true`"));
        };
        if key != "workspace" {
            return Err(de::Error::unknown_field(&key, &["workspace"]));
        }
        if !map.next_value::<bool>()? {
            return Err(de::Error::custom(
                "`workspace = false`; write the value here, or leave the field out",
            ));
        }
        if let Some(extra) = map.next_key::<String>()? {
            return Err(de::Error::unknown_field(&extra, &["workspace"]));
        }
        Ok(Maybe::FromWorkspace)
    }
}
