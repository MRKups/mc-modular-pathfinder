//! The fixed host API surface exposed to plugins.
//!
//! Plugin requests arrive as JSON-RPC requests and are dispatched here. This
//! is the only place that maps a plugin method name to app data. Other modules
//! expose normal functions; they never branch on plugin ids or method names.

use crate::settings::Preferences;
use gpui::App;
use monocode_core::visual::{MAX_NODES, MAX_SOURCE_BYTES};
use monocode_core::{
    agent::Settings,
    catalog,
    provider::{self, Kind, Request},
    session_title,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{fs, io::ErrorKind, path::PathBuf};

const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_SYSTEM_BYTES: usize = 16 * 1024;

/// A prepared one-shot model request. The adapter runs it on a background
/// worker so a slow provider never blocks UI input.
#[derive(Debug)]
pub(crate) struct Completion {
    pub request: Request,
    pub provider: String,
    pub model: Option<String>,
}

/// A host request is answered from this fixed surface. Add methods here as
/// Monocode grows; callers never negotiate a separate API version.
pub(crate) struct HostApi {
    workspace_root: PathBuf,
    data_dir: Option<PathBuf>,
    plugin_dir: Option<PathBuf>,
}

impl HostApi {
    pub(crate) fn new(workspace_root: PathBuf, data_dir: Option<PathBuf>) -> Self {
        Self {
            workspace_root,
            data_dir,
            plugin_dir: None,
        }
    }

    pub(crate) fn with_plugin_dir(mut self, plugin_dir: PathBuf) -> Self {
        self.plugin_dir = Some(plugin_dir);
        self
    }

    pub(crate) fn set_workspace_root(&mut self, root: PathBuf) {
        self.workspace_root = root;
    }

    pub(crate) fn call(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match method {
            "workspace.info" => self.workspace_info(),
            "storage.get" => self.storage_get(plugin_id, params),
            "storage.set" => self.storage_set(plugin_id, params),
            "host.describe" => self.describe(),
            _ => Err(format!("Unknown host method: {method}")),
        }
    }

    /// Installed providers and their cached models. This never starts a
    /// provider process; an empty `models` list means "use the default".
    pub(crate) fn providers(&self, cx: &App) -> Result<Value, String> {
        let workspace = self.workspace_root.to_string_lossy();
        let prefs = cx.try_global::<Preferences>();
        let chosen = prefs.and_then(Preferences::background_text_model);
        let mut providers = Vec::new();
        for kind in Kind::ALL {
            if !session_title::supports(kind) || provider::resolve(kind).is_none() {
                continue;
            }
            let account = crate::accounts::get(cx)
                .map(|center| center.read(cx).selected(kind, &workspace))
                .unwrap_or_default();
            let models = catalog::cached_models_for(kind, &account)
                .unwrap_or_default()
                .into_iter()
                .map(|model| json!({"id": model.id, "name": model.name}))
                .collect::<Vec<_>>();
            providers.push(json!({
                "id": kind.id(),
                "label": kind.label(),
                "account": account.id,
                "models": models,
                "background": chosen.as_ref().is_some_and(|(k, _)| *k == kind),
            }));
        }
        let background = chosen.map(|(kind, model)| {
            json!({
                "provider": kind.id(),
                "model": model,
            })
        });
        Ok(json!({"providers": providers, "background": background}))
    }

    /// Validate and translate `llm.complete` params. The caller runs the
    /// returned blocking request off the UI thread.
    pub(crate) fn prepare_completion(&self, cx: &App, params: Value) -> Result<Completion, String> {
        let params: CompleteParams = serde_json::from_value(params)
            .map_err(|error| format!("Invalid llm.complete params: {error}"))?;
        if params.prompt.trim().is_empty() {
            return Err("Prompt must not be empty".into());
        }
        if params.prompt.len() > MAX_PROMPT_BYTES {
            return Err(format!(
                "Prompt exceeds the {} byte limit",
                MAX_PROMPT_BYTES
            ));
        }
        if params
            .system
            .as_ref()
            .is_some_and(|system| system.len() > MAX_SYSTEM_BYTES)
        {
            return Err(format!(
                "System prompt exceeds the {} byte limit",
                MAX_SYSTEM_BYTES
            ));
        }
        let prefs = cx.try_global::<Preferences>();
        let background = prefs.and_then(Preferences::background_text_model);
        let kind = match params.provider.as_deref() {
            Some(id) => Kind::parse(id).ok_or_else(|| format!("Unknown provider: {id}"))?,
            None => background
                .as_ref()
                .map(|(kind, _)| *kind)
                .or_else(|| {
                    Kind::ALL.into_iter().find(|kind| {
                        session_title::supports(*kind) && provider::resolve(*kind).is_some()
                    })
                })
                .ok_or("No text-capable provider is installed")?,
        };
        if !session_title::supports(kind) {
            return Err(format!(
                "{} cannot run background text requests",
                kind.label()
            ));
        }
        let binary = provider::resolve(kind).ok_or_else(|| kind.unavailable_hint())?;
        let model = params.model.or_else(|| {
            background
                .as_ref()
                .filter(|(chosen, _)| *chosen == kind)
                .and_then(|(_, model)| model.clone())
        });
        let workspace = self.workspace_root.to_string_lossy();
        let account = crate::accounts::get(cx)
            .map(|center| center.read(cx).selected(kind, &workspace))
            .unwrap_or_default();
        let prompt = match params.system.filter(|system| !system.trim().is_empty()) {
            Some(system) => format!("{system}\n\n---\n\n{}", params.prompt),
            None => params.prompt,
        };
        Ok(Completion {
            request: Request {
                kind,
                binary,
                cwd: self.workspace_root.clone(),
                prompt,
                provider_id: None,
                model: model.clone(),
                settings: Settings {
                    account,
                    ..Default::default()
                },
            },
            provider: kind.id().into(),
            model,
        })
    }

    fn describe(&self) -> Result<Value, String> {
        Ok(json!({
            "protocol": monocode_plugin_host::PROTOCOL_VERSION,
            "appVersion": env!("CARGO_PKG_VERSION"),
            "pluginDir": self.plugin_dir.as_ref().map(|path| path.to_string_lossy()),
            "methods": [
                "workspace.info",
                "storage.get",
                "storage.set",
                "host.describe",
                "llm.providers",
                "llm.complete"
            ],
            "features": {
                "codeEditor": true,
                "llm": true
            },
            "limits": {
                "lineBytes": 2 * 1024 * 1024,
                "uiTreeBytes": 1024 * 1024,
                "uiNodes": MAX_NODES,
                "sourceBytes": MAX_SOURCE_BYTES,
                "llmPromptBytes": MAX_PROMPT_BYTES,
                "llmSystemBytes": MAX_SYSTEM_BYTES
            }
        }))
    }

    fn workspace_info(&self) -> Result<Value, String> {
        Ok(json!({
            "root": self.workspace_root.to_string_lossy(),
            "name": self
                .workspace_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        }))
    }

    fn storage_get(&self, plugin_id: &str, params: Value) -> Result<Value, String> {
        let params: StorageGetParams = serde_json::from_value(params)
            .map_err(|error| format!("Invalid storage.get params: {error}"))?;
        validate_key(&params.key)?;
        let store = self.read_store(plugin_id)?;
        Ok(store.get(&params.key).cloned().unwrap_or(Value::Null))
    }

    fn storage_set(&self, plugin_id: &str, params: Value) -> Result<Value, String> {
        let params: StorageSetParams = serde_json::from_value(params)
            .map_err(|error| format!("Invalid storage.set params: {error}"))?;
        validate_key(&params.key)?;
        let mut store = self.read_store(plugin_id)?;
        store.insert(params.key, params.value);
        self.write_store(plugin_id, &store)?;
        Ok(Value::Null)
    }

    fn storage_path(&self, plugin_id: &str) -> Result<PathBuf, String> {
        if plugin_id.is_empty()
            || plugin_id.len() > 96
            || plugin_id == "."
            || plugin_id == ".."
            || plugin_id.contains('/')
            || plugin_id.contains('\\')
        {
            return Err("Invalid plugin id for storage".into());
        }
        let data_dir = self.data_dir.as_ref().ok_or_else(|| {
            "Plugin storage is unavailable: the app has no data directory".to_string()
        })?;
        Ok(data_dir
            .join("plugin-data")
            .join(plugin_id)
            .join("storage.json"))
    }

    fn read_store(&self, plugin_id: &str) -> Result<Map<String, Value>, String> {
        let path = self.storage_path(plugin_id)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(format!("Cannot read plugin storage: {error}")),
        };
        if bytes.len() > 1024 * 1024 {
            return Err("Plugin storage exceeds 1 MiB".into());
        }
        serde_json::from_slice(&bytes).map_err(|error| format!("Invalid plugin storage: {error}"))
    }

    fn write_store(&self, plugin_id: &str, store: &Map<String, Value>) -> Result<(), String> {
        let path = self.storage_path(plugin_id)?;
        let parent = path
            .parent()
            .ok_or_else(|| "Invalid plugin storage path".to_string())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create plugin storage: {error}"))?;
        let bytes = serde_json::to_vec(store)
            .map_err(|error| format!("Cannot encode plugin storage: {error}"))?;
        if bytes.len() > 1024 * 1024 {
            return Err("Plugin storage exceeds 1 MiB".into());
        }
        fs::write(&path, bytes).map_err(|error| format!("Cannot write plugin storage: {error}"))
    }
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("Storage key must not be empty".into());
    }
    if key.len() > 256 {
        return Err("Storage key must be at most 256 bytes".into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct StorageGetParams {
    key: String,
}

#[derive(Deserialize)]
struct StorageSetParams {
    key: String,
    value: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteParams {
    prompt: String,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "monocode-plugin-api-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn workspace_info_uses_the_current_root() {
        let root = temp_root();
        let workspace = root.join("monocode");
        let api = HostApi::new(workspace.clone(), None);
        let info = api.call("com.test", "workspace.info", Value::Null).unwrap();
        assert_eq!(info["root"].as_str(), workspace.to_str());
        assert_eq!(info["name"].as_str(), Some("monocode"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn storage_round_trips_through_a_plugin_scoped_file() {
        let root = temp_root();
        let api = HostApi::new(root.clone(), Some(root.clone()));
        assert_eq!(
            api.call(
                "com.test",
                "storage.set",
                json!({"key": "note", "value": {"text": "hello"}})
            )
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            api.call("com.test", "storage.get", json!({"key": "note"}))
                .unwrap(),
            json!({"text": "hello"})
        );
        assert_eq!(
            api.call("com.test", "storage.get", json!({"key": "missing"}))
                .unwrap(),
            Value::Null
        );
        assert!(root.join("plugin-data/com.test/storage.json").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn storage_is_unavailable_without_a_data_directory() {
        let api = HostApi::new(PathBuf::from("/work/monocode"), None);
        let error = api
            .call("com.test", "storage.get", json!({"key": "note"}))
            .unwrap_err();
        assert!(error.contains("no data directory"));
    }

    #[test]
    fn describe_advertises_the_fixed_surface_and_limits() {
        let api = HostApi::new(PathBuf::from("/work/monocode"), None);
        let value = api.call("com.test", "host.describe", Value::Null).unwrap();
        assert_eq!(value["protocol"], 1);
        assert_eq!(value["features"]["codeEditor"], true);
        assert!(
            value["methods"]
                .as_array()
                .unwrap()
                .iter()
                .any(|method| method == "llm.complete")
        );
        assert_eq!(value["limits"]["uiNodes"], MAX_NODES);
    }

    #[gpui::test]
    fn providers_can_be_listed_without_starting_a_process(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let api = HostApi::new(PathBuf::from("/work/monocode"), None);
            let value = api.providers(cx).unwrap();
            assert!(value["providers"].is_array());
        });
    }

    #[gpui::test]
    fn completion_validation_fails_before_starting_a_provider(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let api = HostApi::new(PathBuf::from("/work/monocode"), None);
            let error = api
                .prepare_completion(cx, json!({"prompt": "  ", "provider": "claude"}))
                .unwrap_err();
            assert!(error.contains("Prompt must not be empty"));
            let error = api
                .prepare_completion(cx, json!({"prompt": "hello", "provider": "nope"}))
                .unwrap_err();
            assert!(error.contains("Unknown provider"));
        });
    }

    #[test]
    fn unknown_methods_and_bad_keys_are_rejected() {
        let api = HostApi::new(PathBuf::from("/work/monocode"), None);
        assert!(
            api.call("com.test", "workspace.unknown", Value::Null)
                .unwrap_err()
                .contains("Unknown host method")
        );
        assert!(
            api.call("com.test", "storage.get", json!({"key": ""}))
                .unwrap_err()
                .contains("must not be empty")
        );
    }
}
