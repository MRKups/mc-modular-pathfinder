# Example plugin roadmap

Status: agreed direction, not implemented. The current four examples are
capability demos; replace them with four plugins that each have a real job.

## Target examples

| Plugin | Language | Purpose | Showcases |
|---|---|---|---|
| **Dashboard** | Python | Data visualization workbench for CSV/JSON in the workspace | Charts, gauges, donuts, heatmaps, progress, stats, tables, dynamic `ui.set` |
| **Workspace Analysis** | Rust | Dynamic analysis and health of any reasonable codebase | Filesystem scanning, manifests, dependencies, TODOs, toolchain checks |
| **Debugging Station** | Go | Live system inspection | Processes, CPU/memory, network connections, listening ports, live refresh |
| **Plugin Authoring Station** | Node.js | Design, scaffold, document, and generate plugins | Docs/API browser, file scaffolding, LLM generation, code editing |

The first three can be built with the current plugin API. The authoring station
needs platform work first.

## Language decision

Recommended mapping (swapped from the first suggestion):

- **Python** — Dashboard. Good for CSV/JSON parsing and data shaping.
- **Rust** — Workspace Analysis. Fast, ignore-aware walking; good TOML/JSON parsing.
- **Go** — Debugging Station. Natural for process/network inspection and concurrency.
- **Node.js** — Plugin Authoring Station. Best fit for templates, docs, child
  processes, and LLM JSON calls.

If the authoring station must be Rust, that is feasible, but Node will be faster
to build and iterate.

## API audit

### Dashboard — no new host API

- Uses existing `ui.set` and control events.
- Recommended data source: a CSV/JSON file selected from the workspace, not
  fabricated service metrics.
- Dynamic behavior: switch dataset, chart type, series, aggregation, panel
  layout; add/remove panels; toggle live refresh.

### Workspace Analysis — no new host API required

- `workspace.info` gives the workspace root.
- The plugin can read files and run tools (`git`, `cargo`, `go`, etc.) directly.
- Current model is trusted plugins with user permissions; no sandboxed file API.
- Must discover languages/manifests dynamically and cap work on large repos.
- Avoid an arbitrary “health score”; show evidence and warnings.

### Debugging Station — system APIs, no new host API

- Go can inspect processes, memory, network, ports, and load.
- Read-only by default; do not add process killing.
- macOS permissions may limit some network/process details; degrade gracefully.
- Optional future API: `plugins.status` to map OS processes to plugin IDs,
  surfaces, status, and last error.

### Plugin Authoring Station — platform work required

Available today:

- Bundled docs and examples
- File scaffolding into a chosen directory
- Reading manifests and existing plugin sources

Missing:

1. **LLM access**
   - No host API for models or completions.
   - Reuse `provider::run_text`, which is text-only, read-only, and tool-free.
   - Proposed methods:
     - `llm.providers` → installed providers/models/accounts.
     - `llm.complete` `{prompt, system?, provider?, model?}` → `{text, provider, model}`.
   - Must be asynchronous. Current host calls run synchronously on the GPUI
     thread; a provider call there would freeze the UI.
   - Do not expose the Operator app token. It grants sessions, notes, worktrees,
     and other mutating capabilities.

2. **Code editing**
   - `visual::Node::Input` is single-line.
   - Add a `CodeEditor` node with language, value, handler, height.
   - Reuse the existing `Input::editor` / `Code` syntax-highlighting machinery.

3. **Live preview / reload**
   - No plugin reload API exists; generated plugins require an app restart.
   - Options:
     - Nested preview: the station spawns the candidate plugin and proxies its
       JSON-RPC/UI tree as its own preview.
     - Host API: `plugins.reload` or `plugins.load` to rescan/start plugins.
     - v1 fallback: generate files, show instructions, restart to test.
   - Recommended: ship v1 without live preview; add preview/reload after the
     station’s core is working.

4. **API documentation**
   - Bundle docs for now.
   - Optional `host.describe` method: protocol version, host methods, plugin
     directory, limits. Keeps the station’s API reference authoritative.

5. **Session context (optional)**
   - If “sessions” means using the configured provider/model, `llm.complete`
     covers it.
   - If it means reading conversation transcripts, add read-only
     `sessions.list` / `sessions.read`; this is a separate privacy decision.
   - Do not expose `sessions.start` / `sessions.send` to plugins.

## Implementation order

### Phase 1 — current API

1. Dashboard (Python), data-driven.
2. Workspace Analysis (Rust).
3. Debugging Station (Go).

### Phase 2 — platform additions

4. Plugin Authoring Station (Node):
   - docs + scaffold + code preview
   - `llm.providers` + async `llm.complete`
   - `CodeEditor` visual node
   - optional `host.describe`
   - preview/reload later

## Open decisions

- Dashboard: require a real CSV/JSON file, or include a labeled sample dataset?
- Authoring station v1: is live preview required, or is generate/save/restart
  acceptable?
- Do we need existing session transcripts as LLM context, or only direct
  prompt-to-LLM generation?

## Current repo state

- Existing examples: Hello (Node), Dashboard (Rust), Tasks (Go), Focus (Python).
- Sources: `plugin-sources/<name>/`; generated packages: `plugins/<name>/`.
- `npm run plugins` builds all packages and launches the app.
- All current plugin work is uncommitted on `kups/mc-plugins`.
