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
    /// Performance configuration
    pub performance: PerformanceConfig,
    /// Logging configuration
    pub logging: LoggingConfig,
}

/// Proxy-related configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// HID gadget device path for Nintendo Switch connection
    pub hid_device_path: String,
}

/// Dump-related configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct DumpConfig {
    /// File path for binary dump output
    pub file_path: String,
}

/// Console output configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct ConsoleConfig {
    /// Enable console output to terminal
    pub enable: bool,
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

        // Validate paths exist (for directories)
        if let Some(parent) = Path::new(&self.dump.file_path).parent() {
            if !parent.exists() {
                anyhow::bail!("Dump file directory does not exist: {}", parent.display());
            }
        }

        Ok(())
    }
}
