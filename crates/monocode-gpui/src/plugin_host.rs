//! Thin GPUI adapter over the standalone plugin runtime.
//!
//! The runtime crate owns discovery, child processes, JSON-RPC, and surface
//! state. This module validates incoming UI trees against Monocode's native
//! component schema, owns `VisualView`s, and renders the active plugin surface.
//! Nothing here runs plugin code on the GPUI thread.

mod api;

use crate::{
    runtime::{Database, locked},
    theme::{accent_color, content, icon, icon_button},
    visuals::{RemoteEvent, VisualView},
};
use api::HostApi;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, ParentElement, Render, Role,
    SharedString, Styled, Task, Window, div, prelude::*, px,
};
use monocode_core::visual::Node;
use monocode_plugin_host::{HostEvent, HostOptions, PluginManifest, PluginRuntime};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::mpsc::Receiver,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// Where plugins are loaded from. The app is unchanged when this is `None`.
pub(crate) struct PluginLocation {
    pub root: PathBuf,
    pub data_dir: Option<PathBuf>,
}

pub(crate) fn plugin_directory(store: &Database) -> Option<PluginLocation> {
    let data_dir = locked(store)
        .ok()
        .and_then(|store| store.data_dir().map(std::path::Path::to_path_buf));
    if let Some(root) = std::env::var_os("MONOCODE_PLUGIN_DIR") {
        return Some(PluginLocation {
            root: PathBuf::from(root),
            data_dir,
        });
    }
    let root = data_dir.as_ref()?.join("plugins");
    root.is_dir().then_some(PluginLocation { root, data_dir })
}

#[derive(Clone, Debug)]
pub(crate) struct SidebarPlugin {
    pub key: String,
    pub plugin_id: String,
    pub surface_id: String,
    pub label: String,
    pub icon: String,
    pub order: Option<i64>,
    pub selected: bool,
    pub offline: bool,
}

struct PluginEntry {
    manifest: PluginManifest,
    offline: bool,
    error: Option<String>,
}

struct ActivePlugin {
    title: String,
    icon: String,
    plugin: String,
    offline: bool,
    error: Option<String>,
}

pub(crate) struct PluginSurfaceHost {
    runtime: PluginRuntime,
    events: Receiver<HostEvent>,
    host_api: HostApi,
    shell: Option<gpui::WeakEntity<crate::shell::Shell>>,
    plugins: HashMap<String, PluginEntry>,
    views: HashMap<String, Entity<VisualView>>,
    /// Native visuals only accept numeric callback ids. Plugins get readable
    /// handler names; each surface's tree is rewritten and mapped back here.
    handler_maps: HashMap<String, Rc<RefCell<HashMap<String, String>>>>,
    surface_errors: HashMap<String, String>,
    active: Option<String>,
    error: Option<String>,
    _pump: Option<Task<()>>,
}

fn surface_key(plugin_id: &str, surface_id: &str) -> String {
    format!("{plugin_id}\0{surface_id}")
}

fn rewrite_handler(handler: &mut String, map: &mut HashMap<String, String>) {
    let original = std::mem::take(handler);
    let id = map.len().to_string();
    map.insert(id.clone(), original);
    *handler = id;
}

fn rewrite_handlers(node: &mut Node, map: &mut HashMap<String, String>) {
    match node {
        Node::Button { handler, .. }
        | Node::Toggle { handler, .. }
        | Node::Select { handler, .. }
        | Node::Slider { handler, .. }
        | Node::Tabs { handler, .. }
        | Node::PageTabs { handler, .. } => rewrite_handler(handler, map),
        Node::Input {
            handler,
            submit_handler,
            ..
        } => {
            rewrite_handler(handler, map);
            if let Some(submit_handler) = submit_handler {
                rewrite_handler(submit_handler, map);
            }
        }
        Node::CodeEditor {
            handler,
            submit_handler,
            selection_handler,
            context_menu,
            ..
        } => {
            rewrite_handler(handler, map);
            if let Some(submit_handler) = submit_handler {
                rewrite_handler(submit_handler, map);
            }
            if let Some(selection_handler) = selection_handler {
                rewrite_handler(selection_handler, map);
            }
            for item in context_menu {
                rewrite_handler(&mut item.handler, map);
            }
        }
        Node::Table {
            handler: Some(handler),
            ..
        } => rewrite_handler(handler, map),
        _ => {}
    }
    match node {
        Node::Column { children, .. }
        | Node::Row { children, .. }
        | Node::Grid { children, .. }
        | Node::Card { children, .. }
        | Node::PageTabs { children, .. } => {
            for child in children {
                rewrite_handlers(child, map);
            }
        }
        _ => {}
    }
}

fn plugin_icon(name: &str) -> &'static str {
    match name {
        "inbox" => "inbox",
        "note" | "notes" => "note",
        "zap" | "automation" => "zap",
        "search" => "search",
        "terminal" => "terminal",
        "git" => "git",
        "bug" | "debug" => "bug",
        "dashboard" => "dashboard",
        "clock" | "timer" => "clock",
        "plug" => "plug",
        _ => "sparkles",
    }
}

impl PluginSurfaceHost {
    #[cfg(test)]
    pub(crate) fn with_surface(
        manifest: PluginManifest,
        tree: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = std::env::temp_dir().join(format!(
            "monocode-plugin-adapter-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (runtime, events) = PluginRuntime::start(&root);
        let mut this = Self {
            runtime,
            events,
            host_api: HostApi::new(root.clone(), None),
            shell: None,
            plugins: HashMap::new(),
            views: HashMap::new(),
            handler_maps: HashMap::new(),
            surface_errors: HashMap::new(),
            active: None,
            error: None,
            _pump: None,
        };
        let surface_id = manifest
            .ui
            .first()
            .map(|surface| surface.id.clone())
            .unwrap_or_default();
        this.apply(
            HostEvent::Starting {
                plugin: manifest.clone(),
            },
            cx,
        );
        this.apply(
            HostEvent::Ready {
                plugin: monocode_plugin_host::PluginReady { manifest },
                trees: vec![monocode_plugin_host::SurfaceTree { surface_id, tree }],
            },
            cx,
        );
        this
    }

    pub(crate) fn new(
        location: PluginLocation,
        workspace_root: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let log_dir = location
            .data_dir
            .as_ref()
            .map(|dir| dir.join("plugin-logs"));
        let host_api = HostApi::new(workspace_root, location.data_dir.clone())
            .with_plugin_dir(location.root.clone());
        let options = HostOptions {
            app_version: env!("CARGO_PKG_VERSION").into(),
            data_dir: location.data_dir,
            log_dir,
            ..Default::default()
        };
        let (runtime, events) = PluginRuntime::start_with_options(&location.root, options);
        let shutdown = runtime.clone();
        // App-level so it still runs if this entity is ever released early.
        let app: &mut gpui::App = cx;
        app.on_app_quit(move |_| {
            let runtime = shutdown.clone();
            async move {
                runtime.shutdown();
            }
        })
        .detach();

        let mut this = Self {
            runtime,
            events,
            host_api,
            shell: None,
            plugins: HashMap::new(),
            views: HashMap::new(),
            handler_maps: HashMap::new(),
            surface_errors: HashMap::new(),
            active: None,
            error: None,
            _pump: None,
        };
        this._pump = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                if this.update(cx, |this, cx| this.drain(cx)).is_err() {
                    break;
                }
            }
        }));
        this
    }

    pub(crate) fn with_shell(mut self, shell: gpui::WeakEntity<crate::shell::Shell>) -> Self {
        self.shell = Some(shell);
        self
    }

    fn drain(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed |= self.apply(event, cx);
        }
        if changed {
            cx.notify();
        }
    }

    fn apply(&mut self, event: HostEvent, cx: &mut Context<Self>) -> bool {
        match event {
            HostEvent::Starting { plugin } => {
                self.plugins.insert(
                    plugin.id.clone(),
                    PluginEntry {
                        manifest: plugin,
                        offline: false,
                        error: None,
                    },
                );
                true
            }
            HostEvent::Ready { plugin, trees } => {
                let id = plugin.manifest.id.clone();
                self.plugins.insert(
                    id.clone(),
                    PluginEntry {
                        manifest: plugin.manifest,
                        offline: false,
                        error: None,
                    },
                );
                for tree in trees {
                    self.set_tree(&id, &tree.surface_id, tree.tree, cx);
                }
                // A click may land while the handshake is still completing.
                // Once the plugin is ready, ask it for the active surface again.
                if let Some((plugin_id, surface_id)) =
                    self.active.as_deref().and_then(|key| key.split_once('\0'))
                    && plugin_id == id
                {
                    self.runtime.send_ui_open(plugin_id, surface_id);
                }
                true
            }
            HostEvent::Surface {
                plugin_id,
                surface_id,
                tree,
            } => {
                self.set_tree(&plugin_id, &surface_id, tree, cx);
                true
            }
            HostEvent::Closed {
                plugin_id,
                surface_id,
            } => {
                let key = surface_key(&plugin_id, &surface_id);
                self.views.remove(&key);
                self.handler_maps.remove(&key);
                self.surface_errors.remove(&key);
                if self.active.as_deref() == Some(key.as_str()) {
                    self.active = None;
                }
                true
            }
            HostEvent::Call {
                plugin_id,
                id,
                method,
                params,
            } => {
                if method == "llm.providers" {
                    let result = self.host_api.providers(cx);
                    self.runtime.respond(&plugin_id, id, result);
                } else if method == "llm.complete" {
                    match self.host_api.prepare_completion(cx, params) {
                        Ok(completion) => {
                            let runtime = self.runtime.clone();
                            let provider = completion.provider.clone();
                            let model = completion.model.clone();
                            std::thread::spawn(move || {
                                let result = monocode_core::provider::run_text(
                                    completion.request,
                                    Arc::new(AtomicBool::new(false)),
                                )
                                .map(|text| {
                                    serde_json::json!({
                                        "text": text,
                                        "provider": provider,
                                        "model": model,
                                    })
                                });
                                runtime.respond(&plugin_id, id, result);
                            });
                        }
                        Err(error) => self.runtime.respond(&plugin_id, id, Err(error)),
                    }
                } else if method == "session.start" {
                    match self.host_api.prepare_session_start(params) {
                        Ok(start) => {
                            let Some(shell) = self.shell.clone() else {
                                self.runtime.respond(
                                    &plugin_id,
                                    id,
                                    Err("No workspace shell is attached".into()),
                                );
                                return false;
                            };
                            let runtime = self.runtime.clone();
                            cx.defer(move |cx| {
                                let fallback_runtime = runtime.clone();
                                let fallback_plugin = plugin_id.clone();
                                if shell
                                    .update(cx, |shell, cx| {
                                        shell.start_plugin_session(
                                            plugin_id,
                                            id,
                                            start.title,
                                            start.prompt,
                                            runtime,
                                            cx,
                                        );
                                    })
                                    .is_err()
                                {
                                    fallback_runtime.respond(
                                        &fallback_plugin,
                                        id,
                                        Err("Workspace shell is unavailable".into()),
                                    );
                                }
                            });
                        }
                        Err(error) => self.runtime.respond(&plugin_id, id, Err(error)),
                    }
                } else {
                    let result = self.host_api.call(&plugin_id, &method, params);
                    self.runtime.respond(&plugin_id, id, result);
                }
                false
            }
            HostEvent::Offline { plugin_id, reason } => {
                if let Some(entry) = self.plugins.get_mut(&plugin_id) {
                    entry.offline = true;
                    entry.error = Some(reason);
                }
                true
            }
            HostEvent::Log {
                plugin_id, message, ..
            } => {
                eprintln!("[plugin {plugin_id}] {message}");
                false
            }
            HostEvent::Error { plugin_id, message } => {
                eprintln!("[plugin] {message}");
                match plugin_id {
                    Some(id) => {
                        if let Some(entry) = self.plugins.get_mut(&id) {
                            entry.error = Some(message);
                        } else {
                            self.error = Some(message);
                        }
                    }
                    None => self.error = Some(message),
                }
                true
            }
        }
    }

    fn set_tree(
        &mut self,
        plugin_id: &str,
        surface_id: &str,
        tree: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let key = surface_key(plugin_id, surface_id);
        let mut node = match serde_json::from_value::<Node>(tree) {
            Ok(node) => node,
            Err(error) => {
                let message = format!("Invalid plugin UI tree: {error}");
                eprintln!("[plugin {plugin_id}/{surface_id}] {message}");
                self.surface_errors.insert(key, message);
                return;
            }
        };
        // The native visual schema reserves handlers for numeric QuickJS
        // callback ids. Give plugins readable names and translate on dispatch.
        let handlers = self
            .handler_maps
            .entry(key.clone())
            .or_insert_with(|| Rc::new(RefCell::new(HashMap::new())))
            .clone();
        let mut rewritten = HashMap::new();
        rewrite_handlers(&mut node, &mut rewritten);
        if let Err(error) = node.validate() {
            let message = format!("Invalid plugin UI tree: {error}");
            eprintln!("[plugin {plugin_id}/{surface_id}] {message}");
            self.surface_errors.insert(key, message);
            return;
        }
        *handlers.borrow_mut() = rewritten;
        let Some(title) = self
            .plugins
            .get(plugin_id)
            .and_then(|entry| {
                entry
                    .manifest
                    .ui
                    .iter()
                    .find(|surface| surface.id == surface_id)
            })
            .map(|surface| surface.title.clone())
        else {
            self.surface_errors.insert(
                key,
                format!("Plugin {plugin_id} sent a tree for unknown surface {surface_id}"),
            );
            return;
        };
        self.surface_errors.remove(&key);
        let view = match self.views.get(&key) {
            Some(view) => view.clone(),
            None => {
                let runtime = self.runtime.clone();
                let owner = plugin_id.to_string();
                let surface = surface_id.to_string();
                let handlers = handlers.clone();
                let on_event: RemoteEvent = Rc::new(move |handler, value| {
                    let handler = handlers
                        .borrow()
                        .get(handler)
                        .cloned()
                        .unwrap_or_else(|| handler.to_string());
                    runtime.send_ui_event(&owner, &surface, &handler, value);
                });
                let document_id = format!("plugin-{plugin_id}-{surface_id}");
                let view = cx.new(|_| {
                    VisualView::remote(document_id, title.clone(), node.clone(), on_event)
                });
                self.views.insert(key, view.clone());
                view
            }
        };
        view.update(cx, |view, cx| view.set_tree(node, cx));
    }

    pub(crate) fn set_workspace_root(&mut self, root: PathBuf) {
        self.host_api.set_workspace_root(root);
    }

    pub(crate) fn open(&mut self, plugin_id: &str, surface_id: &str, cx: &mut Context<Self>) {
        let key = surface_key(plugin_id, surface_id);
        if self.active.as_deref() == Some(key.as_str()) {
            cx.notify();
            return;
        }
        if let Some(previous) = self.active.take()
            && let Some((plugin, surface)) = previous.split_once('\0')
        {
            self.runtime.send_ui_close(plugin, surface);
        }
        self.active = Some(key);
        self.runtime.send_ui_open(plugin_id, surface_id);
        cx.notify();
    }

    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.active.take() {
            if let Some((plugin, surface)) = key.split_once('\0') {
                self.runtime.send_ui_close(plugin, surface);
            }
            cx.notify();
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn has_surface(&self, plugin_id: &str, surface_id: &str) -> bool {
        self.plugins.get(plugin_id).is_some_and(|entry| {
            entry
                .manifest
                .ui
                .iter()
                .any(|surface| surface.id == surface_id)
        })
    }

    pub(crate) fn status_message(&self) -> Option<String> {
        self.error.clone()
    }

    pub(crate) fn sidebar_entries(&self) -> Vec<SidebarPlugin> {
        let active = self.active.as_deref();
        let mut entries = self
            .plugins
            .iter()
            .flat_map(|(plugin_id, entry)| {
                entry.manifest.ui.iter().map(move |surface| SidebarPlugin {
                    key: surface_key(plugin_id, &surface.id),
                    plugin_id: plugin_id.clone(),
                    surface_id: surface.id.clone(),
                    label: surface.title.clone(),
                    icon: surface.icon.clone(),
                    order: surface.order,
                    selected: active == Some(surface_key(plugin_id, &surface.id).as_str()),
                    offline: entry.offline,
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            a.order
                .unwrap_or(i64::MAX)
                .cmp(&b.order.unwrap_or(i64::MAX))
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                .then_with(|| a.key.cmp(&b.key))
        });
        entries
    }

    fn active_plugin(&self) -> Option<ActivePlugin> {
        let key = self.active.as_ref()?;
        for (plugin_id, entry) in &self.plugins {
            for surface in &entry.manifest.ui {
                if surface_key(plugin_id, &surface.id) == *key {
                    return Some(ActivePlugin {
                        title: surface.title.clone(),
                        icon: surface.icon.clone(),
                        plugin: entry.manifest.name.clone(),
                        offline: entry.offline,
                        error: self
                            .surface_errors
                            .get(key)
                            .cloned()
                            .or_else(|| entry.error.clone()),
                    });
                }
            }
        }
        None
    }
}

impl Drop for PluginSurfaceHost {
    fn drop(&mut self) {
        // The app-quit hook normally runs first and does this off the UI
        // thread. This is the last-resort reaping path for a released window.
        self.runtime.shutdown();
    }
}

impl Render for PluginSurfaceHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(active) = self.active_plugin() else {
            return div().size_full();
        };
        let key = self.active.clone().unwrap_or_default();
        let body = self
            .views
            .get(&key)
            .cloned()
            .map(IntoElement::into_any_element)
            .unwrap_or_else(|| {
                div()
                    .text_size(px(13.))
                    .text_color(content(0.55))
                    .child(if active.offline {
                        "This plugin is offline."
                    } else {
                        "Waiting for the plugin to render..."
                    })
                    .into_any_element()
            });
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(44.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(content(0.08))
                    .child(icon(plugin_icon(&active.icon), 16.).text_color(accent_color()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(active.title),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(content(0.4))
                                    .child(active.plugin),
                            ),
                    )
                    .when(active.offline, |row| {
                        row.child(
                            div()
                                .text_size(px(11.))
                                .text_color(content(0.45))
                                .child("offline"),
                        )
                    })
                    .child(
                        icon_button("close-plugin-surface", "Close plugin", "close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
            .children(active.error.map(|error| {
                div()
                    .mx_3()
                    .mt_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(content(0.12))
                    .text_size(px(12.))
                    .text_color(content(0.65))
                    .child(error)
            }))
            .child(
                div()
                    .id("plugin-surface-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(24.))
                    .child(div().w_full().min_w_0().child(body)),
            )
    }
}

impl crate::shell::Shell {
    /// True while a plugin surface owns the workspace body.
    pub(crate) fn plugin_surface_active(&self, cx: &App) -> bool {
        self.plugin_host
            .as_ref()
            .is_some_and(|host| host.read(cx).is_active())
    }

    pub(crate) fn plugin_navigation(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (entries, status) = self.plugin_host.as_ref().map_or_else(
            || (Vec::new(), None),
            |host| {
                let host = host.read(cx);
                (host.sidebar_entries(), host.status_message())
            },
        );
        let mut rows = Vec::new();
        for entry in entries {
            let selected = entry.selected;
            let offline = entry.offline;
            let key = entry.key.clone();
            let glyph = plugin_icon(&entry.icon);
            let plugin_id = entry.plugin_id.clone();
            let surface_id = entry.surface_id.clone();
            let debug_id = format!("sidebar-plugin-{plugin_id}-{surface_id}");
            let badge_debug_id = format!("sidebar-plugin-badge-{plugin_id}-{surface_id}");
            rows.push(
                div()
                    .id(SharedString::from(format!("plugin-nav-{key}")))
                    .debug_selector(move || debug_id.clone())
                    .role(Role::Button)
                    .aria_label(entry.label.clone())
                    .aria_description("Plugin")
                    .aria_selected(selected)
                    .cursor_pointer()
                    .h(px(32.))
                    .px_2()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(content(if selected { 1. } else { 0.5 }))
                    .when(selected, |row| row.bg(content(0.1)))
                    .when(offline, |row| row.opacity(0.55))
                    .hover(|row| row.bg(content(0.1)).text_color(content(1.)))
                    .child(icon(glyph, 16.).text_color(content(if selected { 0.7 } else { 0.35 })))
                    .child(div().flex_1().min_w_0().truncate().child(entry.label))
                    .when(offline, |row| {
                        row.child(
                            div()
                                .text_size(px(10.))
                                .text_color(content(0.4))
                                .child("offline"),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("plugin-badge-{key}")))
                            .debug_selector(move || badge_debug_id.clone())
                            .flex_shrink_0()
                            .child(icon("plug", 12.).text_color(content(if selected {
                                0.6
                            } else {
                                0.35
                            }))),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_plugin_surface(plugin_id.clone(), surface_id.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if let Some(status) = status {
            rows.push(
                div()
                    .px_2()
                    .py_1()
                    .text_size(px(11.))
                    .text_color(content(0.4))
                    .child(status)
                    .into_any_element(),
            );
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use monocode_plugin_host::{PluginReady, SurfaceTree};
    use serde_json::json;

    fn manifest() -> PluginManifest {
        serde_json::from_value(json!({
            "id": "com.test.adapter",
            "name": "Adapter test",
            "protocol": 1,
            "command": ["unused"],
            "ui": [{
                "id": "main",
                "title": "Main",
                "icon": "sparkles"
            }]
        }))
        .unwrap()
    }

    #[gpui::test]
    fn sidebar_entries_follow_surface_order(cx: &mut TestAppContext) {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.test.order",
            "name": "Order test",
            "protocol": 1,
            "command": ["unused"],
            "ui": [
                {"id": "third", "title": "Third", "order": 3},
                {"id": "first", "title": "First", "order": 1},
                {"id": "second", "title": "Second", "order": 2},
                {"id": "alpha", "title": "Alpha"}
            ]
        }))
        .unwrap();
        let tree = json!({"kind": "text", "text": "Order", "size": "body", "color": "ink"});
        let host = cx.update(|cx| cx.new(|cx| PluginSurfaceHost::with_surface(manifest, tree, cx)));
        host.update(cx, |host, _| {
            let labels = host
                .sidebar_entries()
                .into_iter()
                .map(|entry| entry.label)
                .collect::<Vec<_>>();
            assert_eq!(labels, ["First", "Second", "Third", "Alpha"]);
        });
    }

    #[test]
    fn rewrites_semantic_handlers_before_native_validation() {
        let mut node: Node = serde_json::from_value(json!({
            "kind": "column",
            "gap": 8,
            "children": [
                {"kind": "button", "label": "Click", "handler": "click", "disabled": false},
                {"kind": "input", "label": "Name", "value": "", "placeholder": "Name", "handler": "name", "submit_handler": "submit:name"},
                {
                    "kind": "code_editor",
                    "label": "Code",
                    "value": "x = 1",
                    "language": "py",
                    "height": 54,
                    "handler": "cell:1",
                    "submit_handler": "run:1",
                    "selection_handler": "select:1",
                    "context_menu": [
                        {"label": "Run selected", "handler": "run_sel:1", "requires_selection": true},
                        {"label": "Run all", "handler": "run_all"}
                    ]
                },
                {"kind": "table", "columns": ["Path"], "rows": [["a.csv"]], "handler": "pick:file"},
                {
                    "kind": "page_tabs",
                    "labels": ["One", "Two"],
                    "selected": 0,
                    "handler": "page",
                    "right_margin": true,
                    "children": [
                        {"kind": "button", "label": "Inner", "handler": "inner", "disabled": false}
                    ]
                }
            ]
        }))
        .unwrap();
        let mut handlers = HashMap::new();
        rewrite_handlers(&mut node, &mut handlers);
        node.validate().unwrap();
        assert_eq!(handlers.get("0").map(String::as_str), Some("click"));
        assert_eq!(handlers.get("1").map(String::as_str), Some("name"));
        assert_eq!(handlers.get("2").map(String::as_str), Some("submit:name"));
        assert_eq!(handlers.get("3").map(String::as_str), Some("cell:1"));
        assert_eq!(handlers.get("4").map(String::as_str), Some("run:1"));
        assert_eq!(handlers.get("5").map(String::as_str), Some("select:1"));
        assert_eq!(handlers.get("6").map(String::as_str), Some("run_sel:1"));
        assert_eq!(handlers.get("7").map(String::as_str), Some("run_all"));
        assert_eq!(handlers.get("8").map(String::as_str), Some("pick:file"));
        assert_eq!(handlers.get("9").map(String::as_str), Some("page"));
        assert_eq!(handlers.get("10").map(String::as_str), Some("inner"));
    }

    #[gpui::test]
    fn lists_opens_renders_and_closes_a_plugin_surface(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!(
            "monocode-empty-plugins-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (runtime, events) = PluginRuntime::start(&dir);
        let manifest = manifest();
        let ready = PluginReady {
            manifest: manifest.clone(),
        };
        let tree = json!({
            "kind": "column",
            "gap": 8,
            "children": [
                {"kind": "text", "text": "Plugin body", "size": "body", "color": "ink"},
                {"kind": "button", "label": "Click", "handler": "click", "disabled": false}
            ]
        });

        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut host = PluginSurfaceHost {
                        runtime,
                        events,
                        host_api: HostApi::new(dir.clone(), Some(dir.clone())),
                        shell: None,
                        plugins: HashMap::new(),
                        views: HashMap::new(),
                        handler_maps: HashMap::new(),
                        surface_errors: HashMap::new(),
                        active: None,
                        error: None,
                        _pump: None,
                    };
                    host.apply(HostEvent::Starting { plugin: manifest }, cx);
                    host.apply(
                        HostEvent::Ready {
                            plugin: ready,
                            trees: vec![SurfaceTree {
                                surface_id: "main".into(),
                                tree: tree.clone(),
                            }],
                        },
                        cx,
                    );
                    host.open("com.test.adapter", "main", cx);
                    assert!(host.is_active(), "plugin surface should be active");
                    assert_eq!(
                        host.views.len(),
                        1,
                        "plugin tree should have a view: {:?}",
                        host.surface_errors
                    );
                    assert!(
                        host.active_plugin().is_some(),
                        "active plugin metadata should resolve"
                    );
                    host
                })
            })
            .unwrap()
        });
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("visual-button-root/1").is_some(),
            "plugin tree should render through the native visual view"
        );
        window
            .update(visual, |host, _, cx| {
                assert!(host.is_active());
                assert_eq!(host.sidebar_entries().len(), 1);
                assert_eq!(host.sidebar_entries()[0].label, "Main");
                host.close(cx);
                assert!(!host.is_active());
            })
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
