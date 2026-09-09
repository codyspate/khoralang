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

/// How many fields an unboxed type may carry, in its widest variant.
///
/// Three, because that is what `Step` needs -- a tag, the successor and the
/// item -- and an aggregate that size still returns in registers on every
/// target here. A number to raise with a benchmark rather than an argument.
pub const MAX_PAYLOAD_FIELDS: usize = 3;

/// How many words an unboxed value may occupy in total, tag included.
///
/// Counted transitively, because an unboxed field is laid out inline and its
/// own fields with it, and across variants, because they share the space
/// rather than each having their own. Four, so that a `Step` holding a
/// successor and an item fits and a nest of records does not quietly become a
/// memcpy.
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
/// Both halves are built, and `Any` is what a build decides with unless
/// `KHORA_UNBOXED=0` turns the whole thing off. What keeps `Scalars` here is
/// the tests below, which read the two answers against `std` and are the
/// clearest statement of what the second half added.
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
    /// **If more than one variant carries a payload, is every field a word?**
    /// A record has one carrier, and so do `Option`, `Step` and `Range`; those
    /// are laid out at their own field types and nothing about them changed.
    /// `Result` has two, carrying different things, so the two share their
    /// space -- and sharing is defined here only for fields a machine word
    /// wide, because that is the width every one of them can be read at.
    /// `Result<Int, String>` qualifies and `Result<Decimal, E>` does not,
    /// since a `Decimal` held inline is three words and there is no one width
    /// to share. Nullary cases alongside are free, since they lay out nothing.
    ///
    /// **Is the payload small?** [`MAX_PAYLOAD_FIELDS`] fields, in the widest
    /// variant, and [`MAX_WORDS`] words across the whole value.
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
    /// The *first* carrying variant's, which is the only one for a record and
    /// for every type that qualified before unions did. Where there are two,
    /// ask [`Self::payloads`] instead: this one answers about a variant it
    /// does not name, which is only ever right when there is a single choice.
    ///
    /// Empty where every case is nullary, which is a tag and nothing else.
    /// `None` where the type is boxed.
    pub fn payload(&self, ty: &Type) -> Option<Vec<Type>> {
        Some(self.payloads(ty)?.into_iter().next().map(|(_, f)| f).unwrap_or_default())
    }

    /// Every carrying variant's fields, by its index among the declared cases.
    ///
    /// The index is the tag: the declarations are kept in the order the type
    /// map reports them, which is the order the backend counts tags in.
    ///
    /// Empty where every case is nullary. `None` where the type is boxed.
    pub fn payloads(&self, ty: &Type) -> Option<Vec<(u32, Vec<Type>)>> {
        if !self.holds(ty) {
            return None;
        }
        let Type::Adt { name, home, args } = ty else { return Some(Vec::new()) };
        let (params, variants) = self.declarations.get(&(name.clone(), home.clone()))?;
        Some(
            variants
                .iter()
                .enumerate()
                .filter(|(_, v)| !v.fields.is_empty())
                .map(|(tag, v)| {
                    (tag as u32, v.fields.iter().map(|f| substituted(f, params, args)).collect())
                })
                .collect(),
        )
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
        if carrying.is_empty() {
            return true;
        }
        if carrying.iter().any(|v| v.fields.len() > MAX_PAYLOAD_FIELDS) {
            return false;
        }
        if self.reaches_itself(&id) {
            return false;
        }

        let payloads: Vec<Vec<Type>> = carrying
            .iter()
            .map(|v| v.fields.iter().map(|f| substituted(f, params, args)).collect())
            .collect();
        let mut every = payloads.iter().flatten();
        if every.clone().any(|f| {
            matches!(f, Type::Var(_) | Type::Param(_) | Type::Assoc { .. } | Type::Applied { .. })
        }) {
            return false;
        }
        if self.fields == Fields::Scalars && every.any(|f| self.counted(f)) {
            return false;
        }

        seen.push(id);
        let held = self.slot_words(&payloads, seen);
        seen.pop();
        let Some(held) = held else { return false };
        tag_words(variants.len()) + held <= MAX_WORDS
    }

    /// How wide the shared slots are, or `None` where they cannot be shared.
    ///
    /// **Slot `i` holds field `i` of whichever variant the tag names.** Where
    /// the variants agree about what that is, the slot is that type and as
    /// wide as it likes -- which is every type with a single carrying case, so
    /// a `Range` inside a `Step` is two words laid out where they fall and
    /// nothing about it changed.
    ///
    /// Where they disagree, the slot has to hold either, and the only width
    /// both can be read at is a machine word: a pointer goes in as its
    /// address, a float as its bits, a narrow integer widened. **A value held
    /// inline cannot**, because it is an aggregate and there is no bit pattern
    /// of one that fits in a register the other variant reads as a pointer --
    /// so a slot two variants disagree about admits scalars and pointers and
    /// refuses anything laid out flat. `docs/errata.md` 86.
    fn slot_words(&self, payloads: &[Vec<Type>], seen: &mut Vec<TypeId>) -> Option<usize> {
        let widest = payloads.iter().map(Vec::len).max().unwrap_or(0);
        let mut total = 0;
        for index in 0..widest {
            let here: Vec<&Type> = payloads.iter().filter_map(|p| p.get(index)).collect();
            let first = *here.first()?;
            if here.iter().all(|t| *t == first) {
                total += self.words(first, seen);
                continue;
            }
            if here.iter().any(|t| self.words(t, seen) != 1 || self.qualifies(t, seen)) {
                return None;
            }
            // **And they must agree about whether that word is counted.**
            //
            // The rule above admits scalars and pointers into one slot, which
            // is true of how they are *read*. It is not true of how they are
            // released: the reference-counting plan plans a slot from its
            // static type and cannot consult the tag, so a slot holding a
            // pointer under one variant and an integer under the other is
            // released one way for both. `Result<Int, Bad>` where `Bad` is
            // boxed is exactly that shape -- slot zero is an `Int` under `Ok`
            // and a counted pointer under `Err` -- and it compiled, then
            // decremented a refcount through the integer 5. `khora check` and
            // `khora build` were both clean and the program died with SIGILL
            // on the *success* path.
            //
            // A boxed ADT slips past the check above precisely because it is
            // boxed: one word, and `qualifies` is false *because* it is a
            // pointer. So the pointer-ness has to be asked about directly.
            // Roadmap 16.6.
            // **Agreeing that the word is counted is not enough: it has to be
            // the same counted thing.** The check below used to compare
            // `counted` across the variants and admit the slot when they
            // agreed. That is still wrong wherever they agree on `true`,
            // because releasing a counted word is type-specific -- a `Str` and
            // a boxed ADT are both one counted pointer and are not released by
            // the same code. `Result<String, Bad>`, where `Bad` mixes a boxed
            // and an unboxed payload and is therefore itself boxed, is exactly
            // that shape: slot zero is a `Str` under `Ok` and a boxed `Bad`
            // under `Err`, both counted, both admitted -- and the program dies
            // with SIGILL on a `match` over what `attempt` answered.
            //
            // A slot the variants disagree about may therefore hold only words
            // nothing counts. Where they agree about the type, the equality
            // branch above has already taken it and this never runs, so the
            // layouts that pay are untouched. Roadmap 16.6, second half.
            if here.iter().any(|t| self.counted(t)) {
                return None;
            }
            total += 1;
        }
        Some(total)
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
        let carrying: Vec<Vec<Type>> = variants
            .iter()
            .filter(|v| !v.fields.is_empty())
            .map(|v| v.fields.iter().map(|f| substituted(f, params, args)).collect())
            .collect();
        if carrying.is_empty() {
            return 1;
        }
        seen.push(id);
        // The slots, because the variants share them rather than each getting
        // their own -- so what a value of this type occupies is what the union
        // of them needs, which is the same number the layout is built from.
        let inner = self.slot_words(&carrying, seen).unwrap_or(0);
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
