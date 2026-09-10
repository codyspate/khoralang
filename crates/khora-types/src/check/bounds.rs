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
            || matches!(owner, "Int" | "Float" | "Bool" | "Char" | "String")
            || self
                .types
                .traits
                .impls
                .iter()
                .any(|i| crate::traits::head_of(&i.self_type).as_deref() == Some(owner))
    }

    /// The traits the enclosing function requires of `param`.
    pub(super) fn bounds_on(&self, param: &str) -> Vec<Bound> {
        self.signature
            .generics
            .iter()
            .position(|g| g == param)
            .and_then(|i| self.signature.bounds.get(i))
            .cloned()
            .unwrap_or_default()
    }

    /// Those bounds as bare names, for the questions that are about which
    /// trait rather than which of its instances — looking a `TraitDef` up to
    /// see whether it declares a method, and following supertraits.
    pub(super) fn bound_names_on(&self, param: &str) -> Vec<String> {
        self.bounds_on(param).into_iter().map(|b| b.name).collect()
    }

    /// The bounds a projection carries.
    ///
    /// `Self::Key` has no impl to look at — which type it is, is exactly what
    /// the impl has not been chosen yet to say — so what it promises is what
    /// the trait declared for it: `type Key: Show` and nothing else. The owner
    /// is a rigid parameter, and its own bounds name the traits that could
    /// have declared this associated type.
    pub(super) fn assoc_bounds(&self, owner: &Type, name: &str) -> Vec<Bound> {
        let Type::Param(param) = owner else { return Vec::new() };
        let declared = self.bound_names_on(param);
        traits::with_supertraits(&self.types.traits, &declared)
            .iter()
            .filter_map(|t| self.types.traits.traits.get(t))
            .filter_map(|def| def.assoc_types.iter().find(|a| a.name == name))
            .flat_map(|a| a.bounds.iter().cloned())
            .collect()
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
            // A bound's arguments are written in terms of the signature's own
            // parameters — `fn f<T: Convert<U>, U>` — so they mean nothing
            // until this instantiation says what those parameters became.
            let mapping: HashMap<&str, Type> = signature
                .generics
                .iter()
                .map(|g| g.as_str())
                .zip(args.iter().cloned())
                .collect();
            let range = self.body.range(id);

            for (arg, required) in args.iter().zip(&bounds) {
                let arg = self.unifier.zonk(arg);
                for wanted in required {
                    // A trait that does not exist is reported where it is
                    // written, not once per use of the function.
                    if !self.types.traits.traits.contains_key(&wanted.name) {
                        continue;
                    }
                    let at: Vec<Type> = wanted
                        .args
                        .iter()
                        .map(|a| self.unifier.zonk(&crate::unify::substitute(a, &mapping)))
                        .collect();
                    if !self.satisfies_at(&wanted.name, &at, &arg) {
                        let called = traits::readable_key(&name);
                        // The bound *as the instantiation makes it*, not as it
                        // was written: `Convert<U>` names no type the reader
                        // can go and look at, and `Convert<Bool>` is the whole
                        // of what is wrong here.
                        let wanted = Bound { name: wanted.name.clone(), args: at };
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
        self.satisfies_at(wanted, &[], ty)
    }

    /// The same question about a trait used at particular arguments.
    ///
    /// An empty `args` is the wide question — "does this implement `Convert` at
    /// all" — which is what a bound written as a bare name asks and what every
    /// caller here asked before bounds carried their arguments.
    pub(super) fn satisfies_at(&self, wanted: &str, args: &[Type], ty: &Type) -> bool {
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
            Type::Param(p) => self.bounds_answer(&self.bounds_on(p), wanted, args),
            // A projection is rigid in the same way, and its bounds come from
            // the associated type's declaration rather than from a signature.
            Type::Assoc { owner, name } => {
                let declared = self.assoc_bounds(owner, name);
                self.bounds_answer(&declared, wanted, args)
            }
            other => self.types.traits.satisfies_at(wanted, args, other),
        }
    }

    /// Whether a list of declared bounds discharges `wanted` at `args`.
    fn bounds_answer(&self, declared: &[Bound], wanted: &str, args: &[Type]) -> bool {
        // A bound of the same name is the direct answer, and it answers at its
        // own arguments: `T: Convert<Bool>` does not discharge a
        // `Convert<String>` it is passed to.
        if declared.iter().any(|b| b.name == wanted && arguments_match(&b.args, args)) {
            return true;
        }
        // Otherwise the trait can only be reached through a supertrait, and a
        // supertrait list is bare names — `trait Sub: Convert<A>` records
        // `Convert` and nothing else. There is no argument information to be
        // strict with, so this stays the wide answer rather than refusing a
        // program on a fact nobody recorded.
        let names: Vec<String> = declared.iter().map(|b| b.name.clone()).collect();
        traits::with_supertraits(&self.types.traits, &names).iter().any(|t| t == wanted)
    }
}

/// Whether a bound's own arguments answer the ones being asked for.
///
/// The rule [`crate::traits::ImplDef::answers_at`] uses, for the same reason:
/// nothing asked is nothing to disagree with, and an argument inference has not
/// solved is the absence of an answer rather than a wrong one.
fn arguments_match(mine: &[Type], wanted: &[Type]) -> bool {
    if wanted.is_empty() {
        return true;
    }
    mine.len() == wanted.len()
        && mine.iter().zip(wanted).all(|(mine, wanted)| match wanted {
            Type::Unknown | Type::Var(_) | Type::Never => true,
            wanted => mine == wanted,
        })
}
