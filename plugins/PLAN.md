# Monocode plugin system — implementation notes

- Status: runtime, host API, native UI bridge, and four purposeful examples implemented.
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
- Each plugin surface is a sidebar page with a left table-of-contents rail and
  a single 800 px content column.
- Plugins can call a fixed host API for workspace metadata, plugin-scoped
  storage, API metadata, and text-only model completions.
- `npm run plugins` builds Node, Rust, Go, and Python examples into
  self-contained packages under `plugins/`, then launches the app with that
  directory.
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
| Examples | Python, Rust, Go, Node.js |

## Examples

| Plugin | Language | Job |
|---|---|---|
| **Notebook** | Python | Persistent Python kernel, code cells, variables, and workspace data |
| **Workspace Analysis** | Rust | Current-repository review: languages, dependencies, files, TODO markers |
| **Debugger** | Go | Project process targets, inspector, ports, and build configurations |
| **Plugin Authoring** | Node.js | Docs, API reference, scaffold wizard, source editing, and LLM help |

The languages follow the workload: Python for the notebook kernel, Rust for
repository analysis, Go for OS process inspection, Node for tooling and text
generation.

## Manifest

```json
{
  "id": "com.example.authoring-station",
  "name": "Plugin Authoring",
  "protocol": 1,
  "command": ["node", "index.js"],
  "cwd": ".",
  "ui": [
    {
      "id": "authoring-station",
      "title": "Plugin Authoring",
      "icon": "plug",
      "order": 4
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
- `workspace.info`, `storage.get`, `storage.set`, `host.describe`,
  `llm.providers`, `llm.complete` (requests)

Limits:

- 2 MiB per transport line.
- 1 MiB per serialized UI tree.
- 512 nodes per validated UI tree.
- 5 second handshake timeout.
- 2 second shutdown grace before the child is killed.
- `llm.complete`: 64 KiB prompt, 16 KiB system, 64 KiB output.

## UI integration

- Plugin rows are appended to the main sidebar navigation with a small plug
  badge. There is no separate plugin section.
- Opening a plugin closes app views, search, previews, menus, settings, and
  Monos.
- The plugin page deselects session and app title tabs while it owns the body.
- The previously active session is restored when the plugin page closes.
- The right project tools panel remains available beside a plugin page.
- Navigating away sends `ui.close`; the plugin process stays alive.
- `CodeEditor` is a native multiline syntax-highlighting node built on the
  existing file editor.
- `PageTabs` is a native table-of-contents rail with one content page. The
  content column is capped at 800 px, centered, and stays a single vertical
  flow.

## Host API

A plugin sends a normal JSON-RPC request with an `id`. The host replies with a
result or error.

Current methods:

- `workspace.info`
- `storage.get`
- `storage.set`
- `host.describe`
- `llm.providers`
- `llm.complete`

Storage lives at `<app-data-dir>/plugin-data/<plugin-id>/storage.json`.

`llm.complete` runs `provider::run_text` on a worker thread: text-only,
read-only, tool-free, and credential-isolated. It does not expose the Operator
app token.

## Files

```text
crates/monocode-plugin-host/       standalone runtime
crates/monocode-gpui/src/
  plugin_host.rs                   GPUI adapter
  plugin_host/api.rs               fixed host method table
  visuals.rs                       remote-tree VisualView mode and code editor
plugin-sources/
  dashboard/                       Python notebook kernel and cells
  workspace-analysis/              Rust repository review
  debugging-station/               Go process and port debugger
  authoring-station/               Node docs/scaffold/editor/LLM station
plugins/
  <generated packages>             assembled by scripts/build-plugins.mjs
  PROTOCOL.md
  HOST_API.md
  ARCHITECTURE.md
scripts/
  build-plugins.mjs                plugin build
  plugins-dev.mjs                  build + launch wrapper
```
