//! Wire types and framing for the plugin protocol.
//!
//! The transport is JSON-RPC 2.0, one compact JSON object per line. Requests
//! carry integer ids; notifications have no id. Plugin UI trees are data, not
//! code, and are validated against the host's native component schema.

use crate::manifest::SurfaceDescriptor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
/// A plugin cannot make the host allocate an unbounded line.
pub const MAX_LINE_BYTES: usize = 2 * 1024 * 1024;
/// Serialized UI trees are bounded independently of the transport line.
pub const MAX_TREE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
struct OutgoingRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Serialize)]
struct OutgoingNotification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: Value,
}

pub fn request_line(id: u64, method: &str, params: Value) -> Result<String, String> {
    serde_json::to_string(&OutgoingRequest {
        jsonrpc: "2.0",
        id,
        method,
        params,
    })
    .map_err(|error| format!("Cannot encode plugin request: {error}"))
}

pub fn notification_line(method: &str, params: Value) -> Result<String, String> {
    serde_json::to_string(&OutgoingNotification {
        jsonrpc: "2.0",
        method,
        params,
    })
    .map_err(|error| format!("Cannot encode plugin notification: {error}"))
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Incoming {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<RpcErrorObject>,
}

impl Incoming {
    pub fn is_response(&self) -> bool {
        self.method.is_none() && self.id.is_some()
    }

    pub fn response_result(&self) -> Result<Value, String> {
        if let Some(error) = &self.error {
            return Err(format!("Plugin error {}: {}", error.code, error.message));
        }
        Ok(self.result.clone().unwrap_or(Value::Null))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RpcErrorObject {
    pub code: i64,
    pub message: String,
}

pub(crate) fn parse_incoming(line: &str) -> Result<Incoming, String> {
    let message: Incoming =
        serde_json::from_str(line).map_err(|error| format!("Invalid JSON-RPC message: {error}"))?;
    if message.jsonrpc != "2.0" {
        return Err("JSON-RPC messages must declare \"jsonrpc\": \"2.0\"".into());
    }
    if message.method.is_none() && message.id.is_none() {
        return Err("JSON-RPC message has neither a method nor an id".into());
    }
    Ok(message)
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct InitializeResult {
    #[serde(default)]
    pub protocol: Option<u32>,
    /// A plugin may narrow or replace the manifest's static contributions.
    #[serde(default)]
    pub ui: Option<Vec<SurfaceDescriptor>>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct UiSetParams {
    pub surface: String,
    pub tree: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct UiCloseParams {
    pub surface: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct LogParams {
    #[serde(default = "default_log_level")]
    pub level: String,
    pub message: String,
}

fn default_log_level() -> String {
    "info".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_and_notifications_round_trip() {
        let line = notification_line("ui.set", serde_json::json!({"surface": "main", "tree": {}}))
            .unwrap();
        assert!(!line.contains('\n'));
        let message = parse_incoming(&line).unwrap();
        assert!(message.method.as_deref() == Some("ui.set"));
        assert!(message.id.is_none());

        let line = request_line(7, "initialize", serde_json::json!({"protocol": 1})).unwrap();
        let message = parse_incoming(&line).unwrap();
        assert!(!message.is_response());
        assert_eq!(message.id, Some(7));
    }

    #[test]
    fn malformed_frames_are_rejected() {
        assert!(parse_incoming("not json").is_err());
        assert!(parse_incoming(r#"{"jsonrpc":"1.0","method":"ui.set"}"#).is_err());
        assert!(parse_incoming(r#"{"jsonrpc":"2.0"}"#).is_err());
        assert!(parse_incoming(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).is_ok());
    }
}
