//! Fatal parse failures, and the positions they point at.

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// A 1-based line/column position in the manifest text.
///
/// Kept alongside the byte `offset` it was derived from: humans read
/// line/column, and editors and the language server want byte ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    /// 1-based line number.
    pub line: usize,
    /// 1-based column, counted in `char`s.
    pub column: usize,
    /// Byte offset into the manifest text.
    pub offset: usize,
}

impl Location {
    /// Resolves a byte offset within `text`.
    ///
    /// Walks rather than slices so that an offset landing mid-codepoint, or past
    /// the end of the text, yields the nearest sane position instead of a panic;
    /// `toml` reports end-of-input errors at `text.len()`.
    ///
    /// Columns count `char`s because that is what an editor's caret shows for
    /// the mostly-ASCII text a manifest is. Callers speaking LSP will need to
    /// re-count in UTF-16 from `offset`.
    pub fn from_offset(text: &str, offset: usize) -> Location {
        let (mut line, mut column) = (1, 1);
        for (index, ch) in text.char_indices() {
            if index >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        Location { line, column, offset }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// A manifest that could not be read at all.
///
/// Distinct from [`crate::Warning`] on purpose: anything recoverable is a
/// warning, so every `ManifestError` is a genuine stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    message: String,
    span: Option<Range<usize>>,
    location: Option<Location>,
    file: Option<PathBuf>,
}

impl ManifestError {
    /// Lifts a `toml` failure, resolving its span against the text it came from.
    pub(crate) fn from_toml(error: toml::de::Error, text: &str) -> ManifestError {
        let span = error.span();
        ManifestError {
            message: error.message().to_string(),
            location: span.as_ref().map(|span| Location::from_offset(text, span.start)),
            span,
            file: None,
        }
    }

    /// A file that could not be read, which is not a parse failure but reaches
    /// the caller through the same channel.
    pub(crate) fn io(doing: &str, why: &std::io::Error) -> ManifestError {
        ManifestError { message: format!("{doing}: {why}"), span: None, location: None, file: None }
    }

    /// A failure the schema saw rather than the parser: a well-formed TOML
    /// value that is not a legal one.
    ///
    /// `key` is the dotted path, so the message names the field. There is no
    /// span, because by this point the typed value has been parsed out of the
    /// document and the position is gone -- and a message naming
    /// `package.version` is enough to find it.
    pub(crate) fn invalid_value(key: &str, why: String) -> ManifestError {
        ManifestError { message: format!("`{key}`: {why}"), span: None, location: None, file: None }
    }

    /// Records which file the text came from.
    ///
    /// [`crate::Manifest::parse`] is handed text, not a path, so that it stays
    /// usable on unsaved editor buffers. Whoever opened the file supplies the
    /// name, and [`ManifestError`]'s `Display` then renders the full
    /// `file:line:column: message` a build log wants.
    #[must_use]
    pub fn with_file(mut self, file: impl Into<PathBuf>) -> ManifestError {
        self.file = Some(file.into());
        self
    }

    /// The failure, without any position prefix.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Where the failure is, if `toml` could point at it.
    pub fn location(&self) -> Option<Location> {
        self.location
    }

    /// The byte range the failure covers, if `toml` could point at it.
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }

    /// The file set by [`ManifestError::with_file`].
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.file, self.location) {
            (Some(file), Some(at)) => write!(f, "{}:{}: {}", file.display(), at, self.message),
            (Some(file), None) => write!(f, "{}: {}", file.display(), self.message),
            (None, Some(at)) => write!(f, "{}: {}", at, self.message),
            (None, None) => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ManifestError {}
