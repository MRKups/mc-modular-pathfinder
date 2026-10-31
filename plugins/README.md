# Monocode plugins

External plugins are separate processes that speak newline-delimited JSON-RPC
over stdin/stdout. Monocode renders their declarative UI natively through the
existing visual component tree. No plugin code runs inside the app.

## Try the example

```bash
MONOCODE_PLUGIN_DIR="$PWD/plugins" npm run native:dev
```

The sidebar gains a **Plugins → Hello** item. Click it to open a native surface
backed by `plugins/hello/index.js`. The button counter and name field round-trip
through the external Node process.

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
6. Exit on `shutdown`.

See [PROTOCOL.md](PROTOCOL.md) for the complete contract, and
[hello/index.js](hello/index.js) for a working implementation.

## Caveats

- Plugins are trusted programs, not a security sandbox. They run with your
  permissions.
- Phase 1 renders sidebar placements and a full-content surface. Tab placement
  is accepted in manifests but not shown yet.
- Automatic restart after a plugin crash is deferred; the surface shows an
  offline/error state instead.
- WASM is deferred. The same manifest and UI contract can back a WASM runtime
  later.
