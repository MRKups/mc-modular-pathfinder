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

## Errors

All host API failures use JSON-RPC server error code `-32000` and a readable
message. Typical cases:

- `Unknown host method: ...`
- `Invalid storage.get params: ...`
- `Storage key must not be empty`
- `Plugin storage is unavailable: the app has no data directory`

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

Good next candidates are `git.status`, `sessions.list`, `ui.open`, and
`clipboard.write`. They should be added only when a plugin needs them.
