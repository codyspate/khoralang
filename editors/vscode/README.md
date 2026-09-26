# Khora for Visual Studio Code

Official language support for [Khora](https://khoralang.com): errors as you
type, types on hover, completion, navigation, refactorings, and formatting
for `.kh` files.

Every feature except syntax colouring comes from the Khora compiler itself,
through its built-in language server. What the editor shows is exactly what
`khora check` reports, so the two can never disagree.

## Getting started

1. **Install Khora.** Follow the instructions at
   [khoralang.com](https://khoralang.com/docs/getting-started/installation/). The installer
   puts `khora` on your `PATH`.
2. **Install this extension** from the Extensions view: search for *Khora*.
3. **Open a folder with `.kh` files.** The language server starts on its own.
   The status bar shows which Khora version is answering.

No other setup is needed.

## Features

### Errors and warnings as you type

Type errors, missing capabilities, unhandled errors and lint findings appear
while you edit, with the same messages the command line gives. Lint levels
from your project's `khora.toml` (including lint groups) are applied.

### Hover

Hover over any name to see its type, and its documentation if it has any.

### Inlay hints: what a call needs and what it can raise

Khora infers which capabilities a call uses and which errors it can raise.
The extension shows both, in line, at each call where either one applies:

```khora
let answer = charge(account, amount);   // with { db: Db, clock: Clock } raises DbError
```

Calls that need nothing and can't fail get no hint, so the marked lines are
the ones that cross a boundary. Inferred types are shown where the source
doesn't already say them.

### Completion and signature help

Completion for locals, fields, methods, modules and imports, and parameter
hints as you type a call.

### Navigation

- **Go to Definition**, **Go to Type Definition** and **Go to Implementation**
- **Find All References**, and highlighting of every use of the name under the
  cursor
- **Rename** across files
- **Outline** and breadcrumbs for the current file, and **Go to Symbol in
  Workspace** (`Ctrl+T` / `Cmd+T`)

### Quick fixes and refactorings

Open the lightbulb (`Ctrl+.` / `Cmd+.`) for fixes from the diagnostic under
the cursor, and for refactorings. A selection:

- add a missing import;
- add `!` to a call that can raise, or add the error to the function's
  `raises` clause;
- handle a failure with `catch`, or turn it into a `Result` with `attempt`;
- add a `raises` or `with` clause, or an arm to a `catch`;
- write a call as a pipeline (`|>`), or a pipeline as a call;
- extract an expression into a `let` or a function, inline a binding, or
  lift a lambda into a function;
- invert an `if`, write an `if` as a `match`, or add an `else` branch;
- generate a test or a benchmark for a function;
- export a declaration with `pub`, or stop exporting it;
- start a documentation comment or an example block.

### Run tests from the editor

Each `test` in a file gets a **▶ Run** link above it, which runs that test
with `khora test`.

### Formatting

Format Document, and format on save for Khora files, using the same formatter
as `khora fmt`. Formatting options come from your project's `khora.toml`.

### Semantic highlighting

On top of the usual syntax colouring, the compiler tells the editor what each
name is: a local, a parameter, an imported function, a type, a capability.
So colour follows meaning, not just spelling.

### Also

Code folding, smart selection expansion (`Shift+Alt+→` / `Ctrl+Shift+Cmd+→`),
bracket matching, comment toggling, and auto-closing pairs.

## Toolchain versions

If a project pins a Khora version in `khora.toml` (`[toolchain] version`), the
language server switches to that version automatically, just as the `khora`
command does. The status bar shows the version in use; hover over it to see
why that version was chosen.

The status bar item turns yellow when the project pins a version that isn't
installed. Install it with `khora toolchain install <version>`.

## Settings

| Setting | Default | What it does |
| --- | --- | --- |
| `khora.server.path` | *(empty)* | Path to the `khora` executable. Empty means use the one on `PATH`. Set it to use a different installation, such as a compiler you built yourself. |
| `khora.trace.server` | `off` | Record the messages between the editor and the language server in the **Khora** output panel: `off`, `messages` or `verbose`. Useful when reporting a bug. |

Format on save is turned on for Khora files only. To turn it off:

```json
"[khora]": {
  "editor.formatOnSave": false
}
```

## Commands

Open the Command Palette (`Ctrl+Shift+P` / `Cmd+Shift+P`) and type *Khora*:

| Command | What it does |
| --- | --- |
| **Khora: Restart Language Server** | Restart the server, for example after installing a new Khora version or changing `khora.server.path`. |
| **Khora: Show Language Server Output** | Open the **Khora** output panel, where the server's own messages go. Clicking the status bar item does the same. |

## Troubleshooting

**"could not start `khora lsp`"**: the extension couldn't find or run
`khora`. Check that `khora --version` works in a terminal. If VS Code was
open while you installed Khora, restart VS Code so it picks up the new
`PATH`, or set `khora.server.path` to the full path of the executable.

**No errors or hovers, only colours**: the language server isn't running.
Run **Khora: Show Language Server Output** to see why.

**Settings in `khora.toml` don't take effect**: a `khora.toml` that fails to
load is shown as an error on the line to fix. Until it's fixed, the server
uses default settings.

**Upgrading from an extension older than 0.3.1**: earlier versions were
published as `khora-lang.khora`. Uninstall that one, or both will run.

## Other editors

The language server works with any editor that speaks the Language Server
Protocol: run `khora lsp` over standard input and output. See
[Editor setup](https://khoralang.com/docs/getting-started/editor/).

## Feedback

Report bugs and ask questions on
[GitHub](https://github.com/codyspate/khoralang/issues). Khora is released
under the MIT or Apache-2.0 licence, at your option.
