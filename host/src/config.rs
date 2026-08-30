use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use remote_protocol::device::DeviceCapabilities;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct HostConfig {
    pub server_url: String,
    pub device_id: String,
    pub device_name: String,
    pub device_token: String,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_fps")]
    pub fps: u16,
    #[serde(default = "default_bitrate")]
    pub bitrate: u32,
    #[serde(default)]
    pub monitor_index: usize,
    pub h264_file: Option<PathBuf>,
    /// Optional development source that captures the live Windows desktop and writes
    /// Annex-B H.264 to stdout. The production WGC/Media Foundation path remains the
    /// default when this is omitted.
    pub ffmpeg_path: Option<PathBuf>,
    #[serde(default = "default_ffmpeg_encoder")]
    pub ffmpeg_encoder: String,
    #[serde(default = "default_ffmpeg_capture_mode")]
    pub ffmpeg_capture_mode: String,
    #[serde(default)]
    pub ffmpeg_capture_x: i32,
    #[serde(default)]
    pub ffmpeg_capture_y: i32,
    #[serde(default)]
    pub ffmpeg_capture_width: u32,
    #[serde(default)]
    pub ffmpeg_capture_height: u32,
    #[serde(default)]
    pub ice_servers: Vec<IceServerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IceServerConfig {
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

impl HostConfig {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let mut config: Self = toml::from_str(&fs::read_to_string(path)?)?;
        if config.device_id.is_empty() || config.device_name.is_empty() {
            bail!("device identity cannot be empty");
        }
        if config.device_token.len() < 24 {
            bail!("device_token must contain at least 24 characters");
        }
        if !(1..=60).contains(&config.fps) {
            bail!("fps must be between 1 and 60");
        }
        if config.width == 0 || config.height == 0 {
            bail!("capture dimensions cannot be zero");
        }
        if let Some(path) = &config.h264_file {
            config.h264_file = Some(path.canonicalize().context("h264_file does not exist")?);
        }
        if let Some(path) = &config.ffmpeg_path {
            config.ffmpeg_path = Some(path.canonicalize().context("ffmpeg_path does not exist")?);
        }
        if config.h264_file.is_some() && config.ffmpeg_path.is_some() {
            bail!("h264_file and ffmpeg_path are mutually exclusive");
        }
        if !matches!(config.ffmpeg_encoder.as_str(), "h264_nvenc" | "h264_amf") {
            bail!("ffmpeg_encoder must be h264_nvenc or h264_amf");
        }
        if !matches!(config.ffmpeg_capture_mode.as_str(), "gdigrab" | "ddagrab") {
            bail!("ffmpeg_capture_mode must be gdigrab or ddagrab");
        }
        if config.ffmpeg_capture_mode == "ddagrab" && config.ffmpeg_encoder != "h264_nvenc" {
            bail!("ddagrab currently requires h264_nvenc on this host");
        }
        if (config.ffmpeg_capture_width == 0) != (config.ffmpeg_capture_height == 0) {
            bail!("ffmpeg capture width and height must both be zero or both be non-zero");
        }
        Ok(config)
    }

    pub fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            width: self.width,
            height: self.height,
            fps: self.fps,
            codecs: vec!["h264".into()],
            audio: true,
        }
    }

    pub fn h264_level(&self) -> (&'static str, &'static str) {
        let macroblocks_per_second = u64::from(self.width.div_ceil(16))
            * u64::from(self.height.div_ceil(16))
            * u64::from(self.fps);
        if macroblocks_per_second <= 108_000 {
            ("3.1", "1f")
        } else if macroblocks_per_second <= 216_000 {
            // 1152x720 at 60 FPS is 194,400 macroblocks/s and fits Level 3.2.
            ("3.2", "20")
        } else if macroblocks_per_second <= 522_240 {
            ("4.2", "2a")
        } else if macroblocks_per_second <= 589_824 {
            // 1920x1200 at 60 FPS is 540,000 macroblocks/s and fits Level 5.0.
            ("5.0", "32")
        } else {
            ("5.1", "33")
        }
    }
}

const fn default_width() -> u32 {
    1920
}
const fn default_height() -> u32 {
    1080
}
const fn default_fps() -> u16 {
    60
}
const fn default_bitrate() -> u32 {
    8_000_000
}

fn default_ffmpeg_encoder() -> String {
    "h264_nvenc".into()
}

fn default_ffmpeg_capture_mode() -> String {
    "gdigrab".into()
}
