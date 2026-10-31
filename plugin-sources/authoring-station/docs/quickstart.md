# Plugin quickstart

A Monocode plugin is a separate executable that speaks JSON-RPC 2.0 over
newline-delimited stdin/stdout. The app starts it, renders its declarative UI
natively, and forwards control events. Plugins are not sandboxed.

## Smallest useful plugin

```text
my-plugin/
  plugin.json
  package.json
  index.js
```

`package.json`:

```json
{"private": true, "type": "module"}
```

`plugin.json`:

```json
{
  "id": "com.example.my-plugin",
  "name": "My Plugin",
  "protocol": 1,
  "command": ["node", "index.js"],
  "cwd": ".",
  "ui": [
    {"id": "main", "title": "My Plugin", "icon": "sparkles", "order": 10}
  ]
}
```

`index.js`:

```js
import readline from "node:readline";
const SURFACE = "main";

function send(message) {
  process.stdout.write(JSON.stringify(message) + "\n");
}

function tree() {
  return {
    kind: "column",
    gap: 12,
    children: [
      { kind: "text", text: "Hello from my plugin", size: "title", color: "ink" },
      { kind: "button", label: "Click me", handler: "click", disabled: false }
    ]
  };
}

function publish() {
  send({ jsonrpc: "2.0", method: "ui.set", params: { surface: SURFACE, tree: tree() } });
}

readline.createInterface({ input: process.stdin }).on("line", (line) => {
  if (!line.trim()) return;
  const message = JSON.parse(line);
  if (message.method === "initialize") {
    send({
      jsonrpc: "2.0", id: message.id,
      result: { protocol: 1, ui: [{ id: SURFACE, title: "My Plugin", icon: "sparkles" }] }
    });
    publish();
    send({
      jsonrpc: "2.0", id: 2, method: "workspace.info", params: {}
    });
  } else if (message.method === "ui.open") {
    publish();
  } else if (message.method === "ui.event") {
    publish();
  } else if (message.method === "shutdown") {
    process.exit(0);
  }
});
```

## Lifecycle

1. Monocode reads `plugin.json`, validates it, and starts `command`.
2. The host sends `initialize` and waits up to five seconds.
3. The plugin replies with `protocol: 1` and optional `ui` surfaces.
4. The host sends `ui.open`; the plugin sends a `ui.set` tree.
5. Native controls with a `handler` send `ui.event` messages.
6. The plugin can send a new `ui.set` at any time.
7. On quit the host sends `shutdown`, waits two seconds, then kills the process.

## Rules

- One compact JSON object per line. stdout is protocol traffic only.
- Logs and diagnostics go to stderr.
- Maximum line length is 2 MiB; maximum tree is 1 MiB and 512 nodes.
- A plugin may be offline, crash, or be killed; handle failures normally.
- Use `ui.set` to replace a surface tree whenever state changes.
- The process owns its own state. The host owns rendering and input.

## Build and run

From the Monocode repository:

```sh
npm run plugins:build   # build every source in plugin-sources/
npm run plugins         # build, set MONOCODE_PLUGIN_DIR, and launch
```

A generated standalone plugin can be placed in any directory and launched with:

```sh
MONOCODE_PLUGIN_DIR=/path/to/plugins npm run plugins
```

Restart the app after changing plugin source or manifest. Live reload is not
part of plugin protocol v1.
