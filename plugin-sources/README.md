# Plugin sources

Source code and manifests for the example plugins live here. `plugins/` is the
runtime directory assembled by the plugin build:

```bash
npm run plugins
```

That command builds the plugins and then launches the native dev app with
`MONOCODE_PLUGIN_DIR` pointing at the built `plugins/` directory. Use
`npm run plugins:build` if you only want to build without launching.

The build looks at each directory below `plugin-sources/` and produces a
self-contained package in `plugins/<name>/`:

| Source | Kind | Runtime output |
|---|---|---|
| `hello/index.js` | Node.js | copies `index.js` |
| `dashboard/Cargo.toml` | Rust | builds `target/release/dashboard`, copies it to `bin/dashboard` |
| `tasks/go.mod` | Go | builds `bin/tasks` |
| `focus/main.py` | Python | syntax-checks and copies `main.py` |

Each source directory owns its `plugin.json`. The build copies that manifest to
`plugins/<name>/plugin.json` and writes the correct platform-specific command
for the compiled output.

Building the examples expects `node`, `cargo`, `go`, and Python 3 (`python3` or
`python` on PATH).

The host never builds or installs plugin code at app startup. It only runs what
the manifest names, so run `npm run plugins` after changing a plugin.
