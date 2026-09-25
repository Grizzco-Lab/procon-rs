//! Configuration management for ProCon proxy

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Main configuration structure
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Proxy configuration
    pub proxy: ProxyConfig,
    /// Dump configuration  
    pub dump: DumpConfig,
    /// Console output configuration
    pub console: ConsoleConfig,
    /// Visualization configuration
    pub visualization: VisualizationConfig,
    /// Performance configuration
    pub performance: PerformanceConfig,
    /// Logging configuration
    pub logging: LoggingConfig,
}

/// Proxy-related configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Timeout for reading from Pro Controller (milliseconds)
    pub controller_read_timeout_ms: i32,
    /// Interval for logging frame count progress
    pub frame_count_log_interval: u64,
    /// Retry delay when HID gadget device fails to open (milliseconds)
    pub hidg_retry_delay_ms: u64,
}

/// Dump-related configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct DumpConfig {
    /// Directory for recording session files; the dashboard can change it at runtime
    pub dir: String,
    /// Start recording at launch instead of waiting for the dashboard
    pub autostart: bool,
}

/// Console output configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct ConsoleConfig {
    /// Enable console output to terminal
    pub enable: bool,
}

/// Visualization configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct VisualizationConfig {
    /// Enable web-based visualization server
    pub web_enable: bool,
    /// Port for web visualization server
    pub web_port: u16,
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
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {}", path.as_ref().display()))?;

        let config: Config = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file: {}", path.as_ref().display()))?;

        Ok(config)
    }

    /// Save configuration to a TOML file
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let contents =
            toml::to_string_pretty(self).context("Failed to serialize config to TOML")?;

        fs::write(&path, contents)
            .with_context(|| format!("Failed to write config file: {}", path.as_ref().display()))?;

        Ok(())
    }

    /// Validate configuration values
    pub fn validate(&self) -> Result<()> {
        // Validate log level
        match self.logging.level.to_lowercase().as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => {}
            _ => anyhow::bail!("Invalid log level: {}", self.logging.level),
        }

        // Validate the dump directory exists
        if !Path::new(&self.dump.dir).is_dir() {
            anyhow::bail!("Dump directory does not exist: {}", self.dump.dir);
        }

        Ok(())
    }
}
