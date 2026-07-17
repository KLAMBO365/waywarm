use serde::{Deserialize, Serialize};

use crate::config::Settings;

/// Bump when the settings/runtime shape the daemon must understand changes.
/// Clients and daemons with mismatched versions refuse to talk so upgrades
/// cannot silently drop fields (for example day schedule levels).
pub const IPC_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    GetState { version: u32 },
    ReplaceSettings { version: u32, settings: Settings },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeState {
    pub settings: Settings,
    pub outputs: Vec<String>,
    pub backend: String,
    pub active_warmth: u8,
    pub active_brightness: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    State { version: u32, state: RuntimeState },
    Error { version: u32, message: String },
}
