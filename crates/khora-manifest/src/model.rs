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
use std::ops::Deref;

use crate::error::ManifestError;
use crate::inherit::{Maybe, Resolved};

/// A parsed `khora.toml`, with every inherited field already filled in.
///
/// **Nothing here remembers whether a value was written or inherited**, which
/// is the point: a `version` is a `String`, so no reader has to cope with one
/// that has not arrived. See [`crate::inherit`].
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// Package identity, absent in a workspace root that is only a workspace.
    ///
    /// **Optional because a monorepo's root is not a package.** This repository
    /// holds `std`, four examples, four benchmarks and a package, and none of
    /// them is at the top; a root forced to declare a `[package]` would be
    /// inventing a name for a thing that does not exist, and then that name
    /// would appear in error messages. Cargo calls the shape a virtual
    /// manifest and it is the right one.
    ///
    /// Exactly one rule holds it together, checked in `Manifest::parse`: a
    /// manifest must have `[package]` or `[workspace]` or both, and a file with
    /// neither is not a manifest at all.
    pub package: Option<Package>,
    /// The members this manifest is the root of.
    pub workspace: Option<Workspace>,
    /// OS capabilities the package is allowed to ask for.
    pub permissions: Permissions,
    /// Formatter settings, when the manifest configures the formatter.
    pub fmt: Option<Fmt>,
    /// Lint configuration, keyed by lint name.
    pub lints: Lints,
    /// Dependencies, keyed by module path such as `std.effect`.
    pub dependencies: BTreeMap<String, Dependency>,
    /// Which compiler this project expects.
    pub toolchain: Option<Toolchain>,
    /// Build settings, when the manifest configures the build.
    pub build: Option<Build>,
    /// Task-runner entries, keyed by task name.
    pub tasks: BTreeMap<String, Task>,
}

/// A `khora.toml` as written, before the workspace root has been consulted.
///
/// The only difference from [`Manifest`] is that the inheritable fields may
/// still say `workspace = true`. It exists so that [`Manifest`] cannot.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct RawManifest {
    pub(crate) package: Option<RawPackage>,
    pub(crate) workspace: Option<Workspace>,
    #[serde(default)]
    pub(crate) permissions: Permissions,
    pub(crate) fmt: Option<Fmt>,
    #[serde(default)]
    pub(crate) lints: Lints,
    #[serde(default)]
    pub(crate) dependencies: BTreeMap<String, Dependency>,
    #[serde(default)]
    pub(crate) toolchain: Option<Toolchain>,
    pub(crate) build: Option<Build>,
    #[serde(default)]
    pub(crate) tasks: BTreeMap<String, Task>,
}

/// A `[package]` table as written.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct RawPackage {
    // Not inheritable, and deliberately: a name is the one thing that makes a
    // member a distinct package, and a workspace whose members all inherited
    // one would have several packages with the same name.
    pub(crate) name: String,
    // Required, and not an `Option`, so that a `[package]` with no version at
    // all is serde's "missing field `version`" reported at the table's own
    // span. `version.workspace = true` is a version that is *present* and
    // written elsewhere, which is a different thing from an absent one.
    pub(crate) version: Maybe<String>,
    pub(crate) authors: Option<Maybe<Vec<String>>>,
    pub(crate) publish: Option<Maybe<bool>>,
    pub(crate) edition: Option<Maybe<String>>,
}

impl RawManifest {
    /// Whether anything here says `workspace = true`.
    ///
    /// The question [`crate::Manifest::parse_at`] asks before going to the
    /// filesystem for a root, so that a manifest inheriting nothing never
    /// pays for the search.
    pub(crate) fn inherits_anything(&self) -> bool {
        let package = self.package.as_ref().is_some_and(|raw| {
            let fields = [
                matches!(raw.version, Maybe::FromWorkspace),
                matches!(raw.authors, Some(Maybe::FromWorkspace)),
                matches!(raw.publish, Some(Maybe::FromWorkspace)),
                matches!(raw.edition, Some(Maybe::FromWorkspace)),
            ];
            fields.into_iter().any(|asked| asked)
        });
        package
            || self.permissions.workspace
            || self.lints.workspace
            || self.fmt.as_ref().is_some_and(|fmt| fmt.workspace)
    }

    /// Fills in every inherited field from `root`, or explains why it cannot.
    ///
    /// `root` is the `[workspace]` table of the manifest above this one, and
    /// `None` covers both "there is no workspace above" and "this was parsed
    /// from text with no path to look above". A manifest that inherits nothing
    /// does not care which.
    pub(crate) fn resolve(self, root: Option<&Workspace>) -> Result<Manifest, ManifestError> {
        let shared = root.and_then(|table| table.package.as_ref());

        let package = match self.package {
            None => None,
            Some(raw) => {
                let version =
                    Maybe::resolve(Some(raw.version), shared.and_then(|s| s.version.clone()));
                let authors = Maybe::resolve(raw.authors, shared.and_then(|s| s.authors.clone()));
                let publish = Maybe::resolve(raw.publish, shared.and_then(|s| s.publish));
                let edition = Maybe::resolve(raw.edition, shared.and_then(|s| s.edition.clone()));
                for (field, missing) in [
                    ("version", version.is_missing()),
                    ("authors", authors.is_missing()),
                    ("publish", publish.is_missing()),
                    ("edition", edition.is_missing()),
                ] {
                    if missing {
                        return Err(inheritance_error(
                            root.is_some(),
                            &format!("package.{field}"),
                            &format!("`{field}` under `[workspace.package]`"),
                        ));
                    }
                }
                let (Resolved::Own(version) | Resolved::Inherited(version)) = version else {
                    // Unreachable: `Absent` needs an `Option` field and this
                    // one is not, and `Missing` was just returned above.
                    return Err(inheritance_error(
                        root.is_some(),
                        "package.version",
                        "`version` under `[workspace.package]`",
                    ));
                };
                Some(Package {
                    name: raw.name,
                    version,
                    authors: authors.into_option().unwrap_or_default(),
                    publish: publish.into_option(),
                    edition: edition.into_option(),
                })
            }
        };

        let permissions = if self.permissions.workspace {
            if !self.permissions.is_only_the_flag() {
                return Err(ManifestError::invalid_value(
                    "permissions",
                    "`workspace = true` takes the whole table from the root, so the grants \
                     beside it would be silently dropped"
                        .to_string(),
                ));
            }
            match root.and_then(|table| table.permissions.clone()) {
                Some(inherited) => inherited,
                None => {
                    return Err(inheritance_error(
                        root.is_some(),
                        "permissions",
                        "a `[workspace.permissions]` table",
                    ))
                }
            }
        } else {
            self.permissions
        };

        let fmt = match self.fmt {
            Some(own) if own.workspace => {
                if !own.is_only_the_flag() {
                    return Err(ManifestError::invalid_value(
                        "fmt",
                        "`workspace = true` takes the whole table from the root, so the \
                         settings beside it would be silently dropped"
                            .to_string(),
                    ));
                }
                match root.and_then(|table| table.fmt.clone()) {
                    Some(inherited) => Some(inherited),
                    None => {
                        return Err(inheritance_error(
                            root.is_some(),
                            "fmt",
                            "a `[workspace.fmt]` table",
                        ))
                    }
                }
            }
            other => other,
        };

        let lints = if self.lints.workspace {
            if !self.lints.entries.is_empty() {
                return Err(ManifestError::invalid_value(
                    "lints",
                    "`workspace = true` takes the whole table from the root, so the lints \
                     beside it would be silently dropped"
                        .to_string(),
                ));
            }
            match root.map(|table| table.lints.clone()) {
                Some(inherited) => inherited,
                None => {
                    return Err(inheritance_error(
                        root.is_some(),
                        "lints",
                        "a `[workspace.lints]` table",
                    ))
                }
            }
        } else {
            self.lints
        };

        Ok(Manifest {
            package,
            workspace: self.workspace,
            permissions,
            fmt,
            lints,
            dependencies: self.dependencies,
            toolchain: self.toolchain,
            build: self.build,
            tasks: self.tasks,
        })
    }
}

/// Why `workspace = true` could not be honoured.
///
/// Two different mistakes, and telling them apart is most of the value: a
/// missing `[workspace.package]` entry is a one-line fix in a file the reader
/// can open, and no workspace at all means the member is not where they think
/// it is.
fn inheritance_error(had_root: bool, key: &str, add: &str) -> ManifestError {
    let why = if had_root {
        format!(
            "says `workspace = true`, and the workspace root does not set it. Add {add} to \
             the root manifest, or write the value here"
        )
    } else {
        "says `workspace = true`, and there is no workspace root above this manifest \
         to take it from"
            .to_string()
    };
    ManifestError::invalid_value(key, why)
}

impl Manifest {
    /// The package this manifest declares.
    ///
    /// `None` for a workspace root, which is a real manifest describing no
    /// package. Every caller that needs one has to say what it wanted it *for*,
    /// which is why this returns an option rather than erroring here: "a
    /// workspace root is not a package" means nothing without the sentence
    /// about what was being attempted.
    pub fn package(&self) -> Option<&Package> {
        self.package.as_ref()
    }

    /// Whether this manifest is the root of a workspace.
    pub fn is_workspace_root(&self) -> bool {
        self.workspace.is_some()
    }
}

/// The `[workspace]` table.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Workspace {
    /// Directories holding members, relative to this manifest.
    ///
    /// A trailing `*` matches every directory one level down, which is how
    /// `packages/*` and `examples/*` are written. Deliberately not a full glob
    /// language: `**` and character classes are a syntax to document and to get
    /// subtly wrong, and nothing has wanted one. A member that does not fit the
    /// pattern is listed by name, which is clearer anyway.
    #[serde(default)]
    pub members: Vec<String>,
    /// Members matched by `members` that should not be treated as such.
    ///
    /// For the directory inside `examples/*` that is a fixture rather than a
    /// package. Without it the only way to exclude one is to stop using a
    /// pattern and list the rest by hand.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// `[workspace.package]`: the field values members may ask for.
    pub package: Option<WorkspacePackage>,
    /// `[workspace.permissions]`, for a member writing `workspace = true`.
    pub permissions: Option<Permissions>,
    /// `[workspace.fmt]`, for a member writing `workspace = true`.
    pub fmt: Option<Fmt>,
    /// `[workspace.lints]`, for a member writing `workspace = true`.
    #[serde(default)]
    pub lints: Lints,
}

/// The `[workspace.package]` table.
///
/// Every field is optional, because a root supplies the ones its members
/// actually share. A member asking for one the root does not set is an error
/// naming both halves rather than a silent default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WorkspacePackage {
    /// The version members take with `version.workspace = true`.
    #[serde(default)]
    pub version: Option<String>,
    /// The authors members take with `authors.workspace = true`.
    #[serde(default)]
    pub authors: Option<Vec<String>>,
    /// What members take with `publish.workspace = true`.
    #[serde(default)]
    pub publish: Option<bool>,
    /// The edition members take with `edition.workspace = true`.
    #[serde(default)]
    pub edition: Option<String>,
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
    /// Whether this package is offered for other people to depend on.
    ///
    /// **Absent means no**, and that is the opposite of Cargo's default for a
    /// reason. Publishing to crates.io is an act somebody performs, so opting
    /// *out* is the right shape there. Here a package is fetched from a git
    /// URL, so publishing is *passive* — push a repository and it is already
    /// installable. An active choice should be the one that is written down.
    ///
    /// It also matters for a repository that is not only a package. This one
    /// holds `std`, three examples, four benchmarks and `packages/postgres`,
    /// and exactly one of those is a library. Default-true would advertise the
    /// lot.
    ///
    /// **It is an intent marker and not a permission**, which is worth being
    /// plain about: anybody can set it, and anybody can write a `[dependencies]`
    /// entry by hand whatever it says. What it prevents is depending on
    /// somebody's application, or their half-finished experiment, by accident.
    /// A `path` dependency ignores it — that is your own working copy.
    #[serde(default)]
    pub publish: Option<bool>,
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
    /// `workspace = true`: take the whole table from `[workspace.permissions]`.
    ///
    /// Whole table, not field by field. A half-inherited permission set is a
    /// set nobody can read off either file, and the question a reader has is
    /// "what may this package do" — which wants one answer in one place.
    #[serde(default)]
    pub workspace: bool,
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
    /// Paths the package may read.
    #[serde(default)]
    pub read: Vec<String>,
    /// Paths the package may write.
    #[serde(default)]
    pub write: Vec<String>,
}

impl Permissions {
    /// Whether nothing but `workspace = true` was written.
    ///
    /// A grant beside the flag would be silently dropped by inheritance, which
    /// is worth an error rather than a surprise.
    pub(crate) fn is_only_the_flag(&self) -> bool {
        matches!(self.default, Default_::Allow)
            && self.network.is_none()
            && self.fs.is_none()
            && self.env.is_none()
            && self.extern_.is_none()
    }

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
    /// The file system.
    Fs,
    /// Outbound and inbound sockets.
    Network,
    /// The process environment.
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
    /// `workspace = true`: take the whole table from `[workspace.fmt]`.
    #[serde(default)]
    pub workspace: bool,
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

impl Fmt {
    /// Whether nothing but `workspace = true` was written.
    pub(crate) fn is_only_the_flag(&self) -> bool {
        self.indent_style.is_none()
            && self.indent_width.is_none()
            && self.explicit_semicolons.is_none()
    }
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

/// The `[lints]` table.
///
/// A map of lint name to configuration, plus the one key that is not a lint:
/// `workspace = true`, which takes the root's table whole.
///
/// **`workspace` is therefore not available as a lint name.** Cargo has the
/// same collision and resolves it the same way. A lint called `workspace`
/// would be a lint about workspaces, and it can be called something else.
///
/// Derefs to the map, so `manifest.lints["unused-capabilities"]` and iterating
/// read exactly as they did before the key existed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lints {
    /// `workspace = true`: take the whole table from `[workspace.lints]`.
    pub workspace: bool,
    /// The lints themselves, keyed by name.
    pub entries: BTreeMap<String, Lint>,
}

impl Deref for Lints {
    type Target = BTreeMap<String, Lint>;

    fn deref(&self) -> &BTreeMap<String, Lint> {
        &self.entries
    }
}

impl<'a> IntoIterator for &'a Lints {
    type Item = (&'a String, &'a Lint);
    type IntoIter = std::collections::btree_map::Iter<'a, String, Lint>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl FromIterator<(String, Lint)> for Lints {
    fn from_iter<I: IntoIterator<Item = (String, Lint)>>(entries: I) -> Lints {
        Lints { workspace: false, entries: entries.into_iter().collect() }
    }
}

impl<'de> Deserialize<'de> for Lints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Lints, D::Error> {
        deserializer.deserialize_map(LintsVisitor)
    }
}

struct LintsVisitor;

impl<'de> Visitor<'de> for LintsVisitor {
    type Value = Lints;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a table of lint names, optionally with `workspace = true`")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Lints, A::Error> {
        let mut out = Lints::default();
        while let Some(key) = map.next_key::<String>()? {
            if key == "workspace" {
                out.workspace = map.next_value::<bool>()?;
            } else {
                out.entries.insert(key, map.next_value::<Lint>()?);
            }
        }
        Ok(out)
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

/// The `[toolchain]` table: which Khora builds this project.
///
/// ```toml
/// [toolchain]
/// version = "0.1.0"
/// ```
///
/// **In this file rather than one of its own.** Rust and Node both keep the
/// toolchain apart -- `rust-toolchain.toml`, `.nvmrc` -- on the argument that a
/// compiler version is not a property of the package. It is a good argument and
/// it loses to a simpler one: a project with two files describing how to build
/// it has two files that must both be found and both be committed, and only one
/// that anybody remembers. One file that says everything is easier to keep true.
///
/// A version named here and not installed is an error, never a fallback. See
/// `khora-toolchain`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Toolchain {
    /// An exact version. No ranges, and no channels.
    ///
    /// A range would need a resolver and would reintroduce the thing a pin
    /// exists to remove: two machines agreeing on the constraint and
    /// disagreeing on the compiler.
    #[serde(default)]
    pub version: Option<String>,
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
    /// Where in the repository the package is, for a git dependency.
    ///
    /// **A git URL names a repository and not a package**, and the two are the
    /// same thing only in the simplest layout. `packages/postgres` lives inside
    /// a repository that is mostly a compiler, and without this there is no way
    /// to say so — the resolver looked for `khora.toml` at the root, found the
    /// wrong one or none, and the package was simply unreachable.
    #[serde(default, rename = "subdir")]
    pub subdir: Option<String>,
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
