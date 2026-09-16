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
    /// Negotiated per session; never enabled by a configuration file alone.
    #[serde(skip)]
    pub local_cursor: bool,
    #[serde(skip)]
    pub hybrid_video: bool,
    #[serde(default)]
    pub follow_primary_display: bool,
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
    /// Longest edge of the interaction stream while hybrid refinement is active.
    /// Native detail is restored by the lossless layer once input settles, so
    /// this only bounds motion bandwidth - but it is also the entire quality of
    /// what the user sees *during* input, so an aggressively small value is what
    /// makes the sharp/blurred transition obvious. Tune to the deployed link.
    #[serde(default = "default_interaction_max_edge")]
    pub interaction_max_edge: u32,
    /// Bitrate ceiling applied together with `interaction_max_edge`.
    #[serde(default = "default_interaction_bitrate")]
    pub interaction_bitrate: u32,
    /// Largest wheel delta injected in one event, in Windows wheel units where
    /// 120 is one notch. A fast flick arrives as a single multi-notch packet and
    /// lands as one jump; pacing it into steps of this size is what turns it
    /// back into motion. The default never subdivides a notch, so applications
    /// that only handle whole notches behave exactly as before. Lowering it
    /// (40, or 20) glides instead of stepping, but only in applications that
    /// accumulate high-resolution deltas - browsers and modern apps do, some
    /// legacy Win32 apps discard anything under one notch and would not scroll.
    #[serde(default = "default_wheel_step")]
    pub wheel_step: u16,
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
    /// Optional local JSON status file consumed by the Windows control panel.
    pub control_status_path: Option<PathBuf>,
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
    /// Keep the interaction stream ready; native-resolution refinements provide
    /// detail after input stops, without reinitializing NVENC on every key.
    pub fn interaction_video(&self) -> Self {
        let mut config = self.clone();
        let limit = u64::from(self.interaction_max_edge.max(2));
        let longest = u64::from(self.width.max(self.height).max(1));
        if longest > limit {
            config.width = ((u64::from(self.width) * limit / longest) as u32 & !1).max(2);
            config.height = ((u64::from(self.height) * limit / longest) as u32 & !1).max(2);
        }
        config.bitrate = config.bitrate.min(self.interaction_bitrate.max(1));
        config
    }

    pub fn refresh_display(self: &Arc<Self>) -> Arc<Self> {
        #[cfg(windows)]
        if self.follow_primary_display && self.h264_file.is_none() {
            use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
            let mut mode = DEVMODEW::default();
            mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            if unsafe { EnumDisplaySettingsW(None, ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool() {
                return self.with_display_size(mode.dmPelsWidth, mode.dmPelsHeight);
            }
            tracing::warn!("could not refresh primary display dimensions; retaining previous dimensions");
        }
        self.clone()
    }

    fn with_display_size(self: &Arc<Self>, width: u32, height: u32) -> Arc<Self> {
        if width < 2 || height < 2 { return self.clone(); }
        let mut config = self.as_ref().clone();
        config.width = width & !1;
        config.height = height & !1;
        config.ffmpeg_capture_width = width;
        config.ffmpeg_capture_height = height;
        Arc::new(config)
    }

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
        if config.interaction_max_edge < 2 {
            bail!("interaction_max_edge must be at least 2");
        }
        if config.interaction_bitrate == 0 {
            bail!("interaction_bitrate cannot be zero");
        }
        // A slice is carried in the protocol's i16 wheel delta.
        if !(1..=32767).contains(&config.wheel_step) {
            bail!("wheel_step must be between 1 and 32767");
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
        if config.ffmpeg_path.is_some() {
            if !matches!(
                config.ffmpeg_encoder.as_str(),
                "h264_nvenc" | "h264_amf" | "libx264"
            ) {
                bail!(
                    "ffmpeg_encoder must be h264_nvenc, h264_amf, or libx264 when ffmpeg_path is set"
                );
            }
            if !matches!(config.ffmpeg_capture_mode.as_str(), "gdigrab" | "ddagrab") {
                bail!("ffmpeg_capture_mode must be gdigrab or ddagrab when ffmpeg_path is set");
            }
            if config.ffmpeg_capture_mode == "ddagrab" && config.ffmpeg_encoder != "h264_nvenc" {
                bail!("ddagrab currently requires h264_nvenc on this host");
            }
            if config.ffmpeg_encoder == "libx264" && config.ffmpeg_capture_mode != "gdigrab" {
                bail!("libx264 requires gdigrab capture");
            }
        } else if config.ffmpeg_encoder != "mf_h264" || config.ffmpeg_capture_mode != "wgc" {
            bail!(
                "the native Windows pipeline requires ffmpeg_encoder=mf_h264 and ffmpeg_capture_mode=wgc"
            );
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
/// 1920 keeps a 2560/4K desktop within one downscale step instead of three, so
/// the interaction stream stays readable rather than merely recognizable.
const fn default_interaction_max_edge() -> u32 {
    1920
}
const fn default_interaction_bitrate() -> u32 {
    6_000_000
}
/// One Windows notch: pace multi-notch bursts, never subdivide what arrived.
const fn default_wheel_step() -> u16 {
    120
}

fn default_ffmpeg_encoder() -> String {
    "mf_h264".into()
}

fn default_ffmpeg_capture_mode() -> String {
    "wgc".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Arc<HostConfig> {
        Arc::new(HostConfig {
            local_cursor: false,
            hybrid_video: false,
            follow_primary_display: false,
            server_url: String::new(),
            device_id: "device".into(),
            device_name: "desktop".into(),
            device_token: "x".repeat(24),
            width: 2560,
            height: 1600,
            fps: 60,
            bitrate: 20_000_000,
            interaction_max_edge: default_interaction_max_edge(),
            interaction_bitrate: default_interaction_bitrate(),
            wheel_step: default_wheel_step(),
            monitor_index: 0,
            h264_file: None,
            ffmpeg_path: None,
            ffmpeg_encoder: "mf_h264".into(),
            ffmpeg_capture_mode: "wgc".into(),
            ffmpeg_capture_x: 0,
            ffmpeg_capture_y: 0,
            ffmpeg_capture_width: 2560,
            ffmpeg_capture_height: 1600,
            ice_servers: Vec::new(),
            control_status_path: None,
        })
    }

    #[test]
    fn interaction_video_caps_pixels_without_changing_capture_or_aspect() {
        let original = config().with_display_size(4000, 2560);
        let low = original.interaction_video();
        assert_eq!((low.width, low.height, low.bitrate), (1920, 1228, 6_000_000));
        assert_eq!((low.ffmpeg_capture_width, low.ffmpeg_capture_height), (4000, 2560));
        assert_eq!((original.width, original.height), (4000, 2560));
        // A display already inside the ceiling keeps every pixel it captured.
        let portrait = config().with_display_size(1200, 1920).interaction_video();
        assert_eq!((portrait.width, portrait.height), (1200, 1920));
        let small = config().with_display_size(320, 200).interaction_video();
        assert_eq!((small.width, small.height), (320, 200));
        // The encoder ceiling must follow the configured interaction bitrate
        // instead of a second, lower limit hidden inside the argument builder.
        let args = crate::ffmpeg_options::hybrid_encoding_args("h264_nvenc", low.bitrate, low.fps);
        assert_eq!(args[args.iter().position(|a| a == "-maxrate").unwrap() + 1], "6000000");
    }

    #[test]
    fn interaction_ceiling_is_configurable_for_constrained_links() {
        let mut narrow = config().as_ref().clone();
        narrow.interaction_max_edge = 1280;
        narrow.interaction_bitrate = 2_000_000;
        let low = Arc::new(narrow).with_display_size(4000, 2560).interaction_video();
        assert_eq!((low.width, low.height, low.bitrate), (1280, 818, 2_000_000));
    }

    #[test]
    fn refresh_restores_hidpi_after_rdp_resolution_change() {
        let small = config().with_display_size(1512, 950);
        let restored = small.with_display_size(2560, 1600).for_viewport(Some(3024), Some(1900));
        assert_eq!((restored.width, restored.height), (2560, 1600));
        assert_eq!((restored.ffmpeg_capture_width, restored.ffmpeg_capture_height), (2560, 1600));
        assert_eq!(restored.h264_level(), ("5.1", "33"));
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
