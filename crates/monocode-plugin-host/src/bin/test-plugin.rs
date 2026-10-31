//! Deterministic plugin used by the host's integration tests.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut count = 0u64;
    let mut name = String::new();
    let mut host_root = String::new();

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message.get("method").is_none() {
            if message["id"].as_u64() == Some(99) {
                host_root = message["result"]["root"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                send_tree(&mut output, &count, &name, &host_root);
            }
            continue;
        }
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => {
                respond(
                    &mut output,
                    &message,
                    json!({
                        "protocol": 1,
                        "ui": [{
                            "id": "main",
                            "title": "Test plugin",
                            "icon": "sparkles",
                            "placements": ["sidebar"]
                        }]
                    }),
                );
                request_workspace(&mut output);
            }
            "ui.open" => send_tree(&mut output, &count, &name, &host_root),
            "ui.event" => {
                match message["params"]["handler"].as_str().unwrap_or_default() {
                    "click" => count += 1,
                    "name" => {
                        name = message["params"]["value"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                    }
                    _ => {}
                }
                send_tree(&mut output, &count, &name, &host_root);
            }
            "shutdown" => break,
            _ => {}
        }
    }
}

fn request_workspace(output: &mut impl Write) {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "workspace.info",
        "params": {}
    });
    let _ = writeln!(output, "{request}");
    let _ = output.flush();
}

fn respond(output: &mut impl Write, request: &Value, result: Value) {
    let response = json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": result,
    });
    let _ = writeln!(output, "{response}");
    let _ = output.flush();
}

fn send_tree(output: &mut impl Write, count: &u64, name: &str, host_root: &str) {
    let message = json!({
        "jsonrpc": "2.0",
        "method": "ui.set",
        "params": {
            "surface": "main",
            "tree": {
                "kind": "column",
                "gap": 12,
                "children": [
                    {
                        "kind": "text",
                        "text": "External plugin says hello",
                        "size": "title",
                        "color": "ink"
                    },
                    {
                        "kind": "text",
                        "text": format!("Count: {count}"),
                        "size": "body",
                        "color": "muted"
                    },
                    {
                        "kind": "text",
                        "text": format!("Name: {name}"),
                        "size": "body",
                        "color": "muted"
                    },
                    {
                        "kind": "text",
                        "text": format!("Host: {host_root}"),
                        "size": "body",
                        "color": "muted"
                    },
                    {
                        "kind": "input",
                        "label": "Name",
                        "value": name,
                        "placeholder": "Your name",
                        "handler": "name"
                    },
                    {
                        "kind": "button",
                        "label": "Click",
                        "handler": "click",
                        "disabled": false
                    }
                ]
            }
        }
    });
    let _ = writeln!(output, "{message}");
    let _ = output.flush();
}
