# Monocode plugin system — implementation notes

- Status: phase 1 proof implemented.
- Architecture: [ARCHITECTURE.md](ARCHITECTURE.md)
- Wire contract: [PROTOCOL.md](PROTOCOL.md)
- Host API: [HOST_API.md](HOST_API.md)
- Example roadmap: [EXAMPLES_PLAN.md](EXAMPLES_PLAN.md)

## What is built

- Plugins are separate processes, written in any language.
- The host discovers manifests, starts children, performs a JSON-RPC handshake,
  routes UI events and host calls, and shuts children down.
- Plugin UI is a declarative native tree rendered by GPUI. No WebView or native
  library loading.
- Each plugin surface is a full-content sidebar page.
- Plugins can call a small fixed host API for workspace metadata and
  plugin-scoped storage.
- `npm run plugins` builds Node, Rust, Go, and Python examples into
  self-contained packages under `plugins/`, then launches the app with that
  directory: Hello is Node.js, Dashboard is Rust, Tasks is Go, and Focus is
  Python.
- The app behaves normally when no plugin directory is configured.

## Decisions

| Decision | Choice |
|---|---|
| Runtime | One OS process per plugin |
| Transport | JSON-RPC 2.0, one object per line over stdin/stdout |
| UI | `monocode_core::visual::Node`, rendered natively |
| Surface location | Sidebar page only |
| Surface order | Optional integer; lower numbers first |
| Discovery | `MONOCODE_PLUGIN_DIR` or `<app-data-dir>/plugins` |
| Host API | One fixed method table in `plugin_host/api.rs` |
| API versioning | None; new methods are additive |
| Plugin build | `npm run plugins` (build + launch); `npm run plugins:build` (build only) |
| Examples | Node.js, Rust, Go, Python |

## Manifest

```json
{
  "id": "com.example.hello",
  "name": "Hello",
  "protocol": 1,
  "command": ["node", "index.js"],
  "cwd": ".",
  "ui": [
    {
      "id": "hello",
      "title": "Hello",
      "icon": "sparkles",
      "order": 1
    }
  ]
}
```

Rules:

- `id` is unique across plugins.
- `protocol` must be `1`.
- `command` is an argv array, not a shell string.
- `cwd` is optional and must stay inside the plugin directory.
- Each surface needs a unique `id` and a `title`; `icon` defaults to
  `sparkles`.
- Unknown fields are rejected.

## Protocol

Host → plugin:

- `initialize` (request)
- `ui.open`
- `ui.close`
- `ui.event`
- `shutdown`

Plugin → host:

- `ui.set`
- `ui.close`
- `log`
- `workspace.info`, `storage.get`, `storage.set` (requests)

Limits:

- 2 MiB per transport line.
- 1 MiB per serialized UI tree.
- 512 nodes per validated UI tree.
- 5 second handshake timeout.
- 2 second shutdown grace before the child is killed.

## UI integration

- Plugin rows are appended to the main sidebar navigation with a small plug
  badge. There is no separate plugin section.
- Opening a plugin closes app views, search, previews, menus, settings, and
  Monos.
- The plugin page deselects session and app title tabs while it owns the body.
- The previously active session is restored when the plugin page closes.
- The right project tools panel remains available beside a plugin page.
- Navigating away sends `ui.close`; the plugin process stays alive.

## Host API

A plugin sends a normal JSON-RPC request with an `id`. The host replies with a
result or error.

Current methods:

- `workspace.info`
- `storage.get`
- `storage.set`

Storage lives at `<app-data-dir>/plugin-data/<plugin-id>/storage.json`.

## Files

```text
crates/monocode-plugin-host/       standalone runtime
crates/monocode-gpui/src/
  plugin_host.rs                   GPUI adapter
  plugin_host/api.rs               fixed host method table
  visuals.rs                       remote-tree VisualView mode
plugin-sources/
  hello/                           Node source
  dashboard/                       Rust source
  tasks/                           Go source
  focus/                           Python source
plugins/
  hello/ dashboard/ tasks/ focus/  generated runtime packages
  PROTOCOL.md
  HOST_API.md
  ARCHITECTURE.md
scripts/
  build-plugins.mjs                plugin build
```

## Verification

- Host integration test uses a real child process for handshake, host calls,
  UI events, and shutdown.
- Manifest, protocol, and line-limit tests pass.
- Adapter tests cover sidebar entries, open/render/close, handler rewriting,
  host API methods, title-tab de-selection, the plug badge, and the right panel.
- Native dev script tests pass.
- `npm run plugins:build` produces all four packages and the plugin processes
  pass a handshake/UI smoke test.
- `cargo clippy -D warnings` and `cargo fmt --check` pass.

## Limits

- Plugins are trusted processes. There is no sandbox and no permission system.
- A plugin receives the app data directory at startup and can access anything
  the user can.
- Plugin surfaces are sidebar pages only.
- A crashed plugin stays offline until the app restarts.
- The host API has only the methods listed above.
- There is no plugin installation, update, or marketplace flow.
