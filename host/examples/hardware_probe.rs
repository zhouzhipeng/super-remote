#[cfg(windows)]
#[unsafe(no_mangle)]
pub static NvOptimusEnablement: u32 = 1;
#[cfg(windows)]
#[unsafe(no_mangle)]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::time::Duration;

    use media_pipeline::{
        CaptureTarget, VideoConfig,
        capture::{self, CaptureConfig},
        encoder::mf_h264::MfH264Encoder,
    };

    let width = std::env::args()
        .nth(1)
        .as_deref()
        .map(str::parse)
        .transpose()?
        .unwrap_or(1920);
    let height = std::env::args()
        .nth(2)
        .as_deref()
        .map(str::parse)
        .transpose()?
        .unwrap_or(1080);
    let monitor_index = std::env::args()
        .nth(3)
        .as_deref()
        .map(str::parse)
        .transpose()?
        .unwrap_or(0);

    let (session, frames) = capture::start(
        CaptureConfig {
            target: CaptureTarget::Monitor(monitor_index),
            capture_cursor: true,
        },
        1,
    )?;
    let mut encoder = MfH264Encoder::new_with(
        session.device(),
        VideoConfig {
            width,
            height,
            fps: 60,
            bitrate: 8_000_000,
            keyframe_interval: 60,
        },
        true,
    )?;
    let mut frames_encoded = 0usize;
    let mut samples_encoded = 0usize;
    let mut bytes_encoded = 0usize;
    while frames_encoded < 120 {
        let frame = frames.recv_timeout(Duration::from_secs(5))?;
        let mut output = Vec::new();
        encoder.encode(&frame.texture, frame.timestamp, &mut output)?;
        frames_encoded += 1;
        samples_encoded += output.len();
        bytes_encoded += output.iter().map(|sample| sample.data.len()).sum::<usize>();
    }
    println!(
        "hardware WGC/MF probe passed: {frames_encoded} frames, {samples_encoded} samples, {bytes_encoded} bytes"
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("hardware probe is Windows-only");
}
