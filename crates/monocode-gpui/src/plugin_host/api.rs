//! The fixed host API surface exposed to plugins.
//!
//! Plugin requests arrive as JSON-RPC requests and are dispatched here. This
//! is the only place that maps a plugin method name to app data. Other modules
//! expose normal functions; they never branch on plugin ids or method names.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{fs, io::ErrorKind, path::PathBuf};

/// A host request is answered from this fixed surface. Add methods here as
/// Monocode grows; callers never negotiate a separate API version.
pub(crate) struct HostApi {
    workspace_root: PathBuf,
    data_dir: Option<PathBuf>,
}

impl HostApi {
    pub(crate) fn new(workspace_root: PathBuf, data_dir: Option<PathBuf>) -> Self {
        Self {
            workspace_root,
            data_dir,
        }
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
            _ => Err(format!("Unknown host method: {method}")),
        }
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
