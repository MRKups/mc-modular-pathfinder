# Plugin sources

Source code and manifests for the four example plugins live here. `plugins/` is
the runtime directory assembled by the plugin build:

```bash
npm run plugins
```

That command builds the plugins and then launches the native dev app with
`MONOCODE_PLUGIN_DIR` pointing at the built `plugins/` directory. Use
`npm run plugins:build` if you only want to build without launching.

| Source | Language | Purpose |
|---|---|---|
| `dashboard/main.py` | Python | Data workbench for CSV/TSV/JSON files in the workspace |
| `workspace-analysis/Cargo.toml` | Rust | Dynamic repository analysis and health checks |
| `debugging-station/go.mod` | Go | Live process, memory, disk, and network inspection |
| `authoring-station/index.js` | Node.js | Docs, scaffolding, source editing, and model assistance |

Each source directory owns its `plugin.json`. The build copies that manifest to
`plugins/<name>/plugin.json` and writes the correct platform-specific command
for the compiled output. Node sources also copy `package.json`, and any
`docs/` or `README.md` beside the source, so generated packages stay
self-contained.

Building the examples expects `node`, `cargo`, `go`, and Python 3 (`python3` or
`python` on PATH). Rust builds use `--locked` only when the source has a
`Cargo.lock`; generated scaffolds work without one.

The host never builds or installs plugin code at app startup. It only runs what
the manifest names, so run `npm run plugins` after changing a plugin. Protocol
v1 has no live plugin reload; restart after manifest or protocol changes.
