# Monocode Plugin Protocol v1

A plugin is an executable in its own directory. Monocode starts it as a child
process and exchanges one JSON object per line over stdin/stdout. The plugin
owns its logic and state; Monocode owns the window and renders the plugin's
declarative UI natively.

## Directory layout

```text
plugins/hello/
  plugin.json     required manifest
  index.js        the plugin program (any executable works)
```

This repository keeps development sources in `plugin-sources/`; the
`npm run plugins` command assembles the runtime layout above under `plugins/`.

## Discovery

The host loads plugins from `MONOCODE_PLUGIN_DIR` when set, otherwise from
`<app-data-dir>/plugins` when that directory exists. It scans:

- `plugin.json` in the directory itself, or
- `plugin.json` in each immediate child directory.

Malformed manifests and duplicate plugin ids are reported to the app and do
not prevent other plugins from starting. With no plugin directory configured,
no plugin code runs and the app behaves exactly as before.

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

| Field | Rules |
|---|---|
| `id` | Required. 1–96 ASCII letters, digits, `.`, `_`, `-`. Unique across plugins. |
| `name` | Required. 1–80 characters. Shown in the plugin surface. |
| `protocol` | Required. Must be `1`; other versions are rejected. |
| `command` | Required non-empty argv array, not a shell string. |
| `cwd` | Optional relative directory inside the plugin. Absolute paths and `..` are rejected. |
| `ui` | Optional surfaces. At most one surface per `id`; each surface id is unique within the plugin. |

A surface:

| Field | Rules |
|---|---|
| `id` | Required. 1–96 ASCII letters, digits, `.`, `_`, `-`. |
| `title` | Required. 1–80 characters. |
| `icon` | Optional. Defaults to `sparkles`. |
| `order` | Optional integer. Lower numbers appear first; unordered surfaces follow alphabetically. |

Every surface is a sidebar page. Unknown manifest fields are rejected so typos
are visible.

## Transport

- JSON-RPC 2.0.
- One compact UTF-8 JSON object per line. No embedded newlines, no pretty
  printing.
- Requests carry integer ids; notifications omit `id`.
- The host writes to the plugin's stdin and reads its stdout.
- Plugin stderr is captured as log events and appended to
  `<app-data-dir>/plugin-logs/<plugin-id>.log` when a data directory exists.
- Maximum line length is **2 MiB**.
- Maximum serialized UI tree is **1 MiB**; the native tree validator additionally
  limits it to **512 nodes**.
- A `code_editor` value is capped at 64 KiB; unknown languages render plain.

## Handshake

The host sends:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
  "protocol":1,
  "appVersion":"0.16.0",
  "dataDir":"/path/to/data",
  "logDir":"/path/to/data/plugin-logs"
}}
```

The plugin must answer within **5 seconds**:

```json
{"jsonrpc":"2.0","id":1,"result":{
  "protocol":1,
  "ui":[{"id":"hello","title":"Hello","icon":"sparkles"}]
}}
```

`protocol` and `ui` are optional in the response. If `ui` is present it replaces
the manifest's static contributions. After a successful response the host emits
a `Ready` event and the plugin may publish trees. A plugin that exits, errors, or
times out before the response is marked offline and killed.

## Host → plugin methods

All are notifications except `initialize`.

### `ui.open`

```json
{"jsonrpc":"2.0","method":"ui.open","params":{"surface":"hello"}}
```

Sent when the user opens the surface. Reply with a `ui.set` for that surface.
The plugin should also tolerate being opened multiple times.

### `ui.close`

```json
{"jsonrpc":"2.0","method":"ui.close","params":{"surface":"hello"}}
```

The surface is no longer visible. The plugin process stays alive and may keep
its state.

### `ui.event`

```json
{"jsonrpc":"2.0","method":"ui.event","params":{
  "surface":"hello",
  "handler":"click",
  "value":null
}}
```

Sent when a native control fires. `value` is one of:

| Control | Value |
|---|---|
| button | `null` |
| toggle | boolean |
| input | string |
| code_editor | full text |
| select | selected index (number) |
| slider | number |
| tabs | selected index (number) |

The handler string must belong to the currently rendered tree. A plugin may
reply with a `ui.set` to publish a new tree; if it does not, the current tree
stays on screen.

### `shutdown`

```json
{"jsonrpc":"2.0","method":"shutdown","params":{}}
```

Sent on app quit. The plugin should exit promptly. The host waits **2 seconds**,
then kills the process.

## Plugin → host methods

### `ui.set`

```json
{"jsonrpc":"2.0","method":"ui.set","params":{
  "surface":"hello",
  "tree":{"kind":"column","gap":12,"children":[]}
}}
```

Replaces the rendered tree for a manifest-declared surface. Unknown surfaces are
rejected with an error event. Invalid trees are rejected and the previous tree
stays visible.

### `ui.close`

```json
{"jsonrpc":"2.0","method":"ui.close","params":{"surface":"hello"}}
```

Removes the surface's cached tree. The plugin process stays running.

### `log`

```json
{"jsonrpc":"2.0","method":"log","params":{"level":"info","message":"ready"}}
```

`level` defaults to `info`. Messages are truncated to 8 KiB.

### Host requests

A message with an `id` and a method name is a request to the host. The host
replies with a normal JSON-RPC result or error using the same id. Unknown
methods return an error.

```json
{"jsonrpc":"2.0","id":7,"method":"workspace.info","params":{}}
```

```json
{"jsonrpc":"2.0","id":7,"result":{"root":"/Users/me/project","name":"project"}}
```

Current methods:

- `workspace.info`
- `storage.get`
- `storage.set`
- `host.describe`
- `llm.providers`
- `llm.complete`

See [HOST_API.md](HOST_API.md) for params, results, storage and model rules, and
errors. The surface is additive; there is no separate API version. Responses to
unknown ids are reported as protocol errors.

## UI trees

`tree` is a `monocode_core::visual::Node` serialized as JSON. It is bounded,
validated data — never HTML, CSS, JavaScript, or host commands.

Common nodes:

```json
{"kind":"column","gap":12,"children":[]}
{"kind":"row","gap":8,"children":[]}
{"kind":"grid","columns":2,"gap":8,"children":[]}
{"kind":"card","gap":8,"children":[]}
{"kind":"text","text":"Hello","size":"body","color":"ink"}
{"kind":"stat","label":"Clicks","value":"3","color":"accent"}
{"kind":"table","columns":["A","B"],"rows":[["1","2"]]}
{"kind":"button","label":"Click","handler":"click","disabled":false}
{"kind":"toggle","label":"Enabled","value":true,"handler":"toggle"}
{"kind":"input","label":"Name","value":"","placeholder":"Type","handler":"name"}
{"kind":"code_editor","label":"Source","value":"console.log(1)","language":"js","height":360,"handler":"source","read_only":false}
{"kind":"select","label":"Choice","options":["A","B"],"selected":0,"handler":"choice"}
{"kind":"slider","label":"Amount","value":50,"min":0,"max":100,"step":1,"handler":"amount"}
{"kind":"tabs","labels":["One","Two"],"selected":0,"handler":"tab"}
```

Also available: `chart`, `gauge`, `donut`, `heatmap`, `progress`, `canvas`, and
`scene`. Text sizes are `caption`, `small`, `body`, `subtitle`, `title`, `hero`.
Colors are `ink`, `muted`, `faint`, `accent`, `positive`, `negative`, `warning`,
and `series1`…`series6`.

Handlers are plugin-defined strings. They are returned unchanged in `ui.event`,
and the same name may be reused on multiple controls. Internally Monocode maps
them to the numeric callback ids its native visual schema requires; plugins
never see those ids. The visual component model's size and node limits apply.

## Event flow

```text
plugin process                         Monocode
     │ ── initialize ───────────────────►│
     │ ◄────────────────── manifest ─────│
     │ ── ui.set { surface, tree } ─────►│  render native tree
     │                                   │
     │ ◄──────── ui.event { handler } ───│  click / input
     │ ── ui.set { new tree } ───────────►│  update
```

## Failure behavior

- Handshake timeout, invalid response, or early exit → plugin marked offline.
- A crashed plugin stays offline and keeps its last tree visible with an error
  banner until the app restarts.
- Invalid or oversized messages are reported and ignored; an oversized or
  unparseable stream can end the plugin.
- On app quit the host asks every plugin to exit, waits two seconds, then kills
  it. Orphaned child processes are treated as a bug.

## Security

A plugin process is **not sandboxed** by this protocol. It runs with the user's
permissions, can read files, and can use the network. The declarative UI tree is
safe data, but the plugin executable must be trusted.

## Debugging

- Run the plugin by hand and type protocol lines into its stdin.
- Read `<app-data-dir>/plugin-logs/<plugin-id>.log` for stderr.
- Set `MONOCODE_PLUGIN_DIR` to any directory containing `plugin.json` or one
  subdirectory per plugin.
