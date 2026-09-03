//! The span a fiber is inside.
//!
//! **`std::trace` had no notion of a current span, and every trace was one
//! span deep.** `Tracer::start` builds a `Span` out of the name it is given and
//! nothing else, because an effect operation runs where the effect is
//! *performed* and has no access to whatever `around` happened to be on the
//! stack above it. So a nested `around` began a second trace rather than a
//! child span, `Span::parent` was written `0` at every call site in the
//! repository, and the OTLP exporter carried a comment admitting it started a
//! fresh trace per span. A collector showed a request as a dozen unrelated
//! one-span traces.
//!
//! This is the ambient half: one slot per fiber holding the context of the
//! innermost span. `std::trace::current` reads it, a handler's `start` takes
//! the trace id and parent from it, and `spanned` installs the new span for the
//! duration of the body and restores what was there on every path out.
//!
//! # Why it is per fiber, and inherited
//!
//! Per fiber because two fibers serving two requests are inside two different
//! spans at the same instant, and a slot on the thread answers about whichever
//! request the worker last touched. [`crate::current`] exists for exactly this
//! class of state, and says so.
//!
//! Inherited at spawn because that is the case the reference service is about:
//! a request handler spawns three fibers, and their spans belong to the
//! request's trace rather than to three traces of their own. `Fiber::spawned`
//! runs on the spawning side, before the child exists, so the copy it takes is
//! the parent's own current span with no synchronisation needed.
//!
//! A child *copies* rather than shares. A span opened inside a child is the
//! child's business, and pushing it into a slot the parent reads would make
//! two fibers fight over one word — and give the parent a parent it never
//! entered once the child returned.
//!
//! # Zero means none
//!
//! A span id of zero is already how `Span::parent` says "this is a root", so
//! the empty slot needs no separate flag: `khora_span_id` returning zero is
//! what `std::trace::current` reads as `Option::None`. Span ids come from
//! `Random::int`, so zero is not a value a real span takes; if one ever did it
//! would be reported as a root, which is the same thing that happens today.

use crate::current::{SpanContext, current};

/// The trace id's high half, or zero.
#[unsafe(no_mangle)]
pub extern "C" fn khora_span_trace_high() -> i64 {
    current(|fiber| fiber.span().trace_high)
}

/// The trace id's low half, or zero.
#[unsafe(no_mangle)]
pub extern "C" fn khora_span_trace_low() -> i64 {
    current(|fiber| fiber.span().trace_low)
}

/// The innermost span's id, or zero when this fiber is inside none.
#[unsafe(no_mangle)]
pub extern "C" fn khora_span_id() -> i64 {
    current(|fiber| fiber.span().span)
}

/// Whether the current trace is being recorded, as 0 or 1.
///
/// An `i64` rather than a `bool` because nothing in `std` returns a `Bool`
/// across the boundary, and the one-bit-in-a-register question is the sort
/// that is answered differently by two calling conventions.
#[unsafe(no_mangle)]
pub extern "C" fn khora_span_sampled() -> i64 {
    current(|fiber| i64::from(fiber.span().sampled))
}

/// Makes this the span the fiber is inside.
///
/// `span` of zero puts the fiber back outside any span, which is what
/// restoring at the outermost `around` does.
#[unsafe(no_mangle)]
pub extern "C" fn khora_span_set(trace_high: i64, trace_low: i64, span: i64, sampled: i64) {
    current(|fiber| {
        fiber.set_span(SpanContext { trace_high, trace_low, span, sampled: sampled != 0 });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slot starts empty, which is what makes the first `around` a root.
    #[test]
    fn a_fiber_starts_outside_every_span() {
        assert_eq!(khora_span_id(), 0);
    }

    #[test]
    fn what_was_set_is_what_is_read() {
        khora_span_set(7, 9, 11, 1);
        assert_eq!(khora_span_trace_high(), 7);
        assert_eq!(khora_span_trace_low(), 9);
        assert_eq!(khora_span_id(), 11);
        assert_eq!(khora_span_sampled(), 1);
        khora_span_set(0, 0, 0, 0);
        assert_eq!(khora_span_id(), 0, "zero puts the fiber back outside every span");
    }

    /// **The case the whole thing exists for.** A fiber spawned inside a span
    /// belongs to that span's trace; without this a request that fans out into
    /// three fibers is four unrelated traces.
    #[test]
    fn a_spawned_fiber_starts_inside_its_spawners_span() {
        khora_span_set(3, 4, 5, 1);

        let child = crate::current::Fiber::spawned();
        let inherited = child.span();

        khora_span_set(0, 0, 0, 0);
        assert_eq!(inherited.trace_high, 3);
        assert_eq!(inherited.trace_low, 4);
        assert_eq!(inherited.span, 5, "the spawner's span is the child's parent");
        assert!(inherited.sampled);
    }

    /// A copy, not a share: what a child opens is the child's business, and a
    /// parent that could see it would keep a parent it never entered.
    #[test]
    fn a_child_moving_on_does_not_move_its_spawner() {
        khora_span_set(3, 4, 5, 1);
        let child = crate::current::Fiber::spawned();
        child.set_span(SpanContext { trace_high: 3, trace_low: 4, span: 99, sampled: true });

        let mine = current(|fiber| fiber.span());
        khora_span_set(0, 0, 0, 0);
        assert_eq!(mine.span, 5, "the spawner is still in its own span");
    }
}
