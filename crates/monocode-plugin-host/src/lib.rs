//! Standalone process runtime for Monocode plugins.
//!
//! This crate knows nothing about GPUI or `monocode-core`. It discovers plugin
//! manifests, starts child processes, speaks newline-delimited JSON-RPC over
//! stdin/stdout, and forwards the plugin's declarative UI trees to the host.
//! Rendering and UI schema validation live in the application adapter.

pub mod host;
pub mod manifest;
pub mod protocol;

pub use host::{HostEvent, HostOptions, PluginReady, PluginRuntime, SurfaceTree};
pub use manifest::{Discovery, DiscoveryError, LoadedPlugin, PluginManifest, SurfaceDescriptor};
pub use protocol::PROTOCOL_VERSION;
