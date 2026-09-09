# An agent built an HTTP job service from the public docs alone

Rules: `website/content/docs/**` and `README.md` only. No compiler source, no
`std` source. It finished: `GET /health`, `POST /jobs` with `derive(Decode)`
validation, four worker fibers off a `Channel<Job>`, `GET /stats`,
`POST /shutdown`, and accounting that reconciled in every run --
`accepted == completed + failed + in_flight` -- including under load during
shutdown.

**One compile error in about 160 lines**, written from the docs alone. That is
the headline and it should be said first. What follows is what it cost to get
the program to *stop*.

## Cancelling a fiber inside `Router::listen` aborts the process

The only shape the documentation leaves for shutting a service down is: run
`listen` on a fiber and cancel or detach it when a `/shutdown` route sets a
flag. There is no `std::process::exit` and no signal API, and
`deployment/linux.md` says so.

That shape aborts. Exit 134, on stderr:

    khora runtime: a cancellation reached a fiber's root, which cannot absorb
    one yet; see docs/design/fibers.md

Measured: `Fiber::detach(server)` with connections in flight -- 200 jobs at
32-way concurrency, `/shutdown` at 400 ms, one process and a fresh port per
round -- **10 aborts in 11 rounds**. Idle, with nothing being served, it is
clean. `Fiber::cancel` then `Fiber::wait` is worse: three rounds gave one
abort, one bind failure, and one hang that never returned and was killed at
15 s.

**The trigger is narrow.** Moving the detach to *after* the drain -- close the
channel, wait the four workers, print the reconciliation, then detach -- is
6/6 clean at 200 jobs and 4/4 at 800. So it is specifically cancelling the
fiber that is inside `Router::listen` while its accept-loop nursery still has
live connection children.

`reference/concurrency.md` on `detach` says the opposite of what happens --
"signal, and go... a failure it reports afterwards is silent". It is not
silent, it is `abort()`. And `cookbook/http-service.md`, the page somebody
building a service actually reads, says nothing about how a server ends.

The message is also written for a compiler developer, names neither the fiber
nor what cancelled it, and points at `docs/design/fibers.md`, which is not on
the website -- so somebody who installed the toolchain cannot follow it.

## A fiber that fails is invisible, and the program hangs

After an abort left the port unusable, the next start produced **no output at
all** and hung for ever. `Router::listen` inside the spawned fiber raised
`HttpError::BindFailed`, nobody joins that handle, and main sat in its poll
loop. Five threads alive, no server, nothing said.

The same failure reaching `main` is reported well -- though it does not print
*which* `HttpError`, and `BindFailed(8091)` would have named the port that is
taken. `HttpError` derives `Show`.

`limitations/index.md` covers half of this ("`Fiber::wait` tells you nothing
about how the fiber ended"). What is nowhere is that a listener on a fiber is
the *normal* shape for a service, so every service written this way is silent
on bind failure by default.

Compounding it: the listener appears not to set `SO_REUSEADDR`, so an aborted
run makes its own port unusable for a while, and that failure is the silent
one.

## The one compile error, and why the caret was on the wrong line

    error: this argument: `Halt` is not accounted for here
       --> ./src/main.kh:127:47
        |
    127 | |> Router::post("/jobs", SharedFn::of(fn r => enqueue(r, jobs, counters)!))

A `Router<'er>` carries one failure row, and `Router::get("/health", ..)` three
lines above -- a handler that cannot fail and so has no `raises` -- had already
pinned it to empty. The caret is on the innocent party, and "not accounted for
here" reads as "you forgot a `catch`". What it should say is that the row was
fixed by the handler mounted at line 124.

The fix is to give every *named* handler the same `raises` row including ones
that cannot fail, which reads as nonsense until you know why. Lambdas infer it;
named functions do not. No page says this.

## Documentation

- **Nothing shows a handler touching shared state.** Both HTTP cookbook pages
  use stateless handlers. That a `SharedFn::of` closure may capture a
  `Shared<Int>` and a `Channel<Job>`, and that a record of `Shared` fields is
  itself `Share`, are both true, both worked first try, and both were guesses.
  This is what a real service does.
- **No page composes the three pieces.** `http-service.md` says to bound the
  constrained resource; `taking-work-off-a-queue.md` has the pool without HTTP;
  `bounded-concurrency.md` has the nursery without the queue. The
  handlers-feed-a-worker-pool shape is left to the reader.
- **`Channel::send` returning `false` after `close`** is in `sharing.md`; its
  service consequence -- requests arriving during drain must be counted or the
  accounting silently loses them -- is in no cookbook page. In one measured run
  27 of 200 requests took that path and became 27 correct 503s.
- The region-and-flag pattern from `taking-work-off-a-queue.md` cost nothing
  and did nothing here: closing the channel is a cleaner way to stop workers
  than cancelling them, and closing is not a cancellation. The cookbook
  presents that pattern as *the* way a pool reconciles, and for a
  channel-closed shutdown it is not the one doing the work.
- The debug binary is 32 MB and the README quotes 3.6 MB for the release
  benchmark server. A newcomer's first `ls -la build/` sees the 32.

## What it praised

`limitations/index.md`, called the best page on the site and the best of its
kind the agent had read -- measured, numbered, correcting its own earlier
claims in place. The HTTP layer doing exactly what it documents: `HEAD` with a
`Content-Length` and no body, `OPTIONS` with `Allow`, 405 on a wrong method,
404, and a 422 carrying `[{"message":"name should be text, and is 7","path":"name"}]`
out of `derive(Decode)` and a nine-line handler. `khora new` to a running
binary with no configuration and no linker hunting. And `Shared` and `Channel`
as the right two primitives, split better than most languages' equivalent page.
