mod audio;
mod config;
mod input;
mod rtc;
mod signaling;
mod stats;
mod video;

use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use config::HostConfig;
use remote_protocol::signaling::{ClientSignal, ServerSignal};
use tokio::sync::{Mutex, mpsc};
use tracing::{error, info, warn};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("remote_host=info".parse()?),
        )
        .init();
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "remote-host.toml".into());
    let config =
        Arc::new(HostConfig::load(&path).with_context(|| format!("failed to load {path}"))?);
    let (outbound_tx, outbound_rx) = mpsc::channel::<ClientSignal>(128);
    let sessions: Arc<Mutex<HashMap<Uuid, rtc::AcceptedSession>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let mut inbound = signaling::connect(config.clone(), outbound_rx).await?;
    outbound_tx
        .send(ClientSignal::DeviceRegister {
            device_id: config.device_id.clone(),
            name: config.device_name.clone(),
            capabilities: config.capabilities(),
        })
        .await?;
    info!(device_id = %config.device_id, "host is online");

    while let Some(signal) = inbound.recv().await {
        match signal {
            ServerSignal::SessionRequested { session_id, .. } => {
                info!(%session_id, "session requested");
            }
            ServerSignal::WebrtcOffer { session_id, sdp } => {
                // MVP is a single-controller desktop. A fresh connection replaces stale or
                // background mobile tabs immediately so hardware encoders cannot accumulate.
                let previous = sessions
                    .lock()
                    .await
                    .drain()
                    .map(|(_, item)| item)
                    .collect::<Vec<_>>();
                for item in previous {
                    item.stop_media();
                    let _ = item.peer.close().await;
                }
                match rtc::accept_offer(config.clone(), session_id, sdp, outbound_tx.clone()).await
                {
                    Ok(session) => {
                        sessions.lock().await.insert(session_id, session);
                    }
                    Err(error) => error!(%session_id, %error, "failed to accept offer"),
                }
            }
            ServerSignal::WebrtcIce {
                session_id,
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment,
            } => {
                if let Some(peer) = sessions
                    .lock()
                    .await
                    .get(&session_id)
                    .map(|item| item.peer.clone())
                    && let Err(error) = peer
                        .add_ice_candidate(webrtc::peer_connection::RTCIceCandidateInit {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                            username_fragment,
                            url: None,
                        })
                        .await
                {
                    warn!(%session_id, %error, "failed to add remote ICE candidate");
                }
            }
            ServerSignal::SessionClosed { session_id, reason } => {
                info!(%session_id, %reason, "session closed");
                if let Some(session) = sessions.lock().await.remove(&session_id) {
                    session.stop_media();
                    let _ = session.peer.close().await;
                }
            }
            ServerSignal::Error { code, message } => warn!(%code, %message, "signaling error"),
            _ => {}
        }
    }
    anyhow::bail!("signaling connection closed")
}
