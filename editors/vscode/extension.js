// Starts `sedge lsp` for .sg files.
const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

function activate(context) {
  const command = vscode.workspace.getConfiguration("sedge").get("path") || "sedge";
  client = new LanguageClient(
    "sedge",
    "Sedge",
    { command, args: ["lsp"] },
    { documentSelector: [{ scheme: "file", language: "sedge" }] }
  );
  context.subscriptions.push(client);
  client.start();
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
