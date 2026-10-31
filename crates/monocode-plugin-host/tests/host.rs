use monocode_plugin_host::{HostEvent, HostOptions, PluginRuntime};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

fn temp_plugin_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "monocode-plugin-host-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_manifest(root: &Path) {
    let manifest = json!({
        "id": "com.test.echo",
        "name": "Echo",
        "version": "0.1.0",
        "protocol": 1,
        "command": [env!("CARGO_BIN_EXE_test-plugin")],
        "ui": [{
            "id": "main",
            "title": "Test plugin",
            "icon": "sparkles",
            "placements": ["sidebar"]
        }]
    });
    fs::write(root.join("plugin.json"), manifest.to_string()).unwrap();
}

fn texts(tree: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    collect_texts(tree, &mut texts);
    texts
}

fn collect_texts(value: &Value, texts: &mut Vec<String>) {
    if value.get("kind").and_then(Value::as_str) == Some("text") {
        if let Some(text) = value.get("text").and_then(Value::as_str) {
            texts.push(text.to_string());
        }
    }
    if let Some(children) = value.get("children").and_then(Value::as_array) {
        for child in children {
            collect_texts(child, texts);
        }
    }
}

fn wait_for_surface(rx: &Receiver<HostEvent>, surface_id: &str, needle: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = rx
            .recv_timeout(remaining)
            .unwrap_or_else(|error| panic!("Timed out waiting for {needle}: {error}"));
        if let HostEvent::Surface {
            surface_id: actual,
            tree,
            ..
        } = event
        {
            if actual == surface_id && texts(&tree).iter().any(|text| text.contains(needle)) {
                return tree;
            }
        }
    }
}

#[test]
fn starts_handshakes_round_trips_events_and_shuts_down() {
    let root = temp_plugin_dir();
    write_manifest(&root);

    let options = HostOptions {
        handshake_timeout: Duration::from_secs(5),
        shutdown_grace: Duration::from_secs(2),
        ..Default::default()
    };
    let (runtime, rx) = PluginRuntime::start_with_options(&root, options);

    let deadline = Instant::now() + Duration::from_secs(5);
    let manifest = loop {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("plugin should become ready");
        if let HostEvent::Ready { plugin, .. } = event {
            break plugin.manifest;
        }
    };
    assert_eq!(manifest.id, "com.test.echo");
    assert_eq!(manifest.ui[0].id, "main");

    runtime.send_ui_open("com.test.echo", "main");
    let tree = wait_for_surface(&rx, "main", "Count: 0");
    assert!(texts(&tree)
        .iter()
        .any(|text| text == "External plugin says hello"));

    runtime.send_ui_event("com.test.echo", "main", "click", Value::Null);
    wait_for_surface(&rx, "main", "Count: 1");

    runtime.send_ui_event("com.test.echo", "main", "name", json!("Ada"));
    wait_for_surface(&rx, "main", "Name: Ada");

    assert!(runtime.is_alive("com.test.echo"));
    runtime.shutdown();
    let deadline = Instant::now() + Duration::from_secs(2);
    while runtime.is_alive("com.test.echo") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!runtime.is_alive("com.test.echo"));
    let _ = fs::remove_dir_all(root);
}
