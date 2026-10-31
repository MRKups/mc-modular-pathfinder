#!/usr/bin/env node
// Build the example plugins, then launch the native dev app with the built
// plugin packages already on its plugin path.

import { spawn } from "node:child_process";
import { once } from "node:events";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const plugins = join(root, "plugins");

const build = spawn(process.execPath, [join(root, "scripts/build-plugins.mjs")], {
  cwd: root,
  env: process.env,
  stdio: "inherit",
});
const [buildCode, buildSignal] = await once(build, "exit");
if (buildSignal) {
  process.kill(process.pid, buildSignal);
}
if (buildCode !== 0) {
  process.exit(buildCode ?? 1);
}

const app = spawn(
  process.execPath,
  [join(root, "scripts/dev-native.mjs"), ...process.argv.slice(2)],
  {
    cwd: root,
    env: { ...process.env, MONOCODE_PLUGIN_DIR: plugins },
    stdio: "inherit",
  },
);

const stop = (signal) => {
  if (app.exitCode === null && app.signalCode === null) {
    app.kill(signal);
  }
};
process.on("SIGINT", () => stop("SIGINT"));
process.on("SIGTERM", () => stop("SIGTERM"));
app.on("error", (error) => {
  console.error(`[plugins] ${error.message}`);
  process.exit(1);
});
app.on("exit", (code, signal) => {
  process.exit(signal ? 0 : (code ?? 0));
});
