use crate::device::{DeviceCapabilities, DeviceSummary};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_offer_keeps_embedded_cursor_and_single_connection() {
        let signal: ServerSignal = serde_json::from_value(serde_json::json!({
            "type": "webrtc_offer", "session_id": Uuid::nil(), "sdp": "v=0"
        }))
        .unwrap();
        assert!(matches!(
            signal,
            ServerSignal::WebrtcOffer {
                local_cursor: false,
                input_sdp: None,
                ..
            }
        ));
    }
    #[test]
    fn input_ice_route_survives_serialization() {
        let signal = ClientSignal::WebrtcIce {
            session_id: Uuid::nil(),
            candidate: "candidate:test".into(),
            sdp_mid: None,
            sdp_mline_index: None,
            username_fragment: None,
            input: true,
        };
        let decoded: ClientSignal =
            serde_json::from_str(&serde_json::to_string(&signal).unwrap()).unwrap();
        assert!(matches!(
            decoded,
            ClientSignal::WebrtcIce { input: true, .. }
        ));
    }
}

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
        #[serde(default)]
        local_cursor: bool,
        #[serde(default)]
        input_sdp: Option<String>,
    },
    WebrtcAnswer {
        session_id: Uuid,
        sdp: String,
        #[serde(default)]
        input_control: bool,
        #[serde(default)]
        local_cursor: bool,
        #[serde(default)]
        input_sdp: Option<String>,
    },
    InputPacket {
        session_id: Uuid,
        data: Vec<u8>,
    },
    InputAck {
        session_id: Uuid,
        data: Vec<u8>,
    },
    WebrtcIce {
        session_id: Uuid,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
        #[serde(default)]
        input: bool,
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
        #[serde(default)]
        local_cursor: bool,
        #[serde(default)]
        input_sdp: Option<String>,
    },
    WebrtcAnswer {
        session_id: Uuid,
        sdp: String,
        #[serde(default)]
        input_control: bool,
        #[serde(default)]
        local_cursor: bool,
        #[serde(default)]
        input_sdp: Option<String>,
    },
    InputPacket {
        session_id: Uuid,
        data: Vec<u8>,
    },
    InputAck {
        session_id: Uuid,
        data: Vec<u8>,
    },
    WebrtcIce {
        session_id: Uuid,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
        #[serde(default)]
        input: bool,
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
