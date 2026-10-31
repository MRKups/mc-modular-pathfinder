#!/usr/bin/env node
// Plugin Authoring Station: a wide, vertical-tab workspace for reading plugin
// docs, inspecting the host API, scaffolding a package, editing source, and
// asking the configured background model for help.
//
// stdout is protocol traffic only; diagnostics go to stderr.

import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const SURFACE = "authoring-station";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const DOCS_DIR = path.join(HERE, "docs");
const LANGUAGES = ["Node.js", "Python", "Rust", "Go"];
const TABS = ["Guide", "API", "Scaffold", "Generate", "Editor"];
const STEPS = ["Identity", "Destination", "Review", "Created"];
const LANGUAGE_NOTES = [
  "Node.js · readline over stdio, package.json with type: module.",
  "Python · stdlib json and sys, no third-party dependencies.",
  "Rust · serde_json, Cargo.toml, standalone [workspace].",
  "Go · encoding/json, go.mod, no third-party dependencies.",
];
const METHOD_NOTES = {
  "workspace.info": "Active project root and directory name.",
  "storage.get": "Read one plugin-scoped JSON value.",
  "storage.set": "Write one plugin-scoped JSON value.",
  "host.describe": "Protocol, methods, limits, features, plugin directory.",
  "llm.providers": "Installed text-capable providers and cached models.",
  "llm.complete": "Async, text-only, read-only model completion.",
};

const state = {
  root: "",
  tab: 0,
  docs: [],
  docIndex: 0,
  describe: null,
  describeError: "",
  providers: [],
  providersError: "",
  providerIndex: 0,
  model: "",
  system:
    "You are a concise Monocode plugin expert. Produce complete, runnable files. Follow protocol v1 and use only documented host methods.",
  prompt: "",
  response: "",
  generating: false,
  status: "",
  scaffold: {
    step: 0,
    name: "",
    id: "",
    description: "",
    language: 0,
    target: "",
    files: [],
    dir: "",
    mainFile: "",
  },
  editor: {
    path: "",
    content: "",
    language: "js",
    dirty: false,
  },
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

function text(value, size = "body", color = "ink") {
  return { kind: "text", text: clip(value, 16000), size, color };
}

function stat(label, value, detail, color) {
  const node = { kind: "stat", label: clean(label), value: clean(value), color };
  if (detail) node.detail = clean(detail);
  return node;
}

function card(title, children, gap = 10) {
  return {
    kind: "card",
    gap,
    children: [text(title, "subtitle", "ink"), ...children],
  };
}

function table(columns, rows) {
  const safeColumns = columns.map((column) => clean(column).slice(0, 256)).slice(0, 24);
  return {
    kind: "table",
    columns: safeColumns,
    rows: rows
      .slice(0, 250)
      .map((row) =>
        safeColumns.map((_, index) => clip(row[index] ?? "", 2048)),
      ),
  };
}

function button(label, handler, disabled = false) {
  return { kind: "button", label, handler, disabled };
}

function inputNode(label, value, placeholder, handler) {
  return { kind: "input", label, value: clip(value, 4096), placeholder, handler };
}

function selectNode(label, options, selected, handler) {
  const safe = options.length ? options.slice(0, 24) : ["—"];
  return {
    kind: "select",
    label,
    options: safe,
    selected: Math.max(0, Math.min(selected, safe.length - 1)),
    handler,
  };
}

function codeEditor(label, value, language, height, handler, readOnly = false) {
  return {
    kind: "code_editor",
    label,
    value: clip(value, 64 * 1024),
    language,
    height: Math.max(120, Math.min(900, Math.round(height))),
    handler,
    read_only: readOnly,
  };
}

function pageTabs(labels, selected, handler, children) {
  return {
    kind: "page_tabs",
    labels,
    selected,
    handler,
    nav_width: 210,
    children,
  };
}

function publish() {
  notify("ui.set", { surface: SURFACE, tree: tree() });
}

/* ---------------------------------------------------------------------------
 * Documentation
 * ------------------------------------------------------------------------ */

function loadDocs() {
  try {
    const names = fs
      .readdirSync(DOCS_DIR)
      .filter((name) => name.toLowerCase().endsWith(".md"))
      .sort();
    state.docs = names.map((name) => {
      const content = fs.readFileSync(path.join(DOCS_DIR, name), "utf8");
      return {
        name,
        title: name.replace(/\.md$/, "").replace(/[-_]/g, " "),
        content: content.slice(0, 40000),
      };
    });
    state.docIndex = Math.max(0, Math.min(state.docIndex, state.docs.length - 1));
  } catch (error) {
    state.docs = [];
    state.describeError = `Cannot read bundled docs: ${error.message}`;
  }
}

function inlineMarkdown(value) {
  return clean(value)
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g, "$1 ($2)")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/__([^_]+)__/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/_([^_]+)_/g, "$1")
    .trim();
}

/// A small Markdown reader for the bundled docs. It renders headings, prose,
/// lists, code fences, and simple pipe tables as native nodes.
function markdownNodes(markdown) {
  const lines = clean(markdown).slice(0, 40000).split("\n");
  const nodes = [];
  let paragraph = [];
  let truncated = false;

  const flush = () => {
    if (!paragraph.length) return;
    nodes.push(text(inlineMarkdown(paragraph.join(" ")), "body", "muted"));
    paragraph = [];
  };
  const push = (node) => {
    if (nodes.length >= 180) {
      truncated = true;
      return false;
    }
    nodes.push(node);
    return true;
  };

  let index = 0;
  while (index < lines.length) {
    if (truncated) break;
    const line = lines[index];

    if (/^\s*```/.test(line)) {
      flush();
      const language = line.trim().replace(/^```/, "").trim() || "txt";
      const code = [];
      index += 1;
      while (index < lines.length && !/^\s*```/.test(lines[index])) {
        code.push(lines[index]);
        index += 1;
      }
      index += 1;
      const source = code.join("\n");
      if (source.trim()) {
        const height = Math.max(140, Math.min(420, 42 + code.length * 19));
        push(codeEditor("Code block", source, language, height, "doc_code", true));
      }
      continue;
    }

    if (/^\s*$/.test(line)) {
      flush();
      index += 1;
      continue;
    }

    const heading = /^(#{1,4})\s+(.*)$/.exec(line);
    if (heading) {
      flush();
      const level = heading[1].length;
      push(
        text(
          inlineMarkdown(heading[2]),
          level === 1 ? "title" : level === 2 ? "subtitle" : "body",
          "ink",
        ),
      );
      index += 1;
      continue;
    }

    if (/^\s*\|/.test(line) && index + 1 < lines.length && /^[\s|:-]+$/.test(lines[index + 1])) {
      flush();
      const tableLines = [];
      while (index < lines.length && /^\s*\|/.test(lines[index])) {
        tableLines.push(lines[index]);
        index += 1;
      }
      const parsed = tableLines.map((row) =>
        row
          .trim()
          .replace(/^\|/, "")
          .replace(/\|$/, "")
          .split("|")
          .map((cell) => inlineMarkdown(cell)),
      );
      if (parsed.length >= 2) {
        const columns = parsed[0];
        const rows = parsed
          .slice(2)
          .filter((row) => row.some((cell) => cell.length));
        push(table(columns, rows));
      }
      continue;
    }

    if (/^\s*[-*+]\s+/.test(line)) {
      flush();
      push(text("• " + inlineMarkdown(line.replace(/^\s*[-*+]\s+/, "")), "small", "muted"));
      index += 1;
      continue;
    }

    if (/^\s*\d+\.\s+/.test(line)) {
      flush();
      push(text(inlineMarkdown(line), "small", "muted"));
      index += 1;
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      flush();
      push(text(inlineMarkdown(line.replace(/^\s*>\s?/, "")), "small", "muted"));
      index += 1;
      continue;
    }

    if (/^\s*(---+|\*\*\*+)\s*$/.test(line)) {
      flush();
      index += 1;
      continue;
    }

    paragraph.push(line.trim());
    index += 1;
  }
  flush();
  if (truncated) {
    nodes.push(text("Document rendered partially to stay inside the node budget.", "caption", "warning"));
  }
  return nodes.length ? nodes : [text("This document is empty.", "body", "muted")];
}

/* ---------------------------------------------------------------------------
 * Helpers
 * ------------------------------------------------------------------------ */

function provider() {
  return state.providers[state.providerIndex] || null;
}

function providerLabels() {
  if (!state.providers.length) return ["No provider installed"];
  return state.providers.map((item) => `${item.label} · ${item.account || "default"}`);
}

function chooseModel(index) {
  const item = state.providers[index];
  if (!item) {
    state.model = "";
    return;
  }
  const models = Array.isArray(item.models) ? item.models : [];
  state.model = models.length ? models[0].id : "";
}

function insideRoot(relative) {
  if (!state.root) throw new Error("The active project root is not available yet.");
  const resolved = path.resolve(state.root, relative);
  const rel = path.relative(state.root, resolved);
  if (rel.startsWith("..") || path.isAbsolute(rel)) {
    throw new Error("Only paths inside the active project are allowed.");
  }
  return resolved;
}

function relativeToRoot(absolute) {
  return path.relative(state.root, absolute).split(path.sep).join("/");
}

function languageFor(pathname) {
  const extension = path.extname(pathname).toLowerCase().slice(1);
  if (["js", "jsx", "mjs", "cjs"].includes(extension)) return "js";
  if (["ts", "tsx"].includes(extension)) return "ts";
  if (["py", "pyi"].includes(extension)) return "py";
  if (extension === "rs") return "rs";
  if (extension === "go") return "go";
  if (extension === "json") return "json";
  if (extension === "toml") return "toml";
  if (["yaml", "yml"].includes(extension)) return "yaml";
  if (["sh", "bash", "zsh"].includes(extension)) return "sh";
  if (["md", "mdx"].includes(extension)) return "md";
  return "txt";
}

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

function scaffoldSlug() {
  return slugify(state.scaffold.name || state.scaffold.id || "my-plugin");
}

function scaffoldId() {
  return clean(state.scaffold.id).trim() || `com.example.${scaffoldSlug()}`;
}

function scaffoldTarget() {
  return clean(state.scaffold.target).trim() || `plugin-sources/${scaffoldSlug()}`;
}

/* ---------------------------------------------------------------------------
 * Scaffold templates
 * ------------------------------------------------------------------------ */

function nodeTemplate(title) {
  return `import readline from "node:readline";
const SURFACE = "main";
let events = 0;
const send = (message) => process.stdout.write(JSON.stringify(message) + "\\n");
const tree = () => ({
  kind: "column",
  gap: 12,
  children: [
    { kind: "text", text: ${JSON.stringify(title)}, size: "title", color: "ink" },
    { kind: "stat", label: "Events", value: String(events), color: "accent" },
    { kind: "button", label: "Ping", handler: "ping", disabled: false },
  ],
});
const publish = () =>
  send({ jsonrpc: "2.0", method: "ui.set", params: { surface: SURFACE, tree: tree() } });
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
    events += 1;
    publish();
  } else if (message.method === "shutdown") {
    process.exit(0);
  }
});
function respond(id, result) {
  send({ jsonrpc: "2.0", id, result });
}
`;
}

function pythonTemplate(title) {
  return `#!/usr/bin/env python3
import json
import sys

SURFACE = "main"
events = 0


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\\n")
    sys.stdout.flush()


def tree():
    return {
        "kind": "column",
        "gap": 12,
        "children": [
            {"kind": "text", "text": ${JSON.stringify(title)}, "size": "title", "color": "ink"},
            {"kind": "stat", "label": "Events", "value": str(events), "color": "accent"},
            {"kind": "button", "label": "Ping", "handler": "ping", "disabled": False},
        ],
    }


def publish():
    send({"jsonrpc": "2.0", "method": "ui.set", "params": {"surface": SURFACE, "tree": tree()}})


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
        events += 1
        publish()
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

fn tree(events: u64) -> Value {
    json!({
        "kind": "column",
        "gap": 12,
        "children": [
            {"kind": "text", "text": ${JSON.stringify(title)}, "size": "title", "color": "ink"},
            {"kind": "stat", "label": "Events", "value": events.to_string(), "color": "accent"},
            {"kind": "button", "label": "Ping", "handler": "ping", "disabled": false}
        ]
    })
}

fn publish(events: u64) {
    send(json!({"jsonrpc": "2.0", "method": "ui.set", "params": {"surface": SURFACE, "tree": tree(events)}}));
}

fn main() {
    let mut events = 0u64;
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
                publish(events);
            }
            "ui.open" => publish(events),
            "ui.event" => {
                events += 1;
                publish(events);
            }
            "shutdown" => break,
            _ => {}
        }
    }
}
`;
}

function goTemplate(title) {
  return `package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strings"
)

const surface = "main"

var events = 0

func send(message map[string]any) {
	line, _ := json.Marshal(message)
	fmt.Printf("%s\\n", line)
}

func tree() map[string]any {
	return map[string]any{
		"kind": "column",
		"gap":  12,
		"children": []any{
			map[string]any{"kind": "text", "text": ${JSON.stringify(title)}, "size": "title", "color": "ink"},
			map[string]any{"kind": "stat", "label": "Events", "value": fmt.Sprint(events), "color": "accent"},
			map[string]any{"kind": "button", "label": "Ping", "handler": "ping", "disabled": false},
		},
	}
}

func notify(method string, params map[string]any) {
	send(map[string]any{"jsonrpc": "2.0", "method": method, "params": params})
}

func publish() {
	notify("ui.set", map[string]any{"surface": surface, "tree": tree()})
}

func main() {
	scanner := bufio.NewScanner(os.Stdin)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}
		var message map[string]any
		if json.Unmarshal([]byte(line), &message) != nil {
			continue
		}
		method, _ := message["method"].(string)
		switch method {
		case "initialize":
			send(map[string]any{
				"jsonrpc": "2.0",
				"id":      message["id"],
				"result": map[string]any{
					"protocol": 1,
					"ui": []any{map[string]any{
						"id": surface, "title": ${JSON.stringify(title)}, "icon": "sparkles", "order": 10,
					}},
				},
			})
			publish()
		case "ui.open":
			publish()
		case "ui.event":
			events++
			publish()
		case "shutdown":
			return
		}
	}
}
`;
}

function scaffoldFiles(slug, title, pluginId, description, language) {
  const files = [];
  files.push({
    path: "plugin.json",
    content:
      JSON.stringify(
        {
          id: pluginId,
          name: title,
          protocol: 1,
          command:
            language === 0
              ? ["node", "index.js"]
              : language === 1
                ? ["python3", "main.py"]
                : [`./bin/${slug}`],
          cwd: ".",
          ui: [{ id: "main", title, icon: "sparkles", order: 10 }],
        },
        null,
        2,
      ) + "\n",
  });
  if (language === 0) {
    files.push({ path: "index.js", content: nodeTemplate(title) });
    files.push({
      path: "package.json",
      content: `${JSON.stringify({ private: true, type: "module" }, null, 2)}\n`,
    });
  } else if (language === 1) {
    files.push({ path: "main.py", content: pythonTemplate(title) });
  } else if (language === 2) {
    files.push({
      path: "Cargo.toml",
      content: `[package]\nname = "${slug}"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nserde_json = "1"\n\n[workspace]\n`,
    });
    files.push({ path: "src/main.rs", content: rustTemplate(title) });
  } else {
    files.push({
      path: "go.mod",
      content: `module monocode.example/${slug}\n\ngo 1.22\n`,
    });
    files.push({ path: "main.go", content: goTemplate(title) });
  }
  files.push({
    path: "README.md",
    content: `# ${title}\n\n${description || "Generated by the Monocode Plugin Authoring Station."}\n\n## Build and test\n\n\`\`\`sh\nnpm run plugins:build\nnpm run plugins\n\`\`\`\n\nRestart Monocode after changing protocol code or the manifest. The plugin speaks\nJSON-RPC 2.0 over newline-delimited stdin/stdout and publishes native UI trees.\n`,
  });
  return files;
}

function createScaffold() {
  try {
    const slug = scaffoldSlug();
    const title = clean(state.scaffold.name).trim() || slug;
    const pluginId = scaffoldId();
    if (!validPluginId(pluginId)) {
      throw new Error("Plugin id must use 1–96 letters, digits, dots, underscores, or hyphens.");
    }
    const targetInput = scaffoldTarget().replace(/\\/g, "/");
    const target = insideRoot(targetInput);
    if (fs.existsSync(target) && fs.readdirSync(target).length > 0) {
      throw new Error(`Refusing to overwrite non-empty directory: ${targetInput}`);
    }
    fs.mkdirSync(target, { recursive: true });
    const files = scaffoldFiles(
      slug,
      title,
      pluginId,
      clean(state.scaffold.description).trim(),
      state.scaffold.language,
    );
    for (const file of files) {
      const destination = path.join(target, file.path);
      fs.mkdirSync(path.dirname(destination), { recursive: true });
      fs.writeFileSync(destination, file.content);
      if (file.path.endsWith(".py") || file.path.endsWith(".js")) {
        fs.chmodSync(destination, 0o755);
      }
    }
    const mainFile =
      state.scaffold.language === 0
        ? "index.js"
        : state.scaffold.language === 1
          ? "main.py"
          : state.scaffold.language === 2
            ? "src/main.rs"
            : "main.go";
    state.scaffold.files = files.map((file) => ({
      path: path.posix.join(targetInput, file.path),
      bytes: Buffer.byteLength(file.content),
    }));
    state.scaffold.dir = targetInput;
    state.scaffold.mainFile = path.posix.join(targetInput, mainFile);
    state.scaffold.step = 3;
    state.editor.path = state.scaffold.mainFile;
    state.editor.content = fs.readFileSync(path.join(target, mainFile), "utf8");
    state.editor.language = languageFor(mainFile);
    state.editor.dirty = false;
    state.status = `Created ${files.length} files in ${targetInput}. Build with npm run plugins:build, then restart.`;
  } catch (error) {
    state.status = `Scaffold error: ${error.message}`;
  }
  publish();
}

function loadEditor() {
  try {
    const absolute = insideRoot(state.editor.path.trim());
    const info = fs.statSync(absolute);
    if (!info.isFile()) throw new Error("Path is not a file.");
    if (info.size > 64 * 1024) throw new Error("File is larger than 64 KB.");
    state.editor.content = fs.readFileSync(absolute, "utf8");
    state.editor.language = languageFor(absolute);
    state.editor.dirty = false;
    state.status = `Loaded ${relativeToRoot(absolute)}`;
  } catch (error) {
    state.status = `Load error: ${error.message}`;
  }
  publish();
}

function saveEditor() {
  try {
    const absolute = insideRoot(state.editor.path.trim());
    fs.mkdirSync(path.dirname(absolute), { recursive: true });
    fs.writeFileSync(absolute, state.editor.content);
    state.editor.dirty = false;
    state.status = `Saved ${relativeToRoot(absolute)} · restart to test protocol changes`;
  } catch (error) {
    state.status = `Save error: ${error.message}`;
  }
  publish();
}

function generate() {
  if (state.generating) return;
  if (!state.prompt.trim()) {
    state.status = "Write a prompt first.";
    publish();
    return;
  }
  const active = provider();
  if (!active && state.providers.length) {
    state.status = "Select a provider.";
    publish();
    return;
  }
  state.generating = true;
  state.status = active
    ? `Generating with ${active.label}${state.model ? ` · ${state.model}` : ""}…`
    : "Generating…";
  const params = {
    prompt: state.prompt.slice(0, 64 * 1024),
    system: state.system.slice(0, 16 * 1024),
  };
  if (active) params.provider = active.id;
  if (state.model.trim()) params.model = state.model.trim();
  request("llm.complete", params, (result, error) => {
    state.generating = false;
    if (error) {
      state.status = `Model error: ${error.message || "completion failed"}`;
    } else {
      state.response = clean(result && result.text ? result.text : "").slice(0, 32 * 1024);
      const used =
        result && result.provider
          ? `${result.provider}${result.model ? ` · ${result.model}` : ""}`
          : "model";
      state.status = state.response
        ? `Generated ${state.response.length} characters with ${used}`
        : `The model returned no text (${used}).`;
    }
    publish();
  });
  publish();
}

function refreshApi() {
  request("host.describe", {}, (result, error) => {
    if (error) {
      state.describeError = error.message || "host.describe failed";
    } else {
      state.describe = result;
      state.describeError = "";
    }
    publish();
  });
  request("llm.providers", {}, (result, error) => {
    if (error) {
      state.providersError = error.message || "llm.providers failed";
      state.providers = [];
    } else {
      state.providers = Array.isArray(result && result.providers) ? result.providers : [];
      state.providersError = "";
      state.providerIndex = Math.max(0, Math.min(state.providerIndex, state.providers.length - 1));
      if (!state.model) chooseModel(state.providerIndex);
    }
    publish();
  });
}

/* ---------------------------------------------------------------------------
 * Event handling
 * ------------------------------------------------------------------------ */

function handleEvent(handler, value) {
  let republish = true;
  switch (handler) {
    case "tab":
      state.tab = Math.max(0, Math.min(TABS.length - 1, Number(value) || 0));
      break;
    case "doc":
      state.docIndex = Math.max(0, Math.min(state.docs.length - 1, Number(value) || 0));
      break;
    case "reload_docs":
      loadDocs();
      state.status = `Reloaded ${state.docs.length} bundled doc(s).`;
      break;
    case "scaffold_name":
      state.scaffold.name = clip(value, 80);
      republish = false;
      break;
    case "scaffold_id":
      state.scaffold.id = clip(value, 96);
      republish = false;
      break;
    case "scaffold_description":
      state.scaffold.description = clip(value, 500);
      republish = false;
      break;
    case "scaffold_target":
      state.scaffold.target = clip(value, 512);
      republish = false;
      break;
    case "scaffold_language":
      state.scaffold.language = Math.max(
        0,
        Math.min(LANGUAGES.length - 1, Number(value) || 0),
      );
      break;
    case "scaffold_next":
      if (state.scaffold.step === 0) {
        state.scaffold.name =
          clean(state.scaffold.name).trim() || "My Plugin";
        if (!clean(state.scaffold.id).trim()) {
          state.scaffold.id = `com.example.${slugify(state.scaffold.name)}`;
        }
        if (!clean(state.scaffold.target).trim()) {
          state.scaffold.target = `plugin-sources/${slugify(state.scaffold.name)}`;
        }
        state.scaffold.step = 1;
      } else if (state.scaffold.step === 1) {
        state.scaffold.step = 2;
      }
      break;
    case "scaffold_back":
      state.scaffold.step = Math.max(0, state.scaffold.step - 1);
      break;
    case "scaffold_create":
      createScaffold();
      republish = false;
      break;
    case "scaffold_open":
      if (state.scaffold.mainFile) {
        state.editor.path = state.scaffold.mainFile;
        loadEditor();
        state.tab = 4;
      }
      break;
    case "scaffold_reset":
      state.scaffold = {
        step: 0,
        name: "",
        id: "",
        description: "",
        language: state.scaffold.language,
        target: "",
        files: [],
        dir: "",
        mainFile: "",
      };
      state.status = "Scaffold form reset.";
      break;
    case "provider":
      state.providerIndex = Math.max(
        0,
        Math.min(state.providers.length - 1, Number(value) || 0),
      );
      chooseModel(state.providerIndex);
      break;
    case "model":
      state.model = clip(value, 200);
      republish = false;
      break;
    case "system":
      state.system = clip(value, 16 * 1024);
      republish = false;
      break;
    case "prompt":
      state.prompt = clip(value, 64 * 1024);
      republish = false;
      break;
    case "generate":
      generate();
      republish = false;
      break;
    case "clear":
      state.prompt = "";
      state.response = "";
      state.status = "Cleared the prompt and response.";
      break;
    case "use_response":
      if (!state.response.trim()) {
        state.status = "Generate a response first.";
      } else {
        state.editor.content = state.response;
        state.editor.dirty = true;
        state.tab = 4;
        state.status = "Copied the model response into the editor. Review it, then save.";
      }
      break;
    case "editor_path":
      state.editor.path = clip(value, 512);
      republish = false;
      break;
    case "editor_content":
      state.editor.content = clip(value, 64 * 1024);
      state.editor.dirty = true;
      republish = false;
      break;
    case "editor_load":
      loadEditor();
      republish = false;
      break;
    case "editor_save":
      saveEditor();
      republish = false;
      break;
    case "refresh_api":
      refreshApi();
      republish = false;
      break;
    case "doc_code":
    case "response_view":
      republish = false;
      break;
    default:
      break;
  }
  if (republish) publish();
}

/* ---------------------------------------------------------------------------
 * Pages
 * ------------------------------------------------------------------------ */

function guideTab() {
  if (!state.docs.length) {
    return card("Documentation", [
      text(
        "No bundled docs were found next to the plugin. Rebuild with npm run plugins:build so docs/ is copied.",
        "body",
        "warning",
      ),
    ]);
  }
  const doc = state.docs[state.docIndex] || state.docs[0];
  return {
    kind: "grid",
    columns: 2,
    gap: 16,
    children: [
      card("Bundled documentation", [
        text(
          "Everything in this rail ships with the plugin package. Use the selector to move between the protocol, host API, node reference, security notes, and authoring workflow.",
          "small",
          "muted",
        ),
        selectNode(
          "Document",
          state.docs.map((item) => item.title),
          state.docIndex,
          "doc",
        ),
        button("Reload docs", "reload_docs"),
        text("Reference map", "caption", "faint"),
        text("Quickstart · manifest, handshake, lifecycle", "small", "muted"),
        text("Host API · storage, metadata, model access", "small", "muted"),
        text("UI nodes · native controls and limits", "small", "muted"),
        text("Authoring · scaffold, edit, build, restart", "small", "muted"),
        text("Security · trust boundaries and review checks", "small", "muted"),
      ]),
      card(doc.title, [
        text(`${doc.name} · bundled with the plugin package`, "caption", "muted"),
        ...markdownNodes(doc.content),
      ]),
    ],
  };
}

function apiTab() {
  const describe = state.describe || {};
  const methods = Array.isArray(describe.methods)
    ? describe.methods
    : Object.keys(METHOD_NOTES);
  const limits = describe.limits || {};
  const features = describe.features || {};
  const methodRows = methods.map((method) => [
    method,
    METHOD_NOTES[method] || "Host request.",
  ]);
  const limitRows = Object.entries(limits)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([name, value]) => [name, String(value)]);
  const featureRows = Object.entries(features).map(([name, value]) => [
    name,
    value ? "available" : "unavailable",
  ]);
  const providerRows = state.providers.map((item) => [
    item.label,
    item.account || "default",
    `${item.models ? item.models.length : 0} cached model(s)`,
    item.background ? "background" : "",
  ]);
  const children = [
    text(
      "Live values come from host.describe and llm.providers. Methods are additive; unknown methods return an error.",
      "small",
      "muted",
    ),
    {
      kind: "grid",
      columns: 2,
      gap: 16,
      children: [
        card("Host methods", [
          table(["Method", "What it returns"], methodRows),
          text(
            "Every call is a JSON-RPC request with an id. Notifications such as ui.set have no id.",
            "caption",
            "muted",
          ),
        ]),
        card("Limits", [
          limitRows.length
            ? table(["Limit", "Value"], limitRows)
            : text("host.describe did not return limits.", "body", "muted"),
        ]),
      ],
    },
    {
      kind: "grid",
      columns: 2,
      gap: 16,
      children: [
        card("Features", [
          featureRows.length
            ? table(["Feature", "Status"], featureRows)
            : text("No feature flags reported.", "body", "muted"),
        ]),
        card("Model providers", [
          state.providersError
            ? text(state.providersError, "small", "warning")
            : providerRows.length
              ? table(["Provider", "Account", "Models", "Role"], providerRows)
              : text(
                  "No text-capable provider is installed. Configure one in Settings to use the Generate page.",
                  "body",
                  "muted",
                ),
          text(
            "llm.complete runs a separate, text-only, read-only session. Plugins never receive provider credentials or the Operator token.",
            "caption",
            "muted",
          ),
        ]),
      ],
    },
    card("Wire shape", [
      codeEditor(
        "Request example",
        `{"jsonrpc":"2.0","id":7,"method":"host.describe","params":{}}\n\n// result\n{"jsonrpc":"2.0","id":7,"result":{"protocol":1,"methods":[…],"limits":{…}}}`,
        "json",
        190,
        "doc_code",
        true,
      ),
    ]),
  ];
  if (state.describeError) {
    children.push(card("Describe error", [text(state.describeError, "body", "warning")]));
  }
  return { kind: "column", gap: 16, children };
}

function scaffoldSummaryRows() {
  return [
    ["Name", clean(state.scaffold.name).trim() || "My Plugin"],
    ["Plugin id", scaffoldId()],
    ["Language", LANGUAGES[state.scaffold.language]],
    ["Target", scaffoldTarget()],
    ["Description", clean(state.scaffold.description).trim() || "—"],
  ];
}

function scaffoldPreviewFiles() {
  const language = state.scaffold.language;
  const target = scaffoldTarget().replace(/\\/g, "/");
  const files = ["plugin.json", "README.md"];
  if (language === 0) files.push("index.js", "package.json");
  else if (language === 1) files.push("main.py");
  else if (language === 2) files.push("Cargo.toml", "src/main.rs");
  else files.push("go.mod", "main.go");
  return files.map((file) => [path.posix.join(target, file), "generated"]);
}

function scaffoldStep() {
  const step = state.scaffold.step;
  if (step === 0) {
    return {
      kind: "grid",
      columns: 2,
      gap: 16,
      children: [
        card("Identity", [
          inputNode("Display name", state.scaffold.name, "My Plugin", "scaffold_name"),
          inputNode("Plugin id", state.scaffold.id, "com.example.my-plugin", "scaffold_id"),
          inputNode(
            "Description",
            state.scaffold.description,
            "What the plugin does",
            "scaffold_description",
          ),
          text(
            "A plugin is a separate executable that owns its state and publishes native UI trees. The id must be unique across installed plugins.",
            "caption",
            "muted",
          ),
        ]),
        card("Language", [
          selectNode("Implementation", LANGUAGES, state.scaffold.language, "scaffold_language"),
          text(LANGUAGE_NOTES[state.scaffold.language], "body", "muted"),
          text("Generated files", "caption", "faint"),
          text("plugin.json · valid manifest with one surface", "small", "muted"),
          text("Runnable source · handshake, ui.set, button event", "small", "muted"),
          text("Build files · package.json, Cargo.toml, or go.mod", "small", "muted"),
          text("README.md · build and restart instructions", "small", "muted"),
        ]),
      ],
    };
  }
  if (step === 1) {
    return {
      kind: "grid",
      columns: 2,
      gap: 16,
      children: [
        card("Destination", [
          inputNode(
            "Target directory",
            state.scaffold.target,
            "plugin-sources/my-plugin",
            "scaffold_target",
          ),
          text(`Resolved target: ${scaffoldTarget()}`, "small", "muted"),
          text(
            "The path is relative to the active project. Existing non-empty directories are refused.",
            "caption",
            "muted",
          ),
          text("Plugin id", "caption", "faint"),
          text(scaffoldId(), "body", "ink"),
        ]),
        card("Files to write", [
          table(["Path", "Source"], scaffoldPreviewFiles()),
          text(
            "The scaffold is a starting point, not a hidden runtime. Everything is ordinary project source.",
            "caption",
            "muted",
          ),
        ]),
      ],
    };
  }
  if (step === 2) {
    return {
      kind: "column",
      gap: 16,
      children: [
        {
          kind: "grid",
          columns: 2,
          gap: 16,
          children: [
            card("Review", [
              table(["Field", "Value"], scaffoldSummaryRows()),
              text(
                "Create writes the manifest, runnable source, build files, and README into the target directory.",
                "caption",
                "muted",
              ),
            ]),
            card("Files to write", [table(["Path", "Source"], scaffoldPreviewFiles())]),
          ],
        },
        card("Create", [
          text(
            "When the scaffold finishes, build it with npm run plugins:build and restart Monocode. Protocol v1 has no live plugin reload.",
            "body",
            "muted",
          ),
          {
            kind: "row",
            gap: 8,
            children: [button("Create scaffold", "scaffold_create")],
          },
        ]),
      ],
    };
  }
  const rows = state.scaffold.files.map((file) => [file.path, `${file.bytes} B`]);
  return {
    kind: "column",
    gap: 16,
    children: [
      card("Created", [
        text(`Wrote ${rows.length} files under ${state.scaffold.dir || scaffoldTarget()}.`, "body", "positive"),
        table(["Path", "Size"], rows),
      ]),
      {
        kind: "grid",
        columns: 2,
        gap: 16,
        children: [
          card("Next steps", [
            text("1. npm run plugins:build", "body", "ink"),
            text("2. npm run plugins", "body", "ink"),
            text("3. Open the new surface in the sidebar.", "body", "ink"),
            text(
              "Restart Monocode after changing the manifest or protocol code. A running plugin can still send new ui.set trees.",
              "caption",
              "muted",
            ),
          ]),
          card("Open the source", [
            text(
              "The new entry point is ready in the Editor page. Review it, save changes, then rebuild.",
              "body",
              "muted",
            ),
            button("Open main file", "scaffold_open"),
            button("Start another scaffold", "scaffold_reset"),
          ]),
        ],
      },
    ],
  };
}

function scaffoldTab() {
  const step = state.scaffold.step;
  const body = scaffoldStep();
  const navigation =
    step < 3
      ? {
          kind: "row",
          gap: 8,
          children: [
            button("Back", "scaffold_back", step === 0),
            button(step === 2 ? "Review is ready" : "Next", "scaffold_next", step === 2),
            text(`${STEPS[step]} · step ${step + 1} of 4`, "caption", "muted"),
          ],
        }
      : {
          kind: "row",
          gap: 8,
          children: [
            text(`${STEPS[3]} · scaffold complete`, "caption", "positive"),
            text("Open the entry point or start another package.", "caption", "muted"),
          ],
        };
  return {
    kind: "column",
    gap: 16,
    children: [
      card("Scaffold wizard", [
        {
          kind: "progress",
          label: `${STEPS[step]} (${step + 1}/4)`,
          value: step + 1,
          max: 4,
          detail: "Identity → destination → review → created",
          color: step === 3 ? "positive" : "accent",
        },
        text(
          "The wizard writes a complete package in four steps. Nothing is created until the review step.",
          "small",
          "muted",
        ),
        navigation,
      ]),
      body,
    ],
  };
}

function generateTab() {
  const active = provider();
  return {
    kind: "grid",
    columns: 2,
    gap: 16,
    children: [
      {
        kind: "column",
        gap: 16,
        children: [
          card("Provider", [
            {
              kind: "grid",
              columns: 2,
              gap: 10,
              children: [
                selectNode("Provider", providerLabels(), state.providerIndex, "provider"),
                inputNode("Model", state.model, "provider default", "model"),
              ],
            },
            stat(
              "Status",
              state.generating ? "generating" : state.providers.length ? "ready" : "no provider",
              active ? active.label : state.providersError || "configure a provider in Settings",
              state.generating ? "warning" : "accent",
            ),
            text(
              "The host runs this request on a worker thread. The plugin stays responsive while it waits.",
              "caption",
              "muted",
            ),
          ]),
          card("System prompt", [
            codeEditor("System prompt", state.system, "md", 170, "system"),
          ]),
          card("Prompt", [
            codeEditor("Prompt", state.prompt, "md", 260, "prompt"),
            {
              kind: "row",
              gap: 8,
              children: [
                button(state.generating ? "Generating…" : "Generate", "generate", state.generating),
                button("Clear", "clear"),
              ],
            },
          ]),
        ],
      },
      card("Response", [
        codeEditor("Generated response", state.response, "md", 620, "response_view", true),
        {
          kind: "row",
          gap: 8,
          children: [
            button("Use response in editor", "use_response", !state.response),
            text(
              "Text-only and tool-free. Review generated code before saving or running it.",
              "caption",
              "muted",
            ),
          ],
        },
      ]),
    ],
  };
}

function editorTab() {
  return {
    kind: "column",
    gap: 16,
    children: [
      card("File", [
        {
          kind: "row",
          gap: 8,
          children: [
            inputNode(
              "Project file",
              state.editor.path,
              "plugin-sources/my-plugin/index.js",
              "editor_path",
            ),
            button("Load", "editor_load"),
            button("Save", "editor_save"),
            stat(
              "State",
              state.editor.dirty ? "modified" : "clean",
              state.editor.language,
              state.editor.dirty ? "warning" : "positive",
            ),
          ],
        },
        text(
          "Paths are relative to the active project and cannot escape it. Protocol changes require a restart.",
          "caption",
          "muted",
        ),
      ]),
      card("Source", [
        codeEditor("Source", state.editor.content, state.editor.language, 700, "editor_content"),
      ]),
    ],
  };
}

function tree() {
  let body;
  if (state.tab === 1) body = apiTab();
  else if (state.tab === 2) body = scaffoldTab();
  else if (state.tab === 3) body = generateTab();
  else if (state.tab === 4) body = editorTab();
  else body = guideTab();

  const project = state.root || "waiting for workspace";
  const protocol = state.describe ? state.describe.protocol : "?";
  const active = provider();
  const providerLabel = active
    ? `${active.label}${state.model ? ` · ${state.model}` : ""}`
    : state.providers.length
      ? "no provider selected"
      : "no provider installed";
  return {
    kind: "column",
    gap: 16,
    children: [
      {
        kind: "row",
        gap: 16,
        children: [
          {
            kind: "column",
            gap: 4,
            children: [
              text("Plugin Authoring Station", "hero", "ink"),
              text(
                "Documentation, API reference, scaffolding, source editing, and model assistance in one wide workspace.",
                "body",
                "muted",
              ),
            ],
          },
          stat(
            "Protocol",
            String(protocol),
            `${project} · ${providerLabel}`,
            state.describeError ? "warning" : "accent",
          ),
        ],
      },
      text(
        state.status || "Ready. Pick a page from the rail on the left.",
        "small",
        state.status.startsWith("Scaffold error") || state.status.startsWith("Model error")
          ? "warning"
          : "muted",
      ),
      pageTabs(TABS, state.tab, "tab", [body]),
    ],
  };
}

/* ---------------------------------------------------------------------------
 * Protocol loop
 * ------------------------------------------------------------------------ */

loadDocs();
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
            order: 4,
          },
        ],
      });
      publish();
      request("workspace.info", {}, (result) => {
        state.root = result && result.root ? result.root : "";
        publish();
      });
      refreshApi();
    } else if (message.method === "ui.open") {
      request("workspace.info", {}, (result) => {
        state.root = result && result.root ? result.root : "";
        publish();
      });
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
