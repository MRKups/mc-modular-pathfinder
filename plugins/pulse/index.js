#!/usr/bin/env node
// A live operations dashboard. The plugin process owns the metrics and pushes
// a new native tree every couple of seconds while "Live updates" is on.

import readline from "node:readline";

const rl = readline.createInterface({ input: process.stdin });
const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);
const notify = (method, params) => send({ jsonrpc: "2.0", method, params });
const respond = (id, result) => send({ jsonrpc: "2.0", id, result });

const surface = {
  id: "pulse",
  title: "Pulse",
  icon: "zap",
  placements: ["sidebar"],
};

const HISTORY = 24;
let tick = 0;
let tab = 0;
let live = true;
let requests = Array.from({ length: HISTORY }, (_, i) => 220 + Math.sin(i / 3) * 45);
let errors = Array.from({ length: HISTORY }, (_, i) => 4 + Math.sin(i / 2) * 1.7);
let paused = false;

const labels = Array.from({ length: HISTORY }, (_, i) =>
  i === HISTORY - 1 ? "now" : `-${HISTORY - 1 - i}`,
);

function sample() {
  tick += 1;
  requests.push(
    Math.max(80, Math.min(480, requests.at(-1) + (Math.random() - 0.5) * 55)),
  );
  requests.shift();
  errors.push(
    Math.max(0.2, Math.min(18, errors.at(-1) + (Math.random() - 0.5) * 2.4)),
  );
  errors.shift();
}

function percent(value) {
  return `${value.toFixed(2)}%`;
}

function overview() {
  const current = requests.at(-1);
  const err = errors.at(-1);
  const latency = 74 + ((tick * 7) % 51);
  const queue = 2 + (tick % 7);
  return [
    {
      kind: "grid",
      columns: 4,
      gap: 12,
      children: [
        {
          kind: "stat",
          label: "Requests / s",
          value: current.toFixed(0),
          detail: "1m average",
          color: "accent",
          delta: "+4.2%",
          trend: requests.slice(-10),
        },
        {
          kind: "stat",
          label: "Error rate",
          value: percent(err),
          detail: "target < 1%",
          color: "negative",
          delta: "+0.12%",
          trend: errors.slice(-10),
          lower_is_better: true,
        },
        {
          kind: "stat",
          label: "P95 latency",
          value: `${latency} ms`,
          detail: "rolling window",
          color: "warning",
          delta: "-8 ms",
          trend: Array.from({ length: 10 }, (_, i) => 74 + ((tick + i) * 7) % 51),
        },
        {
          kind: "stat",
          label: "Queue depth",
          value: `${queue}`,
          detail: "workers 12/16",
          color: "ink",
        },
      ],
    },
    {
      kind: "grid",
      columns: 2,
      gap: 12,
      children: [
        {
          kind: "card",
          gap: 10,
          children: [
            {
              kind: "text",
              text: "Throughput",
              size: "subtitle",
              color: "ink",
            },
            {
              kind: "chart",
              chart_type: "area",
              labels,
              height: 180,
              series: [
                {
                  label: "Requests / s",
                  values: requests,
                  color: "accent",
                },
                {
                  label: "Errors x 20",
                  values: errors.map((value) => value * 20),
                  color: "negative",
                },
              ],
            },
          ],
        },
        {
          kind: "card",
          gap: 14,
          children: [
            {
              kind: "text",
              text: "Saturation",
              size: "subtitle",
              color: "ink",
            },
            {
              kind: "gauge",
              label: "CPU",
              value: 58 + (tick % 23),
              min: 0,
              max: 100,
              unit: "%",
              color: "accent",
            },
            {
              kind: "progress",
              label: "Queue",
              value: queue,
              max: 16,
              detail: `${queue} of 16 workers busy`,
              color: "warning",
            },
            {
              kind: "donut",
              slices: [
                { label: "API", value: 52, color: "accent" },
                { label: "Workers", value: 28, color: "positive" },
                { label: "Waiting", value: 12, color: "warning" },
                { label: "Other", value: 8, color: "muted" },
              ],
              label: "Capacity",
              value: "74%",
            },
          ],
        },
      ],
    },
  ];
}

function traffic() {
  const topRoutes = [
    ["/v1/sessions", "GET", "86 ms", "0.2%"],
    ["/v1/sessions/:id", "GET", "112 ms", "0.4%"],
    ["/v1/turns", "POST", "248 ms", "0.7%"],
    ["/v1/plugins", "GET", "41 ms", "0.0%"],
    ["/v1/health", "GET", "8 ms", "0.0%"],
  ];
  return [
    {
      kind: "card",
      gap: 10,
      children: [
        {
          kind: "text",
          text: "Traffic by hour",
          size: "subtitle",
          color: "ink",
        },
        {
          kind: "heatmap",
          rows: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
          columns: ["00", "04", "08", "12", "16", "20"],
          color: "accent",
          values: Array.from({ length: 7 }, (_, row) =>
            Array.from({ length: 6 }, (_, column) =>
              Math.round(
                20 +
                  60 *
                    Math.abs(
                      Math.sin((row + 1) * 0.8 + column * 0.6 + tick * 0.05),
                    ),
              ),
            ),
          ),
        },
      ],
    },
    {
      kind: "card",
      gap: 10,
      children: [
        {
          kind: "text",
          text: "Top routes",
          size: "subtitle",
          color: "ink",
        },
        {
          kind: "table",
          columns: ["Route", "Method", "P95", "Errors"],
          rows: topRoutes,
        },
      ],
    },
    {
      kind: "progress",
      label: "Error budget remaining",
      value: Math.max(2, 82 - tick),
      max: 100,
      detail: "Rolling 30-day budget",
      color: "positive",
    },
  ];
}

function tree() {
  const status = live ? "Live updates on" : "Live updates paused";
  return {
    kind: "column",
    gap: 16,
    children: [
      {
        kind: "text",
        text: "Pulse",
        size: "hero",
        color: "ink",
      },
      {
        kind: "text",
        text: "A live dashboard owned by a separate process. Every update below crosses stdio before it is drawn by Monocode.",
        size: "body",
        color: "muted",
      },
      {
        kind: "row",
        gap: 10,
        children: [
          {
            kind: "text",
            text: `${status} · ${paused ? "paused" : "last sample just now"}`,
            size: "small",
            color: live ? "positive" : "faint",
          },
          {
            kind: "button",
            label: "Refresh snapshot",
            handler: "refresh",
            disabled: false,
          },
          {
            kind: "toggle",
            label: "Live updates",
            value: live,
            handler: "live",
          },
        ],
      },
      {
        kind: "tabs",
        labels: ["Overview", "Traffic"],
        selected: tab,
        handler: "tab",
      },
      {
        kind: "column",
        gap: 12,
        children: tab === 0 ? overview() : traffic(),
      },
    ],
  };
}

function publish() {
  paused = false;
  notify("ui.set", { surface: "pulse", tree: tree() });
}

const timer = setInterval(() => {
  if (!live) {
    paused = true;
    return;
  }
  sample();
  publish();
}, 2200);

rl.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    process.stderr.write(`pulse plugin: invalid JSON: ${error}\n`);
    return;
  }

  switch (message.method) {
    case "initialize":
      respond(message.id, { protocol: 1, ui: [surface] });
      notify("log", { level: "info", message: "pulse dashboard online" });
      publish();
      break;
    case "ui.open":
      publish();
      break;
    case "ui.event":
      if (message.params.surface !== "pulse") break;
      switch (message.params.handler) {
        case "tab":
          tab = Math.max(0, Math.min(1, Number(message.params.value) || 0));
          break;
        case "live":
          live = Boolean(message.params.value);
          break;
        case "refresh":
          sample();
          break;
      }
      publish();
      break;
    case "shutdown":
      clearInterval(timer);
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
