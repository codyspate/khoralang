//! The shape of `khora.toml`, as described in `docs/project.md` §4.1.
//!
//! Every table other than `[package]` is optional. Tables whose absence is
//! indistinguishable from being empty (`[permissions]`, `[lints]`,
//! `[dependencies]`, `[tasks]`) default instead of being `Option`; `[fmt]` and
//! `[build]` stay `Option` because a driver has to know whether the manifest
//! said anything at all before falling back to a toolchain-wide default.

use serde::de::{self, MapAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::fmt;

/// A parsed `khora.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Manifest {
    /// Package identity.
    pub package: Package,
    /// OS capabilities the package is allowed to ask for.
    #[serde(default)]
    pub permissions: Permissions,
    /// Formatter settings, when the manifest configures the formatter.
    pub fmt: Option<Fmt>,
    /// Lint configuration, keyed by lint name.
    #[serde(default)]
    pub lints: BTreeMap<String, Lint>,
    /// Dependencies, keyed by module path such as `std.effect`.
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
    /// Build settings, when the manifest configures the build.
    pub build: Option<Build>,
    /// Task-runner entries, keyed by task name.
    #[serde(default)]
    pub tasks: BTreeMap<String, Task>,
}

/// The `[package]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Package {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Authors, in `Name <email>` form.
    #[serde(default)]
    pub authors: Vec<String>,
    /// Language edition, such as `2026`.
    ///
    /// Left as a string rather than an enum: editions are minted over time, and
    /// an unknown one has to reach the driver as a "this toolchain is too old"
    /// diagnostic rather than as a parse failure here.
    pub edition: Option<String>,
}

/// The `[permissions]` table: Deno-style capability limits.
///
/// The grants stay verbatim strings (`allow-read=/etc/config`). Their grammar is
/// the capability checker's business, and it needs the text the author wrote in
/// order to point at the offending grant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Permissions {
    /// Network grants, such as `allow-net=db.internal:5432`.
    #[serde(default)]
    pub network: Vec<String>,
    /// Filesystem grants, such as `allow-write=./tmp`.
    #[serde(default)]
    pub fs: Vec<String>,
    /// Environment variables the package may read.
    #[serde(default)]
    pub env: Vec<String>,
}

/// The `[fmt]` table.
///
/// Every setting is optional so that a manifest can override one knob without
/// having to restate the formatter's defaults, which live in the formatter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Fmt {
    /// Whether indentation is spaces or tabs.
    #[serde(rename = "indent-style")]
    pub indent_style: Option<IndentStyle>,
    /// Columns per indentation level.
    #[serde(rename = "indent-width")]
    pub indent_width: Option<u8>,
    /// Whether the formatter writes statement terminators explicitly.
    #[serde(rename = "explicit-semicolons")]
    pub explicit_semicolons: Option<bool>,
}

/// What `indent-style` may say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndentStyle {
    /// `"space"`.
    Space,
    /// `"tab"`.
    Tab,
}

impl IndentStyle {
    /// The spelling used in the manifest.
    pub fn as_str(self) -> &'static str {
        match self {
            IndentStyle::Space => "space",
            IndentStyle::Tab => "tab",
        }
    }

    /// Parses a manifest spelling.
    pub fn from_name(name: &str) -> Option<IndentStyle> {
        match name {
            "space" => Some(IndentStyle::Space),
            "tab" => Some(IndentStyle::Tab),
            _ => None,
        }
    }
}

// Hand-written rather than `rename_all`, so that `from_name` is the single place
// the accepted spellings are listed and the rejection message can name them.
impl<'de> Deserialize<'de> for IndentStyle {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<IndentStyle, D::Error> {
        let name = String::deserialize(deserializer)?;
        IndentStyle::from_name(&name)
            .ok_or_else(|| de::Error::invalid_value(Unexpected::Str(&name), &"`space` or `tab`"))
    }
}

/// How loud a lint is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LintLevel {
    /// Do not report.
    Allow,
    /// Report, keep compiling.
    Warn,
    /// Report, fail the build.
    Deny,
}

impl LintLevel {
    /// The spelling used in the manifest.
    pub fn as_str(self) -> &'static str {
        match self {
            LintLevel::Allow => "allow",
            LintLevel::Warn => "warn",
            LintLevel::Deny => "deny",
        }
    }

    /// Parses a manifest spelling.
    pub fn from_name(name: &str) -> Option<LintLevel> {
        match name {
            "allow" => Some(LintLevel::Allow),
            "warn" => Some(LintLevel::Warn),
            "deny" => Some(LintLevel::Deny),
            _ => None,
        }
    }
}

impl fmt::Display for LintLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LintLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<LintLevel, D::Error> {
        let name = String::deserialize(deserializer)?;
        LintLevel::from_name(&name).ok_or_else(|| {
            de::Error::invalid_value(Unexpected::Str(&name), &"`allow`, `warn` or `deny`")
        })
    }
}

/// One entry of the `[lints]` table.
///
/// Written either as a bare level (`unused-capabilities = "deny"`) or as a table
/// carrying the level plus knobs the lint itself defines
/// (`cyclomatic-complexity = { level = "warn", max = 15 }`). Both collapse to
/// this one type so callers never have to branch on which spelling was used.
#[derive(Debug, Clone, PartialEq)]
pub struct Lint {
    /// How loud the lint is.
    pub level: LintLevel,
    /// Everything in the table other than `level`.
    ///
    /// Deliberately untyped and unvalidated: the set of knobs belongs to
    /// whichever lint pass reads them, and this crate has no register of lints.
    /// That is also why the unknown-key audit leaves lint tables alone.
    pub options: BTreeMap<String, toml::Value>,
}

impl Lint {
    /// A lint set to `level` with no options, as the bare-string form produces.
    pub fn new(level: LintLevel) -> Lint {
        Lint { level, options: BTreeMap::new() }
    }

    /// Looks up one lint-defined option, such as `max`.
    pub fn option(&self, name: &str) -> Option<&toml::Value> {
        self.options.get(name)
    }
}

impl<'de> Deserialize<'de> for Lint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Lint, D::Error> {
        // `deserialize_any` rather than an untagged enum: TOML is self-describing,
        // and untagged would buffer the value first, which costs the spans that
        // make a bad `level` reportable at its own line.
        deserializer.deserialize_any(LintVisitor)
    }
}

struct LintVisitor;

impl<'de> Visitor<'de> for LintVisitor {
    type Value = Lint;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a lint level string, or a table with a `level` key")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Lint, E> {
        LintLevel::from_name(value).map(Lint::new).ok_or_else(|| {
            de::Error::invalid_value(Unexpected::Str(value), &"`allow`, `warn` or `deny`")
        })
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Lint, A::Error> {
        let mut level = None;
        let mut options = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if key == "level" {
                if level.is_some() {
                    return Err(de::Error::duplicate_field("level"));
                }
                level = Some(map.next_value::<LintLevel>()?);
            } else {
                options.insert(key, map.next_value::<toml::Value>()?);
            }
        }
        Ok(Lint { level: level.ok_or_else(|| de::Error::missing_field("level"))?, options })
    }
}

/// One entry of the `[dependencies]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Dependency {
    /// The requested version.
    pub version: String,
}

/// The `[build]` table.
///
/// Build steps run as sandboxed WASM plugins instead of arbitrary host code, so
/// `plugin` names a plugin and version rather than pointing at a script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Build {
    /// Target triple, such as `x86_64-unknown-linux-musl`.
    pub target: Option<String>,
    /// Build plugin, such as `protobuf-compiler@2.1`.
    pub plugin: Option<String>,
}

/// One entry of the `[tasks]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Task {
    /// One-line summary, shown when tasks are listed.
    pub description: Option<String>,
    /// Tasks that must finish first.
    ///
    /// Not resolved here. §4.1's own example depends on `lint`, `test` and
    /// `build`, none of which it declares, so a name may well refer to a
    /// built-in; only the runner knows the full set and can find a cycle.
    #[serde(default)]
    pub depends_on: Vec<String>,
}
