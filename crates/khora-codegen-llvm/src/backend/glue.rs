//! `drop_fields`: what an object releases when its count reaches zero.
//!
//! One routine per *type* rather than per variant, switching on the tag,
//! because a drop site usually knows only the static type of what it is
//! releasing — and a routine that assumed one variant's fields would read past
//! the end of a smaller sibling.
//!
//! Read at *this* instantiation, never as declared: a generic field is a
//! parameter, a parameter is never boxed, so asking the declaration whether
//! `Box<A>` owns anything always answers no and every `Box<String>` leaks.

use super::*;
use super::driver::CLOSURE_GLUE;

impl<'ctx> Backend<'ctx> {
    /// The `drop_fields` argument for dropping a value of this type.
    ///
    /// Returns a null function pointer for anything that owns no references —
    /// a `String`, an `Int`, or an ADT whose every field is a machine word.
    /// The runtime treats null as "nothing to release", so a drop site never
    /// needs to know which case it is in.
    pub fn drop_glue(&mut self, ty: &Type) -> PointerValue<'ctx> {
        if matches!(ty, Type::Fn { .. }) {
            return self.closure_glue();
        }
        // A tuple gets the generic path: it owns whatever its elements own, and
        // `instantiated_variants` describes it the same way it describes a
        // record. Without this it took a null `drop_fields` and every boxed
        // element it held was freed with nobody releasing it.
        let name: &str = match ty {
            Type::Adt { name, .. } => name,
            // No runtime type has a tuple's shape, so none of the special cases
            // below can match, and an empty name is the honest way to say so.
            Type::Tuple(_) => "",
            _ => return self.null_pointer(),
        };

        // A region's release is the runtime's, not one generated from a field
        // layout: its finalizers live Rust-side because deferring grows the
        // list, and nothing in Khora grows a value in place. Everything else
        // about a region is ordinary — reference counted, released by the same
        // `khora_drop` every other object goes through — which is what makes
        // its finalizers run on the paths that already release a local.
        if name == runtime::REGION_TYPE {
            return self.rt.region_release.as_global_value().as_pointer_value();
        }

        // A fiber handle's release joins the fiber. Same reasoning as a
        // region's, and the same payoff: the paths that already release a
        // binding are the paths a child has to be waited for on.
        if name == runtime::FIBER_TYPE {
            return self.rt.fiber_release.as_global_value().as_pointer_value();
        }

        // A nursery's release cancels its children and waits for them.
        if name == runtime::FIBERS_TYPE {
            return self.rt.fibers_release.as_global_value().as_pointer_value();
        }

        // A cell's release lets go of what is in it. The runtime's, because
        // the value sits behind a lock it owns, and it was told how to release
        // it when the cell was opened rather than here — generated code cannot
        // reach through a `Mutex`.
        if name == runtime::SHARED_TYPE {
            return self.rt.shared_release.as_global_value().as_pointer_value();
        }

        // A channel's release frees the queue and everything abandoned in it,
        // for the same reason: the values are behind a lock the runtime owns.
        if name == runtime::CHANNEL_TYPE {
            return self.rt.channel_release.as_global_value().as_pointer_value();
        }

        // An array's release loops over its elements. The loop is the
        // runtime's because the length is a run-time value; what to do with
        // one element is generated, and travels in the object.
        if name == runtime::ARRAY_TYPE {
            return self.rt.array_release.as_global_value().as_pointer_value();
        }

        let key = ty.to_string();

        if let Some(cached) = self.drop_glue.get(&key) {
            return match cached {
                Some(f) => f.as_global_value().as_pointer_value(),
                None => self.null_pointer(),
            };
        }

        // Read the fields at *this* instantiation. A generic type's declared
        // field is a parameter, which is never boxed, so asking the declaration
        // whether `Box<A>` owns anything always answers no — and every
        // `Box<String>` in the program leaks its contents.
        let variants = self.instantiated_variants(ty);
        if !variants.iter().any(|v| v.fields.iter().any(is_boxed)) {
            self.drop_glue.insert(key, None);
            return self.null_pointer();
        }

        let void = self.ctx.void_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let f = self.module.add_function(
            &format!("kh$drop_fields${}", mangle_type(ty)),
            void.fn_type(&[ptr.into()], false),
            Some(Linkage::Internal),
        );
        // Recorded before the body exists so a recursive type — a list whose
        // tail is a list — reaches this cache instead of declaring itself
        // again forever.
        self.drop_glue.insert(key, Some(f));
        self.pending_glue.push(ty.clone());
        let _ = name;
        f.as_global_value().as_pointer_value()
    }

    /// Gives every queued `drop_fields` routine its body.
    ///
    /// Must run after all function bodies: emitting one repositions the shared
    /// builder, and inkwell's builder carries its insertion point as hidden
    /// state, so doing this mid-body would silently append a caller's next
    /// instruction to the glue routine instead.
    pub(super) fn emit_pending_drop_glue(&mut self) {
        while let Some(ty) = self.pending_glue.pop() {
            match &ty {
                Type::Adt { name, .. } if name == CLOSURE_GLUE => self.emit_closure_glue(),
                _ => self.emit_drop_glue(&ty),
            }
        }
    }

    /// An ADT's variants with this instantiation's arguments substituted in.
    ///
    /// `Box<String>` has a `String` field; the *declaration* only says `A`.
    /// A type's variants with its arguments substituted in.
    ///
    /// The declared field of a generic type is a parameter, and a parameter is
    /// never boxed — so anything that reads the declaration instead of this
    /// sees `V` where the instantiation has a `String`. Drop glue got that
    /// wrong first and leaked; field access got it wrong second and loaded a
    /// pointer as an integer.
    pub(crate) fn instantiated_variants(&self, ty: &Type) -> Vec<VariantInfo> {
        // A tuple has one shape and no declaration to look it up in, so its
        // layout *is* its type: the elements, in order, as positional fields.
        // Answering here is what gives it drop glue, field loads and pattern
        // binding without any of them learning that tuples exist.
        if let Type::Tuple(items) = ty {
            return vec![VariantInfo {
                type_name: ty.to_string(),
                home: None,
                name: ty.to_string(),
                fields: items.clone(),
                labels: (0..items.len()).map(|i| i.to_string()).collect(),
                mutable: vec![false; items.len()],
            }];
        }
        let Type::Adt { name, args, .. } = ty else { return Vec::new() };
        let declared = self.variants_for(ty);
        let generics = self.types.adts.get(name).cloned().unwrap_or_default();
        if args.is_empty() || generics.is_empty() {
            return declared;
        }
        let mapping: HashMap<&str, Type> = generics
            .iter()
            .map(String::as_str)
            .zip(args.iter().cloned())
            .collect();
        declared
            .into_iter()
            .map(|mut v| {
                v.fields = v
                    .fields
                    .iter()
                    .map(|f| khora_types::unify::substitute(f, &mapping))
                    .collect();
                v
            })
            .collect()
    }

    /// Emits one type's `drop_fields`.
    ///
    /// **One routine per type, switching on the tag** — never one per variant.
    /// A drop site knows only the static type of what it is releasing, so a
    /// routine that assumed one variant's fields would read past the end of a
    /// smaller sibling: `Nil` has no tail to load, and the byte after it
    /// belongs to the allocator. The runtime documentation says this outright,
    /// and it is the single most expensive mistake available here.
    pub(super) fn emit_drop_glue(&mut self, ty: &Type) {
        let f = match self.drop_glue.get(&ty.to_string()) {
            Some(Some(f)) => *f,
            _ => return,
        };
        let object = f.get_nth_param(0).expect("drop_fields takes an object").into_pointer_value();

        let entry = self.ctx.append_basic_block(f, "entry");
        let done = self.ctx.append_basic_block(f, "done");

        let mut cases = Vec::new();
        for (tag, variant) in self.instantiated_variants(ty).into_iter().enumerate() {
            let owned: Vec<(usize, Type)> = variant
                .fields
                .iter()
                .enumerate()
                .filter(|(_, ty)| is_boxed(ty))
                .map(|(i, ty)| (i, ty.clone()))
                .collect();
            // A variant with nothing to release needs no case at all: the
            // switch's default already falls through to the return.
            if owned.is_empty() {
                continue;
            }

            let block = self.ctx.append_basic_block(f, &format!("drop.{}", variant.name));
            cases.push((self.ctx.i32_type().const_int(tag as u64, false), block));
            self.builder.position_at_end(block);

            for (index, field_ty) in owned {
                let slot = runtime::field_pointer(self.ctx, &self.builder, object, index as u64);
                let value = self
                    .builder
                    .build_load(self.ctx.ptr_type(AddressSpace::default()), slot, "child")
                    .expect("loading an owned field");
                let glue = self.drop_glue(&field_ty);
                self.builder
                    .build_call(self.rt.drop, &[value.into(), glue.into()], "")
                    .expect("dropping a field");
            }
            self.builder.build_unconditional_branch(done).expect("branch to the return");
        }

        self.builder.position_at_end(entry);
        let tag = runtime::load_tag(self.ctx, &self.builder, object);
        self.builder.build_switch(tag, done, &cases).expect("switching on a tag");

        self.builder.position_at_end(done);
        self.builder.build_return(None).expect("returning from drop_fields");
    }

    // -----------------------------------------------------------------------
    // Entry point
    // -----------------------------------------------------------------------
}
