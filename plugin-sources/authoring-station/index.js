#!/usr/bin/env node
// Plugin Authoring: scaffold a Python, Rust, or Node.js plugin from a
// structured spec and launch a guided Monocode session, or inspect detected
// plugins in the workspace.
//
// stdout is protocol traffic only; diagnostics go to stderr.

import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";

const SURFACE = "authoring-station";
const REPO_URL = "https://github.com/MRKups/monocode";
const REPO_BRANCH = "kups/mc-plugins";
const DOCS_BASE_URL = `${REPO_URL}/blob/${REPO_BRANCH}`;
const TREE_BASE_URL = `${REPO_URL}/tree/${REPO_BRANCH}`;
const TABS = ["New Plugin", "Plugins"];
const LANGUAGES = ["Python", "Rust", "Node.js"];

const state = {
  root: "",
  pluginDir: "",
  tab: 0,
  starting: false,
  status: "",
  form: {
    name: "",
    id: "",
    language: 0,
    purpose: "",
    successCriteria: "",
    constraints: "",
  },
  plugins: [],
  selectedPlugin: 0,
  nextId: 10,
  pending: new Map(),
};

function send(message) {
  process.stdout.write(JSON.stringify(message) + "\n");
}

function respond(id, result) {
  send({ jsonrpc: "2.0", id, result });
}

function notify(method, params) {
  send({ jsonrpc: "2.0", method, params });
}

function request(method, params, callback) {
  const id = state.nextId++;
  state.pending.set(id, callback);
  send({ jsonrpc: "2.0", id, method, params });
}

function clean(value) {
  return String(value ?? "").replace(/\0/g, "");
}

function clip(value, limit) {
  return clean(value).slice(0, limit);
}

function text(value, size = "body", color = "ink", monospace = false) {
  const node = { kind: "text", text: clip(value, 16000), size, color };
  if (monospace) node.monospace = true;
  return node;
}

function field(label, control) {
  return {
    kind: "column",
    gap: 4,
    children: [text(label, "caption", "muted"), control],
  };
}

function section(title, children) {
  return {
    kind: "column",
    gap: 8,
    children: [text(title, "subtitle", "ink"), ...children],
  };
}

function row(children, gap = 8) {
  return { kind: "row", gap, children };
}

function button(label, handler, disabled = false) {
  return { kind: "button", label, handler, disabled };
}

function inputNode(label, value, placeholder, handler, submitHandler = "") {
  const node = {
    kind: "input",
    label,
    value: clip(value, 4096),
    placeholder,
    handler,
  };
  if (submitHandler) node.submit_handler = submitHandler;
  return node;
}

function selectNode(label, options, selected, handler) {
  return {
    kind: "select",
    label,
    options,
    selected: Math.max(0, Math.min(options.length - 1, Number(selected) || 0)),
    handler,
  };
}

function codeEditor(label, value, language, height, handler, readOnly = false) {
  return {
    kind: "code_editor",
    label,
    value: clip(value, 64 * 1024),
    language,
    height: Math.max(80, Math.min(900, Math.round(height))),
    handler,
    read_only: readOnly,
  };
}

function table(columns, rows, weights = [], selected = null, handler = "") {
  const safeColumns = columns.map((col) => clip(col, 256)).slice(0, 24);
  const safeRows = rows
    .slice(0, 250)
    .map((item) => safeColumns.map((_, index) => clip(item[index] ?? "", 2048)));
  const node = {
    kind: "table",
    columns: safeColumns,
    rows: safeRows,
  };
  if (weights.length === safeColumns.length) node.weights = weights;
  if (handler) node.handler = handler;
  if (
    selected !== null &&
    selected !== undefined &&
    safeRows.length > 0 &&
    selected >= 0 &&
    selected < safeRows.length
  ) {
    node.selected = selected;
  }
  return node;
}

function pageTabs(labels, selected, handler, children) {
  return {
    kind: "page_tabs",
    labels,
    selected,
    handler,
    nav_width: 170,
    children,
  };
}

function withStatus(body) {
  if (!state.status) return body;
  const color = state.status.startsWith("Error") ? "warning" : "positive";
  const banner = text(state.status, "small", color);
  if (body && body.kind === "column") {
    return { ...body, children: [banner, ...body.children] };
  }
  return { kind: "column", gap: 12, children: [banner, body] };
}

function publish() {
  notify("ui.set", { surface: SURFACE, tree: tree() });
}

/* ---------------------------------------------------------------------------
 * Form and path helpers
 * ------------------------------------------------------------------------ */

function slugify(value) {
  const slug = clean(value)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  return slug || "my-plugin";
}

function validPluginId(value) {
  return /^[A-Za-z0-9._-]{1,96}$/.test(value);
}

function formSlug() {
  return slugify(state.form.name || state.form.id || "my-plugin");
}

function formId() {
  return clean(state.form.id).trim() || `com.example.${formSlug()}`;
}

function formTarget() {
  return `plugin-sources/${formSlug()}`;
}

function insideRoot(relativePath) {
  if (!state.root) {
    throw new Error("The active project root is not available yet.");
  }
  const resolved = path.resolve(state.root, relativePath);
  const rel = path.relative(state.root, resolved);
  if (rel.startsWith("..") || path.isAbsolute(rel)) {
    throw new Error("Only paths inside the active project are allowed.");
  }
  return resolved;
}

function workspaceRelativePath(absolutePath) {
  if (!absolutePath || !state.root) return "";
  const rel = path.relative(state.root, absolutePath);
  if (rel.startsWith("..") || path.isAbsolute(rel)) return "";
  return rel.split(path.sep).join("/") || ".";
}

function displayPath(absolutePath) {
  if (!absolutePath) return "none";
  const rel = workspaceRelativePath(absolutePath);
  if (rel) return rel;
  return absolutePath;
}

function targetAlreadyPopulated() {
  if (!state.root) return false;
  try {
    const target = insideRoot(formTarget());
    return fs.existsSync(target) && fs.readdirSync(target).length > 0;
  } catch {
    return false;
  }
}

/* ---------------------------------------------------------------------------
 * Minimal blank scaffolds
 * ------------------------------------------------------------------------ */

function pythonTemplate(title) {
  return `#!/usr/bin/env python3
import json
import sys

SURFACE = "main"


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\\n")
    sys.stdout.flush()


def tree():
    return {
        "kind": "column",
        "gap": 12,
        "children": [],
    }


def publish():
    send({"jsonrpc": "2.0", "method": "ui.set", "params": {"surface": SURFACE, "tree": tree()}})


def handle_event(handler, value):
    pass


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": message.get("id"),
            "result": {
                "protocol": 1,
                "ui": [{"id": SURFACE, "title": ${JSON.stringify(title)}, "icon": "sparkles", "order": 10}],
            },
        })
        publish()
    elif method == "ui.open":
        publish()
    elif method == "ui.event":
        params = message.get("params") or {}
        if params.get("surface") == SURFACE:
            handle_event(str(params.get("handler") or ""), params.get("value"))
    elif method == "shutdown":
        break
`;
}

function rustTemplate(title) {
  return `use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const SURFACE: &str = "main";

fn send(value: Value) {
    let mut out = io::stdout().lock();
    writeln!(out, "{}", serde_json::to_string(&value).unwrap()).unwrap();
    out.flush().unwrap();
}

fn tree() -> Value {
    json!({
        "kind": "column",
        "gap": 12,
        "children": []
    })
}

fn publish() {
    send(json!({"jsonrpc": "2.0", "method": "ui.set", "params": {"surface": SURFACE, "tree": tree()}}));
}

fn handle_event(_handler: &str, _value: &Value) {}

fn main() {
    let stdin = io::stdin();
    for line in stdin.lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => {
                send(json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {
                        "protocol": 1,
                        "ui": [{"id": SURFACE, "title": ${JSON.stringify(title)}, "icon": "sparkles", "order": 10}]
                    }
                }));
                publish();
            }
            "ui.open" => publish(),
            "ui.event" => {
                let params = &message["params"];
                if params["surface"].as_str() == Some(SURFACE) {
                    let handler = params["handler"].as_str().unwrap_or_default();
                    handle_event(handler, &params["value"]);
                }
            }
            "shutdown" => break,
            _ => {}
        }
    }
}
`;
}

function nodeTemplate(title) {
  return `import readline from "node:readline";

const SURFACE = "main";

function send(message) {
  process.stdout.write(JSON.stringify(message) + "\\n");
}

function respond(id, result) {
  send({ jsonrpc: "2.0", id, result });
}

function tree() {
  return {
    kind: "column",
    gap: 12,
    children: [],
  };
}

function publish() {
  send({ jsonrpc: "2.0", method: "ui.set", params: { surface: SURFACE, tree: tree() } });
}

function handleEvent(_handler, _value) {}

readline.createInterface({ input: process.stdin }).on("line", (line) => {
  if (!line.trim()) return;
  const message = JSON.parse(line);
  if (message.method === "initialize") {
    respond(message.id, {
      protocol: 1,
      ui: [{ id: SURFACE, title: ${JSON.stringify(title)}, icon: "sparkles", order: 10 }],
    });
    publish();
  } else if (message.method === "ui.open") {
    publish();
  } else if (message.method === "ui.event") {
    const params = message.params || {};
    if (params.surface === SURFACE) {
      handleEvent(String(params.handler || ""), params.value);
    }
  } else if (message.method === "shutdown") {
    process.exit(0);
  }
});
`;
}

function readmeTemplate(title, spec) {
  const sections = [`# ${title}`];
  if (spec.purpose) {
    sections.push(`## What It Does\n\n${spec.purpose}`);
  } else {
    sections.push("Scaffolded with Monocode Plugin Authoring.");
  }
  if (spec.successCriteria) {
    sections.push(`## Success Criteria\n\n${spec.successCriteria}`);
  }
  if (spec.constraints) {
    sections.push(`## Constraints\n\n${spec.constraints}`);
  }
  sections.push(
    "## Build and Launch\n\n```sh\nnpm run plugins:build\nnpm run plugins\n```\n\nRestart Monocode after changing the manifest or rebuilding the package.",
  );
  return sections.join("\n\n") + "\n";
}

function scaffoldFiles(slug, title, pluginId, spec, languageIndex) {
  const command =
    languageIndex === 0
      ? ["python3", "main.py"]
      : languageIndex === 1
        ? [`./bin/${slug}`]
        : ["node", "index.js"];

  const files = [
    {
      path: "plugin.json",
      role: `manifest (${pluginId})`,
      content:
        JSON.stringify(
          {
            id: pluginId,
            name: title,
            protocol: 1,
            command,
            cwd: ".",
            ui: [{ id: "main", title, icon: "sparkles", order: 10 }],
          },
          null,
          2,
        ) + "\n",
    },
  ];

  if (languageIndex === 0) {
    files.push({
      path: "main.py",
      role: "Python entrypoint",
      content: pythonTemplate(title),
    });
  } else if (languageIndex === 1) {
    files.push({
      path: "Cargo.toml",
      role: "Cargo package",
      content: `[package]\nname = "${slug}"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nserde_json = "1"\n\n[workspace]\n`,
    });
    files.push({
      path: "src/main.rs",
      role: "Rust entrypoint",
      content: rustTemplate(title),
    });
  } else {
    files.push({
      path: "package.json",
      role: "ESM package",
      content: `${JSON.stringify({ private: true, type: "module" }, null, 2)}\n`,
    });
    files.push({
      path: "index.js",
      role: "Node.js entrypoint",
      content: nodeTemplate(title),
    });
  }

  files.push({
    path: "README.md",
    role: "spec and build notes",
    content: readmeTemplate(title, spec),
  });

  return files;
}

/* ---------------------------------------------------------------------------
 * Authoring prompt template
 * ------------------------------------------------------------------------ */

function buildAuthoringPrompt(info) {
  const fileLines = info.files.map((item) => `- ${item}`).join("\n");
  const purpose =
    info.purpose && info.purpose.trim()
      ? info.purpose.trim()
      : "Not provided yet. Ask the user what they want this plugin to do.";
  const successCriteria =
    info.successCriteria && info.successCriteria.trim()
      ? info.successCriteria.trim()
      : "Not provided yet. Confirm with the user how we will verify the plugin works as intended.";
  const constraints =
    info.constraints && info.constraints.trim()
      ? info.constraints.trim()
      : "None specified.";

  const intro = info.created
    ? `A basic Monocode plugin scaffold has been generated in \`${info.dir}\`. Your objective is to work with the user and build this plugin for them, under their guidance.`
    : `We are working on the Monocode plugin in \`${info.dir}\`. Your objective is to work with the user and build or refine this plugin for them, under their guidance.`;

  return [
    intro,
    "",
    "## Plugin Details",
    `- Name: ${info.title}`,
    `- Identifier: ${info.id}`,
    `- Language: ${info.language}`,
    `- Scaffold directory: ${info.dir}`,
    "- Files:",
    fileLines,
    "",
    "## What the Plugin Should Do",
    purpose,
    "",
    "## How We Will Know It Works as Intended",
    successCriteria,
    "",
    "## What to Avoid / Constraints",
    constraints,
    "",
    "## Monocode Plugin Contract (Protocol v1)",
    "- Each plugin is a standalone OS child process launched from `plugin.json` (`command` and `cwd`).",
    "- Transport is newline-delimited JSON-RPC 2.0 over `stdin` and `stdout`.",
    "- `stdout` carries compact JSON-RPC messages only (one JSON object per line, max 2 MiB). Write all logs and diagnostics to `stderr`.",
    "- Lifecycle messages sent by Monocode:",
    "  1. `initialize` (request with `id`): reply within 5 seconds with `{\"protocol\": 1, \"ui\": [{\"id\": \"main\", \"title\": \"...\", \"icon\": \"sparkles\", \"order\": 10}]}`.",
    "  2. `ui.open` (notification `{\"surface\": \"main\"}`): send a `ui.set` notification containing the declarative UI tree.",
    "  3. `ui.event` (notification `{\"surface\": \"main\", \"handler\": \"...\", \"value\": ...}`): update plugin state and publish a new `ui.set`.",
    "  4. `ui.close` (notification `{\"surface\": \"main\"}`): surface hidden; process stays alive.",
    "  5. `shutdown` (notification): exit cleanly with code 0.",
    "- Declarative native UI (`ui.set` notification `{\"surface\": \"<id>\", \"tree\": <Node>}`):",
    "  - Native GPUI component tree (no HTML, CSS, or WebView; max 512 nodes, depth 16, 1 MiB JSON).",
    "  - Layout nodes: `column` (`gap`, optional `weight`, `children`), `row` (`gap`, `children`), `grid` (`columns`, `gap`, `children`), `card` (`gap`, `children`), `spacer`, `page_tabs` (`labels`, `selected`, `handler`, `nav_width`, `children`).",
    "  - Display nodes: `text` (`text`, `size`: `caption|small|body|subtitle|title|hero`, `color`: `ink|muted|faint|accent|positive|negative|warning|series1..series6`, optional `monospace`), `stat` (`label`, `value`, optional `detail`, `color`), `table` (`columns`, `rows`, optional `weights`, optional `selected` + `handler`), `chart` (`chart_type`: `bar|line|area`, `labels`, `series`, `height`), `donut`, `gauge`, `heatmap`, `progress`.",
    "  - Control nodes: `button` (`label`, `handler`, `disabled`), `toggle` (`label`, `value`, `handler`), `input` (`label`, `value`, `placeholder`, `handler`, optional `submit_handler`), `select` (`label`, `options`, `selected`, `handler`), `slider` (`label`, `value`, `min`, `max`, `step`, `handler`), `tabs` (`labels`, `selected`, `handler`), `code_editor` (`label`, `value`, `language`, `height`, `handler`, `read_only`, optional `submit_handler`, `selection_handler`, `context_menu`).",
    "  - Visible field labels: `input`, `select`, and `code_editor` use `label` for accessibility only; place a `caption` `text` node above them when a visible label is needed.",
    "- Host API requests (plugin sends JSON-RPC request with `id` to Monocode):",
    "  - `workspace.info` -> `{\"root\", \"name\"}`",
    "  - `storage.get` (`{\"key\"}`) and `storage.set` (`{\"key\", \"value\"}`) -> plugin-scoped JSON store (1 MiB cap)",
    "  - `host.describe` -> protocol, methods, features, limits, and `pluginDir`",
    "  - `llm.providers` and `llm.complete` -> read-only background text completions",
    "  - `session.start` (`{\"prompt\", \"title\"}`) -> start a workspace chat session",
    "- Build and launch:",
    "  - Run `npm run plugins:build` to build `plugin-sources/*` into runnable packages under `plugins/` (or place the runnable package directory inside `MONOCODE_PLUGIN_DIR` / `<app-data-dir>/plugins/`).",
    "  - Launch with `npm run plugins` (or restart Monocode after manifest or binary changes).",
    "",
    "## Reference Documentation (GitHub)",
    `- Repository (${REPO_BRANCH} branch): ${TREE_BASE_URL}`,
    `- Plugin overview: ${DOCS_BASE_URL}/plugins/README.md`,
    `- Wire protocol and UI nodes: ${DOCS_BASE_URL}/plugins/PROTOCOL.md`,
    `- Host API reference: ${DOCS_BASE_URL}/plugins/HOST_API.md`,
    `- Architecture: ${DOCS_BASE_URL}/plugins/ARCHITECTURE.md`,
    `- Example plugin sources: ${TREE_BASE_URL}/plugin-sources`,
    "",
    "## Next Steps",
    `Inspect the minimal blank scaffold in \`${info.dir}\` and perform these steps before writing feature code:`,
    "1. **Analyze the requirements:** Review what the plugin should do, the success criteria, and the constraints above, and call out any ambiguities or open questions.",
    "2. **Check existing Monocode features:** Analyze whether Monocode already has a feature like this (or overlaps with existing built-in capabilities) so the plugin adds distinct value rather than duplicating native functionality.",
    "3. **Assess plugin feasibility:** Analyze whether Monocode's plugin architecture (protocol v1 over stdio, available declarative native UI nodes, and host API methods) can support this kind of plugin cleanly, noting any constraints or tradeoffs up front.",
    "4. **Formulate and present an implementation plan:** Present a clear, step-by-step implementation plan to the user in concise, plain, everyday language, and wait for their guidance before implementing.",
  ].join("\n");
}

/* ---------------------------------------------------------------------------
 * Plugin detection
 * ------------------------------------------------------------------------ */

function listRelativeFiles(dir, prefix = "", out = []) {
  if (out.length >= 16) return out;
  let entries = [];
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  entries.sort((a, b) => a.name.localeCompare(b.name));
  for (const entry of entries) {
    if (out.length >= 16) break;
    if (
      entry.name === ".git" ||
      entry.name === "target" ||
      entry.name === "node_modules" ||
      entry.name === "__pycache__" ||
      entry.name.startsWith(".")
    ) {
      continue;
    }
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      listRelativeFiles(path.join(dir, entry.name), rel, out);
    } else if (entry.isFile()) {
      out.push(rel);
    }
  }
  return out;
}

function detectLanguage(dir, manifest) {
  if (dir) {
    if (fs.existsSync(path.join(dir, "Cargo.toml"))) return "Rust";
    if (fs.existsSync(path.join(dir, "main.py"))) return "Python";
    if (
      fs.existsSync(path.join(dir, "index.js")) ||
      fs.existsSync(path.join(dir, "package.json"))
    ) {
      return "Node.js";
    }
  }
  const cmd =
    manifest && Array.isArray(manifest.command) && manifest.command.length
      ? String(manifest.command[0])
      : "";
  if (/python/i.test(cmd)) return "Python";
  if (/node/i.test(cmd)) return "Node.js";
  if (cmd.includes("/bin/") || cmd.startsWith("./bin/")) return "Rust / Binary";
  return cmd ? path.basename(cmd) : "unknown";
}

function readManifestDir(dir) {
  const manifestPath = path.join(dir, "plugin.json");
  if (!fs.existsSync(manifestPath)) {
    return { manifest: null, error: "Missing plugin.json" };
  }
  try {
    const raw = fs.readFileSync(manifestPath, "utf8");
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") {
      return { manifest: null, error: "Manifest is not an object" };
    }
    if (!parsed.id || !parsed.name || !Array.isArray(parsed.command) || !parsed.command.length) {
      return { manifest: null, error: "Manifest requires id, name, and command" };
    }
    return { manifest: parsed, error: "" };
  } catch (error) {
    return { manifest: null, error: `Invalid plugin.json: ${error.message}` };
  }
}

function childDirectories(rootDir) {
  if (!rootDir || !fs.existsSync(rootDir)) return [];
  try {
    return fs
      .readdirSync(rootDir, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() && !entry.name.startsWith("."))
      .map((entry) => ({
        folder: entry.name,
        fullPath: path.join(rootDir, entry.name),
      }))
      .sort((a, b) => a.folder.localeCompare(b.folder));
  } catch {
    return [];
  }
}

function runtimeCommandExists(dir, manifest) {
  if (!dir || !manifest || !Array.isArray(manifest.command) || !manifest.command.length) {
    return false;
  }
  const bin = String(manifest.command[0]);
  if (bin.startsWith("./") || bin.startsWith("../") || bin.includes("/")) {
    const cwd = manifest.cwd ? path.resolve(dir, String(manifest.cwd)) : dir;
    return fs.existsSync(path.resolve(cwd, bin));
  }
  const scriptArg = manifest.command[1] ? String(manifest.command[1]) : "";
  if (scriptArg && !scriptArg.startsWith("-")) {
    const cwd = manifest.cwd ? path.resolve(dir, String(manifest.cwd)) : dir;
    return fs.existsSync(path.resolve(cwd, scriptArg));
  }
  return true;
}

function parseReadmeSections(readmePath) {
  const out = { purpose: "", successCriteria: "", constraints: "" };
  if (!fs.existsSync(readmePath)) return out;
  try {
    const lines = fs.readFileSync(readmePath, "utf8").split("\n");
    let current = "intro";
    const buckets = { intro: [], purpose: [], successCriteria: [], constraints: [] };
    for (const line of lines) {
      const heading = /^##\s+(.*)$/.exec(line.trim());
      if (heading) {
        const label = heading[1].toLowerCase();
        if (label.includes("what it does")) current = "purpose";
        else if (label.includes("success")) current = "successCriteria";
        else if (label.includes("constraint") || label.includes("avoid")) current = "constraints";
        else current = "other";
        continue;
      }
      if (line.trim().startsWith("# ")) continue;
      if (buckets[current]) buckets[current].push(line);
    }
    const cleanBlock = (arr) => arr.join("\n").trim().slice(0, 2000);
    out.purpose = cleanBlock(buckets.purpose) || cleanBlock(buckets.intro);
    out.successCriteria = cleanBlock(buckets.successCriteria);
    out.constraints = cleanBlock(buckets.constraints);
    return out;
  } catch {
    return out;
  }
}

function scanPlugins() {
  const byKey = new Map();

  const sourcesRoot = state.root ? path.join(state.root, "plugin-sources") : "";
  for (const item of childDirectories(sourcesRoot)) {
    const info = readManifestDir(item.fullPath);
    const key = info.manifest && info.manifest.id ? info.manifest.id : `folder:${item.folder}`;
    byKey.set(key, {
      key,
      folder: item.folder,
      sourceDir: item.fullPath,
      sourceManifest: info.manifest,
      sourceError: info.error,
      runtimeDir: "",
      runtimeManifest: null,
      runtimeError: "",
    });
  }

  const fallbackRuntime = state.root ? path.join(state.root, "plugins") : "";
  const runtimeRoot = state.pluginDir || fallbackRuntime;
  if (runtimeRoot && fs.existsSync(runtimeRoot)) {
    const rootManifest = path.join(runtimeRoot, "plugin.json");
    const runtimeDirs = fs.existsSync(rootManifest)
      ? [{ folder: path.basename(runtimeRoot), fullPath: runtimeRoot }]
      : childDirectories(runtimeRoot);

    for (const item of runtimeDirs) {
      const info = readManifestDir(item.fullPath);
      const key =
        info.manifest && info.manifest.id ? info.manifest.id : `folder:${item.folder}`;
      const existing = byKey.get(key);
      if (existing) {
        existing.runtimeDir = item.fullPath;
        existing.runtimeManifest = info.manifest;
        existing.runtimeError = info.error;
      } else {
        byKey.set(key, {
          key,
          folder: item.folder,
          sourceDir: "",
          sourceManifest: null,
          sourceError: "",
          runtimeDir: item.fullPath,
          runtimeManifest: info.manifest,
          runtimeError: info.error,
        });
      }
    }
  }

  const list = [];
  for (const entry of byKey.values()) {
    const manifest = entry.sourceManifest || entry.runtimeManifest;
    const primaryDir = entry.sourceDir || entry.runtimeDir;
    const error = entry.sourceError || entry.runtimeError;
    const uiList = manifest && Array.isArray(manifest.ui) ? manifest.ui : [];
    const firstSurface = uiList[0] || null;
    const order =
      firstSurface && typeof firstSurface.order === "number"
        ? firstSurface.order
        : Number.MAX_SAFE_INTEGER;
    const surfaceLabel = uiList.length
      ? uiList
          .map((surface) =>
            typeof surface.order === "number"
              ? `${surface.title || surface.id} (#${surface.order})`
              : String(surface.title || surface.id || "surface"),
          )
          .join(", ")
      : "none";

    let status = "invalid";
    if (!error && manifest) {
      const readyInRuntime =
        Boolean(entry.runtimeDir) &&
        !entry.runtimeError &&
        runtimeCommandExists(entry.runtimeDir, entry.runtimeManifest || manifest);
      if (entry.sourceDir && readyInRuntime) {
        status = "built";
      } else if (entry.sourceDir && !entry.runtimeDir) {
        status = "source";
      } else if (!entry.sourceDir && readyInRuntime) {
        status = "runtime";
      } else if (entry.runtimeDir && !readyInRuntime) {
        status = "missing binary";
      }
    }

    const language = detectLanguage(entry.sourceDir || entry.runtimeDir, manifest);
    const command =
      manifest && Array.isArray(manifest.command)
        ? manifest.command.join(" ")
        : "none";
    const relFiles = primaryDir ? listRelativeFiles(primaryDir) : [];
    const readmeSpec = primaryDir
      ? parseReadmeSections(path.join(primaryDir, "README.md"))
      : { purpose: "", successCriteria: "", constraints: "" };
    const workspaceRel = workspaceRelativePath(primaryDir);

    list.push({
      folder: entry.folder,
      name: manifest ? String(manifest.name) : entry.folder,
      id: manifest ? String(manifest.id) : "invalid",
      language,
      surface: surfaceLabel,
      order,
      status,
      command,
      sourceDir: entry.sourceDir,
      runtimeDir: entry.runtimeDir,
      primaryDir,
      workspaceRel,
      files: relFiles,
      readmeSpec,
      error,
    });
  }

  list.sort((a, b) => {
    if (a.order !== b.order) return a.order - b.order;
    return a.name.toLowerCase().localeCompare(b.name.toLowerCase());
  });

  state.plugins = list;
  if (state.selectedPlugin >= state.plugins.length) {
    state.selectedPlugin = Math.max(0, state.plugins.length - 1);
  }
}

/* ---------------------------------------------------------------------------
 * Actions
 * ------------------------------------------------------------------------ */

function startSessionForPlugin(info, resetFormOnSuccess = false) {
  if (state.starting) return;
  state.starting = true;
  state.status = "";
  publish();

  const prompt = buildAuthoringPrompt(info);
  request(
    "session.start",
    {
      title: `Plugin: ${info.title}`,
      prompt,
    },
    (_result, error) => {
      state.starting = false;
      if (error) {
        state.status = `Error: ${error.message || "Could not start authoring session."}`;
      } else {
        if (resetFormOnSuccess) {
          state.form.name = "";
          state.form.id = "";
          state.form.purpose = "";
          state.form.successCriteria = "";
          state.form.constraints = "";
        }
        state.status = `Created ${info.dir} and started session.`;
      }
      publish();
    },
  );
}

function createAndStart() {
  if (state.starting) return;
  const rawName = clean(state.form.name).trim();
  if (!rawName) {
    state.status = "Error: Enter a plugin name.";
    publish();
    return;
  }

  const slug = formSlug();
  const pluginId = formId();
  if (!validPluginId(pluginId)) {
    state.status =
      "Error: Identifier must use 1-96 letters, digits, dots, underscores, or hyphens.";
    publish();
    return;
  }

  const targetRel = formTarget();
  let targetAbs;
  try {
    targetAbs = insideRoot(targetRel);
  } catch (error) {
    state.status = `Error: ${error.message}`;
    publish();
    return;
  }

  if (fs.existsSync(targetAbs) && fs.readdirSync(targetAbs).length > 0) {
    state.status = `Error: Directory already exists and is not empty: ${targetRel}`;
    publish();
    return;
  }

  const spec = {
    purpose: clean(state.form.purpose).trim(),
    successCriteria: clean(state.form.successCriteria).trim(),
    constraints: clean(state.form.constraints).trim(),
  };
  const languageIndex = state.form.language;
  const languageName = LANGUAGES[languageIndex] || LANGUAGES[0];
  const files = scaffoldFiles(slug, rawName, pluginId, spec, languageIndex);

  try {
    fs.mkdirSync(targetAbs, { recursive: true });
    for (const file of files) {
      const destination = path.join(targetAbs, file.path);
      fs.mkdirSync(path.dirname(destination), { recursive: true });
      fs.writeFileSync(destination, file.content);
      if (file.path.endsWith(".py") || file.path.endsWith(".js")) {
        fs.chmodSync(destination, 0o755);
      }
    }
  } catch (error) {
    state.status = `Error: ${error.message}`;
    publish();
    return;
  }

  scanPlugins();
  const createdIndex = state.plugins.findIndex((item) => item.id === pluginId);
  if (createdIndex >= 0) state.selectedPlugin = createdIndex;

  startSessionForPlugin(
    {
      title: rawName,
      id: pluginId,
      language: languageName,
      dir: targetRel,
      files: files.map((file) => `${targetRel}/${file.path}`),
      purpose: spec.purpose,
      successCriteria: spec.successCriteria,
      constraints: spec.constraints,
      created: true,
    },
    true,
  );
}

function openSelectedPluginSession() {
  if (state.starting) return;
  const selected = state.plugins[state.selectedPlugin];
  if (!selected || !selected.workspaceRel) {
    state.status =
      "Error: Select a plugin located inside the active workspace.";
    publish();
    return;
  }
  const relDir = selected.workspaceRel;
  const files = selected.files.length
    ? selected.files.map((item) => `${relDir}/${item}`)
    : [`${relDir}/plugin.json`];

  startSessionForPlugin(
    {
      title: selected.name,
      id: selected.id,
      language: selected.language,
      dir: relDir,
      files,
      purpose: selected.readmeSpec.purpose,
      successCriteria: selected.readmeSpec.successCriteria,
      constraints: selected.readmeSpec.constraints,
      created: false,
    },
    false,
  );
}

function refreshWorkspaceAndHost() {
  request("workspace.info", {}, (result) => {
    state.root = result && result.root ? String(result.root) : "";
    scanPlugins();
    publish();
  });
  request("host.describe", {}, (result) => {
    state.pluginDir = result && result.pluginDir ? String(result.pluginDir) : "";
    scanPlugins();
    publish();
  });
}

/* ---------------------------------------------------------------------------
 * Event handling
 * ------------------------------------------------------------------------ */

function handleEvent(handler, value) {
  switch (handler) {
    case "tab":
      state.tab = Math.max(0, Math.min(TABS.length - 1, Number(value) || 0));
      state.status = "";
      if (state.tab === 1) scanPlugins();
      publish();
      break;
    case "name":
      state.form.name = clip(value, 80);
      publish();
      break;
    case "id":
      state.form.id = clip(value, 96);
      publish();
      break;
    case "language":
      state.form.language = Math.max(
        0,
        Math.min(LANGUAGES.length - 1, Number(value) || 0),
      );
      publish();
      break;
    case "purpose":
      state.form.purpose = clip(value, 8000);
      break;
    case "success_criteria":
      state.form.successCriteria = clip(value, 8000);
      break;
    case "constraints":
      state.form.constraints = clip(value, 8000);
      break;
    case "create_and_start":
      createAndStart();
      break;
    case "reset":
      state.form = {
        name: "",
        id: "",
        language: state.form.language,
        purpose: "",
        successCriteria: "",
        constraints: "",
      };
      state.status = "";
      publish();
      break;
    case "select_plugin":
      state.selectedPlugin = Math.max(
        0,
        Math.min(state.plugins.length - 1, Number(value) || 0),
      );
      state.status = "";
      publish();
      break;
    case "rescan":
      state.status = "";
      scanPlugins();
      publish();
      break;
    case "open_plugin_session":
      openSelectedPluginSession();
      break;
    default:
      break;
  }
}

/* ---------------------------------------------------------------------------
 * Pages
 * ------------------------------------------------------------------------ */

function newPluginPage() {
  const slug = formSlug();
  const title = clean(state.form.name).trim() || "My Plugin";
  const pluginId = formId();
  const targetRel = formTarget();
  const spec = {
    purpose: clean(state.form.purpose).trim(),
    successCriteria: clean(state.form.successCriteria).trim(),
    constraints: clean(state.form.constraints).trim(),
  };
  const preview = scaffoldFiles(
    slug,
    title,
    pluginId,
    spec,
    state.form.language,
  ).map((file) => [`${targetRel}/${file.path}`, file.role]);

  const exists = targetAlreadyPopulated();
  const children = [
    section("Identity", [
      field(
        "Plugin name",
        inputNode("Plugin name", state.form.name, "Port Inspector", "name"),
      ),
      field(
        "Identifier",
        inputNode("Identifier", state.form.id, `com.example.${slug}`, "id"),
      ),
      field(
        "Language",
        selectNode("Language", LANGUAGES, state.form.language, "language"),
      ),
    ]),
    section("Authoring Brief", [
      field(
        "What should the plugin do?",
        codeEditor(
          "What should the plugin do?",
          state.form.purpose,
          "md",
          110,
          "purpose",
        ),
      ),
      field(
        "How will you know it works as intended?",
        codeEditor(
          "How will you know it works as intended?",
          state.form.successCriteria,
          "md",
          96,
          "success_criteria",
        ),
      ),
      field(
        "Anything you want to avoid or not do?",
        codeEditor(
          "Anything you want to avoid or not do?",
          state.form.constraints,
          "md",
          96,
          "constraints",
        ),
      ),
    ]),
    section("Scaffold Preview", [
      table(["Path", "Role"], preview, [2.2, 1.0]),
      ...(exists
        ? [
            text(
              `Directory already exists: ${targetRel}. Choose another name or open it from Plugins.`,
              "small",
              "warning",
            ),
          ]
        : []),
    ]),
    row([
      button(
        state.starting ? "Starting session..." : "Create & Start Session",
        "create_and_start",
        state.starting || !state.root || exists,
      ),
      button("Reset", "reset", state.starting),
    ]),
  ];

  return { kind: "column", gap: 18, children };
}

function pluginsPage() {
  const plugins = state.plugins;
  const builtCount = plugins.filter(
    (item) => item.status === "built" || item.status === "runtime",
  ).length;

  const header = row([
    text(
      `${plugins.length} detected · ${builtCount} ready`,
      "small",
      "muted",
    ),
    { kind: "spacer" },
    button("Rescan", "rescan"),
  ]);

  if (!plugins.length) {
    return {
      kind: "column",
      gap: 14,
      children: [
        header,
        text(
          "No plugins found in plugin-sources/ or the active plugins directory.",
          "body",
          "muted",
        ),
      ],
    };
  }

  const rows = plugins.map((item) => [
    item.name,
    item.id,
    item.language,
    item.surface,
    item.status,
  ]);

  const selected = plugins[state.selectedPlugin] || plugins[0];
  const detailRows = [
    ["Name", selected.name],
    ["Identifier", selected.id],
    ["Language", selected.language],
    ["Command", selected.command],
    ["Source path", displayPath(selected.sourceDir)],
    ["Runtime path", displayPath(selected.runtimeDir)],
    [
      "Status",
      selected.error
        ? `${selected.status} (${selected.error})`
        : selected.status === "source"
          ? "source (run npm run plugins:build)"
          : selected.status,
    ],
    [
      "Files",
      selected.files.length ? selected.files.join(", ") : "none",
    ],
  ];

  return {
    kind: "column",
    gap: 18,
    children: [
      header,
      table(
        ["Name", "ID", "Language", "Surface", "Status"],
        rows,
        [1.2, 1.5, 0.8, 1.2, 0.7],
        state.selectedPlugin,
        "select_plugin",
      ),
      section(selected.name, [
        table(["Field", "Value"], detailRows, [0.9, 2.5]),
        row([
          button(
            state.starting ? "Starting session..." : "Open Authoring Session",
            "open_plugin_session",
            state.starting || !selected.workspaceRel,
          ),
        ]),
      ]),
    ],
  };
}

function tree() {
  const body = state.tab === 1 ? pluginsPage() : newPluginPage();
  return pageTabs(TABS, state.tab, "tab", [withStatus(body)]);
}

/* ---------------------------------------------------------------------------
 * Protocol loop
 * ------------------------------------------------------------------------ */

const scanner = readline.createInterface({ input: process.stdin });
scanner.on("line", (line) => {
  if (!line.trim()) return;
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.method) {
    if (message.method === "initialize") {
      respond(message.id, {
        protocol: 1,
        ui: [
          {
            id: SURFACE,
            title: "Plugin Authoring",
            icon: "plug",
            order: 3,
          },
        ],
      });
      publish();
      refreshWorkspaceAndHost();
    } else if (message.method === "ui.open") {
      refreshWorkspaceAndHost();
      publish();
    } else if (message.method === "ui.event") {
      const params = message.params || {};
      if (params.surface === SURFACE) {
        handleEvent(String(params.handler || ""), params.value);
      }
    } else if (message.method === "shutdown") {
      process.exit(0);
    }
    return;
  }
  if (message.id !== undefined) {
    const callback = state.pending.get(message.id);
    if (!callback) return;
    state.pending.delete(message.id);
    callback(message.result, message.error);
  }
});
