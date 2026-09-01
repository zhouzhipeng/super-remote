use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use remote_protocol::{
    device::DeviceSummary,
    signaling::{ClientSignal, ServerSignal},
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::{
    auth::{Principal, Role},
    state::{AppState, DeviceConnection},
};

pub async fn serve(socket: WebSocket, state: Arc<AppState>, principal: Principal) {
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
        if !handle_signal(&state, &principal, &tx, signal).await {
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
                return false;
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
