#!/usr/bin/env node
// A minimal Monocode plugin: one process, JSON-RPC over stdin/stdout.
//
// The host owns the window and renders the native tree this process returns.
// This process owns the state; UI trees and events are the only interface.

import readline from "node:readline";

const rl = readline.createInterface({ input: process.stdin });
const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);
const notify = (method, params) =>
  send({ jsonrpc: "2.0", method, params });
const respond = (id, result) =>
  send({ jsonrpc: "2.0", id, result });

let clicks = 0;
let name = "";
let workspace = "";

const surface = {
  id: "hello",
  title: "Hello",
  icon: "sparkles",
  placements: ["sidebar"],
};

function tree() {
  return {
    kind: "column",
    gap: 16,
    children: [
      {
        kind: "text",
        text: "Hello from a separate process",
        size: "title",
        color: "ink",
      },
      {
        kind: "text",
        text: "This UI is declarative data. Monocode draws it natively and sends clicks and typing back over stdin/stdout.",
        size: "body",
        color: "muted",
      },
      {
        kind: "row",
        gap: 12,
        children: [
          {
            kind: "stat",
            label: "Clicks",
            value: String(clicks),
            color: "accent",
          },
          {
            kind: "stat",
            label: "Name",
            value: name || "—",
            color: "ink",
          },
          {
            kind: "stat",
            label: "Project",
            value: workspace || "…",
            color: "ink",
          },
        ],
      },
      {
        kind: "card",
        gap: 12,
        children: [
          {
            kind: "text",
            text: "Try the native controls",
            size: "subtitle",
            color: "ink",
          },
          {
            kind: "input",
            label: "Your name",
            value: name,
            placeholder: "Type your name",
            handler: "name",
          },
          {
            kind: "row",
            gap: 8,
            children: [
              {
                kind: "button",
                label: "Click me",
                handler: "click",
                disabled: false,
              },
              {
                kind: "button",
                label: "Reset",
                handler: "reset",
                disabled: false,
              },
            ],
          },
        ],
      },
    ],
  };
}

function publish() {
  notify("ui.set", { surface: "hello", tree: tree() });
}

rl.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    process.stderr.write(`hello plugin: invalid JSON: ${error}\n`);
    return;
  }

  if (message.method === undefined && message.id !== undefined) {
    if (message.id === 1 && message.result) {
      workspace = message.result.name || "";
      publish();
    }
    return;
  }

  switch (message.method) {
    case "initialize":
      respond(message.id, { protocol: 1, ui: [surface] });
      notify("log", { level: "info", message: "hello plugin initialized" });
      publish();
      send({
        jsonrpc: "2.0",
        id: 1,
        method: "workspace.info",
        params: {},
      });
      break;
    case "ui.open":
      publish();
      break;
    case "ui.close":
      break;
    case "ui.event":
      if (message.params.surface !== "hello") break;
      switch (message.params.handler) {
        case "click":
          clicks += 1;
          break;
        case "reset":
          clicks = 0;
          name = "";
          break;
        case "name":
          name =
            typeof message.params.value === "string"
              ? message.params.value.slice(0, 200)
              : "";
          break;
      }
      publish();
      break;
    case "shutdown":
      rl.close();
      process.exit(0);
      break;
    default:
      if (message.id !== undefined) {
        send({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32601, message: `Unknown method: ${message.method}` },
        });
      }
  }
});
