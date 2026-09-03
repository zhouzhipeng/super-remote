use std::{
    fs::File,
    io::{BufReader, Read},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use bytes::{BufMut, Bytes, BytesMut};
use rtc::{
    media::{Sample, io::h26x_reader::H26xSampleReader},
    rtp_transceiver::PayloadType,
};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{info, warn};
use webrtc::media_stream::{Track, track_local::static_sample::TrackLocalStaticSample};

use crate::{config::HostConfig, stats::HostStats};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub async fn stream(
    config: Arc<HostConfig>,
    track: Arc<TrackLocalStaticSample>,
    payload_type: PayloadType,
    stats: Arc<HostStats>,
    active: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    if let Some(path) = config.h264_file.clone() {
        return stream_reader(
            BufReader::new(File::open(path)?),
            track,
            payload_type,
            config.fps,
            stats,
            active,
        )
        .await;
    }
    // Acquiring this guard is intentionally below the prerecorded-file path
    // and inside stream(): rtc only calls stream() once a client is connected.
    // Idle hosts therefore hold no display/system power request.
    #[cfg(windows)]
    let _display_power = crate::display_power::DisplayPowerGuard::acquire()?;
    if let Some(path) = config.ffmpeg_path.clone() {
        return stream_ffmpeg(path, config, track, payload_type, stats, active).await;
    }
    #[cfg(windows)]
    return stream_windows_hardware(config, track, payload_type, stats, active).await;
    #[cfg(not(windows))]
    anyhow::bail!(
        "Windows hardware capture is unavailable; configure h264_file for transport testing"
    )
}

async fn stream_reader<R: Read + Send>(
    reader: R,
    track: Arc<TrackLocalStaticSample>,
    payload_type: PayloadType,
    fps: u16,
    stats: Arc<HostStats>,
    active: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let duration = Duration::from_secs_f64(1.0 / f64::from(fps));
    let ssrc = track_ssrc(&track).await?;
    let mut reader = H26xSampleReader::new(reader, 4 * 1024 * 1024, false);
    let start = tokio::time::Instant::now();
    let mut frame_number = 0u32;
    while active.load(Ordering::Acquire) {
        let sample = reader.next_sample()?;
        // rtc-media correctly excludes parameter sets and SEI from timing, but
        // NVENC CBR also emits one filler-data NAL (type 12) per frame. Treating
        // that filler as a picture halves the effective frame rate. Only VCL
        // slice NALs advance the H.264 presentation clock.
        let advances_time = sample.timed && h264_vcl_nal(&sample.data);
        track
            .sample_writer(ssrc, payload_type)
            .write_sample(&Sample {
                data: sample.data,
                duration: if advances_time {
                    duration
                } else {
                    Duration::ZERO
                },
                ..Default::default()
            })
            .await?;
        if advances_time {
            stats.frames_sent.fetch_add(1, Ordering::Relaxed);
            frame_number += 1;
            tokio::time::sleep_until(start + duration.mul_f64(f64::from(frame_number))).await;
        }
    }
    Ok(())
}

fn h264_vcl_nal(data: &[u8]) -> bool {
    data.first()
        .is_some_and(|header| matches!(header & 0x1f, 1 | 5))
}

async fn stream_ffmpeg(
    path: std::path::PathBuf,
    config: Arc<HostConfig>,
    track: Arc<TrackLocalStaticSample>,
    payload_type: PayloadType,
    stats: Arc<HostStats>,
    active: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let bitrate = config.bitrate.to_string();
    let buffer_size = (config.bitrate / u32::from(config.fps)).to_string();
    let fps = config.fps.to_string();
    // Poll Desktop Duplication slightly faster than the RTP cadence. Display
    // refresh and Windows timer quantization otherwise leave a 60 Hz request at
    // roughly 55-57 delivered frames/s. The latest-frame slot below absorbs the
    // excess without building a queue.
    let desktop_duplication_fps = (u32::from(config.fps) * 67 / 60).to_string();
    let gop = config.fps.to_string();
    let scale = ffmpeg_video_filter(
        &config.ffmpeg_capture_mode,
        &config.ffmpeg_encoder,
        config.width,
        config.height,
        config.fps,
    );
    let profile = match config.ffmpeg_encoder.as_str() {
        "h264_amf" => "constrained_baseline",
        "h264_nvenc" | "libx264" => "baseline",
        _ => unreachable!("validated by HostConfig::load"),
    };
    let mut args = vec![
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "warning".to_owned(),
    ];
    if config.ffmpeg_capture_mode == "ddagrab" {
        args.extend([
            "-f".to_owned(),
            "lavfi".to_owned(),
            "-i".to_owned(),
            format!(
                "ddagrab=output_idx={}:draw_mouse=1:framerate={}:dup_frames=1",
                config.monitor_index, desktop_duplication_fps
            ),
        ]);
    } else {
        if config.ffmpeg_encoder == "h264_nvenc" {
            args.extend([
                "-init_hw_device".to_owned(),
                "d3d11va=desktop".to_owned(),
                "-filter_hw_device".to_owned(),
                "desktop".to_owned(),
            ]);
        }
        args.extend([
            "-f".to_owned(),
            "gdigrab".to_owned(),
            "-draw_mouse".to_owned(),
            "1".to_owned(),
            "-framerate".to_owned(),
            fps.clone(),
        ]);
        if config.ffmpeg_capture_width > 0 {
            args.extend([
                "-offset_x".to_owned(),
                config.ffmpeg_capture_x.to_string(),
                "-offset_y".to_owned(),
                config.ffmpeg_capture_y.to_string(),
                "-video_size".to_owned(),
                format!(
                    "{}x{}",
                    config.ffmpeg_capture_width, config.ffmpeg_capture_height
                ),
            ]);
        }
        args.extend(["-i".to_owned(), "desktop".to_owned()]);
    }
    args.extend([
        "-vf".to_owned(),
        scale,
        "-fps_mode".to_owned(),
        "passthrough".to_owned(),
        "-c:v".to_owned(),
        config.ffmpeg_encoder.clone(),
    ]);
    match config.ffmpeg_encoder.as_str() {
        "h264_nvenc" => args.extend(
            [
                "-rc",
                "cbr",
                "-preset",
                "p4",
                "-tune",
                "ull",
                "-delay",
                "0",
                "-surfaces",
                "2",
                "-zerolatency",
                "1",
                "-forced-idr",
                "1",
                "-rc-lookahead",
                "0",
                "-spatial-aq",
                "1",
                "-aq-strength",
                "8",
                "-temporal-aq",
                "0",
                "-strict_gop",
                "1",
                "-slices",
                "1",
            ]
            .map(str::to_owned),
        ),
        "h264_amf" => args
            .extend(["-rc", "cbr", "-quality", "speed", "-usage", "lowlatency"].map(str::to_owned)),
        "libx264" => args.extend(
            [
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-x264-params",
                "scenecut=0:rc-lookahead=0:sync-lookahead=0",
            ]
            .map(str::to_owned),
        ),
        _ => unreachable!("validated by HostConfig::load"),
    }
    args.extend(
        [
            "-profile:v",
            profile,
            "-b:v",
            &bitrate,
            "-maxrate",
            &bitrate,
            "-bufsize",
            &buffer_size,
            "-g",
            &gop,
            "-bf",
            "0",
            "-f",
            "h264",
            "pipe:1",
        ]
        .map(str::to_owned),
    );
    let mut command = Command::new(&path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn()?;
    info!(
        process_id = child.id(),
        width = config.width,
        height = config.height,
        bitrate = config.bitrate,
        "FFmpeg video capture started"
    );
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("FFmpeg stdout was not piped"))?;
    // A one-frame handoff adds at most one frame of buffering. Blocking the
    // producer when it is full applies backpressure before NVENC can build a
    // backlog and, critically, never removes a reference P-frame from the H.264
    // chain. RTP frame batching keeps this handoff empty during normal operation.
    let (frame_tx, mut frame_rx) = mpsc::channel::<Bytes>(1);
    let (initial_frame_tx, initial_frame_rx) = oneshot::channel();
    let reader_active = active.clone();
    std::thread::Builder::new()
        .name("desktop-h264-reader".into())
        .spawn(move || {
            read_latest_ffmpeg_frame(stdout, initial_frame_tx, frame_tx, reader_active)
        })?;

    let duration = Duration::from_secs_f64(1.0 / f64::from(config.fps));
    let ssrc = track_ssrc(&track).await?;
    // The initial IDR carries the SPS/PPS needed to configure a browser decoder.
    // Deliver it reliably before switching to replaceable low-latency frames.
    let mut initial_frame_rx = initial_frame_rx;
    let initial_frame_started = tokio::time::Instant::now();
    let initial_frame_timeout = if config.ffmpeg_capture_mode == "ddagrab" {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(5)
    };
    let initial_data = loop {
        tokio::select! {
            result = &mut initial_frame_rx => {
                break result.map_err(|_| anyhow::anyhow!(
                    "FFmpeg stopped before its initial keyframe"
                ))?;
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if !active.load(Ordering::Acquire) {
                    let _ = child.kill();
                    return Ok(());
                }
                if initial_frame_started.elapsed() >= initial_frame_timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    if config.ffmpeg_capture_mode == "ddagrab" {
                        info!(
                            timeout_ms = initial_frame_timeout.as_millis(),
                            "DDA produced no initial frame; retrying with GDI capture and NVENC"
                        );
                        let mut fallback_config = config.as_ref().clone();
                        fallback_config.ffmpeg_capture_mode = "gdigrab".into();
                        return Box::pin(stream_ffmpeg(
                            path,
                            Arc::new(fallback_config),
                            track,
                            payload_type,
                            stats,
                            active,
                        ))
                        .await;
                    }
                    anyhow::bail!(
                        "FFmpeg produced no initial H.264 keyframe within {} ms",
                        initial_frame_timeout.as_millis()
                    );
                }
            }
        }
    };
    info!(
        bytes = initial_data.len(),
        "initial H.264 keyframe captured"
    );
    let initial_write_started = tokio::time::Instant::now();
    let initial_write = track
        .sample_writer(ssrc, payload_type)
        .write_sample(&Sample {
            data: initial_data,
            duration,
            ..Default::default()
        })
        .await;
    info!(
        elapsed_ms = initial_write_started.elapsed().as_millis(),
        "initial H.264 keyframe submitted to WebRTC"
    );
    if let Err(error) = initial_write {
        if !active.load(Ordering::Acquire) {
            let _ = child.kill();
            return Ok(());
        }
        return Err(error.into());
    }
    stats.frames_sent.fetch_add(1, Ordering::Relaxed);
    let mut sent_in_window = 0u64;
    let mut max_write_time = Duration::ZERO;
    let mut report_at = tokio::time::Instant::now() + Duration::from_secs(5);
    let result = async {
        while active.load(Ordering::Acquire) {
            let data = tokio::select! {
                data = frame_rx.recv() => {
                    let Some(data) = data else {
                        break;
                    };
                    data
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    continue;
                }
            };
            let write_started = tokio::time::Instant::now();
            let write_result = track
                .sample_writer(ssrc, payload_type)
                .write_sample(&Sample {
                    data,
                    duration,
                    ..Default::default()
                })
                .await;
            if let Err(error) = write_result {
                if !active.load(Ordering::Acquire) {
                    break;
                }
                return Err(error.into());
            }
            max_write_time = max_write_time.max(write_started.elapsed());
            stats.frames_sent.fetch_add(1, Ordering::Relaxed);
            sent_in_window += 1;
            if tokio::time::Instant::now() >= report_at {
                info!(
                    width = config.width,
                    height = config.height,
                    sent_frames = sent_in_window,
                    max_rtp_write_us = max_write_time.as_micros(),
                    "video pipeline five-second window"
                );
                sent_in_window = 0;
                max_write_time = Duration::ZERO;
                report_at += Duration::from_secs(5);
            }
        }
        anyhow::Ok(())
    }
    .await;
    let _ = child.kill();
    result
}

fn ffmpeg_video_filter(
    capture_mode: &str,
    encoder: &str,
    width: u32,
    height: u32,
    fps: u16,
) -> String {
    if capture_mode == "ddagrab" {
        // DDA is deliberately polled above the target rate so Windows timer
        // quantization cannot starve a 60 Hz stream. Resample the GPU frames
        // before NVENC: encoded P-frames must never be dropped, while forwarding
        // all 67 samples with 60 Hz RTP durations makes the browser repeatedly
        // drain and refill its jitter buffer.
        format!("scale_d3d11=width={width}:height={height}:format=nv12,fps=fps={fps}:round=near")
    } else if encoder == "libx264" {
        format!(
            "scale=width={width}:height={height}:flags=fast_bilinear,format=yuv420p,setpts=N/({fps}*TB)"
        )
    } else if encoder == "h264_amf" {
        format!(
            "scale=width={width}:height={height}:flags=fast_bilinear,format=nv12,setpts=N/({fps}*TB)"
        )
    } else {
        format!(
            "hwupload,scale_d3d11=width={width}:height={height}:format=nv12,setpts=N/({fps}*TB)"
        )
    }
}

fn read_latest_ffmpeg_frame(
    stdout: std::process::ChildStdout,
    initial_frame_tx: oneshot::Sender<Bytes>,
    frame_tx: mpsc::Sender<Bytes>,
    active: Arc<AtomicBool>,
) {
    let mut reader = H26xSampleReader::new(BufReader::new(stdout), 4 * 1024 * 1024, false);
    let mut prefix = BytesMut::new();
    let mut initial_frame_tx = Some(initial_frame_tx);
    while active.load(Ordering::Acquire) {
        let Ok(sample) = reader.next_sample() else {
            break;
        };
        let nal_type = sample.data.first().map_or(0, |header| header & 0x1f);
        if nal_type == 12 {
            // NVENC CBR filler is not visual data. Sending it wastes bandwidth and
            // previously made the frame scheduler treat one picture as two frames.
            continue;
        }
        if matches!(nal_type, 1 | 5) {
            prefix.reserve(4 + sample.data.len());
            prefix.put_slice(&[0, 0, 0, 1]);
            prefix.put_slice(&sample.data);
            let data = prefix.split().freeze();
            if nal_type == 5
                && let Some(sender) = initial_frame_tx.take()
            {
                if sender.send(data).is_err() {
                    break;
                }
                continue;
            }
            if initial_frame_tx.is_some() {
                // Do not expose undecodable delta frames before the first IDR.
                continue;
            }
            if frame_tx.blocking_send(data).is_err() {
                break;
            }
        } else {
            prefix.reserve(4 + sample.data.len());
            prefix.put_slice(&[0, 0, 0, 1]);
            prefix.put_slice(&sample.data);
        }
    }
}

#[cfg(windows)]
async fn stream_windows_hardware(
    config: Arc<HostConfig>,
    track: Arc<TrackLocalStaticSample>,
    payload_type: PayloadType,
    stats: Arc<HostStats>,
    active: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    use media_pipeline::{
        CaptureTarget, VideoConfig,
        capture::{self, CaptureConfig},
        encoder::{EncodedSample, mf_h264::MfH264Encoder},
    };
    let default_duration = Duration::from_secs_f64(1.0 / f64::from(config.fps));
    let ssrc = track_ssrc(&track).await?;
    let (sample_tx, mut sample_rx) = watch::channel::<Option<EncodedSample>>(None);
    let capture_config = config.clone();
    let worker_active = active.clone();

    let worker = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let (capture_session, frame_rx) = capture::start(
            CaptureConfig {
                target: CaptureTarget::Monitor(capture_config.monitor_index),
                capture_cursor: true,
            },
            1,
        )?;
        let video_config = VideoConfig {
            width: capture_config.width,
            height: capture_config.height,
            fps: u32::from(capture_config.fps),
            bitrate: capture_config.bitrate,
            keyframe_interval: u32::from(capture_config.fps),
        };
        // Prefer the GPU encoder, but do not let a driver-specific Media Foundation
        // negotiation failure strand an otherwise healthy WebRTC session. Some drivers
        // enumerate a hardware H.264 MFT and then reject larger WGC input types.
        let (mut encoder, mut using_hardware) = match MfH264Encoder::new_with(
            capture_session.device(),
            video_config,
            true,
        ) {
            Ok(encoder) => (encoder, true),
            Err(hardware_error) => {
                warn!(
                    error = %hardware_error,
                    width = video_config.width,
                    height = video_config.height,
                    "hardware H.264 encoder unavailable; falling back to Media Foundation software encoding"
                );
                (
                        MfH264Encoder::new(capture_session.device(), video_config).map_err(
                            |software_error| {
                                anyhow::anyhow!(
                                    "hardware H.264 encoder failed ({hardware_error}); software fallback failed ({software_error})"
                                )
                            },
                        )?,
                        false,
                    )
            }
        };
        while worker_active.load(Ordering::Acquire) {
            let mut frame = frame_rx.recv()?;
            // Capacity is one. Drain before encoding so the most recent GPU texture wins.
            while let Ok(newer) = frame_rx.try_recv() {
                frame = newer;
            }
            let mut encoded = Vec::new();
            if let Err(hardware_error) =
                encoder.encode(&frame.texture, frame.timestamp, &mut encoded)
            {
                if !using_hardware {
                    return Err(hardware_error.into());
                }
                warn!(
                    error = %hardware_error,
                    "hardware H.264 encoder failed while streaming; switching to software encoding"
                );
                encoder = MfH264Encoder::new(capture_session.device(), video_config).map_err(
                    |software_error| {
                        anyhow::anyhow!(
                            "hardware H.264 stream failed ({hardware_error}); software fallback failed ({software_error})"
                        )
                    },
                )?;
                using_hardware = false;
                encoded.clear();
                encoder.encode(&frame.texture, frame.timestamp, &mut encoded)?;
            }
            for sample in encoded {
                if sample_tx.send(Some(sample)).is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    });

    let mut previous_timestamp = None;
    while active.load(Ordering::Acquire) && sample_rx.changed().await.is_ok() {
        let Some(sample) = sample_rx.borrow_and_update().clone() else {
            continue;
        };
        let duration = previous_timestamp
            .map(|previous| sample.timestamp.saturating_sub(previous))
            .filter(|value| !value.is_zero())
            .unwrap_or(default_duration);
        previous_timestamp = Some(sample.timestamp);
        track
            .sample_writer(ssrc, payload_type)
            .write_sample(&Sample {
                data: sample.data.into(),
                duration,
                ..Default::default()
            })
            .await?;
        stats.frames_sent.fetch_add(1, Ordering::Relaxed);
    }
    worker.await??;
    anyhow::bail!("hardware capture/encode worker stopped without an error")
}

async fn track_ssrc(track: &Arc<TrackLocalStaticSample>) -> anyhow::Result<u32> {
    track
        .ssrcs()
        .await
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("video track has no SSRC"))
}

#[cfg(test)]
mod tests {
    use super::{ffmpeg_video_filter, h264_vcl_nal};

    #[test]
    fn only_h264_picture_slices_advance_time() {
        assert!(h264_vcl_nal(&[0x41]));
        assert!(h264_vcl_nal(&[0x65]));
        assert!(!h264_vcl_nal(&[0x06]));
        assert!(!h264_vcl_nal(&[0x0c]));
        assert!(!h264_vcl_nal(&[]));
    }

    #[test]
    fn dda_frames_are_resampled_before_h264_encoding() {
        let filter = ffmpeg_video_filter("ddagrab", "h264_nvenc", 2560, 1600, 60);
        assert_eq!(
            filter,
            "scale_d3d11=width=2560:height=1600:format=nv12,fps=fps=60:round=near"
        );
        assert!(!filter.contains("setpts"));
    }

    #[test]
    fn gdi_capture_keeps_its_fixed_rate_timestamps() {
        assert_eq!(
            ffmpeg_video_filter("gdigrab", "h264_nvenc", 1920, 1080, 60),
            "hwupload,scale_d3d11=width=1920:height=1080:format=nv12,setpts=N/(60*TB)"
        );
    }

    #[test]
    fn software_ffmpeg_uses_cpu_scaling() {
        assert_eq!(
            ffmpeg_video_filter("gdigrab", "libx264", 1920, 1080, 30),
            "scale=width=1920:height=1080:flags=fast_bilinear,format=yuv420p,setpts=N/(30*TB)"
        );
    }

    #[test]
    fn amf_fallback_uses_compatible_cpu_frames() {
        assert_eq!(
            ffmpeg_video_filter("gdigrab", "h264_amf", 1920, 1200, 30),
            "scale=width=1920:height=1200:flags=fast_bilinear,format=nv12,setpts=N/(30*TB)"
        );
    }
}
