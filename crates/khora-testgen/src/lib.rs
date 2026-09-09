//! Turns a seed of bytes into Khora source text.
//!
//! Three generators, in increasing order of how much structure they have:
//!
//! * [`soup::token_soup`] — real tokens in an arbitrary order. Nothing parses;
//!   the invariant is that the parser returns.
//! * [`soup::interpolated_string`] — a string literal, or something that was
//!   trying to be one. The lexer's interpolation scanner is the sharpest
//!   surface the front end has and this is aimed at it.
//! * [`program::program`] — a **syntactically valid** module. Everything it
//!   emits parses with no errors, which is what lets the formatter's
//!   properties be stated over it.
//!
//! All three are driven by [`entropy::Entropy`], a cursor over a byte slice,
//! so the same generator serves `proptest` (which shrinks the slice) and
//! `cargo fuzz` (which mutates it under coverage feedback). See that module
//! for the ordering rule that makes shrinking converge.
//!
//! # What `program` does not generate
//!
//! Documented on [`program::program`] itself, and it is a long list. The point
//! of the crate is that the list is written down and the generator is easy to
//! add to, not that the list is empty.

pub mod entropy;
pub mod program;
pub mod soup;

pub use entropy::Entropy;
pub use program::program;
