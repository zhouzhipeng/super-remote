use thiserror::Error;

pub const HEADER_LEN: usize = 12;
pub const MAX_MESSAGE_LEN: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InputType {
    MouseMove = 0x01,
    MouseButton = 0x02,
    Keyboard = 0x03,
    MouseWheel = 0x04,
    MouseRelative = 0x05,
}

impl TryFrom<u8> for InputType {
    type Error = DecodeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::MouseMove),
            0x02 => Ok(Self::MouseButton),
            0x03 => Ok(Self::Keyboard),
            0x04 => Ok(Self::MouseWheel),
            0x05 => Ok(Self::MouseRelative),
            value => Err(DecodeError::UnknownType(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: u8,
        down: bool,
    },
    Keyboard {
        scan_code: u16,
        down: bool,
        extended: bool,
    },
    MouseWheel {
        delta_x: i16,
        delta_y: i16,
    },
    MouseRelative {
        dx: i16,
        dy: i16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedInputEvent {
    pub flags: u8,
    pub timestamp_us: u64,
    pub event: InputEvent,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("input packet is shorter than the 12-byte header")]
    ShortHeader,
    #[error("unknown input message type {0:#04x}")]
    UnknownType(u8),
    #[error("declared payload length does not match packet")]
    InvalidLength,
    #[error("input message is larger than the protocol limit")]
    TooLarge,
    #[error("invalid boolean field")]
    InvalidBoolean,
}

impl TimedInputEvent {
    pub fn encode(self) -> Vec<u8> {
        let (kind, payload): (InputType, Vec<u8>) = match self.event {
            InputEvent::MouseMove { x, y } => {
                let mut p = Vec::with_capacity(4);
                p.extend_from_slice(&x.to_le_bytes());
                p.extend_from_slice(&y.to_le_bytes());
                (InputType::MouseMove, p)
            }
            InputEvent::MouseButton { button, down } => {
                (InputType::MouseButton, vec![button, u8::from(down)])
            }
            InputEvent::Keyboard {
                scan_code,
                down,
                extended,
            } => {
                let mut p = Vec::with_capacity(4);
                p.extend_from_slice(&scan_code.to_le_bytes());
                p.push(u8::from(down));
                p.push(u8::from(extended));
                (InputType::Keyboard, p)
            }
            InputEvent::MouseWheel { delta_x, delta_y } => {
                let mut p = Vec::with_capacity(4);
                p.extend_from_slice(&delta_x.to_le_bytes());
                p.extend_from_slice(&delta_y.to_le_bytes());
                (InputType::MouseWheel, p)
            }
            InputEvent::MouseRelative { dx, dy } => {
                let mut p = Vec::with_capacity(4);
                p.extend_from_slice(&dx.to_le_bytes());
                p.extend_from_slice(&dy.to_le_bytes());
                (InputType::MouseRelative, p)
            }
        };
        let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
        out.push(kind as u8);
        out.push(self.flags);
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.timestamp_us.to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    pub fn decode(packet: &[u8]) -> Result<Self, DecodeError> {
        if packet.len() < HEADER_LEN {
            return Err(DecodeError::ShortHeader);
        }
        if packet.len() > MAX_MESSAGE_LEN {
            return Err(DecodeError::TooLarge);
        }
        let kind = InputType::try_from(packet[0])?;
        let flags = packet[1];
        let length = u16::from_le_bytes([packet[2], packet[3]]) as usize;
        if packet.len() != HEADER_LEN + length {
            return Err(DecodeError::InvalidLength);
        }
        let timestamp_us = u64::from_le_bytes(packet[4..12].try_into().unwrap());
        let payload = &packet[HEADER_LEN..];
        let boolean = |v: u8| match v {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::InvalidBoolean),
        };
        let event = match (kind, payload) {
            (InputType::MouseMove, [x0, x1, y0, y1]) => InputEvent::MouseMove {
                x: u16::from_le_bytes([*x0, *x1]),
                y: u16::from_le_bytes([*y0, *y1]),
            },
            (InputType::MouseButton, [button, down]) => InputEvent::MouseButton {
                button: *button,
                down: boolean(*down)?,
            },
            (InputType::Keyboard, [s0, s1, down, extended]) => InputEvent::Keyboard {
                scan_code: u16::from_le_bytes([*s0, *s1]),
                down: boolean(*down)?,
                extended: boolean(*extended)?,
            },
            (InputType::MouseWheel, [x0, x1, y0, y1]) => InputEvent::MouseWheel {
                delta_x: i16::from_le_bytes([*x0, *x1]),
                delta_y: i16::from_le_bytes([*y0, *y1]),
            },
            (InputType::MouseRelative, [x0, x1, y0, y1]) => InputEvent::MouseRelative {
                dx: i16::from_le_bytes([*x0, *x1]),
                dy: i16::from_le_bytes([*y0, *y1]),
            },
            _ => return Err(DecodeError::InvalidLength),
        };
        Ok(Self {
            flags,
            timestamp_us,
            event,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_event() {
        let events = [
            InputEvent::MouseMove { x: 12, y: 65535 },
            InputEvent::MouseButton {
                button: 2,
                down: true,
            },
            InputEvent::Keyboard {
                scan_code: 0x1d,
                down: false,
                extended: true,
            },
            InputEvent::MouseWheel {
                delta_x: -120,
                delta_y: 240,
            },
            InputEvent::MouseRelative { dx: -8, dy: 19 },
        ];
        for event in events {
            let message = TimedInputEvent {
                flags: 7,
                timestamp_us: 42,
                event,
            };
            assert_eq!(TimedInputEvent::decode(&message.encode()).unwrap(), message);
        }
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut packet = TimedInputEvent {
            flags: 0,
            timestamp_us: 0,
            event: InputEvent::MouseMove { x: 0, y: 0 },
        }
        .encode();
        packet.push(0);
        assert_eq!(
            TimedInputEvent::decode(&packet),
            Err(DecodeError::InvalidLength)
        );
    }
}
