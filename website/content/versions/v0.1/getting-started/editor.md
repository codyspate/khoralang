---
title: Editor setup
sidebar:
  order: 3
---

Khora ships one language server as part of the compiler toolchain: `khora lsp`. It is backed by the same compiler queries used by `khora check`, so the diagnostics and type information you see in your editor come from the compiler itself.

Your editor should launch the server for `.kh` files; you normally do not need to run it in a terminal yourself.

## VS Code

The extension is not on the Marketplace yet. Install it from a release:

1. Download `khora-lang.khora.vsix` from the newest `vscode-v*` release on GitHub.
2. Run `code --install-extension khora-lang.khora.vsix`, or use **Extensions: Install from VSIX** in the command palette.

It needs `khora` on your `PATH`, which both installers arrange. If it cannot find one it says so and offers to open the `khora.server.path` setting rather than failing quietly. Format-on-save is turned on for `.kh` files only.

The status bar shows which toolchain answered, and turns yellow when a project pins a version that is not installed.

## Language server command

Configure an LSP client with:

```text
command: khora
args:    lsp
```

The server currently provides:

- compiler diagnostics and hover information;
- formatting;
- completion and signature help;
- go-to-definition and find-references;
- document and workspace symbols;
- semantic tokens;
- inlay hints;
- code actions and code lenses;
- highlighting every mention of the name under the cursor;
- go to the *type* of an expression, and to every `impl` of a type or trait;
- folding, and expand-selection through the tree;
- inlay hints: the capability and failure rows a call needs, and the inferred type of a binding that does not say its own;
- rename, across every file that names the thing.

Quick fixes are offered only where a diagnostic's own message names one edit and there is nothing to choose, because an action is applied by somebody who read four words of it. Seven qualify today: adding the `!` a call needs; writing out the trait members an impl has not, with their signatures copied from the trait and `Self` swapped for the type being implemented; writing every missing `match` arm at once, qualified the way the arms already there are, with `todo()` for a body; removing an unused import together with one separating comma; renaming an unused binding to `_name`; taking the spelling a "did you mean" suggests; and adding a record's missing field, again with `todo()`. The message that says a call needs a capability the function does not require offers the signature edit and nothing beside it, because propagating the requirement outwards is one of two answers and only that one is spelled out.

Inside a `with { .. }`, completion offers whole handlers: typing an effect's name inserts `clock: handler for Clock { now: fn () => todo() }`, with a closure of the right arity for every operation the effect declares. Where the code inside the block still needs a capability, the entry is labelled the way the requirement asked for it and sorted first. A signature's `with` clause is a different row and offers types instead.

Completion offers every public name in the workspace, not only what the file has imported, and accepting one that is not in scope writes the `import` with it: merged into an existing import of that module, or placed in sorted order among the others. Names from elsewhere sort below everything already in scope. The documentation for one is fetched when you highlight it rather than for the whole list, which is what keeps the list fast against a workspace the size of `std`.

Assists are the other half, and answer a different question: not what is wrong, but what you want done where the cursor is. A `let` with no annotation offers to write its inferred type down as text, which an inlay hint can only draw. A selected expression offers to become a `let` above the statement it was in. That one refuses where lifting it would cross something conditional, an `if` branch, a `match` arm, a lambda body, or the far side of `&&`, because running code the program said to skip is not a refactoring.

A code lens marks what a function absorbs rather than passes on: `installs { db } · catches DbError` above a function whose signature mentions neither. Rows are transitive, so a lens repeating a signature would be noise; a `with` block and a `catch` are the two places that stops being true, and they are what the type system deliberately hides.

Rename edits the declaration, every use, and the import that brings the name into each file. Where a file imports under an alias, the import's original name is renamed and the alias is left alone, because the alias is that file's own word for it. A trait member and a constructor are still refused, each with a sentence saying why: a trait member's name belongs to the trait and to every impl of it, and a constructor has no recorded range to edit.

The server asks for incremental synchronization, so an edit sends the edit rather than the file.

The installed toolchain is the only server installation you need. Updating or pinning Khora also selects the compiler behavior your editor sees for that project.

## Formatting

Use Khora's formatter rather than editor-specific formatting rules:

```bash
khora fmt .
```

For CI:

```bash
khora fmt . --check
```

Editors that support LSP formatting can delegate format-on-save to `khora lsp`.

## Editor clients

Any editor with a Language Server Protocol client can launch `khora lsp`. The repository ships working configuration for VS Code, Helix and Neovim. Emacs and Sublime Text have LSP snippets in `editors/README.md` that nobody has run; they are a starting point rather than a supported setup. Zed needs an extension compiled to WebAssembly and has none.

The important part is always the same: the project root contains `khora.toml`, `.kh` files are associated with Khora, and the editor starts `khora lsp`.

## AI coding tools

Khora also exposes a compiler-backed MCP server. A coding agent can use it to ask the real Khora compiler about source instead of guessing syntax or type behavior.

The same standard-library index is on the command line as `khora std search <query>`, so it is not an agent-only facility. Both read the compiler's own view of the `std` beside them rather than a checked-in list, which is why neither can describe a version you do not have.

It is optional. Nothing about writing, building or testing correct Khora requires it, and the compiler and language server behave identically whether or not an agent is connected.

A typical MCP client configuration is:

```json
{
  "mcpServers": {
    "khora": {
      "command": "khora",
      "args": ["mcp"]
    }
  }
}
```

As with the language server, the client starts the process for you. You normally do not run `khora mcp` interactively.

## Next

- [Your first Khora project](/docs/getting-started/first-project/) shows the command-line workflow the editor complements.
- The [Language Reference](/docs/reference/) covers the language features the server is checking.
- [Language Reference](/docs/reference/) is the lookup-oriented companion when you need exact syntax or semantics.
