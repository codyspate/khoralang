// Drives the packaged extension's own `activate()` against a stubbed
// `vscode` module: the exact code path VS Code runs, with the exact argv
// `src/extension.js` builds for `vscode-languageclient`, and watches whether
// it answers `initialize`.
//
// **Why not just run `khora lsp <args>` by hand.** The failure this guards
// against was never a shell command typed correctly — 0.3.0 and 0.3.1
// shipped an extension whose argv was fine on its own and wrong as
// `vscode-languageclient` built it. Naming `TransportKind.stdio` makes the
// library append `--stdio`, which every toolchain before 0.4 refuses, and
// that decision is made inside the extension and the library, not by
// whoever runs this check. Only loading the packaged `extension.js` and
// calling `activate` proves which argv a user's editor will actually run.
//
// Usage: node transport-check.js <extracted-vsix-dir> <khora-executable>
//
// Exits 1 and prints why on any of: the extension failed to load, the
// extension itself reported a failed start, or it never spawned the
// configured executable. Exits 0 once `activate()` resolves having spawned
// `khora` with no reported failure -- which, inside `extension.js`, only
// happens after `client.start()` has gotten `initialize` answered.
"use strict";

const Module = require("module");
const path = require("path");
const cp = require("child_process");

const TIMEOUT_MS = 8000;

function fail(message) {
  console.error(`transport-check: ${message}`);
  process.exit(1);
}

const ext = process.argv[2];
const khora = process.argv[3];
if (!ext || !khora) {
  fail("usage: node transport-check.js <extracted-vsix-dir> <khora-executable>");
}

const entry = require(path.join(ext, "package.json")).main;
const entryPath = path.join(ext, entry);

// Record every spawn the library makes, so a wrong argv is visible even when
// the process then happens to start for some other reason.
const spawned = [];
const realSpawn = cp.spawn;
cp.spawn = function (cmd, args, opts) {
  spawned.push([cmd, ...args]);
  return realSpawn.call(this, cmd, args, opts);
};

// A generic stub for the `vscode` APIs this check never calls but the
// library's own code touches at load time -- `CompletionItem`,
// `CodeActionKind`, and anything a future `vscode-languageclient` release
// adds. It is constructible and callable, so `class X extends vscode.Y {}`
// and `new vscode.Z()` both work without this script knowing every name.
function genericStub() {
  const f = function () {
    return genericStub();
  };
  return new Proxy(f, {
    get: (_t, k) => (k === Symbol.toPrimitive ? () => "" : k === "then" ? undefined : genericStub()),
    construct: () => genericStub(),
    apply: () => genericStub(),
  });
}

function noopDisposable() {
  return { dispose() {} };
}

const outputChannel = {
  appendLine(line) {
    process.stderr.write(`[extension output] ${line}\n`);
  },
  append(text) {
    process.stderr.write(text);
  },
  show() {},
  dispose() {},
};

const statusBarItem = {
  show() {},
  dispose() {},
};

let errorShown = null;

// The specific APIs `activate` and `start` actually call. Wrapped in a proxy
// below so anything this script did not think to name falls through to
// `genericStub()` rather than `undefined`, which would fail at the point of
// use instead of at the point of whatever the real bug is.
const vscodeNamed = {
  version: "1.90.0",
  workspace: new Proxy(
    {
      workspaceFolders: undefined,
      textDocuments: [],
      onDidOpenTextDocument: () => noopDisposable(),
      onDidCloseTextDocument: () => noopDisposable(),
      onDidChangeTextDocument: () => noopDisposable(),
      onWillSaveTextDocument: () => noopDisposable(),
      onDidSaveTextDocument: () => noopDisposable(),
      onDidChangeWorkspaceFolders: () => noopDisposable(),
      onDidChangeConfiguration: () => noopDisposable(),
      // The setting the extension reads for which executable to run: point
      // it at the `khora` under test, the same way a user's
      // `khora.server.path` would.
      getConfiguration(section) {
        if (section === "khora") {
          return { get: (key) => (key === "server.path" ? khora : undefined) };
        }
        return { get: () => undefined };
      },
      createFileSystemWatcher() {
        return {
          dispose() {},
          onDidCreate: () => noopDisposable(),
          onDidChange: () => noopDisposable(),
          onDidDelete: () => noopDisposable(),
        };
      },
    },
    {
      // `vscode-languageclient` registers a long list of workspace
      // listeners -- notebook sync, configuration change, and more that
      // vary by its own version -- and none of them affect whether
      // `initialize` gets answered. Each becomes a no-op subscription
      // rather than a name this script has to keep in step with the
      // library.
      get(target, key) {
        if (key in target) return target[key];
        if (key === Symbol.toPrimitive || key === "then") return undefined;
        return (..._args) => noopDisposable();
      },
    },
  ),
  window: {
    createOutputChannel: () => outputChannel,
    createStatusBarItem: () => statusBarItem,
    showErrorMessage: async (message) => {
      errorShown = message;
      return undefined;
    },
  },
  commands: {
    registerCommand: () => noopDisposable(),
    executeCommand: async () => undefined,
  },
  StatusBarAlignment: { Left: 1, Right: 2 },
  ThemeColor: function ThemeColor(id) {
    this.id = id;
  },
};

const vscodeStub = new Proxy(vscodeNamed, {
  get(target, key) {
    if (key in target) return target[key];
    if (key === Symbol.toPrimitive || key === "then") return undefined;
    return genericStub();
  },
});

const orig = Module._resolveFilename;
Module._resolveFilename = function (req, ...rest) {
  if (req === "vscode") return "vscode";
  return orig.call(this, req, ...rest);
};
require.cache["vscode"] = { id: "vscode", filename: "vscode", loaded: true, exports: vscodeStub };

let extension;
try {
  extension = require(entryPath);
} catch (e) {
  fail(`could not load ${entryPath}: ${e.stack}`);
}

const context = { subscriptions: [] };

let settled = false;
const timer = setTimeout(() => {
  if (settled) return;
  fail(`no reply to initialize within ${TIMEOUT_MS}ms\nspawned: ${JSON.stringify(spawned)}`);
}, TIMEOUT_MS);

// A server that starts and then exits immediately -- the shape of "refused
// an argument before answering" -- can throw asynchronously from inside the
// library's own retry and close handling, after `activate()` has already
// resolved or rejected. Node's default for an unhandled rejection is to dump
// a trace and exit 1, which already fails this check; this turns it into
// the same message every other failure gives.
process.on("unhandledRejection", (reason) => {
  if (settled) return;
  settled = true;
  clearTimeout(timer);
  fail(`unhandled rejection while starting the server: ${reason}\nspawned: ${JSON.stringify(spawned)}`);
});

extension
  .activate(context)
  .then(() => {
    // `start` inside `extension.js` never rethrows: it catches a failed
    // `client.start()` itself and shows an error message instead, so
    // `activate()` resolving on its own proves nothing. Whether
    // `showErrorMessage` fired is the signal that distinguishes "the server
    // answered `initialize`" from "it didn't, and the extension said so in
    // the only place a user would see it."
    clearTimeout(timer);
    settled = true;
    if (errorShown) {
      fail(`the extension reported a failed start: ${errorShown}\nspawned: ${JSON.stringify(spawned)}`);
    }
    if (spawned.length === 0) {
      fail("activate() returned without the extension spawning a server at all");
    }
    const [cmd] = spawned[0];
    if (cmd !== khora) {
      fail(`spawned ${JSON.stringify(spawned[0])}, not the configured executable ${khora}`);
    }
    console.log(`transport-check: initialize answered, spawned ${JSON.stringify(spawned[0])}`);
    process.exit(0);
  })
  .catch((e) => {
    fail(`activate() threw: ${e.stack}\nspawned: ${JSON.stringify(spawned)}`);
  });
