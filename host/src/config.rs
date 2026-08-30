use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
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

    /// Select the largest even-sized stream that fits the browser's physical
    /// video area while preserving the captured display's aspect ratio.
    pub fn for_viewport(
        self: &Arc<Self>,
        viewport_width: Option<u32>,
        viewport_height: Option<u32>,
    ) -> Arc<Self> {
        let (Some(viewport_width), Some(viewport_height)) = (viewport_width, viewport_height)
        else {
            return self.clone();
        };
        if viewport_width == 0 || viewport_height == 0 {
            return self.clone();
        }

        let source_width = if self.ffmpeg_capture_width > 0 {
            self.ffmpeg_capture_width
        } else {
            self.width
        };
        let source_height = if self.ffmpeg_capture_height > 0 {
            self.ffmpeg_capture_height
        } else {
            self.height
        };
        let scale = (viewport_width as f64 / source_width as f64)
            .min(viewport_height as f64 / source_height as f64)
            // width/height are the validated encoder ceiling. High-DPI Safari
            // viewports can exceed the source's safe H.264/NVENC level.
            .min(self.width as f64 / source_width as f64)
            .min(self.height as f64 / source_height as f64)
            .min(1.0);
        let even = |value: f64| ((value.floor() as u32).max(2) / 2) * 2;
        let mut session = self.as_ref().clone();
        session.width = even(source_width as f64 * scale);
        session.height = even(source_height as f64 * scale);
        let ceiling_pixels = u64::from(self.width) * u64::from(self.height);
        let session_pixels = u64::from(session.width) * u64::from(session.height);
        let minimum_bitrate = u64::from(self.bitrate).min(8_000_000);
        session.bitrate = (u64::from(self.bitrate) * session_pixels)
            .div_ceil(ceiling_pixels)
            .clamp(minimum_bitrate, u64::from(self.bitrate)) as u32;
        Arc::new(session)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Arc<HostConfig> {
        Arc::new(HostConfig {
            server_url: String::new(),
            device_id: "device".into(),
            device_name: "desktop".into(),
            device_token: "x".repeat(24),
            width: 2560,
            height: 1600,
            fps: 60,
            bitrate: 20_000_000,
            monitor_index: 0,
            h264_file: None,
            ffmpeg_path: None,
            ffmpeg_encoder: "h264_nvenc".into(),
            ffmpeg_capture_mode: "gdigrab".into(),
            ffmpeg_capture_x: 0,
            ffmpeg_capture_y: 0,
            ffmpeg_capture_width: 2560,
            ffmpeg_capture_height: 1600,
            ice_servers: Vec::new(),
        })
    }

    #[test]
    fn fits_complete_display_inside_portrait_browser() {
        let fitted = config().for_viewport(Some(390), Some(793));
        assert_eq!((fitted.width, fitted.height), (390, 242));
        assert_eq!(fitted.bitrate, 8_000_000);
    }

    #[test]
    fn fits_complete_display_inside_landscape_browser() {
        let fitted = config().for_viewport(Some(844), Some(339));
        assert_eq!((fitted.width, fitted.height), (542, 338));
    }

    #[test]
    fn high_dpi_browser_receives_full_physical_display() {
        let fitted = config().for_viewport(Some(5120), Some(3200));
        assert_eq!((fitted.width, fitted.height), (2560, 1600));
        assert_eq!(fitted.bitrate, 20_000_000);
        assert_eq!(fitted.h264_level(), ("5.1", "33"));
    }
}
