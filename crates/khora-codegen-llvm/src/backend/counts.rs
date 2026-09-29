//! The local count: a spawning program's reference counts on objects no
//! other fiber can reach.
//!
//! **What this buys, and what it rests on.** A locked read-modify-write
//! costs about 3 ns on x86, and a server's handler performs thousands per
//! request on objects only its own fiber ever sees. An object whose count
//! word has neither the shared bit (63) nor the immortal bit (62) set was
//! made on this fiber and never published, so no other thread can be
//! counting it, and a relaxed load, an add and a relaxed store are enough.
//! That is true only while every runtime entry that hands a value to another
//! fiber marks it first (`khora_rt::khora_share`, and `scripts/check-share.sh`
//! holding each entry to it). An entry that forgets makes two threads count
//! one object with plain arithmetic: a lost update, then a use-after-free or
//! a leak far from its cause. A debug build's owner check traps on the first
//! such count, naming both fibers.
//!
//! **Relaxed atomics, not plain loads and stores.** They compile to the same
//! `mov` on x86 and `ldr`/`str` on AArch64. The difference is what LLVM may
//! assume: a plain access raced by another thread is undefined behavior the
//! optimizer can exploit, while a relaxed one raced by a missed crossing is
//! at worst a lost update, which the owner check can then report.
//!
//! **Migration is not a crossing.** A fiber moves between workers only
//! through the scheduler's locked queues, so every count it wrote on one
//! worker happens before anything it does on the next (`khora_rt::share`'s
//! module doc has the argument).

use super::*;
use inkwell::values::IntValue;
use inkwell::{AtomicOrdering, AtomicRMWBinOp, IntPredicate};

impl<'ctx> Backend<'ctx> {
    /// Emits one count of `object` by `by` (+1 or -1) at the builder's
    /// position in `function`, and answers the count word as it was before.
    ///
    /// Three paths off one relaxed load and one unsigned compare against bit
    /// 62, which is set in the word of anything that must not take the plain
    /// path:
    /// - **local** (neither flag): an add or subtract, stored relaxed. In a
    ///   debug build the owner check is called first with the word it found.
    /// - **immortal**: nothing is written. Answers 2, the smallest previous
    ///   count `Lower::drop` reads as "survives", so a static never reaches
    ///   `khora_drop_last`.
    /// - **shared**: the locked add (relaxed) or subtract (release) every
    ///   count used before this path existed. `khora_drop_last`'s acquire
    ///   fence pairs with the subtract.
    ///
    /// The answer carries the flags and a debug build's owner on every path.
    /// `Lower::drop` truncates it to the low 32 bits before its
    /// last-reference test, which drops bits 32..63 whatever they hold, and
    /// `khora_drop_last` masks to the count; so neither reads a flag as part
    /// of a count.
    ///
    /// The builder is left at the end of the join block.
    pub(crate) fn emit_local_count(
        &mut self,
        function: FunctionValue<'ctx>,
        object: PointerValue<'ctx>,
        by: i64,
        check_owners: bool,
    ) -> IntValue<'ctx> {
        let i64t = self.ctx.i64_type();
        let one = i64t.const_int(1, false);
        let word = self
            .builder
            .build_load(i64t, object, "rc")
            .expect("loading a refcount")
            .into_int_value();
        let load = word.as_instruction().expect("a load is an instruction");
        load.set_alignment(8).expect("a count word is 8-aligned");
        load.set_atomic_ordering(AtomicOrdering::Monotonic).expect("a relaxed load");
        // Both flags are above every count and every owner, so one unsigned
        // compare finds either. The slow path then tells them apart.
        let flagged = self
            .builder
            .build_int_compare(
                IntPredicate::UGE,
                word,
                i64t.const_int(khora_rt::KHORA_IMMORTAL, false),
                "rc.flagged",
            )
            .expect("testing the flag bits");
        let local = self.ctx.append_basic_block(function, "rc.local");
        let slow = self.ctx.append_basic_block(function, "rc.flagged");
        let shared = self.ctx.append_basic_block(function, "rc.shared");
        let joined = self.ctx.append_basic_block(function, "rc.joined");
        self.builder
            .build_conditional_branch(flagged, slow, local)
            .expect("choosing a count's path");

        self.builder.position_at_end(local);
        if check_owners {
            self.builder
                .build_call(self.rt.rc_check, &[word.into()], "")
                .expect("checking who is counting");
        }
        let next = if by > 0 {
            self.builder.build_int_add(word, one, "rc.up")
        } else {
            self.builder.build_int_sub(word, one, "rc.down")
        }
        .expect("adjusting a refcount");
        let store = self.builder.build_store(object, next).expect("storing a refcount");
        store.set_alignment(8).expect("a count word is 8-aligned");
        store.set_atomic_ordering(AtomicOrdering::Monotonic).expect("a relaxed store");
        self.builder.build_unconditional_branch(joined).expect("leaving the local path");

        self.builder.position_at_end(slow);
        let immortal_bit = self
            .builder
            .build_and(word, i64t.const_int(khora_rt::KHORA_IMMORTAL, false), "rc.immortal")
            .expect("reading the immortal bit");
        let immortal = self
            .builder
            .build_int_compare(IntPredicate::NE, immortal_bit, i64t.const_zero(), "rc.static")
            .expect("testing for a static");
        self.builder
            .build_conditional_branch(immortal, joined, shared)
            .expect("skipping a static's count");

        self.builder.position_at_end(shared);
        let (op, ordering) = if by > 0 {
            (AtomicRMWBinOp::Add, AtomicOrdering::Monotonic)
        } else {
            (AtomicRMWBinOp::Sub, AtomicOrdering::Release)
        };
        let counted = self
            .builder
            .build_atomicrmw(op, object, one, ordering)
            .expect("adjusting a shared refcount");
        self.builder.build_unconditional_branch(joined).expect("leaving the shared path");

        self.builder.position_at_end(joined);
        let phi = self.builder.build_phi(i64t, "rc.previous").expect("joining a count");
        let static_count = i64t.const_int(2, false);
        phi.add_incoming(&[(&word, local), (&static_count, slow), (&counted, shared)]);
        phi.as_basic_value().into_int_value()
    }
}
