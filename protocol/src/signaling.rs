use crate::device::{DeviceCapabilities, DeviceSummary};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientSignal {
    DeviceRegister {
        device_id: String,
        name: String,
        capabilities: DeviceCapabilities,
    },
    WebrtcOffer {
        session_id: Uuid,
        session_token: String,
        sdp: String,
        #[serde(default)]
        viewport_width: Option<u32>,
        #[serde(default)]
        viewport_height: Option<u32>,
    },
    WebrtcAnswer {
        session_id: Uuid,
        sdp: String,
    },
    WebrtcIce {
        session_id: Uuid,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
    },
    SessionClose {
        session_id: Uuid,
    },
    Ping {
        nonce: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerSignal {
    Ready,
    DeviceList {
        devices: Vec<DeviceSummary>,
    },
    SessionRequested {
        session_id: Uuid,
        session_token: String,
    },
    WebrtcOffer {
        session_id: Uuid,
        sdp: String,
        #[serde(default)]
        viewport_width: Option<u32>,
        #[serde(default)]
        viewport_height: Option<u32>,
    },
    WebrtcAnswer {
        session_id: Uuid,
        sdp: String,
    },
    WebrtcIce {
        session_id: Uuid,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
    },
    SessionClosed {
        session_id: Uuid,
        reason: String,
    },
    Pong {
        nonce: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    pub session_id: Uuid,
    pub session_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnCredentials {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub ttl_seconds: u64,
}
