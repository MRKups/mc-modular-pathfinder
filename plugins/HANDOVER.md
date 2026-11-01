# Monocode plugin system — handover

Snapshot: **2026-11-01**, branch **`kups/mc-plugins`**. The implementation
snapshot is commit **`f368128f`**; this handover is committed on top of it.

Upstream **`upstream/main` is `3fa189d1`** (native preview 12). It is already
fully merged into this branch through `71f5998a`, so there are no upstream
commits missing from the feature branch. The fork's `main` was fast-forwarded
from `b9ab5065` to `3fa189d1` as part of the final handover.

Canonical contracts live in:

- [PROTOCOL.md](PROTOCOL.md) — wire format, methods, limits, UI node examples.
- [ARCHITECTURE.md](ARCHITECTURE.md) — runtime/adapter/shell design.
- [HOST_API.md](HOST_API.md) — host methods available to plugins.
- [EXAMPLES_PLAN.md](EXAMPLES_PLAN.md) — what each example is meant to do.
- [PLAN.md](PLAN.md) — implementation decisions and file map.

This handover is the fast orientation; those documents remain authoritative.

---

## 1. TL;DR

Monocode plugins are **separate OS processes**. There is **no WebView**, no
in-process JavaScript, and no native library loading. A plugin:

1. reads newline-delimited JSON-RPC 2.0 from **stdin**;
2. writes newline-delimited JSON-RPC 2.0 to **stdout**;
3. describes its UI as a bounded declarative `monocode_core::visual::Node` tree;
4. receives control events such as `ui.event` and publishes a new tree with
   `ui.set`.

The app renders that tree with its normal **native Rust/GPUI** visual
components. Plugins inherit Monocode typography, colors, spacing, scrolling,
and accessibility. Plugin stdout is protocol traffic only; stderr is a log.

The four examples are independent programs in four languages:

| Surface | Plugin id / surface id | Runtime | Purpose |
|---|---|---|---|
| **Notebook** | `com.example.dashboard` / `dashboard` | Python 3 | Python notebook with a persistent child kernel |
| **Workspace Analysis** | `com.example.workspace-analysis` / `workspace-analysis` | Rust | Review the current repository |
| **Debugger** | `com.example.debugging-station` / `debugging-station` | Go | Read-only IDE-style process/port inspector |
| **Plugin Authoring** | `com.example.authoring-station` / `authoring-station` | Node.js ESM | Docs, scaffold, generate, edit plugins |

The old names survive in ids and package directories (`dashboard`,
`debugging-station`). Only the display titles changed. **Do not rename ids
casually** — storage and log paths are keyed by them.

### Recent work that produced this shape

- `71f5998a` — merged `upstream/main` `3fa189d1` (native preview 12) into this
  branch.
- `ea2335c7` — replaced horizontal selectors with a TOC rail and a narrow
  content column.
- `13147636` — centered the reading column and removed horizontal tabs/selects
  from all examples.
- `f368128f` — reworked the examples: Dashboard → Notebook, Workspace Analysis
  → current-repo review with no live scanning, Debugging Station → read-only
  Debugger, and tables → fit/truncate with optional weights.

---

## 2. Mental model

```text
plugin process (any language)
  stdin/stdout, one compact JSON object per line
        │
        ▼
crates/monocode-plugin-host/          headless runtime
  discovery, manifest validation, spawn, handshake,
  JSON-RPC framing, timeouts, tree cache, event routing
        │ HostEvent over std::sync::mpsc
        ▼
crates/monocode-gpui/src/plugin_host.rs   GPUI adapter
  sidebar entries, active surface, VisualView per surface,
  handler rewriting, tree validation, host API dispatch
        │ native GPUI elements
        ▼
crates/monocode-gpui/src/visuals.rs       native renderer
  VisualView, remote-tree mode, PageTabs, Table, CodeEditor
```

Important boundary: `monocode-plugin-host` knows nothing about GPUI or
`monocode-core`. It depends only on `serde`/`serde_json`. The adapter is the only
place that knows about GPUI, the native visual schema, and shell navigation.

The plugin process is **trusted**. Process isolation gives crash/UI isolation,
not a security sandbox. A plugin runs with the user's permissions and can read
files, run commands, and use the network. The host API is a contract, not an
enforcement boundary.

---

## 3. Repository map

| Path | Responsibility |
|---|---|
| `crates/monocode-plugin-host/src/manifest.rs` | Find `plugin.json`, validate plugin/surface ids, command, `cwd`, duplicate ids. |
| `crates/monocode-plugin-host/src/protocol.rs` | Wire types, request/notification/response framing, protocol version, line/tree limits. |
| `crates/monocode-plugin-host/src/host.rs` | Spawn/supervise processes, handshake, stdout/stderr readers, tree cache, events, host-call replies, shutdown. |
| `crates/monocode-plugin-host/tests/host.rs` | End-to-end test with a real child (`src/bin/test-plugin.rs`). |
| `crates/monocode-gpui/src/plugin_host.rs` | `PluginSurfaceHost` entity, sidebar rows, active surface, one `VisualView` per surface, handler rewriting, event forwarding. |
| `crates/monocode-gpui/src/plugin_host/api.rs` | The **only** place mapping plugin method names to app data. |
| `crates/monocode-gpui/src/visuals.rs` | Native renderer. `VisualView::remote` mode, `CodeEditor`, `PageTabs` layout, tables. |
| `crates/monocode-core/src/visual/model.rs` | Native `Node` schema, serde tags, validation, budgets, `PageTabs`, `Table.weights`. |
| `crates/monocode-core/src/visual/tests.rs` | Node/schema validation tests including table weights. |
| `crates/monocode-gpui/src/shell.rs` | Opens/leaves plugin surfaces, decides when a plugin owns the workspace body. |
| `crates/monocode-gpui/src/shell/sidebar_list.rs` | Appends plugin rows to the main sidebar; no separate plugin section. |
| `crates/monocode-gpui/src/shell/layout.rs` | Renders the active plugin beside the right project tools panel. |
| `plugin-sources/<name>/` | **Source of truth** for the four examples. |
| `plugins/` | Generated runtime packages, plus tracked `.md` docs. `plugins/*/` is gitignored. |
| `scripts/build-plugins.mjs` | Builds/copies each source into `plugins/<name>/`. |
| `scripts/plugins-dev.mjs` | Builds, then launches the native dev app with `MONOCODE_PLUGIN_DIR`. |
| `scripts/dev-native.mjs` | Native dev app runner. |

---

## 4. Lifecycle

### 4.1 Discovery and gating

The adapter resolves a plugin directory in `plugin_directory`:

1. `MONOCODE_PLUGIN_DIR`, when set;
2. otherwise `<app-data-dir>/plugins`, only when that directory exists.

The runtime scans `plugin.json` in the root itself, or in each immediate child
directory. A broken manifest produces a discovery error but does not stop other
plugins. With no plugin directory, `Shell::plugin_host` stays `None` and the app
behaves exactly as before.

All discovered plugins are started **at app startup**, not lazily on open.

### 4.2 Handshake

For each valid manifest:

1. Spawn `command` with `cwd` (relative to the plugin directory) and piped
   stdin/stdout/stderr.
2. Send `initialize` with protocol version, app version, `dataDir`, `logDir`.
3. Plugin must answer within **5 seconds** with an optional protocol version and
   an optional replacement `ui` surface list.
4. On success the adapter receives `Ready`; on failure/timeout/early exit the
   plugin is marked offline and killed.

Startup runs on a supervisor thread. The GPUI thread never waits for a plugin.
The adapter drains `HostEvent`s on a **40 ms** timer.

### 4.3 Open and render

1. Clicking a plugin row calls `Shell::open_plugin_surface`.
2. The shell closes app views, search, previews, menus, settings, and Monos;
   it deselects session/app title tabs but keeps the right project panel.
3. `HostApi` is told the current workspace root.
4. The adapter sends `ui.open`.
5. The plugin replies with `ui.set` for that surface, now or later.
6. The adapter deserializes into `Node`, rewrites handler strings to numeric
   callback ids, validates, and updates the surface's `VisualView`.
7. Plugin UI appears in the workspace body.

The runtime caches the last tree per surface even while closed, so a plugin can
keep working and have a current tree when reopened.

### 4.4 Events and push updates

Native controls fire through the `VisualView` callback. The adapter:

1. looks up the numeric id in that surface's handler map;
2. restores the plugin's original handler string;
3. sends `ui.event` with `{surface, handler, value}`.

The plugin updates its own state and may send `ui.set`. The app never interprets
plugin state. Push updates are supported at any time, not only after events.

### 4.5 Host calls

A plugin message with an `id` and a method name is a request to the host. The
runtime emits `HostEvent::Call`; the adapter dispatches it and calls
`PluginRuntime::respond` with a result or JSON-RPC error.

### 4.6 Close, quit, failure

- Navigating to a session/app view/search/settings/another project sends
  `ui.close` and removes the plugin page. **The process stays alive** and keeps
  its state.
- The close button on the plugin page does the same.
- On app quit, the runtime sends `shutdown` to every plugin, waits **2 seconds**
  per plugin, then kills it. A `Drop` fallback reaps children if the adapter is
  released early.
- A crashed/offline plugin stays in the sidebar with an offline state and an
  error banner; it does not take down the app.
- Invalid/oversized trees are rejected and the previous tree stays on screen.

---

## 5. Manifest contract

Example:

```json
{
  "id": "com.example.my-plugin",
  "name": "My Plugin",
  "protocol": 1,
  "command": ["node", "index.js"],
  "cwd": ".",
  "ui": [
    {
      "id": "main",
      "title": "My Plugin",
      "icon": "sparkles",
      "order": 10
    }
  ]
}
```

Rules:

| Field | Rule |
|---|---|
| `id` | Required, unique across plugins, 1–96 ASCII letters/digits/`.`/`_`/`-`. |
| `name` | Required, 1–80 characters. |
| `protocol` | Required, must be `1`. |
| `command` | Required non-empty argv array, not a shell string. |
| `cwd` | Optional relative directory inside the plugin; absolute paths and `..` rejected. |
| `ui` | Optional list of surfaces; unique surface ids; each surface becomes a sidebar row. |
| `ui[].id` | Required, 1–96 ASCII id characters. |
| `ui[].title` | Required, 1–80 characters. |
| `ui[].icon` | Optional, default `sparkles`. |
| `ui[].order` | Optional integer; lower first, then title. |

Unknown fields are rejected (`deny_unknown_fields`). The `initialize` response
may replace the manifest surface list, but the replacement is revalidated.

### Icons

`plugin_icon` in `crates/monocode-gpui/src/plugin_host.rs` maps a small set of
names to native SVG icons:

```text
inbox, note/notes, zap/automation, search, terminal, git,
bug/debug, dashboard, clock/timer, plug
```

Anything else falls back to `sparkles`. Current examples use `note`, `search`,
`bug`, and `plug`. Language icons in Workspace Analysis are **emoji text** in
table cells, not native icons.

### Sidebar behaviour

Every surface is a sidebar page with the surface icon, title, an `offline` label
when needed, and a small `plug` badge. Rows are sorted by `order`, then title,
and appended after Inbox/Notes/Automations. There is no separate plugin section
and no plugin title tab.

---

## 6. Wire protocol

Transport:

- JSON-RPC 2.0, one compact UTF-8 JSON object per line.
- Requests have integer ids; notifications omit `id`.
- Max transport line: **2 MiB**.
- Max serialized UI tree: **1 MiB**; native validator also caps **512 nodes**
  and **16 levels**.
- `code_editor` value capped at **64 KiB**.

### Host → plugin

All are notifications except `initialize`.

| Method | Params | Meaning |
|---|---|---|
| `initialize` | `{protocol, appVersion, dataDir, logDir}` | Handshake request; reply within 5 s. |
| `ui.open` | `{surface}` | Surface opened; reply with `ui.set`. |
| `ui.close` | `{surface}` | Surface no longer visible; process stays alive. |
| `ui.event` | `{surface, handler, value}` | A native control fired. |
| `shutdown` | `{}` | App quitting; exit promptly or be killed after 2 s. |

Event values:

| Control | `value` |
|---|---|
| button | `null` |
| toggle | boolean |
| input | string |
| code_editor | full text |
| select | selected index |
| slider | number |
| tabs | selected index |
| page_tabs | selected index |

### Plugin → host

| Method | Params | Meaning |
|---|---|---|
| `ui.set` | `{surface, tree}` | Replace that surface's tree. Unknown surfaces/errors rejected. |
| `ui.close` | `{surface}` | Remove cached tree; process stays alive. |
| `log` | `{level, message}` | Log line; truncated to 8 KiB. |
| host request | `id` + method | Dispatched through the fixed host API. |

Handshake response:

```json
{"jsonrpc":"2.0","id":1,"result":{
  "protocol":1,
  "ui":[{"id":"main","title":"My Plugin","icon":"sparkles"}]
}}
```

A plugin may be opened multiple times; it should republish its current tree.
If it does not answer `ui.event` with `ui.set`, the current tree stays.

### Failure behaviour

- Handshake timeout, invalid response, or early exit → offline and killed.
- Oversized/unparseable stdout can end the protocol session and reap the child.
- Host request replies use error code `-32000` and a readable message.
- Unknown host methods return an error; a plugin should treat this as a failed
  call, not exit.

---

## 7. Host API

A plugin sends a normal JSON-RPC request with an `id`, method, and params. The
host replies with a result or error. There is no separate API version; methods
are additive. Unknown methods error.

Current methods:

| Method | Result / purpose |
|---|---|
| `workspace.info` | `{root, name}` for the active project. Root is refreshed when a surface opens. |
| `storage.get` | `{key}` → stored JSON value or `null`. |
| `storage.set` | `{key, value}` → `null`. |
| `host.describe` | Protocol, app version, plugin dir, methods, feature flags, limits. |
| `llm.providers` | Installed text-capable providers, cached models, selected account, background choice. Does not start a provider process. |
| `llm.complete` | One bounded, text-only, read-only completion. Runs on a worker thread; returns `{text, provider, model}`. |

Storage rules:

- Keys: non-empty, max 256 bytes.
- Values: arbitrary JSON.
- Store: max 1 MiB, replaced on write.
- Path: `<app-data-dir>/plugin-data/<plugin-id>/storage.json`.
- No app data dir → storage calls error.

`llm.complete` rules:

- `prompt` required, max 64 KiB.
- `system` optional, max 16 KiB.
- `provider`/`model` optional; background provider/model or provider default.
- Result capped at 64 KiB.
- Text-only, read-only, tool-free, no approvals/questions/workspace writes.
- Credentials stay inside Monocode; plugins never receive the Operator app token.
- No session transcript access and no streaming.

Adding a method:

1. Add a match arm in `crates/monocode-gpui/src/plugin_host/api.rs`.
2. Implement the small provider function there.
3. If it needs GPUI or async work, special-case it in `plugin_host.rs` under
   `HostEvent::Call` (as `llm.providers` / `llm.complete` are).
4. Add tests and document it in `plugins/HOST_API.md` and the Authoring API
   notes if applicable.

Other app modules should expose ordinary functions/data; they should not branch
on plugin ids or method names.

---

## 8. Native UI trees

`tree` is a serialized `monocode_core::visual::Node`. It is **data**, never HTML,
CSS, JavaScript, or host commands. The schema lives in
`crates/monocode-core/src/visual/model.rs`; rendering lives in
`crates/monocode-gpui/src/visuals.rs`.

Common nodes:

```json
{"kind":"column","gap":12,"children":[]}
{"kind":"row","gap":8,"children":[]}
{"kind":"grid","columns":2,"gap":8,"children":[]}
{"kind":"card","gap":8,"children":[]}
{"kind":"text","text":"Hello","size":"body","color":"ink"}
{"kind":"stat","label":"Clicks","value":"3","color":"accent"}
{"kind":"table","columns":["A","B"],"rows":[["1","2"]],"weights":[1.0,2.0]}
{"kind":"button","label":"Click","handler":"click","disabled":false}
{"kind":"toggle","label":"Enabled","value":true,"handler":"toggle"}
{"kind":"input","label":"Name","value":"","placeholder":"Type","handler":"name"}
{"kind":"code_editor","label":"Source","value":"console.log(1)","language":"js","height":360,"handler":"source","read_only":false}
{"kind":"page_tabs","labels":["Guide","API"],"selected":0,"handler":"page","nav_width":180,
 "children":[{"kind":"text","text":"Page body","size":"body","color":"ink"}]}
```

Also available: `select`, `slider`, `tabs`, `chart`, `gauge`, `donut`,
`heatmap`, `progress`, `canvas`, `scene`. Text sizes: `caption`, `small`,
`body`, `subtitle`, `title`, `hero`. Colors: `ink`, `muted`, `faint`, `accent`,
`positive`, `negative`, `warning`, `series1`…`series6`.

### 8.1 `page_tabs` — the only page navigation used by the examples

`page_tabs` is a documentation-style table-of-contents rail on the left and
**one** content child on the right.

Contract:

- 1–12 labels.
- `selected` must be a valid index.
- At most **one** content child.
- `nav_width` optional, default **180**, range **140–360**.
- The plugin sends only the selected page's body and republishes when the
  selection changes. The host does **not** keep all pages and switch locally.

Layout in `visuals.rs`:

```text
outer full-width flex, justify-center
  group max width = rail*2 + gap*2 + 800
    rail (left, nav_width)  |  content panel (max 800)  |  right spacer (rail width)
```

- The left rail is clamped to at least 140 px, and otherwise to at most 32% of
  the available width.
- The content column is capped at **800 logical pixels**.
- The right spacer has the same width as the rail, so the reading column itself
  is centered while the TOC sits beside it.
- Only `children[0]` is rendered.
- Clicking a rail item sends `ui.event` with the numeric index.
- The rail highlight updates only after the plugin republishes with the new
  `selected` value.

**The 800 px cap is a `page_tabs` behavior, not a global app constraint.** A
plugin whose root is a plain `column` fills the available width. All four
examples root at `page_tabs`.

### 8.2 `table` — fit, truncate, no horizontal scroll

Tables are intentionally dense and width-aware:

- Max 24 columns, 250 rows.
- Header cells max 256 bytes; body cells max 2048 bytes.
- Optional `weights` sets relative column widths.
- If `weights` is absent/empty, every column gets weight `1.0`.
- If present, it must have exactly one positive value per column; each is
  validated in **0.1–20.0**.
- Rendering uses flex cells: `flex_grow(weight)`, `flex_shrink(1)`,
  `flex_basis(0)`, `overflow_hidden`, `truncate`.
- Padding is 8 px horizontal / 5 px vertical.
- There is **no horizontal scrolling**; long cells truncate.
- Very narrow columns can truncate aggressively; choose weights deliberately
  (for example paths get 3–4, numeric columns 0.6–0.8).

This replaced the old fixed-width, horizontally scrolling table. The change is
in `crates/monocode-core/src/visual/model.rs` and
`crates/monocode-gpui/src/visuals.rs`; the test is
`visual::tests::table_weights_are_optional_and_must_match_columns`.

### 8.3 `code_editor`

Native multiline, syntax-highlighted editor built on the existing file editor.

- `language` is a short extension such as `js`, `py`, `rs`, `go`, `json`,
  `toml`, `md`, `txt`; unknown languages render plain.
- `height` is clamped to a sensible range.
- `read_only` defaults to `false`.
- Value capped at 64 KiB.
- Used for notebook cells/output, authoring docs/code, generated responses, and
  scaffold source editing.

### 8.4 Handler rewriting

The native visual schema reserves handlers for numeric QuickJS callback ids.
Plugins use readable strings (`"run:3"`, `"tab"`, `"refresh"`).

`rewrite_handlers` walks the tree, replaces each control handler with a unique
numeric string, and stores `numeric -> plugin handler` in a per-surface map. The
`VisualView` event callback maps back before sending `ui.event`. Plugins never
see numeric ids.

Handlers are strings and may be reused on multiple controls; each occurrence
gets its own numeric id. Code editors are leaves for this traversal; containers
(`column`, `row`, `grid`, `card`, `page_tabs`) are walked.

### 8.5 Validation budgets

A tree must satisfy the native schema and the node budget:

- Max 512 validated nodes and 16 levels.
- Max 1 MiB serialized tree.
- Max 128 KiB of validated text across the whole tree.
- Native component-specific limits apply (chart series, canvas primitives,
  table cells, source bytes, etc.).

Invalid trees are logged to stderr and rejected; the last valid tree stays.

### 8.6 Design conventions in this branch

- Page/view navigation uses **vertical rails only**. No horizontal tabs or
  selectors in the four example UIs.
- Content is a single vertical flow. Rows are only for compact control groups.
- Every example roots at `page_tabs` with `nav_width: 180`.
- No filler status prose. Status text is action feedback or an error.
- Tables are dense and fit the container.
- The Notebook title uses `note`; Debugger uses `bug`; ids stay unchanged.

---

## 9. Build and run

```bash
npm run plugins:build   # build packages only
npm run plugins         # build, then launch native dev app with plugins/
```

`scripts/plugins-dev.mjs` builds first, then runs `scripts/dev-native.mjs` with:

```bash
MONOCODE_PLUGIN_DIR="$PWD/plugins"
```

The app itself never builds or installs plugins at startup. It only runs what
the manifest names. Restart after changing a manifest, command, or protocol
code; protocol v1 has no live reload.

### Build script behaviour

`scripts/build-plugins.mjs` scans `plugin-sources/` directories that contain
`plugin.json` and assembles `plugins/<source-dir>/`:

| Source marker | Build |
|---|---|
| `Cargo.toml` | `cargo build --release` (`--locked` only if `Cargo.lock` exists); copy `target/release/<dir>` to `bin/<dir>`; command becomes `./bin/<dir>`. |
| `go.mod` | `go build -o bin/<dir> .`; command becomes `./bin/<dir>`. |
| `main.py` | Python 3 syntax-check; copy `main.py`; command becomes `[<detected python3>, "main.py"]`. |
| `index.js` | Copy `index.js`; command becomes `["node", "index.js"]`. |

It also copies `docs/`, `README.md`, and `package.json` when present, rewrites
the manifest into the package, deletes stale package dirs, and fails if a build
fails. Rust/Go binary names follow the **source directory name**.

Generated `plugins/*/` is gitignored; `plugins/*.md` docs are tracked. Python
packages embed the absolute interpreter path detected at build time, so rebuild
after moving machines/interpreters.

### Discovery locations

```text
$MONOCODE_PLUGIN_DIR/<plugin>/plugin.json
<app-data-dir>/plugins/<plugin>/plugin.json
```

With neither populated, nothing changes in the app.

### Manual protocol debugging

Run a plugin directly and type protocol lines into stdin, or use a small script.
A useful sequence:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":1,"appVersion":"0.16.0","dataDir":null,"logDir":null}}
{"jsonrpc":"2.0","id":2,"method":"workspace.info","params":{}}
{"jsonrpc":"2.0","method":"ui.open","params":{"surface":"dashboard"}}
{"jsonrpc":"2.0","method":"ui.event","params":{"surface":"dashboard","handler":"tab","value":1}}
{"jsonrpc":"2.0","method":"shutdown","params":{}}
```

Read stdout lines and inspect `ui.set` trees. Plugin stderr is separate.
Logs live at `<app-data-dir>/plugin-logs/<plugin-id>.log` when a data dir exists.

---

## 10. The four plugins

### 10.1 Notebook (`plugin-sources/dashboard/main.py`)

- **Manifest:** id `com.example.dashboard`, surface `dashboard`, title
  **Notebook**, icon `note`, order 1. Python command `python3 main.py`.
- **Runtime:** Python 3 standard library only, ~666 lines.
- **Pages:** `Notebook`, `Data`, `Variables`, `Kernel` via `page_tabs`.
- **Kernel:** a long-lived child process started with
  `sys.executable -u -c <KERNEL_SCRIPT>`.
  - Holds a persistent `namespace` dict.
  - Each request executes the cell source; the last expression is evaluated and
    returned as `result`.
  - Captures stdout/stderr.
  - Returns the current variables (up to 60), type names, and repr values
    truncated at 300 chars.
  - 30-second response timeout; on timeout the kernel is stopped.
  - Restart on project change (`set_project`), explicit restart, or shutdown.
- **Notebook page:** initial cell imports `csv`, `json`, `statistics` and creates
  `rows = []`. Each cell has `In [n]`/`In [ ]`, Run, Delete, a code editor, and a
  read-only output editor when there is output. Toolbar: Run all, Clear outputs,
  Add cell.
- **Limits/state:** max 12 cells (oldest dropped), in-memory only, no notebook
  file persistence, no streaming output, run-all is synchronous and can block
  the plugin loop for up to 30 s per cell.
- **Data page:** scans CSV/TSV/JSON up to 24 files, max 20 MB per file, depth 4,
  skipping VCS/build dirs. Shows Path/Kind/Size. A path input inserts a
  ready-to-run loader cell for CSV/TSV (`csv.DictReader`) or JSON (`json.load`).
- **Variables page:** Name/Type/Value table from the last kernel response.
- **Kernel page:** Python version, executable, working directory, status, and
  cell/execution/variable counts.
- **Gotcha:** this is a small custom kernel, not IPython/Jupyter. There are no
  magics, rich display objects, or notebook-file format.
- **Gotcha:** the kernel uses the plugin's interpreter; if the plugin runs under
  a different Python, imports match that interpreter, not the user's shell.
- **Gotcha:** variables and cells disappear when the plugin process exits.

### 10.2 Workspace Analysis (`plugin-sources/workspace-analysis/src/main.rs`)

- **Manifest:** id `com.example.workspace-analysis`, surface
  `workspace-analysis`, title **Workspace Analysis**, icon `search`, order 2.
  Rust binary `./bin/workspace-analysis`.
- **Runtime:** Rust + `serde_json`, no other runtime deps, ~1599 lines.
- **Pages:** `Overview`, `Languages`, `Dependencies`, `Files`, `TODO`.
- **Scan policy:** no live rescanning. Scan happens on initialize/open and when
  **Rescan** is pressed. Toolbar is one Rescan button; `Last scan: HH:MM:SS UTC ·
  N ms` is shown above the page body.
- **Bounds:** 50,000 files, 64 MiB scanned content, 512 KiB read per file, depth
  10, max 200 TODO entries. Skips `.git`, `node_modules`, `target`, `dist`,
  `build`, `vendor`, `__pycache__`, `.next`, `.turbo`, `coverage`, `.idea`,
  `.cache`, `Pods`, `DerivedData`, etc.
- **Overview:** summary metrics, evidence checks (README, license, tests, CI,
  lockfile, clean git tree, toolchains), warnings, top languages, manifests.
- **Languages:** bar chart plus a full breakdown table. Each language name is
  prefixed with an emoji via `language_icon` (`🦀` Rust, `🐍` Python, `🐹` Go,
  `🟨` JavaScript, `🟦` TypeScript, etc.; `📄` fallback). These are text glyphs,
  not native SVGs.
- **Dependencies:** manifests table with `Path`, `Ecosystem`, `Name`,
  `Declared`, `Resolved`, `Source`. Resolution:
  - sort manifests shallowest path first;
  - resolve at most **4 ecosystems**;
  - at most one successfully resolved manifest per ecosystem;
  - each tool command has a **3-second** timeout;
  - `npm ls --all --json --silent` (requires `package-lock.json` or
    `npm-shrinkwrap.json`; counts unique dependency names recursively);
  - `cargo metadata --format-version 1 --locked` (counts resolve nodes; labeled
    `cargo metadata (workspace)`);
  - `go list -m all` (skips the module line);
  - `poetry show --format=json` (requires `poetry.lock`);
  - `composer show --format=json` (requires `composer.lock`);
  - `bundle list` (requires `Gemfile.lock`);
  - `deno info --json`.
  - If a tool is unavailable/fails, `Resolved` is `—` and `Source` stays
    `manifest`; `Declared` is the fallback count parsed from the manifest.
- **Files:** top 100 source files by line count (excludes `Other`), columns
  `Path`, `Language`, `Lines`, `Size`, `Markers`, weighted `3.8/1.2/0.7/0.8/0.7`.
  Caption reports total files/lines and that 100 are shown.
- **TODO:** summary, counts by tag, marker inventory with path/line/tag/excerpt.
- **Checks/warnings:** README, license, tests, CI, lockfile, clean git tree,
  toolchain availability, TODO count, large files, truncated scan, missing
  manifests.
- **Gotcha:** dependency resolution runs real toolchain commands during the
  scan; Rescan can take several seconds and the plugin event loop is blocked
  while it runs.
- **Gotcha:** `cargo metadata` at the workspace root reports the workspace total
  and is attributed to the root `Cargo.toml`; individual crate rows keep their
  declared counts. This is intentional but confusing if read as per-crate totals.

### 10.3 Debugger (`plugin-sources/debugging-station/main.go`)

- **Manifest:** id `com.example.debugging-station`, surface
  `debugging-station`, title **Debugger**, icon `bug`, order 3. Go binary
  `./bin/debugging-station`.
- **Runtime:** Go standard library, ~2163 lines.
- **Pages:** `Debug`, `Processes`, `Ports`, `Build`.
- **Read-only:** never attaches, launches, kills, or changes processes/sockets.
- **Sampling:** `ps` for processes and `lsof`/`netstat` for network, on
  initialize/open/Refresh. There is **no live ticker**; the old live refresh was
  removed.
- **Debug:** finds processes whose args/command contain the project path or the
  project directory basename, sorted by CPU, max 12. Selecting a target is a
  vertical set of buttons (`target:<pid>`). Inspector fields: PID, parent PID,
  state, CPU, memory/RSS, command, arguments. Ports for the selected PID are
  shown when present.
- **Processes:** filter input (name/args/PID), Refresh, top 100, columns
  PID/PPID/CPU/MEM/RSS/State/Command, weights `0.6,0.6,0.7,0.7,0.8,0.8,4.2`.
  Sorting defaults to CPU desc; `Sort` values for memory/name/PID exist but no UI
  currently changes the sort.
- **Ports:** socket state counts, listening sockets (max 120), connections
  (max 120).
- **Build:** detects build systems up to 4 levels down, max 24 systems; shows
  projects, selected project, commands, targets, and tool availability/versions.
  `Refresh` on this page re-runs detection.
- **Gotcha:** target matching by basename can include unrelated processes whose
  command line happens to contain the repository name.
- **Gotcha:** the snapshot struct still contains load/memory/disk fields and
  helper functions from the old activity dashboard, but `sample` now fills only
  processes and network. This is dead-ish code and a cleanup opportunity, not a
  feature.

### 10.4 Plugin Authoring (`plugin-sources/authoring-station/index.js`)

- **Manifest:** id `com.example.authoring-station`, surface `authoring-station`,
  title **Plugin Authoring**, icon `plug`, order 4. Node.js ESM, `node index.js`.
- **Runtime:** Node.js standard library only, ~1260 lines.
- **Pages:** `Guide`, `API`, `Scaffold`, `Generate`, `Editor`.
- **Guide:** loads every Markdown file in bundled `docs/`
  (`authoring.md`, `host-api.md`, `quickstart.md`, `security.md`,
  `ui-nodes.md`) and renders them as one vertical page using a small native
  Markdown parser. It handles headings, paragraphs, lists, blockquotes, rules,
  pipe tables, and fenced code blocks (as read-only `code_editor`s). Each
  document is capped at ~180 nodes, with a warning when truncated.
- **API:** live `host.describe` + `llm.providers`; tables for methods, limits,
  features, and providers; errors shown as warning text.
- **Scaffold:** four steps — Identity → Destination → Review → Created. It
  writes only on Create, refuses a non-empty target, requires a valid plugin id,
  and keeps the target inside the project root. It generates `plugin.json`,
  `index.js`, `package.json`, and `README.md`.
  - **Current UI is Node-only:** `state.scaffold.language` is initialized to 0
    and no control changes it. `LANGUAGES`, Python/Rust/Go templates, and the
    language branches still exist in code but are unreachable from the UI. This
    matches the delivered scope; remove or re-expose deliberately.
- **Generate:** system prompt, prompt, and a free-text model field. There is no
  provider picker: it uses the first installed provider (`providerIndex = 0`)
  and the host default when none is listed. Sends an async `llm.complete`,
  shows a read-only response editor, and can copy the response into the source
  editor. No streaming, no tools, no session context. The host runs the provider
  on a worker thread; the plugin stays responsive.
- **Editor:** load/save a project file by path inside the active project root;
  native syntax-highlighted editor; dirty indicator. No live preview.
- **Status:** `withStatus` prepends action feedback/errors to the current page.
  It is not a dashboard.
- **Startup:** `initialize` responds, publishes, then requests `workspace.info`
  and refreshes `host.describe`/`llm.providers`; `ui.open` refreshes workspace
  info and republishes.

---

## 11. Verification

The important checks:

```bash
# Runtime contract and real child process
cargo test --manifest-path crates/monocode-plugin-host/Cargo.toml

# Native visual schema (including PageTabs + Table.weights)
cargo test --manifest-path crates/monocode-core/Cargo.toml visual::tests

# GPUI adapter / remote visuals / shell sidebar
cargo test --manifest-path crates/monocode-gpui/Cargo.toml

# Lints
cargo clippy --manifest-path crates/monocode-gpui/Cargo.toml --all-targets -- -D warnings

# Build all four packages
npm run plugins:build

# Syntax/build checks for individual sources
node --check plugin-sources/authoring-station/index.js
python3 -m py_compile plugin-sources/dashboard/main.py
(cd plugin-sources/debugging-station && gofmt -l . && go vet ./... && go build ./...)
cargo build --release --locked --manifest-path plugin-sources/workspace-analysis/Cargo.toml

# Launch the native app with the built plugins
MONOCODE_PLUGIN_DIR="$PWD/plugins" npm run native:dev
```

The previous implementation run also used ad hoc probes that launched each
built plugin over stdio, exercised every `page_tabs` index, and validated all
trees against the native schema. Those probe scripts were temporary and are
**not committed**. If you need repeatable coverage, add a committed
`scripts/plugin-probe.mjs` rather than recreating them in `/tmp`.

Current plugin-host tests cover discovery, real child process handshake, open,
host-call round-trip, event round-trip, shutdown, and no orphan. Adapter tests
cover sidebar entries, open/render/close, handler rewriting, and remote trees.

---

## 12. Known gaps and sharp edges

### Docs that are stale relative to the code

- `plugin-sources/README.md` still describes Dashboard as a CSV/TSV/JSON “data
  workbench” and Debugging Station as live system inspection. It should say
  Notebook and Debugger.
- `plugins/ARCHITECTURE.md` §9 ends with “four purposeful examples: data
  dashboard, workspace analysis, debugging station, and plugin authoring
  station.” Update to Notebook/Debugger.
- `plugins/README.md` says Authoring bundles “Node.js, Python, Rust, and Go
  scaffold templates.” The templates exist in `index.js`, but the UI currently
  exposes only Node.js.
- `plugins/EXAMPLES_PLAN.md` is otherwise in good shape.

### Behavioural constraints

- **No live plugin reload.** Restart after manifest/protocol/command changes.
- **All plugins start at app startup.** Opening a surface only sends `ui.open`.
- **`page_tabs` renders only `children[0]`.** The plugin must send the selected
  page body and republish on `tab` events.
- **The 800 px column is provided by `page_tabs`.** A plugin without it fills
  the available width.
- **No horizontal table scrolling.** Long cells truncate; use `weights`.
- **Handler names are surface-local and tree-local.** Rebuilding a tree replaces
  the map; stale numeric ids cannot fire.
- **Plugin state is in the process.** Closing a surface keeps it alive; app quit
  or a crash loses it.
- **No sandbox.** Review plugin code before running it.
- **Icon set is small.** Unknown manifest icons become `sparkles`; language
  icons are emoji text.
- **No session access.** `llm.complete` takes a direct prompt/system prompt only.
- **No `plugins.status`/`plugins.reload`/streaming LLM** yet; these are listed
  as deferred in `EXAMPLES_PLAN.md`.
- **Workspace Analysis resolves at most 4 ecosystems**, one manifest per
  ecosystem; other `Resolved` cells remain `—`.
- **Notebook state is not persisted** and output is captured, not streamed.
- **Python package manifests embed the build-time interpreter path.**
- **Debugger target matching is heuristic** (path/basename substring).
- **Debugger still contains unused system-snapshot code** from the old activity
  dashboard.
- **Scaffold language branches are dead code** in the current UI.

### Git / release state

- Branch: `kups/mc-plugins`.
- Implementation commit: `f368128f` — “Rework examples around notebook, repo
  review, and debugger”; the handover commit follows it.
- `origin` is `MRKups/monocode` (the fork); `upstream` is
  `hardbeat920/monocode`.
- `upstream/main` is `3fa189d1` and is fully merged here.
- `origin/main` and local `main` are fast-forwarded to `3fa189d1`.
- `origin/kups/mc-plugins` is the fork branch for this work.
- `plugins/*/` is generated and gitignored; edit `plugin-sources/`, then rebuild.

---

## 13. Common change recipes

### Add a new example plugin

1. Create `plugin-sources/<name>/plugin.json` and a program.
2. Follow the smallest Node quickstart in
   `plugin-sources/authoring-station/docs/quickstart.md`.
3. Build with `npm run plugins:build`.
4. Launch with `npm run plugins` (or set `MONOCODE_PLUGIN_DIR`).
5. Make sure the manifest `command` matches the built artifact.

### Add a page to a plugin

1. Add a label to `page_tabs.labels`.
2. Add a branch in the plugin's page/tree function.
3. On `ui.event` with handler `tab`, clamp/update the selected index and
   republish.
4. Return exactly `[body]` as `page_tabs.children`.
5. Keep the rail labels short; `nav_width` defaults to 180.

### Add a table

1. Emit `{"kind":"table","columns":[...],"rows":[[...]],"weights":[...]}`.
2. Weights must match columns exactly if provided; use 0.6–1 for IDs/numbers,
   2–4 for paths/text.
3. Keep rows under 250 and cells under 2048 bytes; truncate in the plugin when
   needed.
4. Do not add width/scroll fields; the renderer owns layout.

### Add a host method

1. Add the match arm in `plugin_host/api.rs`.
2. Implement and test it.
3. If it needs `App`, GPUI, or async I/O, handle it in `HostEvent::Call` in
   `plugin_host.rs` instead of the plain `call` match.
4. Document it in `plugins/HOST_API.md`; update Authoring's API notes if needed.
5. Keep it additive; unknown methods must keep returning an error.

### Change the native schema

1. Edit `crates/monocode-core/src/visual/model.rs` (serde + validation).
2. Render it in `crates/monocode-gpui/src/visuals.rs`.
3. Add a validation/rendering test.
4. Update `plugins/PROTOCOL.md` and the bundled Authoring docs
   (`plugin-sources/authoring-station/docs/ui-nodes.md`).
5. Rebuild plugins only if their trees use the new node.

---

## 14. Glossary

| Term | Meaning |
|---|---|
| **Plugin** | An OS process with `plugin.json` and a protocol-speaking program. |
| **Surface** | One manifest-declared page/sidebar row, identified by `surface` id. |
| **Runtime** | `monocode-plugin-host`; headless process/JSON-RPC supervisor. |
| **Adapter** | `crates/monocode-gpui/src/plugin_host.rs`; GPUI/shell integration. |
| **Host API** | Fixed request/response methods in `plugin_host/api.rs`. |
| **`ui.set`** | Plugin → host notification replacing a surface tree. |
| **`ui.event`** | Host → plugin notification for a native control. |
| **`page_tabs`** | Native TOC rail + single 800 px content page. |
| **`Node`** | `monocode_core::visual::Node`; the only UI language. |
| **Handler** | Plugin-defined string attached to a control; translated to/from numeric native ids. |

---

## 15. First commands for the next agent

```bash
cd /Users/michalkups/projects/monocode-fork
git status --short --branch          # confirm kups/mc-plugins
git log --oneline -4                 # f368128f is the implementation snapshot
npm run plugins:build                # regenerate plugins/
MONOCODE_PLUGIN_DIR="$PWD/plugins" npm run native:dev
```

Read in this order:

1. this file;
2. `plugins/PROTOCOL.md`;
3. `plugins/ARCHITECTURE.md`;
4. `plugins/HOST_API.md`;
5. `crates/monocode-gpui/src/plugin_host.rs`;
6. `crates/monocode-gpui/src/plugin_host/api.rs`;
7. `crates/monocode-gpui/src/visuals.rs` (`VisualView::remote`, `PageTabs`,
   `Table`, `CodeEditor`);
8. the example source you need to change.
