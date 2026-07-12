use serde::{Deserialize, Serialize};

use crate::config::Settings;

pub const IPC_VERSION: u32 = 1;

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
