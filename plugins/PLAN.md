# Monocode External Plugin System — Implementation Plan

Status: phase 1 implemented (proof)  
Branch: `kups/mc-plugins`  
Scope: native GPUI app + `monocode-core`; mobile and Tauri/legacy desktop are out of scope.

Current architecture: [ARCHITECTURE.md](ARCHITECTURE.md).

## 1. What we are building

A small, standalone plugin system for Monocode where:

- plugins are **separate processes**, written in any language;
- plugins register **UI contributions** (sidebar items first, tabs later);
- plugin UI is **declarative native UI**, rendered by Monocode — plugins do not
  compile GPUI code or inject native widgets;
- the plugin runtime is a **separate crate** with no dependency on Monocode’s UI;
- Monocode integration is thin, additive, and inert when no plugins are present.

**WASM is explicitly deferred.** The first proof uses process plugins. The
plugin contract is runtime-agnostic, so a WASM runtime can be added later behind
the same manifest/UI/event interfaces.

## 2. Decisions

| Decision | Choice | Why |
|---|---|---|
| Plugin runtime | Separate process, JSON-RPC over stdio | Matches “external process” requirement; any language; simple |
| UI mechanism | Declarative native component tree | Reuses `monocode_core::visual::Node` and `VisualView`; native/theme-aware; cross-platform |
| UI placement, phase 1 | Sidebar item + full-content plugin surface | Avoids changing serialized `TitleTab`/workspace state |
| UI placement, phase 2 | Optional `TitleTab::Plugin { id }` | Same surface type; additive once the proof works |
| Plugin transport | JSON-RPC 2.0, newline-delimited | Language-agnostic, easy to debug, no schema compiler |
| Plugin discovery | `MONOCODE_PLUGIN_DIR` or `<data_dir>/plugins` | Explicit for development; no behavior change otherwise |
| Runtime gating | Start only when a plugin dir exists / env var is set | Normal app behavior is untouched |
| WASM | Out of scope for this iteration | Avoid runtime + host-function work before the UI proof |
| Example plugin | Node.js | Fastest to iterate; protocol is language-agnostic |

## 3. Non-goals for this iteration

- WASM, Wasmtime, Extism, or any in-process guest runtime.
- Plugin installation, updates, signing, marketplace, or dependency resolution.
- A real security sandbox. A plugin process runs with the user’s permissions.
- Plugin-to-plugin service discovery / dependency graph.
- Broader host APIs for sessions, files, git, network, or notifications.
- Native dylib plugins.
- Tauri/mobile integration.
- Arbitrary HTML/WebView plugin UI.

## 4. Architecture

```text
plugins/hello/
  plugin.json               manifest
  index.js                  plugin process

        │ JSON-RPC over stdin/stdout
        ▼
crates/monocode-plugin-host/          standalone runtime
  - manifest discovery / validation
  - process spawn, handshake, shutdown
  - request/response routing, timeouts
  - UI contribution registry
  - surface state and event routing
  - no GPUI, no monocode-core dependency

        │ host events / UI trees
        ▼
crates/monocode-gpui/src/plugin_host.rs   thin adapter
  - PluginSurfaceHost entity
  - sidebar items
  - remote-tree VisualView instances
  - forwards interactions to the plugin process

        │ native rendering
        ▼
VisualView (existing Node renderer, new remote mode)
```

The plugin runtime crate should depend only on `serde`, `serde_json`, and
standard library primitives. Keeping it UI-free makes it testable headlessly and
makes the integration patch easier to merge.

## 5. Plugin contract

### 5.1 Manifest

`plugin.json` lives in each plugin directory:

```json
{
  "id": "com.example.hello",
  "name": "Hello",
  "version": "0.1.0",
  "protocol": 1,
  "command": ["node", "index.js"],
  "cwd": ".",
  "ui": [
    {
      "id": "hello",
      "title": "Hello",
      "icon": "sparkles",
      "placements": ["sidebar"]
    }
  ],
  "permissions": []
}
```

Rules:

- `id` is a stable reverse-DNS or dotted identifier; it must be unique.
- `protocol` is the plugin protocol version; the host rejects unsupported versions.
- `command` is argv (not a shell string) so paths with spaces work.
- `ui[].id` is unique within the plugin; the host uses `plugin_id/surface_id`.
- `placements` is a hint. Phase 1 supports `sidebar`; phase 2 adds `tab`.
- Unknown fields are rejected or warned about, not silently ignored.

### 5.2 Transport

- JSON-RPC 2.0 over stdin/stdout.
- One compact JSON object per line; UTF-8; no embedded newlines.
- Requests have integer ids; notifications have no id.
- Host → plugin:
  - `initialize` (request)
  - `ui.open` (notification)
  - `ui.close` (notification)
  - `ui.event` (notification)
  - `shutdown` (notification)
  - later: `command.invoke`, `host.*`
- Plugin → host:
  - `ui.set` (notification): new tree for a surface
  - `ui.close` (notification)
  - `log` (notification)
  - `workspace.info`, `storage.get`, `storage.set` (requests)
  - later: `event.emit`, more host methods
- Limits:
  - max line: 2 MiB
  - max UI tree: 1 MiB / 512 nodes
  - handshake timeout: 5 s
  - shutdown grace: 2 s, then kill
- The host must not block the GPUI thread waiting for a plugin response.

### 5.3 Lifecycle

1. Host scans plugin directories and validates manifests.
2. Host spawns each enabled plugin with its working directory.
3. Host sends `initialize` with protocol version, app version, data dir, and log dir.
4. Plugin responds with its runtime manifest and UI contributions.
5. Host registers contributions and starts routing UI events.
6. On UI open, host sends `ui.open` (or the plugin sends the initial `ui.set`).
7. On app quit, host sends `shutdown`, waits briefly, then kills the child.
8. On crash, host marks the plugin offline, removes or disables its UI, and
   optionally restarts with backoff (phase 3).

### 5.4 UI tree

Use the existing native component model from `monocode_core::visual::Node`.

Already available: `Column`, `Row`, `Grid`, `Card`, `Text`, `Stat`, `Table`,
`Chart`, `Gauge`, `Donut`, `Heatmap`, `Progress`, `Button`, `Toggle`, `Input`,
`Select`, `Slider`, `Tabs`, `Canvas`, `Scene`.

Example:

```json
{
  "kind": "column",
  "gap": 12,
  "children": [
    { "kind": "text", "text": "Clicked 3 times", "size": "title", "color": "ink" },
    { "kind": "button", "label": "Click me", "handler": "click", "disabled": false }
  ]
}
```

Rules:

- UI trees are data only. No HTML, CSS, scripts, or host API calls.
- Handlers are plugin-defined strings, returned unchanged in `ui.event`. The
  adapter rewrites them to the numeric callback ids the native visual schema
  requires and maps events back; plugins never see the numeric ids.
- The host validates the tree with `Node::validate()` before rendering.
- Invalid trees are rejected with a visible error; the previous tree stays.
- UI state is owned by the plugin. The host stores only the last rendered tree.

### 5.5 Event flow

```text
plugin process                         Monocode
     │                                    │
     │ ── initialize ────────────────────►│
     │ ◄────────────────── manifest ──────│
     │                                    │
     │ ── ui.set { surface, tree } ──────►│  render
     │                                    │
     │ ◄──────── ui.event { handler } ────│  click / input
     │                                    │
     │ ── ui.set { new tree } ───────────►│  update
```

The plugin owns interaction semantics. The host does not run plugin logic; it
only renders the last tree and reports control events.

## 6. Monocode UI integration

### 6.1 Phase 1: sidebar item + full-content surface

- `PluginSurfaceHost` (new GPUI entity) owns the runtime, surface views, active
  surface, and plugin errors.
- `sidebar_navigation()` appends plugin rows directly to the main navigation,
  built from manifests. Each row uses the same visual language as
  Inbox/Notes/Automations and carries a small plug badge on the right.
- Clicking a plugin row calls `PluginSurfaceHost::open(surface_id)` and notifies
  the Shell.
- `Shell::workspace_content()` renders the active plugin surface before the
  normal workspace.
- Navigating to a session, app view, settings, or search calls
  `PluginSurfaceHost::close()` so the plugin surface does not linger.
- The plugin surface renders:
  - a small header with title + close button;
  - the active `VisualView` in remote-tree mode;
  - an inline error state if the plugin is offline or the tree is invalid.

No changes to `TitleTab`, `Workspace`, or persisted UI state in phase 1.

### 6.2 `VisualView` remote mode

Add an additive mode to `crates/monocode-gpui/src/visuals.rs`:

```rust
impl VisualView {
    pub(crate) fn remote(
        id: String,
        title: String,
        tree: Node,
        on_event: Rc<dyn Fn(&str, Value)>,
    ) -> Self;

    pub(crate) fn set_tree(&mut self, tree: Node, cx: &mut Context<Self>);
}
```

- `remote()` sets the tree directly and stores the event callback; no QuickJS.
- `dispatch()` checks for a runtime first (existing behavior); otherwise it
  forwards `(handler, value)` to the callback.
- `set_tree()` updates the tree and emits `VisualChanged`.
- The existing script-based `VisualView::new` path remains unchanged.
- Add tests proving both modes work and existing visual tests still pass.

This reuses the entire existing Node renderer, including inputs, sliders, tabs,
tables, charts, and theme-aware colors. It avoids duplicating ~1,000 lines of UI
rendering.

### 6.3 Phase 2: title tabs (after the proof)

Add one variant:

```rust
TitleTab::Plugin { id: String }
```

Then handle it in the same places as `Session`/`App`/`Mono`:

- visibility, open, close, restore, title/icon, drag/selection, context menu;
- drop the tab if the plugin/surface no longer exists.

The plugin manifest can then use `"placements": ["sidebar", "tab"]`.
The same `PluginSurface` is reused; no plugin-side changes.

## 7. Minimal diff map

New code:

- `crates/monocode-plugin-host/`
  - manifest, protocol, process supervision, host/registry, tests
- `crates/monocode-gpui/src/plugin_host.rs`
  - runtime adapter, `PluginSurfaceHost`, surface rendering
- `plugins/`
  - `PLAN.md`, `PROTOCOL.md`, `README.md`, `hello` example

Touched existing code:

| File | Change |
|---|---|
| `crates/monocode-gpui/Cargo.toml` | add workspace member + `monocode-plugin-host` dependency |
| `crates/monocode-gpui/src/main.rs` | add `mod plugin_host;` |
| `crates/monocode-gpui/src/shell.rs` | one field, one init, one subscription, one render branch, close calls in navigation |
| `crates/monocode-gpui/src/shell/sidebar_list.rs` | append plugin rows and click handling |
| `crates/monocode-gpui/src/visuals.rs` | remote-tree mode (additive) |

**No changes to `monocode-core`, `Store`, providers, settings, or persisted
workspace state in phase 1.**

Integration should be runtime-gated:

- `MONOCODE_PLUGIN_DIR=/path/to/plugins` enables the host.
- If unset, no plugin processes start and no plugin UI appears.
- The host may also scan `<data_dir>/plugins` for installed plugins.

## 8. Milestones

### M0 — Contract and skeleton

- [x] Create `crates/monocode-plugin-host` as a workspace member of the GPUI workspace.
- [x] Define `PluginManifest`, `SurfaceDescriptor`, protocol message types, and error types.
- [x] Write `plugins/PROTOCOL.md` with the manifest, framing, methods, limits, and UI tree.
- [x] Add manifest/protocol unit tests.
- [x] Add a fake plugin fixture for integration tests.

**Done when:** `cargo test -p monocode-plugin-host` passes with no app integration.

### M1 — Process runtime, headless

- [x] Spawn plugin processes with piped stdin/stdout/stderr.
- [x] Implement newline-delimited JSON-RPC framing.
- [x] Implement request ids, pending responses, and timeouts.
- [x] Implement `initialize`, `ui.event`, `ui.set`, `log`, `shutdown`.
- [x] Implement discovery from `MONOCODE_PLUGIN_DIR` / data dir.
- [x] Implement clean shutdown and child reaping.
- [x] Add a headless integration test using the fake plugin.

**Done when:** a test starts a plugin, receives its manifest and UI tree, sends
an event, receives an updated tree, and shuts down with no orphan process.

### M2 — First UI proof

- [x] Add `VisualView` remote-tree mode and tests.
- [x] Build `PluginSurfaceHost` entity.
- [x] Start the runtime from `Shell` and receive host events on the GPUI thread.
- [x] Add plugin rows to the main sidebar navigation with a plug badge.
- [x] Render the active plugin surface in `workspace_content`.
- [x] Add close-on-navigation behavior.
- [x] Build the `hello` Node plugin: sidebar item, text, input, button, counter.
- [x] Translate plugin handler names to the native visual schema's numeric ids
      and map events back, without changing `monocode-core`.
- [x] Smoke test: the native app starts the plugin process and graceful quit
      reaps it.
- [x] End-to-end wiring test: a GPUI test clicks the sidebar row, opens the
      surface, and renders its native tree. The live process round-trip is
      covered by the host integration test; no manual window click-through was
      performed.

**Done when:** a plugin in `plugins/hello` adds a sidebar item and interactive UI
without changing `monocode-core`.

### M3 — Lifecycle and resilience

- [x] Handle handshake timeout and invalid manifests.
- [x] Handle plugin crash/offline state and show an error surface.
- [x] Ensure app quit/restart kills plugin children.
- [x] Capture stderr to a log file and expose a minimal log view or log path.
- [x] Validate duplicate IDs and malformed UI trees.
- [x] Restart-with-backoff policy explicitly deferred; offline state and errors
      are shown instead.

**Done when:** a crashing plugin cannot hang or leak the app, and the user sees a
clear error.

### M4 — Tab placement (deferred to phase 2)

- [ ] Add `TitleTab::Plugin { id }` and handle all exhaustive matches.
- [ ] Persist/restore plugin tabs safely; drop unknown plugin tabs.
- [ ] Add `"placements": ["tab"]` support to the manifest.
- [ ] Reuse the same `PluginSurface`; no plugin-side changes.

**Done when:** a plugin can open as a title tab and survives app restart.

### M5 — Host APIs and permissions (in progress)

- [x] Add fixed host request dispatch and JSON-RPC responses.
- [x] Expose `workspace.info`, `storage.get`, and `storage.set`.
- [ ] Add more host methods as plugins need them (for example `git.status`,
      `sessions.list`, `ui.open`, `clipboard.write`).
- [ ] Define capability names and manifest permissions.
- [ ] Add a host event bus for subscriptions and plugin-to-plugin communication.
- [ ] Evaluate WASM as a second runtime behind `PluginRuntime`.

## 9. Testing

### Rust tests

- [x] Manifest parsing: required fields, IDs, protocol version, duplicate surfaces.
- [x] Protocol framing: line splitting, malformed JSON, request/response round-trip.
- [x] Handshake request routing and pending response ids.
- [x] Process lifecycle: start, handshake, event round-trip, clean shutdown, no orphan.
- [x] UI tree deserialization and validation through the adapter.
- [x] `VisualView` remote mode: set tree, dispatch event, update tree.
- [ ] Explicit handshake-timeout and crash-injection tests (behavior is
      implemented; covered by inspection and the offline surface).

### GPUI tests

- [x] `VisualView` remote mode renders a tree and forwards a click.
- [x] `PluginSurfaceHost` lists, opens, renders, and closes a surface.
- [x] Plugin handler names are rewritten to native callback ids and mapped back.
- [x] Sidebar row click-through opens and renders the plugin surface.
- [ ] Full GPUI suite (run at the end of the effort).

### Manual demo

```bash
MONOCODE_PLUGIN_DIR="$PWD/plugins" npm run native:dev
```

1. App startup starts all four external Node processes (verified together).
2. Sidebar shows the plugin item (GPUI integration test).
3. Clicking it opens the plugin surface (GPUI integration test).
4. Typing and clicking the button sends events to the Node process (host
   integration test covers the protocol round-trip; the live window click was
   not manually exercised).
5. Quitting the app cleanly stops the plugin process (verified with
   `MONOCODE_DEV_RELOAD=1`; no orphan remained).

## 10. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Plugin process is not a security sandbox | Document clearly; add capability/permission model before third-party distribution |
| Plugin inherits full environment | Later: `env_clear`, minimal PATH/HOME, explicit capability env |
| Orphaned plugin processes | Reap on quit, kill on drop, test shutdown path |
| GPUI thread blocked by plugin I/O | Reader thread + async channel; never wait on the UI thread |
| `VisualView` remote mode regresses transcript visuals | Keep script mode untouched; add focused tests |
| Large/frequent UI trees | Enforce size/node limits; throttle updates later |
| Merge conflicts with upstream | New crate + concentrated adapter; phase 1 avoids `TitleTab`/workspace changes |
| Cross-platform process behavior | Use std process APIs; test macOS first; note Windows job-object work later |
| Plugin UI cannot express arbitrary widgets | Extend the shared `Node` schema centrally when needed; no per-plugin native code |

## 11. Open questions

- Should plugin discovery also scan a project-local `.monocode/plugins` directory?
- Do we want plugin UI to remember its last surface across restarts in phase 1?
- Which host data should require explicit permission once capabilities are enforced?
- Should plugins be enabled per-project or globally?
- What is the restart policy for crashing plugins?
- When do we add tabs versus keeping the sidebar-only placement?
- How do we surface plugin logs and errors without adding a full settings page?

## 12. File map

```text
plugins/
  PLAN.md
  PROTOCOL.md
  README.md
  hello/
    plugin.json
    index.js
  pulse/
    plugin.json
    index.js
  tasks/
    plugin.json
    index.js
  gallery/
    plugin.json
    index.js

crates/monocode-plugin-host/
  Cargo.toml
  src/lib.rs
  src/manifest.rs
  src/protocol.rs
  src/host.rs                   # discovery, processes, JSON-RPC, routing
  src/bin/test-plugin.rs        # deterministic test fixture
  tests/host.rs

crates/monocode-gpui/
  Cargo.toml                    # add dependency/workspace member
  src/main.rs                   # mod plugin_host;
  src/plugin_host.rs            # new adapter, surface host, sidebar rows
  src/shell.rs                  # field/init/open/close integration
  src/shell/sidebar_list.rs     # plugin section
  src/shell/layout.rs           # active plugin surface branch
  src/shell/app_views.rs        # close on app-view navigation
  src/shell/search.rs           # close on search
  src/shell/settings.rs         # close on settings
  src/visuals.rs                # remote-tree mode
```

## 13. Definition of done for the proof

- A plugin directory can be pointed at with `MONOCODE_PLUGIN_DIR`.
- The app starts the plugin process and registers its sidebar item.
- Clicking the item opens a native plugin surface.
- The surface can display text, inputs, buttons, and a counter.
- A click travels to the external process and back, updating the UI.
- Closing the surface and quitting the app cleanly stops the plugin.
- With no plugin directory, the app behaves exactly as before.
- No changes are required in `monocode-core`, providers, sessions, or the store.
