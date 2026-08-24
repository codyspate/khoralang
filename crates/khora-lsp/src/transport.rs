//! Base protocol framing: `Content-Length` headers over a byte stream.
//!
//! Deliberately over `Read`/`Write` rather than over stdio, so the server can
//! be driven by a test with two buffers. An editor is a bad test harness.

use std::io::{BufRead, Write};

use anyhow::{bail, Context, Result};

/// Reads one message, or `None` at a clean end of stream.
///
/// Headers are terminated by a blank line; everything but `Content-Length` is
/// ignored, including the `Content-Type` some clients send. An unknown header
/// is not an error — the base protocol says so, and refusing one would break
/// against a client that is doing nothing wrong.
pub fn read_message(input: &mut impl BufRead) -> Result<Option<String>> {
    let mut length: Option<usize> = None;

    loop {
        let mut line = String::new();
        if input.read_line(&mut line).context("reading a header")? == 0 {
            // End of stream between messages is how an editor says goodbye
            // when it did not get to send `exit`.
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = Some(value.trim().parse().context("a Content-Length that is a number")?);
        }
    }

    let Some(length) = length else {
        bail!("a message with no Content-Length, which the base protocol requires")
    };

    let mut body = vec![0u8; length];
    std::io::Read::read_exact(input, &mut body).context("reading a message body")?;
    String::from_utf8(body).context("a message body that is UTF-8").map(Some)
}

/// Writes one message with its header.
///
/// The length is in **bytes**, not characters, which is the same mistake as the
/// one `position` is about and is worth being explicit rather than lucky.
pub fn write_message(output: &mut impl Write, body: &str) -> Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n{}", body.len(), body)
        .context("writing a message")?;
    output.flush().context("flushing a message")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(body: &str) -> String {
        let mut wire = Vec::new();
        write_message(&mut wire, body).expect("writing");
        read_message(&mut wire.as_slice()).expect("reading").expect("a message")
    }

    #[test]
    fn a_message_survives_the_wire() {
        assert_eq!(round_trip(r#"{"jsonrpc":"2.0"}"#), r#"{"jsonrpc":"2.0"}"#);
    }

    /// The length is in bytes. A message whose content is not ASCII is where a
    /// character count would truncate the body and desynchronise the stream for
    /// every message after it.
    #[test]
    fn a_non_ascii_message_survives_the_wire() {
        let body = r#"{"message":"`é` is not a `\u{1F600}`"}"#;
        assert_eq!(round_trip(body), body);
    }

    #[test]
    fn other_headers_are_ignored() {
        let wire = "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\
                    Content-Length: 2\r\n\r\n{}";
        assert_eq!(read_message(&mut wire.as_bytes()).expect("reading"), Some("{}".to_string()));
    }

    #[test]
    fn the_end_of_the_stream_is_not_an_error() {
        assert_eq!(read_message(&mut "".as_bytes()).expect("reading"), None);
    }

    #[test]
    fn a_message_without_a_length_is_refused() {
        assert!(read_message(&mut "X: 1\r\n\r\n{}".as_bytes()).is_err());
    }

    #[test]
    fn two_messages_come_out_in_order() {
        let mut wire = Vec::new();
        write_message(&mut wire, "{\"a\":1}").expect("writing");
        write_message(&mut wire, "{\"b\":2}").expect("writing");
        let mut input = wire.as_slice();
        assert_eq!(read_message(&mut input).unwrap().as_deref(), Some("{\"a\":1}"));
        assert_eq!(read_message(&mut input).unwrap().as_deref(), Some("{\"b\":2}"));
        assert_eq!(read_message(&mut input).unwrap(), None);
    }
}
