#!/usr/bin/env node
// An interactive task board. Demonstrates dynamic lists, inputs, selects,
// toggles and per-item handlers with plugin-owned state.

import readline from "node:readline";

const rl = readline.createInterface({ input: process.stdin });
const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);
const notify = (method, params) => send({ jsonrpc: "2.0", method, params });
const respond = (id, result) => send({ jsonrpc: "2.0", id, result });

const surface = {
  id: "tasks",
  title: "Tasks",
  icon: "note",
  placements: ["sidebar"],
};

const filters = ["All", "Open", "Done"];

let tasks = [
  { title: "Design the plugin contract", done: true, priority: "High" },
  { title: "Ship the native surface", done: true, priority: "High" },
  { title: "Exercise every control", done: false, priority: "Medium" },
  { title: "Write a third-party plugin", done: false, priority: "Low" },
];
let draft = "";
let filter = 0;

function counts() {
  const done = tasks.filter((task) => task.done).length;
  return { done, open: tasks.length - done, total: tasks.length };
}

function visibleTasks() {
  return tasks
    .map((task, index) => ({ task, index }))
    .filter(({ task }) => {
      if (filter === 1) return !task.done;
      if (filter === 2) return task.done;
      return true;
    });
}

function taskRows() {
  const visible = visibleTasks();
  if (visible.length === 0) {
    return [
      {
        kind: "text",
        text: filter === 0 ? "No tasks yet. Add one above." : "Nothing in this filter.",
        size: "body",
        color: "faint",
      },
    ];
  }
  return visible.map(({ task, index }) => ({
    kind: "row",
    gap: 10,
    children: [
      {
        kind: "toggle",
        label: `${task.title}  ·  ${task.priority}`,
        value: task.done,
        handler: `toggle:${index}`,
      },
      {
        kind: "button",
        label: "Remove",
        handler: `remove:${index}`,
        disabled: false,
      },
    ],
  }));
}

function tree() {
  const { done, open, total } = counts();
  const completion = total === 0 ? 0 : Math.round((done / total) * 100);
  return {
    kind: "column",
    gap: 16,
    children: [
      {
        kind: "text",
        text: "Task board",
        size: "hero",
        color: "ink",
      },
      {
        kind: "text",
        text: "The list lives in the plugin process. Toggling, filtering and removing are all round-trips through the host.",
        size: "body",
        color: "muted",
      },
      {
        kind: "grid",
        columns: 3,
        gap: 12,
        children: [
          {
            kind: "stat",
            label: "Open",
            value: String(open),
            detail: "waiting",
            color: "warning",
          },
          {
            kind: "stat",
            label: "Done",
            value: String(done),
            detail: "completed",
            color: "positive",
          },
          {
            kind: "stat",
            label: "Completion",
            value: `${completion}%`,
            detail: `${done} of ${total}`,
            color: "accent",
            trend: [0, completion * 0.25, completion * 0.6, completion],
          },
        ],
      },
      {
        kind: "card",
        gap: 12,
        children: [
          {
            kind: "text",
            text: "Add a task",
            size: "subtitle",
            color: "ink",
          },
          {
            kind: "row",
            gap: 10,
            children: [
              {
                kind: "input",
                label: "New task",
                value: draft,
                placeholder: "What needs doing?",
                handler: "draft",
              },
              {
                kind: "button",
                label: "Add task",
                handler: "add",
                disabled: draft.trim().length === 0,
              },
            ],
          },
          {
            kind: "row",
            gap: 10,
            children: [
              {
                kind: "select",
                label: "Filter",
                options: filters,
                selected: filter,
                handler: "filter",
              },
              {
                kind: "button",
                label: "Clear completed",
                handler: "clear-done",
                disabled: done === 0,
              },
            ],
          },
        ],
      },
      {
        kind: "card",
        gap: 8,
        children: taskRows(),
      },
      {
        kind: "progress",
        label: "Completion",
        value: completion,
        max: 100,
        detail: `${open} open`,
        color: "accent",
      },
    ],
  };
}

function publish() {
  notify("ui.set", { surface: "tasks", tree: tree() });
}

rl.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    process.stderr.write(`tasks plugin: invalid JSON: ${error}\n`);
    return;
  }

  switch (message.method) {
    case "initialize":
      respond(message.id, { protocol: 1, ui: [surface] });
      notify("log", { level: "info", message: "task board ready" });
      publish();
      break;
    case "ui.open":
      publish();
      break;
    case "ui.event": {
      if (message.params.surface !== "tasks") break;
      const handler = String(message.params.handler);
      const value = message.params.value;
      if (handler === "draft") {
        draft = typeof value === "string" ? value.slice(0, 200) : "";
      } else if (handler === "add") {
        const title = draft.trim();
        if (title) {
          tasks.push({ title, done: false, priority: "Medium" });
          draft = "";
        }
      } else if (handler === "filter") {
        filter = Math.max(0, Math.min(filters.length - 1, Number(value) || 0));
      } else if (handler === "clear-done") {
        tasks = tasks.filter((task) => !task.done);
      } else if (handler.startsWith("toggle:")) {
        const index = Number(handler.slice("toggle:".length));
        if (tasks[index]) tasks[index].done = Boolean(value);
      } else if (handler.startsWith("remove:")) {
        const index = Number(handler.slice("remove:".length));
        if (tasks[index]) tasks.splice(index, 1);
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
