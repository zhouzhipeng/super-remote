use std::sync::Arc;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use remote_protocol::signaling::{ClientSignal, ServerSignal};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::config::HostConfig;

#[derive(Deserialize)]
struct TicketResponse {
    ticket: String,
}

pub async fn connect(
    config: Arc<HostConfig>,
    mut outbound: mpsc::Receiver<ClientSignal>,
) -> anyhow::Result<mpsc::Receiver<ServerSignal>> {
    let client = reqwest::Client::new();
    let ticket: TicketResponse = client
        .post(format!(
            "{}/api/device-ticket",
            config.server_url.trim_end_matches('/')
        ))
        .header("x-device-id", &config.device_id)
        .header("x-device-token", &config.device_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mut url = Url::parse(&format!(
        "{}/api/ws",
        config.server_url.trim_end_matches('/')
    ))?;
    url.set_scheme(match url.scheme() {
        "https" => "wss",
        _ => "ws",
    })
    .map_err(|_| anyhow::anyhow!("invalid server URL scheme"))?;
    url.query_pairs_mut().append_pair("ticket", &ticket.ticket);
    let (socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .context("websocket connection failed")?;
    let (mut writer, mut reader) = socket.split();
    let (inbound_tx, inbound_rx) = mpsc::channel(128);

    tokio::spawn(async move {
        while let Some(signal) = outbound.recv().await {
            let Ok(json) = serde_json::to_string(&signal) else {
                continue;
            };
            if writer.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(Ok(message)) = reader.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            if let Ok(signal) = serde_json::from_str::<ServerSignal>(&text)
                && inbound_tx.send(signal).await.is_err()
            {
                break;
            }
        }
    });
    Ok(inbound_rx)
}
