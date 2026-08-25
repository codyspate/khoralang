/* setjmp/longjmp, which Rust has no portable way to spell.
 *
 * `docs/design/c-export.md` §8. Containing a trap at an export boundary needs
 * a non-local exit back to the boundary, and the two candidates were this and
 * unwinding. Unwinding is out: the Khora frames in between are LLVM-generated
 * with no personality routine, and unwinding through them is undefined.
 *
 * `setjmp` cannot live in a helper that returns, because the buffer belongs to
 * that helper's frame and jumping into a dead frame is undefined. So the frame
 * that owns the buffer has to be the one that calls the body — which is what
 * this file is, and the whole reason it exists rather than being three lines
 * in `contain.rs`.
 *
 * The `libc` crate does not expose `setjmp` portably: it is a macro on most
 * platforms and an intrinsic on MSVC. A twelve-line C file compiled by `cc` is
 * the answer every language in this position reaches, and the toolchain
 * already requires clang.
 */

#include <setjmp.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

/* Set while a guarded call is on the stack, and cleared by hand on the way out
 * so that a trap outside one cannot find a dead buffer.
 *
 * Thread-local because the buffer belongs to a frame, and a frame belongs to a
 * thread. A guarded call never runs on a fiber stack — `contain.rs` refuses to
 * arm containment once anything has spawned — so this cannot be read from a
 * stack other than the one that wrote it. */
#if defined(_MSC_VER)
#define KHORA_THREAD __declspec(thread)
#else
#define KHORA_THREAD __thread
#endif

static KHORA_THREAD jmp_buf khora_landing;
static KHORA_THREAD int khora_armed = 0;

/* Runs `body(ctx)` with a landing point set, and returns what it returned.
 *
 * `*trapped` is 0 if the body ran to completion and 1 if it jumped. The result
 * is meaningless in the second case, and the caller is expected to say so
 * rather than hand it on. */
uint64_t khora_guarded_call(uint64_t (*body)(void *), void *ctx, int *trapped) {
    int previous = khora_armed;
    jmp_buf saved;
    /* Nested guarded calls are not expected — an export cannot call another
     * export through C without leaving Khora — but a host that does it anyway
     * should get the inner landing point back, not a corrupted outer one. */
    if (previous) {
        memcpy(&saved, &khora_landing, sizeof(jmp_buf));
    }

    if (setjmp(khora_landing) == 0) {
        khora_armed = 1;
        uint64_t result = body(ctx);
        *trapped = 0;
        khora_armed = previous;
        if (previous) {
            memcpy(&khora_landing, &saved, sizeof(jmp_buf));
        }
        return result;
    }

    *trapped = 1;
    khora_armed = previous;
    if (previous) {
        memcpy(&khora_landing, &saved, sizeof(jmp_buf));
    }
    return 0;
}

/* Whether a landing point is set on this thread. */
int khora_guard_armed(void) { return khora_armed; }

/* Jumps to the landing point. Undefined unless `khora_guard_armed`. */
void khora_guard_jump(void) {
    khora_armed = 0;
    longjmp(khora_landing, 1);
}
