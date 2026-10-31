//! A Rust plugin for the Monocode dashboard example.
//!
//! It owns the live metrics and speaks newline-delimited JSON-RPC on stdio.
//! stdout is protocol traffic only; logs go to stderr.

use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufWriter, Write},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

const HISTORY: usize = 24;
const SURFACE: &str = "dashboard";

struct State {
    tick: u64,
    tab: usize,
    live: bool,
    workspace: String,
    requests: Vec<f64>,
    errors: Vec<f64>,
}

impl State {
    fn new() -> Self {
        let requests = (0..HISTORY)
            .map(|i| 220.0 + (i as f64 / 3.0).sin() * 45.0)
            .collect();
        let errors = (0..HISTORY)
            .map(|i| 4.0 + (i as f64 / 2.0).sin() * 1.7)
            .collect();
        Self {
            tick: 0,
            tab: 0,
            live: true,
            workspace: String::new(),
            requests,
            errors,
        }
    }

    fn advance(&mut self) {
        self.tick += 1;
        let requests = self.requests.last().copied().unwrap_or(220.0)
            + ((self.tick as f64 * 1.7).sin() * 32.0);
        let errors =
            self.errors.last().copied().unwrap_or(4.0) + ((self.tick as f64 * 0.9).cos() * 1.3);
        self.requests.push(requests.clamp(80.0, 480.0));
        self.errors.push(errors.clamp(0.2, 18.0));
        self.requests.remove(0);
        self.errors.remove(0);
    }
}

fn text(value: impl Into<String>, size: &str, color: &str) -> Value {
    json!({
        "kind": "text",
        "text": value.into(),
        "size": size,
        "color": color
    })
}

fn card(title: &str, children: Vec<Value>) -> Value {
    let mut all = vec![text(title, "subtitle", "ink")];
    all.extend(children);
    json!({ "kind": "card", "gap": 10, "children": all })
}

fn recent(values: &[f64]) -> Vec<f64> {
    values.iter().rev().take(10).rev().copied().collect()
}

fn overview(state: &State) -> Value {
    let requests = state.requests.last().copied().unwrap_or_default();
    let errors = state.errors.last().copied().unwrap_or_default();
    let latency = 74 + (state.tick * 7) % 51;
    let queue = 2 + state.tick % 7;
    let labels = (0..HISTORY)
        .map(|i| {
            if i == HISTORY - 1 {
                "now".to_string()
            } else {
                format!("-{}", HISTORY - 1 - i)
            }
        })
        .collect::<Vec<_>>();

    let stats = json!({
        "kind": "grid",
        "columns": 4,
        "gap": 12,
        "children": [
            {
                "kind": "stat",
                "label": "Requests / s",
                "value": format!("{requests:.0}"),
                "detail": "live sample",
                "color": "accent",
                "delta": "+4.2%",
                "trend": recent(&state.requests)
            },
            {
                "kind": "stat",
                "label": "Error rate",
                "value": format!("{errors:.2}%"),
                "detail": "target < 1%",
                "color": "negative",
                "delta": "+0.12%",
                "trend": recent(&state.errors),
                "lower_is_better": true
            },
            {
                "kind": "stat",
                "label": "P95 latency",
                "value": format!("{latency} ms"),
                "detail": "rolling window",
                "color": "warning",
                "delta": "-8 ms"
            },
            {
                "kind": "stat",
                "label": "Queue depth",
                "value": format!("{queue}"),
                "detail": "workers 12/16",
                "color": "ink"
            }
        ]
    });

    let charts = json!({
        "kind": "grid",
        "columns": 2,
        "gap": 12,
        "children": [
            card("Throughput", vec![json!({
                "kind": "chart",
                "chart_type": "area",
                "labels": labels,
                "height": 180,
                "series": [
                    { "label": "Requests / s", "values": &state.requests, "color": "accent" },
                    { "label": "Errors x 20", "values": state.errors.iter().map(|v| v * 20.0).collect::<Vec<_>>(), "color": "negative" }
                ]
            })]),
            card("Saturation", vec![
                json!({
                    "kind": "gauge",
                    "label": "CPU",
                    "value": 58 + (state.tick % 23) as i64,
                    "min": 0,
                    "max": 100,
                    "unit": "%",
                    "color": "accent"
                }),
                json!({
                    "kind": "progress",
                    "label": "Queue",
                    "value": queue,
                    "max": 16,
                    "detail": format!("{queue} of 16 workers busy"),
                    "color": "warning"
                }),
                json!({
                    "kind": "donut",
                    "label": "Capacity",
                    "value": "74%",
                    "slices": [
                        { "label": "API", "value": 52, "color": "accent" },
                        { "label": "Workers", "value": 28, "color": "positive" },
                        { "label": "Waiting", "value": 12, "color": "warning" },
                        { "label": "Other", "value": 8, "color": "muted" }
                    ]
                })
            ])
        ]
    });

    json!({
        "kind": "column",
        "gap": 12,
        "children": [stats, charts]
    })
}

fn traffic() -> Value {
    let rows = vec![
        vec!["/v1/sessions", "GET", "86 ms", "0.2%"],
        vec!["/v1/sessions/:id", "GET", "112 ms", "0.4%"],
        vec!["/v1/turns", "POST", "248 ms", "0.7%"],
        vec!["/v1/plugins", "GET", "41 ms", "0.0%"],
        vec!["/v1/health", "GET", "8 ms", "0.0%"],
    ];
    json!({
        "kind": "column",
        "gap": 12,
        "children": [
            card("Routes", vec![json!({
                "kind": "table",
                "columns": ["Route", "Method", "P95", "Errors"],
                "rows": rows
            })]),
            card("Notes", vec![
                text("The Rust process owns this state and publishes a new tree every two seconds.", "body", "muted")
            ])
        ]
    })
}

fn tree(state: &State) -> Value {
    let project = if state.workspace.is_empty() {
        "…".to_string()
    } else {
        state.workspace.clone()
    };
    json!({
        "kind": "column",
        "gap": 16,
        "children": [
            text("Dashboard", "hero", "ink"),
            text("Live metrics from a Rust plugin process.", "body", "muted"),
            {
                "kind": "row",
                "gap": 8,
                "children": [
                    { "kind": "toggle", "label": "Live updates", "value": state.live, "handler": "live" },
                    { "kind": "button", "label": "Sample now", "handler": "refresh", "disabled": false },
                    { "kind": "stat", "label": "Project", "value": project, "color": "ink" }
                ]
            },
            {
                "kind": "tabs",
                "labels": ["Overview", "Traffic"],
                "selected": state.tab,
                "handler": "tab"
            },
            if state.tab == 0 { overview(state) } else { traffic() }
        ]
    })
}

fn write_line(out: &mut impl Write, value: Value) {
    let line = serde_json::to_string(&value).expect("dashboard message should encode");
    writeln!(out, "{line}").expect("dashboard stdout should be writable");
    out.flush().expect("dashboard stdout should flush");
}

fn publish(out: &mut impl Write, state: &State) {
    write_line(
        out,
        json!({
            "jsonrpc": "2.0",
            "method": "ui.set",
            "params": { "surface": SURFACE, "tree": tree(state) }
        }),
    );
}

fn main() {
    let (sender, receiver) = mpsc::channel::<Value>();
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        }
    });

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut state = State::new();

    loop {
        match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(message) => {
                let Some(method) = message["method"].as_str() else {
                    if message["id"].as_u64() == Some(1) {
                        state.workspace = message["result"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        publish(&mut out, &state);
                    }
                    continue;
                };

                match method {
                    "initialize" => {
                        write_line(
                            &mut out,
                            json!({
                                "jsonrpc": "2.0",
                                "id": message["id"],
                                "result": {
                                    "protocol": 1,
                                    "ui": [{
                                        "id": SURFACE,
                                        "title": "Dashboard",
                                        "icon": "dashboard",
                                        "order": 2
                                    }]
                                }
                            }),
                        );
                        publish(&mut out, &state);
                        write_line(
                            &mut out,
                            json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "method": "workspace.info",
                                "params": {}
                            }),
                        );
                    }
                    "ui.open" => publish(&mut out, &state),
                    "ui.event" => {
                        if message["params"]["surface"].as_str() != Some(SURFACE) {
                            continue;
                        }
                        match message["params"]["handler"].as_str().unwrap_or_default() {
                            "tab" => {
                                state.tab = message["params"]["value"]
                                    .as_u64()
                                    .unwrap_or_default()
                                    .min(1) as usize;
                            }
                            "live" => {
                                state.live = message["params"]["value"].as_bool().unwrap_or(true);
                            }
                            "refresh" => {
                                state.advance();
                            }
                            _ => {}
                        }
                        publish(&mut out, &state);
                    }
                    "shutdown" => break,
                    _ => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if state.live {
                    state.advance();
                    publish(&mut out, &state);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}
