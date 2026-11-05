use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "USB Capture";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_REPO: &str = "https://github.com/crwgrant/usb-capture-app";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CapturePreset {
    LowLatency,
    #[default]
    Balanced,
    HighDetail,
}

impl CapturePreset {
    pub fn id(self) -> &'static str {
        match self {
            Self::LowLatency => "low_latency",
            Self::Balanced => "balanced",
            Self::HighDetail => "high_detail",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LowLatency => "Low latency",
            Self::Balanced => "Balanced",
            Self::HighDetail => "High detail",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "low_latency" => Some(Self::LowLatency),
            "balanced" => Some(Self::Balanced),
            "high_detail" => Some(Self::HighDetail),
            _ => None,
        }
    }

    pub fn all() -> [Self; 3] {
        [Self::LowLatency, Self::Balanced, Self::HighDetail]
    }

    pub fn target_pixels(self) -> u32 {
        match self {
            Self::LowLatency | Self::Balanced => 1920 * 1080,
            Self::HighDetail => 2560 * 1440,
        }
    }

    pub fn target_fps(self) -> u32 {
        match self {
            Self::LowLatency | Self::Balanced => 60,
            Self::HighDetail => 120,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormatPref {
    #[default]
    Auto,
    Nv12,
    Yuy2,
    Mjpeg,
}

impl PixelFormatPref {
    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Nv12 => "nv12",
            Self::Yuy2 => "yuy2",
            Self::Mjpeg => "mjpeg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Nv12 => "NV12",
            Self::Yuy2 => "YUY2",
            Self::Mjpeg => "MJPEG",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "auto" => Some(Self::Auto),
            "nv12" => Some(Self::Nv12),
            "yuy2" => Some(Self::Yuy2),
            "mjpeg" => Some(Self::Mjpeg),
            _ => None,
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::Auto, Self::Nv12, Self::Yuy2, Self::Mjpeg]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub capture_preset: CapturePreset,
    #[serde(default)]
    pub pixel_format: PixelFormatPref,
    #[serde(default = "default_auto")]
    pub resolution_id: String,
    #[serde(default = "default_auto")]
    pub fps_id: String,
    #[serde(default, skip_serializing)]
    pub mode_id: String,
    #[serde(default = "default_capture_buffers")]
    pub capture_buffers: u32,
    #[serde(default = "default_audio_buffer")]
    pub audio_buffer: u32,
    #[serde(default = "default_true")]
    pub vsync: bool,
    #[serde(default)]
    pub hardware_decode: bool,
}

fn default_capture_buffers() -> u32 {
    2
}

fn default_audio_buffer() -> u32 {
    128
}

fn default_true() -> bool {
    true
}

fn default_auto() -> String {
    "auto".into()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            capture_preset: CapturePreset::Balanced,
            pixel_format: PixelFormatPref::Auto,
            resolution_id: "auto".into(),
            fps_id: "auto".into(),
            mode_id: String::new(),
            capture_buffers: 2,
            audio_buffer: 128,
            vsync: true,
            hardware_decode: false,
        }
    }
}

impl AppSettings {
    pub const BUFFER_CHOICES: [u32; 3] = [2, 3, 4];
    pub const AUDIO_CHOICES: [u32; 4] = [64, 128, 256, 512];

    pub fn load() -> Self {
        let path = settings_path();
        let Ok(bytes) = fs::read(&path) else {
            return Self::default();
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(mut settings) => {
                settings.sanitize();
                settings
            }
            Err(err) => {
                log::warn!("Ignoring invalid settings at {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            log::warn!("Could not create {}: {err}", parent.display());
            return;
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(err) = fs::write(&path, bytes) {
                    log::warn!("Could not write {}: {err}", path.display());
                }
            }
            Err(err) => log::warn!("Could not serialize settings: {err}"),
        }
    }

    pub fn sanitize(&mut self) {
        self.capture_buffers = Self::BUFFER_CHOICES
            .into_iter()
            .find(|n| *n == self.capture_buffers)
            .unwrap_or(2);
        self.audio_buffer = Self::AUDIO_CHOICES
            .into_iter()
            .find(|n| *n == self.audio_buffer)
            .unwrap_or(128);
        if self.hardware_decode {
            self.hardware_decode = false;
        }
        if self.resolution_id == "auto"
            && self.fps_id == "auto"
            && let Some(((width, height), fps)) = parse_legacy_mode(&self.mode_id)
        {
            self.resolution_id = format!("{width}x{height}");
            self.fps_id = fps.to_string();
        }
        self.mode_id.clear();
    }
}

fn parse_legacy_mode(id: &str) -> Option<((u32, u32), u32)> {
    if id.is_empty() || id == "auto" {
        return None;
    }
    let (size, fps) = id.split_once('@')?;
    let (width, height) = size.split_once('x')?;
    Some((
        (width.parse().ok()?, height.parse().ok()?),
        fps.parse().ok()?,
    ))
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

fn config_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("usb-capture-app")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/usb-capture-app")
    } else {
        PathBuf::from(".usb-capture-app")
    }
}
