# Trust and security

Monocode plugins are trusted local executables. Protocol v1 is not a sandbox:
the process runs with your user permissions, can read files, and can use the
network. Declarative UI trees are safe data, but the plugin binary itself must
be trusted.

## Boundaries

- The host owns the window, input, native controls, and visual validation.
- A plugin owns its process, state, storage, and event handling.
- `storage.*` is scoped to the plugin id and capped at 1 MiB.
- `workspace.info` is read-only metadata.
- `llm.complete` runs a separate, tool-free, read-only text session and returns
  only text. It does not expose the Operator app token or provider credentials.
- Plugins cannot call `sessions.*`, notes, worktrees, or mutating app actions.

## Failure behavior

- A crashed plugin stays offline and keeps its last tree visible.
- Invalid or oversized messages are reported and ignored.
- Handshake timeout is five seconds.
- Shutdown waits two seconds before killing the child.
- Plugin stderr is captured under `<app-data-dir>/plugin-logs/`.

## Review checklist

Before running a third-party plugin, read its manifest and source. Check:

- `command` and `cwd` point at expected files inside the package.
- The process does not write outside paths you expect.
- Any network access is intentional.
- `storage` keys are not used for secrets.
- Generated code is reviewed before it is built and run.
