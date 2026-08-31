use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use audiopus::{Application, Bitrate, Channels, SampleRate, coder::Encoder};
use bytes::Bytes;
use rtc::{media::Sample, rtp_transceiver::PayloadType};
use tokio::sync::mpsc;
use tracing::info;
use wasapi::{
    DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError, WaveFormat, initialize_mta,
};
use webrtc::media_stream::{Track, track_local::static_sample::TrackLocalStaticSample};

const SAMPLE_RATE: usize = 48_000;
const CHANNELS: usize = 2;
const FRAME_DURATION_MS: u64 = 20;
const FRAMES_PER_PACKET: usize = SAMPLE_RATE * FRAME_DURATION_MS as usize / 1_000;
const SAMPLES_PER_PACKET: usize = FRAMES_PER_PACKET * CHANNELS;
const BYTES_PER_PACKET: usize = SAMPLES_PER_PACKET * size_of::<f32>();

pub async fn stream(
    track: Arc<TrackLocalStaticSample>,
    payload_type: PayloadType,
    active: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let (packet_tx, mut packet_rx) = mpsc::channel::<Vec<u8>>(2);
    let capture_active = active.clone();
    std::thread::Builder::new()
        .name("desktop-audio-loopback".into())
        .spawn(move || {
            if let Err(error) = capture_loop(packet_tx, capture_active) {
                tracing::warn!(%error, "WASAPI loopback audio stopped");
            }
        })?;

    let ssrc = track
        .ssrcs()
        .await
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("audio track has no SSRC"))?;
    let duration = Duration::from_millis(FRAME_DURATION_MS);
    while active.load(Ordering::Acquire) {
        let Some(packet) = packet_rx.recv().await else {
            if active.load(Ordering::Acquire) {
                anyhow::bail!("WASAPI loopback audio worker stopped");
            }
            return Ok(());
        };
        let result = track
            .sample_writer(ssrc, payload_type)
            .write_sample(&Sample {
                data: Bytes::from(packet),
                duration,
                ..Default::default()
            })
            .await;
        if let Err(error) = result {
            if !active.load(Ordering::Acquire) {
                return Ok(());
            }
            return Err(error.into());
        }
    }
    Ok(())
}

fn capture_loop(packet_tx: mpsc::Sender<Vec<u8>>, active: Arc<AtomicBool>) -> anyhow::Result<()> {
    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = enumerator.get_default_device(&Direction::Render)?;
    let device_name = device.get_friendlyname()?;
    let mut audio_client = device.get_iaudioclient()?;
    let desired_format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE, CHANNELS, None);
    let (default_period, _) = audio_client.get_device_period()?;
    audio_client.initialize_client(
        &desired_format,
        &Direction::Capture,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: default_period.max(200_000),
        },
    )?;
    let event = audio_client.set_get_eventhandle()?;
    let capture_client = audio_client.get_audiocaptureclient()?;
    let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::LowDelay)?;
    encoder.set_bitrate(Bitrate::BitsPerSecond(128_000))?;
    encoder.set_complexity(5)?;
    let mut bytes = VecDeque::with_capacity(BYTES_PER_PACKET * 3);
    let mut pcm = vec![0.0f32; SAMPLES_PER_PACKET];
    let mut encoded = vec![0u8; 4_000];

    audio_client.start_stream()?;
    info!(device = %device_name, "WASAPI system loopback audio started");
    while active.load(Ordering::Acquire) {
        match event.wait_for_event(100) {
            Ok(()) => {}
            Err(WasapiError::EventTimeout) => continue,
            Err(error) => return Err(error.into()),
        }

        while capture_client.get_next_packet_size()?.unwrap_or(0) > 0 {
            let before = bytes.len();
            let info = capture_client.read_from_device_to_deque(&mut bytes)?;
            if info.flags.data_discontinuity {
                bytes.clear();
                continue;
            }
            if info.flags.silent {
                for value in bytes.iter_mut().skip(before) {
                    *value = 0;
                }
            }
        }

        // Keep at most two complete packets. Old system audio is less useful
        // than current audio in an interactive remote-desktop session.
        while bytes.len() > BYTES_PER_PACKET * 2 {
            for _ in 0..BYTES_PER_PACKET.min(bytes.len()) {
                bytes.pop_front();
            }
        }
        while bytes.len() >= BYTES_PER_PACKET {
            for sample in &mut pcm {
                let raw = [
                    bytes.pop_front().unwrap(),
                    bytes.pop_front().unwrap(),
                    bytes.pop_front().unwrap(),
                    bytes.pop_front().unwrap(),
                ];
                *sample = f32::from_le_bytes(raw);
            }
            let length = encoder.encode_float(&pcm, &mut encoded)?;
            match packet_tx.try_send(encoded[..length].to_vec()) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    audio_client.stop_stream()?;
                    return Ok(());
                }
            }
        }
    }
    audio_client.stop_stream()?;
    Ok(())
}
