# General-purpose positioning

Khora is a general-purpose language whose design is particularly well suited
to financial reconciliation and audit-heavy systems. It is not a
finance-specific language.

The distinction matters. Financial institutions hire engineers from every
part of the industry, and adopting a new language already carries an onboarding
cost. Requiring those engineers to learn a domain-specific programming model at
the same time would make that cost difficult to justify. An engineer who knows
Rust, Go or TypeScript should be able to learn Khora as an ordinary application
language and remain useful outside finance.

## The intended position

Khora should be a serious candidate wherever a team is considering Go,
backend TypeScript or application-level Rust: services, workers, event
consumers, data pipelines, command-line tools, infrastructure software,
workflow engines and other concurrent applications.

**Effect (TypeScript) belongs in that list even though it is a library rather
than a language**, because it is half of the thesis in `vision.md`: Rust has the
better developer experience, Effect has the better functional model, and nothing
has both. A team already using Effect has decided that typed effects,
capability-based injection and structured concurrency are what application code
wants — they are the audience least in need of persuading and most able to
judge the result. Khora's claim against them is direct style rather than
`Effect.gen`, one compilation rather than a runtime, and a native binary at the
end. Dropping Effect from the comparison because it is unfamiliar to the Go
audience would be dropping the half of the thesis that is harder to meet.

Its intended advantage is not that it knows financial terminology. It is that
it makes properties important to reliable software visible without requiring
Rust's ownership complexity:

- external authority is represented by capabilities;
- expected failures are part of a function's type;
- concurrent work has an owner and a defined lifetime;
- shared state crosses a fiber only through an explicit safe boundary;
- clocks, randomness, files and external services can be replaced in tests;
- programs compile to native executables without a tracing garbage collector.

Two of those are true today with a qualification worth stating wherever the
list is quoted. **A fiber is an OS thread**, so "concurrent work has an owner
and a defined lifetime" is about structure rather than cost; stackful
coroutines are intended and not written. And **the manifest is not a sandbox**
— the compile-time gate over Khora code is total, `extern fn` goes around it,
and closing that needs package identity. `docs/design/permissions.md`.

These properties are valuable in finance, but none belongs only to finance.
They apply equally to healthcare, logistics, security, infrastructure,
developer tooling and ordinary business services.

The concise product statement is:

> Khora is a general-purpose native language for reliable concurrent
> applications. It makes failures, external authority, resource lifetimes and
> shared state explicit without imposing Rust's ownership complexity.

## Finance is the proving ground

**This section is intent, not status.** There is no reconciliation reference
application in the repository today, and several things it names — decimal
correctness, durable decisions, replay, database access — do not exist. What
follows is the argument for building it, and it should not be read as a
description of what has been built.

Financial reconciliation is a demanding validation workload for the language,
not its public boundary. A reconciliation system has to combine large data
sets, external providers, decimal correctness, concurrency, expected business
failures, retries, durable decisions, replay and operational investigation. It
repeatedly has to answer:

- What data was consulted?
- Which rule produced this decision?
- Why did two sources disagree?
- What failed, retried or was overridden?
- Can the result be reproduced later?
- Could this code access or change anything outside its mandate?

A language that answers those questions cleanly is likely to be good at many
other serious applications. Reconciliation should therefore be used as an
adversarial reference application: it should expose weaknesses in numeric
types, effects, concurrency, transactions, serialization, database access and
operational tooling that a simpler demonstration would hide.

Khora should also retain non-financial reference applications. Services, CLIs,
job runners and infrastructure tools keep the language honest about being
general purpose and make it approachable to engineers with no finance
background.

A reference application is **evidence that the pieces compose**, and nothing
more. The three in the repository show capabilities, handlers, typed failure,
shared state, JSON and HTTP working together; none of them is a claim that the
language is production-complete, and the README's list of what is missing
applies to all of them.

## What belongs in libraries

Financial concepts should normally be packages rather than language syntax or
built-in types. Examples include `Money<USD>`, fixed-scale decimal policies,
reconciliation results, settlement calendars, ledgers and audit event schemas.

The core language should provide the general mechanisms those packages need:
precise numeric foundations, algebraic data types, generics, capabilities,
typed failures, structured concurrency, safe state transitions, stable
serialization and strong foreign-system interoperability.

Audit recording should likewise be an application capability rather than an
implicit language side effect. Logging is diagnostic and may be sampled;
durable audit recording is part of a business transaction. Khora should make
that distinction easy to express without forcing it on programs that do not
need it.

## The competitive scope

Khora does not need to replace every use of its comparison languages to be
general purpose.

- Rust will remain the stronger fit for kernels, drivers, constrained embedded
  systems, custom allocation and the most latency-sensitive low-level code.
- TypeScript will remain the natural fit for browser applications and projects
  whose primary advantage is the npm ecosystem.
- Go remains the operational benchmark for simple, highly concurrent network
  services, but Khora's networking and tooling should strive to be equally dependable.

The target is the large and useful area where those languages overlap:
reliable native application software. In that area Khora should combine more
static architectural information than Go or TypeScript with less application
complexity than Rust.

## Adoption is more than language design

A financial institution will not adopt Khora because its effect system is
elegant. Adoption requires an engineer to become productive quickly and an
operations team to trust what that engineer deploys.

The list below is ordered by **what it would cost to be wrong about**, not by
effort. The first group are claims the project already makes; a gap there is a
credibility problem rather than a missing feature, and costs more than
anything below it.

**Claims that must be true before they are repeated**

- The manifest is not a sandbox until package identity closes the `extern`
  hole. Designed, partly enforced, not a guarantee. `docs/design/permissions.md`.
- A type must know which declaration it came from. Two modules that each
  declare a `Point` are currently one type. Errata 46.
- Numbers must name their workload and machine. `bench/README.md` does; anyone
  quoting them must too.

**Load-bearing for a first real user**

- Reproducible package management, and the compatibility policy it enforces.
  The policy is decided — `docs/design/compatibility.md` — and nothing applies
  it yet.
- Dependable HTTP, TLS, database, messaging and serialization libraries.
  HTTP, JSON and TLS exist. **A database driver does not**, and that is now the
  gap of this kind that matters most — almost every service is one. Decided:
  SQLite in `std`, because it is embedded and a program that cannot persist
  anything is a demo; Postgres as a package, because pooling, authentication
  and version compatibility are things an application should pin.
  `docs/design/ecosystem.md`.
- Straightforward interoperability with existing Rust and C libraries.
- Load, soak, cancellation, recovery and malformed-input testing.

**Everything else, which is ordinary work**

- Familiar surface syntax and documentation that does not require type theory.
- Editor support, debugging, profiling. Diagnostics and formatting exist and
  are held to a standard from phase 2 by decision A7.
- Logging, metrics and distributed tracing.
- Conventional container and cloud deployment.
- Editions, when something first needs one.

An important onboarding test is whether a competent Go, Rust or TypeScript
engineer can build, test and deploy a small Khora service after one day with
the documentation. If that is difficult, the language or its tooling still has
work to do. Engineers should not need to understand row unification, effect
evidence or Perceus before writing useful programs. **That test cannot be run
yet** — there is no package manager and no installable toolchain — and until it
can, every claim about adoption on this page is a prediction.

## The goal

Khora should not be described as a financial language that happens to support
general programming. It should be a top-tier general-purpose language whose
strengths are demonstrated by financial systems because those systems are
unusually good at exposing unreliable design.

Success means both claims hold at once:

1. Khora is an excellent language for financial reconciliation and auditable
   systems.
2. An engineer with no financial background can reasonably choose Khora for an
   ordinary service, worker, CLI or platform component.

Finance provides the pressure. General-purpose engineering remains the product.
