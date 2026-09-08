//! Which types need not be heap objects.
//!
//! A Khora record has **no identity**: it is an immutable value compared
//! structurally, so whether it lives behind a pointer is a representation
//! choice the compiler may make and no program can observe. That is the
//! property most languages lack, and it is the whole licence for this module.
//!
//! What it buys is measured rather than assumed. `bench/iteration` walks a
//! list three ways and the idiomatic one is 83ms against 18ms for the same
//! loop written out. `for` desugars to a `loop` over `Step`, and `Step` is an
//! ordinary ADT -- so the difference is one object an element: two runtime
//! calls, a header write, field stores and loads, where the hand-written loop
//! loads a tag and adds. Removing that object's *allocation* bought nothing
//! (`docs/roadmap.md` § Unboxed records), because the cost was never the
//! allocation. It is the object.

use crate::{Type, TypeMap, VariantInfo};
use std::collections::HashMap;

/// A type identity: its name and the module that declares it.
///
/// Both halves, for the reason `docs/errata.md` 46 gives four times over: two
/// modules may each declare a `Pair`, and answering by name alone hands one
/// the other's layout.
pub type TypeId = (String, Option<khora_hir::ModulePath>);

/// How many fields an unboxed type may carry.
///
/// Three, because that is what `Step` needs -- a tag, the successor and the
/// item -- and an aggregate that size still returns in registers on every
/// target here. A number to raise with a benchmark rather than an argument.
pub const MAX_PAYLOAD_FIELDS: usize = 3;

/// How many words an unboxed value may occupy in total, tag included.
///
/// Counted transitively, because an unboxed field is laid out inline and its
/// own fields with it. Four, so that a `Step` holding a successor and an item
/// fits and a nest of records does not quietly become a memcpy.
pub const MAX_WORDS: usize = 4;

/// How deep to look before giving up and leaving a type boxed.
///
/// A guard rather than a limit: the cycle check below already terminates, and
/// this is here so that a shape it does not anticipate costs an allocation
/// rather than the compiler's stack.
const MAX_DEPTH: usize = 16;

/// The types a program may hold without a header.
///
/// **Asked of an instantiated type, not of a declaration.** `Step<S, A>` has
/// two fields and both are parameters, which have no width; `Step<List<Int>,
/// Int>` has a pointer and a word. Deciding from declarations answers "no" for
/// every generic type, which is every type this exists for. So the
/// declarations are what is kept, and the arguments go in when the question is
/// asked.
#[derive(Debug, Clone)]
pub struct Unboxed {
    declarations: HashMap<TypeId, (Vec<String>, Vec<VariantInfo>)>,
    fields: Fields,
}

/// Which fields an unboxed value may carry.
///
/// **It was a staging control and its work is done.** A value held inline with
/// a pointer among its fields needs those fields counted when it is copied and
/// released when it is dropped -- there is no header to hang that on any more.
/// `Scalars` is the half of the change that needs none of it, and it existed so
/// that laying values out flat could be proved before ownership moved too.
///
/// Both halves are built and `KHORA_UNBOXED=1` decides with `Any`. What keeps
/// `Scalars` here is the tests below, which read the two answers against `std`
/// and are the clearest statement of what the second half added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fields {
    /// Words only: nothing inside is reference counted.
    Scalars,
    /// Pointers too, each counted by the value that holds them.
    Any,
}

/// Indexes a program's declarations. The deciding happens per use.
pub fn decide(map: &TypeMap, fields: Fields) -> Unboxed {
    let mut declarations: HashMap<TypeId, (Vec<String>, Vec<VariantInfo>)> = HashMap::new();
    for v in &map.variants {
        let id = (v.type_name.clone(), v.home.clone());
        let params = map
            .adts_in
            .get(&id)
            .or_else(|| map.adts.get(&v.type_name))
            .cloned()
            .unwrap_or_default();
        declarations.entry(id).or_insert_with(|| (params, Vec::new())).1.push(v.clone());
    }
    Unboxed { declarations, fields }
}

impl Unboxed {
    /// Whether values of this type are held inline.
    ///
    /// Four questions, and a type answers all of them or stays boxed.
    ///
    /// **Is it recursive?** A `List` holds a `List`, so holding one inline has
    /// no finite size. Caught transitively by the `seen` stack, so a type
    /// holding a record that holds itself is refused with it.
    ///
    /// **Does one variant carry the payload?** A record has exactly one, and
    /// so do `Option`, `Step` and `Range`. Two variants carrying *different*
    /// payloads need a union and a size taken across them, which is a later
    /// question; nullary cases alongside are free, since they lay out nothing.
    ///
    /// **Is the payload small?** [`MAX_PAYLOAD_FIELDS`], counted in fields,
    /// because every field is a machine word in this representation.
    ///
    /// **Is anything `mut`?** A record with a mutable field has observable
    /// identity after all -- a write through one holder must be seen by
    /// another -- so it stays behind a pointer. The one question here that is
    /// about meaning rather than layout.
    pub fn holds(&self, ty: &Type) -> bool {
        self.qualifies(ty, &mut Vec::new())
    }

    /// The fields an unboxed value carries, at this instantiation.
    ///
    /// Empty where every case is nullary, which is a tag and nothing else.
    /// `None` where the type is boxed.
    pub fn payload(&self, ty: &Type) -> Option<Vec<Type>> {
        if !self.holds(ty) {
            return None;
        }
        let Type::Adt { name, home, args } = ty else { return Some(Vec::new()) };
        let (params, variants) = self.declarations.get(&(name.clone(), home.clone()))?;
        Some(match variants.iter().find(|v| !v.fields.is_empty()) {
            Some(v) => v.fields.iter().map(|f| substituted(f, params, args)).collect(),
            None => Vec::new(),
        })
    }

    fn qualifies(&self, ty: &Type, seen: &mut Vec<TypeId>) -> bool {
        let Type::Adt { name, home, args } = ty else { return false };
        let id = (name.clone(), home.clone());
        if seen.len() >= MAX_DEPTH {
            return false;
        }
        let Some((params, variants)) = self.declarations.get(&id) else { return false };
        if variants.iter().any(|v| v.mutable.iter().any(|m| *m)) {
            return false;
        }
        let carrying: Vec<&VariantInfo> =
            variants.iter().filter(|v| !v.fields.is_empty()).collect();
        if carrying.len() > 1 {
            return false;
        }
        let Some(one) = carrying.first() else { return true };
        if one.fields.len() > MAX_PAYLOAD_FIELDS {
            return false;
        }
        if self.reaches_itself(&id) {
            return false;
        }

        let fields: Vec<Type> =
            one.fields.iter().map(|f| substituted(f, params, args)).collect();
        if fields.iter().any(|f| matches!(f, Type::Var(_) | Type::Param(_) | Type::Assoc { .. } | Type::Applied { .. }))
        {
            return false;
        }
        if self.fields == Fields::Scalars && fields.iter().any(|f| self.counted(f)) {
            return false;
        }
        seen.push(id);
        let total: usize =
            tag_words(variants.len()) + fields.iter().map(|f| self.words(f, seen)).sum::<usize>();
        seen.pop();
        total <= MAX_WORDS
    }

    /// How many machine words a value of this type occupies inline.
    ///
    /// One for anything behind a pointer, which is what a boxed field is --
    /// **that is the case `Step` needs and the roadmap's all-scalar criterion
    /// refused.** `Step`'s successor is a `List`; a `List` cannot be inline,
    /// so the field is a pointer, so the `Step` around it lays out in three
    /// words and needs no header of its own.
    fn words(&self, ty: &Type, seen: &mut Vec<TypeId>) -> usize {
        let Type::Adt { name, home, args } = ty else { return 1 };
        let id = (name.clone(), home.clone());
        if seen.contains(&id) || !self.qualifies(ty, seen) {
            // Boxed, so a pointer.
            return 1;
        }
        let Some((params, variants)) = self.declarations.get(&id) else { return 1 };
        let Some(one) = variants.iter().find(|v| !v.fields.is_empty()) else { return 1 };
        let fields: Vec<Type> =
            one.fields.iter().map(|f| substituted(f, params, args)).collect();
        seen.push(id);
        let inner: usize = fields.iter().map(|f| self.words(f, seen)).sum();
        seen.pop();
        tag_words(variants.len()) + inner
    }

    /// Whether a field is a pointer somebody has to count.
    ///
    /// An unboxed field is not: it is laid out inline, and whatever *it* holds
    /// is asked about in turn.
    fn counted(&self, field: &Type) -> bool {
        match field {
            Type::Str | Type::Fn { .. } | Type::Tuple(_) => true,
            Type::Adt { .. } => !self.holds(field),
            _ => false,
        }
    }

    /// Whether a type's fields lead back to it.
    ///
    /// **Followed through every ADT field, boxed or not**, which is
    /// deliberately more than is strictly required: a pointer breaks a cycle,
    /// so `Node = { next: Option<Node> }` could in principle be laid out with
    /// `Option<Node>` boxed. Deciding that needs a fixed point over "which
    /// types are boxed", and the two questions define each other. Answering
    /// the conservative one costs a few types their inline layout and cannot
    /// produce one of infinite size.
    fn reaches_itself(&self, start: &TypeId) -> bool {
        let mut seen: Vec<TypeId> = Vec::new();
        let mut queue: Vec<TypeId> = vec![start.clone()];
        let mut first = true;
        while let Some(id) = queue.pop() {
            if !first && &id == start {
                return true;
            }
            first = false;
            if seen.contains(&id) {
                continue;
            }
            seen.push(id.clone());
            let Some((_, variants)) = self.declarations.get(&id) else { continue };
            for v in variants {
                for f in &v.fields {
                    collect_adts(f, &mut queue);
                }
            }
        }
        false
    }
}

/// Every ADT named anywhere inside a type, including its arguments.
///
/// The arguments matter: `Cons(head: A, tail: List<A>)` reaches `List` through
/// the *head* of `List<A>` and not through `A`, and a walk that looked only at
/// the head would miss `Option<Node>` reaching `Node`.
fn collect_adts(ty: &Type, out: &mut Vec<TypeId>) {
    match ty {
        Type::Adt { name, home, args } => {
            out.push((name.clone(), home.clone()));
            for a in args {
                collect_adts(a, out);
            }
        }
        Type::Tuple(items) => items.iter().for_each(|t| collect_adts(t, out)),
        Type::Applied { head, args } => {
            collect_adts(head, out);
            args.iter().for_each(|a| collect_adts(a, out));
        }
        _ => {}
    }
}

/// Whether a type needs a tag at all.
///
/// **A record does not.** One case is one shape, so there is nothing to
/// discriminate and nothing to store: `Range` is two integers, not a tag and
/// two integers. That is a word saved on every record in the language, and it
/// is the difference between `Step<Range, Int>` fitting and not.
fn tag_words(cases: usize) -> usize {
    usize::from(cases > 1)
}

impl Default for Unboxed {
    /// Nothing inline, which is what every representation decision answered
    /// before this module existed.
    fn default() -> Self {
        Unboxed { declarations: HashMap::new(), fields: Fields::Scalars }
    }
}

/// A declared field type with this use's arguments put in.
fn substituted(field: &Type, params: &[String], args: &[Type]) -> Type {
    if params.is_empty() || args.is_empty() {
        return field.clone();
    }
    let mapping: HashMap<&str, Type> =
        params.iter().map(String::as_str).zip(args.iter().cloned()).collect();
    crate::unify::substitute(field, &mapping)
}
