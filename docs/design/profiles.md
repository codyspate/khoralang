# Build profiles

**Status: built.** Roadmap 13.10.

Two profiles, `debug` and `release`. `khora build --release` selects the
second; `KHORA_PROFILE=release` says the same thing to every command, including
the ones with no flag of their own.

## What each one is

|                      | `debug`        | `release`      |
| -------------------- | -------------- | -------------- |
| LLVM pass pipeline   | none           | `default<O2>`  |
| instruction selection| `-O2`'s        | `-O3`'s        |
| debug information    | on             | off            |
| bit-for-bit reproducible | no         | **yes**        |

`debug` is exactly what every build did before this existed. That is
deliberate: it is the default, and every number, every soak, every timing in
this repository was taken against it. A profile that changed the default would
have invalidated all of them to no purpose.

## Why two

**A profile is a name for a set of answers**, and its whole value is that a
person can ask for the set without knowing what is in it. Three is where that
stops being true. The third profile is always "release, but with something" —
release with debug information, release with assertions, release with less
inlining — and each of those is better asked for directly than smuggled into a
name that then has to be explained.

So the knobs stay separable where somebody genuinely needs them apart:
`KHORA_DEBUG` overrides the profile's debug-information decision **in both
directions**. `KHORA_DEBUG=1 khora build --release` is a profiling build, and
`KHORA_DEBUG=0 khora build` is how 12.9 measured reproducibility before there
was a profile to ask for. Neither needs a third name.

## Why the profile owns debug information

12.4 left this open: debug information is on by default "because there is no
release mode to hang it off", and it "should become part of an optimization
level when there is one". This is that.

12.9 supplies the other half, and it is the constraint that decides the
question rather than a preference. **A build with debug information is not
reproducible on Windows** — measured, not assumed: relinking one unchanged
object twice gives identical bytes without `-g` and different bytes with it,
`/Brepro` or not, and what varies is inside lld-link's PDB emission.

So one of the two profiles can be bit-for-bit reproducible and it has to be the
one that ships. `release` turns debug information off, which is what makes
`tests/profiles.rs` able to assert that two release builds produce the same
executable — the same claim 12.9 could only make with an environment variable
set by hand.

The linker is told the profile rather than reading the variable, and that is
load-bearing. A build asked for with `--release` never touches `KHORA_DEBUG`;
if the link read the environment it would add `-g` to a module carrying no
debug metadata and cost exactly the reproducibility that not emitting it buys.

## Why `default<O2>` and not a list of passes

The pipeline is named, not assembled. LLVM's `default<O2>` is maintained by
people who measure it, changes with every release, and is what every other
front end runs. A hand-picked list is a promise to keep picking, and the first
regression from not picking again is silent.

`O2` rather than `O3`: the difference is mostly loop unrolling and
vectorization, which costs compile time and code size for programs that are not
numerical kernels, and there is no measurement here saying Khora's output is.
`O3` is a change to make with a benchmark in hand, and `bench/` is where the
benchmark would come from.

**Debug runs no pipeline at all**, which is what it has always done. A target
machine on its own does instruction selection, so the IR reaching it is the IR
that was written — which is why a debug build is readable, quite apart from the
line tables.

Instruction selection stays at `-O2`'s level in debug rather than dropping to
`None`. Dropping it would make the default build slower than the one every soak
and every timing here was calibrated against, in exchange for a readability the
unoptimized IR already provides.

## The module is verified twice in release

Once after lowering, as always, and again after the pipeline. A pass that
breaks the module is a compiler bug, and finding it here names it as one rather
than handing invalid IR to the assembler. It costs a walk of a module the build
has already spent much longer optimizing.

## What was checked

`tests/profiles.rs` pins the four claims above for one program. The broader
check is that the *whole* suite runs under the optimizer:

    KHORA_PROFILE=release cargo test --workspace --features llvm
    KHORA_PROFILE=release sh scripts/http_conformance.sh

Both are green, the first including the live PostgreSQL tests. That matters
more than any single assertion here, because an optimizer is what turns a
latent assumption in generated code into a wrong answer — an `inbounds` GEP
whose bounds check was not quite right, an aliasing claim nothing enforces —
and this is a few hundred generated programs, a real socket and a real
database, all of it compiled at `O2`.

It found one thing, and the thing was a test: `tests/debugging.rs` asserts on
line numbers in a backtrace, which a release build has none of. It now names
`Profile::Debug` explicitly, which is what it was always testing.

Neither command is in `scripts/baseline.sh`. The baseline is the gate for every
change and doubling its build time to re-check the same programs is a bad
trade; run these after a change to code generation.

## What this does not do

**No profile for a package.** `khora.toml` has no `[profile.release]` section,
and should not get one until there is a reason a *library* would want different
answers from the program linking it — which, in a whole-program compiler with
no separate compilation, there is not yet. `docs/design/distribution.md` and
D12 are where that would change.

**`khora test` and `khora bench` have no flag.** They read `KHORA_PROFILE` like
everything else. A benchmark almost certainly wants `release`, and the numbers
recorded in `bench/README.md` and in the roadmap were taken without it — so
whoever turns it on should re-measure rather than compare across the change.

**No `--opt-level`.** It is the third profile wearing a number.
