//! Non-fatal observations made while reading a manifest.

use crate::error::Location;
use std::fmt;
use std::ops::Range;

/// Why a [`Warning`] was raised.
///
/// Non-exhaustive: later toolchain versions will diagnose more without that
/// being a breaking change for anyone matching on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WarningKind {
    /// A key that no table in this toolchain's schema declares.
    UnknownKey,
}

impl fmt::Display for WarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WarningKind::UnknownKey => f.write_str("unrecognised key"),
        }
    }
}

/// Something worth telling the user about that did not stop the parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    kind: WarningKind,
    key: String,
    span: Option<Range<usize>>,
    location: Option<Location>,
}

impl Warning {
    pub(crate) fn unknown_key(key: String, span: Option<Range<usize>>, text: &str) -> Warning {
        Warning {
            kind: WarningKind::UnknownKey,
            key,
            location: span.as_ref().map(|span| Location::from_offset(text, span.start)),
            span,
        }
    }

    /// What kind of observation this is.
    pub fn kind(&self) -> WarningKind {
        self.kind
    }

    /// The dotted path of the key involved, such as `tasks.ci.retries`.
    ///
    /// Segments containing a `.` of their own are quoted, so a dependency named
    /// `std.effect` reads as `dependencies."std.effect".version` and cannot be
    /// mistaken for three nested tables.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Where the key is, when the position survived parsing.
    pub fn location(&self) -> Option<Location> {
        self.location
    }

    /// The byte range the key covers, when the position survived parsing.
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.location {
            Some(at) => write!(f, "{}: {} `{}`", at, self.kind, self.key),
            None => write!(f, "{} `{}`", self.kind, self.key),
        }
    }
}
