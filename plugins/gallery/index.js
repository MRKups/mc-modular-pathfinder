#!/usr/bin/env node
// A visual reference for the native plugin component tree. Every control here
// is declarative data rendered by Monocode; this process only owns the state.

import readline from "node:readline";

const rl = readline.createInterface({ input: process.stdin });
const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);
const notify = (method, params) => send({ jsonrpc: "2.0", method, params });
const respond = (id, result) => send({ jsonrpc: "2.0", id, result });

const surface = {
  id: "gallery",
  title: "Component gallery",
  icon: "sparkles",
  placements: ["sidebar"],
};

const sections = ["Type", "Data", "Controls", "Canvas"];
const themes = [
  "ink",
  "muted",
  "faint",
  "accent",
  "positive",
  "negative",
  "warning",
  "series1",
  "series2",
  "series3",
  "series4",
  "series5",
  "series6",
];
const choices = ["Accent", "Positive", "Negative", "Warning"];

let section = 0;
let name = "";
let choice = 0;
let amount = 42;
let notifyEnabled = true;
let clicks = 0;

function text(value, size = "body", color = "ink") {
  return { kind: "text", text: value, size, color };
}

function card(title, children) {
  return {
    kind: "card",
    gap: 12,
    children: [text(title, "subtitle"), ...children],
  };
}

function typography() {
  return [
    {
      kind: "grid",
      columns: 2,
      gap: 12,
      children: [
        card("Type scale", [
          text("Hero — 34px", "hero"),
          text("Title — 22px", "title"),
          text("Subtitle — 17px", "subtitle"),
          text("Body — 14px", "body"),
          text("Small — 12px", "small"),
          text("Caption — 11px", "caption"),
        ]),
        card(
          "Colors",
          themes.map((color) => text(color, "body", color)),
        ),
      ],
    },
    card("Stats", [
      {
        kind: "grid",
        columns: 3,
        gap: 12,
        children: [
          {
            kind: "stat",
            label: "Revenue",
            value: "$48.2K",
            detail: "this month",
            color: "accent",
            delta: "+12.4%",
            trend: [12, 15, 13, 19, 22, 21, 28, 32],
          },
          {
            kind: "stat",
            label: "Latency",
            value: "86 ms",
            detail: "p95",
            color: "warning",
            delta: "-8 ms",
            trend: [110, 104, 108, 98, 101, 94, 90, 86],
            lower_is_better: true,
          },
          {
            kind: "stat",
            label: "Errors",
            value: "0.4%",
            detail: "last 24h",
            color: "negative",
            delta: "+0.1%",
            trend: [0.2, 0.3, 0.25, 0.4, 0.35, 0.5, 0.45, 0.4],
            lower_is_better: true,
          },
        ],
      },
    ]),
  ];
}

function data() {
  return [
    {
      kind: "grid",
      columns: 2,
      gap: 12,
      children: [
        card("Bar chart", [
          {
            kind: "chart",
            chart_type: "bar",
            labels: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
            height: 200,
            series: [
              {
                label: "Desktop",
                values: [42, 51, 48, 62, 70, 38, 31],
                color: "accent",
              },
              {
                label: "Mobile",
                values: [18, 22, 26, 29, 34, 27, 21],
                color: "series2",
              },
            ],
          },
        ]),
        card("Area chart", [
          {
            kind: "chart",
            chart_type: "area",
            labels: ["w1", "w2", "w3", "w4", "w5", "w6"],
            height: 200,
            series: [
              {
                label: "Active",
                values: [120, 168, 154, 210, 238, 260],
                color: "accent",
              },
              {
                label: "Returning",
                values: [60, 74, 90, 108, 120, 142],
                color: "series3",
              },
            ],
          },
        ]),
      ],
    },
    {
      kind: "grid",
      columns: 3,
      gap: 12,
      children: [
        card("Gauge", [
          {
            kind: "gauge",
            label: "CPU",
            value: 68,
            min: 0,
            max: 100,
            unit: "%",
            color: "accent",
          },
        ]),
        card("Donut", [
          {
            kind: "donut",
            label: "Requests",
            value: "12.4K",
            slices: [
              { label: "API", value: 54, color: "accent" },
              { label: "Web", value: 28, color: "positive" },
              { label: "CLI", value: 12, color: "warning" },
              { label: "Other", value: 6, color: "muted" },
            ],
          },
        ]),
        card("Progress", [
          {
            kind: "progress",
            label: "Build",
            value: 68,
            max: 100,
            detail: "34 of 50 steps",
            color: "positive",
          },
          {
            kind: "progress",
            label: "Storage",
            value: 82,
            max: 100,
            detail: "8.2 of 10 GB",
            color: "warning",
          },
        ]),
      ],
    },
    {
      kind: "grid",
      columns: 2,
      gap: 12,
      children: [
        card("Heatmap", [
          {
            kind: "heatmap",
            rows: ["Mon", "Tue", "Wed", "Thu", "Fri"],
            columns: ["00", "06", "12", "18", "24"],
            color: "accent",
            values: [
              [12, 28, 64, 48, 22],
              [18, 34, 72, 54, 26],
              [16, 30, 68, 51, 24],
              [20, 38, 80, 60, 30],
              [22, 42, 86, 66, 34],
            ],
          },
        ]),
        card("Table", [
          {
            kind: "table",
            columns: ["Name", "Status", "Requests", "P95"],
            rows: [
              ["gateway", "healthy", "8,412", "42 ms"],
              ["sessions", "healthy", "5,208", "86 ms"],
              ["turns", "degraded", "2,104", "248 ms"],
              ["plugins", "healthy", "318", "12 ms"],
            ],
          },
        ]),
      ],
    },
  ];
}

function controls() {
  return [
    {
      kind: "grid",
      columns: 2,
      gap: 12,
      children: [
        card("Inputs", [
          {
            kind: "input",
            label: "Display name",
            value: name,
            placeholder: "Type a name",
            handler: "name",
          },
          {
            kind: "select",
            label: "Tone",
            options: choices,
            selected: choice,
            handler: "choice",
          },
          {
            kind: "slider",
            label: "Intensity",
            value: amount,
            min: 0,
            max: 100,
            step: 1,
            handler: "amount",
          },
          {
            kind: "toggle",
            label: "Notifications",
            value: notifyEnabled,
            handler: "notify",
          },
        ]),
        card("State", [
          text(`Name: ${name || "—"}`, "body", "ink"),
          text(`Tone: ${choices[choice]}`, "body", "accent"),
          text(`Intensity: ${amount}`, "body", "warning"),
          text(
            `Notifications: ${notifyEnabled ? "on" : "off"}`,
            "body",
            notifyEnabled ? "positive" : "muted",
          ),
          text(`Button clicks: ${clicks}`, "body", "ink"),
          {
            kind: "row",
            gap: 10,
            children: [
              {
                kind: "button",
                label: "Increment",
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
        ]),
      ],
    },
    {
      kind: "card",
      gap: 10,
      children: [
        text("Every interaction is a ui.event sent to this process", "body", "muted"),
        {
          kind: "tabs",
          labels: sections,
          selected: section,
          handler: "section",
        },
      ],
    },
  ];
}

function canvas() {
  return [
    card("Shapes", [
      {
        kind: "canvas",
        width: 720,
        height: 280,
        shapes: [
          {
            kind: "rect",
            x: 24,
            y: 30,
            width: 180,
            height: 100,
            color: "accent",
          },
          {
            kind: "circle",
            x: 300,
            y: 80,
            radius: 46,
            color: "positive",
          },
          {
            kind: "line",
            x1: 24,
            y1: 170,
            x2: 210,
            y2: 220,
            width: 3,
            color: "warning",
          },
          {
            kind: "polyline",
            points: [
              [250, 220],
              [310, 170],
              [370, 200],
              [430, 140],
              [490, 180],
              [550, 120],
            ],
            width: 3,
            color: "series3",
          },
          {
            kind: "label",
            x: 24,
            y: 248,
            text: "Native canvas shapes",
            color: "ink",
          },
        ],
      },
      text(
        "Canvas shapes are geometry data, not images or SVG. They share the app's theme colors.",
        "small",
        "muted",
      ),
    ]),
  ];
}

function tree() {
  const bodies = [typography, data, controls, canvas];
  return {
    kind: "column",
    gap: 16,
    children: [
      text("Component gallery", "hero"),
      text(
        "Everything below is one declarative tree produced by an external process and rendered natively.",
        "body",
        "muted",
      ),
      {
        kind: "tabs",
        labels: sections,
        selected: section,
        handler: "section",
      },
      {
        kind: "column",
        gap: 12,
        children: bodies[section](),
      },
    ],
  };
}

function publish() {
  notify("ui.set", { surface: "gallery", tree: tree() });
}

rl.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    process.stderr.write(`gallery plugin: invalid JSON: ${error}\n`);
    return;
  }

  switch (message.method) {
    case "initialize":
      respond(message.id, { protocol: 1, ui: [surface] });
      notify("log", { level: "info", message: "component gallery ready" });
      publish();
      break;
    case "ui.open":
      publish();
      break;
    case "ui.event": {
      if (message.params.surface !== "gallery") break;
      const handler = String(message.params.handler);
      const value = message.params.value;
      if (handler === "section") {
        section = Math.max(0, Math.min(sections.length - 1, Number(value) || 0));
      } else if (handler === "name") {
        name = typeof value === "string" ? value.slice(0, 120) : "";
      } else if (handler === "choice") {
        choice = Math.max(0, Math.min(choices.length - 1, Number(value) || 0));
      } else if (handler === "amount") {
        amount = Math.max(0, Math.min(100, Number(value) || 0));
      } else if (handler === "notify") {
        notifyEnabled = Boolean(value);
      } else if (handler === "click") {
        clicks += 1;
      } else if (handler === "reset") {
        name = "";
        choice = 0;
        amount = 42;
        notifyEnabled = true;
        clicks = 0;
      }
      publish();
      break;
    }
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
