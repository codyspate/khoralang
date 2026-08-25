//! Type checking and inference.
//!
//! Bodies are inferred by unification ([`unify`]) against declared signatures.
//! Signatures stay explicit at function boundaries — that is the decision in
//! `docs/design/associated-items.md` and it is what keeps errors local — but
//! everything inside a body is solved.
//!
//! Row unification for effects arrives in phase 4; the shape [`Type`] needs for
//! it is noted where it will go.

pub mod derive;
pub mod foreign;
pub mod mono;
pub mod traits;
pub mod unify;
pub mod usefulness;

use std::collections::{HashMap, HashSet};

use khora_db::{Db, SourceFile};
use khora_hir::body::{BinOp, Body, Expr, ExprId, Literal, LocalId, Pat, PatId, Stmt, UnOp};
use khora_hir::HirError;
use khora_syntax::ast::{self, AstNode};
use text_size::TextRange;
use unify::{Mismatch, Unifier};
use usefulness::{ColumnType, Ctor, FieldType, Pattern};

/// A fixed-width integer type: `U8`, `I32`, and the rest.
///
/// **`Int` is not one of these.** `Int` is the 64-bit signed integer, and
/// `I64` is a second spelling of it rather than a distinct type — two
/// different 64-bit signed integers would mean a conversion between them that
/// can never fail and never does anything, which is a tax with no payer.
///
/// Everything else is here because `Int` alone cannot describe a byte, and a
/// byte is what a wire format, a file and a string are made of.
/// `docs/design/numbers.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IntKind {
    pub signed: bool,
    /// 8, 16, 32 or 64. A `signed` 64 never appears: that is `Type::Int`.
    pub bits: u8,
}

impl IntKind {
    /// The type as written in source, which is also how it prints.
    pub fn name(self) -> String {
        format!("{}{}", if self.signed { 'I' } else { 'U' }, self.bits)
    }

    /// Reads one back from its name, or `None` if that is not one.
    ///
    /// `I64` is deliberately absent — it is `Type::Int`, resolved before this
    /// is reached.
    pub fn parse(name: &str) -> Option<Self> {
        let (signed, rest) = match name.split_at_checked(1)? {
            ("U", rest) => (false, rest),
            ("I", rest) => (true, rest),
            _ => return None,
        };
        let bits = match rest {
            "8" => 8,
            "16" => 16,
            "32" => 32,
            "64" => 64,
            _ => return None,
        };
        (!(signed && bits == 64)).then_some(IntKind { signed, bits })
    }

    /// The largest value the type can hold, and the smallest.
    ///
    /// Both fit in an `i128` because the widest type here is 64 bits, which is
    /// exactly why the range check can be a comparison rather than an argument.
    pub fn range(self) -> (i128, i128) {
        if self.signed {
            let half = 1i128 << (self.bits - 1);
            (-half, half - 1)
        } else {
            (0, (1i128 << self.bits) - 1)
        }
    }

    /// Whether every value of `self` is also a value of `wider` — that is,
    /// whether the conversion is the one that cannot fail.
    pub fn fits_in(self, wider: IntKind) -> bool {
        let (lo, hi) = self.range();
        let (wide_lo, wide_hi) = wider.range();
        wide_lo <= lo && hi <= wide_hi
    }
}

/// Every fixed-width integer, in the order they are declared in `std::core`.
pub const INT_KINDS: [IntKind; 7] = [
    IntKind { signed: false, bits: 8 },
    IntKind { signed: false, bits: 16 },
    IntKind { signed: false, bits: 32 },
    IntKind { signed: false, bits: 64 },
    IntKind { signed: true, bits: 8 },
    IntKind { signed: true, bits: 16 },
    IntKind { signed: true, bits: 32 },
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// The 64-bit signed integer, spelled `Int` or `I64`.
    Int,
    /// One of the other integers: `U8`, `U16`, `U32`, `U64`, `I8`, `I16`,
    /// `I32`. See [`IntKind`] for why `I64` is not among them.
    Fixed(IntKind),
    /// IEEE-754 double precision.
    ///
    /// Note what does *not* follow: `Float` implements neither `Eq` nor `Ord`.
    /// `==` and `<` on floats are primitive and mean what IEEE says they mean,
    /// which is what every reader expects — and exactly why the *traits* are
    /// withheld, since `NaN == NaN` is false and a law-abiding `Eq` cannot say
    /// so. `docs/design/numbers.md`.
    Float,
    Bool,
    Str,
    Unit,
    /// An opaque machine address: a `void *`.
    ///
    /// **Not counted, not dereferenceable from Khora, and never a pointer into
    /// Khora's own heap.** It exists so that a foreign function can hand back
    /// a handle — a `FILE *`, an `SSL_CTX *` — and be given it again later.
    /// Nothing in the language produces one from a Khora value, which is what
    /// makes it impossible to create a dangling one: the only pointers that
    /// exist came from the other side, and their lifetimes are that side's
    /// business.
    ///
    /// Lending a *buffer* — `Array<U8>`'s bytes — is the harder question and is
    /// deliberately not answered by this type. `docs/design/ffi.md`.
    Ptr,
    /// A user-declared variant type, with its type arguments.
    ///
    /// **`home` is what makes this an identity rather than a spelling.** Two
    /// modules may each declare a `Point`, and without the module they were one
    /// type: the importer looked its fields up by name, found its own
    /// declaration, and was told that `Point` has no field `label` about a
    /// value that has exactly that field. An alias had the mirror problem —
    /// `import other::{Point as Other}` keyed the import under `Other`, so a
    /// rename invented a type. Errata 46.
    ///
    /// `None` for a name that did not resolve to a declaration, which is a type
    /// error already reported. It deliberately does *not* mean "any module":
    /// two unresolved names are equal to each other and to nothing else, so a
    /// failure here cannot quietly satisfy a comparison.
    ///
    /// [`std::fmt::Display`] prints `name` alone. A reader wants `Point`, and
    /// the two places where that is genuinely ambiguous — a message naming two
    /// types that print alike — ask for [`Type::qualified`] instead.
    Adt { name: String, home: Option<khora_hir::ModulePath>, args: Vec<Type> },
    /// A function *value*'s type, effects included.
    ///
    /// The rows are why a function that needs capabilities can be passed as a
    /// value at all: without them, mentioning `analyze` would have to charge
    /// its requirements to whatever function wrote the name, rather than to
    /// whoever eventually calls it. `List::map(analyze)` working is the single
    /// largest ergonomic difference from a monadic design — see
    /// `docs/design/effects.md`.
    Fn { params: Vec<Type>, ret: Box<Type>, requires: Box<Type>, raises: Box<Type> },
    /// A hole inference is free to fill.
    Var(unify::TypeVar),
    /// A type parameter the *caller* chose. Rigid: the body of a generic
    /// function cannot decide what it is. See `unify`.
    Param(String),
    /// A type applied to arguments where the head is not yet a constructor:
    /// `Self<A>` in a higher-kinded trait, or `F<B>` at a call site before `F`
    /// is known.
    ///
    /// The head is a [`Type::Param`] when rigid and a [`Type::Var`] when the
    /// caller still gets to choose it. Solving that variable against a concrete
    /// `Option<Int>` is what decides `F := Option` and `B := Int`, and the
    /// application collapses into an ordinary [`Type::Adt`] as soon as it does —
    /// so nothing downstream of instance selection ever sees one.
    Applied { head: Box<Type>, args: Vec<Type> },
    /// A set of labelled requirements: `{ ledger: Ledger | 'e }`.
    ///
    /// Serves both effect clauses. A capability row labels each field with the
    /// name the caller supplies it under; an error row labels each with the
    /// error's own type name, since two errors of one type cannot be told
    /// apart and by-name is how they are handled.
    ///
    /// `tail` is what else may be present: a variable for an open row, `None`
    /// for a closed one. `{}` is the closed empty row, which is what an
    /// entry point must reduce to.
    Row {
        /// Sorted by label, so two rows written in different orders are one
        /// type without unification having to sort them.
        fields: Vec<(String, Type)>,
        tail: Option<Box<Type>>,
    },
    /// A fixed-length product, as in `(Int, Bool)`.
    ///
    /// The empty tuple is `Unit`, not `Tuple(vec![])`, so there is exactly one
    /// spelling of "no information".
    Tuple(Vec<Type>),
    /// An associated type projected off another type: `Self::Item`.
    ///
    /// Normalizes to whatever the owner's impl bound the name to as soon as the
    /// owner is known — `Range::Item` becomes `Int` given
    /// `impl Iterator for Range { type Item = Int; }`. Until then it stands for
    /// itself, and unifies only with the same projection.
    Assoc { owner: Box<Type>, name: String },
    /// A type-level integer, as in `Matrix<3, 4>`.
    ///
    /// Unifies only with an equal value, which is what turns a shape mismatch
    /// into a compile error instead of a runtime assertion.
    Const(i64),
    /// The type of `return`, `break` and a diverging branch. Compatible with
    /// everything, because control never reaches the consumer.
    Never,
    /// Stands in for an expression whose type could not be determined —
    /// usually downstream of an error already reported. Compatible with
    /// everything, so one mistake does not cascade.
    Unknown,
    // Phase 4 adds Row(..).
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Int => write!(f, "Int"),
            Type::Ptr => write!(f, "Ptr"),
            Type::Fixed(kind) => write!(f, "{}", kind.name()),
            Type::Float => write!(f, "Float"),
            Type::Bool => write!(f, "Bool"),
            Type::Str => write!(f, "String"),
            Type::Unit => write!(f, "()"),
            Type::Adt { name, args, .. } if args.is_empty() => write!(f, "{name}"),
            Type::Adt { name, args, .. } => {
                let inner: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                write!(f, "{name}<{}>", inner.join(", "))
            }
            Type::Param(name) => write!(f, "{name}"),
            // `_` rather than an internal number: this is what a Rust or
            // TypeScript developer already reads as "not pinned down yet".
            Type::Var(_) => write!(f, "_"),
            Type::Const(n) => write!(f, "{n}"),
            Type::Assoc { owner, name } => write!(f, "{owner}::{name}"),
            Type::Row { fields, tail } => {
                let mut parts: Vec<String> =
                    fields.iter().map(|(label, ty)| format!("{label}: {ty}")).collect();
                if let Some(tail) = tail {
                    parts.push(format!("| {tail}"));
                }
                if parts.is_empty() {
                    write!(f, "{{}}")
                } else {
                    write!(f, "{{ {} }}", parts.join(", "))
                }
            }
            Type::Applied { head, args } => {
                let inner: Vec<String> = args.iter().map(Type::to_string).collect();
                write!(f, "{head}<{}>", inner.join(", "))
            }
            // `(Int,)` for the one-element case, so it is not read as a
            // parenthesised `Int` - the same disambiguation Rust and Python use.
            Type::Tuple(items) => {
                let inner: Vec<String> = items.iter().map(Type::to_string).collect();
                let trailing = if items.len() == 1 { "," } else { "" };
                write!(f, "({}{trailing})", inner.join(", "))
            }
            Type::Never => write!(f, "Never"),
            Type::Unknown => write!(f, "?"),
            Type::Fn { params, ret, requires, raises } => {
                let ps: Vec<String> = params.iter().map(|p| p.to_string()).collect();
                write!(f, "({}) -> {ret}", ps.join(", "))?;
                // An empty row is the ordinary case and says nothing, so it is
                // not printed: `(Int) -> Int` should not read as a type with
                // two invisible clauses.
                if !is_empty_row(requires) {
                    write!(f, " with {requires}")?;
                }
                if !is_empty_row(raises) {
                    write!(f, " raises {raises}")?;
                }
                Ok(())
            }
        }
    }
}

/// Whether a row asks for nothing at all — no labels and no tail.
fn is_empty_row(ty: &Type) -> bool {
    matches!(ty, Type::Row { fields, tail } if fields.is_empty() && tail.is_none())
}

impl Type {
    /// The closed empty row: requires nothing, raises nothing.
    pub fn empty_row() -> Type {
        Type::Row { fields: Vec::new(), tail: None }
    }

    /// A row from labelled entries, canonically ordered.
    pub fn row(mut fields: Vec<(String, Type)>, tail: Option<Type>) -> Type {
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        fields.dedup_by(|a, b| a.0 == b.0);
        Type::Row { fields, tail: tail.map(Box::new) }
    }

    /// A nullary ADT whose declaring module is not known here.
    ///
    /// Two callers, and both are legitimate. The backend works on names
    /// monomorphization has already made unique, so there is nothing left for a
    /// module to disambiguate. And a handful of types are the compiler's own —
    /// [`FAILED`] is produced by `assert` and caught by a test, and no source
    /// declares it.
    ///
    /// Anything reading a name a *program* wrote wants [`Type::adt_in`], or the
    /// two `Point`s of errata 46 become one type again.
    pub fn adt(name: impl Into<String>) -> Type {
        Type::Adt { name: name.into(), home: None, args: Vec::new() }
    }

    /// An ADT with the module that declares it.
    pub fn adt_in(name: impl Into<String>, home: Option<khora_hir::ModulePath>) -> Type {
        Type::Adt { name: name.into(), home, args: Vec::new() }
    }

    /// The name with its module, for a message where the short name would be
    /// ambiguous: `expected `Point`, found `Point`` helps nobody.
    ///
    /// Only [`Type::Adt`] has a module to add; everything else prints as it
    /// always does.
    pub fn qualified(&self) -> String {
        match self {
            Type::Adt { name, home: Some(home), args } if args.is_empty() => {
                format!("{home}::{name}")
            }
            Type::Adt { name, home: Some(home), args } => {
                let inner: Vec<String> = args.iter().map(|a| a.qualified()).collect();
                format!("{home}::{name}<{}>", inner.join(", "))
            }
            other => other.to_string(),
        }
    }

    /// A function that needs nothing and cannot fail.
    ///
    /// Most functions in most programs, and every one the backend can build a
    /// value of today — so this is the constructor, and the effectful form is
    /// spelled out where it is meant.
    pub fn func(params: Vec<Type>, ret: Type) -> Type {
        Type::Fn {
            params,
            ret: Box::new(ret),
            requires: Box::new(Type::empty_row()),
            raises: Box::new(Type::empty_row()),
        }
    }
}

/// A function's declared signature.
///
/// `generics` names the rigid parameters. Inside the body they stay rigid; at
/// a call site they are instantiated to fresh variables, which is what lets two
/// calls to the same generic function have unrelated types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// Whether this names a C symbol rather than a Khora body.
    ///
    /// Read by the code generator, which is the only thing that can tell the
    /// difference: to the type checker a declaration is a declaration, and a
    /// signature written ahead of its implementation is a perfectly good one to
    /// check against. `docs/design/ffi.md`.
    pub is_extern: bool,
    pub generics: Vec<String>,
    /// What the caller must supply: the `with { .. }` row. The closed empty
    /// row when the clause is absent, so "requires nothing" is the default and
    /// an entry point needs no annotation to be checked.
    pub requires: Type,
    /// How the call can fail: the `raises ..` row, empty when absent.
    pub raises: Type,
    /// The traits each generic parameter requires, in the order declared.
    ///
    /// Parallel to `generics` rather than a map, so the parameter a bound
    /// belongs to is positional — which is how instantiation already matches
    /// arguments to parameters.
    pub bounds: Vec<Vec<String>>,
    pub params: Vec<Type>,
    pub ret: Type,
}

impl Signature {
    /// The signature as a function type, with its parameters still rigid.
    pub fn as_fn(&self) -> Type {
        Type::Fn {
            params: self.params.clone(),
            ret: Box::new(self.ret.clone()),
            requires: Box::new(self.requires.clone()),
            raises: Box::new(self.raises.clone()),
        }
    }
}

/// A variant of an ADT and the types of its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantInfo {
    pub type_name: String,
    /// The module that declares `type_name`.
    ///
    /// Without it, a file that declares a `Point` and imports another module's
    /// `Point` has one key for two declarations, and whichever arrived first
    /// answers for both — which is how the importer was told its own type had
    /// no field `label`. Errata 46.
    ///
    /// `None` for a declaration whose module is not known, which is only the
    /// compiler's own.
    pub home: Option<khora_hir::ModulePath>,
    pub name: String,
    pub fields: Vec<Type>,
    /// What each field is called, where it has a name.
    ///
    /// Matching is positional and never needed these; `p.x` does, and so does
    /// a record literal, which names its fields and nothing else.
    pub labels: Vec<String>,
    /// Which fields were declared `mut`, positionally.
    ///
    /// Empty for a variant declared before this mattered, which reads as "none
    /// of them" — the safe answer, and the one every constructor wants.
    pub mutable: Vec<bool>,
}

impl VariantInfo {
    /// The position and type of a named field.
    pub fn field(&self, label: &str) -> Option<(usize, &Type)> {
        let index = self.labels.iter().position(|l| l == label)?;
        self.fields.get(index).map(|ty| (index, ty))
    }

    /// Whether a named field may be written after the record is built.
    pub fn is_mut(&self, label: &str) -> bool {
        self.labels
            .iter()
            .position(|l| l == label)
            .and_then(|i| self.mutable.get(i))
            .copied()
            .unwrap_or(false)
    }

    /// Whether any field may be written. What makes a value unshareable, and
    /// what ends the DAG invariant — see `docs/design/memory.md`.
    pub fn has_mutable_field(&self) -> bool {
        self.mutable.iter().any(|m| *m)
    }
}

/// Signatures and ADT shapes for one file.
///
/// Read from the syntax tree rather than from `ItemMap`, which records what
/// exists but not what shape it has. Keeping that in one place here avoids
/// growing a HIR type layer before generics force its shape.
/// The marker trait that answers for a type the compiler cannot see inside.
///
/// `docs/design/sharing.md`.
pub const SHARE: &str = "Share";

/// What each type name written in one file actually refers to.
///
/// A name is a spelling and a type is a declaration, and this is the map
/// between them. Two things follow that did not work before it existed
/// (errata 46): a file's own `Point` is distinct from one it imports, and an
/// alias resolves to the *declared* name, so `import other::{Point as Other}`
/// gives `Other` and `other::Point` one identity instead of two.
///
/// A file's own declaration wins over an import of the same name, which is
/// what shadowing means everywhere else in the language.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypeHomes {
    /// Local spelling to the module that declares it and the name it is
    /// declared under.
    by_name: HashMap<String, (khora_hir::ModulePath, String)>,
}

impl TypeHomes {
    /// The declaration `local` names here, as `(module, declared name)`.
    ///
    /// `None` for a name nothing declares — a type parameter has already been
    /// handled by the time this is asked, so what is left is a name that did
    /// not resolve, and that is an error somebody else reports.
    pub fn of(&self, local: &str) -> Option<(khora_hir::ModulePath, String)> {
        self.by_name.get(local).cloned()
    }

    /// Records a declaration this file makes itself.
    pub fn declares(&mut self, name: &str, home: &khora_hir::ModulePath) {
        self.by_name.insert(name.to_string(), (home.clone(), name.to_string()));
    }
}

/// Where every type name one file can write comes from.
#[salsa::tracked(returns(ref))]
pub fn type_homes(db: &dyn Db, file: SourceFile) -> TypeHomes {
    let mut homes = TypeHomes::default();
    let items = khora_hir::item_map(db, file);

    // Declarations first, so an import cannot displace them.
    if let Some(home) = &items.module {
        for item in &items.items {
            if is_a_type(item.kind) {
                homes.declares(&item.name, home);
            }
        }
    }

    for origin in &khora_hir::file_scope(db, file).origins {
        if is_a_type(origin.kind) {
            homes
                .by_name
                .entry(origin.local.clone())
                .or_insert_with(|| (origin.module.clone(), origin.name.clone()));
        }
    }
    homes
}

/// Whether an item is something a type name can refer to.
///
/// An `effect` is one: it is a record of function types, and a `with` clause
/// names it exactly as a type annotation names a type.
fn is_a_type(kind: khora_hir::ItemKind) -> bool {
    matches!(
        kind,
        khora_hir::ItemKind::Type | khora_hir::ItemKind::Effect | khora_hir::ItemKind::Trait
    )
}

/// The type whose `spawn` starts a fiber.
///
/// Named here rather than in the backend because the *checker* enforces what
/// may cross into one, and a rule about sharing is a type error rather than a
/// code-generation one.
pub const FIBER_TYPE: &str = "Fiber";

/// The certified-closure wrapper. `SharedFn::of` is where the check happens.
pub const SHARED_FN_TYPE: &str = "SharedFn";

/// Every declaration the compiler treats specially, by the name it goes by.
///
/// **These are matched by name, and a name is not an identity.** A type is a
/// [`Type::Adt`] carrying a bare `String`, so `Array` declared in a user's
/// module and `Array` declared in `std::core` are one type to everything
/// downstream — which meant a user's `Array` was given the runtime's array
/// layout and dropping one read a garbage element width and aborted the
/// process. Not "no privilege": memory corruption.
///
/// [`collides_with_a_builtin`] refuses the collision, which closes that hole
/// without pretending the underlying problem is solved. It is not. Two modules
/// that each declare a `Point` still collide, and an alias still splits one
/// type into two — see D13's neighbours in `docs/roadmap.md` and errata 46.
/// The real fix is for a type to carry the declaration it resolved to, and it
/// changes how every type in the program is keyed.
///
/// `Int`, `Float`, `Bool`, `String` and `Ptr` are here for the same reason
/// even though they are not ADTs: [`named_type`] answers with the builtin
/// before it ever consults what the file declared, so a user's `type String`
/// silently never existed.
///
/// `Share` is a trait rather than a type, so no *definition* of it can exist
/// to conflict — a marker trait is empty and `std::core`'s declaration and a
/// user's are the same three characters. It is listed because it is
/// compiler-known, not because the check below can act on it.
pub const COMPILER_KNOWN: [&str; 12] = [
    SHARE,
    FIBER_TYPE,
    SHARED_FN_TYPE,
    "Fibers",
    "Shared",
    "Array",
    "Int",
    "I64",
    "Float",
    "Bool",
    "String",
    "Ptr",
];

/// Whether declaring `name` with a definition would collide with the compiler.
///
/// **Only a definition collides.** `export type Array<A>;` with no right-hand
/// side is how the builtin is *named* — it is what `std::core` writes, and what
/// every backend test writes to reach an array without importing the standard
/// library. Nothing is claimed by it that the compiler does not already
/// provide, so nothing conflicts.
///
/// `type Array = { label: String }` is the other thing entirely: a shape the
/// compiler will ignore in favour of the runtime's, on a name it will hand an
/// array's layout. That is the case worth refusing, and it is the only one.
///
/// So the rule needs no exemption for `std` and no notion of a blessed module,
/// which is just as well — there is nothing finer than a module path to check
/// against until a package has an identity of its own (roadmap 10.2).
pub fn collides_with_a_builtin(name: &str) -> bool {
    COMPILER_KNOWN.contains(&name)
}

/// The error a failed assertion is.
///
/// Not a type a program can declare or name: `assert` is the only thing that
/// produces one and a test is the only thing that catches one, so there is
/// nothing for a `catch` arm to say about it.
pub const FAILED: &str = "Failed";

/// The label an error type carries in a `raises` row: its own name.
///
/// One definition, in `unify`, because substitution has to relabel with it.
use unify::row_label as label_of;

/// Whether `declared` and `written` name the same set of fields.
fn covers(declared: &[String], written: &[&str]) -> bool {
    declared.len() == written.len() && declared.iter().all(|d| written.iter().any(|w| w == d))
}

// One module per responsibility. This was 4,543 lines, of which one `impl
// Checker` was 2,350 — a new reader looking for where a call's arguments are
// checked had no way to find it but to read the file. Roadmap 9.6.3.
mod check;
mod exports;
mod reporting;
mod map;
mod queries;
mod syntax;
mod unresolved;

pub use foreign::{can_raise, foreign_obstacle, foreign_signature_obstacle};

pub use reporting::*;
pub use map::*;
pub use queries::*;
pub use syntax::*;
