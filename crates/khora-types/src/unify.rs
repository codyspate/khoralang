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
            Mismatch::Rigid { param, ty } => write!(
                f,
                "`{param}` is a type the caller chooses, so it cannot be assumed to be `{ty}`"
            ),
            Mismatch::Arity { expected, found } => {
                write!(f, "expected {expected} argument(s), found {found}")
            }
        }
    }
}

/// The substitution built up while inferring one function body.
#[derive(Debug, Default)]
pub struct Unifier {
    /// What each flexible variable has been solved to, if anything.
    solved: Vec<Option<Type>>,
}

impl Unifier {
    pub fn new() -> Unifier {
        Unifier::default()
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

            (Type::Param(x), Type::Param(y)) if x == y => Ok(()),
            // A rigid parameter only unifies with itself. Anything else is the
            // body trying to pick its caller's type.
            (Type::Param(p), other) | (other, Type::Param(p)) => {
                Err(Mismatch::Rigid { param: p.clone(), ty: other.clone() })
            }

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
            _ => false,
        }
    }

    /// Replaces each rigid parameter with a fresh flexible variable.
    ///
    /// This is what makes a generic function usable: `id<A>` becomes
    /// `?0 -> ?0` at one call site and `?7 -> ?7` at the next, so the two do
    /// not constrain each other.
    pub fn instantiate(&mut self, generics: &[String], ty: &Type) -> Type {
        if generics.is_empty() {
            return ty.clone();
        }
        let mapping: HashMap<&str, Type> =
            generics.iter().map(|g| (g.as_str(), self.fresh())).collect();
        substitute(ty, &mapping)
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
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
