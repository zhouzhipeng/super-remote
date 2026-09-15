use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use ::rtc::{
    ice::mdns::MulticastDnsMode,
    interceptor::Registry,
    media_stream::MediaStreamTrack,
    peer_connection::configuration::{
        RTCConfigurationBuilder,
        interceptor_registry::register_default_interceptors,
        media_engine::{MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine},
        setting_engine::SettingEngine,
    },
    peer_connection::{sdp::RTCSessionDescription, transport::RTCIceServer},
    rtp_transceiver::{
        PayloadType,
        rtp_sender::{
            RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
            RtpCodecKind,
        },
    },
};
use remote_protocol::{
    clipboard::{ClipboardRequest, ClipboardResponse, MAX_CLIPBOARD_TEXT_BYTES},
    signaling::ClientSignal,
};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};
use uuid::Uuid;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    media_stream::track_local::{TrackLocal, static_sample::TrackLocalStaticSample},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
        RTCPeerConnectionIceEvent, RTCPeerConnectionState,
    },
    rtp_transceiver::RtpSender,
    runtime::{Runtime, default_runtime},
};

use crate::{
    audio, clipboard, config::HostConfig, control::ControlStatus, input, stats::HostStats, video,
};

#[derive(Clone)]
struct Handler {
    session_id: Uuid,
    outbound: mpsc::Sender<ClientSignal>,
    runtime: Arc<dyn Runtime>,
    stats: Arc<HostStats>,
    media_active: Arc<AtomicBool>,
    retired: Arc<AtomicBool>,
    media_state: watch::Sender<MediaState>,
    control: Arc<ControlStatus>,
    input_channels: Arc<AtomicU8>,
    input_state: Arc<std::sync::Mutex<input::SessionInput>>,
    local_cursor: bool,
    control_only: bool,
    tile_mode: watch::Sender<Option<bool>>,
    tile_eligible: bool,
}

pub struct AcceptedSession {
    pub peer: Arc<dyn PeerConnection>,
    pub input_peer: Option<Arc<dyn PeerConnection>>,
    pub input_control: mpsc::Sender<Vec<u8>>,
    media_active: Arc<AtomicBool>,
    retired: Arc<AtomicBool>,
    media_state: watch::Sender<MediaState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MediaState {
    running: bool,
    stopped: bool,
}

impl MediaState {
    const WAITING: Self = Self {
        running: false,
        stopped: false,
    };
    const RUNNING: Self = Self {
        running: true,
        stopped: false,
    };
    const STOPPED: Self = Self {
        running: false,
        stopped: true,
    };
}

impl AcceptedSession {
    pub fn stop_media(&self) {
        if let Some(peer) = self.input_peer.clone() {
            tokio::spawn(async move {
                let _ = peer.close().await;
            });
        }
        // Retirement is permanent. A late Connected callback from a peer that
        // is being replaced must never reopen capture or the hardware encoder.
        self.retired.store(true, Ordering::Release);
        self.media_active.store(false, Ordering::Release);
        self.media_state.send_replace(MediaState::STOPPED);
    }
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let Ok(candidate) = event.candidate.to_json() else {
            return;
        };
        let kind = candidate
            .candidate
            .split_ascii_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find(|pair| pair[0].eq_ignore_ascii_case("typ"))
            .map(|pair| pair[1].to_owned())
            .unwrap_or_else(|| "unknown".to_owned());
        let transport = candidate
            .candidate
            .split_ascii_whitespace()
            .nth(2)
            .unwrap_or("unknown")
            .to_owned();
        info!(
            session_id = %self.session_id,
            direction = "host_to_browser",
            %transport,
            %kind,
            "sending ICE candidate"
        );
        let _ = self
            .outbound
            .send(ClientSignal::WebrtcIce {
                session_id: self.session_id,
                candidate: candidate.candidate,
                sdp_mid: candidate.sdp_mid,
                sdp_mline_index: candidate.sdp_mline_index,
                username_fragment: candidate.username_fragment,
                input: self.control_only,
            })
            .await;
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if self.control_only {
            return;
        }
        info!(session_id = %self.session_id, %state, "peer connection state changed");
        match state {
            RTCPeerConnectionState::Connected => {
                if self.retired.load(Ordering::Acquire) {
                    self.media_active.store(false, Ordering::Release);
                    self.media_state.send_replace(MediaState::STOPPED);
                    return;
                }
                self.media_active.store(true, Ordering::Release);
                self.media_state.send_replace(MediaState::RUNNING);
                // stop_media() can race this callback between the first check
                // and the writes above. Recheck so retirement always wins.
                if self.retired.load(Ordering::Acquire) {
                    self.media_active.store(false, Ordering::Release);
                    self.media_state.send_replace(MediaState::STOPPED);
                    return;
                }
                self.control.connected(self.session_id);
                #[cfg(windows)]
                {
                    let session_id = self.session_id;
                    drop(tokio::task::spawn_blocking(move || {
                        match crate::window_layout::move_secondary_windows_to_primary() {
                            Ok(summary) => info!(
                                %session_id,
                                monitors = summary.monitor_count,
                                candidates = summary.candidates,
                                moved = summary.moved,
                                failed = summary.failed,
                                "moved secondary-monitor application windows to the primary monitor"
                            ),
                            Err(error) => warn!(
                                %session_id,
                                %error,
                                "failed to consolidate application windows on the primary monitor"
                            ),
                        }
                    }));
                }
            }
            RTCPeerConnectionState::Disconnected => {
                // Stop capture, encoding and loopback immediately while the peer is
                // disconnected. A later Connected event restarts the sources on demand.
                self.media_active.store(false, Ordering::Release);
                self.media_state.send_replace(MediaState::WAITING);
                self.control.disconnected(self.session_id);
            }
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                self.media_active.store(false, Ordering::Release);
                self.media_state.send_replace(MediaState::STOPPED);
                self.control.disconnected(self.session_id);
            }
            _ => {}
        }
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) && !self.retired.load(Ordering::Acquire)
        {
            let _ = self
                .outbound
                .send(ClientSignal::SessionClose {
                    session_id: self.session_id,
                })
                .await;
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let runtime = self.runtime.clone();
        let stats = self.stats.clone();
        let input_channels = self.input_channels.clone();
        let retired = self.retired.clone();
        let input_state = self.input_state.clone();
        let local_cursor = self.local_cursor;
        let tile_mode = self.tile_mode.clone();
        let tile_eligible = self.tile_eligible && !self.control_only;
        let tile_active = self.media_active.clone();
        runtime.spawn(Box::pin(async move {
            let label = channel.label().await.unwrap_or_default();
            #[cfg(windows)]
            if label == "desktop-refinement-v3" && tile_eligible {
                if input_channels.fetch_or(4, Ordering::AcqRel) & 4 != 0 {
                    let _ = channel.close().await;
                    return;
                }
                let handshake = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        match channel.poll().await {
                            Some(DataChannelEvent::OnMessage(message))
                                if message.is_string && message.data.as_ref() == b"start" =>
                            {
                                return true;
                            }
                            None | Some(DataChannelEvent::OnClose | DataChannelEvent::OnError) => {
                                return false;
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .unwrap_or(false);
                if handshake {
                    let _ = tile_mode.send(Some(true));
                    // Start capture in its final mode, but do not expose PNG
                    // overlays until the browser has actually presented video.
                    let video_ready = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                        loop {
                            match channel.poll().await {
                                Some(DataChannelEvent::OnMessage(message)) if message.is_string && message.data.as_ref() == b"video-ready" => return true,
                                None | Some(DataChannelEvent::OnClose | DataChannelEvent::OnError) => return false,
                                _ => {}
                            }
                        }
                    }).await.unwrap_or(false);
                    if !video_ready {
                        let _ = tile_mode.send(Some(false));
                        let _ = channel.close().await;
                        return;
                    }
                    if let Err(error) = crate::desktop_tiles::serve(
                        channel.clone(),
                        retired,
                        tile_active,
                        input_state.clone(),
                    )
                    .await
                    {
                        warn!(%error, "idle refinement stopped; restoring full-resolution video");
                    }
                    let _ = tile_mode.send(Some(false));
                }
                let _ = channel.close().await;
                return;
            }
            if label == "cursor" && local_cursor {
                #[cfg(windows)]
                crate::cursor::serve(channel, retired).await;
                return;
            }
            if label == "clipboard" {
                debug!(%label, "clipboard data channel created");
                serve_clipboard(channel).await;
                return;
            }
            if label != "input-fast" && label != "input-reliable" {
                warn!(%label, "closing unknown data channel");
                let _ = channel.close().await;
                return;
            }
            debug!(%label, "input data channel created");
            let bit = if label == "input-fast" { 1 } else { 2 };
            if input_channels.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
                let _ = channel.close().await;
                return;
            }
            let worker_channel = channel.clone();
            let worker = async move {
                let channel = worker_channel;
                while let Some(event) = channel.poll().await {
                    if retired.load(Ordering::Acquire) {
                        break;
                    }
                    match event {
                        DataChannelEvent::OnMessage(message) if !message.is_string => {
                            let result = input_state.lock().unwrap().inject(&message.data);
                            match result {
                                Ok(Some(input)) => {
                                    stats.input_ok();
                                    // Bit 0 requests a tiny post-injection echo. It lets the
                                    // browser report real input round-trip latency without
                                    // delaying this receive loop or every high-rate move.
                                    if input.flags & 1 != 0 {
                                        let _ = channel.try_send(message.data.clone()).await;
                                    }
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    stats.input_invalid();
                                    warn!(%error, %label, "rejected input packet");
                                }
                            }
                        }
                        DataChannelEvent::OnMessage(_) => {
                            warn!(%label, "text input message rejected")
                        }
                        DataChannelEvent::OnClose => break,
                        _ => {}
                    }
                }
                if bit == 2 {
                    input_state.lock().unwrap().release_rtc();
                }
            };
            if let Err(error) = input::spawn_priority(format!("remote-input-{bit}"), worker) {
                warn!(%error, "could not start dedicated input receiver");
                let _ = channel.close().await;
            }
        }));
    }
}

async fn serve_clipboard(channel: Arc<dyn DataChannel>) {
    let mut image_upload = Vec::new();
    while let Some(event) = channel.poll().await {
        match event {
            DataChannelEvent::OnMessage(message) if message.is_string => {
                let response = match String::from_utf8(message.data.to_vec())
                    .map_err(anyhow::Error::from)
                    .and_then(|json| {
                        serde_json::from_str::<ClipboardRequest>(&json).map_err(Into::into)
                    }) {
                    Ok(request) => {
                        let id = request.id();
                        let image_result = if let ClipboardRequest::ImageChunk {
                            data,
                            start,
                            last,
                            paste,
                            ..
                        } = &request
                        {
                            use base64::Engine;
                            if *start {
                                image_upload.clear();
                            }
                            let result = (|| -> anyhow::Result<Option<(Vec<u8>, bool)>> {
                                anyhow::ensure!(data.len() <= 12 * 1024, "image chunk too large");
                                let bytes =
                                    base64::engine::general_purpose::STANDARD.decode(data)?;
                                anyhow::ensure!(
                                    image_upload.len() + bytes.len()
                                        <= crate::clipboard_image::MAX_IMAGE_BYTES,
                                    "image exceeds 16 MiB"
                                );
                                image_upload.extend(bytes);
                                Ok(if *last {
                                    Some((std::mem::take(&mut image_upload), *paste))
                                } else {
                                    None
                                })
                            })();
                            if result.is_err() {
                                image_upload.clear();
                            }
                            Some(result)
                        } else {
                            None
                        };
                        match tokio::task::spawn_blocking(move || {
                            if let Some(result) = image_result {
                                if let Some((png, paste)) = result? {
                                    crate::clipboard_image::write_png(&png)?;
                                    if paste {
                                        input::paste_clipboard()?;
                                    }
                                }
                                return Ok(ClipboardResponse::Ack { id });
                            }
                            process_clipboard_request(request)
                        })
                        .await
                        {
                            Ok(Ok(response)) => response,
                            Ok(Err(error)) => ClipboardResponse::Error {
                                id,
                                message: error.to_string(),
                            },
                            Err(error) => ClipboardResponse::Error {
                                id,
                                message: format!("clipboard worker failed: {error}"),
                            },
                        }
                    }
                    Err(error) => ClipboardResponse::Error {
                        id: 0,
                        message: format!("invalid clipboard request: {error}"),
                    },
                };
                match serde_json::to_string(&response) {
                    Ok(json) => {
                        if let Err(error) = channel.send_text(&json).await {
                            warn!(%error, "failed to send clipboard response");
                            break;
                        }
                    }
                    Err(error) => warn!(%error, "failed to encode clipboard response"),
                }
            }
            DataChannelEvent::OnMessage(_) => warn!("binary clipboard message rejected"),
            DataChannelEvent::OnClose => break,
            _ => {}
        }
    }
}

fn process_clipboard_request(request: ClipboardRequest) -> anyhow::Result<ClipboardResponse> {
    match request {
        ClipboardRequest::Read { id } => {
            let text = clipboard::read_text()?;
            anyhow::ensure!(
                text.len() <= MAX_CLIPBOARD_TEXT_BYTES,
                "主机剪贴板文本超过 {} KiB 限制",
                MAX_CLIPBOARD_TEXT_BYTES / 1024
            );
            info!(bytes = text.len(), "read Host clipboard for Web client");
            Ok(ClipboardResponse::Content { id, text })
        }
        ClipboardRequest::Write { id, text, paste } => {
            anyhow::ensure!(
                text.len() <= MAX_CLIPBOARD_TEXT_BYTES,
                "剪贴板文本超过 {} KiB 限制",
                MAX_CLIPBOARD_TEXT_BYTES / 1024
            );
            clipboard::write_text(&text)?;
            if paste {
                input::paste_text(&text)?;
            }
            info!(bytes = text.len(), paste, "wrote Web clipboard to Host");
            Ok(ClipboardResponse::Ack { id })
        }
        ClipboardRequest::ImageChunk { .. } => anyhow::bail!("image chunk requires session state"),
        ClipboardRequest::Paste { id } => {
            if crate::clipboard_image::read_png()?.is_some() {
                input::paste_clipboard()?;
                return Ok(ClipboardResponse::Ack { id });
            }
            let text = clipboard::read_text()?;
            anyhow::ensure!(
                text.len() <= MAX_CLIPBOARD_TEXT_BYTES,
                "主机剪贴板文本超过 {} KiB 限制",
                MAX_CLIPBOARD_TEXT_BYTES / 1024
            );
            input::paste_text(&text)?;
            info!(
                bytes = text.len(),
                "pasted existing Host clipboard as Unicode text"
            );
            Ok(ClipboardResponse::Ack { id })
        }
    }
}

pub async fn accept_offer(
    config: Arc<HostConfig>,
    session_id: Uuid,
    sdp: String,
    outbound: mpsc::Sender<ClientSignal>,
    control: Arc<ControlStatus>,
    input_sdp: Option<String>,
) -> anyhow::Result<AcceptedSession> {
    info!(
        %session_id,
        bitrate = config.bitrate,
        fps = config.fps,
        "configuring H.264 session"
    );
    let runtime =
        default_runtime().ok_or_else(|| anyhow::anyhow!("webrtc runtime is not enabled"))?;
    let stats = Arc::new(HostStats::default());
    let (_, level_idc) = config.h264_level();
    let codec = h264_codec(
        config.h264_file.is_none() && config.ffmpeg_path.is_none(),
        level_idc,
    );
    let audio_codec = opus_codec();
    let mut media_engine = MediaEngine::default();
    media_engine.register_codec(codec.clone(), RtpCodecKind::Video)?;
    media_engine.register_codec(audio_codec.clone(), RtpCodecKind::Audio)?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
    let ice_servers: Vec<RTCIceServer> = config
        .ice_servers
        .iter()
        .map(|server| RTCIceServer {
            urls: server.urls.clone(),
            username: server.username.clone(),
            credential: server.credential.clone(),
        })
        .collect();
    // Negotiation itself must stay idle. The event handler opens the media gate
    // only after ICE/DTLS reports a genuinely connected peer.
    let media_active = Arc::new(AtomicBool::new(false));
    let retired = Arc::new(AtomicBool::new(false));
    let (media_state, media_state_rx) = watch::channel(MediaState::WAITING);
    let input_state = Arc::new(std::sync::Mutex::new(input::SessionInput::with_wheel_step(
        config.wheel_step,
    )));
    let tile_eligible = config.local_cursor && config.h264_file.is_none() && config.monitor_index == 0;
    let (tile_mode, tile_mode_rx) = watch::channel(if tile_eligible { None } else { Some(false) });
    let handler = Arc::new(Handler {
        session_id,
        outbound: outbound.clone(),
        runtime: runtime.clone(),
        stats: stats.clone(),
        media_active: media_active.clone(),
        retired: retired.clone(),
        media_state: media_state.clone(),
        control,
        input_channels: Arc::new(AtomicU8::new(0)),
        input_state: input_state.clone(),
        local_cursor: config.local_cursor,
        control_only: false,
        tile_mode,
        tile_eligible: config.local_cursor
            && config.h264_file.is_none()
            && config.monitor_index == 0,
    });
    let peer = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(
                RTCConfigurationBuilder::new()
                    .with_ice_servers(ice_servers.clone())
                    .build(),
            )
            // Chromium on macOS publishes its LAN address as an mDNS `.local`
            // candidate. The async wrapper defaults its network driver to mDNS
            // Disabled even though rtc::SettingEngine defaults to QueryOnly.
            // Pass the engine explicitly so the Host resolves Chrome's LAN
            // candidate instead of depending on unreliable NAT hairpinning.
            .with_setting_engine(host_setting_engine())
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_handler(handler.clone())
            .with_runtime(runtime.clone())
            // Chrome uses the bundled TURN/TCP server when its direct WebRTC UDP
            // path is blocked. That relay runs on this Windows host; use ordinary
            // UDP datagrams so loopback never receives an unsegmented GSO aggregate.
            .with_udp_gso_enabled(false)
            .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
            .build()
            .await?,
    );

    let video_track = Arc::new(TrackLocalStaticSample::new(MediaStreamTrack::new(
        "remote-desktop".into(),
        "desktop-video".into(),
        "desktop-video".into(),
        RtpCodecKind::Video,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(rand::random::<u32>()),
                ..Default::default()
            },
            codec: codec.rtp_codec.clone(),
            ..Default::default()
        }],
    ))?);
    let video_sender = peer
        .add_track(video_track.clone() as Arc<dyn TrackLocal>)
        .await?;
    let audio_track = Arc::new(TrackLocalStaticSample::new(MediaStreamTrack::new(
        "remote-desktop".into(),
        "desktop-audio".into(),
        "desktop-audio".into(),
        RtpCodecKind::Audio,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(rand::random::<u32>()),
                ..Default::default()
            },
            codec: audio_codec.rtp_codec.clone(),
            ..Default::default()
        }],
    ))?);
    let audio_sender = peer
        .add_track(audio_track.clone() as Arc<dyn TrackLocal>)
        .await?;
    let offer: RTCSessionDescription =
        serde_json::from_value(serde_json::json!({ "type": "offer", "sdp": sdp }))?;
    peer.set_remote_description(offer).await?;
    let answer = peer.create_answer(None).await?;
    peer.set_local_description(answer).await?;
    let answer = peer
        .local_description()
        .await
        .ok_or_else(|| anyhow::anyhow!("local answer is missing"))?;
    let mut input_peer: Option<Arc<dyn PeerConnection>> = None;
    let mut input_answer = None;
    if let Some(sdp) = input_sdp {
        let mut input_handler = (*handler).clone();
        input_handler.control_only = true;
        let control_peer = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(
                    RTCConfigurationBuilder::new()
                        .with_ice_servers(ice_servers)
                        .build(),
                )
                .with_setting_engine(host_setting_engine())
                .with_handler(Arc::new(input_handler))
                .with_runtime(runtime)
                .with_udp_gso_enabled(false)
                .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
                .build()
                .await?,
        );
        let offer: RTCSessionDescription =
            serde_json::from_value(serde_json::json!({"type": "offer", "sdp": sdp}))?;
        control_peer.set_remote_description(offer).await?;
        let answer = control_peer.create_answer(None).await?;
        control_peer.set_local_description(answer).await?;
        input_answer = control_peer
            .local_description()
            .await
            .map(|answer| answer.sdp);
        input_peer = Some(control_peer);
    }
    outbound
        .send(ClientSignal::WebrtcAnswer {
            session_id,
            sdp: answer.sdp,
            input_control: true,
            local_cursor: config.local_cursor,
            input_sdp: input_answer,
        })
        .await?;

    let video_payload_type = negotiated_payload_type(&video_sender).await?;
    let audio_payload_type = negotiated_payload_type(&audio_sender).await?;
    let stream_active = media_active.clone();
    let video_state = media_state_rx.clone();
    tokio::spawn(async move {
        if let Err(error) = supervise_video(
            config,
            video_track,
            video_payload_type,
            stats,
            stream_active,
            video_state,
            tile_mode_rx,
        )
        .await
        {
            warn!(%session_id, %error, "video source supervisor stopped");
        }
    });
    let audio_active = media_active.clone();
    tokio::spawn(async move {
        if let Err(error) = supervise_audio(
            audio_track,
            audio_payload_type,
            audio_active,
            media_state_rx,
        )
        .await
        {
            warn!(%session_id, %error, "audio source supervisor stopped");
        }
    });
    let (input_control, mut input_rx) = mpsc::channel::<Vec<u8>>(128);
    let input_retired = retired.clone();
    let input_active = media_active.clone();
    // This task outlives every individual input channel, so it also owns paced
    // wheel injection for the whole session. Wheel packets arrive on the RTC
    // reliable channel, which is a different task: the wake keeps this one from
    // polling an idle pointer without letting a queued burst stall.
    let wheel_wake = input_state.lock().unwrap().wheel_wake();
    input::spawn_priority("remote-input-control".into(), async move {
        loop {
            if input_retired.load(Ordering::Acquire) {
                break;
            }
            let pending = input_state.lock().unwrap().wheel_pending();
            let wheel = async {
                if pending {
                    tokio::time::sleep(input::WHEEL_TICK).await;
                } else {
                    wheel_wake.notified().await;
                }
            };
            tokio::select! {
                data = input_rx.recv() => {
                    let Some(data) = data else { break };
                    if input_retired.load(Ordering::Acquire) {
                        break;
                    }
                    if !input_active.load(Ordering::Acquire) {
                        continue;
                    }
                    let result = input_state.lock().unwrap().inject_control(&data);
                    match result {
                        Ok(Some(event)) => {
                            if event.flags & 1 != 0 {
                                let _ = outbound.try_send(ClientSignal::InputAck { session_id, data });
                            }
                        }
                        Ok(None) => {}
                        Err(error) => warn!(%error, "rejected control input packet"),
                    }
                }
                // A wake with nothing queued costs one lock and returns.
                () = wheel => input_state.lock().unwrap().drain_wheel(),
            }
        }
        input_state.lock().unwrap().release_all();
    })?;
    Ok(AcceptedSession {
        input_peer,
        input_control,
        peer: peer as Arc<dyn PeerConnection>,
        media_active,
        retired,
        media_state,
    })
}

fn host_setting_engine() -> SettingEngine {
    let mut settings = SettingEngine::default();
    settings.set_multicast_dns_mode(MulticastDnsMode::QueryOnly);
    settings
}

async fn wait_until_running(state: &mut watch::Receiver<MediaState>) -> anyhow::Result<bool> {
    loop {
        let current = *state.borrow_and_update();
        if current.stopped {
            return Ok(false);
        }
        if current.running {
            return Ok(true);
        }
        state.changed().await?;
    }
}

async fn resolved_video_mode(mode: &mut watch::Receiver<Option<bool>>) -> anyhow::Result<bool> {
    loop {
        if let Some(value) = *mode.borrow_and_update() { return Ok(value); }
        mode.changed().await?;
    }
}

async fn changed_video_mode(mode: &mut watch::Receiver<Option<bool>>, current: bool) -> anyhow::Result<()> {
    loop {
        if resolved_video_mode(mode).await? != current { return Ok(()); }
        mode.changed().await?;
    }
}

async fn supervise_video(
    config: Arc<HostConfig>,
    track: Arc<TrackLocalStaticSample>,
    payload_type: PayloadType,
    stats: Arc<HostStats>,
    active: Arc<AtomicBool>,
    mut state: watch::Receiver<MediaState>,
    mut tile_mode: watch::Receiver<Option<bool>>,
) -> anyhow::Result<()> {
    while wait_until_running(&mut state).await? {
        let mut session_config = (*config).clone();
        session_config.hybrid_video = tokio::time::timeout(
            std::time::Duration::from_secs(5), resolved_video_mode(&mut tile_mode)
        ).await.unwrap_or(Ok(false))?;
        if !active.load(Ordering::Acquire) { continue; }
        let current_mode = session_config.hybrid_video;
        let session_config = Arc::new(session_config);
        let result = tokio::select! {
            result = video::stream(
                session_config.clone(),
                track.clone(),
                payload_type,
                stats.clone(),
                active.clone(),
            ) => result,
            result = changed_video_mode(&mut tile_mode, current_mode) => { result?; continue; },
        };
        if let Err(error) = result {
            // RDP/display changes temporarily invalidate Desktop Duplication.
            // Keep the negotiated peer alive and recreate capture after a delay.
            warn!(%error, "video capture interrupted; retrying in one second");
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
            result = state.changed() => { result?; },
        }
    }
    Ok(())
}

async fn supervise_audio(
    track: Arc<TrackLocalStaticSample>,
    payload_type: PayloadType,
    active: Arc<AtomicBool>,
    mut state: watch::Receiver<MediaState>,
) -> anyhow::Result<()> {
    while wait_until_running(&mut state).await? {
        let result = tokio::select! {
            result = audio::stream(track.clone(), payload_type, active.clone()) => result,
            result = state.changed() => { result?; continue; },
        };
        if let Err(error) = result {
            warn!(%error, "audio capture interrupted; reopening the current playback device");
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {},
            result = state.changed() => { result?; },
        }
    }
    Ok(())
}

fn opus_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.into(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1;sprop-stereo=1".into(),
            rtcp_feedback: vec![],
        },
        payload_type: 111,
    }
}

fn h264_codec(hardware_media_foundation: bool, level_idc: &str) -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.into(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: format!(
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}{}",
                if hardware_media_foundation {
                    "4d00"
                } else {
                    "42e0"
                },
                level_idc,
            ),
            rtcp_feedback: vec![],
        },
        payload_type: 102,
    }
}

async fn negotiated_payload_type(sender: &Arc<dyn RtpSender>) -> anyhow::Result<PayloadType> {
    sender
        .get_parameters()
        .await?
        .rtp_parameters
        .codecs
        .first()
        .map(|codec| codec.payload_type)
        .ok_or_else(|| anyhow::anyhow!("no negotiated video codec"))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn startup_waits_for_mode_and_duplicate_notifications_do_not_restart_capture() {
        let (tx, mut rx) = tokio::sync::watch::channel(None);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(10), super::resolved_video_mode(&mut rx)).await.is_err());
        tx.send(Some(true)).unwrap();
        assert!(super::resolved_video_mode(&mut rx).await.unwrap());
        tx.send(Some(true)).unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(10), super::changed_video_mode(&mut rx, true)).await.is_err());
        tx.send(Some(false)).unwrap();
        super::changed_video_mode(&mut rx, true).await.unwrap();
    }

    use super::{MulticastDnsMode, host_setting_engine};

    #[test]
    fn host_resolves_browser_mdns_candidates() {
        assert_eq!(
            host_setting_engine().multicast_dns().mode,
            MulticastDnsMode::QueryOnly
        );
    }
}
