//! Unification: the engine underneath type inference.
//!
//! Phase 3 replaces phase 2's "compare against the declared type" checking with
//! Algorithm W. The part that makes it work is here — everything else is
//! walking the tree and calling [`Unifier::unify`].
//!
//! # Rigid and flexible variables
//!
//! Two kinds of variable, and keeping them apart is what makes the errors good:
//!
//! - [`Type::Param`] is **rigid**: a type the *caller* chose. Inside
//!   `fn id<A>(x: A) -> A`, `A` is rigid, so `x + 1` must fail — the body does
//!   not get to decide that `A` is `Int`.
//! - [`Type::Var`] is **flexible**: a hole inference is free to fill. Calling
//!   `id(1)` instantiates `A` to a fresh flexible variable, which then unifies
//!   with `Int`.
//!
//! Conflating them yields a checker that accepts `fn f<A>(x: A) -> Int { x }`,
//! which is unsound.

use std::collections::HashMap;

use crate::Type;

/// A flexible type variable, by index into the substitution.
pub type TypeVar = u32;

/// Why two types could not be made equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// Two concrete types that are simply different.
    Types { expected: Type, found: Type },
    /// A variable would have to contain itself: `a = List<a>`. Without this
    /// check, unification builds an infinite type and the compiler hangs.
    Infinite { var: TypeVar, ty: Type },
    /// A rigid parameter cannot be narrowed. The body of a generic function
    /// tried to decide what its caller's type argument is.
    Rigid { param: String, ty: Type },
    /// Functions of different arity.
    Arity { expected: usize, found: usize },
    /// A row lacks a label the other requires, and cannot grow one because it
    /// is closed.
    Missing { label: String, ty: Type },
    /// A projection whose owner inference never settled. `A::Spec` cannot be
    /// solved backwards — two types may share a `Spec` — so if nothing else
    /// says what `A` is, nothing will.
    Unsolved,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mismatch::Types { expected, found } => {
                write!(f, "expected `{expected}`, found `{found}`")
            }
            Mismatch::Infinite { ty, .. } => {
                write!(f, "this would make an infinite type, containing `{ty}`")
            }
            Mismatch::Unsolved => write!(
                f,
                "nothing here says which type this is projected from; \
                 name it at the call or annotate the result"
            ),
            Mismatch::Rigid { param, ty } => write!(
                f,
                "`{param}` is a type the caller chooses, so it cannot be assumed to be `{ty}`"
            ),
            Mismatch::Arity { expected, found } => {
                write!(f, "expected {expected} argument(s), found {found}")
            }
            Mismatch::Missing { label, ty } => {
                write!(f, "`{label}: {ty}` is required here but not provided")
            }
        }
    }
}

/// The substitution built up while inferring one function body.
#[derive(Debug, Default)]
pub struct Unifier {
    /// What each flexible variable has been solved to, if anything.
    solved: Vec<Option<Type>>,
    /// Every `type Item = ..` an impl in scope declares, so a projection can be
    /// normalized the moment its owner becomes concrete. Carried by value
    /// rather than borrowed: there are a handful per program, and a lifetime
    /// here would spread to everything that holds a `Unifier`.
    assoc: Vec<AssocBinding>,
    /// Projections whose owner was not known yet, and what they have to equal.
    ///
    /// `extract<A: Extract>(spec: A::Spec) -> A` called as `extract(Num::spec())`
    /// meets `?A::Spec ~ NumSpec` before anything has said what `?A` is, and
    /// projection is not injective — two types may share a `Spec` — so this
    /// cannot be solved backwards. It is not an error either: the return type
    /// usually settles `?A` a moment later. So it waits, and
    /// [`Unifier::settle`] retries it once inference is done.
    deferred: Vec<(Type, Type)>,
}

/// One `type Name = Value` from one impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssocBinding {
    /// Head constructor of the implementing type: `List` for `impl .. for List<A>`.
    pub head: String,
    pub name: String,
    /// The impl's own parameters, rigid in `self_type` and `value`.
    pub generics: Vec<String>,
    pub self_type: Type,
    pub value: Type,
}

impl Unifier {
    pub fn new() -> Unifier {
        Unifier::default()
    }

    /// Supplies the associated-type bindings projections normalize through.
    pub fn with_assoc(mut self, assoc: Vec<AssocBinding>) -> Unifier {
        self.assoc = assoc;
        self
    }

    /// `Range::Item` given `impl Iterator for Range { type Item = Int; }`.
    ///
    /// The impl's parameters are matched against the concrete owner first, so
    /// `List<Int>::Item` under `impl<A> Iterator for List<A> { type Item = A; }`
    /// projects to `Int` rather than to a rigid `A`.
    fn normalize_assoc(&self, owner: &Type, name: &str) -> Option<Type> {
        let head = head_name(owner)?;
        let binding = self.assoc.iter().find(|b| b.head == head && b.name == name)?;

        let mut mapping = HashMap::new();
        match_type(&binding.self_type, owner, &binding.generics, &mut mapping);
        Some(substitute(&binding.value, &mapping))
    }

    /// How many projections are waiting. Paired with [`Unifier::settle`] so a
    /// caller can tell which of its own unifications deferred one.
    pub fn deferred_len(&self) -> usize {
        self.deferred.len()
    }

    /// Retries the projections that were waiting on their owner.
    ///
    /// One entry per deferral, **in the order they were deferred**, so a caller
    /// that recorded where each one came from can pair the two up. Each is the
    /// projection as it now stands and the mismatch if it still does not fit;
    /// an owner that is *still* a hole is [`Mismatch::Unsolved`], since nothing
    /// later is going to decide it.
    pub fn settle(&mut self) -> Vec<(Type, Option<Mismatch>)> {
        // Settling one can solve the owner of another — `f(g(x))` leaves the
        // inner call waiting on the outer call's return type — so this runs to
        // a fixed point. Results carry their original index because the order
        // they *resolve* in is not the order they were written in.
        let mut waiting: Vec<(usize, Type, Type)> = std::mem::take(&mut self.deferred)
            .into_iter()
            .enumerate()
            .map(|(i, (projection, other))| (i, projection, other))
            .collect();
        let mut out: Vec<(usize, Type, Option<Mismatch>)> = Vec::new();

        loop {
            let before = waiting.len();
            let mut again = Vec::new();
            for (index, projection, other) in waiting {
                let Type::Assoc { owner, .. } = &projection else { continue };
                if matches!(self.shallow(owner), Type::Var(_)) {
                    again.push((index, projection, other));
                    continue;
                }
                let resolved = self.shallow(&projection);
                let why = self.unify(&resolved, &other).err();
                out.push((index, resolved, why));
            }
            waiting = again;
            if waiting.len() == before {
                break;
            }
        }

        for (index, projection, _) in waiting {
            out.push((index, self.zonk(&projection), Some(Mismatch::Unsolved)));
        }
        out.sort_by_key(|(index, _, _)| *index);
        out.into_iter().map(|(_, projection, why)| (projection, why)).collect()
    }

    /// A new hole for inference to fill.
    pub fn fresh(&mut self) -> Type {
        self.solved.push(None);
        Type::Var((self.solved.len() - 1) as TypeVar)
    }

    /// Follows a variable to whatever it currently stands for, one level deep.
    ///
    /// Cheap and non-recursive; [`Unifier::zonk`] is the deep version.
    pub fn shallow(&self, ty: &Type) -> Type {
        let mut current = ty.clone();
        while let Type::Var(v) = current {
            match self.solved.get(v as usize).and_then(|s| s.clone()) {
                Some(next) => current = next,
                None => break,
            }
        }
        // `?F<A>` with `?F` solved to `Option` *is* `Option<A>`. Collapsing here
        // means every caller sees the concrete type without asking.
        if let Type::Applied { head, args } = &current {
            if let Type::Adt { name, args: none } = self.shallow(head) {
                if none.is_empty() {
                    return Type::Adt { name, args: args.clone() };
                }
            }
        }

        // `Range::Item` *is* `Int` once the owner is known. Doing it here means
        // unification, zonking and every diagnostic see the real type without
        // any of them having to know projections exist.
        if let Type::Assoc { owner, name } = &current {
            let owner = self.shallow(owner);
            if let Some(value) = self.normalize_assoc(&owner, name) {
                return self.shallow(&value);
            }
            return Type::Assoc { owner: Box::new(owner), name: name.clone() };
        }
        current
    }

    /// Replaces every solved variable throughout a type.
    ///
    /// Run before reporting anything: a diagnostic naming `?3` instead of `Int`
    /// is useless.
    pub fn zonk(&self, ty: &Type) -> Type {
        match self.shallow(ty) {
            Type::Fn { params, ret } => Type::Fn {
                params: params.iter().map(|p| self.zonk(p)).collect(),
                ret: Box::new(self.zonk(&ret)),
            },
            Type::Adt { name, args } => Type::Adt {
                name,
                args: args.iter().map(|a| self.zonk(a)).collect(),
            },
            Type::Tuple(items) => Type::Tuple(items.iter().map(|i| self.zonk(i)).collect()),
            Type::Row { fields, tail } => {
                let tail = tail.map(|t| self.zonk(&t));
                // Zonking a tail may reveal more labels, which belong in this
                // row rather than nested inside it.
                let mut fields: Vec<(String, Type)> =
                    fields.iter().map(|(l, t)| (l.clone(), self.zonk(t))).collect();
                match tail {
                    Some(Type::Row { fields: more, tail }) => {
                        fields.extend(more);
                        Type::row(fields, tail.map(|t| *t))
                    }
                    other => Type::row(fields, other),
                }
            }
            Type::Assoc { owner, name } => {
                Type::Assoc { owner: Box::new(self.zonk(&owner)), name }
            }
            Type::Applied { head, args } => {
                let head = self.zonk(&head);
                let args: Vec<Type> = args.iter().map(|a| self.zonk(a)).collect();
                match head {
                    Type::Adt { name, args: none } if none.is_empty() => {
                        Type::Adt { name, args }
                    }
                    head => Type::Applied { head: Box::new(head), args },
                }
            }
            other => other,
        }
    }

    /// Makes two types equal, or explains why they cannot be.
    pub fn unify(&mut self, expected: &Type, found: &Type) -> Result<(), Mismatch> {
        let a = self.shallow(expected);
        let b = self.shallow(found);

        match (&a, &b) {
            // `Unknown` is downstream of an error already reported, and `Never`
            // means control does not arrive. Neither should fail twice.
            (Type::Unknown, _) | (_, Type::Unknown) => Ok(()),
            (Type::Never, _) | (_, Type::Never) => Ok(()),

            (Type::Var(x), Type::Var(y)) if x == y => Ok(()),
            (Type::Var(v), other) | (other, Type::Var(v)) => self.bind(*v, other),

            // Two projections of the same name off the same owner are the same
            // type. One that has not normalized is rigid — its owner is still a
            // parameter, so nothing here may assume what it will become.
            (Type::Assoc { owner: o1, name: n1 }, Type::Assoc { owner: o2, name: n2 })
                if n1 == n2 =>
            {
                self.unify(o1, o2)
            }
            (Type::Assoc { owner, name }, other) | (other, Type::Assoc { owner, name }) => {
                // An owner that is still a hole may yet be filled, and usually
                // is — by the call's own return type. One that is a rigid
                // parameter never will be, and that is the real error.
                if matches!(self.shallow(owner), Type::Var(_)) {
                    let projection = Type::Assoc { owner: owner.clone(), name: name.clone() };
                    self.deferred.push((projection, other.clone()));
                    return Ok(());
                }
                Err(Mismatch::Rigid {
                    param: format!("{owner}::{name}"),
                    ty: other.clone(),
                })
            }

            (Type::Param(x), Type::Param(y)) if x == y => Ok(()),
            // A rigid parameter only unifies with itself. Anything else is the
            // body trying to pick its caller's type.
            (Type::Param(p), other) | (other, Type::Param(p)) => {
                Err(Mismatch::Rigid { param: p.clone(), ty: other.clone() })
            }

            // A dimension only matches itself. The mismatch carries both
            // values, so the diagnostic can name them.
            (Type::Const(x), Type::Const(y)) if x == y => Ok(()),

            (
                Type::Row { fields: f1, tail: t1 },
                Type::Row { fields: f2, tail: t2 },
            ) => self.unify_rows(f1, t1.as_deref(), f2, t2.as_deref()),

            (Type::Int, Type::Int)
            | (Type::Bool, Type::Bool)
            | (Type::Str, Type::Str)
            | (Type::Unit, Type::Unit) => Ok(()),

            (Type::Adt { name: n1, args: a1 }, Type::Adt { name: n2, args: a2 }) => {
                if n1 != n2 {
                    return Err(Mismatch::Types { expected: a.clone(), found: b.clone() });
                }
                if a1.len() != a2.len() {
                    return Err(Mismatch::Arity { expected: a1.len(), found: a2.len() });
                }
                for (x, y) in a1.iter().zip(a2) {
                    self.unify(x, y)?;
                }
                Ok(())
            }

            // Componentwise, and only at equal width. Two tuples of different
            // lengths are different types, so the whole pair is reported rather
            // than a component that has no counterpart.
            // Two applications: heads against heads, arguments against
            // arguments. Widths must agree — `F<A>` and `G<A, B>` describe
            // constructors of different kinds.
            (Type::Applied { head: h1, args: a1 }, Type::Applied { head: h2, args: a2 })
                if a1.len() == a2.len() =>
            {
                self.unify(h1, h2)?;
                for (x, y) in a1.iter().zip(a2) {
                    self.unify(x, y)?;
                }
                Ok(())
            }

            // The case the whole representation exists for: `?F<?B>` against
            // `Option<Int>` decides `?F := Option` and `?B := Int`. Restricted
            // higher-order unification, and it has a unique answer because the
            // head is a variable applied to a fixed number of arguments.
            (Type::Applied { head, args }, Type::Adt { name, args: concrete })
            | (Type::Adt { name, args: concrete }, Type::Applied { head, args })
                if args.len() == concrete.len() =>
            {
                self.unify(head, &Type::Adt { name: name.clone(), args: Vec::new() })?;
                for (x, y) in args.iter().zip(concrete) {
                    self.unify(x, y)?;
                }
                Ok(())
            }

            (Type::Tuple(x), Type::Tuple(y)) => {
                if x.len() != y.len() {
                    return Err(Mismatch::Types { expected: a.clone(), found: b.clone() });
                }
                for (p, q) in x.iter().zip(y) {
                    self.unify(p, q)?;
                }
                Ok(())
            }

            (
                Type::Fn { params: p1, ret: r1 },
                Type::Fn { params: p2, ret: r2 },
            ) => {
                if p1.len() != p2.len() {
                    return Err(Mismatch::Arity { expected: p1.len(), found: p2.len() });
                }
                for (x, y) in p1.iter().zip(p2) {
                    self.unify(x, y)?;
                }
                self.unify(r1, r2)
            }

            _ => Err(Mismatch::Types { expected: a.clone(), found: b.clone() }),
        }
    }

    /// Rémy-style row unification: shared labels agree, and whatever one side
    /// lacks has to fit through its tail.
    ///
    /// The whole effect system rests on this. `f() with { ledger: L }` called
    /// from a context providing `{ ledger: L, ai: A }` works because the
    /// callee's row is *open* — its tail absorbs `ai`. An entry point works
    /// because its row is closed and nothing is left to absorb.
    fn unify_rows(
        &mut self,
        f1: &[(String, Type)],
        t1: Option<&Type>,
        f2: &[(String, Type)],
        t2: Option<&Type>,
    ) -> Result<(), Mismatch> {
        // Labels both sides carry must agree on what they carry.
        for (label, left) in f1 {
            if let Some((_, right)) = f2.iter().find(|(l, _)| l == label) {
                self.unify(left, right)?;
            }
        }
        let only_in = |a: &[(String, Type)], b: &[(String, Type)]| -> Vec<(String, Type)> {
            a.iter().filter(|(l, _)| !b.iter().any(|(o, _)| o == l)).cloned().collect()
        };
        let missing_from_2 = only_in(f1, f2);
        let missing_from_1 = only_in(f2, f1);

        match (missing_from_1.is_empty(), missing_from_2.is_empty()) {
            // The same labels on both sides: the tails describe the same rest.
            (true, true) => self.unify_tails(t1, t2),
            // One side is short. Its tail has to be exactly what it is short
            // by, plus whatever the other side's tail allows.
            (false, true) => self.grow(t1, missing_from_1, t2),
            (true, false) => self.grow(t2, missing_from_2, t1),
            // Both are short. A fresh tail stands for what neither named.
            (false, false) => {
                let rest = self.fresh();
                self.grow(t1, missing_from_1, Some(&rest))?;
                self.grow(t2, missing_from_2, Some(&rest))
            }
        }
    }

    /// Requires `tail` to cover `missing`, with `rest` beyond it.
    fn grow(
        &mut self,
        tail: Option<&Type>,
        missing: Vec<(String, Type)>,
        rest: Option<&Type>,
    ) -> Result<(), Mismatch> {
        let Some(tail) = tail else {
            // A closed row cannot grow, which is the error worth reporting
            // well: it names the label nobody supplied.
            let (label, ty) = missing.into_iter().next().expect("non-empty by construction");
            return Err(Mismatch::Missing { label, ty });
        };
        self.unify(tail, &Type::row(missing, rest.cloned()))
    }

    fn unify_tails(&mut self, t1: Option<&Type>, t2: Option<&Type>) -> Result<(), Mismatch> {
        match (t1, t2) {
            (None, None) => Ok(()),
            (Some(a), Some(b)) => self.unify(a, b),
            // One side is closed, so the other's tail must turn out to be
            // empty rather than standing for something unnamed.
            (Some(open), None) | (None, Some(open)) => self.unify(open, &Type::empty_row()),
        }
    }

    fn bind(&mut self, var: TypeVar, ty: &Type) -> Result<(), Mismatch> {
        if self.occurs(var, ty) {
            return Err(Mismatch::Infinite { var, ty: self.zonk(ty) });
        }
        self.solved[var as usize] = Some(ty.clone());
        Ok(())
    }

    /// Whether `var` appears anywhere in `ty`, which would make it infinite.
    fn occurs(&self, var: TypeVar, ty: &Type) -> bool {
        match self.shallow(ty) {
            Type::Var(v) => v == var,
            Type::Fn { params, ret } => {
                params.iter().any(|p| self.occurs(var, p)) || self.occurs(var, &ret)
            }
            Type::Adt { args, .. } => args.iter().any(|a| self.occurs(var, a)),
            Type::Tuple(items) => items.iter().any(|i| self.occurs(var, i)),
            Type::Row { fields, tail } => {
                fields.iter().any(|(_, t)| self.occurs(var, t))
                    || tail.is_some_and(|t| self.occurs(var, &t))
            }
            Type::Assoc { owner, .. } => self.occurs(var, &owner),
            Type::Applied { head, args } => {
                self.occurs(var, &head) || args.iter().any(|a| self.occurs(var, a))
            }
            _ => false,
        }
    }

    /// Replaces each rigid parameter with a fresh flexible variable.
    ///
    /// This is what makes a generic function usable: `id<A>` becomes
    /// `?0 -> ?0` at one call site and `?7 -> ?7` at the next, so the two do
    /// not constrain each other.
    pub fn instantiate(&mut self, generics: &[String], ty: &Type) -> Type {
        self.instantiate_with(generics, ty).0
    }

    /// As [`Unifier::instantiate`], also returning the fresh variables in the
    /// order the parameters were declared.
    ///
    /// Monomorphization needs those: once solved, they are the type arguments
    /// this particular mention chose.
    pub fn instantiate_with(&mut self, generics: &[String], ty: &Type) -> (Type, Vec<Type>) {
        if generics.is_empty() {
            return (ty.clone(), Vec::new());
        }
        let args: Vec<Type> = generics.iter().map(|_| self.fresh()).collect();
        let mapping: HashMap<&str, Type> = generics
            .iter()
            .zip(&args)
            .map(|(g, a)| (g.as_str(), a.clone()))
            .collect();
        (substitute(ty, &mapping), args)
    }
}

/// Solves `params` by matching `pattern` against `concrete`.
///
/// One-way: `concrete` is fixed and only `pattern`'s parameters are assigned.
/// This is how `impl<A> Functor for Option<A>` learns that `A = Int` when the
/// call site's receiver is `Option<Int>` — instance selection, not inference,
/// so there is nothing to unify and no variables to create.
pub fn match_params(
    pattern: &Type,
    concrete: &Type,
    params: &[String],
    out: &mut HashMap<String, Type>,
) -> bool {
    match (pattern, concrete) {
        (Type::Param(p), _) if params.iter().any(|g| g == p) => {
            match out.get(p) {
                Some(existing) => existing == concrete,
                None => {
                    out.insert(p.clone(), concrete.clone());
                    true
                }
            }
        }
        (Type::Adt { name: a, args: x }, Type::Adt { name: b, args: y }) => {
            a == b
                && x.len() == y.len()
                && x.iter().zip(y).all(|(p, c)| match_params(p, c, params, out))
        }
        (Type::Tuple(x), Type::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, c)| match_params(p, c, params, out))
        }
        (Type::Fn { params: p1, ret: r1 }, Type::Fn { params: p2, ret: r2 }) => {
            p1.len() == p2.len()
                && p1.iter().zip(p2).all(|(p, c)| match_params(p, c, params, out))
                && match_params(r1, r2, params, out)
        }
        _ => pattern == concrete,
    }
}

/// The head constructor of a type, when it has one.
fn head_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Adt { name, .. } => Some(name.clone()),
        Type::Int => Some("Int".to_string()),
        Type::Bool => Some("Bool".to_string()),
        Type::Str => Some("String".to_string()),
        _ => None,
    }
}

/// Reads an impl's parameters off the type it is being used at.
///
/// One-directional and forgiving: anything that does not line up is left out of
/// the mapping rather than reported, because a mismatch here means the impl was
/// not the right one and that is decided elsewhere.
fn match_type<'a>(
    pattern: &'a Type,
    concrete: &Type,
    generics: &[String],
    out: &mut HashMap<&'a str, Type>,
) {
    match (pattern, concrete) {
        (Type::Param(p), _) if generics.iter().any(|g| g == p) => {
            out.insert(p.as_str(), concrete.clone());
        }
        (Type::Adt { args: a, .. }, Type::Adt { args: b, .. }) if a.len() == b.len() => {
            for (x, y) in a.iter().zip(b) {
                match_type(x, y, generics, out);
            }
        }
        _ => {}
    }
}

/// Rewrites rigid parameters according to `mapping`.
pub fn substitute(ty: &Type, mapping: &HashMap<&str, Type>) -> Type {
    match ty {
        Type::Param(name) => mapping.get(name.as_str()).cloned().unwrap_or(ty.clone()),
        Type::Fn { params, ret } => Type::Fn {
            params: params.iter().map(|p| substitute(p, mapping)).collect(),
            ret: Box::new(substitute(ret, mapping)),
        },
        Type::Adt { name, args } => Type::Adt {
            name: name.clone(),
            args: args.iter().map(|a| substitute(a, mapping)).collect(),
        },
        Type::Tuple(items) => Type::Tuple(items.iter().map(|i| substitute(i, mapping)).collect()),
        Type::Row { fields, tail } => {
            let mut fields: Vec<(String, Type)> =
                fields.iter().map(|(l, t)| (l.clone(), substitute(t, mapping))).collect();
            // A row variable standing for more labels is spliced in, not
            // nested: `{ a | 'e }` with `'e := { b }` is `{ a, b }`.
            match tail.as_ref().map(|t| substitute(t, mapping)) {
                Some(Type::Row { fields: more, tail }) => {
                    fields.extend(more);
                    Type::row(fields, tail.map(|t| *t))
                }
                other => Type::row(fields, other),
            }
        }
        Type::Assoc { owner, name } => {
            Type::Assoc { owner: Box::new(substitute(owner, mapping)), name: name.clone() }
        }
        // Substituting the head is what turns `Self<A>` into `Option<A>`. The
        // parameter maps to a bare constructor, so its own arguments are empty
        // and the application supplies them.
        // Substituting the head is what turns `Self<A>` into `Option<A>`: the
        // parameter maps to a bare constructor, and the application supplies the
        // arguments it was missing.
        Type::Applied { head, args } => {
            let args: Vec<Type> = args.iter().map(|a| substitute(a, mapping)).collect();
            match substitute(head, mapping) {
                Type::Adt { name, args: none } if none.is_empty() => {
                    Type::Adt { name, args }
                }
                head => Type::Applied { head: Box::new(head), args },
            }
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- rows --------------------------------------------------------------

    fn closed(labels: &[(&str, Type)]) -> Type {
        Type::row(labels.iter().map(|(l, t)| (l.to_string(), t.clone())).collect(), None)
    }

    fn open(labels: &[(&str, Type)], tail: Type) -> Type {
        Type::row(labels.iter().map(|(l, t)| (l.to_string(), t.clone())).collect(), Some(tail))
    }

    fn ledger() -> Type {
        Type::adt("Ledger".to_string())
    }

    fn ai() -> Type {
        Type::adt("Ai".to_string())
    }

    /// Order is not part of a row's identity.
    #[test]
    fn a_row_is_the_same_written_either_way() {
        let mut u = Unifier::new();
        let a = closed(&[("ledger", ledger()), ("ai", ai())]);
        let b = closed(&[("ai", ai()), ("ledger", ledger())]);
        assert!(u.unify(&a, &b).is_ok());
    }

    /// The case the whole effect system rests on: a callee requiring less than
    /// the caller provides fits, because its tail absorbs the difference.
    #[test]
    fn an_open_row_absorbs_what_it_did_not_name() {
        let mut u = Unifier::new();
        let rest = u.fresh();
        let callee = open(&[("ledger", ledger())], rest.clone());
        let caller = closed(&[("ledger", ledger()), ("ai", ai())]);
        assert!(u.unify(&callee, &caller).is_ok());
        assert_eq!(u.zonk(&rest), closed(&[("ai", ai())]), "the tail became the remainder");
    }

    /// And a closed row cannot, which is what makes a missing capability an
    /// error rather than an inference that silently succeeds.
    #[test]
    fn a_closed_row_cannot_absorb_an_extra_label() {
        let mut u = Unifier::new();
        let entry = closed(&[]);
        let needed = closed(&[("ledger", ledger())]);
        match u.unify(&entry, &needed) {
            Err(Mismatch::Missing { label, .. }) => assert_eq!(label, "ledger"),
            other => panic!("expected a missing label, got {other:?}"),
        }
    }

    /// The diagnostic names the label, which is phase 4's exit criterion.
    #[test]
    fn a_missing_label_is_named() {
        let mut u = Unifier::new();
        let err = u.unify(&closed(&[]), &closed(&[("ledger", ledger())])).unwrap_err();
        assert!(err.to_string().contains("`ledger: Ledger`"), "{err}");
    }

    /// Shared labels have to agree on what they carry.
    #[test]
    fn one_label_cannot_have_two_types() {
        let mut u = Unifier::new();
        let a = closed(&[("ledger", ledger())]);
        let b = closed(&[("ledger", ai())]);
        assert!(u.unify(&a, &b).is_err());
    }

    /// Two open rows, each naming something the other did not: a fresh tail
    /// stands for what neither named, and each side ends up complete.
    #[test]
    fn two_open_rows_meet_in_the_middle() {
        let mut u = Unifier::new();
        let (r1, r2) = (u.fresh(), u.fresh());
        let a = open(&[("ledger", ledger())], r1.clone());
        let b = open(&[("ai", ai())], r2.clone());
        assert!(u.unify(&a, &b).is_ok());

        assert_eq!(u.zonk(&a), u.zonk(&b), "the two rows agree once solved");

        // Each tail now stands for the label the other side named, so neither
        // can be emptied on its own — `r1` has absorbed `ai`.
        assert!(u.unify(&r1, &Type::empty_row()).is_err());

        // Closing the whole thing closes both at the same set of labels.
        let both = closed(&[("ledger", ledger()), ("ai", ai())]);
        assert!(u.unify(&a, &both).is_ok());
        assert_eq!(u.zonk(&a), both);
        assert_eq!(u.zonk(&b), both);
        let _ = r2;
    }

    /// An open row unified with a closed one is closed too: there is nothing
    /// left for the tail to stand for.
    #[test]
    fn meeting_a_closed_row_closes_the_open_one() {
        let mut u = Unifier::new();
        let rest = u.fresh();
        let open_row = open(&[("ledger", ledger())], rest.clone());
        assert!(u.unify(&open_row, &closed(&[("ledger", ledger())])).is_ok());
        assert_eq!(u.zonk(&rest), Type::empty_row());
    }

    /// Zonking splices a solved tail in rather than leaving a row inside a row.
    #[test]
    fn a_solved_tail_flattens() {
        let mut u = Unifier::new();
        let rest = u.fresh();
        let row = open(&[("ledger", ledger())], rest.clone());
        assert!(u.unify(&rest, &closed(&[("ai", ai())])).is_ok());
        assert_eq!(u.zonk(&row), closed(&[("ai", ai()), ("ledger", ledger())]));
    }

    fn adt(name: &str, args: Vec<Type>) -> Type {
        Type::Adt { name: name.to_string(), args }
    }

    #[test]
    fn concrete_types_unify_with_themselves() {
        let mut u = Unifier::new();
        assert!(u.unify(&Type::Int, &Type::Int).is_ok());
        assert!(u.unify(&Type::Int, &Type::Bool).is_err());
    }

    #[test]
    fn a_variable_takes_the_type_it_meets() {
        let mut u = Unifier::new();
        let v = u.fresh();
        assert!(u.unify(&v, &Type::Int).is_ok());
        assert_eq!(u.zonk(&v), Type::Int);
    }

    #[test]
    fn a_variable_stays_consistent_across_uses() {
        let mut u = Unifier::new();
        let v = u.fresh();
        assert!(u.unify(&v, &Type::Int).is_ok());
        assert!(u.unify(&v, &Type::Bool).is_err(), "a solved variable must not be re-solved");
    }

    #[test]
    fn variables_unify_with_each_other() {
        let mut u = Unifier::new();
        let (a, b) = (u.fresh(), u.fresh());
        assert!(u.unify(&a, &b).is_ok());
        assert!(u.unify(&b, &Type::Str).is_ok());
        assert_eq!(u.zonk(&a), Type::Str, "solving one should solve the other");
    }

    /// Without this the compiler builds an infinite type and hangs.
    #[test]
    fn the_occurs_check_rejects_a_self_referential_type() {
        let mut u = Unifier::new();
        let v = u.fresh();
        let recursive = adt("List", vec![v.clone()]);
        assert!(matches!(u.unify(&v, &recursive), Err(Mismatch::Infinite { .. })));
    }

    #[test]
    fn generic_types_unify_argument_by_argument() {
        let mut u = Unifier::new();
        let v = u.fresh();
        assert!(u.unify(&adt("Option", vec![v.clone()]), &adt("Option", vec![Type::Int])).is_ok());
        assert_eq!(u.zonk(&v), Type::Int);

        assert!(u.unify(&adt("Option", vec![Type::Int]), &adt("List", vec![Type::Int])).is_err());
    }

    #[test]
    fn functions_unify_pointwise() {
        let mut u = Unifier::new();
        let v = u.fresh();
        let expected = Type::Fn { params: vec![v.clone()], ret: Box::new(Type::Bool) };
        let found = Type::Fn { params: vec![Type::Int], ret: Box::new(Type::Bool) };
        assert!(u.unify(&expected, &found).is_ok());
        assert_eq!(u.zonk(&v), Type::Int);
    }

    #[test]
    fn functions_of_different_arity_do_not_unify() {
        let mut u = Unifier::new();
        let one = Type::Fn { params: vec![Type::Int], ret: Box::new(Type::Int) };
        let two = Type::Fn { params: vec![Type::Int, Type::Int], ret: Box::new(Type::Int) };
        assert!(matches!(u.unify(&one, &two), Err(Mismatch::Arity { .. })));
    }

    /// The body of a generic function does not get to decide what its caller's
    /// type argument is.
    #[test]
    fn a_rigid_parameter_does_not_unify_with_a_concrete_type() {
        let mut u = Unifier::new();
        let a = Type::Param("A".into());
        assert!(u.unify(&a, &a).is_ok());
        assert!(matches!(u.unify(&a, &Type::Int), Err(Mismatch::Rigid { .. })));
        assert!(matches!(
            u.unify(&Type::Param("A".into()), &Type::Param("B".into())),
            Err(Mismatch::Rigid { .. })
        ));
    }

    /// Each call site gets its own copy, or two unrelated calls would constrain
    /// each other.
    #[test]
    fn instantiation_gives_each_call_site_fresh_variables() {
        let mut u = Unifier::new();
        let identity = Type::Fn {
            params: vec![Type::Param("A".into())],
            ret: Box::new(Type::Param("A".into())),
        };
        let generics = vec!["A".to_string()];

        let first = u.instantiate(&generics, &identity);
        let second = u.instantiate(&generics, &identity);

        let Type::Fn { params: p1, .. } = &first else { panic!() };
        let Type::Fn { params: p2, .. } = &second else { panic!() };
        assert!(u.unify(&p1[0], &Type::Int).is_ok());
        assert!(
            u.unify(&p2[0], &Type::Bool).is_ok(),
            "the second call site should be independent of the first"
        );
    }

    #[test]
    fn instantiation_keeps_a_parameter_consistent_within_one_signature() {
        let mut u = Unifier::new();
        let identity = Type::Fn {
            params: vec![Type::Param("A".into())],
            ret: Box::new(Type::Param("A".into())),
        };
        let instance = u.instantiate(&["A".to_string()], &identity);

        let Type::Fn { params, ret } = &instance else { panic!() };
        assert!(u.unify(&params[0], &Type::Int).is_ok());
        assert_eq!(u.zonk(ret), Type::Int, "argument and return share one variable");
    }

    #[test]
    fn errors_and_divergence_never_fail_to_unify() {
        let mut u = Unifier::new();
        assert!(u.unify(&Type::Unknown, &Type::Int).is_ok());
        assert!(u.unify(&Type::Int, &Type::Never).is_ok());
    }
}
