# Example plugin roadmap

Status: implemented. The four capability demos were replaced with four plugins
that each have a real job.

## Target examples

| Plugin | Language | Purpose | Showcases |
|---|---|---|---|
| **Dashboard** | Python | Data visualization workbench for CSV/TSV/JSON in the workspace | Charts, gauges, donuts, heatmaps, progress, stats, tables, dynamic `ui.set`, live file reload |
| **Workspace Analysis** | Rust | Dynamic analysis and health of any reasonable codebase | Filesystem scanning, manifests, dependencies, TODOs, toolchain checks |
| **Debugging Station** | Go | Build-system detection and live system inspection | Manifest/script/task detection, tool checks, processes, memory, disks, sockets, live refresh |
| **Plugin Authoring** | Node.js | Design, scaffold, document, and generate plugins | Vertical docs/API/scaffold/generate/editor pages, step wizard, source editor, LLM generation |

## Language decision

- **Python** — Dashboard. CSV/JSON parsing and data shaping.
- **Rust** — Workspace Analysis. Fast, bounded repository walking.
- **Go** — Debugging Station. Process/network/system inspection without deps.
- **Node.js** — Plugin Authoring. Templates, docs, child-process-free file IO,
  and LLM JSON calls.

## Implementation status

### Dashboard

- No host API beyond `workspace.info` and `storage.*`.
- Discovers workspace CSV/TSV/JSON files, with three labeled sample datasets.
- Shapes rows by x column, measure, aggregation, chart type, top-N, and compare
  toggle.
- Shows stats, chart, donut, gauge, progress, heatmap, and tables together.
- Reloads the active file when Live is on and its mtime changes.
- Persists the last view in plugin storage.

### Workspace Analysis

- Uses `workspace.info` and direct filesystem/subprocess access.
- Dynamically detects languages, manifests, dependency counts, lockfiles,
  tests, CI, docs, TODO markers, large files, git state, and toolchains.
- Reports evidence-based checks rather than an arbitrary health score.
- Bounded scan: 50k files, 64 MiB content, 512 KiB per file.
- Rescan and optional five-second live scanning.

### Debugging Station

- Read-only. Never kills processes, changes sockets, or executes builds.
- Left vertical pages: **Build**, **Processes**, **Network**, **System**.
- Detects Node (npm, pnpm, yarn, bun), Cargo, Go modules, Python
  (pip, uv, poetry), Make, Task, Just, Maven, Gradle, CMake, Composer,
  Bundler, Deno, .NET, and Docker manifests up to four levels down.
- Reads package scripts, Make targets, Task tasks, Just recipes, Cargo bins,
  Python scripts, Composer and Deno tasks.
- Infers install/build/test/lint/format commands and checks tool availability
  and versions on `PATH`.
- Samples `ps`, load, memory, `df`, and `lsof`/`netstat`.
- Gracefully degrades when a command or permission is unavailable.
- Processes can be filtered and sorted by CPU, memory, name, or PID.
- Network tab lists listeners, established connections, and socket states.
- No charts, donuts, or gauges; the station is tables and live evidence.

### Plugin Authoring

Platform work completed:

- `host.describe` returns the protocol, methods, limits, feature flags, and
  plugin directory.
- `llm.providers` lists installed text-capable providers and cached models.
- `llm.complete` runs `provider::run_text` asynchronously on a worker thread.
- `CodeEditor` is a native multiline, syntax-highlighted visual node.
- Bundled docs are copied into the package by the build.

Authoring station features:

- Wide layout with a left vertical rail: Guide, API, Scaffold, Generate, Editor.
- Markdown docs render as native headings, prose, lists, tables, and code
  blocks, not as one long code field.
- Live API/limits/features/provider tables from `host.describe` and
  `llm.providers`.
- Four-step scaffold wizard: identity → destination → review → created.
- Scaffolds Node, Python, Rust, or Go plugins with manifest, source, README, and
  build files. Review shows every path before writing.
- Loads and saves project files in the code editor.
- Two-column Generate page: provider/system/prompt on the left, response on the
  right. Sends one async `llm.complete` request and can copy the response into
  the editor.

## Resolved decisions

- Dashboard supports a real workspace file and includes clearly labeled sample
  datasets as a fallback.
- Authoring v1 is docs + scaffold + LLM + code editing. Live preview/reload is
  deferred; restart after manifest/protocol changes.
- No session transcript access. `llm.complete` takes a direct prompt and
  optional system prompt only. Plugins do not receive `sessions.*` or the
  Operator app token.

## Deferred

- `plugins.status` to map OS processes to plugin ids and last errors.
- `plugins.reload` / `plugins.load` for live plugin lifecycle.
- Streaming `llm.complete` or model/account pickers beyond cached catalogs.
- Read-only `sessions.list` / `sessions.read` if transcript context is needed.
