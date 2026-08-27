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

#![deny(missing_docs)]

use std::path::Path;

mod audit;
mod error;
pub(crate) mod inherit;
mod model;
mod semver;
mod warning;
mod workspace;

pub use crate::error::{Location, ManifestError};
pub use crate::model::{
    Build, Category, Default_, Dependency, Fmt, FsGrants, IndentStyle, Lint, LintLevel, Lints, Manifest, Package, Permissions, Task, Toolchain, WorkspacePackage, granted_host, granted_name, granted_path,
};
pub use crate::semver::Version;

/// A canonical path without Windows' `\\?\` prefix.
///
/// `canonicalize` returns a *verbatim* path, which is correct and which no
/// person wants to read in a diagnostic: `\\?\C:\Users\...\khora.lock` is the
/// same file as `C:\Users\...\khora.lock` and looks like a mistake. The prefix
/// only turns off path normalisation these paths do not need, so the stripped
/// form is safe to keep using and not only to print.
///
/// Here rather than in a utilities crate because there is no utilities crate,
/// and this is the lowest thing every caller already depends on. A no-op
/// everywhere but Windows, which is why it is not behind a `cfg`.
pub fn readable(path: std::path::PathBuf) -> std::path::PathBuf {
    let Some(text) = path.to_str() else { return path };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return std::path::PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => std::path::PathBuf::from(rest),
        None => path,
    }
}
pub use crate::warning::{Warning, WarningKind};
pub use crate::workspace::{enclosing, read as read_workspace, Workspace as WorkspaceLayout};

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
    /// Reads and parses the manifest at `path`, inheriting from its workspace.
    ///
    /// **The entry point for everything that reads a manifest off disk.**
    /// `workspace = true` needs a root to resolve against, and a root is found
    /// by walking up from the file — which [`Manifest::parse`] cannot do,
    /// because it has only text.
    pub fn load(path: &Path) -> Result<Parsed, ManifestError> {
        let text = std::fs::read_to_string(path)
            .map_err(|why| ManifestError::io(&format!("reading {}", path.display()), &why))?;
        Manifest::parse_at(&text, path).map_err(|error| error.with_file(path))
    }

    /// Parses manifest text that belongs at `path`.
    ///
    /// For an editor holding a buffer that has not been saved: the buffer is
    /// unsaved, but the workspace root it inherits from is not, so the text
    /// comes from the caller and the root comes from the disk.
    pub fn parse_at(text: &str, path: &Path) -> Result<Parsed, ManifestError> {
        let raw: crate::model::RawManifest =
            toml::from_str(text).map_err(|error| ManifestError::from_toml(error, text))?;

        // **The root is only looked for when something asks to inherit.**
        // Finding one expands the member list, which is a `read_dir` per
        // pattern; almost every manifest writes all its own fields and should
        // not pay for a question it did not ask.
        let root = if raw.inherits_anything() {
            // Above *this* manifest's directory, not at it. A root resolving
            // its own `[package]` against its own `[workspace.package]` is not
            // wrong exactly, but nothing has wanted it and allowing it means
            // deciding what a cycle means.
            let here = path.parent().unwrap_or(Path::new("."));
            let found = here.parent().and_then(crate::workspace::enclosing_root);
            if let Some(root) = &found {
                // Being *under* a root is not being *in* it. A directory the
                // root does not list is not a member, and taking a version
                // from a workspace you are not part of is the kind of thing
                // that is only noticed once it is published.
                if !root.lists(here) {
                    return Err(ManifestError::invalid_value(
                        "workspace",
                        format!(
                            "this manifest inherits from a workspace, and the root at {} does \
                             not list it as a member. Add it to `members` there, or write the \
                             values here",
                            root.directory.join("khora.toml").display()
                        ),
                    ));
                }
            }
            found
        } else {
            None
        };

        Manifest::finish(raw, root.as_ref().map(|root| &root.table), text)
    }

    /// Parses manifest text.
    ///
    /// Takes text rather than a path so that the language server can parse an
    /// unsaved buffer; attach the file name to a failure with
    /// [`ManifestError::with_file`].
    ///
    /// **`workspace = true` is an error here**, naming the reason: with no
    /// path there is no root to take the value from. [`Manifest::load`] is the
    /// one to use for a manifest that exists as a file.
    pub fn parse(text: &str) -> Result<Parsed, ManifestError> {
        let raw: crate::model::RawManifest =
            toml::from_str(text).map_err(|error| ManifestError::from_toml(error, text))?;
        Manifest::finish(raw, None, text)
    }

    /// Resolves inheritance and runs the checks both entry points share.
    fn finish(
        raw: crate::model::RawManifest,
        root: Option<&crate::model::Workspace>,
        text: &str,
    ) -> Result<Parsed, ManifestError> {
        let manifest = raw.resolve(root)?;
        // A file with neither table is not a manifest. Said here rather than
        // by serde, because serde's own message for a missing `[package]`
        // would now be wrong half the time -- a workspace root is allowed to
        // have none.
        if manifest.package.is_none() && manifest.workspace.is_none() {
            return Err(ManifestError::invalid_value(
                "package",
                "a manifest needs a `[package]` table, or a `[workspace]` one if it is the \
                 root of a monorepo rather than a package itself"
                    .to_string(),
            ));
        }

        // `version` is the field `docs/design/compatibility.md` is written
        // entirely in terms of -- what a major may break, what a minor may add
        // -- and none of that means anything against a string nobody parsed.
        // `"1.2"`, `"v1.2.3"` and `"latest"` all used to be accepted, and the
        // first place any of them would have been noticed is a resolver
        // comparing two and giving an answer nobody could explain. Roadmap
        // 10.1.
        if let Some(package) = &manifest.package {
            crate::semver::Version::parse(&package.version)
                .map_err(|why| ManifestError::invalid_value("package.version", why))?;
        }

        // Second read: the typed one above cannot report what it ignored.
        let warnings = audit::unknown_keys(text)?;
        Ok(Parsed { manifest, warnings })
    }
}
