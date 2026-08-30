use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceCapabilities {
    pub width: u32,
    pub height: u32,
    pub fps: u16,
    pub codecs: Vec<String>,
    pub audio: bool,
}

impl Default for DeviceCapabilities {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            codecs: vec!["h264".into()],
            audio: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub capabilities: DeviceCapabilities,
}
