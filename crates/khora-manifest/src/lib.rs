//! The `khora.toml` project manifest.
//!
//! One file configures the whole toolchain: package identity, the OS
//! capabilities the package may ask for, formatter and lint settings,
//! dependencies, the sandboxed WASM build plugin, and the task DAG. The format
//! is specified in `docs/project.md` §4.1.
//!
//! Reading a manifest is deliberately hard to fail. A key this toolchain does
//! not recognize produces a [`Warning`] and the parse continues, because the
//! format will keep growing and a manifest written against a newer toolchain
//! has to stay buildable by an older one -- the alternative is a lockstep
//! upgrade across every consumer of a package. Only TOML that does not parse,
//! or a known key holding the wrong kind of value, is fatal.
//!
//! ```
//! use khora_manifest::{LintLevel, Manifest};
//!
//! let parsed = Manifest::parse(
//!     r#"
//!     [package]
//!     name = "risk_analyzer"
//!     version = "0.1.0"
//!
//!     [lints]
//!     unused-capabilities = "deny"
//!     "#,
//! )
//! .expect("a well-formed manifest");
//!
//! assert!(parsed.warnings.is_empty());
//! assert_eq!(parsed.manifest.lints["unused-capabilities"].level, LintLevel::Deny);
//! ```
//!
//! Positions survive parsing, so a driver can report against the source:
//!
//! ```
//! use khora_manifest::Manifest;
//!
//! let error = Manifest::parse("[package]\nname = 12\n").expect_err("name is not a string");
//! assert_eq!(error.location().map(|at| (at.line, at.column)), Some((2, 8)));
//! assert!(error.with_file("khora.toml").to_string().starts_with("khora.toml:2:8: "));
//! ```

mod audit;
mod error;
mod model;
mod warning;

pub use crate::error::{Location, ManifestError};
pub use crate::model::{
    granted_host, granted_name, granted_path, Build, Category, Default_, Dependency, Fmt,
    FsGrants, IndentStyle, Lint, LintLevel, Manifest, Package, Permissions, Task,
};
pub use crate::warning::{Warning, WarningKind};

/// The result of a successful parse.
///
/// Warnings ride alongside the manifest rather than being logged from inside the
/// parser: the same parse feeds the CLI, the language server and the tests, and
/// each renders diagnostics its own way.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// The manifest.
    pub manifest: Manifest,
    /// Everything non-fatal noticed on the way. Empty for a clean manifest.
    pub warnings: Vec<Warning>,
}

impl Manifest {
    /// Parses manifest text.
    ///
    /// Takes text rather than a path so that the language server can parse an
    /// unsaved buffer; attach the file name to a failure with
    /// [`ManifestError::with_file`].
    pub fn parse(text: &str) -> Result<Parsed, ManifestError> {
        let manifest: Manifest =
            toml::from_str(text).map_err(|error| ManifestError::from_toml(error, text))?;
        // Second read: the typed one above cannot report what it ignored.
        let warnings = audit::unknown_keys(text)?;
        Ok(Parsed { manifest, warnings })
    }
}
