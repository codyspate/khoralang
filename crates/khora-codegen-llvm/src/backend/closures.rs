//! Closure sites, and the shared routine that releases one.
//!
//! A lambda is lifted to a function and its captures go in a heap object under
//! the ordinary header. The tag a closure carries is its site's index, which is
//! how one `drop_fields` routine can release any closure in the program: it
//! switches on the tag to learn which captures this one holds.

use super::*;
use super::driver::CLOSURE_GLUE;

impl<'ctx> Backend<'ctx> {
    /// Records a lambda site and declares the function it lifts to.
    ///
    /// The lifted function takes the closure object as its first argument and
    /// the lambda's own parameters after it, which is what makes an indirect
    /// call possible without knowing anything about the captures at the call
    /// site.
    ///
    /// `shape` is the lambda's `Type::Fn` — all four of what it takes, gives
    /// back, can fail with and has to be handed, which travel together because
    /// they are one type.
    pub fn declare_closure(
        &mut self,
        owner: &str,
        expr: khora_hir::body::ExprId,
        shape: Type,
        captures: Vec<(khora_hir::body::LocalId, Type)>,
    ) -> Option<usize> {
        let Type::Fn { params, ret, raises, requires } = shape else { return None };
        let (params, ret, raises, requires) = (params, *ret, *raises, *requires);
        let index = self.closures.len();
        let symbol = format!("{owner}$$lambda{}", self.closures_by_owner.get(owner).map_or(0, Vec::len));

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let mut llvm_params: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        for param in &params {
            llvm_params.push(self.llvm_type(param)?.into());
        }
        // After the written parameters, in label order — the same convention a
        // named function follows, and the one `invoke_closure_at` already
        // builds its call around.
        let handed: Vec<(String, Type)> = match &requires {
            Type::Row { fields, .. } => fields.clone(),
            _ => Vec::new(),
        };
        for (_, ty) in &handed {
            llvm_params.push(self.llvm_type(ty)?.into());
        }
        let fallible =
            matches!(&raises, Type::Row { fields, tail } if !fields.is_empty() || tail.is_some());
        let fn_type = if fallible {
            self.tagged_type().fn_type(&llvm_params, false)
        } else {
            match &ret {
                Type::Unit => self.ctx.void_type().fn_type(&llvm_params, false),
                other => self.llvm_type(other)?.fn_type(&llvm_params, false),
            }
        };

        let f = self.module.add_function(&mangle(&symbol), fn_type, Some(Linkage::Internal));
        self.functions.insert(symbol.clone(), f);
        self.defined.insert(symbol.clone());
        self.instance_signatures.insert(
            symbol.clone(),
            Signature {
                is_extern: false,
                generics: Vec::new(),
                bounds: Vec::new(),
                requires: requires.clone(),
                raises: raises.clone(),
                params: params.clone(),
                ret: ret.clone(),
            },
        );

        self.closures.push(ClosureSite {
            owner: owner.to_string(),
            expr,
            symbol,
            ret,
            raises,
            requires,
            captures,
        });
        self.closures_by_owner.entry(owner.to_string()).or_default().push(index);
        Some(index)
    }

    /// The closure site for a lambda expression inside `owner`.
    pub fn closure_at(&self, owner: &str, expr: khora_hir::body::ExprId) -> Option<&ClosureSite> {
        self.closures_by_owner
            .get(owner)?
            .iter()
            .map(|i| &self.closures[*i])
            .find(|c| c.expr == expr)
    }

    /// The tag a closure site's objects carry.
    pub fn closure_tag(&self, owner: &str, expr: khora_hir::body::ExprId) -> Option<u32> {
        self.closures_by_owner
            .get(owner)?
            .iter()
            .find(|i| self.closures[**i].expr == expr)
            .map(|i| *i as u32)
    }

    pub fn closure_sites(&self) -> Vec<ClosureSite> {
        self.closures.clone()
    }

    /// The `drop_fields` routine shared by every closure.
    ///
    /// One routine switching on the tag, exactly as an ADT's does and for the
    /// same reason: a drop site knows only that it holds a value of *some*
    /// function type, and two lambdas with the same signature capture entirely
    /// different things. The tag is what distinguishes them.
    pub(super) fn closure_glue(&mut self) -> PointerValue<'ctx> {
        if !self.closures.iter().any(|c| c.captures.iter().any(|(_, t)| is_boxed(t))) {
            return self.null_pointer();
        }
        if let Some(Some(f)) = self.drop_glue.get(CLOSURE_GLUE) {
            return f.as_global_value().as_pointer_value();
        }

        let void = self.ctx.void_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let f = self.module.add_function(
            "kh$drop_fields$closure",
            void.fn_type(&[ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.drop_glue.insert(CLOSURE_GLUE.to_string(), Some(f));
        // The closure routine is shared, so it is queued under a name no
        // Khora type can have rather than under an instantiation.
        self.pending_glue.push(Type::adt(CLOSURE_GLUE.to_string()));
        f.as_global_value().as_pointer_value()
    }

    /// Emits the shared closure `drop_fields`.
    pub(super) fn emit_closure_glue(&mut self) {
        let f = match self.drop_glue.get(CLOSURE_GLUE) {
            Some(Some(f)) => *f,
            _ => return,
        };
        let object = f.get_nth_param(0).expect("drop_fields takes an object").into_pointer_value();

        let entry = self.ctx.append_basic_block(f, "entry");
        let done = self.ctx.append_basic_block(f, "done");

        let mut cases = Vec::new();
        for (tag, site) in self.closure_sites().into_iter().enumerate() {
            let owned: Vec<(usize, Type)> = site
                .captures
                .iter()
                .enumerate()
                .filter(|(_, (_, ty))| is_boxed(ty))
                // Field 0 holds the function pointer, so capture `i` is field
                // `i + 1`.
                .map(|(i, (_, ty))| (i + CLOSURE_CAPTURE_BASE, ty.clone()))
                .collect();
            if owned.is_empty() {
                continue;
            }

            let block = self.ctx.append_basic_block(f, &format!("drop.{}", site.symbol));
            cases.push((self.ctx.i32_type().const_int(tag as u64, false), block));
            self.builder.position_at_end(block);

            for (index, field_ty) in owned {
                let slot = runtime::field_pointer(self.ctx, &self.builder, object, index as u64);
                let value = self
                    .builder
                    .build_load(self.ctx.ptr_type(AddressSpace::default()), slot, "captured")
                    .expect("loading a captured field");
                let glue = self.drop_glue(&field_ty);
                self.builder
                    .build_call(self.rt.drop, &[value.into(), glue.into()], "")
                    .expect("dropping a capture");
            }
            self.builder.build_unconditional_branch(done).expect("branch to the return");
        }

        self.builder.position_at_end(entry);
        let tag = runtime::load_tag(self.ctx, &self.builder, object);
        self.builder.build_switch(tag, done, &cases).expect("switching on a closure tag");

        self.builder.position_at_end(done);
        self.builder.build_return(None).expect("returning from drop_fields");
    }
}
