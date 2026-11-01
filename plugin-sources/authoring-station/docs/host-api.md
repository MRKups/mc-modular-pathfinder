# Host API

A plugin asks the app for data with a JSON-RPC request carrying an `id`:

```json
{"jsonrpc":"2.0","id":7,"method":"workspace.info","params":{}}
```

The host answers with a normal result or a readable error:

```json
{"jsonrpc":"2.0","id":7,"result":{"root":"/work/project","name":"project"}}
```

Methods are additive. Unknown methods return an error. There is no separate
API version; protocol v1 is the wire contract.

## workspace.info

Returns the active project root and directory name.

```json
{"root":"/work/project","name":"project"}
```

The root is refreshed when a surface is opened, so switching projects and
reopening a plugin gives the new root.

## storage.get / storage.set

Plugin-scoped JSON storage. Keys are 1–256 bytes. The whole store is capped at
1 MiB and lives at `<app-data-dir>/plugin-data/<plugin-id>/storage.json`.

```json
{"jsonrpc":"2.0","id":8,"method":"storage.get","params":{"key":"last-run"}}
```

```json
{"jsonrpc":"2.0","id":9,"method":"storage.set","params":{
  "key":"last-run",
  "value":{"status":"passed","at":"2026-10-31T15:00:00Z"}
}}
```

`storage.get` returns the stored JSON value or `null`. `storage.set` returns
`null`. With no app data directory, both return an error.

## host.describe

Returns the authoritative API surface, protocol, limits, and plugin directory.

```json
{
  "protocol": 1,
  "appVersion": "0.16.0",
  "methods": ["workspace.info", "storage.get", "storage.set", "host.describe", "llm.providers", "llm.complete", "session.start"],
  "features": {"codeEditor": true, "llm": true, "sessions": true},
  "limits": {"uiNodes": 512, "uiTreeBytes": 1048576, "lineBytes": 2097152, "sessionTitleBytes": 200}
}
```

Use this instead of hard-coding a copied list of methods.

## llm.providers

Lists installed providers that can run one-shot text requests, with their
cached models and selected account. This does not start a provider process.

```json
{
  "providers": [
    {"id": "claude", "label": "Claude Code", "account": "default",
     "models": [{"id": "claude-sonnet-4-5", "name": "Sonnet 4.5"}],
     "background": true}
  ],
  "background": {"provider": "claude", "model": "claude-haiku-4-5"}
}
```

An empty `models` list means the provider's default model.

## llm.complete

Runs one bounded, text-only, read-only completion. The app calls the provider;
credentials never leave the app. The call is asynchronous and may take a while,
so the plugin stays responsive while it waits.

Request:

```json
{"jsonrpc":"2.0","id":10,"method":"llm.complete","params":{
  "prompt": "Explain this plugin manifest",
  "system": "You are a concise Monocode plugin expert.",
  "provider": "claude",
  "model": "claude-sonnet-4-5"
}}
```

Result:

```json
{"jsonrpc":"2.0","id":10,"result":{
  "text": "The manifest declares...",
  "provider": "claude",
  "model": "claude-sonnet-4-5"
}}
```

Params:

- `prompt` is required, at most 64 KiB.
- `system` is optional, at most 16 KiB.
- `provider` is optional; the configured background provider is the default.
- `model` is optional; the provider's default or background choice is the default.
- The result is capped at 64 KiB.
- Tools, approvals, questions, and workspace writes are refused by the runner.
- Do not expose the Operator app token to plugins. `llm.complete` is the narrow
  boundary for model access.

## session.start

Creates a new Monocode chat session with the user's default provider, switches
focus from the plugin surface to the new session, and submits an opening prompt.

Request:

```json
{"jsonrpc":"2.0","id":11,"method":"session.start","params":{
  "title": "Plugin: Port Inspector",
  "prompt": "Help me build this plugin."
}}
```

Result:

```json
{"jsonrpc":"2.0","id":11,"result":{"sessionId":42,"provider":"claude"}}
```

Params:

- `prompt` is required, at most 64 KiB.
- `title` is optional, at most 200 bytes.

## Errors

All host errors use JSON-RPC server error code `-32000`. Treat an error as a
failed call, not a reason to exit.
