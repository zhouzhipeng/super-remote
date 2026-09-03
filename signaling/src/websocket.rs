use std::{net::IpAddr, sync::Arc};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use remote_protocol::{
    device::DeviceSummary,
    signaling::{ClientSignal, ServerSignal},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    auth::{Principal, Role},
    state::{AppState, DeviceConnection},
};

pub async fn serve(socket: WebSocket, state: Arc<AppState>, principal: Principal, peer_ip: IpAddr) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerSignal>(64);

    let writer = tokio::spawn(async move {
        while let Some(signal) = rx.recv().await {
            let Ok(json) = serde_json::to_string(&signal) else {
                continue;
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });
    let _ = tx.send(ServerSignal::Ready).await;

    while let Some(Ok(message)) = ws_rx.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let signal: ClientSignal = match serde_json::from_str(&text) {
            Ok(signal) => signal,
            Err(error) => {
                let _ = tx
                    .send(ServerSignal::Error {
                        code: "invalid_message".into(),
                        message: error.to_string(),
                    })
                    .await;
                continue;
            }
        };
        if !handle_signal(&state, &principal, &tx, peer_ip, signal).await {
            let _ = tx
                .send(ServerSignal::Error {
                    code: "forbidden".into(),
                    message: "signal is not authorized for this connection".into(),
                })
                .await;
        }
    }

    if principal.role == Role::Device {
        let mut devices = state.devices.write().await;
        if let Some(device) = devices.get_mut(&principal.subject) {
            device.summary.online = false;
            device.sender = None;
        }
    }
    writer.abort();
    debug!(subject = %principal.subject, "websocket disconnected");
}

async fn handle_signal(
    state: &Arc<AppState>,
    principal: &Principal,
    tx: &mpsc::Sender<ServerSignal>,
    peer_ip: IpAddr,
    signal: ClientSignal,
) -> bool {
    match signal {
        ClientSignal::DeviceRegister {
            device_id,
            name,
            capabilities,
        } if principal.role == Role::Device && principal.subject == device_id => {
            state.devices.write().await.insert(
                device_id.clone(),
                DeviceConnection {
                    summary: DeviceSummary {
                        id: device_id,
                        name,
                        online: true,
                        capabilities,
                    },
                    sender: Some(tx.clone()),
                },
            );
            true
        }
        ClientSignal::WebrtcOffer {
            session_id,
            session_token,
            sdp,
            viewport_width,
            viewport_height,
        } if principal.role == Role::User => {
            if !state
                .authorize_offer(session_id, &principal.subject, &session_token, tx.clone())
                .await
            {
                // A newer create-session request may have evicted this session
                // between the HTTP response and offer creation. Close this
                // browser deterministically instead of leaving it negotiating.
                return tx
                    .send(ServerSignal::SessionClosed {
                        session_id,
                        reason: "session_unavailable".into(),
                    })
                    .await
                    .is_ok();
            }
            // ICE candidates are always trickled as separate WebrtcIce
            // messages. Chromium may also copy an already-gathered mDNS host
            // candidate into localDescription SDP. Forwarding both forms lets
            // the receiving ICE agent deduplicate the later, server-rewritten
            // LAN-address candidate against the unusable `.local` form. Strip
            // every inline candidate at this trust boundary so all browsers
            // use the same ordered offer-then-candidates protocol.
            let (sdp, stripped_candidates) = strip_inline_ice_candidates(sdp);
            if stripped_candidates > 0 {
                info!(
                    %session_id,
                    count = stripped_candidates,
                    "stripped inline ICE candidates from browser offer"
                );
            }
            state
                .route_to_device(
                    session_id,
                    &principal.subject,
                    ServerSignal::WebrtcOffer {
                        session_id,
                        sdp,
                        viewport_width,
                        viewport_height,
                    },
                )
                .await
        }
        ClientSignal::WebrtcAnswer { session_id, sdp } if principal.role == Role::Device => {
            state
                .route_to_browser(
                    session_id,
                    &principal.subject,
                    ServerSignal::WebrtcAnswer { session_id, sdp },
                )
                .await
        }
        ClientSignal::WebrtcIce {
            session_id,
            candidate,
            sdp_mid,
            sdp_mline_index,
            username_fragment,
        } => {
            let (candidate, mdns_rewritten) = match principal.role {
                Role::User => rewrite_mdns_candidate(candidate, peer_ip),
                Role::Device => (candidate, false),
            };
            if mdns_rewritten {
                // macOS Chromium masks its LAN address behind a randomized
                // `.local` hostname. Windows mDNS resolution is not reliable
                // across every network profile, while this authenticated
                // WebSocket already proves the browser's reachable LAN IP.
                // Preserve the ICE port/priority and substitute that observed
                // address so the Host can check the real peer immediately.
                info!(%session_id, "rewrote browser mDNS candidate from websocket peer address");
            }
            let message = ServerSignal::WebrtcIce {
                session_id,
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment,
            };
            match principal.role {
                Role::User => {
                    state
                        .route_to_device(session_id, &principal.subject, message)
                        .await
                }
                Role::Device => {
                    state
                        .route_to_browser(session_id, &principal.subject, message)
                        .await
                }
            }
        }
        ClientSignal::SessionClose { session_id } => {
            let message = ServerSignal::SessionClosed {
                session_id,
                reason: "peer_closed".into(),
            };
            let routed = match principal.role {
                Role::User => {
                    state
                        .route_to_device(session_id, &principal.subject, message)
                        .await
                }
                Role::Device => {
                    state
                        .route_to_browser(session_id, &principal.subject, message)
                        .await
                }
            };
            if routed {
                state.sessions.write().await.remove(&session_id);
            }
            routed
        }
        ClientSignal::Ping { nonce } => tx.send(ServerSignal::Pong { nonce }).await.is_ok(),
        unauthorized => {
            warn!(subject = %principal.subject, signal = ?unauthorized, "rejected websocket signal");
            false
        }
    }
}

fn rewrite_mdns_candidate(candidate: String, peer_ip: IpAddr) -> (String, bool) {
    let mut fields = candidate
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let Some(address) = fields.get_mut(4) else {
        return (candidate, false);
    };
    if !address.to_ascii_lowercase().ends_with(".local") {
        return (candidate, false);
    }
    *address = peer_ip.to_string();
    (fields.join(" "), true)
}

fn strip_inline_ice_candidates(sdp: String) -> (String, usize) {
    let mut sanitized = String::with_capacity(sdp.len());
    let mut removed = 0;

    for segment in sdp.split_inclusive('\n') {
        let line = segment.trim_end_matches(|character| character == '\r' || character == '\n');
        let normalized = line.trim_start().to_ascii_lowercase();
        if normalized.starts_with("a=candidate:") || normalized == "a=end-of-candidates" {
            removed += 1;
        } else {
            sanitized.push_str(segment);
        }
    }

    if removed == 0 {
        (sdp, 0)
    } else {
        (sanitized, removed)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::{rewrite_mdns_candidate, strip_inline_ice_candidates};

    #[test]
    fn rewrites_chromium_mdns_host_candidate_to_authenticated_peer_ip() {
        let original =
            "candidate:1 1 udp 2122260223 random-name.local 54877 typ host generation 0".to_owned();
        let (rewritten, changed) =
            rewrite_mdns_candidate(original, IpAddr::V4(Ipv4Addr::new(192, 168, 0, 42)));

        assert!(changed);
        assert_eq!(
            rewritten,
            "candidate:1 1 udp 2122260223 192.168.0.42 54877 typ host generation 0"
        );
    }

    #[test]
    fn preserves_non_mdns_and_related_addresses() {
        let srflx = "candidate:2 1 udp 1686052607 203.0.113.2 60000 typ srflx raddr machine.local rport 54877"
            .to_owned();
        let (candidate, changed) =
            rewrite_mdns_candidate(srflx.clone(), IpAddr::V4(Ipv4Addr::new(192, 168, 0, 42)));

        assert!(!changed);
        assert_eq!(candidate, srflx);
    }

    #[test]
    fn strips_inline_candidates_and_end_marker_from_browser_offer() {
        let original = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\n",
            "a=ice-options:trickle\r\n",
            "a=candidate:1 1 udp 2122260223 hidden.local 54877 typ host\r\n",
            "a=CANDIDATE:2 1 udp 1686052607 203.0.113.2 60000 typ srflx\r\n",
            "a=end-of-candidates\r\n",
            "a=sendrecv\r\n",
        )
        .to_owned();

        let (sanitized, removed) = strip_inline_ice_candidates(original);

        assert_eq!(removed, 3);
        assert_eq!(
            sanitized,
            concat!(
                "v=0\r\n",
                "m=video 9 UDP/TLS/RTP/SAVPF 96\r\n",
                "a=ice-options:trickle\r\n",
                "a=sendrecv\r\n",
            )
        );
    }

    #[test]
    fn preserves_candidate_free_offer_byte_for_byte() {
        let original = "v=0\nm=video 9 UDP/TLS/RTP/SAVPF 96\na=ice-options:trickle".to_owned();

        let (sanitized, removed) = strip_inline_ice_candidates(original.clone());

        assert_eq!(removed, 0);
        assert_eq!(sanitized, original);
    }
}
