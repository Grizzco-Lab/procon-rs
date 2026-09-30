//! Configuration of the USB proxy (`proxy.toml`)

use anyhow::Result;
use procon_core::config::{LoggingConfig, load};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// USB proxy configuration (`proxy.toml`)
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Proxy configuration
    pub proxy: ProxyConfig,
    /// Dump configuration
    pub dump: DumpConfig,
    /// Frame streaming to Grizzco Lab
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
    /// Record a session from launch until exit, next to what the lab records
    pub autostart: bool,
    /// Path prefix of that session folder, e.g. "/home/pi/procon-"
    pub prefix: String,
}

/// Frame streaming configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct StreamConfig {
    /// TCP port Grizzco Lab connects to
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
