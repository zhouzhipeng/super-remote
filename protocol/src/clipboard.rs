use serde::{Deserialize, Serialize};

/// Keeps one message below the conservative WebRTC data-channel message limit.
pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 12 * 1024;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipboardRequest {
    Read { id: u32 },
    Write { id: u32, text: String, paste: bool },
    Paste { id: u32 },
}

impl ClipboardRequest {
    pub fn id(&self) -> u32 {
        match self {
            Self::Read { id } | Self::Write { id, .. } | Self::Paste { id } => *id,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipboardResponse {
    Content { id: u32, text: String },
    Ack { id: u32 },
    Error { id: u32, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_preserved() {
        assert_eq!(ClipboardRequest::Read { id: 7 }.id(), 7);
        assert_eq!(
            ClipboardRequest::Write {
                id: 9,
                text: "hello".into(),
                paste: true,
            }
            .id(),
            9
        );
        assert_eq!(ClipboardRequest::Paste { id: 11 }.id(), 11);
    }
}
