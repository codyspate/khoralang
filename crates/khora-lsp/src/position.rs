//! Byte offsets to LSP positions, and back.
//!
//! **The protocol counts UTF-16 code units, and Khora counts bytes.** Every
//! `TextRange` in the compiler is a byte range into UTF-8 source; every
//! `Position` on the wire is a zero-based line plus a character offset that,
//! by default, means UTF-16 code units into that line. Those three units — byte,
//! character, code unit — agree exactly as long as the file is ASCII, which is
//! why getting this wrong looks like it works.
//!
//! They stop agreeing at the first accented letter, and the symptom is a
//! diagnostic underlining the wrong span for the rest of the line. `é` is two
//! bytes and one code unit; an emoji is four bytes and *two* code units,
//! because it is a surrogate pair.
//!
//! `Encoding` is negotiated: a client may say it can count UTF-8, in which case
//! there is nothing to convert. Both paths are here because the conversion is
//! the one that is easy to get wrong, and having it beside its trivial
//! counterpart makes the difference visible.

use lsp_types::{Position, Range};
use text_size::{TextRange, TextSize};

/// How the client counts a character offset within a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// The default the protocol mandates when nothing is negotiated.
    #[default]
    Utf16,
    /// Offered since 3.17, and free for us: it is what the source already is.
    Utf8,
}

/// A document's line boundaries, computed once per edit.
///
/// Kept rather than recomputed per position because a file with three hundred
/// diagnostics would otherwise scan the whole text three hundred times.
#[derive(Debug, Clone, Default)]
pub struct LineIndex {
    /// Byte offset of the start of each line. Always begins with 0.
    starts: Vec<u32>,
    text: String,
}

impl LineIndex {
    /// Indexes where every line of `text` starts.
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0u32];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset as u32 + 1);
            }
        }
        LineIndex { starts, text: text.to_string() }
    }

    /// The source this was built from.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The LSP position of a byte offset.
    ///
    /// An offset past the end of the text answers the very end, because a range
    /// that runs off the end is a diagnostic about the end of the file and
    /// clamping is friendlier than refusing.
    pub fn position(&self, offset: TextSize, encoding: Encoding) -> Position {
        let offset = u32::from(offset).min(self.text.len() as u32) as usize;
        let line = match self.starts.binary_search(&(offset as u32)) {
            Ok(exact) => exact,
            Err(after) => after - 1,
        };
        let start = self.starts[line] as usize;
        let prefix = &self.text[start..offset];
        let character = match encoding {
            Encoding::Utf8 => prefix.len(),
            Encoding::Utf16 => prefix.chars().map(char::len_utf16).sum(),
        };
        Position { line: line as u32, character: character as u32 }
    }

    /// A byte range as the protocol's line-and-column pair.
    pub fn range(&self, range: TextRange, encoding: Encoding) -> Range {
        Range {
            start: self.position(range.start(), encoding),
            end: self.position(range.end(), encoding),
        }
    }

    /// The byte offset of an LSP position.
    ///
    /// Out-of-range input is clamped rather than rejected: a client can send a
    /// position from a document state one edit ahead of ours, and answering
    /// about the nearest real place beats an error nobody sees.
    pub fn offset(&self, position: Position, encoding: Encoding) -> TextSize {
        let line = (position.line as usize).min(self.starts.len().saturating_sub(1));
        let start = self.starts[line] as usize;
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.text.len(), |next| (*next as usize).saturating_sub(1));
        let text = &self.text[start..end.max(start)];

        let wanted = position.character as usize;
        let mut counted = 0usize;
        for (offset, ch) in text.char_indices() {
            if counted >= wanted {
                return TextSize::from((start + offset) as u32);
            }
            counted += match encoding {
                Encoding::Utf8 => ch.len_utf8(),
                Encoding::Utf16 => ch.len_utf16(),
            };
        }
        TextSize::from((start + text.len()) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_agrees_in_every_encoding() {
        let index = LineIndex::new("abc\ndef\n");
        for encoding in [Encoding::Utf8, Encoding::Utf16] {
            let at = index.position(TextSize::from(5), encoding);
            assert_eq!((at.line, at.character), (1, 1), "{encoding:?}");
        }
    }

    /// Two bytes, one UTF-16 code unit. The two encodings disagree from here
    /// on, and this is the case a test suite of ASCII would never notice.
    #[test]
    fn an_accented_letter_is_two_bytes_and_one_code_unit() {
        let index = LineIndex::new("é x");
        assert_eq!(index.position(TextSize::from(3), Encoding::Utf16).character, 2);
        assert_eq!(index.position(TextSize::from(3), Encoding::Utf8).character, 3);
    }

    /// Four bytes and *two* code units: outside the basic plane, UTF-16 uses a
    /// surrogate pair, so the count is not "characters" either.
    #[test]
    fn an_emoji_is_four_bytes_and_two_code_units() {
        let index = LineIndex::new("\u{1F600}x");
        assert_eq!(index.position(TextSize::from(4), Encoding::Utf16).character, 2);
        assert_eq!(index.position(TextSize::from(4), Encoding::Utf8).character, 4);
    }

    #[test]
    fn a_position_round_trips_through_an_offset() {
        let text = "module a;\nfn é() -> Int { \u{1F600} }\nlet x = 1;\n";
        let index = LineIndex::new(text);
        for encoding in [Encoding::Utf8, Encoding::Utf16] {
            for offset in 0..text.len() {
                if !text.is_char_boundary(offset) {
                    continue;
                }
                let size = TextSize::from(offset as u32);
                let back = index.offset(index.position(size, encoding), encoding);
                assert_eq!(back, size, "{encoding:?} at {offset}");
            }
        }
    }

    #[test]
    fn an_offset_past_the_end_clamps() {
        let index = LineIndex::new("ab\n");
        let at = index.position(TextSize::from(9_999), Encoding::Utf16);
        assert_eq!((at.line, at.character), (1, 0));
    }

    #[test]
    fn a_position_past_the_end_clamps() {
        let index = LineIndex::new("ab\ncd\n");
        let offset = index.offset(Position { line: 99, character: 99 }, Encoding::Utf16);
        assert!(u32::from(offset) as usize <= index.text().len());
    }
}
