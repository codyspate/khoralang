// The client half: start `khora lsp` and let VS Code talk to it.
//
// Everything this extension shows -- errors as you type, hover types,
// formatting -- is answered by `khora-lsp`, which is a subcommand of the
// compiler rather than a second program to install. So there is nothing here
// but "find the binary, start it, and say something useful when it is not
// there".
//
// **Plain JavaScript on purpose.** TypeScript would mean a build step, a
// `tsconfig`, and a compiled output tree in a repository whose gate is Rust and
// `sh` -- for a file this size, all of that is cost with no reader on the other
// end of it. The one thing TypeScript would have caught is a typo in an API
// name, and the extension development host reports those on the first F5.

const { workspace, window, commands, StatusBarAlignment, ThemeColor } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");
const { execFile } = require("child_process");

/** The running client, so the restart command has something to stop. */
let client;

/** Where the server's own complaints go, and the trace when it is on. */
let output;

/** Which toolchain answered, in the corner of the window. */
let status;

async function activate(context) {
  output = window.createOutputChannel("Khora");
  context.subscriptions.push(output);

  // **Which compiler is answering, where it can be read without asking for
  // it.** The pin already worked -- `khora lsp` hands over to the version a
  // project names -- and nothing said so, so "is the editor using my build or
  // an installed toolchain?" could only be settled by driving the server by
  // hand. Two versions on one machine is the ordinary case for anybody working
  // on the compiler, and the ordinary case was the unanswerable one.
  status = window.createStatusBarItem(StatusBarAlignment.Right, 100);
  status.command = "khora.showOutput";
  context.subscriptions.push(status);

  context.subscriptions.push(
    commands.registerCommand("khora.restartServer", () => restart(context)),
    commands.registerCommand("khora.showOutput", () => output.show(true)),
    commands.registerCommand("khora.runTest", runTest),
  );

  await start(context);
}

async function start(context) {
  const command = serverCommand();

  // `khora lsp` rather than a `khora-lsp` binary: the toolchain is one
  // executable, which is what 13.25 decided, and an extension that needed a
  // second download would undo it.
  const server = {
    command,
    args: ["lsp"],
    transport: TransportKind.stdio,
  };

  const client_options = {
    documentSelector: [{ scheme: "file", language: "khora" }],
    outputChannel: output,
    // The server reads the whole workspace at `initialize` -- cross-file
    // resolution needs one source root -- so it wants to know when a `.kh`
    // file appears or disappears outside the editor.
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.kh"),
    },
  };

  client = new LanguageClient("khora", "Khora Language Server", { run: server, debug: server }, client_options);

  try {
    await client.start();
    context.subscriptions.push(client);
    await reportToolchain(command);
  } catch (why) {
    showStatus("$(error) Khora: no server", "The language server did not start. Click for why.", true);
    // **The likeliest failure by far is that Khora is not installed**, and the
    // stack trace `vscode-languageclient` would otherwise surface says nothing
    // about that. Naming the setting and the install page is the whole content
    // of a useful message here.
    client = undefined;
    output.appendLine(`could not start \`${command} lsp\`: ${why}`);
    const install = "Install Khora";
    const settings = "Set the path";
    const chosen = await window.showErrorMessage(
      `Khora: could not start the language server by running \`${command} lsp\`. ` +
        `Install Khora, or point \`khora.server.path\` at the executable.`,
      install,
      settings,
    );
    if (chosen === install) {
      commands.executeCommand("vscode.open", "https://khora-lang.org/docs/getting-started/installation/");
    } else if (chosen === settings) {
      commands.executeCommand("workbench.action.openSettings", "khora.server.path");
    }
  }
}

/**
 * Asks the toolchain which toolchain this is, and shows the answer.
 *
 * **`khora toolchain which` rather than a rule reimplemented here.** It already
 * decides between a `[toolchain]` pin, the machine default and whatever is on
 * `PATH`, and it answers in a sentence that gives the reason -- *"no pin here
 * and no default, so whatever is on the path runs. This is Khora 0.1.0."* A
 * second copy of that decision in JavaScript would be a second answer to
 * disagree with the first, which is the failure this extension avoids
 * everywhere else by making the compiler the only source.
 *
 * It is also the one subcommand that never hands over to a pin, deliberately,
 * so it reports on the machine rather than on whichever build answered.
 */
async function reportToolchain(command) {
  const folder = workspace.workspaceFolders?.[0]?.uri?.fsPath;
  const said = await run(command, ["toolchain", "which"], folder);
  if (said === undefined) {
    // The server started, so the binary is there; only the question failed.
    // Saying nothing beats inventing a version.
    showStatus("$(tools) Khora", `Started \`${command} lsp\`.`, false);
    return;
  }
  output.appendLine(`${command} toolchain which: ${said}`);

  // The version out of the sentence, for the corner; the sentence itself for
  // the tooltip, because the *reason* is the half that settles an argument.
  //
  // **Not anchored to the end**, because the sentence has more than one shape.
  // `no pin here and no default, so whatever is on the path runs. This is
  // Khora 0.1.0.` finishes with it; `this project pins Khora 0.2.0, which is
  // not installed.` does not, and that one is the case worth getting right --
  // a pin nothing can satisfy is a project that will not build, and the corner
  // should say so rather than sit there looking content.
  const version = said.match(/Khora (\d[^\s,;.]*(?:\.[^\s,;.]+)*)/)?.[1];
  const wrong = /not installed|cannot|could not|no such/i.test(said);
  const label = wrong ? `$(warning) Khora ${version ?? "?"}` : `$(tools) Khora ${version ?? ""}`;
  showStatus(label.trim(), said, wrong);
}

/** Runs a command and returns its output, or `undefined` if it would not run. */
function run(command, args, cwd) {
  return new Promise((resolve) => {
    execFile(command, args, { cwd }, (error, stdout, stderr) => {
      if (error) {
        resolve(undefined);
        return;
      }
      resolve(`${stdout}${stderr}`.trim());
    });
  });
}

/** Puts `text` in the corner, with `detail` on hover. */
function showStatus(text, detail, bad) {
  if (!status) {
    return;
  }
  status.text = text;
  status.tooltip = `${detail}\n\nClick to show the language server output.`;
  status.backgroundColor = bad ? new ThemeColor("statusBarItem.errorBackground") : undefined;
  status.show();
}

/**
 * Runs one test, from the lens above it.
 *
 * **A terminal rather than a task or a captured process.** The output of a
 * test run is the thing somebody is asking for, and a terminal shows it as it
 * arrives, keeps it after the run, and can be scrolled and re-run. Capturing it
 * into a channel would be more code for a worse version of what the terminal
 * already is.
 *
 * The command is the same one a person would type, which is the point: nothing
 * here is a private path into the compiler.
 */
function runTest(name, directory) {
  const command = serverCommand();
  const terminal = window.createTerminal({ name: `khora test`, cwd: directory });
  terminal.show(true);
  terminal.sendText(`${quote(command)} test ${quote(directory ?? ".")} --filter ${quote(name)}`);
}

/** Quotes an argument for whichever shell the terminal is running. */
function quote(value) {
  const text = String(value ?? "");
  // Only when it needs it: quoting everything makes the command line in the
  // terminal harder to read and to copy, and reading it is half of why the
  // terminal was chosen.
  return /[\s"']/.test(text) ? JSON.stringify(text) : text;
}

/**
 * The executable to run.
 *
 * The setting first, for somebody testing a build of their own, then `khora` on
 * `PATH`, which is where both installers put it. Nothing hunts through
 * `~/.khora` directly: the bootstrap there is the thing that gets put on PATH,
 * and looking for it twice is how the two answers come to disagree.
 */
function serverCommand() {
  const configured = workspace.getConfiguration("khora").get("server.path");
  return configured && configured.trim() !== "" ? configured.trim() : "khora";
}

async function restart(context) {
  if (client) {
    await client.stop();
    client = undefined;
  }
  await start(context);
  output.appendLine("restarted");
}

async function deactivate() {
  if (client) {
    await client.stop();
  }
}

module.exports = { activate, deactivate };
