# Monocode host API

Plugins can ask the app for a small, fixed set of things. The surface is
intentionally plain JSON-RPC: a plugin sends a request with an `id`, a method
name, and params. Monocode replies with a normal result or error.

There is no separate API version. Methods are additive; unknown methods return
an error. If a method ever needs a breaking change, add a new method name.

## Request

```json
{"jsonrpc":"2.0","id":7,"method":"workspace.info","params":{}}
```

Successful response:

```json
{"jsonrpc":"2.0","id":7,"result":{"root":"/Users/me/project","name":"project"}}
```

Error response:

```json
{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"Unknown host method: workspace.nope"}}
```

Notifications such as `ui.set`, `ui.close`, and `log` have no `id`. A message
with an `id` and an unrecognized method is treated as a host request.

## Methods

### `workspace.info`

Returns the active project root and its directory name.

Params: none.

```json
{"root":"/Users/me/project","name":"project"}
```

The root is refreshed when a plugin surface is opened, so switching projects
and reopening a plugin gives the new root.

### `storage.get`

Reads one value from the plugin's own storage.

```json
{"jsonrpc":"2.0","id":8,"method":"storage.get","params":{"key":"last-run"}}
```

Result is the stored JSON value, or `null` when the key is absent.

### `storage.set`

Writes one JSON value to the plugin's own storage.

```json
{"jsonrpc":"2.0","id":9,"method":"storage.set","params":{
  "key":"last-run",
  "value":{"status":"passed","at":"2026-10-31T15:00:00Z"}
}}
```

Result is `null`.

Storage rules:

- Keys are non-empty strings of at most 256 bytes.
- Values are arbitrary JSON.
- The store is capped at 1 MiB.
- Data lives at `<app-data-dir>/plugin-data/<plugin-id>/storage.json`.
- Writes replace the file.
- With no app data directory, storage calls return an error.

### `host.describe`

Returns the protocol version, app version, plugin directory, fixed method list,
feature flags, and limits. Use it instead of copying a versioned API list.

```json
{"jsonrpc":"2.0","id":10,"method":"host.describe","params":{}}
```

```json
{
  "protocol": 1,
  "appVersion": "0.16.0",
  "pluginDir": "/Users/me/project/plugins",
  "methods": ["workspace.info", "storage.get", "storage.set", "host.describe", "llm.providers", "llm.complete"],
  "features": {"codeEditor": true, "llm": true},
  "limits": {"lineBytes": 2097152, "uiTreeBytes": 1048576, "uiNodes": 512, "sourceBytes": 65536, "llmPromptBytes": 65536, "llmSystemBytes": 16384}
}
```

### `llm.providers`

Lists installed providers that can run one-shot text work, with cached models
and the selected account. It does not start a provider process.

```json
{
  "providers": [
    {
      "id": "claude",
      "label": "Claude Code",
      "account": "default",
      "models": [{"id": "claude-sonnet-4-5", "name": "Sonnet 4.5"}],
      "background": true
    }
  ],
  "background": {"provider": "claude", "model": "claude-haiku-4-5"}
}
```

An empty `models` list means the provider's default model.

### `llm.complete`

Runs a bounded, text-only, read-only completion through a configured provider.
The host calls the provider on a worker thread; credentials never reach the
plugin and the plugin remains responsive.

Request:

```json
{"jsonrpc":"2.0","id":11,"method":"llm.complete","params":{
  "prompt": "Explain this plugin manifest",
  "system": "You are a concise Monocode plugin expert.",
  "provider": "claude",
  "model": "claude-sonnet-4-5"
}}
```

Result:

```json
{"jsonrpc":"2.0","id":11,"result":{
  "text": "The manifest declares...",
  "provider": "claude",
  "model": "claude-sonnet-4-5"
}}
```

Params:

- `prompt` is required, at most 64 KiB.
- `system` is optional, at most 16 KiB.
- `provider` is optional; the configured background provider is the default.
- `model` is optional; the background choice or provider default is used.
- The result is capped at 64 KiB.
- The runner refuses tools, approvals, questions, and workspace writes.
- Do not expose the Operator app token to plugins. This is the narrow model
  boundary.

## UI code editor

The native UI schema now includes a multiline, syntax-highlighted
`code_editor` node:

```json
{"kind":"code_editor","label":"Source","value":"console.log(1)","language":"js","height":360,"handler":"source","read_only":false}
```

`language` is a short extension such as `js`, `py`, `rs`, `go`, `json`, or
`toml`. Unknown languages stay plain. See [PROTOCOL.md](PROTOCOL.md#ui-trees).

## Errors

All host API failures use JSON-RPC server error code `-32000` and a readable
message. Typical cases:

- `Unknown host method: ...`
- `Invalid storage.get params: ...`
- `Storage key must not be empty`
- `Plugin storage is unavailable: the app has no data directory`
- `Prompt must not be empty`
- `Unknown provider: ...`
- `No text-capable provider is installed`

A plugin should treat an error as a normal failed call, not as a reason to exit.

## Adding a method

Host methods live in one place:

```text
crates/monocode-gpui/src/plugin_host/api.rs
```

Add a match arm, implement the small provider function, add a test, and document
it here. Other Monocode modules should expose ordinary functions or data; they
should not branch on plugin ids or method names.

The current API is deliberately small:

- `workspace.info`
- `storage.get`
- `storage.set`
- `host.describe`
- `llm.providers`
- `llm.complete`
