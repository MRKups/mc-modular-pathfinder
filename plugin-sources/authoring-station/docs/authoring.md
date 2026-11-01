# Authoring workflow

Plugin Authoring has two pages:

1. **New Plugin:** Enter the plugin identity (`Plugin name`, `Identifier`,
   `Language`) and three authoring brief fields:
   - What should the plugin do?
   - How will you know it works as intended?
   - Anything you want to avoid or not do?
2. **Plugins:** Inspect detected plugins across `plugin-sources/` and the
   active runtime `plugins/` directory, check build status, and open an
   authoring session for any workspace plugin.

## Create & Start Session

Clicking **Create & Start Session** writes a starter package under
`plugin-sources/<slug>/`:

- `plugin.json` with a valid id, command, and surface definition.
- A runnable `main.py` (Python), `Cargo.toml` + `src/main.rs` (Rust), or
  `package.json` + `index.js` (Node.js).
- `README.md` recording the brief and build commands.

It then calls `session.start` to open a new Monocode chat session with your
default provider. The opening prompt explains that a basic scaffold has been
generated, sets the assistant's objective to build the plugin collaboratively
under your guidance, injects all three brief answers, summarizes protocol v1,
and links to the official Monocode GitHub documentation.
