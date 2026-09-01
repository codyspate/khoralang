//! Calls: what is being called, with what, and what comes back.
//!
//! Four shapes reach this — a plain function, a method, a trait function
//! reached without a receiver, and a call through a value — and they differ
//! mostly in how the signature is found. Once it is, `apply` is common: check
//! the arguments, instantiate the parameters, and hand the row work to
//! `effects`.

use super::*;

impl<'a> Checker<'a> {
    pub(super) fn infer_call(
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

    pub(super) fn infer_call_inner(
        &mut self,
        callee: ExprId,
        args: &[ExprId],
        hint: Option<Type>,
        range: TextRange,
    ) -> Type {
        // A constructor call builds its ADT.
        if let Expr::Path(resolution) = self.body.expr(callee).clone() {
            if let Some((home, owner, case)) = variant_case(&resolution) {
                if let Some(variant) = self.types.variant_of(home.as_ref(), &owner, &case).cloned()
                {
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

    /// Why a call's callee is not a function.
    ///
    /// **The interesting case is a capability with a function's name.**
    /// `fn f() -> () with { nursery: Nursery }` binds `nursery` in the body,
    /// and it shadows `std::core`'s `nursery` exactly as any other binding of
    /// that name would. So the body's `nursery(..)` calls the capability,
    /// which is a record of operations and not a function, and the message
    /// said only ``Nursery` is not a function` -- true, unhelpful, and about a
    /// type the reader never wrote.
    ///
    /// The guide warns about this for lambdas. It happens for a declared
    /// capability row too, and it happens to the names most likely to collide,
    /// because a capability is usually called after the function that installs
    /// it.
    fn not_callable(&self, callee: Option<ExprId>, zonked: &Type) -> String {
        let named = callee.and_then(|id| match self.body.expr(id) {
            khora_hir::body::Expr::Local(local) => Some(self.body.local(*local).name.clone()),
            _ => None,
        });
        let head = traits::head_of(zonked);
        let is_effect = head.as_deref().is_some_and(|h| self.types.effects.contains(h));
        match (named, is_effect) {
            (Some(name), true) => format!(
                "`{name}` here is the capability this function requires, of type `{zonked}`, \
                 and it shadows any function of the same name. Call one of its operations \
                 (`{name}.something(..)`), or give the capability another name"
            ),
            _ => format!("`{zonked}` is not a function, so it cannot be called"),
        }
    }

    /// Checks a call whose callee is an ordinary value of function type.
    ///
    /// This is also where a call is charged to the enclosing function. The
    /// rows come from the callee's *type*, not from a signature looked up by
    /// name, which is what makes calling an effectful function through a
    /// variable — or a parameter, or a field — check the same as calling it
    /// directly.
    pub(super) fn apply(
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
                self.error(self.not_callable(callee, &zonked), range);
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
    pub(super) fn callee_label(&self, callee: ExprId) -> String {
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

    /// Why a method was not found, which is two different situations.
    ///
    /// ``Nursery has no method `adopt` `` was said to somebody who had
    /// imported `nursery` and not `Nursery`, and it is false twice over:
    /// `Nursery` has that operation, and nothing was misspelled. The
    /// capability's *type* arrived from `std::core`'s signature without
    /// anybody naming it, and a trait's methods need the trait in scope --
    /// Rust's rule, and a defensible one, but not one the message mentioned.
    ///
    /// So: nothing known about the head at all means nothing was imported and
    /// the fix is an import line. A head that is known without this method is
    /// a spelling mistake, and the original message is right about it.
    fn no_such_method(&self, self_ty: &Type, method: &str) -> String {
        let head = traits::head_of(self_ty);
        // **An effect is imported into `effects`, not into the trait table**,
        // so asking the traits alone said `Nursery` was unimported when it was
        // right there and `adpot` was a spelling mistake. Both halves, or the
        // message is wrong in exactly the case it was written for.
        let unimported = head.as_deref().is_some_and(|h| {
            !self.types.traits.knows(h) && !self.types.effects.contains(h)
        });
        match (unimported, head, traits::home_of(self_ty)) {
            (true, Some(name), Some(home)) => format!(
                "`{name}` is not imported here, so `{method}` cannot be found. Write \
                 `import {}::{{{name}}};` — a capability arrives with its type without \
                 the name being in scope, and calling a method on it needs the name",
                home.segments().join("::")
            ),
            (true, Some(name), None) => format!(
                "`{name}` is not imported here, so `{method}` cannot be found — a \
                 capability arrives with its type without the name being in scope, and \
                 calling a method on it needs the name"
            ),
            _ => format!("`{self_ty}` has no method `{method}`"),
        }
    }

    /// Resolves `receiver.method(args)` through the traits in scope.
    ///
    /// Returns `None` when the receiver has a *field* of that name, so a record
    /// holding a function keeps working — the field reading is the more
    /// specific one and wins, exactly as it does in Rust.
    pub(super) fn infer_method_call(
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
        // There, and not for this file. Reported here rather than falling
        // through to "has no method", which would send the reader looking for
        // a spelling mistake instead of at a missing keyword.
        if let Some(hidden) = self.types.traits.inherent_hidden(&self_ty, method) {
            // The head rather than `self_ty`: the advice is to write a keyword
            // in `impl<A> Option<A>`, and `Option<Int>::unwrap_or` points at a
            // block that does not exist.
            let owner = hidden.head.clone();
            for arg in args {
                self.infer(*arg);
            }
            self.error(self.not_exported(&owner, method), range);
            return Some(Type::Unknown);
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
                self.error(self.no_such_method(&self_ty, method), range);
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
    pub(super) fn infer_bounded_method(
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
    pub(super) fn call_signature(
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

    pub(super) fn signature_for(&self, key: &str, _self_ty: &Type) -> Option<Signature> {
        self.types.signatures.get(key).cloned()
    }

    /// What to say about a method that exists and is not this file's to call.
    ///
    /// Names the fix, because it is one word in a file the reader may not have
    /// thought to open — and because the *other* fix is real too: a helper
    /// that only its own module should reach is a method whose caller should
    /// be somewhere else.
    pub(super) fn not_exported(&self, owner: &str, method: &str) -> String {
        format!(
            "`{owner}::{method}` is not exported, so only the module that declares it may call it. Write `pub fn {method}` there if it is part of the type's interface — otherwise this call belongs inside that module"
        )
    }

    /// The type of `Owner::name`, where `Owner` is a trait or a bounded type
    /// parameter and `name` is one of the trait's functions.
    ///
    /// `Self` is left as a fresh variable when the owner is a trait, so the
    /// expected type decides which impl runs — `Applicative::pure(x)` in a
    /// position wanting `Option<Int>` resolves to `Option`'s. When the owner is
    /// a type parameter, `Self` is that parameter and the choice is the
    /// caller's.
    pub(super) fn type_of_trait_item(&mut self, at: ExprId, owner: &str, name: &str) -> Type {
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
        if self.types.traits.inherent_hidden(&self_ty, name).is_some() {
            let range = self.body.range(at);
            self.error(self.not_exported(owner, name), range);
            return Type::Unknown;
        }
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
        //
        // **Asked of any owner, not only a declared one.** This used to be
        // gated on `types.adts`, which holds the types the program *declares*
        // -- so `Decimal::show(x)` resolved and `Int::show(x)` did not, with
        // the same impl written the same way three hundred lines apart in the
        // same file. `Int`, `Bool`, `String` and the fixed-width integers have
        // no declaration to be in that map, and a caller has no way to know
        // which side of the line a type falls on. The search itself is by the
        // impl's own head, so an owner with no impls simply finds nothing and
        // falls through to the trait lookup below, which is what `Show::show`
        // and a bounded type parameter both need.
        {
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
                } else if bounds.is_empty() && self.names_a_type(owner) {
                    // A type asked for a function it has not got. Saying it
                    // "is not a trait" answers a question the caller did not
                    // ask -- and for a builtin it is the only thing they were
                    // told.
                    format!("`{owner}` has no function named `{name}`")
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

    pub(super) fn type_of_resolution(&mut self, at: ExprId, resolution: &khora_hir::Resolution) -> Type {
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
            khora_hir::Resolution::Variant { module, type_name, name } => {
                // A nullary constructor is a value; one with a payload is
                // reached through a call, handled in `infer_call`.
                match self.types.variant_of(Some(module), type_name, name) {
                    Some(_) => self.instantiate_adt(type_name).0,
                    None => Type::Unknown,
                }
            }
            khora_hir::Resolution::Unsupported(_) => Type::Unknown,
        }
    }
}
