//! Type parameters: instantiating them, and checking their bounds.
//!
//! Instantiation is fresh variables for a signature's parameters; the bounds
//! are checked once the variables are solved, which is why `check_bounds` runs
//! at the end of a body rather than at each call.

use super::*;

impl<'a> Checker<'a> {
    /// Maps a type's parameters onto the arguments `ty` supplies.
    ///
    /// Falls back to fresh variables when the scrutinee is not the expected
    /// ADT — usually downstream of another error, where inventing a variable
    /// keeps one mistake from becoming several.
    pub(super) fn substitution_for(&mut self, type_name: &str, ty: &Type) -> HashMap<String, Type> {
        let generics = self.types.adts.get(type_name).cloned().unwrap_or_default();
        let args = match self.unifier.zonk(ty) {
            Type::Adt { name, args, .. } if name == type_name => args,
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
    pub(super) fn instantiate_adt(&mut self, name: &str) -> (Type, HashMap<String, Type>) {
        let generics = self.types.adts.get(name).cloned().unwrap_or_default();
        let mapping: HashMap<String, Type> =
            generics.iter().map(|g| (g.clone(), self.unifier.fresh())).collect();
        let args = generics.iter().map(|g| mapping[g].clone()).collect();
        // `name` is what the mention spelled. The identity is what it
        // resolves to, so an alias instantiates the type it names rather than
        // one of its own.
        let (home, declared) = match self.types.homes.of(name) {
            Some((home, declared)) => (Some(home), declared),
            None => (None, name.to_string()),
        };
        (Type::Adt { name: declared, home, args }, mapping)
    }

    /// Whether `owner` names a type at all, rather than a trait.
    ///
    /// Used only to choose wording. A *type* asked for a function it has not
    /// got wants "has no function named"; saying it "is not a trait" answers a
    /// question the caller did not ask, and is what `Int::show(x)` and
    /// `U8::show(x)` were both told.
    ///
    /// Three ways to be one, because a type can be known three ways: declared
    /// in this program, carrying an impl somebody wrote, or built in — and a
    /// builtin is in neither of the first two maps, which is the whole reason
    /// the message was wrong for exactly the types a newcomer tries first.
    pub(super) fn names_a_type(&self, owner: &str) -> bool {
        self.types.adts.contains_key(owner)
            || crate::IntKind::parse(owner).is_some()
            || matches!(owner, "Int" | "Float" | "Bool" | "String")
            || self
                .types
                .traits
                .impls
                .iter()
                .any(|i| crate::traits::head_of(&i.self_type).as_deref() == Some(owner))
    }

    /// The traits the enclosing function requires of `param`.
    pub(super) fn bounds_on(&self, param: &str) -> Vec<String> {
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
    pub(crate) fn check_bounds(&mut self) {
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
                        let called = traits::readable_key(&name);
                        self.error(
                            format!(
                                "`{arg}` does not implement `{wanted}`, which `{called}` \
                                 requires"
                            ),
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
    pub(super) fn satisfies(&self, wanted: &str, ty: &Type) -> bool {
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
}
