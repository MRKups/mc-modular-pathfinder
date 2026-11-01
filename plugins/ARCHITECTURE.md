# Monocode plugin architecture

Status: protocol v1, four examples implemented.
Scope: the native GPUI app (`crates/monocode-gpui`). Mobile and the legacy
Tauri desktop are out of scope.

This document explains how plugins are discovered, executed, rendered, and
integrated into the app. For the wire contract see [PROTOCOL.md](PROTOCOL.md);
for usage and examples see [README.md](README.md); for implementation notes see
[PLAN.md](PLAN.md).

## 1. Shape

Plugins are ordinary child processes. Monocode does not load native libraries or
run plugin JavaScript inside the app. A plugin reads JSON-RPC messages from
stdin, writes JSON-RPC messages to stdout, and describes its UI as a declarative
tree that the app renders with its existing native component model.

```text
plugin-sources/authoring-station/
  index.js                    plugin process
plugins/authoring-station/
  plugin.json                 runtime manifest: id, command, surfaces

        │  newline-delimited JSON-RPC 2.0 over stdin/stdout
        ▼
crates/monocode-plugin-host/  standalone runtime
  - discovery and manifest validation
  - process spawn, handshake, shutdown
  - request ids, timeouts, line/tree limits
  - surface tree cache and event routing
  - no GPUI, no monocode-core dependency

        │  HostEvent channel (mpsc)
        ▼
crates/monocode-gpui/src/plugin_host.rs   thin GPUI adapter
  - PluginSurfaceHost entity
  - sidebar rows and active-surface state
  - native VisualView instances
  - tree validation and handler translation

        │  native elements
        ▼
crates/monocode-gpui/src/visuals.rs
  - VisualView: existing native visual renderer
  - remote mode: renders a plugin tree and forwards control events
```

The important boundary is between the runtime crate and the app. The runtime is
headless and only depends on `serde` and `serde_json`; it can be tested without
opening a window. The adapter is the only place that knows about GPUI, the
native visual schema, and the shell's navigation state.

## 2. Design rules

- **Out of process.** One OS process per plugin. A plugin crash cannot take down
  the app or the UI thread.
- **Data, not code.** Plugin UI is a bounded `monocode_core::visual::Node` tree.
  There is no HTML, CSS, JavaScript, or native widget injection.
- **Native rendering.** Plugins inherit Monocode's typography, colors, spacing,
  scrolling, and accessibility instead of shipping a WebView.
- **Thin integration.** The app starts the runtime only when a plugin directory
  exists. No plugin directory means no plugin process and no behavior change.
- **Language agnostic.** `command` is an argv array. Any executable that speaks
  the protocol can be a plugin. The examples use Node.js, Rust, Go, and
  Python.
- **Fixed host surface.** Plugins get app data only through the explicit
  method table in `plugin_host/api.rs`, not by reaching into shell or store
  internals.

## 3. Components

### 3.1 `monocode-plugin-host`

A standalone crate with three modules:

| File | Responsibility |
|---|---|
| `src/manifest.rs` | Find `plugin.json`, reject malformed manifests, validate ids, commands, `cwd`, and surfaces. |
| `src/protocol.rs` | Wire types, JSON-RPC framing, protocol version, line/tree size limits. |
| `src/host.rs` | Spawn and supervise processes, handshake, read stdout/stderr, cache trees, route UI events and host calls, shut down. |
| `tests/host.rs` | End-to-end test with a real child process (`src/bin/test-plugin.rs`). |

`PluginRuntime` is the public handle. `start_with_options` discovers and spawns
plugins, then returns immediately with an event receiver. Each plugin gets a
supervisor thread for the handshake and reader threads for stdout and stderr.
The runtime never waits for a plugin on the GPUI thread.

### 3.2 GPUI adapter

`crates/monocode-gpui/src/plugin_host.rs` owns `PluginSurfaceHost`:

- starts the runtime from `Shell` when a plugin directory is configured;
- drains `HostEvent`s on a 40 ms timer and applies them on the GPUI thread;
- creates one `VisualView` per `(plugin, surface)`;
- keeps the last tree per surface;
- renders the active plugin surface and its header;
- builds the sidebar rows;
- maps plugin handler names to the native schema's numeric callback ids and
  maps events back;
- dispatches host requests through `plugin_host/api.rs` and replies through the
  runtime.

`crates/monocode-gpui/src/visuals.rs` gained an additive `VisualView::remote`
mode. Existing QuickJS transcript visuals still use the original script mode.

### 3.3 Shell integration

`crates/monocode-gpui/src/shell.rs` and its submodules only know about the
adapter, not the protocol:

| Change | Purpose |
|---|---|
| `Shell::plugin_host: Option<Entity<PluginSurfaceHost>>` | Present only when plugins are configured. |
| `Shell::open_plugin_surface` | Takes over the workspace body and closes app views, search, previews, menus, settings, and Monos. |
| `Shell::leave_plugin_surface` | Closes the active plugin surface when normal navigation resumes. |
| `Shell::plugin_surface_active` | Tells the rest of the shell which page owns the body. |
| `workspace_content` | Renders the plugin body and keeps the project tools panel beside it. |
| `plugin_navigation` | Appends plugin rows to the main sidebar navigation. |
| `active_title_tab` | Returns `None` while a plugin page is active. |
| Sidebar session rows | Suppress the session's active highlight while a plugin page is active. |

### 3.4 Asset source

The plugin badge uses the fallback icon `plug` in
`crates/monocode-gpui/src/assets.rs`. The plugin's own leading icon is resolved
by `plugin_icon`, with `sparkles` as the default.

### 3.5 Plugin build

`npm run plugins` builds the packages and then launches the native dev app with
`MONOCODE_PLUGIN_DIR` set to `plugins/`. The build step scans
`plugin-sources/` and assembles runnable packages under `plugins/`:

- Node sources are copied with `package.json` when present.
- Python sources are syntax-checked and copied.
- Rust sources are built with `cargo build --release`; `--locked` is used only
  when the source has a `Cargo.lock`. The binary is copied to
  `plugins/<name>/bin/`.
- Go sources are built with `go build`; the binary is copied to
  `plugins/<name>/bin/`.
- `docs/` and `README.md` beside a source are copied into the package.

`npm run plugins:build` runs only the build. The Monocode app build itself does
not build plugins, so the app can stay inert when no plugin packages are
present.

## 4. Lifecycle

### 4.1 Discovery and gating

`plugin_directory` in the adapter chooses the root:

1. `MONOCODE_PLUGIN_DIR` when set;
2. otherwise `<app-data-dir>/plugins`, when that directory exists.

The runtime scans for `plugin.json` in the root itself, or in each immediate
child directory. A broken manifest produces a discovery error but does not stop
other plugins. With no directory, `Shell::plugin_host` stays `None` and the app
behaves exactly as before.

### 4.2 Start and handshake

For each valid manifest:

1. The runtime spawns `command` with the manifest `cwd` (relative to the plugin
   directory) and piped stdin/stdout/stderr.
2. It sends `initialize` with the protocol version, app version, data directory,
   and log directory.
3. The plugin responds within five seconds with an optional protocol version and
   an optional replacement list of surfaces.
4. On success the adapter receives `Ready`; on failure it receives `Offline` and
   the process is killed.

Startup is asynchronous. The window is not blocked by a slow plugin.

### 4.3 Open and render

When the user clicks a plugin row:

1. The shell closes other workspace surfaces and tells the adapter to open the
   `(plugin, surface)` key.
2. The adapter sends `ui.open`.
3. The plugin replies with `ui.set` containing a native UI tree, or with a tree
   at any later point.
4. The adapter deserializes the tree into `Node`, rewrites handler names to
   numeric ids, validates it, and updates the surface's `VisualView`.
5. The shell renders the plugin page in the workspace body and keeps the
   project tools panel available on the right.
6. `PageTabs` provides a native table-of-contents rail with one content page.
   The content column is capped at 800 px and stays a single vertical flow.

The host caches the last tree even while the surface is closed, so a plugin can
continue work in the background and have a current tree ready when reopened.

### 4.4 Events

Native controls fire through the `VisualView` callback. The adapter:

1. looks up the numeric callback id in the surface's handler map;
2. restores the plugin's original handler string;
3. sends `ui.event` with `{surface, handler, value}` to the owning process.

The plugin updates its own state and may send a new `ui.set`. The app never
interprets plugin state.

### 4.5 Host calls

A plugin message with an `id` and a method name is a host request. The runtime
emits `HostEvent::Call`; the adapter routes it through `HostApi`; then
`PluginRuntime::respond` writes the JSON-RPC result or error back to the plugin.

The current surface is deliberately small:

- `workspace.info` returns the active project root and name;
- `storage.get` and `storage.set` read and write a plugin-scoped JSON store;
- `host.describe` returns the protocol, app version, method list, feature flags,
  limits, and plugin directory;
- `llm.providers` lists installed text-capable providers and cached models;
- `llm.complete` runs a bounded, text-only, read-only provider request on a
  worker thread and returns `{text, provider, model}`.

`llm.complete` must never run on the GPUI thread. The adapter validates and
prepares the request, spawns the blocking `provider::run_text` call, and sends
the result through `PluginRuntime::respond` when it finishes. Credentials stay
inside the app; plugins do not receive the Operator app token.

There is no separate host API version. Methods are additive; unknown methods
return an error. App modules expose normal functions, and the single match in
`plugin_host/api.rs` is the only place that knows plugin method names. See
[HOST_API.md](HOST_API.md).

### 4.6 Close and quit

- Navigating to a session, app view, search, settings, or another project sends
  `ui.close` and removes the plugin page. The process stays alive.
- The plugin page's close button does the same.
- On app quit, the runtime sends `shutdown` to every process, waits up to two
  seconds per plugin, then kills anything still running. A `Drop` fallback also
  reaps children if the adapter is released early.
- A crashed or offline plugin stays in the sidebar with an offline badge and
  shows an offline/error state instead of taking down the app.

## 5. UI contribution model

### 5.1 Surfaces

Every manifest surface becomes a sidebar row. A plugin with two surfaces gets
two rows. Rows are sorted by the optional surface `order`, then by label
(case-insensitive, then stable key), and appended after the built-in navigation
items. Only one plugin surface is active at a time; opening another sends
`ui.close` to the previous surface.

Plugin rows intentionally have no separate section header. They use the same
row styling as Inbox, Notes, and Automations, with:

- the manifest icon on the left;
- the surface title;
- an `offline` label when needed;
- a small `plug` badge on the right.

The badge and `aria_description("Plugin")` make plugin rows identifiable without
creating a separate visual group.

### 5.2 Page ownership

A plugin page is an independent workspace page:

- the sidebar plugin row is selected;
- no session or app title tab is selected, even though the previously active
  session remains open;
- session rows in the sidebar drop their active highlight;
- the previously active session is preserved and becomes selected again when
  the plugin page closes;
- the right project tools panel remains functional beside the plugin page.

This is implemented through `plugin_surface_active`, which reads the adapter's
active state. The shell does not persist plugin state in `Workspace` or
`TitleTab`, so existing workspace files remain compatible.

### 5.3 Handler translation

The native visual schema reserves handlers for digit/colon callback ids.
Plugins get readable strings such as `"click"` or `"toggle-task"`. The adapter
rewrites each control handler to a unique numeric id for the render tree and
keeps a per-surface `numeric id -> plugin handler` map. Events are translated
back before leaving the app. Plugins never see the native ids.

## 6. State, storage, and logging

- The plugin owns its own process state. The app stores only the last rendered
  tree per surface.
- `initialize` passes `dataDir` and `logDir`. The supported plugin-scoped store
  is `<app-data-dir>/plugin-data/<plugin-id>/storage.json`, reached through
  `storage.get` and `storage.set`.
- Plugin stderr is appended to
  `<app-data-dir>/plugin-logs/<plugin-id>.log` when a data directory exists.
  Lines are truncated to 8 KiB.
- `log` notifications are surfaced as host log events and printed to the app's
  stderr. They do not grant any host capability.
- UI trees are limited to 1 MiB serialized and 512 validated nodes. Transport
  lines are limited to 2 MiB. Invalid trees are rejected and the previous tree
  stays on screen.

## 7. Trust model

Plugins are trusted local programs, not a security sandbox. A plugin process
runs with the user's permissions and can read files, run commands, and use the
network. It also receives the app data directory at startup. Process isolation
provides crash and UI isolation only; the host API is a contract, not an
enforcement boundary.

There is no permission system. The host API exposes workspace metadata,
plugin-scoped storage, API metadata, and narrow model completions. Outside that
API, a plugin can access anything the user can. `llm.complete` is text-only and
tool-free, but it is not a sandbox for the plugin process itself.

## 8. Testing

| Layer | Coverage |
|---|---|
| `crates/monocode-plugin-host/tests/host.rs` | Discovery, real child process, handshake, open, host-call round-trip, event round-trip, shutdown, no orphan. |
| `crates/monocode-plugin-host/src/host.rs` | Framing and oversized-line limits. |
| `crates/monocode-plugin-host/src/manifest.rs` | Manifest and surface validation. |
| `crates/monocode-gpui/src/plugin_host.rs` | Sidebar entries, open/render/close, handler rewriting. |
| `crates/monocode-gpui/src/plugin_host/api.rs` | Workspace info, plugin-scoped storage round-trip, missing keys, invalid keys, unknown methods, describe limits, completion validation. |
| `crates/monocode-gpui/src/shell/sidebar_list.rs` | Clicking a plugin row opens the surface, deselects title tabs, shows the plug badge, and keeps the right panel rendered. |
| `crates/monocode-gpui/src/visuals.rs` | Remote-tree event forwarding, tree replacement, and code-editor rendering/events. |
| `scripts/dev-native.test.mjs` | Native dev runner, including the adaptive Cargo job formula. |

The four example packages are also probed directly over stdio for all surfaces
and native-tree validation. `npm run plugins:build` is the build check.

Run the proof manually with:

```bash
MONOCODE_PLUGIN_DIR="$PWD/plugins" npm run native:dev
```

## 9. Current shape

- process discovery, spawning, handshake, shutdown, and crash visibility;
- native declarative UI with interaction and live updates;
- sidebar rows, plugin pages with table-of-contents rails, title-tab
  de-selection, and a functional right project tools panel;
- a fixed host API for workspace metadata, plugin-scoped storage, API
  metadata, and text-only model completions;
- a native multiline code editor in the visual schema;
- file-backed plugin state and stderr logs;
- four purposeful examples: data dashboard, workspace analysis, debugging
  station, and plugin authoring station.
