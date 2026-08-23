//! `derive(Eq, Ord, Show, Hash, ToJson, FromJson)`, expanded into ordinary
//! impls.
//!
//! # Why this is a source-to-source expansion
//!
//! An `impl` written by hand and an `impl` the compiler wrote for you are the
//! same thing, and the cheapest way to be sure of that is to make them *be* the
//! same thing. So this pass produces Khora, parses it, and hands the resulting
//! declarations to the same collectors that read the file itself: the checker
//! type-checks a derived method exactly as it would a written one, the
//! usefulness pass sees its `match` arms, monomorphization instantiates it, and
//! the backend never learns that `derive` exists.
//!
//! The alternative — a `Derived` flag on `ImplDef` and a synthesized body in
//! codegen — needs every pass between here and LLVM to grow a second path that
//! nobody writing an ordinary impl exercises. That is where the bugs would
//! live: a derived `Eq` that reference-counts its fields differently from a
//! written one is not something a type test would catch.
//!
//! What it costs is that the generated text has ranges of its own, belonging to
//! no file. That is handled by *blaming the `derive`*: every range in a derived
//! impl is rewritten to the range of the `derive(..)` clause that asked for it,
//! so a diagnostic can only ever point at something the author wrote. It is
//! also the right place to point. A derived impl that does not check is either
//! the author's fault — a field whose type lacks the trait — or the compiler's,
//! and in both cases the `derive` line is where the reader has to start.
//!
//! # Why the check for "this field is not `Eq`" is not here
//!
//! It needs to know which types implement which traits, and that lives in
//! `khora-types`, which depends on this crate rather than the other way round.
//! So this pass generates unconditionally and `khora_types::derive_report`
//! reports; a refused derive still contributes its impl, so a use site says
//! nothing about a `Point` that "does not implement `Eq`" when the real answer
//! is already on screen.

use khora_db::{Db, SourceFile};
use khora_syntax::ast::{self, AstNode};
use text_size::TextRange;

use crate::HirError;

/// The traits the compiler knows how to write.
///
/// Six, and not extensible. Every one of them is *structural* — the answer is
/// determined by the fields and nothing else — which is the property that makes
/// generating the code better than writing it. A trait whose implementation is
/// a decision (`Default`, `Iterator`) has nothing here to generate.
pub const DERIVABLE: [&str; 6] = ["Eq", "Ord", "Show", "Hash", "ToJson", "FromJson"];

/// The prime the derived `Hash` reduces by at every step.
///
/// Khora's `*` and `+` **trap on overflow**, so the usual `hash * 31 + next`
/// accumulator is a runtime abort waiting for a large enough field: `Hash for
/// Int` is the identity, so a record holding one big number is enough to reach
/// it. Reducing both operands first bounds the accumulator at about 3.2e7 and
/// makes the trap unreachable by construction.
///
/// The mixing this gives up is not missed. `std::core` says as much where `Hash
/// for Int` hands back the number unchanged: spreading a hash over buckets is
/// the map's job, and all this has to be is *consistent*.
const HASH_MODULUS: i64 = 1_000_003;

/// One impl this pass wrote, and the `derive` that asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedImpl {
    pub trait_name: String,
    /// The type as declared: `Point`, or `Box` for `Box<A>`.
    pub type_name: String,
    /// Where the `derive(..)` clause is, in the *file*. Everything a derived
    /// impl can be blamed for is blamed here.
    pub at: TextRange,
    /// Whether the type is a record. Whoever reads this does not otherwise
    /// have the declaration to hand, and "the field `x`" and "field 0 of
    /// `Circle`" are the two ways a diagnostic has to name where it went
    /// wrong.
    pub is_record: bool,
}

impl DerivedImpl {
    /// The key this impl's method body is recorded under: `Eq#Point::eq`.
    ///
    /// Each derivable trait declares exactly one function, which is why one
    /// impl has one key. Spelled the way `khora_hir::body::impl_key` spells
    /// it, because it *is* that key: the body was lowered by the same code.
    pub fn body_key(&self) -> String {
        format!(
            "{}#{}::{}",
            self.trait_name,
            self.type_name,
            method_of(&self.trait_name)
        )
    }
}

/// The one function each derivable trait declares.
///
/// Written down here rather than read off the trait, because the question is
/// asked before anything has looked at a trait declaration — and because a
/// `derive` that wrote a different set of functions than the trait declares
/// would be a bug in this expander rather than something to discover at run
/// time.
pub fn method_of(trait_name: &str) -> &'static str {
    match trait_name {
        "Eq" => "eq",
        "Ord" => "cmp",
        "Show" => "show",
        "Hash" => "hash",
        "ToJson" => "to_json",
        "FromJson" => "from_json",
        _ => "",
    }
}

/// Every impl a file's `derive` clauses expand to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    text: String,
    parse: khora_syntax::Parse,
    /// One per generated impl, in the order they appear in [`Derived::text`].
    pub impls: Vec<DerivedImpl>,
    /// What was wrong with the `derive` clauses themselves — a trait nobody can
    /// derive, a type with no fields to look at. Everything that needs to know
    /// what a trait *is* is reported by `khora_types::derive_report` instead.
    pub errors: Vec<HirError>,
}

impl Derived {
    /// The Khora this pass wrote. Only tests and `khora explain` want it, but
    /// a generated program nobody can read is a generated program nobody can
    /// debug.
    pub fn source(&self) -> &str {
        &self.text
    }

    pub fn source_file(&self) -> ast::SourceFile {
        self.parse.source_file()
    }

    /// Each generated impl paired with the `derive` it came from.
    ///
    /// Matched by position rather than by name: one type may derive four
    /// traits, and two types may derive the same one, so neither half of
    /// `(trait, type)` identifies a clause on its own. The pairing holds
    /// because [`expand`] appends to `text` and to `impls` together.
    pub fn declarations(&self) -> Vec<(ast::ImplDecl, &DerivedImpl)> {
        let written: Vec<ast::ImplDecl> = self
            .source_file()
            .decls()
            .filter_map(|d| match d {
                ast::Decl::Impl(i) => Some(i),
                _ => None,
            })
            .collect();
        debug_assert_eq!(
            written.len(),
            self.impls.len(),
            "the generated source and the record of what was generated disagree:\n{}",
            self.text
        );
        written.into_iter().zip(self.impls.iter()).collect()
    }
}

/// Expands the `derive` clauses of one file.
#[salsa::tracked(returns(ref))]
pub fn derived(db: &dyn Db, file: SourceFile) -> Derived {
    expand(&khora_db::parse(db, file).source_file())
}

/// What a type holds, which is all a structural derive needs to know.
enum Shape {
    /// A record's fields, in declaration order — which is also comparison
    /// order, because that is what every language does and what someone
    /// reordering two fields expects to change.
    Record(Vec<String>, Vec<String>),
    /// A variant's cases, in declaration order — which decides which one is
    /// `Less`, again because that is the only ordering the reader can see.
    Variant(Vec<Case>),
}

struct Case {
    name: String,
    /// How many values the case carries. Named and positional payloads are
    /// alike here: Khora builds and matches both positionally, so a derive has
    /// no reason to tell them apart.
    arity: usize,
    /// Payload types as the author wrote them. JSON decoding uses these in
    /// generated annotations so the checker never has to infer a generic
    /// decoder's result backwards through a constructor.
    field_types: Vec<String>,
}

/// Expands every `derive` in a source file. Pure, so it can be tested without
/// a database.
pub fn expand(source: &ast::SourceFile) -> Derived {
    let mut text = String::new();
    let mut impls = Vec::new();
    let mut errors = Vec::new();

    for decl in source.decls() {
        let ast::Decl::Type(t) = decl else { continue };
        let Some(clause) = t.derive_clause() else {
            continue;
        };
        let at = clause.syntax().text_range();
        let Some(type_name) = t.name().and_then(|n| n.ident()) else {
            continue;
        };

        let mut wanted: Vec<String> = Vec::new();
        for named in clause.traits() {
            let Some(name) = named.ident() else { continue };
            if !DERIVABLE.contains(&name.as_str()) {
                errors.push(HirError {
                    message: format!(
                        "`{name}` cannot be derived. The compiler can write `{}`, \
                         because each of those is decided by the fields and nothing \
                         else; anything else has to be written out",
                        DERIVABLE.join("`, `")
                    ),
                    range: at,
                });
                continue;
            }
            if wanted.contains(&name) {
                errors.push(HirError {
                    message: format!("`{name}` is named twice in this `derive`"),
                    range: at,
                });
                continue;
            }
            wanted.push(name);
        }
        if wanted.is_empty() {
            continue;
        }

        let Some(shape) = shape_of(&t, &type_name, at, &mut errors) else {
            continue;
        };
        let Some(params) = parameters_of(&t, &type_name, at, &mut errors) else {
            continue;
        };

        let is_record = matches!(shape, Shape::Record(_, _));
        for trait_name in wanted {
            text.push_str(&write_impl(&trait_name, &type_name, &params, &shape));
            text.push('\n');
            impls.push(DerivedImpl {
                trait_name,
                type_name: type_name.clone(),
                at,
                is_record,
            });
        }
    }

    let parse = khora_syntax::parse(&text);
    debug_assert!(
        parse.errors().is_empty(),
        "the derive expander wrote Khora that does not parse: {:?}\n{text}",
        parse.errors()
    );
    // Unreachable: every name interpolated above came out of a tree that
    // already parsed, and the rest is a template. If it is reached anyway, the
    // impls are holes in the shape of impls, and compiling them would turn a
    // bug in this file into wrong code somewhere else. Withdrawing the lot and
    // saying so keeps the blast radius at the `derive`.
    if !parse.errors().is_empty() {
        errors.push(HirError {
            message: format!(
                "the compiler wrote an impl for this `derive` that it cannot itself \
                 parse; that is a compiler bug. What it wrote:\n{text}"
            ),
            range: impls.first().map_or(TextRange::empty(0.into()), |i| i.at),
        });
        return Derived {
            text: String::new(),
            parse: khora_syntax::parse(""),
            impls: Vec::new(),
            errors,
        };
    }
    Derived {
        text,
        parse,
        impls,
        errors,
    }
}

/// What the declaration holds, or `None` with the reason recorded.
fn shape_of(
    t: &ast::TypeDecl,
    type_name: &str,
    at: TextRange,
    errors: &mut Vec<HirError>,
) -> Option<Shape> {
    let refuse = |errors: &mut Vec<HirError>, why: &str| {
        errors.push(HirError {
            message: format!(
                "nothing can be derived for `{type_name}`: {why}. A derive is written \
                 from the fields, so there has to be a list of them to read"
            ),
            range: at,
        });
        None
    };

    match t.definition() {
        Some(ast::Type::Record(r)) => {
            if r.row_tail().is_some() {
                return refuse(
                    errors,
                    "its row is open, so what it holds is not fixed here",
                );
            }
            let mut fields = Vec::new();
            let mut field_types = Vec::new();
            for field in r.fields() {
                let Some(name) = field.name().and_then(|n| n.ident()) else {
                    continue;
                };
                let Some(ty) = field.ty() else {
                    continue;
                };
                fields.push(name);
                field_types.push(ty.syntax().text().to_string());
            }
            Some(Shape::Record(fields, field_types))
        }
        Some(ast::Type::Variant(v)) => {
            let mut cases = Vec::new();
            for case in v.cases() {
                let Some(name) = case.name().and_then(|n| n.ident()) else {
                    continue;
                };
                let field_types: Vec<String> = if let Some(list) = case.fields() {
                    list.fields()
                        .filter_map(|field| field.ty())
                        .map(|ty| ty.syntax().text().to_string())
                        .collect()
                } else if let Some(list) = case.tuple_fields() {
                    list.types()
                        .map(|ty| ty.syntax().text().to_string())
                        .collect()
                } else {
                    Vec::new()
                };
                cases.push(Case {
                    name,
                    arity: field_types.len(),
                    field_types,
                });
            }
            if cases.is_empty() {
                return refuse(errors, "it has no cases");
            }
            Some(Shape::Variant(cases))
        }
        None => refuse(errors, "it is declared with no body"),
        Some(_) => refuse(errors, "it is not a record or a variant type"),
    }
}

/// The type's parameters, or `None` when one of them is a kind this cannot
/// carry into an impl header.
///
/// Const generics and row variables are refused rather than guessed at. Both
/// would need the impl to restate something the declaration wrote in a
/// different notation — `impl<const N: Int> Eq for Vector<N>` — and getting it
/// subtly wrong produces an impl for a *different* type than the one that was
/// derived, which is worse than saying no.
fn parameters_of(
    t: &ast::TypeDecl,
    type_name: &str,
    at: TextRange,
    errors: &mut Vec<HirError>,
) -> Option<Vec<String>> {
    let Some(params) = t.type_params() else {
        return Some(Vec::new());
    };
    let mut names = Vec::new();
    for param in params.params() {
        if param.is_const() || param.row_var().is_some() {
            errors.push(HirError {
                message: format!(
                    "`{type_name}` has a const or row parameter, which `derive` cannot \
                     write an impl for yet; write the impl by hand"
                ),
                range: at,
            });
            return None;
        }
        let Some(name) = param.name().and_then(|n| n.ident()) else {
            continue;
        };
        names.push(name);
    }
    Some(names)
}

/// `impl<A: Eq> Eq for Box<A> { .. }` for one trait.
fn write_impl(trait_name: &str, type_name: &str, params: &[String], shape: &Shape) -> String {
    // Every parameter is bounded by the trait being derived, whether or not a
    // field uses it. Rust makes the same trade for the same reason: knowing
    // which parameters are *reachable* from a field means resolving the field
    // types, and this pass deliberately runs before anything knows what a type
    // is. The cost is an over-strict bound on a phantom parameter, which is a
    // thing to fix by writing the impl out.
    let header = if params.is_empty() {
        String::new()
    } else {
        let bounded: Vec<String> = params
            .iter()
            .map(|p| format!("{p}: {trait_name}"))
            .collect();
        format!("<{}>", bounded.join(", "))
    };
    let self_type = if params.is_empty() {
        type_name.to_string()
    } else {
        format!("{type_name}<{}>", params.join(", "))
    };

    let method = match (trait_name, shape) {
        ("Eq", Shape::Record(fields, _)) => format!(
            "fn eq(self, other: {self_type}) -> Bool {{ {} }}",
            all_equal(&projections(fields))
        ),
        ("Eq", Shape::Variant(cases)) => format!(
            "fn eq(self, other: {self_type}) -> Bool {{ {} }}",
            variant_eq(type_name, cases)
        ),
        ("Ord", Shape::Record(fields, _)) => format!(
            "fn cmp(self, other: {self_type}) -> Ordering {{ {} }}",
            compare_in_turn(&projections(fields))
        ),
        ("Ord", Shape::Variant(cases)) => format!(
            "fn cmp(self, other: {self_type}) -> Ordering {{ {} }}",
            variant_cmp(type_name, cases)
        ),
        ("Hash", Shape::Record(fields, _)) => {
            let parts: Vec<String> = fields.iter().map(|f| format!("self.{f}.hash()")).collect();
            format!("fn hash(self) -> Int {{ {} }}", hash_mix(0, &parts))
        }
        ("Hash", Shape::Variant(cases)) => {
            format!(
                "fn hash(self) -> Int {{ {} }}",
                variant_hash(type_name, cases)
            )
        }
        ("Show", Shape::Record(fields, _)) => {
            format!(
                "fn show(self) -> String {{ {} }}",
                record_show(type_name, fields)
            )
        }
        ("Show", Shape::Variant(cases)) => {
            format!(
                "fn show(self) -> String {{ {} }}",
                variant_show(type_name, cases)
            )
        }
        ("ToJson", Shape::Record(fields, _)) => {
            format!("fn to_json(self) -> Json {{ {} }}", record_to_json(fields))
        }
        ("ToJson", Shape::Variant(cases)) => format!(
            "fn to_json(self) -> Json {{ {} }}",
            variant_to_json(type_name, cases)
        ),
        ("FromJson", Shape::Record(fields, field_types)) => format!(
            "fn from_json(value: Json) -> {self_type} raises DecodeError {{ {} }}",
            record_from_json(fields, field_types)
        ),
        ("FromJson", Shape::Variant(cases)) => format!(
            "fn from_json(value: Json) -> {self_type} raises DecodeError {{ {} }}",
            variant_from_json(type_name, cases)
        ),
        _ => unreachable!("`{trait_name}` is not derivable and should have been refused"),
    };

    format!("impl{header} {trait_name} for {self_type} {{\n  {method}\n}}\n")
}

/// `(self.x, other.x), (self.y, other.y)` — the pairs a record compares.
fn projections(fields: &[String]) -> Vec<(String, String)> {
    fields
        .iter()
        .map(|f| (format!("self.{f}"), format!("other.{f}")))
        .collect()
}

/// `a.eq(b) && c.eq(d)`, or `true` for nothing to compare.
///
/// `&&` short-circuits, so this stops at the first field that differs, which is
/// what a hand-written `eq` would do.
fn all_equal(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return "true".to_string();
    }
    pairs
        .iter()
        .map(|(l, r)| format!("{l}.eq({r})"))
        .collect::<Vec<_>>()
        .join(" && ")
}

/// Compares pairs in order, stopping at the first that is not `Equal`.
///
/// Nested `match` rather than a `let` chain because a `match` is an expression
/// and stops evaluating the rest: comparing every field and then picking the
/// first non-equal answer would be both slower and observably different for a
/// field whose `cmp` is expensive.
fn compare_in_turn(pairs: &[(String, String)]) -> String {
    match pairs.split_first() {
        None => "Ordering::Equal".to_string(),
        Some(((l, r), [])) => format!("{l}.cmp({r})"),
        Some(((l, r), rest)) => format!(
            "match {l}.cmp({r}) {{ \
             Ordering::Less => Ordering::Less, \
             Ordering::Greater => Ordering::Greater, \
             Ordering::Equal => {} }}",
            compare_in_turn(rest)
        ),
    }
}

/// The accumulator a derived `Hash` folds its fields through.
///
/// Starts from `seed` — zero for a record, the case's position for a variant,
/// so that two payload-free cases do not hash alike. See [`HASH_MODULUS`] for
/// why both operands are reduced first.
fn hash_mix(seed: usize, parts: &[String]) -> String {
    let mut acc = seed.to_string();
    for part in parts {
        acc = format!("(({acc} % {HASH_MODULUS}) * 31 + ({part} % {HASH_MODULUS}))");
    }
    acc
}

/// `T::Case(a0, a1)`, or `T::Case` for a case with no payload.
fn case_pattern(type_name: &str, case: &Case, prefix: &str) -> String {
    if case.arity == 0 {
        return format!("{type_name}::{}", case.name);
    }
    let binders: Vec<String> = (0..case.arity).map(|i| format!("{prefix}{i}")).collect();
    format!("{type_name}::{}({})", case.name, binders.join(", "))
}

/// `T::Case(_, _)` — the case, with its payload ignored.
///
/// A case pattern has to state the whole payload even when none of it is
/// wanted: the arity is part of what the pattern matches, and a bare
/// `T::Case` for a case that carries something is a different pattern.
fn case_shape(type_name: &str, case: &Case) -> String {
    if case.arity == 0 {
        return format!("{type_name}::{}", case.name);
    }
    format!(
        "{type_name}::{}({})",
        case.name,
        vec!["_"; case.arity].join(", ")
    )
}

/// The pairs of binders two matched-up occurrences of one case bind.
fn case_pairs(case: &Case) -> Vec<(String, String)> {
    (0..case.arity)
        .map(|i| (format!("a{i}"), format!("b{i}")))
        .collect()
}

/// Whether an inner `match` on `other` needs a catch-all.
///
/// It does not when there is only one case, and writing one anyway is an
/// unreachable arm — which this compiler reports as an error, correctly.
fn needs_catch_all(cases: &[Case]) -> bool {
    cases.len() > 1
}

fn variant_eq(type_name: &str, cases: &[Case]) -> String {
    let arms: Vec<String> = cases
        .iter()
        .map(|case| {
            let mine = case_pattern(type_name, case, "a");
            let theirs = case_pattern(type_name, case, "b");
            let answer = all_equal(&case_pairs(case));
            let fallback = if needs_catch_all(cases) {
                ", _ => false".to_string()
            } else {
                String::new()
            };
            format!("{mine} => match other {{ {theirs} => {answer}{fallback} }}")
        })
        .collect();
    format!("match self {{ {} }}", arms.join(", "))
}

/// Declaration order decides which case is `Less`, and payloads break the tie.
///
/// The position of each case is read off by a `match` on both sides rather than
/// by nesting one `match` inside another for every pair: that would be `n`
/// squared arms of which all but `n` say the same two things, and it would put
/// the ordering rule — earlier is less — in `n` squared places instead of one.
fn variant_cmp(type_name: &str, cases: &[Case]) -> String {
    let positions: Vec<String> = cases
        .iter()
        .enumerate()
        .map(|(i, case)| format!("{} => {i}", case_shape(type_name, case)))
        .collect();
    let positions = positions.join(", ");

    let arms: Vec<String> = cases
        .iter()
        .map(|case| {
            let mine = case_pattern(type_name, case, "a");
            if case.arity == 0 {
                return format!("{mine} => Ordering::Equal");
            }
            let theirs = case_pattern(type_name, case, "b");
            let answer = compare_in_turn(&case_pairs(case));
            // Reached only when both sides are this case, which the position
            // comparison above has already established — but the checker does
            // not know that, and an exhaustive `match` is what it asks for.
            let fallback = if needs_catch_all(cases) {
                ", _ => Ordering::Equal".to_string()
            } else {
                String::new()
            };
            format!("{mine} => match other {{ {theirs} => {answer}{fallback} }}")
        })
        .collect();

    format!(
        "let mine = match self {{ {positions} }}; \
         let theirs = match other {{ {positions} }}; \
         if mine < theirs {{ Ordering::Less }} \
         else if mine > theirs {{ Ordering::Greater }} \
         else {{ match self {{ {} }} }}",
        arms.join(", ")
    )
}

fn variant_hash(type_name: &str, cases: &[Case]) -> String {
    let arms: Vec<String> = cases
        .iter()
        .enumerate()
        .map(|(i, case)| {
            let pattern = case_pattern(type_name, case, "a");
            let parts: Vec<String> = (0..case.arity).map(|n| format!("a{n}.hash()")).collect();
            format!("{pattern} => {}", hash_mix(i, &parts))
        })
        .collect();
    format!("match self {{ {} }}", arms.join(", "))
}

/// `Point { x: 1, y: 2 }` — the record's own name, then its fields.
///
/// A `Show` is read by a person and asserted on by a test, so the format has to
/// be one thing forever. This is Rust's, because that is the one the audience
/// already reads, and naming the type is what makes two records with the same
/// field names distinguishable in a log.
fn record_show(type_name: &str, fields: &[String]) -> String {
    if fields.is_empty() {
        return format!("\"{type_name} {{}}\"");
    }
    let mut parts = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let lead = if i == 0 {
            format!("{type_name} {{ ")
        } else {
            ", ".to_string()
        };
        parts.push(format!("\"{lead}{field}: \""));
        parts.push(format!("self.{field}.show()"));
    }
    parts.push("\" }\"".to_string());
    parts.join(" + ")
}

/// `Shape::Circle(3)` — a constructor written the way a program writes it.
///
/// Not Rust's `Circle { r: 3 }` for a case with named fields, because Khora has
/// no such expression: a case is applied positionally however it was declared,
/// so this is the spelling that can be read back.
fn variant_show(type_name: &str, cases: &[Case]) -> String {
    let arms: Vec<String> = cases
        .iter()
        .map(|case| {
            let pattern = case_pattern(type_name, case, "a");
            let label = format!("{type_name}::{}", case.name);
            if case.arity == 0 {
                return format!("{pattern} => \"{label}\"");
            }
            let mut parts = vec![format!("\"{label}(\"")];
            for i in 0..case.arity {
                if i > 0 {
                    parts.push("\", \"".to_string());
                }
                parts.push(format!("a{i}.show()"));
            }
            parts.push("\")\"".to_string());
            format!("{pattern} => {}", parts.join(" + "))
        })
        .collect();
    format!("match self {{ {} }}", arms.join(", "))
}

/// `List::Cons(a, List::Cons(b, List::Nil))` — the list the helpers take.
///
/// **Kept written out even though `[a, b]` now compiles.** D13 settled that a
/// list literal denotes a `List`, and settled it by desugaring to exactly this
/// chain in HIR lowering — so the two spellings produce the same expressions,
/// and this one needs no `List` in the scope the expansion lands in. Emitting
/// `[a, b]` here would be a shorter string that lowers to the same thing and
/// depends on one more condition holding.
///
/// This function was the reason D13 was asked: generated code tried to be the
/// literal's first user and could not be, because the construct meant nothing.
///
/// `List` is in scope because `file_scope` brings it along with the other
/// names a derived body borrows from the trait's home module.
fn cons_chain(items: impl IntoIterator<Item = String>) -> String {
    items
        .into_iter()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .fold("List::Nil".to_string(), |rest, item| format!("List::Cons({item}, {rest})"))
}

/// A record is its JSON object: declaration names become field names and the
/// field values choose their own representation through `ToJson`.
fn record_to_json(fields: &[String]) -> String {
    let bindings = fields
        .iter()
        .map(|field| format!("let {field}: Json = self.{field}.to_json();"))
        .collect::<Vec<_>>()
        .join(" ");
    let encoded = fields
        .iter()
        .map(|field| format!("member(\"{field}\", {field})"));
    format!("{bindings} object({})", cons_chain(encoded))
}

/// Every record field is required. Unknown fields are deliberately ignored by
/// `field_as`, which lets an older reader accept a document written by a newer
/// producer that only added data.
fn record_from_json(fields: &[String], field_types: &[String]) -> String {
    let decoded: Vec<String> = fields
        .iter()
        .zip(field_types)
        .map(|(field, ty)| format!("let {field}: {ty} = field_as(value, \"{field}\")!;"))
        .collect();
    let initialized = fields
        .iter()
        .map(|field| format!("{field}: {field}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} {{ {initialized} }}", decoded.join(" "))
}

/// Variants use one adjacent-tag shape, including payload-free cases:
/// `{ "case": "Circle", "fields": [3] }`.
fn variant_to_json(type_name: &str, cases: &[Case]) -> String {
    let arms: Vec<String> = cases
        .iter()
        .map(|case| {
            let pattern = case_pattern(type_name, case, "a");
            let bindings = (0..case.arity)
                .map(|i| format!("let field{i}: Json = a{i}.to_json();"))
                .collect::<Vec<_>>()
                .join(" ");
            let fields = (0..case.arity).map(|i| format!("field{i}"));
            format!(
                "{pattern} => {{ {bindings} variant(\"{}\", {}) }}",
                case.name,
                cons_chain(fields)
            )
        })
        .collect();
    format!("match self {{ {} }}", arms.join(", "))
}

/// The tag decides the case, so this is a lookup keyed by a string.
///
/// **A `match` on the tag**, which is what it always wanted to be. It was an
/// `if` chain for as long as matching a `String` literal parsed, type-checked
/// and then failed in the backend — the same comparisons in a spelling that
/// compiled. D14 decided that a literal pattern is an equality test, so the
/// chain became the workaround it had always been described as.
fn variant_from_json(type_name: &str, cases: &[Case]) -> String {
    let expected = cases
        .iter()
        .map(|case| format!("`{}`", case.name))
        .collect::<Vec<_>>()
        .join(", ");

    let mut arms: Vec<String> = Vec::with_capacity(cases.len() + 1);
    for case in cases {
        let constructor = if case.arity == 0 {
            format!("{type_name}::{}", case.name)
        } else {
            let fields: Vec<String> = case
                .field_types
                .iter()
                .enumerate()
                .map(|(i, ty)| format!("let field{i}: {ty} = variant_field(value, {i})!;"))
                .collect();
            let arguments =
                (0..case.arity).map(|i| format!("field{i}")).collect::<Vec<_>>().join(", ");
            format!("{} {type_name}::{}({arguments})", fields.join(" "), case.name)
        };
        arms.push(format!(
            "\"{}\" => {{ variant_arity(value, {})!; {constructor} }}",
            case.name, case.arity
        ));
    }
    // A literal pattern does not make a `match` exhaustive, and this one is
    // genuinely not: the tag came out of somebody else's JSON and may say
    // anything at all. Naming what was expected is the whole value of the
    // error, so the catch-all is where the message lives.
    arms.push(format!("_ => unknown_variant(case, \"one of {expected}\")!"));
    format!("let case = variant_case(value)!; match case {{ {} }}", arms.join(", "))
}
