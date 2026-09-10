use std::{net::IpAddr, sync::Arc};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use remote_protocol::{
    device::DeviceSummary,
    signaling::{ClientSignal, ServerSignal},
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    auth::{Principal, Role},
    state::{AppState, DeviceConnection},
};

pub async fn serve(socket: WebSocket, state: Arc<AppState>, principal: Principal, peer_ip: IpAddr) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerSignal>(64);

    let writer = tokio::spawn(async move {
        // Keep the authenticated control socket active even when all user
        // input travels over the independent RTC connection. WebSocket Ping
        // is answered by browsers even when background JS timers are throttled.
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let signal = tokio::select! {
                _ = heartbeat.tick() => {
                    if ws_tx.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
                    continue;
                }
                signal = rx.recv() => match signal { Some(signal) => signal, None => break },
            };
            let Ok(json) = serde_json::to_string(&signal) else {
                continue;
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });
    let _ = tx.send(ServerSignal::Ready).await;

    while let Some(result) = ws_rx.next().await {
        let message = match result {
            Ok(Message::Close(frame)) => {
                info!(subject = %principal.subject, code = ?frame.as_ref().map(|frame| frame.code), "signaling websocket close received");
                break;
            }
            Ok(message) => message,
            Err(error) => {
                warn!(subject = %principal.subject, %error, "signaling websocket receive failed");
                break;
            }
        };
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
    info!(subject = %principal.subject, "websocket disconnected");
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
            local_cursor,
            input_sdp,
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
                        local_cursor,
                        input_sdp: input_sdp.map(|sdp| strip_inline_ice_candidates(sdp).0),
                    },
                )
                .await
        }
        ClientSignal::WebrtcAnswer {
            session_id,
            sdp,
            input_control,
            local_cursor,
            input_sdp,
        } if principal.role == Role::Device => {
            state
                .route_to_browser(
                    session_id,
                    &principal.subject,
                    ServerSignal::WebrtcAnswer {
                        session_id,
                        sdp,
                        input_control,
                        local_cursor,
                        input_sdp,
                    },
                )
                .await
        }
        ClientSignal::InputPacket { session_id, data } if principal.role == Role::User => {
            if !valid_input_control_packet(&data) {
                return false;
            }
            let bound = state
                .sessions
                .read()
                .await
                .get(&session_id)
                .is_some_and(|session| {
                    session.owner == principal.subject
                        && session
                            .browser_sender
                            .as_ref()
                            .is_some_and(|sender| sender.same_channel(tx))
                });
            if !bound {
                return false;
            }
            state
                .route_to_device(
                    session_id,
                    &principal.subject,
                    ServerSignal::InputPacket { session_id, data },
                )
                .await
        }
        ClientSignal::InputAck { session_id, data } if principal.role == Role::Device => {
            if !valid_input_control_packet(&data) {
                return false;
            }
            let bound = state
                .devices
                .read()
                .await
                .get(&principal.subject)
                .and_then(|device| device.sender.as_ref())
                .is_some_and(|sender| sender.same_channel(tx));
            if !bound {
                return false;
            }
            state
                .route_to_browser(
                    session_id,
                    &principal.subject,
                    ServerSignal::InputAck { session_id, data },
                )
                .await
        }
        ClientSignal::WebrtcIce {
            session_id,
            candidate,
            sdp_mid,
            sdp_mline_index,
            username_fragment,
            input,
        } => {
            let (candidate, mdns_rewritten) = match principal.role {
                Role::User => rewrite_mdns_candidate(candidate, peer_ip),
                Role::Device => (candidate, false),
            };
            if mdns_rewritten {
                // macOS Chromium masks its LAN address behind a randomized
                // `.local` hostname. Windows mDNS resolution is not reliable
                // across every network profile. For a private LAN peer only,
                // try the observed IP while preserving ICE port/priority. It
                // is still a candidate, not proof of UDP reachability.
                info!(%session_id, "rewrote browser mDNS candidate from websocket peer address");
            }
            let message = ServerSignal::WebrtcIce {
                session_id,
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment,
                input,
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
        _ => {
            warn!(subject = %principal.subject, "rejected websocket signal");
            false
        }
    }
}

fn valid_input_control_packet(data: &[u8]) -> bool {
    data.len() <= 18 && remote_protocol::input::TimedInputEvent::decode(data).is_ok()
}

fn rewrite_mdns_candidate(candidate: String, peer_ip: IpAddr) -> (String, bool) {
    // FRP commonly connects to signaling over loopback. That address belongs
    // to the proxy, not the browser. A public TCP source also cannot supply a
    // browser's private UDP port mapping; let STUN/TURN discover it instead.
    let is_lan_peer = match peer_ip {
        IpAddr::V4(ip) => ip.is_private(),
        IpAddr::V6(ip) => ip.is_unique_local(),
    };
    if !is_lan_peer {
        return (candidate, false);
    }
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

    #[tokio::test]
    async fn control_input_requires_bound_live_session_and_correct_socket_and_role() {
        use super::*;
        use crate::auth::AuthConfig;
        use remote_protocol::{
            device::DeviceCapabilities,
            input::{InputEvent, TimedInputEvent},
        };
        let state = Arc::new(AppState::new(AuthConfig::for_tests()));
        let user = Principal {
            subject: "user".into(),
            role: Role::User,
        };
        let device = Principal {
            subject: "host".into(),
            role: Role::Device,
        };
        let (device_tx, mut device_rx) = mpsc::channel(16);
        let (browser_tx, mut browser_rx) = mpsc::channel(16);
        let (other_tx, _other_rx) = mpsc::channel(16);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(
            handle_signal(
                &state,
                &device,
                &device_tx,
                ip,
                ClientSignal::DeviceRegister {
                    device_id: "host".into(),
                    name: "test".into(),
                    capabilities: DeviceCapabilities::default()
                }
            )
            .await
        );
        let session = state.create_session("user", "host").await.unwrap();
        device_rx.recv().await.unwrap();
        let data = TimedInputEvent {
            flags: 1,
            timestamp_us: 123,
            event: InputEvent::MouseRelative { dx: 0, dy: 0 },
        }
        .encode();
        let packet = ClientSignal::InputPacket {
            session_id: session.session_id,
            data: data.clone(),
        };
        assert!(!handle_signal(&state, &user, &browser_tx, ip, packet.clone()).await);
        assert!(
            state
                .authorize_offer(
                    session.session_id,
                    "user",
                    &session.session_token,
                    browser_tx.clone()
                )
                .await
        );
        assert!(!handle_signal(&state, &user, &other_tx, ip, packet.clone()).await);
        assert!(!handle_signal(&state, &device, &device_tx, ip, packet.clone()).await);
        assert!(handle_signal(&state, &user, &browser_tx, ip, packet.clone()).await);
        assert!(
            matches!(device_rx.recv().await, Some(ServerSignal::InputPacket { data: received, .. }) if received == data)
        );
        let ack = ClientSignal::InputAck {
            session_id: session.session_id,
            data: data.clone(),
        };
        assert!(!handle_signal(&state, &user, &browser_tx, ip, ack.clone()).await);
        assert!(!handle_signal(&state, &device, &other_tx, ip, ack.clone()).await);
        assert!(handle_signal(&state, &device, &device_tx, ip, ack).await);
        assert!(
            matches!(browser_rx.recv().await, Some(ServerSignal::InputAck { data: received, .. }) if received == data)
        );
        assert!(
            !handle_signal(
                &state,
                &user,
                &browser_tx,
                ip,
                ClientSignal::InputPacket {
                    session_id: session.session_id,
                    data: vec![0; 19]
                }
            )
            .await
        );
        state.sessions.write().await.remove(&session.session_id);
        assert!(!handle_signal(&state, &user, &browser_tx, ip, packet).await);
    }

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
    fn does_not_replace_mdns_with_frp_loopback_or_public_tcp_address() {
        let original = "candidate:1 1 udp 2122260223 browser.local 54877 typ host";
        for address in [
            "127.0.0.1",
            "::1",
            "203.0.113.10",
            "0.0.0.0",
            "::ffff:127.0.0.1",
        ] {
            let (candidate, changed) =
                rewrite_mdns_candidate(original.into(), address.parse().unwrap());
            assert!(
                !changed,
                "incorrectly rewrote a proxy/NAT candidate: {address}"
            );
            assert_eq!(candidate, original);
        }
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
