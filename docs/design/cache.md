# The build cache

`khora build` over inputs it has already built returns the artifact it produced
last time. Roadmap 14.17.

```
$ khora build examples/core_demo
built examples/core_demo/src/main.exe from 16 module(s) [debug]      12.1s

$ khora build examples/core_demo
reused examples/core_demo/src/main.exe from the cache [c8560dc61c21, debug]   0.4s
```

## Why this one is a proof and most are a bet

Every build cache is a bet that the key captures everything that could change
the output. Turborepo hashes inputs and hopes; the usual way it loses is a
toolchain difference nobody hashed, and the symptom is an artifact that is
subtly not the one your source describes.

Khora can settle the bet instead of taking it, because 13.10 made
`KHORA_PROFILE=release` **bit-for-bit reproducible** — measured, not assumed.
When the same inputs provably produce the same bytes, a hit is not "an artifact
built from these inputs". It is *the* artifact, and the difference is testable:

```rust
assert_eq!(bytes(&reused), bytes(&fresh), "a release hit must be the same bytes");
```

That test builds twice for real, once from the cache and once with
`--no-cache`, and compares. It is the claim, executed.

## What is in the key

The source, and the toolchain that turns source into bytes:

| | why |
| --- | --- |
| every source file's contents | the obvious half |
| **the compiler binary, hashed** | not `khora --version`: a version string is constant across every dev build out of `target/debug`, and a cache that served the compiler you had ten minutes ago would be worst in the repository that has to trust it most |
| **the linker binary** | Khora emits an object and a C driver links it, so the driver's bytes are in the output's — this is the difference other caches lose to |
| **the runtime archive** | every executable links `khora-rt` statically |
| the target triple | |
| the profile | |
| **whether debug information is on** | `KHORA_DEBUG` overrides the profile in *both* directions, so the profile's name does not determine it |
| executable or library | |
| the source **paths**, when debug information is on | see below |

Fields are length-prefixed before hashing, so two adjacent ones cannot be slid
into each other — the same reason `khora_pkg::hash` does it.

Sources are sorted by content rather than by path, so the order does not depend
on where the checkout is.

## Debug information puts paths in the key

A debug build embeds each source file's absolute path in DWARF or a PDB, so two
checkouts of identical content do not produce identical artifacts. When debug
information is on the paths go into the key; when it is off they do not, and
two checkouts share an entry.

That is not a special case bolted on. It is the same rule as everything else
here: **the key holds exactly what the output depends on.**

The honest limit that follows: `debug` is not bit-for-bit reproducible on
Windows — 12.9 measured it, and what varies is inside lld-link's PDB emission.
A debug hit returns an artifact built from these inputs by this toolchain,
which is what a fresh build would also have given you, but not necessarily the
same bytes as one run right now. **The release claim is the strong one**, and
it is the one under test.

## A stat is not a content hash

Hashing two large binaries on every build would cost more than the cache saves,
so a file's digest is memoised against its size and modification time.

The obvious version of that is wrong, and the unit test that says so was
written to check the claim and failed on the first run. Two writes inside one
filesystem timestamp tick are indistinguishable, so a memo trusted on size and
mtime alone can describe contents that are no longer there. Git has exactly
this problem and calls such entries *racily clean*.

The rule here is the same shape: **a memo is believed only when it was recorded
strictly after the file it describes was last written.** The memo file's own
mtime is the record of when the hash was taken; a subject that is not older
than its record may have changed since, and is read again.

In the steady state the compiler was built minutes ago and every build pays for
a `stat`. Right after a rebuild — exactly when it matters — the memo is
distrusted and the file is hashed.

## It can explain itself

```
$ KHORA_CACHE_EXPLAIN=1 khora build .
khora: key from compiler af126c475886 linker 8bbe086dfb0f runtime 1ecc48dd160a
       target None profile debug debug true kind exe
khora:   source .../src/main.kh d0acfb0cb2e2...
khora: cache key a12bb5ede947...
khora: cache miss, nothing has been built with this key
```

**A cache that cannot say why it missed is a cache nobody can maintain.** This
is shipped rather than scaffolding, and errata 51 is why: three plausible
diagnoses of an intermittent miss were guesses, and what ended it was one line
saying the runtime archive digest had changed between two builds seconds apart.
The cache had been correct every time.

## A cache never fails a build

An unwritable directory, a corrupt entry, a linker that cannot be found: every
one of those is a miss and at most a warning. The moment a cache can break a
build is the moment people start passing `--no-cache` by reflex, and then it
may as well not exist.

A hit re-hashes the artifact and compares it against what the entry recorded
before handing it over. Entries are written under a temporary name and renamed,
so a half-written one should not exist — and "should not" is what a cache says
right before it hands somebody a truncated binary.

Two processes racing to store the same key both succeed: the loser's rename
fails and the answer it wanted is what the winner put there. The package store
does the same thing for the same reason.

## Managing it

```
$ khora cache
/home/you/.khora/cache
3 entr(y/ies), 14.7 MB

$ khora cache --clear
cleared 3 entr(y/ies), 14.7 MB back
```

**No eviction policy, deliberately.** A cache that decides for itself what to
throw away needs a rule — least recently used, a size budget — and a wrong rule
is a cache that evicts the entry somebody was about to hit. `--clear` is the
whole management story until somebody's disk says otherwise, and the numbers
above are how they will know.

## What this does not do yet

- **Only `khora build`.** `khora test` and `khora bench` compile too and do not
  consult it. The same key would work; what they produce and where they put it
  is different enough to want its own pass.
- **No remote cache.** The same key over a network is the shared-with-CI
  version, and it is worth building once somebody has a team. The key is
  already machine-independent for release builds, which is the hard half.
- **No partial reuse.** A one-character edit rebuilds everything, because
  compilation is whole-program. That is a real cost of the language's design
  and not something the cache can paper over; what makes it bearable is that
  the cache turns *unchanged* members in a monorepo free, which with 14.16's
  `--since` is most of a CI run.
