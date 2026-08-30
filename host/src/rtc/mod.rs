use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use ::rtc::{
    interceptor::Registry,
    media_stream::MediaStreamTrack,
    peer_connection::configuration::{
        RTCConfigurationBuilder,
        interceptor_registry::register_default_interceptors,
        media_engine::{MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine},
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
use remote_protocol::signaling::ClientSignal;
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

use crate::{audio, config::HostConfig, input, stats::HostStats, video};

#[derive(Clone)]
struct Handler {
    session_id: Uuid,
    outbound: mpsc::Sender<ClientSignal>,
    runtime: Arc<dyn Runtime>,
    stats: Arc<HostStats>,
    media_active: Arc<AtomicBool>,
    media_state: watch::Sender<MediaState>,
}

pub struct AcceptedSession {
    pub peer: Arc<dyn PeerConnection>,
    media_active: Arc<AtomicBool>,
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
        let _ = self
            .outbound
            .send(ClientSignal::WebrtcIce {
                session_id: self.session_id,
                candidate: candidate.candidate,
                sdp_mid: candidate.sdp_mid,
                sdp_mline_index: candidate.sdp_mline_index,
                username_fragment: candidate.username_fragment,
            })
            .await;
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        info!(session_id = %self.session_id, %state, "peer connection state changed");
        match state {
            RTCPeerConnectionState::Connected => {
                self.media_active.store(true, Ordering::Release);
                self.media_state.send_replace(MediaState::RUNNING);
            }
            RTCPeerConnectionState::Disconnected => {
                // Stop capture, encoding and loopback immediately while the peer is
                // disconnected. A later Connected event restarts the sources on demand.
                self.media_active.store(false, Ordering::Release);
                self.media_state.send_replace(MediaState::WAITING);
            }
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                self.media_active.store(false, Ordering::Release);
                self.media_state.send_replace(MediaState::STOPPED);
            }
            _ => {}
        }
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
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
        runtime.spawn(Box::pin(async move {
            let label = channel.label().await.unwrap_or_default();
            if label != "input-fast" && label != "input-reliable" {
                warn!(%label, "closing unknown data channel");
                let _ = channel.close().await;
                return;
            }
            debug!(%label, "input data channel created");
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnMessage(message) if !message.is_string => {
                        match input::inject_packet(&message.data) {
                            Ok(()) => stats.input_ok(),
                            Err(error) => {
                                stats.input_invalid();
                                warn!(%error, %label, "rejected input packet");
                            }
                        }
                    }
                    DataChannelEvent::OnMessage(_) => warn!(%label, "text input message rejected"),
                    DataChannelEvent::OnClose => break,
                    _ => {}
                }
            }
        }));
    }
}

pub async fn accept_offer(
    config: Arc<HostConfig>,
    session_id: Uuid,
    sdp: String,
    outbound: mpsc::Sender<ClientSignal>,
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
    let ice_servers = config
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
    let (media_state, media_state_rx) = watch::channel(MediaState::WAITING);
    let handler = Arc::new(Handler {
        session_id,
        outbound: outbound.clone(),
        runtime: runtime.clone(),
        stats: stats.clone(),
        media_active: media_active.clone(),
        media_state: media_state.clone(),
    });
    let peer = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(
                RTCConfigurationBuilder::new()
                    .with_ice_servers(ice_servers)
                    .build(),
            )
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_handler(handler)
            .with_runtime(runtime)
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
    outbound
        .send(ClientSignal::WebrtcAnswer {
            session_id,
            sdp: answer.sdp,
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
    Ok(AcceptedSession {
        peer: peer as Arc<dyn PeerConnection>,
        media_active,
        media_state,
    })
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

async fn supervise_video(
    config: Arc<HostConfig>,
    track: Arc<TrackLocalStaticSample>,
    payload_type: PayloadType,
    stats: Arc<HostStats>,
    active: Arc<AtomicBool>,
    mut state: watch::Receiver<MediaState>,
) -> anyhow::Result<()> {
    while wait_until_running(&mut state).await? {
        video::stream(
            config.clone(),
            track.clone(),
            payload_type,
            stats.clone(),
            active.clone(),
        )
        .await?;
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
        audio::stream(track.clone(), payload_type, active.clone()).await?;
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
