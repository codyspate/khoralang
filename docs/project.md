# Technical Specification & Implementation Guide: The Khora Language (`.kh`)

**Target Paradigm:** Native, Statically-Typed, Pure-Functional Systems Language  
**Compilation Target:** Bare-metal native static executables via LLVM / Cranelift (Zero VM, Zero Tracing GC)  
**Memory Model:** Perceus Reference Counting with Static In-Place Mutation (Functional But In-Place / FBIP)  
**Type System:** Sound Hindley-Milner Core, Higher-Kinded Types ($* \to *$), Scoped Row Polymorphism, Const Generics  

---

## 1. Syntax, Grammar & Lexical Standards

### 1.1 Lexical Rules

* **Explicit Semicolons (`;`):** All statements, module declarations, type definitions, and multi-line pipe expressions must terminate with a semicolon.
* **Universal Dot Notation (`.`):** Consistent `.` symbol for namespaces, static enum constructors, record field access, and method invocations. No `::` or `->` symbol clutter.
* **Prefix Imports:** `import path.to.module.{SymbolA, SymbolB};`
* **Native Pipe (`|>`):** First-class binary operator for dataflow with `_` placeholder support for non-first positional arguments.
* **Pattern Matching:** Native, compiler-verified exhaustive `match` expressions with algebraic data type destructuring and match guards (`if`).
* **Null Safety:** No ambient `null` or `undefined`. Absence is strictly typed via `Option<T> = | Some(value: T) | None`.

### 1.2 Formal EBNF Grammar

> **This is the original sketch, not the implemented grammar.** It is kept as
> written because it is the document the project started from and other
> sections of it are still cited — §6.1's reproducible builds, among others.
> The syntax below has been overtaken in at least three ways: paths are `::`
> rather than `.`, a function body is a block rather than `= expr;`, and
> visibility is `pub` rather than `export`. `docs/grammar.ebnf` is what the
> parser implements, and `docs/design/keywords.md` records why each of those
> changed.

```ebnf
Program        ::= ModuleDecl ( ImportDecl )* ( TopLevelDecl )* ;
ModuleDecl     ::= "module" IdentPath ";" ;
ImportDecl     ::= "import" IdentPath "." ( "{" ImportList "}" | "*" ) ";" ;
ImportList     ::= ImportItem ( "," ImportItem )* ;
ImportItem     ::= Ident ( "as" Ident )? ;

TopLevelDecl   ::= TypeDecl | FunctionDecl | LayerDecl | LetDecl ;

TypeDecl       ::= ( "export" )? "type" Ident ( TypeParams )? "=" TypeDef ";" ;
TypeDef        ::= VariantType | RecordType | FunctionType | AliasType ;

VariantType    ::= ( "|" Ident ( "(" RecordFields | TupleFields ")" )? )+ ;
RecordType     ::= "{" ( RecordField ( "," RecordField )* ( "," )? )? ( "|" Ident )? "}" ;
FunctionType   ::= ( Type | "(" TypeList ")" ) "->" Type ;

FunctionDecl   ::= ( "export" )? "fn" Ident ( TypeParams )? "(" ParamList ")" ( "->" Type )? "=" BlockExpr ";" ;
BlockExpr      ::= "{" ( Statement )* ( Expr )? "}" ;

Statement      ::= LetDecl | ExprStmt ;
LetDecl        ::= "let" ( "mut" )? Pattern ( ":" Type )? "=" Expr ";" ;
ExprStmt       ::= Expr ";" ;

Expr           ::= PipeExpr ;
PipeExpr       ::= MatchExpr ( "|>" ( MatchExpr | PlaceholderExpr ) )* ;
MatchExpr      ::= "match" Expr "{" ( MatchArm )+ "}" | PrimaryExpr ;
MatchArm       ::= Pattern ( "if" Expr )? "=>" ( BlockExpr | Expr ) ( "," )? ;

PrimaryExpr    ::= Literal | IdentPath | RecordInit | TupleInit | BlockExpr | LambdaExpr ;
LambdaExpr     ::= "fn" ( Ident | "(" ParamList ")" ) "=>" ( BlockExpr | Expr ) ;

```

---

## 2. Type System & Dependency Architecture

### 2.1 The `Effect` Monad

The core computational primitive in the compiler is represented as:

$$\text{Effect}\langle +A, -R, +E \rangle$$

* **$A$ (Covariant):** Successful evaluation value.
* **$R$ (Contravariant):** Open row of capability requirements (`{ db: Database, ai: LLMService | 'r }`).
* **$E$ (Covariant):** Open row/union of typed failure channels (`DbError | ModelError`).

### 2.2 Row Unification & Resolution Rules

When two operations $f: \text{Effect}\langle A_1, R_1, E_1\rangle$ and $g: A_1 \to \text{Effect}\langle A_2, R_2, E_2\rangle$ compose:

$$R_{\text{combined}} = R_1 \cup R_2 \quad \text{and} \quad E_{\text{combined}} = E_1 \cup E_2$$

Applying `Effect.provide(label, service)` executes row subtraction:

$$R_{\text{out}} = R_{\text{in}} \setminus \{ \text{label}: T \}$$

The entrypoint `Effect.run_native()` is legal to call only when $R = \emptyset$ (`{}`).

---

## 3. Standard Library Specification

### 3.1 `std.effect` (Runtime & Dependency Injection)

```typescript
module std.effect;

pub type Option<+A> =
  | Some(value: A)
  | None;

pub type Result<+A, +E> =
  | Ok(value: A)
  | Err(error: E);

pub type Effect<+A, -R, +E>;
pub type Layer<+OutR, -InR, +E>;
pub type Scope;

pub fn succeed<A>(value: A) -> Effect<A, Never {},>;
pub fn fail<E>(error: E) -> Effect<Never, E {},>;
pub fn sync<A>(thunk: () -> A) -> Effect<A, Never {},>;
pub fn try_catch<A, E>(thunk: () -> A, on_error: Error -> E) -> Effect<A, E {},>;

pub fn ask<T>(label: Label) -> Effect<T, 'r Never T label: { | },>;

pub fn map<A, B, E R,>(effect: Effect<A, E R,>, f: A -> B) -> Effect<B, E R,>;
pub fn flat_map<A, B, E1, E2 R1, R2,>(
  effect: Effect<A, E1 R1,>, 
  f: A -> Effect<B, E2 R2,>
) -> Effect<B, + E1 E2 R1 R2 { | } },>;

pub fn tap<A, E R,>(effect: Effect<A, E R,>, f: A -> Effect<(), R, E>) -> Effect<A, E R,>;
pub fn catch<A, E1, E2 R,>(effect: Effect<A, E1 R,>, handler: E1 -> Effect<A, E2 R,>) -> Effect<A, E2 R,>;

pub fn acquire_release<A, E1 R1, R2,>(
  acquire: Effect<A, E1 R1,>, 
  release: A -> Effect<(), R2, Never>
) -> Effect<A, + E1 R1 R2 Scope scope: { | },>;

pub fn scoped<A, E R,>(effect: Effect<A, E R Scope scope: { | },>) -> Effect<A, E R,>;

pub fn provide_layer<A, E1, E2 R1, R2,>(
  effect: Effect<A, E1 R1,>, 
  layer: Layer<R1, E2 R2,>
) -> Effect<A, E1 E2 R2, { | }>;

pub fn run_native<A, E>(effect: Effect<A, E {},>) -> Result<A, E>;

```

### 3.2 `std.ai` (Native Shape-Safe Tensors & Typed LLMs)

```typescript
module std.ai;

import std.effect.{Effect};

pub type Device = | Cpu | Cuda(device_id: Int) | Metal | Npu;

// Shape-safe tensor parameterized by Device, Shape Tuple, and Element Type
pub type Tensor<D: Device, Scalar Shape: Tuple, Type:>;
pub type Embedding<const Dim: Int, Type: Scalar> = Tensor<Device.Cpu, (Dim), Type>;

// Compile-time shape-verified matrix multiplication
pub fn matmul<D: Device, Int, K: M: N: Scalar T: const>(
  a: Tensor<D, (M, K), T>,
  b: Tensor<D, (K, N), T>
) -> Tensor<D, (M, N), T>;

pub type Message = {
  role: String,
  content: String,
};

pub type Prompt = {
  system_instructions: Option<String>,
  messages: List<Message>,
  temperature: Float,
};

pub type ModelError =
  | ContextLengthExceeded(max_tokens: Int)
  | RateLimited(retry_after_ms: Int)
  | InferenceEngineFailure(msg: String)
  | SchemaExtractionError(details: String);

pub type LLMService = {
  complete: Prompt -> Effect<String, ModelError {},>,
  extract: forall <Schema> . (Prompt, Schema.Spec) -> Effect<Schema, ModelError {},>,
  embed: forall <const Dim: Int> . String -> Effect<Embedding<Dim, F32>, {}, ModelError>,
};

pub fn cosine_similarity<const Dim: Int>(
  a: Embedding<Dim, F32>, 
  b: Embedding<Dim, F32>
) -> Float;

```

---

## 4. Reference Implementation

### 4.1 Project Manifest (`khora.toml`)

The manifest features unified toolchain configurations, Deno-style OS capability limitations, and sandboxed WebAssembly build plugins to prevent supply-chain attacks.

```toml
[package]
name = "risk_analyzer"
version = "0.1.0"
authors = ["Engineering Team <dev@khora.internal>"]
edition = "2026"

# Which capabilities this program may hold — checked when it is compiled.
# Which host or path each may be used with — checked where the access happens.
# See docs/design/permissions.md, which decides D4 and narrows the claim this
# section used to make.
[permissions]
network = ["0.0.0.0:8080", "*.internal:5432"]
env = ["DB_*"]

[permissions.fs]
read = ["/etc/config", "./data/**"]
write = ["./tmp/**"]

# Unified Toolchain Config
#
# The whole table. Two knobs, because everything else the formatter does --
# where lines break, how imports sort, what spacing punctuation gets -- is not
# a setting: a canonical formatter with knobs is two formatters. `indent-width`
# is ignored with tabs, since the width of a tab is the reader's setting.
[fmt]
indent-style = "space"
indent-width = 2

[lints]
unused-capabilities = "deny"
cyclomatic-complexity = { level = "warn", max = 15 }

[dependencies]
"std.effect" = { version = "1.0.0" }
"std.net.http" = { version = "1.0.0" }
"std.ai" = { version = "1.0.0" }

# Sandboxed Build Plugins (WASM) replace arbitrary build.rs execution
[build]
target = "x86_64-unknown-linux-musl"
plugin = "protobuf-compiler@2.1" 

# Native DAG Task Runner
[tasks.ci]
description = "Run the full CI pipeline"
depends_on = ["lint", "test", "build"]

```

#### The workspace, and fields a member inherits

A repository holding several packages puts a `[workspace]` table in a
`khora.toml` at its root. That manifest has no `[package]`: the root is not one,
and a root forced to declare a package would be inventing a name for a thing
that does not exist.

```toml
[workspace]
members = ["examples/*", "packages/*", "bench/*"]
exclude = ["examples/scratch"]

# What a member takes with `field.workspace = true`.
[workspace.package]
version = "0.1.0"
edition = "2026"

[workspace.fmt]
indent-style = "space"
indent-width = 2

[workspace.lints]
unused-capabilities = "deny"
```

A pattern is a directory, optionally with a trailing `*` matching one level
down. Not a glob language: `**`, character classes and brace expansion are a
syntax to document and a set of edge cases to get subtly wrong, and a member
that does not fit a pattern is listed by name. A directory with no `khora.toml`
is quietly not a member; `exclude` is for one that has a manifest and should
still be left out.

`khora check` and `khora fmt` at a root run over every member, each as its own
package with its own dependencies, and every member runs even after one fails.

`--since <rev>` narrows either command to the members a diff can reach:

```
$ khora check . --since main
1 of 8 member(s) affected since main; skipping examples/core_demo, ...
```

Exact rather than inferred. The resolver already knows which directories each
member compiles, so a change inside a *dependency* selects the members that
reach it and not the ones that do not. A changed file inside no member and
inside nothing a member depends on — the compiler, the root manifest, a script
— selects **every** member and says which file did it: a tool that answers
"nothing was affected" about a file it did not recognise is worse than no tool.
Untracked files count.

There is one `khora.lock`, at the root, and every member seeds the resolution
that writes it. So two members cannot quietly hold two revisions of a shared
dependency: the second one to ask is refused, naming both. The cost is that
resolving any member resolves all of them. What comes back to a build is still
only what that member reaches -- the lock covers the workspace, the compilation
does not.

A member takes a shared value by asking for it:

```toml
[package]
name = "postgres"
version.workspace = true
edition.workspace = true
publish = true          # not inherited: exactly one thing here is a library

[fmt]
workspace = true

[lints]
workspace = true
```

**Nothing is inherited implicitly.** A root value that applied unless overridden
would mean reading a member's manifest tells you what it *adds*, and the only
way to know what it *is* would be to read two files and remember which wins.

`name` is not inheritable — it is the one thing that makes a member a distinct
package. `[fmt]`, `[lints]` and `[permissions]` are taken whole rather than
field by field: a half-inherited permission set is one nobody can read off
either file, and "what may this package do" wants one answer in one place.
Writing a grant beside `workspace = true` is an error rather than a silent loss.

Three ways to get this wrong, each with its own message: asking a root for a
field it does not set, asking with no workspace root above you at all, and
asking from a directory the root does not list as a member.


### 4.2 Application Code (`src/main.kh`)

```typescript
module app.main;

import std.effect.{Effect, Layer, Scope, ask, Option};
import std.net.http.{Request, Response, Router};
import std.ai.{LLMService, Prompt, Embedding, ModelError, Tensor, cosine_similarity};

pub type RiskLevel =
  | Low
  | Moderate(reason: String)
  | Critical(action_required: String);

pub type AnalysisReport = {
  account_id: String,
  risk: RiskLevel,
  confidence: Float,
};

pub type Ledger = {
  get_history: String -> Effect<String, String {},>,
  flag_account: (String, RiskLevel) -> Effect<(), {}, String>,
};

pub fn analyze_transaction_risk(account_id: String) 
  -> Effect<AnalysisReport, LLMService Ledger, String ai: ledger: { },> = {
    
    account_id
    |> ask(:ledger.get_history)
    |> Effect.map_error(fn err => "Failed to fetch account history: " + err)
    |> Effect.flat_map(fn history =>
        Prompt.new()
        |> Prompt.system("You are a financial fraud and risk engine. Output structured assessments.")
        |> Prompt.user("Analyze history: " + history)
        |> ask(:ai.extract(_, AnalysisReport.spec))
        |> Effect.map_error(fn err =>
            match err {
              ModelError.ContextLengthExceeded(_) => "Prompt context limit exceeded",
              ModelError.RateLimited(ms) => "Upstream model rate limited",
              ModelError.InferenceEngineFailure(msg) => "Inference failure: " + msg,
              ModelError.SchemaExtractionError(d) => "Invalid extraction schema: " + d,
            }
        )
    )
    |> Effect.flat_map(fn report =>
        match report.risk {
          RiskLevel.Low =>
            Effect.succeed(report),

          RiskLevel.Moderate(reason) =>
            account_id
            |> ask(:ledger.flag_account(_, report.risk))
            |> Effect.map(fn _ => report),

          RiskLevel.Critical(action) =>
            account_id
            |> ask(:ledger.flag_account(_, report.risk))
            |> Effect.tap(fn _ => Effect.sync(fn _ => print("ALERT: Critical threat detected on " + account_id)))
            |> Effect.map(fn _ => report),
        }
    );
};

let mock_ledger_layer: Layer<{ ledger: Ledger }, {}, Never> =
  Layer.succeed({
    get_history: fn id => Effect.succeed("Transaction: $50000 to offshore entity"),
    flag_account: fn (id, risk) => Effect.succeed(()),
  });

let mock_ai_layer: Layer<{ ai: LLMService }, {}, Never> =
  Layer.succeed({
    complete: fn _ => Effect.succeed(""),
    embed: fn _ => Effect.succeed(Tensor.zeros((1536))),
    extract: fn (_, _) => Effect.succeed({
      account_id: "acc_9921",
      risk: RiskLevel.Critical("Immediate fund freeze"),
      confidence: 0.98,
    }),
  });

pub fn main() = {
  let app_layer =
    mock_ledger_layer
    |> Layer.merge(mock_ai_layer);

  let router =
    Router.new()
    |> Router.post("/analyze/:account_id", fn req =>
        req.params.get("account_id")
        |> analyze_transaction_risk
        |> Effect.map(fn report => Response.json(200, report))
        |> Effect.catch(fn err => 
            Effect.succeed(Response.json(500, { error: err }))
        )
    );

  router
  |> Router.listen(8080)
  |> Effect.provide_layer(app_layer)
  |> Effect.scoped
  |> Effect.run_native();
};

```

---

## 5. Compiler Implementation Blueprint for AI Agents

Build the compiler in Rust as a Cargo workspace with strict crate boundaries:

```text
khora/
├── Cargo.toml
├── crates/
│   ├── khora-syntax/       # Logos lexer, Rowan lossless CST parser, AST definitions
│   ├── khora-hir/          # AST -> HIR lowering, desugaring `|>` and `_` placeholders
│   ├── khora-types/        # HM Inference, Row Polymorphism unification, HKT solver
│   ├── khora-perceus/      # Static reference counting pass & in-place reuse analysis
│   ├── khora-codegen-llvm/ # Inkwell/LLVM backend, native target emission, C-FFI linking
│   └── khora-cli/          # Unified `khora` CLI toolchain (build, check, test, fmt, lsp)

```

### 5.1 Agent Task Breakdown

* **Agent 1 (`khora-syntax`):**
* Implement tokenizer using `logos` containing tokens: `;`, `.`, `|>`, `_`, keywords (`module`, `import`, `type`, `fn`, `match`, `pub`, `let`, `mut`).
* Implement recursive descent / Pratt parser using `rowan` to support lossless Concrete Syntax Trees for LSP integration and error resilience.


* **Agent 2 (`khora-types`):**
* Implement Algorithm W extended with Leijen/Rémy-style Scoped Row Polymorphism.
* Represent types as `Type::Constructor(Name, Vec<Type>)`, `Type::Row(Fields, TailVar)`, `Type::Var(Index)`, and `Type::Const(Int)`.
* Enforce row subtraction rules for `Effect.provide` and exhaustiveness checking for `match` patterns.


* **Agent 3 (`khora-perceus`):**
* Implement Perceus reference counting: insert precise `dup` and `drop` instructions at lexical scope boundaries.
* Implement static reuse analysis: identify when an ADT variant or tensor is dropped and another allocated within the same branch, replacing `drop` + `alloc` with an in-place `reuse` instruction.


* **Agent 4 (`khora-codegen-llvm` & `std`):**
* Lower verified and optimized HIR to LLVM IR using `inkwell`.
* Expose native bindings in `std.ai` to C BLAS/LibTorch/GGML interfaces.
* Integrate `lld` to produce fully self-contained static executables (`x86_64-unknown-linux-musl`, `aarch64-apple-darwin`).



---

## 6. Unified Toolchain & Developer Platform (`khora` CLI)

The `khora` executable is a single static binary containing the compiler, package manager, formatter, linter, test runner, monorepo workspace orchestrator, and Language Server Protocol (LSP) daemon.

### 6.1 Package & Workspace Management (`khora-pkg`)

* **Deterministic Lockfile (`khora.lock`):** Generated automatically on `khora build` or `khora add <package>`. Contains cryptographic SHA-256 hashes of all resolved package ASTs and artifacts, guaranteeing bit-for-bit reproducible builds.
* **Monorepo Orchestration (`khora-workspaces`):** The workspace runner includes an integrated, content-addressed DAG (Directed Acyclic Graph) task runner.
* Zero Configuration Caching: Compiler caches intermediate HIR/LLVM artifacts in `~/.cache/khora/` keyed by source hash and compiler build triple.
* Hermetic Execution: Workspaces share a single root `khora.lock` to prevent diamond dependency conflicts.



### 6.2 Built-in Zero-Config Formatter (`khora fmt`)

* **Speed Target:** Sub-millisecond formatting powered by `rowan` lossless CST traversal (similar to Biome / `rustfmt`).
* **Canonical Rules:**
* 2-space indentation by default; `[fmt] indent-style` and `indent-width`
  change it, and nothing else in the formatter is configurable.
* Explicit semicolons, which are a rule of the grammar rather than a setting —
  see §14. `[fmt] explicit-semicolons` was briefly written here as though it
  were a choice; it was removed in 14.20b, and a manifest still carrying it
  gets a warning saying why rather than "unrecognized key".
* Multi-line pipeline (`|>`) indentation aligned to the source expression.
* Alphabetized, deduplicated prefix imports (`import a.b.{A, B, C};`).



### 6.3 Built-in Linter & Diagnostic Engine (`khora lint`)

The linter runs as a pass immediately following type inference, leveraging row-polymorphic tracking:

* **Unused Capability Warning:** Flags when a capability row `{ db: Database | 'r }` is requested via `ask()` but never invoked.
* **Dangling Pure Expressions:** Flags non-`Effect` expressions whose return values are discarded without being piped or bound.
* **Redundant Pattern Match Arms:** Identifies unreachable branches in `match` blocks.

### 6.4 Native Test Framework (`khora test`)

Unit and integration tests are first-class declarations in the language, written directly in source files or dedicated `tests/` directories.

* **In-Source Test Block Syntax:** Uses `test "name" = { ... }` blocks built into the syntax tree.
* **Parallel Execution:** Runs isolated test fibers across all CPU cores by default.
* **Snapshot Testing:** Native `assert_snapshot(value)` with interactive `khora test --update-snapshots` CLI.
* **Benchmark Engine:** Native `bench "name" = { ... }` with statistical throughput analysis ($P_{50}$, $P_{95}$, $P_{99}$).

### 6.5 Integrated Language Server Protocol (`khora lsp`)

Built into the same binary to ensure zero version drift between IDE features and compiler behavior:

* **Instant Diagnostics:** Incremental salsa-based query engine providing sub-15ms auto-complete and type hover info.
* **Capability Inlay Hints:** Displays inferred open rows (`{ db: Database | 'r }`) inline above function signatures.
* **Smart Refactoring:** Semantic symbol renaming across whole workspaces, auto-import resolution for prefix imports (`import a.b.{X}`), and match pattern stub generation.
