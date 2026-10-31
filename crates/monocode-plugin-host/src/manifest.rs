//! Plugin discovery and manifest validation.

use crate::protocol::PROTOCOL_VERSION;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

fn default_icon() -> String {
    "sparkles".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SurfaceDescriptor {
    pub id: String,
    pub title: String,
    #[serde(default = "default_icon")]
    pub icon: String,
    /// Lower numbers appear first. Unordered surfaces follow in title order.
    #[serde(default)]
    pub order: Option<i64>,
}

impl SurfaceDescriptor {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(
                "Surface id must contain 1–96 ASCII letters, digits, dots, underscores or hyphens"
                    .into(),
            );
        }
        if self.title.trim().is_empty() || self.title.chars().count() > 80 {
            return Err("Surface title must contain 1–80 characters".into());
        }
        if self.icon.trim().is_empty() || self.icon.len() > 64 {
            return Err("Surface icon must contain 1–64 bytes".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub protocol: u32,
    /// argv, never a shell string. Paths with spaces therefore work.
    pub command: Vec<String>,
    /// Optional working directory relative to the plugin directory.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub ui: Vec<SurfaceDescriptor>,
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(
                "Plugin id must contain 1–96 ASCII letters, digits, dots, underscores or hyphens"
                    .into(),
            );
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 80 {
            return Err("Plugin name must contain 1–80 characters".into());
        }
        if self.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "Unsupported plugin protocol {} (host supports {})",
                self.protocol, PROTOCOL_VERSION
            ));
        }
        if self.command.is_empty() || self.command[0].trim().is_empty() {
            return Err("Plugin command must be a non-empty argv".into());
        }
        if self.command.iter().any(|part| part.contains('\0')) {
            return Err("Plugin command must not contain NUL bytes".into());
        }
        if let Some(cwd) = &self.cwd {
            let path = Path::new(cwd);
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
            {
                return Err("Plugin cwd must be a relative path inside the plugin".into());
            }
        }
        let mut ids = std::collections::HashSet::new();
        for surface in &self.ui {
            surface.validate()?;
            if !ids.insert(surface.id.as_str()) {
                return Err(format!("Duplicate surface id: {}", surface.id));
            }
        }
        Ok(())
    }

    pub fn surfaces(&self) -> &[SurfaceDescriptor] {
        &self.ui
    }
}

#[derive(Clone, Debug)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct DiscoveryError {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Discovery {
    pub plugins: Vec<LoadedPlugin>,
    pub errors: Vec<DiscoveryError>,
}

/// Load `plugin.json` from `root` itself, or one level below it.
///
/// A missing or unreadable directory yields one discovery error rather than a
/// hard failure, so a broken plugin never prevents the others from starting.
pub fn discover_plugins(root: &Path) -> Discovery {
    let mut discovery = Discovery::default();
    let direct = root.join("plugin.json");
    if direct.is_file() {
        match load_plugin(root) {
            Ok(plugin) => discovery.plugins.push(plugin),
            Err(message) => discovery.errors.push(DiscoveryError {
                path: direct,
                message,
            }),
        }
        return discovery;
    }

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            discovery.errors.push(DiscoveryError {
                path: root.to_path_buf(),
                message: format!("Cannot read plugin directory: {error}"),
            });
            return discovery;
        }
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();

    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        if !dir.join("plugin.json").is_file() {
            continue;
        }
        match load_plugin(&dir) {
            Ok(plugin) => {
                if !seen.insert(plugin.manifest.id.clone()) {
                    discovery.errors.push(DiscoveryError {
                        path: dir.join("plugin.json"),
                        message: format!("Duplicate plugin id: {}", plugin.manifest.id),
                    });
                    continue;
                }
                discovery.plugins.push(plugin);
            }
            Err(message) => discovery.errors.push(DiscoveryError {
                path: dir.join("plugin.json"),
                message,
            }),
        }
    }
    discovery
}

pub fn load_plugin(root: &Path) -> Result<LoadedPlugin, String> {
    let path = root.join("plugin.json");
    let raw =
        fs::read_to_string(&path).map_err(|error| format!("Cannot read manifest: {error}"))?;
    let raw = raw.trim_start_matches('\u{feff}');
    let manifest: PluginManifest =
        serde_json::from_str(raw).map_err(|error| format!("Invalid manifest: {error}"))?;
    manifest.validate()?;
    Ok(LoadedPlugin {
        manifest,
        root: root.to_path_buf(),
    })
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(raw: &str) -> Result<PluginManifest, serde_json::Error> {
        serde_json::from_str(raw)
    }

    fn minimal(command: &str) -> String {
        format!(
            r#"{{
                "id": "com.example.test",
                "name": "Test",
                "protocol": 1,
                "command": [{command}],
                "ui": [{{"id": "main", "title": "Main"}}]
            }}"#
        )
    }

    #[test]
    fn a_valid_manifest_defaults_the_icon_and_order() {
        let manifest = manifest(&minimal(r#""node""#)).unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.ui[0].icon, "sparkles");
        assert_eq!(manifest.ui[0].order, None);
    }

    #[test]
    fn explicit_surface_order_is_kept() {
        let raw =
            minimal(r#""node""#).replace(r#""title": "Main""#, r#""title": "Main", "order": 2"#);
        let manifest = manifest(&raw).unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.ui[0].order, Some(2));
    }

    #[test]
    fn duplicate_and_malformed_surface_ids_are_rejected() {
        let duplicate = minimal(r#""node""#).replace(
            r#""ui": [{"id": "main", "title": "Main"}]"#,
            r#""ui": [{"id": "main", "title": "One"}, {"id": "main", "title": "Two"}]"#,
        );
        let value = manifest(&duplicate).unwrap();
        assert!(value.validate().unwrap_err().contains("Duplicate surface"));

        let bad_id = minimal(r#""node""#).replace(r#""id": "main""#, r#""id": "bad id""#);
        let value = manifest(&bad_id).unwrap();
        assert!(value.validate().unwrap_err().contains("Surface id"));
    }

    #[test]
    fn unsupported_protocols_and_escaping_cwds_are_rejected() {
        let value = manifest(&minimal(r#""node""#).replace(r#""protocol": 1"#, r#""protocol": 9"#))
            .unwrap();
        assert!(value.validate().unwrap_err().contains("Unsupported"));

        let value = manifest(&minimal(r#""node""#).replace(
            r#""command": ["node"]"#,
            r#""cwd": "../elsewhere", "command": ["node"]"#,
        ))
        .unwrap();
        assert!(value.validate().unwrap_err().contains("cwd"));
    }

    #[test]
    fn unknown_manifest_fields_are_rejected() {
        let raw = minimal(r#""node""#).replace(r#""name": "Test""#, r#""name": "Test", "trap": 1"#);
        assert!(manifest(&raw).is_err());
    }

    #[test]
    fn discovery_skips_directories_without_manifests() {
        let root = std::env::temp_dir().join(format!(
            "monocode-plugin-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::create_dir_all(root.join("one")).unwrap();
        fs::write(root.join("one/plugin.json"), minimal(r#""node""#)).unwrap();
        let discovery = discover_plugins(&root);
        assert_eq!(discovery.plugins.len(), 1);
        assert!(discovery.errors.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
