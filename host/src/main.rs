mod audio;
mod config;
mod control;
#[cfg(windows)]
mod display_power;
mod input;
mod rtc;
mod signaling;
mod stats;
mod video;

use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use config::HostConfig;
use control::ControlStatus;
use remote_protocol::signaling::{ClientSignal, ServerSignal};
use tokio::sync::{Mutex, mpsc};
use tracing::{error, info, warn};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    unsafe {
        windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    }
    .context("failed to enable per-monitor V2 DPI awareness")?;
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
    let control = Arc::new(ControlStatus::new(&config));
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
    control.online();

    while let Some(signal) = inbound.recv().await {
        match signal {
            ServerSignal::SessionRequested { session_id, .. } => {
                info!(%session_id, "session requested");
            }
            ServerSignal::WebrtcOffer {
                session_id,
                sdp,
                viewport_width,
                viewport_height,
            } => {
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
                let session_config = config.for_viewport(viewport_width, viewport_height);
                control.preparing(session_id, &session_config);
                match rtc::accept_offer(
                    session_config,
                    session_id,
                    sdp,
                    outbound_tx.clone(),
                    control.clone(),
                )
                .await
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
                control.disconnected(session_id);
            }
            ServerSignal::Error { code, message } => warn!(%code, %message, "signaling error"),
            _ => {}
        }
    }
    control.offline();
    anyhow::bail!("signaling connection closed")
}
