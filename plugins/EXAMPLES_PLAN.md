# Example plugin roadmap

Status: implemented. The four capability demos were replaced with four plugins
that each have a real job.

## Target examples

| Plugin | Language | Purpose | Showcases |
|---|---|---|---|
| **Notebook** | Python | Python notebook for data work in the active workspace | Code cells, persistent Python kernel, captured output, variables, data files |
| **Workspace Analysis** | Rust | Review the current repository | Language breakdown, dependency resolution, dense file tables, TODO markers |
| **Debugger** | Go | Inspect processes and ports like an IDE debugger | Target selection, process inspector, ports, build configurations |
| **Plugin Authoring** | Node.js | Design, scaffold, document, and generate plugins | Vertical docs/API/scaffold/generate/editor pages, step wizard, source editor, LLM generation |

## Language decision

- **Python** — Notebook. Persistent Python kernel, code cells, and data files.
- **Rust** — Workspace Analysis. Fast, bounded repository walking.
- **Go** — Debugger. Process and port inspection without dependencies.
- **Node.js** — Plugin Authoring. Templates, docs, child-process-free file IO,
  and LLM JSON calls.

## Implementation status

### Notebook

- Python kernel runs in a child `python3` process with a persistent namespace.
- Vertical pages: **Notebook**, **Data**, **Variables**, **Kernel**.
- Code cells show execution count, source, and captured text output.
- Run one cell, run all cells, add/delete cells, and clear outputs.
- Data page scans CSV/TSV/JSON files and inserts a ready-to-run loader cell.
- Variables page lists the current kernel namespace.
- Kernel page shows Python version, executable, working directory, and status.

### Workspace Analysis

- Uses `workspace.info` and direct filesystem/subprocess access.
- Reviews the current repository: languages, manifests, dependencies, files,
  TODO markers, checks, git state, and toolchains.
- Language breakdown uses a language icon beside each name.
- Resolves dependency counts by running read-only toolchain commands
  (for example `npm ls`, `cargo metadata`, `go list -m all`), with declared
  counts as fallback.
- Dense, width-aware tables with no horizontal scrolling.
- One Rescan button and the time of the last scan.
- Bounded scan: 50k files, 64 MiB content, 512 KiB per file.

### Debugger

- Read-only. Never attaches, kills processes, or changes sockets.
- Vertical pages: **Debug**, **Processes**, **Ports**, **Build**.
- Debug page lists processes running from the active project and inspects the
  selected target: parent, state, CPU, memory, command, arguments, and ports.
- Processes page is a filterable process table.
- Ports page lists socket states, listeners, and connections.
- Build page detects build systems, scripts/targets, and toolchains.
- No activity dashboard, charts, gauges, memory/disk overview, or live ticking.

### Plugin Authoring

Platform work completed:

- `host.describe` returns the protocol, methods, limits, feature flags, and
  plugin directory.
- `llm.providers` lists installed text-capable providers and cached models.
- `llm.complete` runs `provider::run_text` asynchronously on a worker thread.
- `CodeEditor` is a native multiline, syntax-highlighted visual node.
- Bundled docs are copied into the package by the build.

Authoring station features:

- Documentation-style layout: a left table-of-contents rail with Guide, API,
  Scaffold, Generate, and Editor, plus a single 800 px content column.
- Markdown docs render as native headings, prose, lists, tables, and code
  blocks, not as one long code field.
- Live API/limits/features/provider tables from `host.describe` and
  `llm.providers`.
- Four-step scaffold wizard: identity → destination → review → created.
- Scaffolds a Node.js plugin with manifest, source, README, and package file.
  Review shows every path before writing.
- Loads and saves project files in the code editor.
- Single-column Generate page: provider, system prompt, prompt, and response.
  Sends one async `llm.complete` request and can copy the response into the
  editor.

## Resolved decisions

- Notebook uses a real child Python kernel and workspace data files. Notebook
  state is in memory; the document itself is not persisted yet.
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
