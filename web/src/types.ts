export interface DeviceCapabilities {
  width: number;
  height: number;
  fps: number;
  codecs: string[];
  audio: boolean;
}

export interface DeviceSummary {
  id: string;
  name: string;
  online: boolean;
  capabilities: DeviceCapabilities;
}

export type ClientSignal =
  | { type: "webrtc_offer"; session_id: string; session_token: string; sdp: string }
  | { type: "webrtc_ice"; session_id: string; candidate: string; sdp_mid: string | null; sdp_mline_index: number | null; username_fragment: string | null }
  | { type: "session_close"; session_id: string }
  | { type: "ping"; nonce: number };

export type ServerSignal =
  | { type: "ready" }
  | { type: "webrtc_answer"; session_id: string; sdp: string }
  | { type: "webrtc_ice"; session_id: string; candidate: string; sdp_mid: string | null; sdp_mline_index: number | null; username_fragment: string | null }
  | { type: "session_closed"; session_id: string; reason: string }
  | { type: "pong"; nonce: number }
  | { type: "error"; code: string; message: string };
