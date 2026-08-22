//! Type checking and inference.
//!
//! Bodies are inferred by unification ([`unify`]) against declared signatures.
//! Signatures stay explicit at function boundaries — that is the decision in
//! `docs/design/associated-items.md` and it is what keeps errors local — but
//! everything inside a body is solved.
//!
//! Row unification for effects arrives in phase 4; the shape [`Type`] needs for
//! it is noted where it will go.

pub mod mono;
pub mod traits;
pub mod unify;
pub mod usefulness;

use std::collections::{HashMap, HashSet};

use khora_db::{Db, SourceFile};
use khora_hir::body::{BinOp, Body, Expr, ExprId, Literal, LocalId, Pat, PatId, Stmt, UnOp};
use khora_hir::HirError;
use khora_syntax::ast::{self};
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
    Adt { name: String, args: Vec<Type> },
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
            Type::Adt { name, args } if args.is_empty() => write!(f, "{name}"),
            Type::Adt { name, args } => {
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

    /// A nullary ADT, which is what most of the phase 2 subset used.
    pub fn adt(name: impl Into<String>) -> Type {
        Type::Adt { name: name.into(), args: Vec::new() }
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypeMap {
    pub signatures: HashMap<String, Signature>,
    pub variants: Vec<VariantInfo>,
    /// Generic parameters of each declared type, by name.
    pub adts: HashMap<String, Vec<String>>,
    /// The traits and impls this file declares.
    pub traits: traits::Traits,
    /// The kind of every named type, so an impl can be checked against the
    /// kind its trait requires.
    pub kinds: HashMap<String, traits::Kind>,
    /// The names declared with `effect` rather than `type`.
    ///
    /// An effect is a record of function types, which would make every one of
    /// them unshareable — and so make a capability impossible to hand to a
    /// fiber, which is every concurrent server there is. It is shareable
    /// instead, and what pays for that is a check where each handler is
    /// *written*. `docs/design/sharing.md`.
    pub effects: HashSet<String>,
}

impl TypeMap {
    fn variants_of(&self, type_name: &str) -> Vec<&VariantInfo> {
        self.variants.iter().filter(|v| v.type_name == type_name).collect()
    }

    /// A constructor, found by the type it belongs to *and* its own name.
    ///
    /// Both halves are required. Case names are not unique across a program —
    /// two types may each have a `Some` — and `Resolution::Variant` carries the
    /// type for exactly this reason. Looking one up by its bare name resolves
    /// `Maybe::Some` to `Option::Some` whenever `Option` was declared first,
    /// which is a wrong tag rather than an error.
    pub fn variant_of(&self, type_name: &str, case: &str) -> Option<&VariantInfo> {
        self.variants.iter().find(|v| v.type_name == type_name && v.name == case)
    }

    /// Whether a value of this type may be handed to another fiber.
    ///
    /// False for anything that can be written, transitively: a record with a
    /// `mut` field, and anything holding one. Two fibers sharing a value they
    /// can both write is a data race, and refcount atomicity (D10) does not
    /// help — it protects the count, not the fields.
    ///
    /// **A function type is never shareable**, conservatively, because a
    /// closure's captures are not in its type and so nothing here can see what
    /// it holds. A named function referenced by path captures nothing and is
    /// not affected; only a closure kept in a binding is.
    ///
    /// **An effect is the exception, and it has to be.** An effect *is* a
    /// record of function types, so the rule above would make every capability
    /// unshareable — and a fiber could never be spawned from a function holding
    /// one, which is the shape of every concurrent server. What pays for the
    /// exception is [`Checker::check_handler_is_shareable`]: a handler's
    /// closures are written at the `handler for` literal, where the checker can
    /// see exactly what they captured, so the question is answered once, there,
    /// instead of at every spawn where it cannot be answered at all.
    ///
    /// `docs/design/memory.md` §5a and `docs/design/sharing.md`.
    /// **A type the caller chooses answers only if it was asked to.** A
    /// generic function cannot see what `A` will be, so `A` is shareable
    /// exactly when the signature wrote `A: Share` — otherwise
    /// `fn launder<A>(v: A) -> Fiber { Fiber::spawn(fn () => sink(v)) }`
    /// would hand a caller's mutable record to a fiber with nothing to say
    /// about it, which is what it did before `bounded` existed.
    ///
    /// `bounded` is the parameters of the enclosing signature that carry the
    /// bound, which the checker reads off `bounds_on`.
    /// Whether this compiler can see what `ty` holds.
    ///
    /// A declared type with no body cannot be looked into, which is the one
    /// place `impl Share` is allowed to speak. Everything else — a record, a
    /// variant, a tuple, a primitive — answers for itself.
    pub fn is_opaque(&self, ty: &Type) -> bool {
        match ty {
            Type::Applied { head, .. } => self.is_opaque(head),
            // A name with no variants recorded and no `effect` declaration.
            // Not `adts.contains_key`, deliberately: a type imported from
            // another module reaches this map through its impls but not its
            // declaration, and treating an absent name as *visible* would
            // refuse `impl Share for Fibers` in every file but the one that
            // declared it. A name that exists nowhere is reported as unknown
            // by resolution, which is the diagnostic that helps.
            Type::Adt { name, .. } => {
                !self.variants.iter().any(|v| &v.type_name == name)
                    && !self.effects.contains(name)
            }
            _ => false,
        }
    }

    pub fn is_shareable(&self, ty: &Type, bounded: &[String]) -> bool {
        self.shareable(ty, &mut Vec::new(), bounded)
    }

    /// Why a value of this type may not be handed to another fiber.
    ///
    /// Two different reasons wear the same refusal, and telling them apart is
    /// the difference between a fix and a hunt. A record with a `mut` field is
    /// a *race*, and the answer is to stop sharing it. A closure is refused
    /// because **what it captured is not in its type** — it may hold nothing at
    /// all — and the answer is a language question rather than a change to the
    /// program.
    ///
    /// The second is the one that matters: an effect *is* a record of function
    /// types, so no capability can cross into a fiber. The message says so
    /// rather than sending the reader to look for a `mut` that is not there.
    pub fn why_unshareable(&self, ty: &Type) -> String {
        if let Type::Param(name) = ty {
            return format!(
                "`{name}` is a type the caller chooses, so nothing here can tell whether it \
                 can be written. Require it: `{name}: Share`"
            );
        }
        if let Type::Adt { name, .. } = ty {
            if !self.variants.iter().any(|v| &v.type_name == name) && !self.effects.contains(name) {
                return format!(
                    "`{ty}` is declared without a body, so nothing here can see whether it \
                     can be written — and `Array` and `Ptr` both can. A type that is safe \
                     for two fibers to hold at once says so with `impl Share for {name}`"
                );
            }
        }
        if self.holds_a_closure(ty, &mut Vec::new()) {
            format!(
                "`{ty}` holds a closure, and what a closure captured is not in its type — so \
                 nothing here can tell whether *that* can be written. An effect is a record \
                 of function types, so this is every capability"
            )
        } else {
            format!("`{ty}` can be written, and two fibers writing one value is a race")
        }
    }

    fn holds_a_closure(&self, ty: &Type, visiting: &mut Vec<String>) -> bool {
        match ty {
            Type::Fn { .. } => true,
            Type::Tuple(items) => items.iter().any(|t| self.holds_a_closure(t, visiting)),
            Type::Applied { head, args } => {
                self.holds_a_closure(head, visiting)
                    || args.iter().any(|t| self.holds_a_closure(t, visiting))
            }
            Type::Adt { name, args } => {
                if args.iter().any(|t| self.holds_a_closure(t, visiting)) {
                    return true;
                }
                if self.effects.contains(name) {
                    return false;
                }
                if visiting.iter().any(|n| n == name) {
                    return false;
                }
                visiting.push(name.clone());
                let found = self
                    .variants
                    .iter()
                    .filter(|v| &v.type_name == name)
                    .any(|v| v.fields.iter().any(|t| self.holds_a_closure(t, visiting)));
                visiting.pop();
                found
            }
            _ => false,
        }
    }

    fn shareable(&self, ty: &Type, visiting: &mut Vec<String>, bounded: &[String]) -> bool {
        match ty {
            // A row variable is not a value and carries none: `'e` is how a
            // function fails, and nobody hands a failure to a fiber. Only a
            // *type* the caller chooses has to be asked about.
            Type::Param(name) if name.starts_with('\'') => true,
            Type::Param(name) => bounded.iter().any(|b| b == name),
            Type::Fn { .. } => false,
            Type::Tuple(items) => items.iter().all(|t| self.shareable(t, visiting, bounded)),
            Type::Applied { head, args } => {
                self.shareable(head, visiting, bounded)
                    && args.iter().all(|t| self.shareable(t, visiting, bounded))
            }
            Type::Adt { name, args } => {
                // The trusted answer comes first, arguments included. A type
                // with no body does not necessarily *hold* its parameters —
                // `SharedFn<Request, Response, 'e>` describes a call rather
                // than a contents, and a `Request` is built inside the fiber
                // that answers it — so asking about them would refuse the one
                // thing the wrapper exists to allow. An impl asserts for every
                // instantiation, which is what makes it a thing you have to be
                // trusted to write.
                if self.is_opaque(ty) {
                    return self.traits.find(SHARE, ty).is_some();
                }
                if !args.iter().all(|t| self.shareable(t, visiting, bounded)) {
                    return false;
                }
                // A type may contain itself, so an in-progress name answers
                // "yes" — anything genuinely unshareable in the cycle is found
                // by the field that is not the recursive one.
                if visiting.iter().any(|n| n == name) {
                    return true;
                }
                // A handler's operations are closures whose captures were
                // checked where the handler was written, so they are not asked
                // about again here — see the note above.
                if self.effects.contains(name) {
                    return true;
                }
                // **A type with no body has to say.** Nothing here can see
                // inside `export type Array<A>;`, and answering "shareable"
                // because no mutable field is *visible* is the wrong default in
                // the one direction that matters: `Array::set` writes, `Ptr`
                // points at foreign memory, and a runtime handle may need a
                // lock of its own. All three looked safe to share until this
                // line existed, and two fibers writing one array compiled.
                //
                // So the answer is declared, with `impl Share for T`. That is
                // the same trade `unsafe impl Sync` makes, minus a keyword this
                // language does not have: the author of the type knows what the
                // compiler cannot, and writing it down is what makes it
                // reviewable. `docs/design/sharing.md`.
                // The declared field types speak in the *type's* parameters —
                // `Cons(A, List<A>)` — and those are not the enclosing
                // function's, so they have to be replaced by what this use
                // actually supplied before anything is asked about them.
                // Reading `A` as a rigid parameter of the caller made every
                // generic container unshareable, `List` included.
                let parameters = self.adts.get(name).cloned().unwrap_or_default();
                let mapping: HashMap<&str, Type> = parameters
                    .iter()
                    .map(String::as_str)
                    .zip(args.iter().cloned())
                    .collect();
                visiting.push(name.clone());
                let ok = self
                    .variants
                    .iter()
                    .filter(|v| &v.type_name == name)
                    .all(|v| {
                        !v.has_mutable_field()
                            && v.fields.iter().all(|t| {
                                let t = unify::substitute(t, &mapping);
                                self.shareable(&t, visiting, bounded)
                            })
                    });
                visiting.pop();
                ok
            }
            // Foreign memory. Nothing on this side of the ABI knows what is
            // behind it or who else is writing there, and a pointer is exactly
            // the value whose whole purpose is to be written through.
            Type::Ptr => false,
            _ => true,
        }
    }

}

#[salsa::tracked(returns(ref))]
pub fn type_map(db: &dyn Db, file: SourceFile) -> TypeMap {
    let parse = khora_db::parse(db, file);
    let mut map = TypeMap::default();
    // Which of each type's parameters are const, so `Matrix<const R, const C>`
    // gets the kind `Int -> Int -> *` rather than `* -> * -> *`.
    let mut consts: HashMap<String, Vec<bool>> = HashMap::new();

    for (index, decl) in parse.source_file().decls().enumerate() {
        match decl {
            // A test takes nothing, returns nothing, and can fail — which is
            // the only interesting thing about its signature. The row is
            // *opened* where the body is checked, because a test may fail any
            // way it likes: that is what a failing test is.
            ast::Decl::Test(_) => {
                map.signatures.insert(
                    khora_hir::test_key(index),
                    Signature {
                        is_extern: false,
                        generics: Vec::new(),
                        bounds: Vec::new(),
                        requires: Type::empty_row(),
                        raises: Type::row(vec![(FAILED.to_string(), Type::adt(FAILED))], None),
                        params: Vec::new(),
                        ret: Type::Unit,
                    },
                );
            }
            ast::Decl::Fn(f) => {
                let Some(name) = f.name().and_then(|n| n.ident()) else { continue };
                let generics = generic_names(f.type_params().as_ref());
                let bounds = bound_lists(f.type_params().as_ref());
                let params = f
                    .params()
                    .map(|list| {
                        list.params().map(|p| type_of_syntax(p.ty().as_ref(), &generics)).collect()
                    })
                    .unwrap_or_default();
                let ret = f
                    .return_type()
                    .map_or(Type::Unit, |t| type_of_syntax(Some(&t), &generics));
                let requires =
                    row_of_syntax(f.with_clause().and_then(|c| c.row()).as_ref(), &generics);
                let raises =
                    row_of_syntax(f.raises_clause().and_then(|c| c.row()).as_ref(), &generics);
                map.signatures.insert(
                    name,
                    Signature {
                        is_extern: f.is_extern(),
                        generics,
                        bounds,
                        requires,
                        raises,
                        params,
                        ret,
                    },
                );
            }
            // An effect *is* a record of function types — `effect Ledger
            // { get: String -> Int }` and `type Ledger = { get: (String) -> Int }`
            // describe the same value. Collecting it as one keeps handlers,
            // field access and reference counting on the paths that already
            // work. `docs/design/effects.md` says as much: the shape "is a
            // record of function types".
            ast::Decl::Effect(e) => {
                let Some(name) = e.name().and_then(|n| n.ident()) else { continue };
                let generics = generic_names(e.type_params().as_ref());
                consts.insert(name.clone(), vec![false; generics.len()]);
                map.adts.insert(name.clone(), generics.clone());

                let mut labels = Vec::new();
                let mut fields = Vec::new();
                for op in e.operations() {
                    let Some(label) = op.name().and_then(|n| n.ident()) else { continue };
                    labels.push(label);
                    fields.push(type_of_syntax(op.ty().as_ref(), &generics));
                }
                map.effects.insert(name.clone());
                map.variants.push(VariantInfo {
                    type_name: name.clone(),
                    name,
                    fields,
                    labels,
                    // An effect's operations are a handler's fields, and a
                    // handler is built once and read.
                    mutable: Vec::new(),
                });
            }
            ast::Decl::Type(t) => {
                let Some(type_name) = t.name().and_then(|n| n.ident()) else { continue };
                let generics = generic_names(t.type_params().as_ref());
                let is_const: Vec<bool> = t
                    .type_params()
                    .map(|p| p.params().map(|g| g.is_const()).collect())
                    .unwrap_or_default();
                consts.insert(type_name.clone(), is_const);
                map.adts.insert(type_name.clone(), generics.clone());
                // `type Point = { x: Int, y: Int }` is one variant carrying
                // named fields — the same shape a constructor already has, so
                // field access, construction and drop glue are all reused.
                if let Some(ast::Type::Record(r)) = t.definition() {
                    let (labels, fields) = record_fields(&r, &generics);
                    let mutable = r.fields().map(|f| f.is_mut()).collect();
                    map.variants.push(VariantInfo {
                        type_name: type_name.clone(),
                        name: type_name.clone(),
                        fields,
                        labels,
                        mutable,
                    });
                }
                if let Some(ast::Type::Variant(v)) = t.definition() {
                    for case in v.cases() {
                        let Some(name) = case.name().and_then(|n| n.ident()) else { continue };
                        let fields = case
                            .fields()
                            .map(|list| {
                                list.fields()
                                    .map(|f| type_of_syntax(f.ty().as_ref(), &generics))
                                    .collect()
                            })
                            .or_else(|| {
                                case.tuple_fields().map(|list| {
                                    list.types()
                                        .map(|t| type_of_syntax(Some(&t), &generics))
                                        .collect()
                                })
                            })
                            .unwrap_or_default();
                        let labels = case
                            .fields()
                            .map(|list| {
                                list.fields()
                                    .filter_map(|f| f.name().and_then(|n| n.ident()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mutable = case
                            .fields()
                            .map(|list| list.fields().map(|f| f.is_mut()).collect())
                            .unwrap_or_default();
                        map.variants.push(VariantInfo {
                            type_name: type_name.clone(),
                            name,
                            fields,
                            labels,
                            mutable,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    map.signatures.extend(traits::impl_signatures(&parse.source_file()));
    map.traits = traits::collect(&parse.source_file());

    // What this file imported, under the names it uses for them. Without this
    // a cross-module call resolves and then type checks against nothing, which
    // is a *false pass* — strictly worse than the unresolved-name error it
    // replaced.
    import_types(db, file, &mut map, &mut consts);

    map.kinds = traits::kinds(&map.adts, &consts);
    map
}

/// Brings every inherent impl of an imported module into view.
///
/// Every one, not only the ones whose type was named in the import. A type's
/// own methods are part of the type exactly as its constructors are, and a
/// value can arrive without its type ever being written down: `req.params` has
/// type `Params`, and `req.params.get(..)` should work whether or not the file
/// also imported `Params`. There is nothing to shadow either — an inherent
/// impl is not a name that can be referred to, only a method reached by
/// having a value — so gating it on an import buys nothing and costs the
/// obvious call.
fn import_inherent(exported: &TypeMap, map: &mut TypeMap) {
    for imp in &exported.traits.inherent {
        if map.traits.inherent.contains(imp) {
            continue;
        }
        map.traits.inherent.push(imp.clone());
        let own = format!("#{}::", imp.head);
        for (key, signature) in &exported.signatures {
            if key.starts_with(&own) {
                map.signatures.insert(key.clone(), signature.clone());
            }
        }
    }
}

/// Copies the declarations a file imported into its own view.
///
/// Reads only the *defining* file's `type_map`, so this stays incremental: a
/// body edit in one module cannot invalidate another module's types.
fn import_types(
    db: &dyn Db,
    file: SourceFile,
    map: &mut TypeMap,
    consts: &mut HashMap<String, Vec<bool>>,
) {
    let scope = khora_hir::file_scope(db, file);
    let Some(root) = khora_db::source_root(db) else { return };
    let graph = khora_hir::module_graph(db, root);

    for origin in &scope.origins {
        let (local, module, name, kind) =
            (&origin.local, &origin.module, &origin.name, &origin.kind);
        let Some(source) = graph.file(module) else { continue };
        if source == file {
            continue;
        }
        let exported = type_map(db, source);
        import_inherent(exported, map);

        match kind {
            khora_hir::ItemKind::Function => {
                // `entry` rather than `insert`: a file's own declaration wins
                // over an import of the same name, which is what shadowing
                // means everywhere else in the language.
                if let Some(signature) = exported.signatures.get(name.as_str()) {
                    map.signatures.entry(local.clone()).or_insert_with(|| signature.clone());
                }
            }
            // An `effect` declares exactly what a type does here: an entry in
            // `adts` and one `VariantInfo` holding its operations as fields.
            // Left out, an imported effect arrived as `Unknown` and every
            // operation call on it read as a missing method.
            khora_hir::ItemKind::Type | khora_hir::ItemKind::Effect => {
                if let Some(generics) = exported.adts.get(name.as_str()) {
                    if !map.adts.contains_key(local.as_str()) {
                        map.adts.insert(local.clone(), generics.clone());
                        consts.insert(local.clone(), vec![false; generics.len()]);
                    }
                }
                // That it was declared `effect` travels with it. Without this
                // an imported capability was a plain record of closures here,
                // and so unshareable — which is to say no capability from
                // another module could reach a fiber, the exact thing the
                // exception exists to allow.
                if exported.effects.contains(name.as_str()) {
                    map.effects.insert(local.clone());
                }
                map.variants.extend(
                    exported.variants.iter().filter(|v| &v.type_name == name).cloned(),
                );
                // **`Share` travels with the type, not with the trait.** An
                // impl normally arrives when its trait is imported, which is
                // right for `Show` — you ask for the trait, you get the impls.
                // `Share` is not asked for: it is a property the compiler reads
                // when a value crosses a fiber, and a file that never mentions
                // it still needs the answer. Without this, importing `SharedFn`
                // without also importing `Share` made it silently unshareable.
                for shared in exported.traits.impls.iter().filter(|i| {
                    i.trait_name == SHARE && i.head().as_deref() == Some(name.as_str())
                }) {
                    let known = map
                        .traits
                        .impls
                        .iter()
                        .any(|i| i.trait_name == SHARE && i.head() == shared.head());
                    if known {
                        // Imported twice under two names is still one impl, and
                        // the duplicate check downstream would call it two.
                        continue;
                    }
                    map.traits.impls.push(shared.clone());
                    // The declaration too, or the impl is an impl of nothing.
                    if let Some(def) = exported.traits.traits.get(SHARE) {
                        map.traits.traits.entry(SHARE.to_string()).or_insert_with(|| def.clone());
                    }
                    if let Some(kind) = exported.kinds.get(name.as_str()) {
                        map.kinds.entry(local.clone()).or_insert_with(|| kind.clone());
                    }
                }
                // A type's own methods come with it — see `import_inherent`,
                // which brings the whole module's rather than this type's.
            }
            khora_hir::ItemKind::Trait => {
                if let Some(def) = exported.traits.traits.get(name.as_str()) {
                    map.traits.traits.insert(local.clone(), def.clone());
                }
                // A trait's impls travel with it: an imported `Show` is useless
                // if the impls that satisfy it stayed behind.
                map.traits
                    .impls
                    .extend(exported.traits.impls.iter().filter(|i| &i.trait_name == name).cloned());
                for (key, signature) in &exported.signatures {
                    if key.starts_with(&format!("{name}::"))
                        || key.starts_with(&format!("{name}#"))
                    {
                        map.signatures.insert(key.clone(), signature.clone());
                    }
                }
                if let Some(kind) = exported.kinds.get(name.as_str()) {
                    map.kinds.insert(local.clone(), kind.clone());
                }
            }
            _ => {}
        }
    }
}

/// Points at the part of two large types that actually disagrees.
///
/// Unification reports the innermost conflicting pair, which on its own reads
/// as "expected `3`, found `4`" and leaves the reader hunting for where either
/// number came from. The caller leads with the whole types; this adds the
/// detail, and adds nothing when the conflict *is* the whole type, since
/// repeating it would say the same thing twice.
fn disagreement(outer: (&Type, &Type), inner: (&Type, &Type)) -> String {
    if outer == inner {
        return String::new();
    }
    match inner {
        (Type::Const(_), Type::Const(_)) => {
            format!("; dimension `{}` does not match `{}`", inner.0, inner.1)
        }
        _ => format!("; `{}` does not match `{}`", inner.0, inner.1),
    }
}

/// The traits each parameter requires, positionally matched to
/// [`generic_names`]. A parameter with no bounds contributes an empty list, so
/// the two are always the same length.
pub(crate) fn bound_lists(params: Option<&ast::TypeParams>) -> Vec<Vec<String>> {
    params
        .map(|p| {
            p.params()
                .filter(|g| g.name().and_then(|n| n.ident()).is_some())
                .map(|g| traits::bound_names(g.bounds().as_ref()))
                .collect()
        })
        .unwrap_or_default()
}

fn generic_names(params: Option<&ast::TypeParams>) -> Vec<String> {
    params
        .map(|p| {
            p.params()
                // A row variable is a parameter like any other, and is rigid
                // inside the body for the same reason: the caller chooses what
                // the rest of the row is.
                .filter_map(|g| g.name().and_then(|n| n.ident()).or_else(|| g.row_var()))
                .collect()
        })
        .unwrap_or_default()
}

/// A method key as it was written in the source.
///
/// Keys are mangled so the two halves cannot collide with a name a program
/// chose — `#Router::listen`, `Eq#Int::eq` — and `#` cannot occur in an
/// identifier, which is exactly why it must not reach a diagnostic either.
pub fn as_written(key: &str) -> String {
    match key.split_once('#') {
        // `#Head::method`: a type's own function.
        Some(("", rest)) => rest.to_string(),
        // `Trait#Head::method`: reached through the type that implements it.
        Some((_, rest)) => rest.to_string(),
        None => key.to_string(),
    }
}

/// The type whose `spawn` starts a fiber.
///
/// Named here rather than in the backend because the *checker* enforces what
/// may cross into one, and a rule about sharing is a type error rather than a
/// code-generation one.
pub const FIBER_TYPE: &str = "Fiber";

/// The certified-closure wrapper. `SharedFn::of` is where the check happens.
pub const SHARED_FN_TYPE: &str = "SharedFn";

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

/// The labels and types of a record type's fields, in written order.
fn record_fields(r: &ast::RecordType, generics: &[String]) -> (Vec<String>, Vec<Type>) {
    let mut labels = Vec::new();
    let mut fields = Vec::new();
    for f in r.fields() {
        let Some(label) = f.name().and_then(|n| n.ident()) else { continue };
        labels.push(label);
        fields.push(type_of_syntax(f.ty().as_ref(), generics));
    }
    (labels, fields)
}

/// Reads a `with` or `raises` clause into a row.
///
/// Absent means the closed empty row: a function with no clause requires
/// nothing and raises nothing, which is what makes those the safe defaults and
/// what an entry point has to reduce to.
fn row_of_syntax(clause: Option<&ast::Type>, generics: &[String]) -> Type {
    let Some(clause) = clause else { return Type::empty_row() };
    match clause {
        // `with { ledger: Ledger | 'e }`
        ast::Type::Record(r) => {
            // With a tail, the labels after the `|` are nested inside it
            // rather than beside it, so both places have to be read.
            let after_tail: Vec<ast::Field> =
                r.row_tail().map(|t| t.fields().collect()).unwrap_or_default();
            let fields: Vec<(String, Type)> = r
                .fields()
                .chain(after_tail)
                .filter_map(|f| {
                    let label = f.name()?.ident()?;
                    Some((label, type_of_syntax(f.ty().as_ref(), generics)))
                })
                .collect();
            let tail = r
                .row_tail()
                .and_then(|t| t.types().next())
                .map(|t| type_of_syntax(Some(&t), generics));
            Type::row(fields, tail)
        }
        // `raises DbError + ModelError`. An error row labels each entry with
        // the error's own type name: two errors of one type cannot be told
        // apart, and by name is how they are handled.
        //
        // A row variable among the operands is the row's *tail*, not an entry
        // in it: `raises 'e + HttpError` means "whatever the caller's handler
        // can raise, and also this". Reading it as a label gave the row an
        // entry called `'e`, which no `raises` clause could ever satisfy.
        ast::Type::Union(u) => {
            let mut fields = Vec::new();
            let mut tail = None;
            for operand in u.operands() {
                match type_of_syntax(Some(&operand), generics) {
                    Type::Param(name) if name.starts_with('\'') => {
                        tail = Some(Type::Param(name));
                    }
                    resolved => fields.push((label_of(&resolved), resolved)),
                }
            }
            Type::row(fields, tail)
        }
        // `raises DbError`, or a bare `'r`.
        other => {
            let ty = type_of_syntax(Some(other), generics);
            match &ty {
                // A bare row variable is the whole row.
                Type::Param(name) if name.starts_with('\'') => Type::row(Vec::new(), Some(ty)),
                _ => match error_label(other, generics) {
                    Some(entry) => Type::row(vec![entry], None),
                    None => Type::empty_row(),
                },
            }
        }
    }
}

/// One entry of an error row, labelled by the error type's own name.
fn error_label(ty: &ast::Type, generics: &[String]) -> Option<(String, Type)> {
    let resolved = type_of_syntax(Some(ty), generics);
    let label = match &resolved {
        Type::Adt { name, .. } => name.clone(),
        Type::Param(name) => name.clone(),
        other => other.to_string(),
    };
    Some((label, resolved))
}

/// Maps written syntax to a type.
///
/// `generics` are the names in scope as rigid parameters — a bare `A` inside
/// `fn f<A>(..)` is [`Type::Param`], not an undeclared ADT. Anything else
/// unrecognized becomes [`Type::Unknown`], which suppresses follow-on errors.
fn type_of_syntax(ty: Option<&ast::Type>, generics: &[String]) -> Type {
    let Some(ty) = ty else { return Type::Unknown };
    match ty {
        ast::Type::Unit(_) => Type::Unit,
        // `(Int, Bool) -> Int`. The parameter list parses as whatever shape the
        // parentheses made of it: a tuple for several, a paren for one, a unit
        // for none. All three mean the same thing here.
        ast::Type::Fn(f) => {
            let params = match f.param_type() {
                Some(ast::Type::Tuple(t)) => {
                    t.elements().map(|e| type_of_syntax(Some(&e), generics)).collect()
                }
                Some(ast::Type::Unit(_)) | None => Vec::new(),
                Some(ast::Type::Paren(p)) => {
                    vec![type_of_syntax(p.inner().as_ref(), generics)]
                }
                Some(other) => vec![type_of_syntax(Some(&other), generics)],
            };
            let ret = type_of_syntax(f.return_type().as_ref(), generics);
            Type::Fn {
                params,
                ret: Box::new(ret),
                requires: Box::new(row_of_syntax(
                    f.with_clause().and_then(|c| c.row()).as_ref(),
                    generics,
                )),
                raises: Box::new(row_of_syntax(
                    f.raises_clause().and_then(|c| c.row()).as_ref(),
                    generics,
                )),
            }
        }
        // A bare integer in type position is a const-generic argument.
        ast::Type::Literal(l) => l.value().map(Type::Const).unwrap_or(Type::Unknown),
        ast::Type::Tuple(t) => {
            let items: Vec<Type> =
                t.elements().map(|e| type_of_syntax(Some(&e), generics)).collect();
            if items.is_empty() { Type::Unit } else { Type::Tuple(items) }
        }
        ast::Type::Path(p) => {
            // A bare `'r`. It has no `Path` of its own — it is one token — so
            // without this it read as the empty name and became `Unknown`,
            // which then absorbed whatever it was unified with and made every
            // row-polymorphic signature pass by saying nothing.
            if let Some(row_var) = p.row_var() {
                return Type::Param(row_var.text().to_string());
            }
            let name = p.path().map(|p| p.text_path()).unwrap_or_default();
            let args: Vec<Type> = p
                .type_args()
                .map(|a| a.args().map(|t| type_of_syntax(Some(&t), generics)).collect())
                .unwrap_or_default();
            named_type(&name, args, generics)
        }
        _ => Type::Unknown,
    }
}

/// A type written as a name and some arguments, resolved against the type
/// parameters in scope.
///
/// The single place a type *name* means something. Reached from the syntax
/// above and from a [`TypeRef`] below, because two interpreters of one name is
/// how the two come to disagree — and the disagreement is silent.
fn named_type(name: &str, args: Vec<Type>, generics: &[String]) -> Type {
    // `T::Item` where `T` is a parameter in scope is a projection, not a type
    // whose name happens to contain `::`.
    if let Some((owner, assoc)) = name.split_once("::") {
        if generics.iter().any(|g| g == owner) {
            return Type::Assoc {
                owner: Box::new(Type::Param(owner.to_string())),
                name: assoc.to_string(),
            };
        }
    }

    match name {
        "Int" | "I64" => Type::Int,
        "Float" => Type::Float,
        "Bool" => Type::Bool,
        "String" => Type::Str,
        "Ptr" => Type::Ptr,
        "" => Type::Unknown,
        other if IntKind::parse(other).is_some() => {
            Type::Fixed(IntKind::parse(other).expect("just checked"))
        }
        other if generics.iter().any(|g| g == other) => {
            if args.is_empty() {
                Type::Param(other.to_string())
            } else {
                Type::Applied { head: Box::new(Type::Param(other.to_string())), args }
            }
        }
        other => Type::Adt { name: other.to_string(), args },
    }
}

/// The same, for a type a *body* wrote down: a `let` annotation.
///
/// [`TypeRef::Opaque`] becomes `Unknown`, which unifies with everything and so
/// checks nothing — the shapes it stands for are the ones the echo does not
/// carry yet, and saying nothing about them is what every annotation used to
/// get.
pub fn type_of_ref(ty: &khora_hir::body::TypeRef, generics: &[String]) -> Type {
    use khora_hir::body::TypeRef;
    match ty {
        TypeRef::Unit => Type::Unit,
        TypeRef::Const(value) => Type::Const(*value),
        TypeRef::Opaque => Type::Unknown,
        TypeRef::Tuple(items) => {
            let items: Vec<Type> = items.iter().map(|t| type_of_ref(t, generics)).collect();
            if items.is_empty() { Type::Unit } else { Type::Tuple(items) }
        }
        TypeRef::Named { name, args } => {
            let args = args.iter().map(|t| type_of_ref(t, generics)).collect();
            named_type(name, args, generics)
        }
    }
}

/// Every type the checker worked out for one body.
///
/// The checker computes these on its way to a verdict, and code generation
/// cannot work without them. Publishing them here is what stops a second
/// implementation of the same rules existing downstream and drifting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BodyTypes {
    exprs: HashMap<ExprId, Type>,
    locals: HashMap<LocalId, Type>,
    /// Which instantiation each mention of a generic function chose.
    ///
    /// Recorded here because the checker is the only place that knows: it
    /// created the variables and solved them. Monomorphization reads it to
    /// find out which specializations a body needs.
    instantiations: HashMap<ExprId, (String, Vec<Type>)>,
    /// Bindings a lambda captures because its body uses them *implicitly*.
    ///
    /// A `with` block lowers to a block of `let`s, so a capability is an
    /// ordinary binding and a lambda that uses one captures it like any other.
    /// But nothing in the body *names* it — `report(n)` needs `ledger` without
    /// saying so — and the capture scan watches names. Which labels a call
    /// needs is the callee's row, which only the checker has read, so the
    /// answer is published here rather than guessed at twice.
    lambda_captures: HashMap<ExprId, Vec<khora_hir::body::LocalId>>,
}

impl BodyTypes {
    /// The type of an expression. `Unknown` for anything the checker could not
    /// determine, which is also what an id it never visited reports.
    pub fn of(&self, id: ExprId) -> &Type {
        self.exprs.get(&id).unwrap_or(&Type::Unknown)
    }

    pub fn local(&self, id: LocalId) -> &Type {
        self.locals.get(&id).unwrap_or(&Type::Unknown)
    }

    /// The generic function this expression mentions, and at what arguments.
    pub fn instantiation(&self, id: ExprId) -> Option<&(String, Vec<Type>)> {
        self.instantiations.get(&id)
    }

    /// Bindings this lambda captures implicitly. See the field.
    pub fn implicit_captures(&self, id: ExprId) -> &[khora_hir::body::LocalId] {
        self.lambda_captures.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn instantiations(&self) -> impl Iterator<Item = (&ExprId, &(String, Vec<Type>))> {
        self.instantiations.iter()
    }

    /// This body's types with `mapping` applied, which is one specialization.
    pub fn specialized(&self, mapping: &HashMap<&str, Type>) -> BodyTypes {
        BodyTypes {
            exprs: self
                .exprs
                .iter()
                .map(|(k, v)| (*k, unify::substitute(v, mapping)))
                .collect(),
            locals: self
                .locals
                .iter()
                .map(|(k, v)| (*k, unify::substitute(v, mapping)))
                .collect(),
            instantiations: self
                .instantiations
                .iter()
                .map(|(k, (name, args))| {
                    let args = args.iter().map(|a| unify::substitute(a, mapping)).collect();
                    (*k, (name.clone(), args))
                })
                .collect(),
            // Bindings, not types: a specialization captures the same ones the
            // generic body does, and there is nothing in a `LocalId` to
            // substitute.
            lambda_captures: self.lambda_captures.clone(),
        }
    }
}

/// The result of checking one file: the verdict, and the working.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Checked {
    pub errors: Vec<HirError>,
    /// Per function, in declaration order.
    pub bodies: Vec<(String, BodyTypes)>,
}

/// Checks a file, keeping both the diagnostics and the types.
///
/// One query rather than two so the work is done once; the accessors below are
/// what callers normally want.
#[salsa::tracked(returns(ref))]
pub fn checked(db: &dyn Db, file: SourceFile) -> Checked {
    let types = type_map(db, file);
    let mut out = Checked::default();
    // A file that did not parse has holes in it, and a hole is an `Unknown`
    // that nothing in *this* pass reported. The syntax error is the message
    // worth reading; see `Checker::check_unknowns`.
    let parsed = khora_db::parse(db, file).errors().is_empty();

    for (name, body) in khora_hir::body::bodies(db, file) {
        let mut signature = types.signatures.get(name).cloned().unwrap_or(Signature {
            is_extern: false,
            generics: Vec::new(),
            bounds: Vec::new(),
            requires: Type::empty_row(),
            raises: Type::empty_row(),
            params: Vec::new(),
            ret: Type::Unknown,
        });
        let mut unifier = Unifier::new().with_assoc(types.traits.assoc_bindings());
        // A test's error row is open: an error escaping a test is a *failing
        // test*, not a program that does not compile. Opened here rather than
        // in the signature because only a unifier can make a flexible tail,
        // and a rigid one would reject the very thing this is for.
        if name.starts_with(khora_hir::TEST_PREFIX) {
            signature.raises = Type::row(Vec::new(), Some(unifier.fresh()));
        }
        let mut checker = Checker {
            types,
            body,
            signature: &signature,
            locals: HashMap::new(),
            exprs: HashMap::new(),
            instantiations: HashMap::new(),
            unifier,
            lambdas: Vec::new(),
            demanded: Vec::new(),
            projections: Vec::new(),
            enclosing_lambdas: Vec::new(),
            lambda_captures: HashMap::new(),
            installed: Vec::new(),
            loops: Vec::new(),
            open_raises: Vec::new(),
            hint: None,
            marked: Vec::new(),
            errors: Vec::new(),
        };
        checker.check_function();
        checker.close_open_raises();
        checker.check_bounds();
        checker.settle_projections();
        checker.check_effects();
        if parsed {
            checker.check_unknowns();
        }
        out.errors.extend(checker.errors);
        // Published types are zonked: a consumer should never see a variable,
        // and code generation cannot do anything with one.
        let exprs = checker.exprs.iter().map(|(k, v)| (*k, checker.unifier.zonk(v))).collect();
        let locals = checker.locals.iter().map(|(k, v)| (*k, checker.unifier.zonk(v))).collect();
        let instantiations = checker
            .instantiations
            .iter()
            .map(|(k, (n, args))| {
                let args = args.iter().map(|a| checker.unifier.zonk(a)).collect();
                (*k, (n.clone(), args))
            })
            .collect();
        let lambda_captures = std::mem::take(&mut checker.lambda_captures);
        out.bodies.push((
            name.clone(),
            BodyTypes { exprs, locals, instantiations, lambda_captures },
        ));
    }
    out
}

/// The type of every expression and binding, per function.
pub fn body_types(db: &dyn Db, file: SourceFile) -> &Vec<(String, BodyTypes)> {
    &checked(db, file).bodies
}

/// Type errors for one file, and nothing else.
///
/// Kept separate from lowering errors so "does this type-check" stays a
/// question with its own answer; [`diagnostics`] is what a driver wants.
pub fn check_file(db: &dyn Db, file: SourceFile) -> &Vec<HirError> {
    &checked(db, file).errors
}

/// Everything wrong with the traits and impls a file declares.
///
/// Separate from `check_file` because none of it depends on a function body:
/// an impl is well-formed or it is not, whatever any caller does with it.
#[salsa::tracked(returns(ref))]
pub fn trait_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let types = type_map(db, file);
    traits::check(&types.traits, &types.kinds, &types.signatures, &|ty| types.is_opaque(ty))
}

/// Which clause a requirement came from.
///
/// Recorded rather than guessed. The two rows look alike — both are sets of
/// labels — and the only reliable difference is which clause wrote them, since
/// a capability's label is a field name and an error's is a type name only by
/// convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    Requires,
    Raises,
}

impl Clause {
    fn verb(self) -> &'static str {
        match self {
            Clause::Requires => "require",
            Clause::Raises => "raise",
        }
    }

    /// How to name one entry of this kind of row in a message.
    fn describe(self, label: &str, ty: &Type) -> String {
        match self {
            // A capability is supplied under a label, so both halves matter.
            Clause::Requires => format!("{label}: {ty}"),
            // An error is labelled by its own type name, and printing
            // `DbError: DbError` reads as a mistake.
            Clause::Raises => format!("{ty}"),
        }
    }
}

/// One effect a body's call sites asked of the function containing them.
struct Demand {
    /// Whether the callee was *known* to be fallible when this was recorded.
    ///
    /// Kept because the row does not survive to say so: a `catch` empties it,
    /// and so does a closure absorbing it, and neither of those excuses the
    /// mark. The row answers "what can leave"; this answers "was there
    /// anything to mark", which is a different question the moment something
    /// discharges the first one.
    fallible: bool,
    clause: Clause,
    row: Type,
    range: TextRange,
    callee: String,
    /// The call this came from, for checking that a fallible one is marked.
    /// `None` for a `raise`, which is its own mark.
    site: Option<ExprId>,
}

struct Checker<'a> {
    types: &'a TypeMap,
    body: &'a Body,
    signature: &'a Signature,
    locals: HashMap<LocalId, Type>,
    exprs: HashMap<ExprId, Type>,
    instantiations: HashMap<ExprId, (String, Vec<Type>)>,
    unifier: Unifier,
    /// The type of each lambda currently being inferred, innermost last, so
    /// that a recursive closure can refer to itself before its body is done.
    lambdas: Vec<Type>,
    /// What this body has demanded of its caller so far, accumulated as calls
    /// are checked and compared against the signature at the end.
    ///
    /// Requirements flow *upward*: a function that calls something needing
    /// `ledger` needs `ledger` too, unless a `with` block supplies it. Rows
    /// are checked against the declaration rather than inferred into it,
    /// because an exported signature is a promise and inferring one silently
    /// would let a body widen it. `docs/design/effects.md`.
    demanded: Vec<Demand>,
    /// Where each deferred projection was written, in the order the unifier
    /// deferred them, so `settle_projections` can report against the source.
    projections: Vec<(TextRange, String)>,
    /// The lambdas currently being inferred, innermost last, each with the
    /// bindings it has been found to use implicitly.
    enclosing_lambdas: Vec<(ExprId, Vec<khora_hir::body::LocalId>)>,
    /// The finished answer, moved out as each lambda closes.
    lambda_captures: HashMap<ExprId, Vec<khora_hir::body::LocalId>>,
    /// The capabilities in scope from enclosing `with` blocks.
    ///
    /// A call inside one is served by it, so its labels never reach the
    /// signature. That is row subtraction: `with` *discharges* a requirement
    /// rather than forwarding it.
    installed: Vec<String>,
    /// The loops currently being inferred, innermost last.
    ///
    /// Each holds the type its `break`s agree on and whether any `break`
    /// carried a value at all. A loop nobody breaks out of with a value
    /// produces `()`; one that does produces what they carry, and two `break`s
    /// carrying different types is a mismatch reported at the second.
    loops: Vec<(Type, bool)>,
    /// The open tail of every lambda's inferred `raises` row, in the order the
    /// lambdas were seen.
    ///
    /// A lambda's error row is a **lower bound**, not an exact answer: the body
    /// raises at least these, and the context may reasonably ask it to be
    /// declared as raising more. That is what makes a stub usable where a
    /// fallible operation is expected — a mock that never fails satisfies
    /// `raises IoError`, because raising fewer things is always safe.
    ///
    /// The tail is a variable, so it is filled in by whatever the lambda is
    /// checked against. Anything still unsolved when the body is done was never
    /// asked for anything, and defaults to closed-empty: nothing said this
    /// could fail, so it cannot. Without that default an unconstrained tail
    /// would leave the row *open*, and an open row is a fallible one to the
    /// code generator — every lambda would return a tagged pair for nothing.
    open_raises: Vec<Type>,
    /// The type the surrounding expression is asking for, when there is one.
    ///
    /// Only integer literals read it, and only to decide which integer they
    /// are: `let b: U8 = 65` has to work, and 65 on its own is an `Int`. This
    /// is a *hint*, not a demand — `require` still runs afterwards, so a wrong
    /// hint changes which error is reported and never whether one is.
    ///
    /// Consumed by the first `infer` that sees it, and re-armed only where a
    /// type flows through unchanged: the branches of an `if`, the tail of a
    /// block, the arms of a `match`. Anywhere else it would leak into a
    /// subexpression that means something different — the `0` in `array[0]`
    /// is an index, not a `U8`, however the result is being used.
    hint: Option<Type>,
    /// Calls written with `!`.
    ///
    /// A call that can leave the function has to say so at the call site —
    /// that is the whole justification for the mark in
    /// `docs/design/effects.md`. Recorded rather than checked inline because
    /// the inner expression is inferred before its parent is known.
    marked: Vec<ExprId>,
    errors: Vec<HirError>,
}

impl<'a> Checker<'a> {
    fn error(&mut self, message: impl Into<String>, range: TextRange) {
        self.errors.push(HirError { message: message.into(), range });
    }

    fn check_function(&mut self) {
        for (i, pat) in self.body.params.iter().enumerate() {
            let ty = self.signature.params.get(i).cloned().unwrap_or(Type::Unknown);
            self.bind_pattern(*pat, &ty);
        }
        // `with { ledger: Ledger }` binds `ledger` for the body at the type the
        // row gave it.
        let required = match self.signature.requires.clone() {
            Type::Row { fields, .. } => fields,
            _ => Vec::new(),
        };
        for (label, pat) in self.body.evidence.clone() {
            let ty = required
                .iter()
                .find(|(l, _)| *l == label)
                .map(|(_, t)| t.clone())
                .unwrap_or(Type::Unknown);
            self.bind_pattern(pat, &ty);
        }

        let Some(root) = self.body.root else { return };
        let actual = self.infer(root);
        let expected = self.signature.ret.clone();
        if let Err(why) = self.unifier.unify(&expected, &actual) {
            let expected = self.unifier.zonk(&expected);
            let actual = self.unifier.zonk(&actual);
            let range = self.body.range(root);
            // The plain mismatch would read "expected `Int`, found `Bool`",
            // which repeats what the sentence already said.
            let message = match why {
                Mismatch::Types { expected: inner, found: got } => {
                    let inner = self.unifier.zonk(&inner);
                    let got = self.unifier.zonk(&got);
                    let detail = disagreement((&expected, &actual), (&inner, &got));
                    let head = format!("this function returns `{expected}`,");
                    format!("{head} but its body has type `{actual}`{detail}")
                }
                // The other mismatches are whole sentences of their own, so
                // they are joined rather than folded into "but its body ...",
                // which produced "but its body `A` is a type the caller
                // chooses".
                other => format!("this function returns `{expected}`; {other}"),
            };
            self.error(message, range);
        }
    }

    /// Records the type of every binding a pattern introduces.
    fn bind_pattern(&mut self, pat: PatId, ty: &Type) {
        match self.body.pat(pat).clone() {
            Pat::Bind(local) => {
                self.locals.insert(local, ty.clone());
            }
            Pat::TupleStruct { resolution, fields } => {
                let variant = variant_case(&resolution)
                    .and_then(|(t, n)| self.types.variant_of(&t, &n))
                    .cloned();
                // Field types are declared against the type's own parameters,
                // so they have to be read at the scrutinee's instantiation:
                // matching `Option<Int>` binds `v` to `Int`, not to `A`.
                let mapping = variant
                    .as_ref()
                    .map(|v| self.substitution_for(&v.type_name, ty))
                    .unwrap_or_default();
                let borrowed: HashMap<&str, Type> =
                    mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();

                for (i, field) in fields.iter().enumerate() {
                    let declared = variant
                        .as_ref()
                        .and_then(|v| v.fields.get(i).cloned())
                        .unwrap_or(Type::Unknown);
                    let field_ty = unify::substitute(&declared, &borrowed);
                    self.bind_pattern(*field, &field_ty);
                }
            }
            Pat::Tuple(fields) => {
                // Destructuring only knows the component types when the
                // scrutinee is a tuple of the same width; a mismatch is
                // reported where the two are unified, not here.
                for (i, field) in fields.iter().enumerate() {
                    let component = match ty {
                        Type::Tuple(items) => items.get(i).cloned().unwrap_or(Type::Unknown),
                        _ => Type::Unknown,
                    };
                    self.bind_pattern(*field, &component);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => {}
        }
    }

    /// Infers `id` and requires it to fit `expected`.
    fn expect(&mut self, id: ExprId, expected: &Type, context: &str) -> Type {
        // Armed for the literal case and cleared by the `infer` below whatever
        // it turns out to be, so it can never be read by an unrelated later
        // expression.
        self.hint = Some(self.unifier.zonk(expected));
        let actual = self.infer(id);
        let range = self.body.range(id);
        self.require(expected, &actual, context, range);
        actual
    }

    /// Reports a literal that cannot be the fixed-width integer being asked of
    /// it.
    ///
    /// A compile-time version of the overflow trap, and the same reasoning:
    /// `let b: U8 = 300` is a mistake with one right answer, and truncating it
    /// silently to 44 is the kind of thing that is found in production.
    fn check_literal_fits(&mut self, text: &str, kind: IntKind, range: TextRange) {
        let cleaned = text.replace('_', "");
        let Ok(value) = cleaned.parse::<i128>() else {
            // Too wide for even an i128, so certainly too wide for this. The
            // `Int` path reports its own version of this.
            self.error(format!("`{text}` does not fit in `{}`", kind.name()), range);
            return;
        };
        let (lo, hi) = kind.range();
        if value < lo || value > hi {
            self.error(
                format!(
                    "`{text}` does not fit in `{}`, which holds {lo} to {hi}",
                    kind.name()
                ),
                range,
            );
        }
    }

    /// Unifies two types for the information, not for the verdict.
    ///
    /// Used to push an expected type into a call before its arguments are
    /// checked. A failure is dropped: the caller is speculating, and the real
    /// check happens where the expectation came from.
    ///
    /// The deferred-projection bookkeeping still has to happen, because
    /// `settle_projections` pairs the unifier's deferred list with
    /// `self.projections` by position — leaving one out slides every later
    /// diagnostic onto the wrong range.
    fn hint_at(&mut self, expected: &Type, found: &Type, range: TextRange) {
        let before = self.unifier.deferred_len();
        let _ = self.unifier.unify(expected, found);
        for _ in before..self.unifier.deferred_len() {
            self.projections.push((range, "this call".to_string()));
        }
    }

    /// Reports any type the checker finished without working out.
    ///
    /// **`Unknown` is a silence, not a type.** It is compatible with
    /// everything, which is exactly what makes it useful downstream of an error
    /// — one mistake should not become five — and exactly what makes it
    /// invisible when nothing went wrong. Four errata are the same sentence
    /// about different holes (24, 26, 27, 30), and the fifth, entry 40, was
    /// found by the *code generator* three layers away, in a message naming a
    /// variable the author never wrote.
    ///
    /// So: a body the checker finished cleanly must have no `Unknown` left in
    /// it. If one is there, either the program is ambiguous in a way nothing
    /// reported, or the checker has a gap — and both are worth a sentence
    /// where they happened rather than a symptom somewhere else.
    ///
    /// Run **only when the body is otherwise clean**, because after an error
    /// `Unknown` is doing its job and saying so again would bury the real
    /// message. "Clean" means more than this pass being quiet: a name that did
    /// not resolve or a fragment that did not parse leaves an `Unknown` behind
    /// too, and both were already reported — by a different pass, whose errors
    /// are not in this list.
    fn check_unknowns(&mut self) {
        if !self.errors.is_empty() || !self.body.errors.is_empty() {
            return;
        }
        let visited: Vec<ExprId> = self.exprs.keys().copied().collect();
        if visited
            .iter()
            .any(|id| matches!(self.body.expr(*id), Expr::Missing | Expr::Unresolved(_)))
        {
            return;
        }

        let mut found: Vec<TextRange> = Vec::new();
        for id in visited {
            let ty = self.exprs[&id].clone();
            if matches!(self.unifier.zonk(&ty), Type::Unknown) {
                found.push(self.body.range(id));
            }
        }
        // One report, at the *narrowest* expression. They cascade — an
        // expression of unknown type makes the block around it one too — and
        // the smallest range is the innermost, which is where the trail starts.
        found.sort_by_key(|r| (r.len(), r.start()));
        if let Some(range) = found.first().copied() {
            self.error(
                "the type of this expression was never worked out, and nothing else was \
                 reported — so either it needs an annotation, or this is a gap in the \
                 compiler worth reporting"
                    .to_string(),
                range,
            );
        }
    }

    /// Closes every lambda error row nothing ever asked to be wider.
    ///
    /// Run once the body is checked. A tail still unsolved here was never
    /// compared against anything, so the honest reading is "this raises exactly
    /// what its body raises" — which is the closed empty row on the end.
    fn close_open_raises(&mut self) {
        for tail in std::mem::take(&mut self.open_raises) {
            if matches!(self.unifier.shallow(&tail), Type::Var(_)) {
                let _ = self.unifier.unify(&tail, &Type::empty_row());
            }
        }
    }

    /// Requires two types to be equal, reporting `context` if they are not.
    ///
    /// `context` is a noun phrase: the mismatch supplies the detail after it,
    /// so the two read as one sentence.
    fn require(&mut self, expected: &Type, found: &Type, context: &str, range: TextRange) -> bool {
        // A projection whose owner is not known yet defers instead of failing,
        // and is retried in `settle_projections`. Where it was written is not
        // recoverable from the unifier, so it is noted here, in the one place
        // that has both a range and a reason.
        let before = self.unifier.deferred_len();
        let outcome = self.unifier.unify(expected, found);
        for _ in before..self.unifier.deferred_len() {
            self.projections.push((range, context.to_string()));
        }
        match outcome {
            Ok(()) => true,
            Err(why) => {
                // Zonk first: a message naming `?3` instead of `Int` is useless,
                // and unification may have solved the variable on the way to
                // discovering the conflict.
                let message = match why {
                    Mismatch::Types { expected: inner, found: got } => {
                        let inner = self.unifier.zonk(&inner);
                        let got = self.unifier.zonk(&got);
                        let outer = self.unifier.zonk(expected);
                        let whole = self.unifier.zonk(found);
                        let head =
                            Mismatch::Types { expected: outer.clone(), found: whole.clone() };
                        let detail = disagreement((&outer, &whole), (&inner, &got));
                        format!("{context}: {head}{detail}")
                    }
                    other => format!("{context}: {other}"),
                };
                self.error(message, range);
                false
            }
        }
    }

    fn infer(&mut self, id: ExprId) -> Type {
        let ty = self.infer_uncached(id);
        self.exprs.insert(id, ty.clone());
        ty
    }

    fn infer_uncached(&mut self, id: ExprId) -> Type {
        let range = self.body.range(id);
        let hint = self.hint.take();
        match self.body.expr(id).clone() {
            Expr::Missing | Expr::Unresolved(_) => Type::Unknown,
            Expr::Unit => Type::Unit,
            Expr::Literal(lit) => match lit {
                // An integer literal is an `Int` unless something is asking
                // for a narrower one, in which case it is that — and has to
                // fit it. There is no widening anywhere else in the language,
                // so this is the only way to write a `U8` that is not a
                // conversion, and without it every byte in a table would be
                // `Int::to_u8(65)`.
                Literal::Int(text) => match hint {
                    Some(Type::Fixed(kind)) => {
                        self.check_literal_fits(&text, kind, range);
                        Type::Fixed(kind)
                    }
                    _ => Type::Int,
                },
                Literal::Float(_) => Type::Float,
                Literal::Str(_) => Type::Str,
                Literal::Bool(_) => Type::Bool,
            },
            Expr::Local(local) => self.locals.get(&local).cloned().unwrap_or(Type::Unknown),
            Expr::Path(resolution) => self.type_of_resolution(id, &resolution),
            Expr::Field { base, name } => {
                let owner = self.infer(base);
                let owner = self.unifier.shallow(&owner);
                let Some((_, field)) = self.record_field(&owner, &name) else {
                    // Silent for a type that is not known yet: `Unknown` is
                    // downstream of an error already reported.
                    if !matches!(owner, Type::Unknown | Type::Var(_) | Type::Never) {
                        self.error(format!("`{owner}` has no field `{name}`"), range);
                    }
                    return Type::Unknown;
                };
                field
            }
            Expr::Unary { op, operand } => match op {
                UnOp::Neg => self.infer_negation(operand, hint, range),
                UnOp::Not => self.expect(operand, &Type::Bool, "`!`"),
            },
            Expr::Binary { op, lhs, rhs } => self.infer_binary(id, op, lhs, rhs, hint),
            Expr::Assign { target, value } => {
                let target_ty = self.infer(target);
                self.check_writable(target, range);
                self.expect(value, &target_ty, "this assignment");
                Type::Unit
            }
            Expr::Call { callee, args } => self.infer_call(callee, &args, hint, range),
            Expr::Block { stmts, tail } => {
                // A `with` block lowered to an ordinary one, and its labels
                // are supplied to everything inside it.
                let supplied = self.body.installs.get(&id).cloned().unwrap_or_default();
                let depth = self.installed.len();
                self.installed.extend(supplied);
                self.hint = hint;
                let ty = self.infer_block(&stmts, tail);
                self.installed.truncate(depth);
                ty
            }
            Expr::If { condition, then_branch, else_branch } => {
                self.expect(condition, &Type::Bool, "an `if` condition");
                self.hint = hint.clone();
                let then_ty = self.infer(then_branch);
                match else_branch {
                    Some(else_id) => {
                        self.hint = hint;
                        let else_ty = self.infer(else_id);
                        if !self.require(&then_ty, &else_ty, "`if` branches disagree", range) {
                            return Type::Unknown;
                        }
                        if matches!(then_ty, Type::Never) { else_ty } else { then_ty }
                    }
                    // Without an `else`, the branch is only well typed if it
                    // produces nothing — the same rule `match` follows.
                    None => {
                        self.require(
                            &Type::Unit,
                            &then_ty,
                            "an `if` without `else` must produce `()`",
                            range,
                        );
                        Type::Unit
                    }
                }
            }
            Expr::While { condition, body } => {
                self.expect(condition, &Type::Bool, "a `while` condition");
                self.infer(body);
                Type::Unit
            }
            Expr::Loop { body } => {
                // A `loop` yields whatever its `break`s carry. Left as
                // `Unknown` through phase 2 rather than guessed — which was
                // fine until `Unknown` stopped being allowed to mean "not
                // worked out", because `Unknown` unifies with everything and so
                // `let n: Bool = loop { break 1 };` was accepted.
                let answer = self.unifier.fresh();
                self.loops.push((answer.clone(), false));
                self.infer(body);
                let (answer, carried) = self.loops.pop().expect("just pushed");
                // Nothing broke with a value, so there is no value: an infinite
                // loop and a loop that just stops both produce `()`.
                if carried { answer } else { Type::Unit }
            }
            Expr::Break(value) => {
                if let Some(v) = value {
                    let carried = self.infer(v);
                    // A `break` outside a loop is reported by HIR lowering,
                    // which knows the nesting; nothing to add here.
                    if let Some((answer, _)) = self.loops.last().cloned() {
                        self.require(&answer, &carried, "`break` values disagree", range);
                        if let Some(last) = self.loops.last_mut() {
                            last.1 = true;
                        }
                    }
                }
                Type::Never
            }
            Expr::Continue => Type::Never,
            Expr::Return(value) => {
                let expected = self.signature.ret.clone();
                match value {
                    Some(v) => {
                        self.expect(v, &expected, "this `return`");
                    }
                    None => {
                        if self.unifier.unify(&expected, &Type::Unit).is_err() {
                            self.error(
                                format!("this function returns `{expected}`, so `return` needs a value"),
                                range,
                            );
                        }
                    }
                }
                Type::Never
            }
            Expr::List(items) => {
                for item in items {
                    self.infer(item);
                }
                Type::Unknown
            }
            Expr::Tuple(items) => {
                let types: Vec<Type> = items.iter().map(|i| self.infer(*i)).collect();
                if types.is_empty() { Type::Unit } else { Type::Tuple(types) }
            }
            Expr::Match { scrutinee, arms } => {
                self.hint = hint;
                self.infer_match(scrutinee, &arms, range)
            }
            Expr::Record { owner, fields } => self.infer_record(owner, &fields, range),

            // `raise e` leaves the function, so it stands wherever an
            // expression can and its type constrains nothing.
            Expr::Raise(error) => {
                let ty = self.infer(error);
                let ty = self.unifier.zonk(&ty);
                if !matches!(ty, Type::Unknown | Type::Var(_)) {
                    self.demanded.push(Demand {
                        // A `raise` is the mark: there is no call to write `!`
                        // on, and `site: None` already says the check does not
                        // apply here.
                        fallible: false,
                        clause: Clause::Raises,
                        row: Type::row(vec![(label_of(&ty), ty)], None),
                        range,
                        callee: "raise".to_string(),
                        site: None,
                    });
                }
                Type::Never
            }

            // `f()!` is the identity on types. What it does is mark, and the
            // mark is what excuses the call from needing one.
            Expr::Try(inner) => {
                // A demand is recorded against the *callee*, since that is
                // what carries the signature, so the mark has to reach it too:
                // `f()!` marks the call and the `f` inside it.
                self.marked.push(inner);
                if let Expr::Call { callee, .. } = self.body.expr(inner) {
                    self.marked.push(*callee);
                }
                self.infer(inner)
            }
            Expr::Catch { inner, arms } => self.infer_catch(inner, &arms, range),
            Expr::Lambda { params, body, .. } => {
                // A parameter with no annotation gets a variable, so the type
                // is settled by how the lambda is used: `map(xs, (x) => x + 1)`
                // learns `x: Int` from `map`'s signature, not from the lambda.
                let types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        let ty = self.unifier.fresh();
                        self.bind_pattern(*p, &ty);
                        ty
                    })
                    .collect();

                // The whole type exists before the body is checked, because a
                // recursive closure mentions itself inside it. The result is a
                // variable the body then solves, and so is the error row.
                let result = self.unifier.fresh();
                let raises = self.unifier.fresh();
                let whole = Type::Fn {
                    params: types,
                    ret: Box::new(result.clone()),
                    // Always empty. A capability a closure uses is *captured*,
                    // because a `with` block is a block of `let`s and a
                    // closure captures the bindings it reads — so there is
                    // nothing left for a caller to supply. A failure cannot be
                    // captured, which is why the other row is inferred.
                    requires: Box::new(Type::empty_row()),
                    raises: Box::new(raises.clone()),
                };
                self.lambdas.push(whole.clone());
                self.enclosing_lambdas.push((id, Vec::new()));

                let before = self.demanded.len();
                let ret = self.infer(body);
                let mine = self.absorb_raises(before);
                // Left open, because what the body raises is a lower bound
                // rather than the answer — see `open_raises`. A closed row here
                // is what made a mock that cannot fail unusable as an operation
                // declared to fail.
                let mine = match mine {
                    Type::Row { fields, tail: None } => {
                        let rest = self.unifier.fresh();
                        self.open_raises.push(rest.clone());
                        Type::row(fields, Some(rest))
                    }
                    already_open => already_open,
                };
                let _ = self.unifier.unify(&raises, &mine);

                if let Some((_, found)) = self.enclosing_lambdas.pop() {
                    self.lambda_captures.insert(id, found);
                }
                self.lambdas.pop();

                self.require(&result, &ret, "this closure's body", range);
                whole
            }
            // Inside its own body, a closure's name is the closure.
            Expr::LambdaSelf => {
                self.lambdas.last().cloned().unwrap_or(Type::Unknown)
            }
        }
    }

    /// `-x`, over any number.
    ///
    /// Two things it has to get right. The type is whatever is being negated
    /// rather than always `Int`, so `-1.5` is a `Float` and `-b` on a `U8` is
    /// refused for the better reason.
    ///
    /// And a negated *literal* is checked as one number, not as a negation
    /// applied to another: `-128` is an `I8` even though `128` is not, and
    /// there is no other way to write that type's smallest value. Folding it
    /// here is what makes the sign part of the literal, which is what a reader
    /// already assumes it is.
    fn infer_negation(&mut self, operand: ExprId, hint: Option<Type>, range: TextRange) -> Type {
        if let (Some(Type::Fixed(kind)), Expr::Literal(Literal::Int(text))) =
            (&hint, self.body.expr(operand).clone())
        {
            let kind = *kind;
            self.check_literal_fits(&format!("-{text}"), kind, range);
            let ty = Type::Fixed(kind);
            self.exprs.insert(operand, ty.clone());
            return ty;
        }

        self.hint = hint;
        let inner = self.infer(operand);
        let inner = self.unifier.zonk(&inner);
        // An unsolved variable becomes `Int`, which is what a bare `-x` in a
        // generic position has always meant.
        let expected = match inner {
            Type::Float => Type::Float,
            Type::Fixed(kind) => Type::Fixed(kind),
            _ => Type::Int,
        };
        self.require(&expected, &inner, "negation", self.body.range(operand));
        expected
    }

    fn infer_binary(
        &mut self,
        site: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        hint: Option<Type>,
    ) -> Type {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                // The left operand decides which arithmetic this is, so the
                // hint goes to it and to nothing else: `let b: U8 = 1 + 2`
                // needs the `1` to know, and once it does the `2` is told by
                // the `expect` below rather than by guessing again.
                if matches!(hint, Some(Type::Fixed(_))) {
                    self.hint = hint;
                }
                // `+` is also string concatenation, which the reference
                // program relies on.
                let left = self.infer(lhs);
                if op == BinOp::Add && matches!(left, Type::Str) {
                    self.expect(rhs, &Type::Str, "string concatenation");
                    return Type::Str;
                }
                // Arithmetic is over `Int` or over `Float`, and the left
                // operand says which. No mixing and no promotion: `1 + 2.0` is
                // an error rather than a silent conversion, which is what Go
                // and Rust both do and what stops a rounding surprise from
                // being invisible.
                let left = self.unifier.zonk(&left);
                let expected = match left {
                    Type::Float => Type::Float,
                    Type::Fixed(kind) => Type::Fixed(kind),
                    _ => Type::Int,
                };
                let lhs_range = self.body.range(lhs);
                self.require(&expected, &left, "arithmetic", lhs_range);
                self.expect(rhs, &expected, "arithmetic");
                expected
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let left = self.infer(lhs);
                self.expect(rhs, &left, "this comparison");
                let zonked = self.unifier.zonk(&left);
                let asks = match op {
                    BinOp::Eq | BinOp::Ne => needs_an_eq_impl(&zonked),
                    _ => needs_an_ord_impl(&zonked),
                };
                if asks {
                    let range = self.body.range(site);
                    match op {
                        BinOp::Eq | BinOp::Ne => {
                            self.require_comparison(site, "Eq", "eq", &zonked, range)
                        }
                        // Ordering is `Ord::cmp`, for the same reason equality
                        // is `Eq::eq`: what "less than" means for a type is the
                        // type's answer, and `Ord: Eq` is the trait saying the
                        // two have to agree.
                        _ => self.require_comparison(site, "Ord", "cmp", &zonked, range),
                    }
                }
                Type::Bool
            }
            BinOp::And | BinOp::Or => {
                self.expect(lhs, &Type::Bool, "a logical operator");
                self.expect(rhs, &Type::Bool, "a logical operator");
                Type::Bool
            }
        }
    }

    /// Resolves the impl that a comparison operator on this type will call.
    ///
    /// **`==` on a scalar is a machine instruction; on anything else it is
    /// `Eq::eq`, and `<` is `Ord::cmp`.** That keeps one meaning for each
    /// operator rather than two: a type decides what equality and order mean
    /// for it, in Khora, in a function a reader can go and look at.
    /// `impl Eq for Int` is written *in terms of* `==` and not the other way
    /// round, which is what stops the rule being circular — and is why `Float`
    /// can have the operators without the traits.
    ///
    /// Recorded as an instantiation so that monomorphization emits the impl and
    /// the code generator can find it, exactly as a written `a.eq(b)` would.
    fn require_comparison(
        &mut self,
        site: ExprId,
        trait_name: &str,
        method: &str,
        ty: &Type,
        range: TextRange,
    ) {
        let key = format!("{trait_name}::{method}");

        // Inside a generic function the operand is *rigid*, and the only
        // comparison it has is the one its bounds promise. Which impl runs is
        // decided when the function is specialized, exactly as it is for a
        // written `a.cmp(b)` on a bounded parameter.
        let available = match ty {
            Type::Param(param) => self.bounds_on(param).iter().any(|b| b == trait_name),
            other => self.types.traits.find(trait_name, other).is_some(),
        };
        if !available {
            let advice = match ty {
                Type::Param(param) => format!("Add the bound, as `{param}: {trait_name}`"),
                other => format!("Write `impl {trait_name} for {other}`"),
            };
            let operators = if trait_name == "Eq" {
                "`==` and `!=` have"
            } else {
                "`<`, `>`, `<=` and `>=` have"
            };
            self.error(
                format!("`{ty}` has no `{trait_name}` impl, so {operators} nothing to call. {advice}"),
                range,
            );
            return;
        }
        // Reported whether or not the trait is in this file's scope. "There is
        // no impl" is true either way, and staying quiet here is what let the
        // reference application reach the code generator before anybody
        // mentioned that `RiskLevel` could not be compared.
        let Some(signature) = self.types.signatures.get(key.as_str()).cloned() else { return };

        // `Self` is the method's first type argument — the same fact
        // `call_signature` relies on — and binding it is what tells
        // monomorphization which impl to emit.
        let (_, type_args) =
            self.unifier.instantiate_with(&signature.generics, &signature.as_fn());
        if let Some(self_arg) = type_args.first() {
            let _ = self.unifier.unify(self_arg, ty);
        }
        self.instantiations.insert(site, (key, type_args));
    }

    /// Maps a type's parameters onto the arguments `ty` supplies.
    ///
    /// Falls back to fresh variables when the scrutinee is not the expected
    /// ADT — usually downstream of another error, where inventing a variable
    /// keeps one mistake from becoming several.
    fn substitution_for(&mut self, type_name: &str, ty: &Type) -> HashMap<String, Type> {
        let generics = self.types.adts.get(type_name).cloned().unwrap_or_default();
        let args = match self.unifier.zonk(ty) {
            Type::Adt { name, args } if name == type_name => args,
            _ => Vec::new(),
        };
        generics
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let arg = args.get(i).cloned().unwrap_or_else(|| self.unifier.fresh());
                (g.clone(), arg)
            })
            .collect()
    }

    /// A fresh instance of an ADT, and the substitution that produced it.
    ///
    /// The substitution is what lets a constructor's declared field types be
    /// read at the same instantiation as the result: for `Some(1)` the field is
    /// `?0` and the result `Option<?0>`, and unifying the argument solves both.
    fn instantiate_adt(&mut self, name: &str) -> (Type, HashMap<String, Type>) {
        let generics = self.types.adts.get(name).cloned().unwrap_or_default();
        let mapping: HashMap<String, Type> =
            generics.iter().map(|g| (g.clone(), self.unifier.fresh())).collect();
        let args = generics.iter().map(|g| mapping[g].clone()).collect();
        (Type::Adt { name: name.to_string(), args }, mapping)
    }

    fn infer_call(
        &mut self,
        callee: ExprId,
        args: &[ExprId],
        hint: Option<Type>,
        range: TextRange,
    ) -> Type {
        // Checked after the arguments, because a lambda's implicit captures are
        // only known once it has been inferred.
        let certifying = match self.body.expr(callee) {
            Expr::Path(khora_hir::Resolution::TraitItem { owner, name }) => {
                (owner == FIBER_TYPE && name == "spawn")
                    || (owner == SHARED_FN_TYPE && name == "of")
            }
            _ => false,
        };
        if certifying {
            let result = self.infer_call_inner(callee, args, hint, range);
            self.check_spawnable(args, range);
            return result;
        }
        self.infer_call_inner(callee, args, hint, range)
    }

    fn infer_call_inner(
        &mut self,
        callee: ExprId,
        args: &[ExprId],
        hint: Option<Type>,
        range: TextRange,
    ) -> Type {
        // A constructor call builds its ADT.
        if let Expr::Path(resolution) = self.body.expr(callee).clone() {
            if let Some((owner, case)) = variant_case(&resolution) {
                if let Some(variant) = self.types.variant_of(&owner, &case).cloned() {
                    if args.len() != variant.fields.len() {
                        self.error(
                            format!(
                                "`{}` takes {} argument(s), but {} were given",
                                variant.name,
                                variant.fields.len(),
                                args.len()
                            ),
                            range,
                        );
                    }
                    let (result, mapping) = self.instantiate_adt(&variant.type_name);
                    // What the constructor is *for* reaches its arguments, by
                    // way of what it builds: `let b: Option<U8> = Option::Some(200)`
                    // needs the `200` to be a `U8`, and nothing in
                    // `Some(value: A)` says so until `Option<A>` has met
                    // `Option<U8>`. The same rule a call already follows, and
                    // silent for the same reason — see `hint_at`.
                    if let Some(hint) = &hint {
                        self.hint_at(hint, &result, range);
                    }
                    let borrowed: HashMap<&str, Type> =
                        mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                    for (arg, declared) in args.iter().zip(&variant.fields) {
                        let expected = unify::substitute(declared, &borrowed);
                        self.expect(*arg, &expected, "this argument");
                    }
                    return result;
                }
            }
        }

        if let Expr::Field { base, name } = self.body.expr(callee).clone() {
            // A *field* holding a function wins over a method of the same
            // name, which is decision D2 in `docs/design/associated-items.md`:
            // `x.f()` finds a field of `x`, or an item declared against `x`'s
            // type, and the field is the more specific of the two.
            let owner = self.infer(base);
            let owner = self.unifier.shallow(&owner);
            if self.record_field(&owner, &name).is_some() {
                return self.apply(Some(callee), args, hint, range);
            }
            if let Some(ty) = self.infer_method_call(callee, base, &name, args, range) {
                return ty;
            }
        }

        // Resolved first: a callee's type is often a *variable solved to* a
        // function rather than a function, and matching the shape without
        // following the variable silently treats it as uncallable.
        self.apply(Some(callee), args, hint, range)
    }

    /// Checks a call whose callee is an ordinary value of function type.
    ///
    /// This is also where a call is charged to the enclosing function. The
    /// rows come from the callee's *type*, not from a signature looked up by
    /// name, which is what makes calling an effectful function through a
    /// variable — or a parameter, or a field — check the same as calling it
    /// directly.
    fn apply(
        &mut self,
        callee: Option<ExprId>,
        args: &[ExprId],
        hint: Option<Type>,
        range: TextRange,
    ) -> Type {
        let inferred = match callee {
            Some(callee) => self.infer(callee),
            None => Type::Unknown,
        };
        let callee_ty = self.unifier.shallow(&inferred);
        let Type::Fn { params, ret, requires, raises } = callee_ty else {
            for arg in args {
                self.infer(*arg);
            }
            // Silent for a type that is not known yet: `Unknown` is downstream
            // of an error already reported, and a variable may still turn out
            // to be a function. Anything else is a real mistake, and one that
            // became reachable the moment functions became values.
            if !matches!(callee_ty, Type::Unknown | Type::Var(_) | Type::Never) {
                let zonked = self.unifier.zonk(&callee_ty);
                self.error(format!("`{zonked}` is not a function, so it cannot be called"), range);
            }
            return Type::Unknown;
        };

        if args.len() != params.len() {
            self.error(
                format!("this call takes {} argument(s), but {} were given", params.len(), args.len()),
                range,
            );
        }
        // What the call is *for* reaches its arguments, by way of its result.
        // `let cells: Array<U8> = Array::new(4, 0)` needs the `0` to be a `U8`,
        // and nothing in `Array::new(length, fill)` says so until `Array<A>` has
        // met `Array<U8>`. Solving the return first is what carries it.
        //
        // Silently, because a hint that does not fit is not itself the error —
        // whoever wrote the annotation is about to be told about it by the
        // `require` that asked for the hint, and reporting it twice, once here
        // against the wrong range, is worse than not reporting it at all.
        if let Some(hint) = hint {
            self.hint_at(&hint, &ret, range);
        }

        for (arg, expected) in args.iter().zip(&params) {
            self.expect(*arg, expected, "this argument");
        }

        let label = callee.map(|c| self.callee_label(c)).unwrap_or_else(|| "this call".into());
        self.demand_rows(&requires, &raises, &label, callee, range);
        *ret
    }

    /// What to call the callee in a diagnostic.
    ///
    /// A name when there is one, and otherwise a description: `(f(x))(y)` has
    /// no name for its callee, and "this call" beats inventing one.
    fn callee_label(&self, callee: ExprId) -> String {
        match self.body.expr(callee) {
            Expr::Path(khora_hir::Resolution::Item { name, .. }) => as_written(name),
            Expr::Path(khora_hir::Resolution::Variant { type_name, name, .. }) => {
                format!("{type_name}::{name}")
            }
            Expr::Path(khora_hir::Resolution::TraitItem { owner, name }) => {
                format!("{owner}::{name}")
            }
            Expr::Local(local) => self.body.local(*local).name.clone(),
            Expr::Field { name, .. } => name.clone(),
            _ => "this call".to_string(),
        }
    }

    /// Resolves `receiver.method(args)` through the traits in scope.
    ///
    /// Returns `None` when the receiver has a *field* of that name, so a record
    /// holding a function keeps working — the field reading is the more
    /// specific one and wins, exactly as it does in Rust.
    fn infer_method_call(
        &mut self,
        callee: ExprId,
        receiver: ExprId,
        method: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Option<Type> {
        let inferred = self.infer(receiver);
        let self_ty = self.unifier.zonk(&inferred);

        // A receiver whose type is still open cannot select an impl. Saying so
        // is better than picking one and being wrong about it later.
        if matches!(self_ty, Type::Unknown | Type::Var(_) | Type::Never) {
            return None;
        }

        // A type's own method wins over a trait's. Adding a trait to a program
        // must not silently change what an existing call does.
        if let Some(own) = self.types.traits.inherent_method(&self_ty, method) {
            let key = traits::method_key("", &own.head, method);
            return Some(self.call_signature(callee, &key, &self_ty, args, range));
        }

        // Inside a generic function the receiver is rigid, and the only methods
        // it has are the ones its bounds promise. `F<B>` counts: the methods
        // available on it are the ones `F`'s bounds promise, which is what makes
        // `f(v).map(..)` work inside a `traverse`.
        let rigid = match &self_ty {
            Type::Param(p) => Some(p.clone()),
            Type::Applied { head, .. } => match &**head {
                Type::Param(p) => Some(p.clone()),
                _ => None,
            },
            _ => None,
        };
        if let Some(param) = rigid {
            return Some(
                self.infer_bounded_method(callee, &param, &self_ty, method, args, range),
            );
        }

        let (def, imp) = match traits::method_source(&self.types.traits, &self_ty, method) {
            Ok(found) => found,
            // Records do not exist yet, so there is no field that could hold a
            // function and no other reading of `x.f()`. When they land, the
            // field is checked before this and only reaches here if absent.
            Err(traits::MethodError::Unknown) => {
                for arg in args {
                    self.infer(*arg);
                }
                self.error(format!("`{self_ty}` has no method `{method}`"), range);
                return Some(Type::Unknown);
            }
            Err(traits::MethodError::NotImplemented(owners)) => {
                self.error(
                    format!(
                        "`{self_ty}` does not implement `{}`, which is where `{method}` comes from",
                        owners.join("` or `")
                    ),
                    range,
                );
                return Some(Type::Unknown);
            }
            Err(traits::MethodError::Ambiguous(names)) => {
                self.error(
                    format!(
                        "`{method}` is declared by `{}`, and `{self_ty}` implements more than one",
                        names.join("` and `")
                    ),
                    range,
                );
                return Some(Type::Unknown);
            }
        };

        let key = format!("{}::{method}", def.name);
        let _ = imp;
        Some(self.call_signature(callee, &key, &self_ty, args, range))
    }

    /// A method reached through a bound rather than through an impl.
    ///
    /// `fn f<T: Eq>(a: T, b: T) { a.eq(b) }` has no impl to select — `T` is
    /// whatever the caller passes — so the *trait's* signature is used, and
    /// which impl runs is settled by monomorphization.
    fn infer_bounded_method(
        &mut self,
        callee: ExprId,
        param: &str,
        receiver: &Type,
        method: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Type {
        let declared = self.bounds_on(param);
        let available = traits::with_supertraits(&self.types.traits, &declared);
        let found = available.iter().find_map(|name| {
            let def = self.types.traits.traits.get(name)?;
            def.method(method).map(|m| (def.name.clone(), m.signature.clone()))
        });

        let Some((trait_name, _)) = found else {
            for arg in args {
                self.infer(*arg);
            }
            self.error(
                if declared.is_empty() {
                    format!(
                        "`{param}` is a type the caller chooses and has no bounds, so it has no \
                         method `{method}`; add one, as `{param}: Trait`"
                    )
                } else {
                    format!(
                        "no method `{method}` on `{param}`, whose bounds are `{}`",
                        declared.join("` + `")
                    )
                },
                range,
            );
            return Type::Unknown;
        };

        let key = format!("{trait_name}::{method}");
        self.call_signature(callee, &key, receiver, args, range)
    }

    /// Checks a call against `key`'s signature with `Self` bound to `self_ty`.
    fn call_signature(
        &mut self,
        callee: ExprId,
        key: &str,
        self_ty: &Type,
        args: &[ExprId],
        range: TextRange,
    ) -> Type {
        let Some(signature) = self.signature_for(key, self_ty) else {
            for arg in args {
                self.infer(*arg);
            }
            return Type::Unknown;
        };

        // `Self` is the method's first type argument, so a call through a
        // trait carries the one fact that decides which impl runs. It reaches
        // monomorphization the same way every other type argument does.
        let (ty, type_args) =
            self.unifier.instantiate_with(&signature.generics, &signature.as_fn());
        self.demand(&signature, &type_args, key, callee, range);
        self.instantiations.insert(callee, (key.to_string(), type_args));
        let Type::Fn { params, ret, .. } = ty else { return Type::Unknown };

        // Bind `Self` by unifying the *receiver parameter* with the receiver,
        // not by assigning the receiver's type to `Self` directly. For `Eq` the
        // parameter is `Self` and the two are the same thing; for `Functor` it
        // is `Self<A>`, and only unifying through it decides `Self := Option`
        // and `A := Int` rather than the nonsense `Self := Option<Int>`.
        if let Some(receiver) = params.first() {
            let _ = self.unifier.unify(receiver, self_ty);
        }

        // The receiver is the first parameter, and it is already checked: it is
        // what selected this signature. Only the written arguments remain.
        let expected = params.get(1..).unwrap_or(&[]);
        if args.len() != expected.len() {
            self.error(
                format!(
                    "`{key}` takes {} argument(s) after the receiver, but {} were given",
                    expected.len(),
                    args.len()
                ),
                range,
            );
        }
        for (arg, want) in args.iter().zip(expected) {
            self.expect(*arg, want, "this argument");
        }
        *ret
    }

    /// The trait's signature for a method key, with `Self` still a parameter.
    /// The type of a block: its tail, or `Never` if anything in it diverges.
    ///
    /// A statement that diverges makes the whole block diverge — `{ return 0; }`
    /// has type `Never`, not `()`, or an `if` whose branch returns would
    /// wrongly disagree with the other branch.
    fn infer_block(&mut self, stmts: &[Stmt], tail: Option<ExprId>) -> Type {
        let mut diverged = false;
        for stmt in stmts {
            match stmt {
                Stmt::Let { pat, ty: declared, init } => {
                    // An annotation is checked against the initializer and
                    // then *is* the binding's type. Until errata 36 it was
                    // parsed and dropped, so `let x: Bool = 5` compiled clean
                    // — an annotation that is only a comment is worse than no
                    // annotation, because it is believed.
                    let declared = declared
                        .as_ref()
                        .map(|t| type_of_ref(t, &self.signature.generics));
                    let ty = match (declared, init) {
                        (Some(declared), Some(e)) => {
                            self.expect(*e, &declared, "this binding");
                            declared
                        }
                        (Some(declared), None) => declared,
                        (None, Some(e)) => self.infer(*e),
                        (None, None) => Type::Unknown,
                    };
                    diverged |= matches!(ty, Type::Never);
                    self.bind_pattern(*pat, &ty);
                }
                Stmt::Expr(e) => {
                    diverged |= matches!(self.infer(*e), Type::Never);
                }
            }
        }
        let tail_ty = tail.map(|t| self.infer(t)).unwrap_or(Type::Unit);
        if diverged {
            Type::Never
        } else {
            tail_ty
        }
    }

    /// Records what a call requires of the enclosing function.
    ///
    /// The rows are instantiated with the same arguments the signature was, so
    /// a row variable in the callee becomes a fresh variable here and is
    /// solved by whatever the caller turns out to provide.
    fn demand(
        &mut self,
        signature: &Signature,
        type_args: &[Type],
        key: &str,
        callee_site: ExprId,
        range: TextRange,
    ) {
        let mapping: HashMap<&str, Type> = signature
            .generics
            .iter()
            .map(String::as_str)
            .zip(type_args.iter().cloned())
            .collect();
        let requires = unify::substitute(&signature.requires, &mapping);
        let raises = unify::substitute(&signature.raises, &mapping);
        self.demand_rows(&requires, &raises, key, Some(callee_site), range);
    }

    /// Records what a call requires, given rows that are already instantiated.
    ///
    /// This is the form a call *through a value* uses: the rows are part of
    /// the callee's type rather than looked up from a signature, which is what
    /// lets an effectful function be passed around and called somewhere else.
    fn demand_rows(
        &mut self,
        requires: &Type,
        raises: &Type,
        key: &str,
        callee_site: Option<ExprId>,
        range: TextRange,
    ) {
        // Before anything is subtracted: what an enclosing `with` block
        // supplies is exactly what a lambda inside it has to capture, so the
        // labels have to be read here rather than after they are discharged.
        if let (Some(site), Type::Row { fields, .. }) = (callee_site, &self.unifier.zonk(requires))
        {
            for (label, _) in fields {
                self.note_implicit_capture(site, label);
            }
        }

        for (clause, row) in [(Clause::Requires, requires), (Clause::Raises, raises)] {
            // Zonked here, not later: the `installed` subtraction below needs
            // the labels, and `installed` is scoped to the `with` block this
            // call sits in — by the time `check_effects` runs, the block is
            // long gone. A row that a call's arguments have already solved is
            // known by now, which is the ordinary case; one that is still a
            // variable simply has nothing to subtract.
            let mut row = self.unifier.zonk(row);
            // Whatever an enclosing `with` block supplies is already answered.
            if clause == Clause::Requires {
                if let Type::Row { fields, tail } = &row {
                    let left: Vec<(String, Type)> = fields
                        .iter()
                        .filter(|(l, _)| !self.installed.contains(l))
                        .cloned()
                        .collect();
                    row = Type::row(left, tail.as_deref().cloned());
                }
            }
            if matches!(&row, Type::Row { fields, tail } if fields.is_empty() && tail.is_none()) {
                continue;
            }
            // A row with something *in* it. A variable is not one yet — a
            // closure calling itself asks for the row it is in the middle of
            // inferring — and neither is an open tail, which says "possibly
            // more" rather than "at least one". Every lambda's row is open
            // now, because what a body raises is a lower bound; counting a
            // tail here would make every self-call demand a `!` for nothing.
            //
            // Nothing is lost by ignoring it: if the tail is later solved to
            // something with labels in it, the row itself says so, and
            // `check_effects` re-reads the row.
            let known_fallible = matches!(&row, Type::Row { fields, .. } if !fields.is_empty());
            self.demanded.push(Demand {
                fallible: clause == Clause::Raises && known_fallible,
                clause,
                row,
                range,
                callee: key.to_string(),
                site: callee_site,
            });
        }
    }

    /// Takes the failures demanded since `before` as a closure's own row.
    ///
    /// A closure cannot charge its failures to whoever wrote it — it may be
    /// called anywhere, and by then that function has returned. So the demands
    /// its body raised become part of *its* type, and the enclosing function
    /// is left answering only what it was asked directly.
    ///
    /// The demands stay in the list with their rows emptied rather than being
    /// removed, because they are also what checks that a fallible call wore
    /// its `!`. A closure does not excuse the mark any more than a `catch`
    /// does.
    fn absorb_raises(&mut self, before: usize) -> Type {
        let window: Vec<Demand> = self.demanded.split_off(before);
        let mut fields: Vec<(String, Type)> = Vec::new();
        let mut tail = None;

        let kept: Vec<Demand> = window
            .into_iter()
            .map(|mut demand| {
                if demand.clause == Clause::Raises {
                    if let Type::Row { fields: raised, tail: rest } = self.unifier.zonk(&demand.row)
                    {
                        fields.extend(raised);
                        tail = tail.take().or(rest.map(|t| *t));
                        demand.row = Type::empty_row();
                    }
                }
                demand
            })
            .collect();
        self.demanded.extend(kept);
        Type::row(fields, tail)
    }

    /// What a fiber's body may close over.
    ///
    /// A mutable value handed to another fiber is a data race, and this is the
    /// only place one can cross: a fiber touches exactly what its thunk
    /// captured. `docs/design/memory.md` §5a.
    ///
    /// The thunk therefore has to be one whose captures are visible here — a
    /// lambda written at the call, or a named function, which captures
    /// nothing. Anything else is refused rather than waved through, because a
    /// check that cannot see what it is checking is not a check. That also
    /// makes the rule worth having on its own terms: **a fiber's body is
    /// written where it starts**, so what it closes over is on the screen.
    fn check_spawnable(&mut self, args: &[ExprId], range: TextRange) {
        let Some(body) = args.first().copied() else { return };
        let captures: Vec<khora_hir::body::LocalId> = match self.body.expr(body) {
            Expr::Lambda { captures, .. } => captures
                .iter()
                .copied()
                .chain(self.lambda_captures.get(&body).into_iter().flatten().copied())
                .collect(),
            // A named function captures nothing. Its own `with` clause is
            // checked at the call like any other.
            Expr::Path(_) => return,
            _ => {
                self.error(
                    "this has to be a closure written here or a named function, so that \
                     what it closes over can be checked — a closure that arrived under a \
                     name captured whatever it captured somewhere else"
                        .to_string(),
                    range,
                );
                return;
            }
        };

        for local in captures {
            let ty = self.unifier.zonk(self.locals.get(&local).unwrap_or(&Type::Unknown));
            if self.types.is_shareable(&ty, &self.shared_params()) {
                continue;
            }
            let name = self.body.local(local).name.clone();
            let why = self.types.why_unshareable(&ty);
            self.error(format!("`{name}` cannot be handed to another fiber: {why}"), range);
        }
    }

    /// Every operation of a handler must be safe to hand to another fiber.
    ///
    /// **This is what buys an effect its shareability.** A capability has to be
    /// able to cross into a fiber — a request arrives, a fiber handles it, the
    /// handler needs the database — and an effect is a record of closures,
    /// which nothing at the type level can see inside. So the question is asked
    /// *here*, at the one place a handler comes into existence, where the
    /// lambdas are written and what they captured is on the screen.
    ///
    /// Answered once, where it is answerable, instead of at every spawn where
    /// it is not. `docs/design/sharing.md`.
    ///
    /// The cost is a real restriction: a handler may not capture something
    /// writable, so a test double that counts its calls in a `mut` field is
    /// refused. That is the same trade `Shared<A>` is being kept in reserve
    /// for, and the error says which binding and why.
    fn check_handler_is_shareable(&mut self, owner: &str, fields: &[(String, ExprId)]) {
        for (label, value) in fields {
            let range = self.body.range(*value);
            // **The closure has to be written here.** A binding holding one
            // was written somewhere else, and its captures went with it:
            //
            // ```
            // let leak = fn () => bump(tally);
            // let h = handler for Counting { tick: leak };
            // ```
            //
            // Nothing at this line can see what `leak` took, so accepting it
            // would let any closure through by the simple move of naming it
            // first — and the whole exception that makes an effect shareable
            // rests on this check being the one place it cannot be dodged.
            if !matches!(self.body.expr(*value), Expr::Lambda { .. } | Expr::Path(_)) {
                self.error(
                    format!(
                        "`{owner}`'s `{label}` has to be a closure written here or a named \
                         function: a handler is safe to hand to another fiber only because \
                         what its operations captured is checked at this line, and a \
                         closure that arrived under a name captured it somewhere else"
                    ),
                    range,
                );
                continue;
            }
            for local in self.captures_of(*value) {
                let ty = self.unifier.zonk(self.locals.get(&local).unwrap_or(&Type::Unknown));
                if self.types.is_shareable(&ty, &self.shared_params()) {
                    continue;
                }
                let name = self.body.local(local).name.clone();
                let why = self.types.why_unshareable(&ty);
                self.error(
                    format!(
                        "`{owner}`'s `{label}` captures `{name}`, and a handler has to be safe \
                         to hand to another fiber: {why}"
                    ),
                    range,
                );
            }
        }
    }

    /// What the expression behind a handler's operation closes over.
    ///
    /// A lambda's captures are recorded; a named function has none. Anything
    /// else is a closure this expression did not create, whose captures were
    /// decided elsewhere — and "elsewhere" is exactly what cannot be checked,
    /// so it is refused by having no answer rather than by pretending to one.
    fn captures_of(&self, value: ExprId) -> Vec<khora_hir::body::LocalId> {
        match self.body.expr(value) {
            Expr::Lambda { captures, .. } => captures
                .iter()
                .copied()
                .chain(self.lambda_captures.get(&value).into_iter().flatten().copied())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether an assignment's target may be written.
    ///
    /// Lowering already rejects the targets that are wrong on their face — a
    /// literal, a call, a binding that is not `mut`. What is left is a *field*,
    /// and whether that may be written is a question about its record's
    /// declaration, which only the checker has read.
    fn check_writable(&mut self, target: ExprId, range: TextRange) {
        let Expr::Field { base, name } = self.body.expr(target).clone() else { return };
        let owner = self.infer(base);
        let owner = self.unifier.zonk(&owner);
        let Type::Adt { name: type_name, .. } = &owner else { return };
        let Some(variant) = self.types.variant_of(type_name, type_name) else { return };
        if variant.field(&name).is_none() || variant.is_mut(&name) {
            return;
        }
        self.error(
            format!(
                "cannot assign to `{name}`, which `{type_name}` does not declare `mut`"
            ),
            range,
        );
    }

    /// Records that a lambda uses a capability without naming it.
    ///
    /// Against every enclosing lambda, not just the innermost: an inner
    /// lambda reads the binding out of the outer one's frame, so the outer one
    /// has to have captured it too. The mark is what says whether a binding is
    /// outside a given lambda — below it means declared before the lambda
    /// began, which is what captured means everywhere else.
    fn note_implicit_capture(&mut self, site: ExprId, label: &str) {
        let Some(local) = self.body.capability_at(site, label) else { return };
        for (lambda, found) in &mut self.enclosing_lambdas {
            let mark = self.body.lambda_marks.get(lambda).copied().unwrap_or(0);
            if local.index() < mark && !found.contains(&local) {
                found.push(local);
            }
        }
    }

    /// Retries the projections that were waiting on their owner.
    ///
    /// Run after the body, for the same reason `check_effects` is: the fact
    /// that settles `?A` in `extract(Num::spec())` is the call's return type,
    /// and that is not known until the expression it sits in has been
    /// inferred. `docs/design/associated-items.md` decides this (D3).
    fn settle_projections(&mut self) {
        let sites = std::mem::take(&mut self.projections);
        for ((_, why), (range, context)) in self.unifier.settle().into_iter().zip(sites) {
            let Some(why) = why else { continue };
            self.error(format!("{context}: {why}"), range);
        }
    }

    /// Checks everything the body demanded against what the signature promised.
    ///
    /// Run once, after the body: a requirement is satisfied by the declaration
    /// or it is an error, and reporting it at the call that raised it is what
    /// makes the message actionable.
    fn check_effects(&mut self) {
        for Demand { fallible, clause, row, range, callee, site } in
            std::mem::take(&mut self.demanded)
        {
            let callee = as_written(&callee);

            // Zonked before anything is decided: a row recorded as a variable
            // is only now known to be anything.
            let row = self.unifier.zonk(&row);
            let empty =
                matches!(&row, Type::Row { fields, tail } if fields.is_empty() && tail.is_none());
            // Nothing left to satisfy, but possibly still something to mark:
            // a `catch` discharges the row and does not excuse the `!`.
            if empty && !fallible {
                continue;
            }
            // Satisfied means *subsumed*, not equal: a caller providing
            // `{ ledger, ai }` can call something needing only `{ ledger }`.
            // Opening the demand is that check — its labels must all be
            // present, and its fresh tail absorbs whatever the promise has
            // that this call did not ask for.
            let row = match &row {
                Type::Row { fields, tail: None } => {
                    let rest = self.unifier.fresh();
                    Type::row(fields.clone(), Some(rest))
                }
                // A bare row *variable* is a row already: `'r` means
                // `{ | 'r }`. Written out so the two shapes are one shape from
                // here on.
                Type::Param(_) => Type::row(Vec::new(), Some(row.clone())),
                other => other.clone(),
            };
            let promise = match clause {
                Clause::Requires => self.signature.requires.clone(),
                Clause::Raises => self.signature.raises.clone(),
            };

            // A call that can leave the function says so at the call site.
            // Reported before the row is compared: "mark it" is the actionable
            // half, and a marked call whose row is also wrong reports both.
            if clause == Clause::Raises {
                if let Some(site) = site {
                    if !self.marked.contains(&site) {
                        self.error(
                            format!(
                                "`{callee}` can leave this function, so the call needs `!`: \
                                 write `{callee}(..)!`"
                            ),
                            range,
                        );
                    }
                }
            }

            if empty {
                continue;
            }

            // A demand whose tail is a *rigid* variable cannot be opened —
            // there is no fresh tail to absorb what the promise has extra,
            // because the demand already stands for "whatever `'r` is". It is
            // satisfied when the promise carries the same tail and at least the
            // same labels, which is what subsumption means when neither side
            // knows what the tail holds.
            //
            // This is what a row-polymorphic library function needs the moment
            // it adds a capability of its own: `listen` promising
            // `{ 'r | scope: Scope }` and calling something needing `'r` is
            // ordinary, and unification alone reads it as `'r` being asked to
            // equal `{ scope: Scope | 'r }`.
            if self.demand_is_carried(&row, &promise) {
                continue;
            }

            if let Err(why) = self.unifier.unify(&promise, &row) {
                self.error(
                    match why {
                        unify::Mismatch::Missing { label, ty } => format!(
                            "`{callee}` needs `{}`, which this function does not {}",
                            clause.describe(&label, &ty),
                            clause.verb()
                        ),
                        other => format!("`{callee}` cannot be called here: {other}"),
                    },
                    range,
                );
            }
        }
    }

    /// Whether `promise` covers `demand` outright, tails and all.
    ///
    /// Only asked of a demand with a rigid tail, where opening it is not
    /// possible — see the caller. `false` means "not obviously", and the
    /// ordinary comparison runs and reports whatever it finds; nothing is
    /// accepted here that unification would have rejected for a reason.
    fn demand_is_carried(&mut self, demand: &Type, promise: &Type) -> bool {
        let (Type::Row { fields: wanted, tail: Some(wanted_tail) }, Type::Row { fields: held, tail: Some(held_tail) }) =
            (self.unifier.zonk(demand), self.unifier.zonk(promise))
        else {
            return false;
        };
        // Both rigid, and the same one. Two different rigid tails are two
        // different unknowns and neither covers the other.
        let (Type::Param(wanted_tail), Type::Param(held_tail)) = (*wanted_tail, *held_tail) else {
            return false;
        };
        if wanted_tail != held_tail {
            return false;
        }
        wanted.iter().all(|(label, ty)| {
            held.iter().any(|(held_label, held_ty)| {
                held_label == label && self.unifier.unify(held_ty, ty).is_ok()
            })
        })
    }

    /// `{ x: 1, y: 2 }`, or the operations of a handler.
    ///
    /// Nominal, like everything else: the literal is not a type of its own, it
    /// is *some declared record*. `handler for Ledger` says which; a bare
    /// literal is found by its labels, and having to say so when that is
    /// ambiguous is better than inventing a structural type nobody declared.
    fn infer_record(
        &mut self,
        owner: Option<String>,
        fields: &[(String, ExprId)],
        range: TextRange,
    ) -> Type {
        let written: Vec<&str> = fields.iter().map(|(l, _)| l.as_str()).collect();

        let candidates: Vec<VariantInfo> = match &owner {
            Some(name) => self
                .types
                .variants
                .iter()
                .filter(|v| &v.type_name == name && v.name == *name)
                .cloned()
                .collect(),
            None => {
                let record = |exact: bool| -> Vec<VariantInfo> {
                    self.types
                        .variants
                        .iter()
                        .filter(|v| v.name == v.type_name)
                        .filter(|v| {
                            if exact {
                                covers(&v.labels, &written)
                            } else {
                                // A literal short of a field still names its
                                // record. Saying which field is missing beats
                                // saying no type has these fields.
                                written.iter().all(|w| v.labels.iter().any(|l| l == w))
                            }
                        })
                        .cloned()
                        .collect()
                };
                match record(true) {
                    found if !found.is_empty() => found,
                    _ => record(false),
                }
            }
        };

        let record = match candidates.as_slice() {
            [only] => only.clone(),
            [] => {
                for (_, value) in fields {
                    self.infer(*value);
                }
                self.error(
                    match &owner {
                        Some(name) => format!("`{name}` is not a record type"),
                        None => format!(
                            "no record type has exactly the fields {}",
                            written
                                .iter()
                                .map(|l| format!("`{l}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    },
                    range,
                );
                return Type::Unknown;
            }
            several => {
                for (_, value) in fields {
                    self.infer(*value);
                }
                let names: Vec<String> =
                    several.iter().map(|v| format!("`{}`", v.type_name)).collect();
                self.error(
                    format!(
                        "these fields fit {} — say which with `handler for ..`, or annotate it",
                        names.join(" and ")
                    ),
                    range,
                );
                return Type::Unknown;
            }
        };

        // Field types are declared against the record's own parameters, so the
        // literal decides them: `{ value: 1 }` for `Wrapper<A>` is `Wrapper<Int>`.
        let (whole, mapping) = self.instantiate_adt(&record.type_name);
        let borrowed: HashMap<&str, Type> =
            mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();

        for (label, value) in fields {
            match record.field(label) {
                Some((_, declared)) => {
                    let declared = unify::substitute(declared, &borrowed);
                    self.expect(*value, &declared, &format!("field `{label}`"));
                }
                None => {
                    self.infer(*value);
                    let range = self.body.range(*value);
                    self.error(
                        format!("`{}` has no field `{label}`", record.type_name),
                        range,
                    );
                }
            }
        }
        for label in &record.labels {
            if !written.iter().any(|w| w == label) {
                self.error(
                    format!("this `{}` is missing `{label}`", record.type_name),
                    range,
                );
            }
        }
        // A handler is the one place a capability's closures are visible, and
        // an effect's shareability is paid for by asking here.
        if self.types.effects.contains(&record.type_name) {
            let owner = record.type_name.clone();
            self.check_handler_is_shareable(&owner, fields);
        }
        whole
    }

    /// The position and type of `label` on a record, at this instantiation.
    ///
    /// A record's fields are declared against the type's own parameters, so
    /// they have to be read at the value's arguments: `Pair<Int>.first` is
    /// `Int`, not `A`.
    fn record_field(&mut self, owner: &Type, label: &str) -> Option<(usize, Type)> {
        let Type::Adt { name, .. } = owner else { return None };
        // A *record* — `type Point = { x: Int }` — whose one variant carries
        // the type's own name. `type User = | Of(age: Int)` is a sum that
        // happens to have one case, and its payload is reached by matching:
        // `Of` is a constructor, not a field, and the two must not blur.
        let record = self
            .types
            .variants
            .iter()
            .find(|v| &v.type_name == name && v.name == v.type_name)?;
        let (index, declared) = record.field(label).map(|(i, t)| (i, t.clone()))?;

        let mapping = self.substitution_for(name, owner);
        let borrowed: HashMap<&str, Type> =
            mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        Some((index, unify::substitute(&declared, &borrowed)))
    }

    fn signature_for(&self, key: &str, _self_ty: &Type) -> Option<Signature> {
        self.types.signatures.get(key).cloned()
    }

    /// The type parameters this function declared `Share` for.
    fn shared_params(&self) -> Vec<String> {
        self.signature
            .generics
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                self.signature.bounds.get(*i).is_some_and(|b| b.iter().any(|t| t == SHARE))
            })
            .map(|(_, g)| g.clone())
            .collect()
    }

    /// The traits the enclosing function requires of `param`.
    fn bounds_on(&self, param: &str) -> Vec<String> {
        self.signature
            .generics
            .iter()
            .position(|g| g == param)
            .and_then(|i| self.signature.bounds.get(i))
            .cloned()
            .unwrap_or_default()
    }

    /// Reports every trait bound this body left unsatisfied.
    ///
    /// Runs after inference rather than during it: a bound is a question about
    /// a *solved* type argument, and asking it while the argument is still a
    /// variable would report whichever call happened to be visited first.
    fn check_bounds(&mut self) {
        let mentions: Vec<(ExprId, String, Vec<Type>)> = self
            .instantiations
            .iter()
            .map(|(id, (name, args))| (*id, name.clone(), args.clone()))
            .collect();

        for (id, name, args) in mentions {
            let Some(signature) = self.types.signatures.get(name.as_str()) else { continue };
            let bounds = signature.bounds.clone();
            let range = self.body.range(id);

            for (arg, required) in args.iter().zip(&bounds) {
                let arg = self.unifier.zonk(arg);
                for wanted in required {
                    // A trait that does not exist is reported where it is
                    // written, not once per use of the function.
                    if !self.types.traits.traits.contains_key(wanted) {
                        continue;
                    }
                    if !self.satisfies(wanted, &arg) {
                        self.error(
                            format!("`{arg}` does not implement `{wanted}`, which `{name}` requires"),
                            range,
                        );
                    }
                }
            }
        }
    }

    /// Whether `ty` implements `wanted`, here in this body.
    ///
    /// A rigid parameter has no impl to find: what it satisfies is whatever the
    /// enclosing signature promised about it, which is why this is a method on
    /// the checker rather than on `Traits`.
    fn satisfies(&self, wanted: &str, ty: &Type) -> bool {
        // `Share` is answered by looking, not by finding an impl. A record of
        // immutable fields is safe for two fibers whether or not anybody wrote
        // it down, and requiring the impl would mean writing one for every
        // type that ever crosses — which is the tax `Send`/`Sync` avoid by
        // being derived. The impl still matters for the types this cannot see
        // into; `TypeMap::is_shareable` is what asks for it there.
        if wanted == SHARE {
            return self.types.is_shareable(ty, &self.shared_params());
        }
        match ty {
            // Not solved, or downstream of an error already reported.
            Type::Unknown | Type::Var(_) | Type::Never => true,
            Type::Param(p) => {
                let declared = self.bounds_on(p);
                traits::with_supertraits(&self.types.traits, &declared)
                    .iter()
                    .any(|t| t == wanted)
            }
            other => self.types.traits.satisfies(wanted, other),
        }
    }

    /// The type of `Owner::name`, where `Owner` is a trait or a bounded type
    /// parameter and `name` is one of the trait's functions.
    ///
    /// `Self` is left as a fresh variable when the owner is a trait, so the
    /// expected type decides which impl runs — `Applicative::pure(x)` in a
    /// position wanting `Option<Int>` resolves to `Option`'s. When the owner is
    /// a type parameter, `Self` is that parameter and the choice is the
    /// caller's.
    fn type_of_trait_item(&mut self, at: ExprId, owner: &str, name: &str) -> Type {
        // A type's own function comes first, for the same reason its own
        // method beats a trait's: adding a trait must not silently change what
        // an existing call does.
        // `Type::adt` for a builtin gives an ADT that shares its name, which
        // is all `inherent_method` looks at — it compares head constructors,
        // and `Int`'s is `Int` however the type was spelled.
        let self_ty = match owner {
            "Int" | "I64" => Type::Int,
            "Float" => Type::Float,
            "Bool" => Type::Bool,
            "String" => Type::Str,
            "Ptr" => Type::Ptr,
            other => match IntKind::parse(other) {
                Some(kind) => Type::Fixed(kind),
                None => Type::adt(other),
            },
        };
        if let Some(own) = self.types.traits.inherent_method(&self_ty, name) {
            let key = traits::method_key("", &own.head, name);
            let Some(signature) = self.types.signatures.get(key.as_str()).cloned() else {
                return Type::Unknown;
            };
            // No demand here: the rows are in the type now, and are charged
            // where the function is *called* rather than where it is named.
            let (ty, type_args) =
                self.unifier.instantiate_with(&signature.generics, &signature.as_fn());
            self.instantiations.insert(at, (key, type_args));
            return ty;
        }

        // `Num::spec()` where `spec` belongs to a trait `Num` implements. The
        // owner names the *impl* rather than the trait, which is the reading a
        // caller with a concrete type in hand wants: they know what they have,
        // not which trait declared the function.
        if self.types.adts.contains_key(owner) {
            let found = self.types.traits.impls.iter().find(|i| {
                traits::head_of(&i.self_type).as_deref() == Some(owner)
                    && i.methods.iter().any(|m| m == name)
            });
            if let Some(chosen) = found {
                let key = traits::method_key(&chosen.trait_name, owner, name);
                let Some(signature) = self.types.signatures.get(key.as_str()).cloned() else {
                    return Type::Unknown;
                };
                let (ty, type_args) =
                    self.unifier.instantiate_with(&signature.generics, &signature.as_fn());
                self.instantiations.insert(at, (key, type_args));
                return ty;
            }
        }

        let bounds = self.bounds_on(owner);
        let candidates: Vec<String> = if bounds.is_empty() {
            vec![owner.to_string()]
        } else {
            traits::with_supertraits(&self.types.traits, &bounds)
        };

        let found = candidates.iter().find_map(|t| {
            let def = self.types.traits.traits.get(t)?;
            def.method(name).map(|_| t.clone())
        });
        let Some(trait_name) = found else {
            let range = self.body.range(at);
            self.error(
                if self.types.adts.contains_key(owner) {
                    // `Fruit::Red` where `Red` is `Color`'s is the common way
                    // to get here, and naming the type that does have it is
                    // the whole of the fix.
                    match self.types.variants.iter().find(|v| v.name == name) {
                        Some(elsewhere) => format!(
                            "`{owner}` has no `{name}`; `{}::{name}` is `{}`'s",
                            elsewhere.type_name, elsewhere.type_name
                        ),
                        None => format!(
                            "`{owner}` has no constructor or function named `{name}`"
                        ),
                    }
                } else if bounds.is_empty() {
                    format!("`{owner}` is not a trait with a function named `{name}`")
                } else {
                    format!(
                        "no function `{name}` on `{owner}`, whose bounds are `{}`",
                        bounds.join("` + `")
                    )
                },
                range,
            );
            return Type::Unknown;
        };

        let key = format!("{trait_name}::{name}");
        let Some(signature) = self.types.signatures.get(key.as_str()).cloned() else {
            return Type::Unknown;
        };
        let (ty, type_args) =
            self.unifier.instantiate_with(&signature.generics, &signature.as_fn());

        // A type parameter names itself as `Self`; a trait leaves it open for
        // the surrounding expression to decide.
        if !bounds.is_empty() {
            if let Some(chosen) = type_args.first() {
                let _ = self.unifier.unify(chosen, &Type::Param(owner.to_string()));
            }
        }
        self.instantiations.insert(at, (key, type_args));
        ty
    }

    fn type_of_resolution(&mut self, at: ExprId, resolution: &khora_hir::Resolution) -> Type {
        match resolution {
            khora_hir::Resolution::TraitItem { owner, name } => {
                let (owner, name) = (owner.clone(), name.clone());
                self.type_of_trait_item(at, &owner, &name)
            }
            khora_hir::Resolution::Item { name, .. } => {
                // Each mention gets its own copy of the signature, so two calls
                // to the same generic function do not constrain each other.
                match self.types.signatures.get(name).cloned() {
                    Some(sig) => {
                        let (ty, args) =
                            self.unifier.instantiate_with(&sig.generics, &sig.as_fn());
                        self.instantiations.insert(at, (name.clone(), args));
                        ty
                    }
                    None => Type::Unknown,
                }
            }
            khora_hir::Resolution::Variant { type_name, name, .. } => {
                // A nullary constructor is a value; one with a payload is
                // reached through a call, handled in `infer_call`.
                match self.types.variant_of(type_name, name) {
                    Some(_) => self.instantiate_adt(type_name).0,
                    None => Type::Unknown,
                }
            }
            khora_hir::Resolution::Unsupported(_) => Type::Unknown,
        }
    }

    fn infer_match(
        &mut self,
        scrutinee: ExprId,
        arms: &[khora_hir::body::MatchArm],
        range: TextRange,
    ) -> Type {
        let scrutinee_ty = self.infer(scrutinee);

        let mut result: Option<Type> = None;
        for arm in arms {
            self.bind_pattern(arm.pat, &scrutinee_ty);
            if let Some(guard) = arm.guard {
                self.expect(guard, &Type::Bool, "a match guard");
            }
            let arm_ty = self.infer(arm.body);
            match result.clone() {
                None => result = Some(arm_ty),
                Some(expected) => {
                    let range = self.body.range(arm.body);
                    if self.require(&expected, &arm_ty, "match arms disagree", range) {
                        if matches!(expected, Type::Never) {
                            result = Some(arm_ty);
                        }
                    } else {
                        result = Some(Type::Unknown);
                    }
                }
            }
        }

        self.check_match_coverage(&scrutinee_ty, arms, range);
        result.unwrap_or(Type::Unknown)
    }

    /// `f()! catch { .. }` — handles part of the error row.
    ///
    /// The subtraction is by error *type*, named by the arms' constructors. So
    /// this is not a `match` on a result: the arms do not see a value the
    /// operand produced, they see the error it left with, and the ones they
    /// name stop being the enclosing function's problem.
    ///
    /// **A `_` arm subtracts the whole row, including its tail.** That is the
    /// one thing naming constructors cannot express, and a general-purpose
    /// language needs it: a supervisor — a server answering a request, a queue
    /// running a job — has to recover from work whose failures it does not know
    /// the shape of, because they are the *caller's* choice. Every neighbour
    /// has the form (`catch_unwind`, `recover`, `catchAll`); this one is
    /// checked rather than dynamic, and it costs what it should — the arm
    /// learns nothing about what went wrong, since there is no name to learn
    /// it under. Name the constructors when they are known; `_` is for when
    /// they cannot be.
    fn infer_catch(
        &mut self,
        inner: ExprId,
        arms: &[khora_hir::body::MatchArm],
        range: TextRange,
    ) -> Type {
        // Demands raised *inside* the operand are the ones this `catch` is in
        // a position to handle. Remembering where the list stood draws that
        // window: a demand from an enclosing expression is not in it, and a
        // nested `catch` has already narrowed its own.
        let before = self.demanded.len();
        let value = self.infer(inner);

        // Each arm is matched against its own error type rather than against
        // one scrutinee, which is the other way this differs from `match`.
        let mut caught: Vec<String> = Vec::new();
        let mut result: Option<Type> = None;
        // Whether an arm handles what the named ones did not.
        let mut everything = false;
        for arm in arms {
            let owner = match self.body.pat(arm.pat) {
                Pat::Path(r) | Pat::TupleStruct { resolution: r, .. } => variant_case(r).map(|(t, _)| t),
                _ => None,
            };
            if owner.is_none() && matches!(self.body.pat(arm.pat), Pat::Wildcard) {
                everything = true;
                if let Some(guard) = arm.guard {
                    self.expect(guard, &Type::Bool, "a match guard");
                }
                let arm_ty = self.infer(arm.body);
                match result.clone() {
                    None => result = Some(arm_ty),
                    Some(expected) => {
                        let at = self.body.range(arm.body);
                        if self.require(&expected, &arm_ty, "catch arms disagree", at) {
                            if matches!(expected, Type::Never) {
                                result = Some(arm_ty);
                            }
                        } else {
                            result = Some(Type::Unknown);
                        }
                    }
                }
                continue;
            }
            let Some(owner) = owner else {
                // Silent when the pattern named a constructor that did not
                // resolve: that is already reported, and saying it twice buries
                // the message that can actually be acted on.
                if !matches!(
                    self.body.pat(arm.pat),
                    Pat::Path(_) | Pat::TupleStruct { .. } | Pat::Missing
                ) {
                    self.error(
                        "a `catch` arm has to name an error constructor, since it is the \
                         constructor's type that says which errors are handled here"
                            .to_string(),
                        self.body.range(arm.body),
                    );
                }
                continue;
            };
            if !caught.contains(&owner) {
                caught.push(owner.clone());
            }
            self.bind_pattern(arm.pat, &Type::adt(&owner));
            if let Some(guard) = arm.guard {
                self.expect(guard, &Type::Bool, "a match guard");
            }
            let arm_ty = self.infer(arm.body);
            match result.clone() {
                None => result = Some(arm_ty),
                Some(expected) => {
                    let range = self.body.range(arm.body);
                    if self.require(&expected, &arm_ty, "catch arms disagree", range) {
                        if matches!(expected, Type::Never) {
                            result = Some(arm_ty);
                        }
                    } else {
                        result = Some(Type::Unknown);
                    }
                }
            }
        }

        // Naming a type commits to all of it. A partially handled type would
        // have to stay in the row *and* divert some of its variants, so the
        // signature would say it can still leave while the reader sees it
        // handled — the subtraction is only honest if it is total.
        //
        // Unless a `_` arm is there to take the rest, which is what makes
        // `catch { NotFound => .., _ => .. }` the ordinary shape it looks like.
        for owner in caught.iter().filter(|_| !everything) {
            let mine: Vec<khora_hir::body::MatchArm> = arms
                .iter()
                .filter(|a| {
                    matches!(self.body.pat(a.pat),
                        Pat::Path(r) | Pat::TupleStruct { resolution: r, .. }
                            if variant_case(r).is_some_and(|(t, _)| &t == owner))
                })
                .cloned()
                .collect();
            self.check_match_coverage(&Type::adt(owner), &mine, range);
        }

        // The bodies stand in for the operand's value, so the whole expression
        // has one type whichever way it went.
        if let Some(handled) = result.clone() {
            self.require(&value, &handled, "a `catch` arm", range);
        }

        // Subtract. The demand stays even when nothing is left of its row: it
        // is also what checks that the call wore its `!`, and a `catch` does
        // not excuse the mark — control still leaves the operand.
        let window: Vec<Demand> = self.demanded.split_off(before);
        let mut names = Vec::new();
        let kept: Vec<Demand> = window
            .into_iter()
            .map(|mut demand| {
                if demand.clause == Clause::Raises {
                    if let Type::Row { fields, tail } = &demand.row {
                        names.extend(fields.iter().map(|(l, _)| l.clone()));
                        if everything {
                            // Tail and all: that is the difference between this
                            // and any number of named arms.
                            demand.row = Type::row(Vec::new(), None);
                        } else {
                            let left: Vec<(String, Type)> = fields
                                .iter()
                                .filter(|(l, _)| !caught.contains(l))
                                .cloned()
                                .collect();
                            demand.row = Type::row(left, tail.as_deref().cloned());
                        }
                    }
                }
                demand
            })
            .collect();
        self.demanded.extend(kept);

        for owner in &caught {
            if !names.contains(owner) {
                self.error(
                    format!("nothing in this expression raises `{owner}`"),
                    range,
                );
            }
        }

        value
    }

    fn check_match_coverage(
        &mut self,
        scrutinee_ty: &Type,
        arms: &[khora_hir::body::MatchArm],
        range: TextRange,
    ) {
        // A guard can fail, so a guarded arm covers nothing for the purposes of
        // exhaustiveness. Excluding them keeps the check sound.
        let unguarded: Vec<&khora_hir::body::MatchArm> =
            arms.iter().filter(|a| a.guard.is_none()).collect();
        let patterns: Vec<Pattern> =
            unguarded.iter().map(|a| self.to_pattern(a.pat)).collect();

        let column = column_type(self.types, scrutinee_ty);
        if matches!(column, ColumnType::Unknown) {
            return;
        }

        // Named types are expanded lazily: an ADT may contain itself, so
        // resolving eagerly would not terminate.
        // Named types expand lazily: an ADT may contain itself, so resolving
        // eagerly would not terminate. Captures the map, not the checker, so
        // reporting can still borrow `self` mutably.
        let types = self.types;
        let resolve = move |name: &str| -> ColumnType {
            let ty =
                if name == BOOL_TYPE { Type::Bool } else { Type::adt(name) };
            column_type(types, &ty)
        };

        let missing = usefulness::missing_patterns(&patterns, &column, &resolve);
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|p| p.to_string()).collect();
            self.error(
                format!("this `match` is not exhaustive: pattern `{}` not covered", names.join("`, `")),
                range,
            );
        }

        for index in usefulness::unreachable_arms(&patterns, &column, &resolve) {
            if let Some(arm) = unguarded.get(index) {
                self.error("this arm is unreachable", self.body.range(arm.body));
            }
        }
    }

    /// A constructor carrying the types of its payload, so specialization can
    fn to_pattern(&self, pat: PatId) -> Pattern {
        match self.body.pat(pat) {
            // A binding matches everything, exactly like `_`.
            Pat::Wildcard | Pat::Bind(_) | Pat::Missing => Pattern::Wildcard,
            Pat::Literal(lit) => Pattern::Constructor {
                ctor: match lit {
                    Literal::Bool(b) => Ctor::Bool(*b),
                    Literal::Int(n) => Ctor::Literal(n.clone()),
                    Literal::Float(n) => Ctor::Literal(n.clone()),
                    Literal::Str(s) => Ctor::Literal(format!("\"{s}\"")),
                },
                fields: Vec::new(),
            },
            Pat::Path(resolution) | Pat::TupleStruct { resolution, .. } => {
                let sub = match self.body.pat(pat) {
                    Pat::TupleStruct { fields, .. } => {
                        fields.iter().map(|f| self.to_pattern(*f)).collect()
                    }
                    _ => Vec::new(),
                };
                match variant_case(resolution).and_then(|(t, n)| self.types.variant_of(&t, &n)) {
                    Some(v) => Pattern::Constructor { ctor: ctor_for(self.types, v), fields: sub },
                    None => Pattern::Wildcard,
                }
            }
            Pat::Tuple(fields) => Pattern::Constructor {
                ctor: Ctor::Tuple(fields.len()),
                fields: fields.iter().map(|f| self.to_pattern(*f)).collect(),
            },
        }
    }
}

/// expand nested patterns to the right column types.
fn ctor_for(_types: &TypeMap, variant: &VariantInfo) -> Ctor {
    Ctor::Variant {
        name: variant.name.clone(),
        fields: variant.fields.iter().map(field_type).collect(),
    }
}

fn field_type(ty: &Type) -> FieldType {
    match ty {
        Type::Adt { name, .. } => FieldType::Named(name.clone()),
        Type::Bool => FieldType::Named(BOOL_TYPE.to_string()),
        Type::Int | Type::Str => FieldType::Unbounded,
        _ => FieldType::Opaque,
    }
}

fn column_type(types: &TypeMap, ty: &Type) -> ColumnType {
    match ty {
        Type::Bool => ColumnType::Finite(vec![Ctor::Bool(true), Ctor::Bool(false)]),
        Type::Int | Type::Str => ColumnType::Unbounded,
        Type::Adt { name, .. } => {
            let variants = types.variants_of(name);
            if variants.is_empty() {
                ColumnType::Unknown
            } else {
                ColumnType::Finite(
                    variants.iter().map(|v| ctor_for(types, v)).collect(),
                )
            }
        }
        _ => ColumnType::Unknown,
    }
}


/// `Bool` has constructors but is not an ADT, so the resolver needs a name for
/// it. Lowercase, which no declared type can be.
const BOOL_TYPE: &str = "bool";


/// The type a constructor belongs to, and the constructor's own name.
///
/// Always prefer this to [`variant_name`] when looking a constructor up: the
/// name alone is ambiguous across types.
fn variant_case(resolution: &khora_hir::Resolution) -> Option<(String, String)> {
    match resolution {
        khora_hir::Resolution::Variant { type_name, name, .. } => {
            Some((type_name.clone(), name.clone()))
        }
        _ => None,
    }
}

/// Every semantic diagnostic for one file: name resolution and lowering
/// errors from `khora-hir`, then type errors.
///
/// Lowering errors come first because a name that did not resolve makes the
/// type error that follows it noise.
#[salsa::tracked(returns(ref))]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let mut all: Vec<HirError> = khora_hir::item_map(db, file).errors.clone();
    // An import that resolved to nothing is the most useful thing to say about
    // a file full of "cannot find" errors downstream of it.
    all.extend(khora_hir::file_scope(db, file).errors.iter().cloned());
    for (_, body) in khora_hir::body::bodies(db, file) {
        all.extend(body.errors.iter().cloned());
    }
    all.extend(trait_errors(db, file).iter().cloned());
    all.extend(check_file(db, file).iter().cloned());
    all
}

/// Whether `==` on this type has to go through an `Eq` impl.
///
/// The scalars compare with one instruction and `String` by its bytes, so those
/// are primitive. Everything with a shape needs a decision about what equality
/// *means* for it, and the type is the only thing that can make it.
///
/// A type still being inferred is not asked: whatever it turns out to be, the
/// question is answered where it is answered, and guessing here would report
/// against whichever expression happened to be visited first.
fn needs_an_eq_impl(ty: &Type) -> bool {
    !matches!(
        ty,
        Type::Int
            | Type::Fixed(_)
            | Type::Float
            | Type::Bool
            | Type::Str
            | Type::Unit
            | Type::Var(_)
            | Type::Never
            | Type::Unknown
    )
}

/// Whether `<` on this type has to go through an `Ord` impl.
///
/// Nearly [`needs_an_eq_impl`], and `String` is the difference. Two strings
/// compare for *equality* by their bytes, which is one runtime call and the
/// only answer anybody wants; which of them comes *first* is a different
/// question with several defensible answers — bytes, code points, a locale —
/// and the one a program means belongs in an impl it can read.
fn needs_an_ord_impl(ty: &Type) -> bool {
    matches!(ty, Type::Str) || needs_an_eq_impl(ty)
}
