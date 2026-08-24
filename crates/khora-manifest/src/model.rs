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

/// What an unmentioned category grants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Default_ {
    /// Everything. A category nobody wrote down is not a category anybody is
    /// restricting, and a program that has never heard of permissions should
    /// compile.
    #[default]
    Allow,
    /// Nothing. The strict posture: one line, set once, and adding a capability
    /// becomes a deliberate edit.
    Deny,
}

/// The `[permissions]` table.
///
/// **The manifest decides what capabilities a program may hold; the capability
/// decides what may be done with it.** The first half is checked when the
/// program is compiled and is total; the second is checked where the access
/// happens, because a host read out of a config file cannot be checked any
/// earlier. `docs/design/permissions.md` is the argument.
///
/// A missing table grants everything, and each category is independent of the
/// others: naming `network` says nothing about `fs`. Tightening is opt-in, so
/// that the first step towards being careful is not also the most expensive
/// one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Permissions {
    /// What a category nobody mentioned grants.
    #[serde(default)]
    pub default: Default_,
    /// Hosts that may be reached, as `host:port`. `["*"]` is any.
    #[serde(default)]
    pub network: Option<Vec<String>>,
    /// Paths that may be read and written.
    #[serde(default)]
    pub fs: Option<FsGrants>,
    /// Environment variables that may be read.
    #[serde(default)]
    pub env: Option<Vec<String>>,
    /// Packages that may declare `extern fn`. `std` is always among them.
    ///
    /// **This is the key the rest of the table rests on.** Every other grant
    /// here is a rule about Khora code, and `extern fn` is the door out of
    /// Khora: a foreign declaration's effect row is a promise the compiler
    /// takes on trust, so a dependency that simply declines to make the promise
    /// reaches the operating system with nothing in its signature and nothing
    /// in yours. `docs/design/permissions.md` calls it "the hole this does not
    /// close yet"; it is a rule about *which package* a declaration is in, and
    /// there were no packages until 10.2.
    ///
    /// Absent grants every package, like the rest of the table -- tightening is
    /// opt-in, and a project that has not thought about this is not punished
    /// for it. `[]` is the interesting value: nothing but `std` may reach out.
    #[serde(default, rename = "extern")]
    pub extern_: Option<Vec<String>>,
}

/// `[permissions.fs]`: reading and writing are not the same grant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct FsGrants {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

impl Permissions {
    /// Whether the manifest grants this category at all.
    ///
    /// The compile-time half of the decision, and the only half the compiler
    /// can keep: whether a program may *hold* the capability. What it may do
    /// with it is the capability's own business.
    pub fn grants(&self, category: Category) -> bool {
        let listed = match category {
            Category::Network => self.network.as_ref().map(|g| !g.is_empty()),
            Category::Env => self.env.as_ref().map(|g| !g.is_empty()),
            Category::Fs => {
                self.fs.as_ref().map(|g| !g.read.is_empty() || !g.write.is_empty())
            }
        };
        match listed {
            Some(any) => any,
            None => matches!(self.default, Default_::Allow),
        }
    }

    /// Whether `package` may declare `extern fn`.
    ///
    /// `std` always may, and that is the whole design rather than an exception
    /// to it: the point of the allow-list is that everything reaching outside
    /// Khora goes through functions whose signatures carry capability rows, and
    /// the standard library is where those live. A `std` that could not declare
    /// `fopen` could not offer `Fs`.
    pub fn may_declare_extern(&self, package: &str) -> bool {
        if package == "std" {
            return true;
        }
        match &self.extern_ {
            Some(allowed) => allowed.iter().any(|a| a == package),
            None => true,
        }
    }
}

/// A kind of access to the outside world, as the manifest names it.
///
/// Short because the manifest names *kinds of access*, and there are not many.
/// A capability an application defines for itself is not here: it is a seam the
/// program chose to have, and nothing outside the program has an opinion about
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Fs,
    Network,
    Env,
}

impl Category {
    /// The capability type this category governs.
    pub fn capability(self) -> &'static str {
        match self {
            Category::Fs => "Fs",
            Category::Network => "Net",
            Category::Env => "Env",
        }
    }

    /// The manifest key, for a diagnostic that tells the reader what to add.
    pub fn key(self) -> &'static str {
        match self {
            Category::Fs => "fs",
            Category::Network => "network",
            Category::Env => "env",
        }
    }

    /// The category governing a capability type, if any does.
    pub fn of_capability(name: &str) -> Option<Category> {
        match name {
            "Fs" => Some(Category::Fs),
            "Net" => Some(Category::Network),
            "Env" => Some(Category::Env),
            _ => None,
        }
    }
}

/// Whether any of `grants` covers the environment variable `name`.
///
/// `*` matches any run of characters, because a variable name has no structure
/// to respect. `DB_*` is the shape almost every grant takes.
pub fn granted_name(grants: &[String], name: &str) -> bool {
    grants.iter().any(|g| glob(g, name, None))
}

/// Whether any of `grants` covers `path`.
///
/// `*` matches within one path segment and `**` across them, which is the glob
/// dialect everyone already has in their fingers from `.gitignore`. Separators
/// are normalized, so a grant written with `/` covers a Windows path.
pub fn granted_path(grants: &[String], path: &str) -> bool {
    let path = path.replace('\\', "/");
    grants.iter().any(|g| glob(&g.replace('\\', "/"), &path, Some('/')))
}

/// Whether any of `grants` covers `host`, which is `name` or `name:port`.
///
/// Two rules, and both are chosen to be the reading that costs a newcomer
/// least:
///
/// - **`*` in a host spans dots**, so `*.internal` covers `db.eu.internal` and
///   not only `db.internal`. This is what a Content-Security-Policy origin
///   means by it and what most people expect; the one-label reading belongs to
///   TLS certificates, and surprising somebody into a denied connection is a
///   worse failure than covering a subdomain they did not enumerate.
/// - **A grant with no port covers every port.** `api.example.com` grants the
///   host, which is the same thing Deno's `--allow-net=example.com` does.
pub fn granted_host(grants: &[String], host: &str) -> bool {
    let (name, port) = split_port(host);
    grants.iter().any(|g| {
        let (pattern, allowed) = split_port(g);
        let port_ok = match allowed {
            None | Some("*") => true,
            Some(p) => Some(p) == port,
        };
        port_ok && glob(pattern, name, None)
    })
}

/// Splits a trailing `:port` off, if there is one.
fn split_port(text: &str) -> (&str, Option<&str>) {
    match text.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (text, None),
    }
}

/// Matches `pattern` against `value`.
///
/// `**` matches any run of characters. `*` matches any run that does not cross
/// `boundary`, or any run at all when there is no boundary. Everything else is
/// literal.
///
/// Backtracking, and exponential on a pathological pattern — which a grant in a
/// manifest is not, being a handful of characters written by hand.
fn glob(pattern: &str, value: &str, boundary: Option<char>) -> bool {
    if let Some(rest) = pattern.strip_prefix("**") {
        return splits(value, value.len()).any(|at| glob(rest, &value[at..], boundary));
    }
    if let Some(rest) = pattern.strip_prefix('*') {
        let limit = boundary.and_then(|b| value.find(b)).unwrap_or(value.len());
        return splits(value, limit).any(|at| glob(rest, &value[at..], boundary));
    }
    match (pattern.chars().next(), value.chars().next()) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(p), Some(v)) => {
            p == v && glob(&pattern[p.len_utf8()..], &value[v.len_utf8()..], boundary)
        }
    }
}

/// Every character boundary in `value` up to and including `limit`.
fn splits(value: &str, limit: usize) -> impl Iterator<Item = usize> + '_ {
    (0..=limit).filter(|at| value.is_char_boundary(*at))
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
///
/// Exactly one of `version` and `path` says where the package comes from. A
/// `path` is resolved relative to the manifest, and needs no version because
/// the source is right there; a `version` is resolved through the registry,
/// which does not exist until phase 10.
///
/// **`std` is not among these.** The standard library is found beside the
/// compiler, the way `rustc` finds its sysroot, so a program that has never
/// written a manifest still has one. Declaring it would be a line every
/// package repeats and no package can get wrong.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Dependency {
    /// The requested version, for a package from the registry.
    #[serde(default)]
    pub version: Option<String>,
    /// Where the package is, relative to this manifest.
    #[serde(default)]
    pub path: Option<String>,
    /// A git repository to fetch the package from.
    ///
    /// **Added in phase 10.2, ahead of the registry**, because a `path`
    /// dependency does not exercise any of what a package manager is for:
    /// nothing is fetched, so nothing is hashed, pinned or cached, and a
    /// lockfile over paths alone records almost nothing. A git source is the
    /// smallest one that is really a source.
    #[serde(default)]
    pub git: Option<String>,
    /// The revision to take from `git` -- a commit id, a tag or a branch.
    ///
    /// Resolved to a full commit id before it reaches `khora.lock`, so a
    /// locked build cannot change under a moved tag.
    #[serde(default)]
    pub rev: Option<String>,
    /// A tag to take from `git`. Spelt separately from `rev` because that is
    /// how people think about it; they mean the same thing here.
    #[serde(default)]
    pub tag: Option<String>,
}

impl Dependency {
    /// Whether this says where the package comes from at all.
    ///
    /// A dependency with neither is the mistake worth naming: it parses, and
    /// then resolves to nothing.
    pub fn is_located(&self) -> bool {
        self.version.is_some() || self.path.is_some() || self.git.is_some()
    }
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
