//! Process supervision and event routing for external plugins.

use crate::manifest::{discover_plugins, LoadedPlugin, PluginManifest};
use crate::protocol::{
    self, InitializeResult, LogParams, UiCloseParams, UiSetParams, MAX_LINE_BYTES, MAX_TREE_BYTES,
    PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

/// Knobs the application adapter may set. Defaults are useful for tests.
#[derive(Clone, Debug)]
pub struct HostOptions {
    pub app_version: String,
    pub data_dir: Option<PathBuf>,
    pub log_dir: Option<PathBuf>,
    pub handshake_timeout: Duration,
    pub shutdown_grace: Duration,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            app_version: env!("CARGO_PKG_VERSION").into(),
            data_dir: None,
            log_dir: None,
            handshake_timeout: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(2),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginReady {
    pub manifest: PluginManifest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceTree {
    pub surface_id: String,
    pub tree: Value,
}

/// Events are drained by the UI adapter; the runtime never waits for the UI.
#[derive(Clone, Debug)]
pub enum HostEvent {
    /// The manifest is valid and the process is starting. This makes a failed
    /// handshake visible in the sidebar instead of silently omitting the plugin.
    Starting {
        plugin: PluginManifest,
    },
    Ready {
        plugin: PluginReady,
        trees: Vec<SurfaceTree>,
    },
    Surface {
        plugin_id: String,
        surface_id: String,
        tree: Value,
    },
    Closed {
        plugin_id: String,
        surface_id: String,
    },
    Offline {
        plugin_id: String,
        reason: String,
    },
    Log {
        plugin_id: String,
        level: String,
        message: String,
    },
    Error {
        plugin_id: Option<String>,
        message: String,
    },
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value, String>>>>>;

pub(crate) struct ProcessHandle {
    manifest: RwLock<PluginManifest>,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    alive: AtomicBool,
    ready: AtomicBool,
    shutting_down: AtomicBool,
}

impl ProcessHandle {
    fn write_line(&self, line: &str) -> Result<(), String> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| "Plugin stdin unavailable".to_string())?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("Cannot write to plugin: {error}"))
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let line = protocol::notification_line(method, params)?;
        self.write_line(&line)
    }

    fn request(&self, id: u64, method: &str, params: Value) -> Result<(), String> {
        let line = protocol::request_line(id, method, params)?;
        self.write_line(&line)
    }

    fn shutdown(&self, grace: Duration) {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.notify("shutdown", json!({}));
        let deadline = Instant::now() + grace;
        loop {
            match self.child.lock() {
                Ok(mut child) => match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {}
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    Err(_) => break,
                },
                Err(_) => break,
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.alive.store(false, Ordering::SeqCst);
    }

    fn terminate(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn is_running(&self) -> bool {
        if !self.alive.load(Ordering::SeqCst) {
            return false;
        }
        self.child
            .lock()
            .map(|mut child| matches!(child.try_wait(), Ok(None)))
            .unwrap_or(false)
    }

    fn surface_known(&self, surface: &str) -> bool {
        self.manifest
            .read()
            .map(|manifest| manifest.ui.iter().any(|entry| entry.id == surface))
            .unwrap_or(false)
    }
}

struct Inner {
    plugins: Mutex<HashMap<String, Arc<ProcessHandle>>>,
    trees: Mutex<HashMap<(String, String), Value>>,
    sender: Sender<HostEvent>,
    next_id: AtomicU64,
    shutdown_grace: Duration,
}

impl Inner {
    fn send(&self, event: HostEvent) {
        let _ = self.sender.send(event);
    }
}

/// A clonable handle to the running plugin set.
#[derive(Clone)]
pub struct PluginRuntime {
    inner: Arc<Inner>,
}

impl PluginRuntime {
    /// Discover and start plugins below `root`.
    ///
    /// Startup is off the caller's thread in the sense that the child process
    /// handshake runs on a supervisor thread; this returns immediately.
    pub fn start(root: impl AsRef<Path>) -> (Self, Receiver<HostEvent>) {
        Self::start_with_options(root, HostOptions::default())
    }

    pub fn start_with_options(
        root: impl AsRef<Path>,
        options: HostOptions,
    ) -> (Self, Receiver<HostEvent>) {
        let (sender, receiver) = mpsc::channel();
        let inner = Arc::new(Inner {
            plugins: Mutex::new(HashMap::new()),
            trees: Mutex::new(HashMap::new()),
            sender,
            next_id: AtomicU64::new(1),
            shutdown_grace: options.shutdown_grace,
        });

        let discovery = discover_plugins(root.as_ref());
        for error in discovery.errors {
            inner.send(HostEvent::Error {
                plugin_id: None,
                message: format!("{}: {}", error.path.display(), error.message),
            });
        }
        for plugin in discovery.plugins {
            let plugin_id = plugin.manifest.id.clone();
            inner.send(HostEvent::Starting {
                plugin: plugin.manifest.clone(),
            });
            if let Err(error) = spawn_plugin(inner.clone(), plugin, &options) {
                inner.send(HostEvent::Error {
                    plugin_id: Some(plugin_id.clone()),
                    message: error.clone(),
                });
                inner.send(HostEvent::Offline {
                    plugin_id,
                    reason: error,
                });
            }
        }

        (Self { inner }, receiver)
    }

    pub fn tree(&self, plugin_id: &str, surface_id: &str) -> Option<Value> {
        self.inner
            .trees
            .lock()
            .ok()?
            .get(&(plugin_id.to_string(), surface_id.to_string()))
            .cloned()
    }

    pub fn is_alive(&self, plugin_id: &str) -> bool {
        self.inner
            .plugins
            .lock()
            .ok()
            .and_then(|plugins| plugins.get(plugin_id).cloned())
            .is_some_and(|process| process.is_running())
    }

    pub fn send_ui_open(&self, plugin_id: &str, surface_id: &str) {
        self.notify(plugin_id, "ui.open", json!({ "surface": surface_id }));
    }

    pub fn send_ui_close(&self, plugin_id: &str, surface_id: &str) {
        self.notify(plugin_id, "ui.close", json!({ "surface": surface_id }));
    }

    pub fn send_ui_event(&self, plugin_id: &str, surface_id: &str, handler: &str, value: Value) {
        self.notify(
            plugin_id,
            "ui.event",
            json!({
                "surface": surface_id,
                "handler": handler,
                "value": value,
            }),
        );
    }

    fn notify(&self, plugin_id: &str, method: &str, params: Value) {
        let process = self
            .inner
            .plugins
            .lock()
            .ok()
            .and_then(|plugins| plugins.get(plugin_id).cloned());
        let Some(process) = process else {
            return;
        };
        if !process.alive.load(Ordering::SeqCst) {
            return;
        }
        if let Err(error) = process.notify(method, params) {
            self.inner.send(HostEvent::Error {
                plugin_id: Some(plugin_id.to_string()),
                message: error,
            });
        }
    }

    /// Ask every plugin to exit, then kill anything still running.
    pub fn shutdown(&self) {
        let processes: Vec<_> = self
            .inner
            .plugins
            .lock()
            .map(|plugins| plugins.values().cloned().collect())
            .unwrap_or_default();
        for process in processes {
            process.shutdown(self.inner.shutdown_grace);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(plugins) = self.plugins.lock() {
            for process in plugins.values() {
                process.terminate();
            }
        }
    }
}

fn spawn_plugin(
    inner: Arc<Inner>,
    plugin: LoadedPlugin,
    options: &HostOptions,
) -> Result<(), String> {
    let LoadedPlugin { manifest, root } = plugin;
    let cwd = manifest
        .cwd
        .as_ref()
        .map_or_else(|| root.clone(), |cwd| root.join(cwd));
    let program = manifest
        .command
        .first()
        .cloned()
        .ok_or_else(|| format!("Plugin {} has an empty command", manifest.id))?;
    let mut command = Command::new(&program);
    command
        .args(manifest.command.iter().skip(1))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        format!(
            "Cannot start plugin {} ({}): {error}",
            manifest.id,
            cwd.display()
        )
    })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("Plugin {} has no stdin pipe", manifest.id))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("Plugin {} has no stdout pipe", manifest.id))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("Plugin {} has no stderr pipe", manifest.id))?;

    let plugin_id = manifest.id.clone();
    let process = Arc::new(ProcessHandle {
        manifest: RwLock::new(manifest),
        stdin: Mutex::new(stdin),
        child: Mutex::new(child),
        alive: AtomicBool::new(true),
        ready: AtomicBool::new(false),
        shutting_down: AtomicBool::new(false),
    });

    let duplicate = {
        let mut plugins = inner
            .plugins
            .lock()
            .map_err(|_| "Plugin registry unavailable".to_string())?;
        if plugins.contains_key(&plugin_id) {
            true
        } else {
            plugins.insert(plugin_id.clone(), process.clone());
            false
        }
    };
    if duplicate {
        process.terminate();
        return Err(format!("Duplicate plugin id: {plugin_id}"));
    }

    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    spawn_stdout_reader(inner.clone(), process.clone(), stdout, pending.clone());
    spawn_stderr_reader(
        inner.clone(),
        process.clone(),
        stderr,
        options.log_dir.clone(),
    );
    let options = options.clone();
    thread::Builder::new()
        .name(format!("plugin-init-{plugin_id}"))
        .spawn(move || handshake(inner, process, pending, options))
        .map_err(|error| format!("Cannot start plugin supervisor: {error}"))?;
    Ok(())
}

fn handshake(
    inner: Arc<Inner>,
    process: Arc<ProcessHandle>,
    pending: Pending,
    options: HostOptions,
) {
    let plugin_id = process
        .manifest
        .read()
        .map(|manifest| manifest.id.clone())
        .unwrap_or_default();
    let request_id = inner.next_id.fetch_add(1, Ordering::SeqCst);
    let (response_tx, response_rx) = mpsc::channel();
    if let Ok(mut waiters) = pending.lock() {
        waiters.insert(request_id, response_tx);
    }
    let params = json!({
        "protocol": PROTOCOL_VERSION,
        "appVersion": options.app_version,
        "dataDir": options.data_dir.as_ref().map(|path| path.to_string_lossy()),
        "logDir": options.log_dir.as_ref().map(|path| path.to_string_lossy()),
    });
    if let Err(error) = process.request(request_id, "initialize", params) {
        fail(&inner, &process, error);
        return;
    }
    let response = match response_rx.recv_timeout(options.handshake_timeout) {
        Ok(response) => response,
        Err(RecvTimeoutError::Timeout) => {
            if let Ok(mut waiters) = pending.lock() {
                waiters.remove(&request_id);
            }
            fail(&inner, &process, "Plugin handshake timed out".into());
            return;
        }
        Err(RecvTimeoutError::Disconnected) => {
            fail(&inner, &process, "Plugin exited during handshake".into());
            return;
        }
    };
    let result = match response {
        Ok(result) => result,
        Err(error) => {
            fail(&inner, &process, error);
            return;
        }
    };
    let result: InitializeResult = match serde_json::from_value(result) {
        Ok(result) => result,
        Err(error) => {
            fail(
                &inner,
                &process,
                format!("Invalid initialize response: {error}"),
            );
            return;
        }
    };
    if result
        .protocol
        .is_some_and(|version| version != PROTOCOL_VERSION)
    {
        fail(
            &inner,
            &process,
            format!(
                "Plugin requires an unsupported protocol: {:?}",
                result.protocol
            ),
        );
        return;
    }

    let mut manifest = match process.manifest.read() {
        Ok(manifest) => manifest.clone(),
        Err(_) => {
            fail(&inner, &process, "Plugin manifest unavailable".into());
            return;
        }
    };
    if let Some(ui) = result.ui {
        manifest.ui = ui;
    }
    if let Err(error) = manifest.validate() {
        fail(&inner, &process, error);
        return;
    }
    if let Ok(mut current) = process.manifest.write() {
        *current = manifest.clone();
    }
    let trees = inner
        .trees
        .lock()
        .map(|trees| {
            trees
                .iter()
                .filter(|((plugin, _), _)| plugin == &plugin_id)
                .map(|((_, surface), tree)| SurfaceTree {
                    surface_id: surface.clone(),
                    tree: tree.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    process.ready.store(true, Ordering::SeqCst);
    inner.send(HostEvent::Ready {
        plugin: PluginReady { manifest },
        trees,
    });
}

fn fail(inner: &Arc<Inner>, process: &Arc<ProcessHandle>, reason: String) {
    let plugin_id = process
        .manifest
        .read()
        .map(|manifest| manifest.id.clone())
        .unwrap_or_default();
    inner.send(HostEvent::Error {
        plugin_id: Some(plugin_id.clone()),
        message: reason.clone(),
    });
    process.shutting_down.store(true, Ordering::SeqCst);
    if process.alive.swap(false, Ordering::SeqCst) {
        inner.send(HostEvent::Offline { plugin_id, reason });
    }
    process.terminate();
}

fn spawn_stdout_reader(
    inner: Arc<Inner>,
    process: Arc<ProcessHandle>,
    stdout: std::process::ChildStdout,
    pending: Pending,
) {
    let plugin_id = process
        .manifest
        .read()
        .map(|manifest| manifest.id.clone())
        .unwrap_or_default();
    let _ = thread::Builder::new()
        .name(format!("plugin-out-{plugin_id}"))
        .spawn(move || {
            read_stdout(inner.clone(), process.clone(), stdout, pending);
            if !process.shutting_down.load(Ordering::SeqCst)
                && process.alive.swap(false, Ordering::SeqCst)
            {
                // A closed or malformed stdout stream ends the protocol
                // session. Reap the child instead of leaving it running
                // invisibly after the surface goes offline.
                process.terminate();
                let reason = "Plugin process exited".to_string();
                let plugin_id = process
                    .manifest
                    .read()
                    .map(|manifest| manifest.id.clone())
                    .unwrap_or_default();
                inner.send(HostEvent::Offline { plugin_id, reason });
            }
        });
}

fn read_stdout(
    inner: Arc<Inner>,
    process: Arc<ProcessHandle>,
    stdout: std::process::ChildStdout,
    pending: Pending,
) {
    let plugin_id = process
        .manifest
        .read()
        .map(|manifest| manifest.id.clone())
        .unwrap_or_default();
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        match read_line_limited(&mut reader, &mut line) {
            Ok(false) => break,
            Ok(true) => {}
            Err(error) => {
                inner.send(HostEvent::Error {
                    plugin_id: Some(plugin_id.clone()),
                    message: error,
                });
                break;
            }
        }
        let line = match std::str::from_utf8(&line) {
            Ok(line) => line.trim_end_matches('\r'),
            Err(error) => {
                inner.send(HostEvent::Error {
                    plugin_id: Some(plugin_id.clone()),
                    message: format!("Plugin output is not UTF-8: {error}"),
                });
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let message = match protocol::parse_incoming(line) {
            Ok(message) => message,
            Err(error) => {
                inner.send(HostEvent::Error {
                    plugin_id: Some(plugin_id.clone()),
                    message: error,
                });
                continue;
            }
        };
        if message.is_response() {
            let id = message.id.unwrap_or_default();
            let waiter = pending
                .lock()
                .ok()
                .and_then(|mut waiters| waiters.remove(&id));
            match waiter {
                Some(waiter) => {
                    let _ = waiter.send(message.response_result());
                }
                None => inner.send(HostEvent::Error {
                    plugin_id: Some(plugin_id.clone()),
                    message: format!("Unexpected response id {id}"),
                }),
            }
            continue;
        }
        let method = message.method.unwrap_or_default();
        let params = message.params.unwrap_or(Value::Null);
        match method.as_str() {
            "ui.set" => {
                let params: UiSetParams = match serde_json::from_value(params) {
                    Ok(params) => params,
                    Err(error) => {
                        inner.send(HostEvent::Error {
                            plugin_id: Some(plugin_id.clone()),
                            message: format!("Invalid ui.set: {error}"),
                        });
                        continue;
                    }
                };
                if !process.surface_known(&params.surface) {
                    inner.send(HostEvent::Error {
                        plugin_id: Some(plugin_id.clone()),
                        message: format!("ui.set references unknown surface {}", params.surface),
                    });
                    continue;
                }
                let bytes = serde_json::to_vec(&params.tree)
                    .map(|bytes| bytes.len())
                    .unwrap_or(usize::MAX);
                if bytes > MAX_TREE_BYTES {
                    inner.send(HostEvent::Error {
                        plugin_id: Some(plugin_id.clone()),
                        message: format!(
                            "UI tree for {} exceeds {MAX_TREE_BYTES} bytes",
                            params.surface
                        ),
                    });
                    continue;
                }
                if let Ok(mut trees) = inner.trees.lock() {
                    trees.insert(
                        (plugin_id.clone(), params.surface.clone()),
                        params.tree.clone(),
                    );
                }
                if process.ready.load(Ordering::SeqCst) {
                    inner.send(HostEvent::Surface {
                        plugin_id: plugin_id.clone(),
                        surface_id: params.surface,
                        tree: params.tree,
                    });
                }
            }
            "ui.close" => {
                let params: UiCloseParams = match serde_json::from_value(params) {
                    Ok(params) => params,
                    Err(error) => {
                        inner.send(HostEvent::Error {
                            plugin_id: Some(plugin_id.clone()),
                            message: format!("Invalid ui.close: {error}"),
                        });
                        continue;
                    }
                };
                if let Ok(mut trees) = inner.trees.lock() {
                    trees.remove(&(plugin_id.clone(), params.surface.clone()));
                }
                if process.ready.load(Ordering::SeqCst) {
                    inner.send(HostEvent::Closed {
                        plugin_id: plugin_id.clone(),
                        surface_id: params.surface,
                    });
                }
            }
            "log" => {
                let params: LogParams = match serde_json::from_value(params) {
                    Ok(params) => params,
                    Err(error) => {
                        inner.send(HostEvent::Error {
                            plugin_id: Some(plugin_id.clone()),
                            message: format!("Invalid log message: {error}"),
                        });
                        continue;
                    }
                };
                let mut message = params.message;
                message.truncate(8192);
                inner.send(HostEvent::Log {
                    plugin_id: plugin_id.clone(),
                    level: params.level,
                    message,
                });
            }
            other => inner.send(HostEvent::Error {
                plugin_id: Some(plugin_id.clone()),
                message: format!("Unsupported plugin method: {other}"),
            }),
        }
    }
}

/// Read one newline-delimited message without letting a plugin allocate an
/// unbounded line. The newline is consumed but excluded from `out`.
fn read_line_limited<R: BufRead>(reader: &mut R, out: &mut Vec<u8>) -> Result<bool, String> {
    out.clear();
    loop {
        let buffer = reader
            .fill_buf()
            .map_err(|error| format!("Cannot read plugin output: {error}"))?;
        if buffer.is_empty() {
            return Ok(!out.is_empty());
        }
        if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            if out.len() + index > MAX_LINE_BYTES {
                return Err(format!("Plugin message exceeds {MAX_LINE_BYTES} bytes"));
            }
            out.extend_from_slice(&buffer[..index]);
            reader.consume(index + 1);
            return Ok(true);
        }
        if out.len() + buffer.len() > MAX_LINE_BYTES {
            return Err(format!("Plugin message exceeds {MAX_LINE_BYTES} bytes"));
        }
        let len = buffer.len();
        out.extend_from_slice(buffer);
        reader.consume(len);
    }
}

fn spawn_stderr_reader(
    inner: Arc<Inner>,
    process: Arc<ProcessHandle>,
    stderr: std::process::ChildStderr,
    log_dir: Option<PathBuf>,
) {
    let plugin_id = process
        .manifest
        .read()
        .map(|manifest| manifest.id.clone())
        .unwrap_or_default();
    let _ = thread::Builder::new()
        .name(format!("plugin-err-{plugin_id}"))
        .spawn(move || {
            let mut log = log_dir.and_then(|dir| {
                let _ = std::fs::create_dir_all(&dir);
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join(format!("{plugin_id}.log")))
                    .ok()
            });
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut line = line;
                line.truncate(8192);
                if let Some(log) = log.as_mut() {
                    let _ = writeln!(log, "{line}");
                    let _ = log.flush();
                }
                inner.send(HostEvent::Log {
                    plugin_id: plugin_id.clone(),
                    level: "stderr".into(),
                    message: line,
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn line_framing_splits_lines_and_rejects_oversized_messages() {
        let mut reader = BufReader::new(Cursor::new(b"one\r\ntwo\n".to_vec()));
        let mut line = Vec::new();
        assert!(read_line_limited(&mut reader, &mut line).unwrap());
        assert_eq!(line, b"one\r");
        assert!(read_line_limited(&mut reader, &mut line).unwrap());
        assert_eq!(line, b"two");
        assert!(!read_line_limited(&mut reader, &mut line).unwrap());

        let oversized = vec![b'a'; MAX_LINE_BYTES + 1];
        let mut reader = BufReader::new(Cursor::new(oversized));
        assert!(read_line_limited(&mut reader, &mut line).is_err());
    }
}
