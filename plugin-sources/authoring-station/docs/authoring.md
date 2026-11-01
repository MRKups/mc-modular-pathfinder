# Authoring workflow

This station is a working example of a plugin that helps build plugins. It
shows documentation, scaffolds a new plugin, edits source, and asks the
configured background model for help.

## Scaffold

The scaffold wizard has four steps: identity, destination, review, and created.
Only the review step writes anything. It produces a complete starting package:

- `plugin.json` with a valid id, command, and one surface.
- A runnable Node.js program.
- A `README.md` with build and restart instructions.
- A `package.json` for the module layout.

The target directory is relative to the active project. The default is
`plugin-sources/<name>`, which is the layout this repository uses. Do not
overwrite an existing non-empty directory; choose a new name.

After creating a plugin:

1. Run `npm run plugins:build`.
2. Launch with `npm run plugins`.
3. Open the new surface from the sidebar.

Protocol v1 has no plugin reload. Restart Monocode after changing manifest,
command, or protocol code. A plugin's `ui.set` can change while it runs.

## Editor

The Editor tab loads a project file by path, shows it in a syntax-highlighted
code editor, and writes it back. Paths must stay inside the active project.
Editing is intentionally plain: save, then build/restart to test. All pages use
a table-of-contents rail on the left and a single 800 px content column.

## Model assistance

The Generate tab calls `llm.complete`, which is text-only and read-only:

- The app runs the configured provider, not the plugin.
- Credentials stay inside Monocode.
- The model cannot use tools, request approvals, or write files.
- The result is capped at 64 KiB.

Good prompts include the target language, the surface you want, the host
methods you need, and the event handlers. Ask for a complete `plugin.json` and
one source file, then paste or scaffold it.

The model is a starting point. Verify the manifest, handler names, and JSON
shapes against the API tab and the quickstart.
