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
    /// A key this toolchain used to have and deliberately does not any more.
    ///
    /// Separate from [`WarningKind::UnknownKey`] because the two want opposite
    /// reactions. An unknown key may be from a *newer* toolchain and the right
    /// move is to leave it alone; a removed one is from an older one, is never
    /// coming back, and should be deleted. Telling somebody "unrecognized key"
    /// about a line the documentation told them to write last month is the
    /// sort of thing that makes a warning stop being read.
    RemovedKey,
}

impl fmt::Display for WarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WarningKind::UnknownKey => f.write_str("unrecognized key"),
            WarningKind::RemovedKey => f.write_str("removed key"),
        }
    }
}

/// Something worth telling the user about that did not stop the parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    kind: WarningKind,
    key: String,
    /// What to do instead, for a key that was removed rather than never known.
    note: Option<&'static str>,
    /// The schema key this one is probably a misspelling of.
    ///
    /// **Because an ignored key and an absent one are the same thing**, and for
    /// `[permissions]` they are the same thing in the worst direction: a
    /// manifest with no `[permissions]` table grants everything, so
    /// `[permission.fs]` -- one letter short -- reads exactly like a program
    /// that was never sandboxed. The author believes they wrote a sandbox. The
    /// warning said "unrecognized key" and `check` exited 0.
    ///
    /// Taken from the sibling names in the schema at the point the key sits, so
    /// it is a real key at a real place rather than a guess from a global list.
    suggestion: Option<&'static str>,
    span: Option<Range<usize>>,
    location: Option<Location>,
}

impl Warning {
    pub(crate) fn unknown_key(
        key: String,
        suggestion: Option<&'static str>,
        span: Option<Range<usize>>,
        text: &str,
    ) -> Warning {
        Warning {
            kind: WarningKind::UnknownKey,
            key,
            note: None,
            suggestion,
            location: span.as_ref().map(|span| Location::from_offset(text, span.start)),
            span,
        }
    }

    /// The nearest sibling name, when this key looks like a misspelling of one.
    pub fn suggestion(&self) -> Option<&'static str> {
        self.suggestion
    }

    pub(crate) fn removed_key(
        key: String,
        note: &'static str,
        span: Option<Range<usize>>,
        text: &str,
    ) -> Warning {
        Warning {
            kind: WarningKind::RemovedKey,
            key,
            note: Some(note),
            suggestion: None,
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

    /// What to do about it, when there is something specific to say.
    ///
    /// Only a removed key has one: for an unknown key this toolchain has, by
    /// definition, nothing to add.
    pub fn note(&self) -> Option<&'static str> {
        self.note
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
            Some(at) => write!(f, "{}: {} `{}`", at, self.kind, self.key)?,
            None => write!(f, "{} `{}`", self.kind, self.key)?,
        }
        if let Some(note) = self.note {
            return write!(f, ": {note}");
        }
        match self.suggestion {
            // **`permissions` gets a sentence the others do not**, because it
            // is the one key where being ignored is not the same as doing
            // nothing. Every other misspelled table leaves a setting at its
            // default; this one leaves the program unsandboxed, which is the
            // opposite of what its author was writing when they typed it.
            Some("permissions") => write!(
                f,
                " -- did you mean `permissions`? Nothing is reading it, and a \
                 manifest with no `[permissions]` table grants everything, so \
                 this program is running unsandboxed"
            ),
            Some(near) => write!(f, " -- did you mean `{near}`?"),
            None => Ok(()),
        }
    }
}
