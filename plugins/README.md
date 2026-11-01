# Monocode plugins

External plugins are separate processes that speak newline-delimited JSON-RPC
over stdin/stdout. Monocode renders their declarative UI natively through the
existing visual component tree. No plugin code runs inside the app.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how the runtime, adapter, and shell
integration fit together, and [HOST_API.md](HOST_API.md) for the methods a
plugin can call.

## Try the examples

Build the plugin packages and launch Monocode with one command:

```bash
npm run plugins
```

It compiles the Rust and Go plugins, copies the Node and Python plugins, writes
self-contained packages under `plugins/`, and launches the native dev app with
`MONOCODE_PLUGIN_DIR` pointing at them. For a build-only run, use
`npm run plugins:build`.

Building the examples expects `node`, `cargo`, `go`, and Python 3 (`python3` or
`python` on PATH).

The sidebar gains four plugin rows alongside Inbox, Notes and Automations,
each marked with a small plug icon:

| Plugin | Language | What it does |
|---|---|---|
| **Notebook** | Python | Runs a persistent Python kernel with notebook cells, captured output, variables, and workspace data files |
| **Workspace Analysis** | Rust | Reviews the current repository: languages, dependency counts, files, TODO markers, and toolchains |
| **Debugger** | Go | Targets project processes and inspects their identity, state, command line, and open ports |
| **Plugin Authoring** | Node.js | Vertical docs/API/scaffold/generate/editor pages, a four-step scaffold wizard, and `llm.complete` |

Each one is an independent process built from `plugin-sources/<name>/`. Open
them from the sidebar and interact with the controls; all state lives in the
plugin process and every update crosses stdio. Every multi-page example uses
the native `page_tabs` table-of-contents rail and an 800 px content column.

Plugin rows are ordered by the optional `order` field on each surface, then
alphabetically by title. The examples set `order` to Notebook, Workspace
Analysis, Debugger, Plugin Authoring.

The authoring station can ask a configured provider for text through
`llm.complete`. The host runs a separate, text-only, read-only session on a
worker thread; plugins never receive provider credentials or the Operator app
token. The app's Background model setting selects the default provider/model.

The app data directory also enables plugins without an environment variable:

```text
<app-data-dir>/plugins/<your-plugin>/plugin.json
```

With neither location populated, the app is unchanged.

## Write a plugin

1. Create `my-plugin/plugin.json` with an id, name, protocol `1`, an argv
   `command`, and one or more `ui` surfaces.
2. Make `command` an executable program that reads JSON lines from stdin and
   writes JSON lines to stdout.
3. On `initialize`, respond with the protocol version and (optionally) surfaces.
4. On `ui.open`, send a `ui.set` containing a native UI tree.
5. On `ui.event`, update your state and send another `ui.set`.
6. If you need app data, send a request such as `workspace.info`,
   `storage.get`, `host.describe`, or `llm.complete`; see
   [HOST_API.md](HOST_API.md).
7. Exit on `shutdown`.

See [PROTOCOL.md](PROTOCOL.md) for the complete contract, and
[debugging-station/main.go](../plugin-sources/debugging-station/main.go) or
[authoring-station/index.js](../plugin-sources/authoring-station/index.js) for
working implementations. The authoring plugin also bundles a doc browser and
Node.js, Python, Rust, and Go scaffold templates.

## Caveats

- Plugins are trusted programs, not a security sandbox. They run with your
  permissions.
- Every plugin surface is a sidebar page. Use `page_tabs` for a left
  table-of-contents rail and a centered 800 px content column. There are no
  plugin title tabs.
- A crashed plugin stays offline until the app restarts; its surface shows an
  offline/error state.
