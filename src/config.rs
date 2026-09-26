//! Configuration for the studio (`config.toml`) and the USB proxy (`proxy.toml`)

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// USB proxy configuration (`proxy.toml`)
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Proxy configuration
    pub proxy: ProxyConfig,
    /// Dump configuration  
    pub dump: DumpConfig,
    /// Frame streaming to the studio host
    pub stream: StreamConfig,
    /// Actions replayed to the Switch
    pub replay: ReplayConfig,
    /// Performance configuration
    pub performance: PerformanceConfig,
    /// Logging configuration
    pub logging: LoggingConfig,
}

/// Proxy-related configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Retry delay when HID gadget device fails to open (milliseconds)
    pub hidg_retry_delay_ms: u64,
}

/// Local backup recording on the proxy
#[derive(Debug, Serialize, Deserialize)]
pub struct DumpConfig {
    /// Record a session from launch until exit, next to what the studio records
    pub autostart: bool,
    /// Path prefix of that session folder, e.g. "/home/pi/procon-"
    pub prefix: String,
}

/// Frame streaming configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct StreamConfig {
    /// TCP port the studio host connects to
    pub port: u16,
}

/// Replay configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct ReplayConfig {
    /// TCP port that takes JSON-line actions (see `replay`)
    pub port: u16,
}

/// Performance-related configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct PerformanceConfig {
    /// Enable CPU affinity pinning to random core
    pub enable_cpu_affinity: bool,
}

/// Logging configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level: error, warn, info, debug, trace
    pub level: String,
}

impl Config {
    /// Load configuration from a TOML file
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        load(path)
    }

    /// Validate configuration values
    pub fn validate(&self) -> Result<()> {
        self.logging.validate()
    }
}

impl LoggingConfig {
    /// Check the log level is one env_logger knows
    pub fn validate(&self) -> Result<()> {
        match self.level.to_lowercase().as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => Ok(()),
            _ => anyhow::bail!("Invalid log level: {}", self.level),
        }
    }
}

/// Load any configuration struct from a TOML file
pub fn load<T: DeserializeOwned, P: AsRef<Path>>(path: P) -> Result<T> {
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file: {}", path.as_ref().display()))?;

    toml::from_str(&contents)
        .with_context(|| format!("Failed to parse config file: {}", path.as_ref().display()))
}

/// Studio host configuration (`config.toml`)
#[derive(Debug, Serialize, Deserialize)]
pub struct StudioConfig {
    /// Where the proxy streams frames from
    pub proxy: RemoteProxyConfig,
    /// Dashboard server
    pub web: WebConfig,
    /// Session recording defaults
    pub recording: RecordingConfig,
    /// Video capture
    pub video: VideoConfig,
    /// The Inkspector app
    #[serde(default)]
    pub inspect: InspectConfig,
    /// The Cuttlefish app
    #[serde(default)]
    pub cuttlefish: CuttlefishConfig,
    /// Logging configuration
    pub logging: LoggingConfig,
}

/// The Inkspector app's data
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InspectConfig {
    /// Folder holding the session folders; by default the recording
    /// prefix's folder
    pub root: Option<String>,
    /// AgentZero's `calibration.json` with each session's video delay,
    /// relative to this config file; by default
    /// `../AgentZero/calibration.json`. A missing file means no calibration.
    pub calibration: Option<String>,
    /// Folder of object labels (`classes.json`, `<session>/<segment
    /// stem>.objects.jsonl`), relative to this config file; by default
    /// `Annotations` next to the root
    pub annotations: Option<String>,
}

/// The Cuttlefish app: video reviews with comments and drawings
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CuttlefishConfig {
    /// Folder of review JSON files, relative to this config file; by
    /// default `Reviews` next to the Inkspector's root
    pub reviews: Option<String>,
    /// Folder for YouTube ranges downloaded with yt-dlp, relative to this
    /// config file; by default `procon-cuttlefish` in the user's cache
    /// folder (`$XDG_CACHE_HOME` or `~/.cache`)
    pub cache: Option<String>,
}

/// The machine running `procon-proxy`
#[derive(Debug, Serialize, Deserialize)]
pub struct RemoteProxyConfig {
    /// `host:port` of the proxy's `[stream]` port
    pub address: String,
    /// `host:port` of the proxy's `[replay]` port
    pub replay_address: String,
}

/// Dashboard server configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct WebConfig {
    /// Port for the dashboard
    pub port: u16,
}

/// Session recording defaults
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordingConfig {
    /// Path prefix for session folders until one is set from the dashboard
    pub prefix: String,
}

/// Video capture configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoConfig {
    /// Input at first launch: "screen", a device like "/dev/video0", or "" for none
    pub input: String,
    /// Capture frame rate
    pub fps: u32,
    /// Extra ffmpeg input options for V4L2 devices, such as format and size
    pub v4l2_args: Vec<String>,
    /// Recorded height in pixels, 0 for the source size; the dashboard can change it
    pub record_height: u32,
    /// Recorded frame rate; the dashboard can change it
    pub record_fps: u32,
    /// ffmpeg encoder options for recordings
    pub encoder: Vec<String>,
    /// Recording file extension
    pub extension: String,
    /// Preview height in pixels (1080, 720, 540 or 360), unless it follows the recording
    pub preview_height: u32,
    /// Preview frame rate, unless it follows the recording
    pub preview_fps: u32,
    /// ffmpeg encoder options for the live preview; keep them low latency
    pub preview_encoder: Vec<String>,
    /// PulseAudio source recorded as the videos' sound track, such as the
    /// capture card's (`pactl list short sources`); "" for none
    pub audio_input: String,
    /// How much later the source's sound arrives than its picture, in ms
    pub audio_offset_ms: i64,
}
