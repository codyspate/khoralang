//! What every type name in a file refers to.
//!
//! Built per file and then merged across imports, which is where declaration
//! identity is enforced: a type carries the module that declares it, so two
//! modules may each have a `Point` and neither is handed the other's layout.
//! Errata 46.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypeMap {
    pub signatures: HashMap<String, Signature>,
    pub variants: Vec<VariantInfo>,
    /// Bodies this module never named, reachable from the ones it did.
    ///
    /// **Whether a type is shareable is a fact about the type, not about the
    /// importer.** `std::db`'s `Cell` holds a `Decimal`, so answering "may two
    /// fibers hold a `Cell`" means looking inside `Decimal` -- and a module
    /// that imported `Cell` without also importing `Decimal` could not, so it
    /// got `false`. The same type, the same question, two answers depending on
    /// an unrelated line at the top of the file.
    ///
    /// Kept apart from [`TypeMap::variants`] rather than merged into it because
    /// these names are deliberately **not in scope**: a record literal must not
    /// infer as a type the file cannot name, and `Decimal::scaled` must still
    /// be an unknown path. Only the shareability walk reads this, through
    /// [`TypeMap::bodies_of`].
    pub(crate) reachable: Vec<VariantInfo>,
    /// The type parameters of those bodies.
    ///
    /// Needed for the same reason and kept apart for the same reason. Without
    /// it `Row`'s `List<Cell>` found `List`'s body but not that its parameter
    /// is called `A`, so the substitution was empty, `A` stayed a type the
    /// caller chooses, and a list of anything was unshareable.
    pub(crate) reachable_adts: HashMap<String, Vec<String>>,
    /// Generic parameters of each declared type, by name.
    pub adts: HashMap<String, Vec<String>>,
    /// The traits and impls this file declares.
    pub traits: traits::Traits,
    /// The kind of every named type, so an impl can be checked against the
    /// kind its trait requires.
    pub kinds: HashMap<String, traits::Kind>,
    /// The type names this file declares itself, before any import.
    ///
    /// The one question an orphan rule needs, and `adts` cannot answer it: an
    /// imported type is in there too. `docs/design/sharing.md`.
    pub declared_here: HashSet<String>,
    /// What each type name written in this file refers to.
    ///
    /// Carried on the map because everything that turns a name into a
    /// [`Type`] needs it, and because the checker asks the same question about
    /// a mention that `type_map` asked about a declaration.
    pub homes: TypeHomes,
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
    /// Whether a recorded variant is the one being asked about.
    ///
    /// A `home` of `None` asks by name alone, which is what a caller holding
    /// only a spelling can do — the compiler's own types, and the backend,
    /// which works on names monomorphization has already made unique. A
    /// `Some` asks exactly, and every lookup driven by a [`Type`] does.
    fn is_the_same_type(v: &VariantInfo, home: Option<&khora_hir::ModulePath>, name: &str) -> bool {
        v.type_name == name && home.is_none_or(|wanted| v.home.as_ref() == Some(wanted))
    }

    pub(crate) fn variants_of(
        &self,
        home: Option<&khora_hir::ModulePath>,
        type_name: &str,
    ) -> Vec<&VariantInfo> {
        self.variants.iter().filter(|v| Self::is_the_same_type(v, home, type_name)).collect()
    }

    /// A constructor, found by the type it belongs to *and* its own name.
    ///
    /// Both halves are required. Case names are not unique across a program —
    /// two types may each have a `Some` — and `Resolution::Variant` carries the
    /// type for exactly this reason. Looking one up by its bare name resolves
    /// `Maybe::Some` to `Option::Some` whenever `Option` was declared first,
    /// which is a wrong tag rather than an error.
    pub fn variant_of(
        &self,
        home: Option<&khora_hir::ModulePath>,
        type_name: &str,
        case: &str,
    ) -> Option<&VariantInfo> {
        self.variants
            .iter()
            .find(|v| v.name == case && Self::is_the_same_type(v, home, type_name))
    }

    /// The variant a type's own record shape is recorded as, found by identity.
    ///
    /// The lookup every field access wants: a `Type` knows its module, so
    /// there is no reason for it to ask by spelling.
    pub fn record_of(&self, ty: &Type) -> Option<&VariantInfo> {
        let Type::Adt { name, home, .. } = ty else { return None };
        self.variant_of(home.as_ref(), name, name)
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
    /// Whether this file is the one that declares `ty`.
    pub fn declares(&self, ty: &Type) -> bool {
        match ty {
            Type::Applied { head, .. } => self.declares(head),
            Type::Adt { name, .. } => self.declared_here.contains(name),
            _ => false,
        }
    }

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
                self.bodies_of(name).next().is_none() && !self.effects.contains(name)
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
            if self.bodies_of(name).next().is_none() && !self.effects.contains(name) {
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
            Type::Adt { name, args, .. } => {
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
                    .bodies_of(name)
                    .any(|v| v.fields.iter().any(|t| self.holds_a_closure(t, visiting)));
                visiting.pop();
                found
            }
            _ => false,
        }
    }

    fn shareable(&self, ty: &Type, visiting: &mut Vec<String>, bounded: &[String]) -> bool {
        self.shareable_with(ty, visiting, bounded, false)
    }

    /// Every known body for `name`, in scope or merely reachable.
    ///
    /// The one place [`TypeMap::reachable`] is read. See its note for why the
    /// two lists are separate.
    fn bodies_of<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a VariantInfo> + 'a {
        self.variants.iter().chain(self.reachable.iter()).filter(move |v| v.type_name == name)
    }

    /// The type parameters of `name`, in scope or merely reachable.
    fn params_of(&self, name: &str) -> Vec<String> {
        match self.adts.get(name) {
            Some(found) => found.clone(),
            None => self.reachable_adts.get(name).cloned().unwrap_or_default(),
        }
    }

    /// Whether this module may *assert* that two fibers can hold `ty`.
    ///
    /// **A `Ptr` is refused because the compiler cannot know, and that is
    /// exactly when a declaring module may say.** The rule used to be "opaque
    /// types only", on the argument that an impl may only be written where
    /// there is nothing to check. That argument is about a `mut` field — where
    /// the compiler *can* see, and an assertion would be overriding knowledge —
    /// and it does not reach foreign memory, where the compiler's `false` is a
    /// conservative default rather than a finding.
    ///
    /// So: opaque, as before, or a type whose only obstacle is a pointer. A
    /// `mut` field still refuses, a closure field still refuses, and a field of
    /// somebody else's unshareable type still refuses — the vouch is only for
    /// what this module put across the ABI itself.
    ///
    /// `std::net::tls` needed it. A `rustls` configuration behind an `Arc` is
    /// immutable and safe for any number of readers, and a server that cannot
    /// be handed to the fiber answering a connection is not a server. It was
    /// smuggled across as an `Int` for a while, which is worse in every way:
    /// same sharing, no review, no diagnostic if it were wrong.
    pub fn may_vouch_for(&self, ty: &Type) -> bool {
        if self.is_opaque(ty) {
            return true;
        }
        // Both questions are asked of the *structure*, ignoring any impl on
        // `ty` itself — that is the thing being judged, and consulting it would
        // let anything vouch for itself.
        //
        // Blocked only by a pointer, and not already shareable without one. The
        // second half keeps the original rule's other edge: an assertion on a
        // type the compiler can already see is fine is not harmless, it is a
        // reader being told something is dangerous when it is not.
        self.structurally_shareable(ty, true) && !self.structurally_shareable(ty, false)
    }

    /// Whether `ty`'s *contents* allow sharing, ignoring any assertion on `ty`.
    fn structurally_shareable(&self, ty: &Type, pointers_ok: bool) -> bool {
        let Type::Adt { name, args, .. } = ty else { return false };
        let parameters = self.params_of(name);
        let mapping: HashMap<&str, Type> =
            parameters.iter().map(String::as_str).zip(args.iter().cloned()).collect();
        let mut visiting = vec![name.clone()];
        self.bodies_of(name).all(|v| {
            !v.has_mutable_field()
                && v.fields.iter().all(|t| {
                    let t = unify::substitute(t, &mapping);
                    self.shareable_with(&t, &mut visiting, &[], pointers_ok)
                })
        })
    }

    fn shareable_with(
        &self,
        ty: &Type,
        visiting: &mut Vec<String>,
        bounded: &[String],
        pointers_ok: bool,
    ) -> bool {
        match ty {
            // A row variable is not a value and carries none: `'e` is how a
            // function fails, and nobody hands a failure to a fiber. Only a
            // *type* the caller chooses has to be asked about.
            Type::Param(name) if name.starts_with('\'') => true,
            Type::Param(name) => bounded.iter().any(|b| b == name),
            Type::Fn { .. } => false,
            Type::Tuple(items) => {
                items.iter().all(|t| self.shareable_with(t, visiting, bounded, pointers_ok))
            }
            Type::Applied { head, args } => {
                self.shareable_with(head, visiting, bounded, pointers_ok)
                    && args
                        .iter()
                        .all(|t| self.shareable_with(t, visiting, bounded, pointers_ok))
            }
            Type::Adt { name, args, .. } => {
                // The trusted answer comes first, arguments included. A type
                // with no body does not necessarily *hold* its parameters —
                // `SharedFn<Request, Response, 'e>` describes a call rather
                // than a contents, and a `Request` is built inside the fiber
                // that answers it — so asking about them would refuse the one
                // thing the wrapper exists to allow. An impl asserts for every
                // instantiation, which is what makes it a thing you have to be
                // trusted to write.
                // A trusted assertion answers for any type whose module was
                // allowed to write one — opaque, or blocked only by a pointer.
                // `traits::check` refuses the rest, and a refused program is
                // not compiled, so trusting the impl here cannot outlive the
                // diagnostic.
                if self.traits.find(SHARE, ty).is_some() {
                    return true;
                }
                if self.is_opaque(ty) {
                    return false;
                }
                if !args.iter().all(|t| self.shareable_with(t, visiting, bounded, pointers_ok)) {
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
                let parameters = self.params_of(name);
                let mapping: HashMap<&str, Type> = parameters
                    .iter()
                    .map(String::as_str)
                    .zip(args.iter().cloned())
                    .collect();
                visiting.push(name.clone());
                let ok = self
                    .bodies_of(name)
                    .all(|v| {
                        !v.has_mutable_field()
                            && v.fields.iter().all(|t| {
                                let t = unify::substitute(t, &mapping);
                                self.shareable_with(&t, visiting, bounded, pointers_ok)
                            })
                    });
                visiting.pop();
                ok
            }
            // Foreign memory. Nothing on this side of the ABI knows what is
            // behind it or who else is writing there, and a pointer is exactly
            // the value whose whole purpose is to be written through.
            //
            // `pointers_ok` is set only by `may_vouch_for`, which is asking a
            // different question: not "is this shareable" but "is a pointer the
            // *only* reason it is not", which is the one case where the
            // declaring module knows something the compiler cannot.
            Type::Ptr => pointers_ok,
            _ => true,
        }
    }

}

#[salsa::tracked(returns(ref))]
pub fn type_map(db: &dyn Db, file: SourceFile) -> TypeMap {
    let parse = khora_db::parse(db, file);
    // What every type name in this file refers to, worked out before any of
    // them is read: a type is a declaration rather than a spelling, and this is
    // the only thing that knows which declaration. Errata 46.
    let homes = type_homes(db, file);
    // Everything this file declares is declared here, which is the home every
    // `VariantInfo` below is recorded under.
    let here = khora_hir::item_map(db, file).module.clone();
    let mut map = TypeMap { homes: homes.clone(), ..TypeMap::default() };
    // Which of each type's parameters are const, so `Matrix<const R, const C>`
    // gets the kind `Int -> Int -> *` rather than `* -> * -> *`.
    let mut consts: HashMap<String, Vec<bool>> = HashMap::new();

    for (index, decl) in parse.source_file().decls().enumerate() {
        match decl {
            // A test takes nothing, returns nothing, and can fail — which is
            // the only interesting thing about its signature. The row is
            // *opened* where the body is checked, because a test may fail any
            // way it likes: that is what a failing test is.
            // A bench has the same signature for the same reasons, and the
            // consequence of leaving it out is quiet: no signature means no
            // registered instance, which means `emit_function` declines to
            // declare the body, which means the entry point's registration
            // loop finds no function to point at. The build succeeds and
            // reports `no benchmarks`.
            ast::Decl::Test(_) | ast::Decl::Bench(_) => {
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
                        list.params().map(|p| type_of_syntax(p.ty().as_ref(), &generics, homes)).collect()
                    })
                    .unwrap_or_default();
                let ret = f
                    .return_type()
                    .map_or(Type::Unit, |t| type_of_syntax(Some(&t), &generics, homes));
                let requires =
                    row_of_syntax(f.with_clause().and_then(|c| c.row()).as_ref(), &generics, homes);
                let raises =
                    row_of_syntax(f.raises_clause().and_then(|c| c.row()).as_ref(), &generics, homes);
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
                    fields.push(type_of_syntax(op.ty().as_ref(), &generics, homes));
                }
                map.effects.insert(name.clone());
                map.declared_here.insert(name.clone());
                map.variants.push(VariantInfo {
                    type_name: name.clone(),
                    home: here.clone(),
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
                map.declared_here.insert(type_name.clone());
                // `type Point = { x: Int, y: Int }` is one variant carrying
                // named fields — the same shape a constructor already has, so
                // field access, construction and drop glue are all reused.
                if let Some(ast::Type::Record(r)) = t.definition() {
                    let (labels, fields) = record_fields(&r, &generics, homes);
                    let mutable = r.fields().map(|f| f.is_mut()).collect();
                    map.variants.push(VariantInfo {
                        type_name: type_name.clone(),
                        home: here.clone(),
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
                                    .map(|f| type_of_syntax(f.ty().as_ref(), &generics, homes))
                                    .collect()
                            })
                            .or_else(|| {
                                case.tuple_fields().map(|list| {
                                    list.types()
                                        .map(|t| type_of_syntax(Some(&t), &generics, homes))
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
                            home: here.clone(),
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

    map.signatures.extend(traits::impl_signatures(&parse.source_file(), homes));
    map.traits = traits::collect(&parse.source_file(), homes);

    // The impls this file's `derive` clauses expanded to, read by the same two
    // functions that just read the written ones. Nothing downstream of here
    // can tell the difference, which is the whole design: see the module
    // comment on `khora_hir::derive`.
    //
    // They are appended rather than prepended so that a type which both
    // derives `Eq` and writes one gets the duplicate reported against the
    // `derive` — the shorter of the two things to delete, and the one whose
    // author probably did not realize the other existed.
    let expanded = khora_hir::derive::derived(db, file);
    map.signatures.extend(traits::impl_signatures(&expanded.source_file(), homes));
    let mut generated = traits::collect(&expanded.source_file(), homes);
    debug_assert_eq!(generated.impls.len(), expanded.impls.len());
    for (imp, from) in generated.impls.iter_mut().zip(&expanded.impls) {
        // The range it was collected with is an offset into generated text and
        // means nothing here. `khora_hir::derive` explains why the `derive` is
        // the right thing to point at instead.
        imp.range = from.at;
    }
    map.traits.impls.append(&mut generated.impls);

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
pub(crate) fn import_inherent(exported: &TypeMap, map: &mut TypeMap) {
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
pub(crate) fn import_types(
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
                // And the bodies those fields reach, which are not in scope
                // here but have to be *visible* -- see `TypeMap::reachable`.
                let (bodies, generics) = reachable_from(exported, name);
                map.reachable.extend(bodies);
                for (reached, parameters) in generics {
                    map.reachable_adts.entry(reached).or_insert(parameters);
                }
                // **An impl travels with its type as well as with its
                // trait.** Importing a trait brings the impls that satisfy it,
                // which is right: you ask for `Show`, you get the instances.
                // But it was the *only* way one arrived, so `impl Eq for Point`
                // written beside `Point` was invisible to any file that
                // imported `Point` — and `Eq` from `std::core` was already in
                // scope there. That is the ordinary shape of a program, and it
                // made a derived impl useless outside the module it came from.
                //
                // So: visible if either half is. `Share` needed this first and
                // needed it most, being a property nobody asks for by name,
                // but nothing about the rule was ever specific to it.
                for extra in exported
                    .traits
                    .impls
                    .iter()
                    .filter(|i| i.head().as_deref() == Some(name.as_str()))
                {
                    let known = map
                        .traits
                        .impls
                        .iter()
                        .any(|i| i.trait_name == extra.trait_name && i.head() == extra.head());
                    if known {
                        // Imported twice — under two names, or from both sides
                        // at once — is still one impl, and the coherence check
                        // downstream would call it two.
                        continue;
                    }
                    let mut extra = extra.clone();
                    extra.local = false;
                    let trait_name = extra.trait_name.clone();
                    map.traits.impls.push(extra);

                    // The declaration too, or it is an impl of nothing.
                    if let Some(def) = exported.traits.traits.get(&trait_name) {
                        map.traits
                            .traits
                            .entry(trait_name.clone())
                            .or_insert_with(|| def.clone());
                    }
                    // And the methods, filed under `Trait#Head::method`.
                    // Without them the call resolves and then has no signature
                    // to be checked against.
                    let own = format!("{trait_name}#{name}::");
                    for (key, signature) in &exported.signatures {
                        if key.starts_with(&own) {
                            map.signatures.insert(key.clone(), signature.clone());
                        }
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
                //
                // Skipping what is already here, because since an impl also
                // travels with its *type* the same one can arrive twice — a
                // file importing both `Iterator` and `Range` was told that
                // `Iterator` is already implemented for `Range`, by itself.
                for imported in
                    exported.traits.impls.iter().filter(|i| &i.trait_name == name)
                {
                    let known = map.traits.impls.iter().any(|i| {
                        i.trait_name == imported.trait_name && i.head() == imported.head()
                    });
                    if known {
                        continue;
                    }
                    let mut imported = imported.clone();
                    imported.local = false;
                    map.traits.impls.push(imported);
                }
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
pub(crate) fn disagreement(outer: (&Type, &Type), inner: (&Type, &Type)) -> String {
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

pub(crate) fn generic_names(params: Option<&ast::TypeParams>) -> Vec<String> {
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

/// Every body reachable from `name`'s fields, minus `name`'s own.
///
/// A worklist over type names rather than a recursion, because a type may
/// contain itself and the visited set is the termination argument.
///
/// **Only what `exported` already has.** A module's own map holds the bodies it
/// imported, so `std::db`'s map has `Decimal` in it — that is how `Cell` was
/// checkable inside `std::db` and not outside it. Nothing is fetched from a
/// third module here; this closes the gap by carrying forward what the exporter
/// already saw, which is exactly the set the exporter used to answer the same
/// question.
fn reachable_from(
    exported: &TypeMap,
    name: &str,
) -> (Vec<VariantInfo>, Vec<(String, Vec<String>)>) {
    let mut seen: Vec<String> = vec![name.to_string()];
    let mut queue: Vec<String> = vec![name.to_string()];
    let mut found = Vec::new();
    let mut generics = Vec::new();

    while let Some(here) = queue.pop() {
        for variant in exported.variants.iter().filter(|v| v.type_name == here) {
            for field in &variant.fields {
                for mentioned in type_names(field) {
                    if seen.contains(&mentioned) {
                        continue;
                    }
                    seen.push(mentioned.clone());
                    queue.push(mentioned.clone());
                    found.extend(
                        exported.variants.iter().filter(|v| v.type_name == mentioned).cloned(),
                    );
                    if let Some(parameters) = exported.adts.get(&mentioned) {
                        generics.push((mentioned.clone(), parameters.clone()));
                    }
                }
            }
        }
    }
    (found, generics)
}

/// Every ADT name a type mentions, at any depth.
fn type_names(ty: &Type) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(ty: &Type, out: &mut Vec<String>) {
        match ty {
            Type::Adt { name, args, .. } => {
                out.push(name.clone());
                args.iter().for_each(|t| walk(t, out));
            }
            Type::Tuple(items) => items.iter().for_each(|t| walk(t, out)),
            Type::Applied { head, args } => {
                walk(head, out);
                args.iter().for_each(|t| walk(t, out));
            }
            // A function's parameters and result say nothing about what a
            // closure captured, which is the whole reason a closure is
            // unshareable. Walking into one would suggest otherwise.
            _ => {}
        }
    }
    walk(ty, &mut out);
    out
}
