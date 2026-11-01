#!/usr/bin/env node
// Build the example plugins from plugin-sources/ into self-contained packages
// under plugins/. Node and Python sources are copied; Rust sources are
// compiled.
//
// Run `npm run plugins` to build and launch, or `npm run plugins:build` to
// build only.

import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const sources = join(root, "plugin-sources");
const output = join(root, "plugins");
const executable = process.platform === "win32" ? ".exe" : "";
let pythonCommand;

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: "inherit",
    ...options,
  });
  if (result.error) {
    console.error(`[plugins] ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function copyBinary(from, to) {
  mkdirSync(dirname(to), { recursive: true });
  copyFileSync(from, to);
  chmodSync(to, 0o755);
}

function findPython() {
  if (pythonCommand !== undefined) {
    return pythonCommand;
  }
  for (const candidate of ["python3", "python"]) {
    const result = spawnSync(
      candidate,
      ["-c", "import sys; raise SystemExit(0 if sys.version_info[0] == 3 else 1)"],
      { stdio: "ignore" },
    );
    if (!result.error && result.status === 0) {
      pythonCommand = candidate;
      return pythonCommand;
    }
  }
  pythonCommand = null;
  return null;
}

function buildPlugin(name, source, pluginDir) {
  const manifestPath = join(source, "plugin.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  let kind;

  if (existsSync(join(source, "Cargo.toml"))) {
    kind = "rust";
    const args = ["build", "--release"];
    if (existsSync(join(source, "Cargo.lock"))) {
      args.push("--locked");
    }
    args.push("--manifest-path", join(source, "Cargo.toml"));
    run("cargo", args);
    const binary = join(source, "target", "release", `${name}${executable}`);
    if (!existsSync(binary)) {
      throw new Error(`Rust build did not produce ${binary}`);
    }
    copyBinary(binary, join(pluginDir, "bin", `${name}${executable}`));
    manifest.command = [`./bin/${name}${executable}`];
  } else if (existsSync(join(source, "main.py"))) {
    kind = "python";
    const python = findPython();
    if (!python) {
      throw new Error(`Cannot build ${name}: Python 3 is required for main.py`);
    }
    run(python, [
      "-c",
      "import ast, pathlib, sys; ast.parse(pathlib.Path(sys.argv[1]).read_text(encoding='utf-8'), filename=sys.argv[1])",
      join(source, "main.py"),
    ]);
    copyFileSync(join(source, "main.py"), join(pluginDir, "main.py"));
    manifest.command = [python, "main.py"];
  } else if (existsSync(join(source, "index.js"))) {
    kind = "node";
    copyFileSync(join(source, "index.js"), join(pluginDir, "index.js"));
    manifest.command = ["node", "index.js"];
  } else {
    throw new Error(
      `Cannot determine how to build ${name}: add Cargo.toml, main.py, or index.js`,
    );
  }

  writeFileSync(
    join(pluginDir, "plugin.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  for (const extra of ["docs", "README.md", "package.json"]) {
    const from = join(source, extra);
    if (!existsSync(from)) {
      continue;
    }
    const to = join(pluginDir, extra);
    cpSync(from, to, { recursive: true });
  }
  console.log(`[plugins] built ${name} (${kind}) -> plugins/${name}`);
}

function main() {
  mkdirSync(output, { recursive: true });
  const entries = readdirSync(sources, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .sort((a, b) => a.name.localeCompare(b.name));
  const expected = new Set(
    entries
      .filter((entry) => existsSync(join(sources, entry.name, "plugin.json")))
      .map((entry) => entry.name),
  );
  for (const entry of readdirSync(output, { withFileTypes: true })) {
    if (entry.isDirectory() && !expected.has(entry.name)) {
      rmSync(join(output, entry.name), { recursive: true, force: true });
    }
  }

  let built = 0;
  for (const entry of entries) {
    const source = join(sources, entry.name);
    if (!existsSync(join(source, "plugin.json"))) {
      continue;
    }
    const pluginDir = join(output, entry.name);
    rmSync(pluginDir, { recursive: true, force: true });
    mkdirSync(pluginDir, { recursive: true });
    try {
      buildPlugin(entry.name, source, pluginDir);
      built += 1;
    } catch (error) {
      rmSync(pluginDir, { recursive: true, force: true });
      console.error(`[plugins] ${error.message}`);
      process.exit(1);
    }
  }

  if (built === 0) {
    console.error("[plugins] no plugin source directories found");
    process.exit(1);
  }
  console.log(`[plugins] ${built} plugin(s) ready in plugins/`);
}

main();
